mod cli;
mod error;
mod http;
mod init;
mod ledger;
mod local;
mod paths;
mod skills;
mod update;
mod user_agent;

use clap::error::ErrorKind;
use clap::{CommandFactory, FromArgMatches};
use cli::{Cli, Commands, SkillsArgs, UpdateArgs};

use error::{Error, Result};

#[tokio::main]
async fn main() {
    // REQ-011: `local` is the default mode — engine subcommands promote to
    // the top level via argv preprocessing (both `dctl server start` and
    // `dctl local server start` parse identically). A full tree restructure
    // can follow later; this is the zero-breakage transition.
    const ENGINE_COMMANDS: &[&str] = &[
        "server", "postgres", "falkordb", "registry", "install", "init", "client",
    ];
    let mut raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if raw_args.len() > 1 {
        let second = raw_args[1].to_string_lossy().to_string();
        // `clickhouse` is the engine's proper name (clap alias on Server).
        if second == "clickhouse" || ENGINE_COMMANDS.contains(&second.as_str()) {
            raw_args.insert(1, "local".into());
        }
    }

    // Parse via ArgMatches (rather than `Cli::try_parse()`) so post-parse
    // validation can attach errors to the right subcommand in the tree.
    let argv: Vec<std::ffi::OsString> = raw_args;
    let mut cmd = Cli::command();

    // Single-exit invariant (#320): every invocation — bare, help, version,
    // typo, dispatched command — falls through to this one exit. The `exec()`
    // handoffs (`local client`, host psql) replace the process image and are
    // the sanctioned departures. Child-process exit codes are returned as
    // `Error::ChildExit` so they also flow through this tail. Do not add
    // exit paths.
    let exit_code = match cmd.try_get_matches_from_mut(argv.iter()) {
        Ok(matches) => {
            // The matches were produced by this very `cmd`, so a mismatch is
            // a clap derive bug, not a user error.
            let cli = Cli::from_arg_matches(&matches)
                .expect("Cli::from_arg_matches must accept matches from Cli::command()");
            match validate_post_parse(&cli, &mut cmd) {
                Ok(()) => run_parsed(cli).await,
                Err(e) => {
                    let _ = e.print();
                    e.exit_code()
                }
            }
        }
        Err(e) => {
            // clap keeps its own formatting and colors; help/version print to
            // stdout, usage errors to stderr. Print failures are swallowed
            // like clap's own `Error::exit` swallows them: a broken pipe must
            // not turn exit 2 into a panic (which would also bypass this
            // single exit).
            let _ = e.print();
            match e.kind() {
                // --version always hits the network to refresh the cache + timer,
                // then prints the notice from the freshly-updated cache.
                ErrorKind::DisplayVersion => {
                    update::force_refresh_update_cache().await;
                    update::print_cached_update_notice();
                }
                // --help shows the notice from cache (no blocking network call).
                ErrorKind::DisplayHelp => update::print_cached_update_notice(),
                // Usage errors do no update-cache work: a mistyped invocation
                // must not cause network activity.
                _ => {}
            }
            // clap's own exit codes: 0 for help/version, 2 for usage errors.
            e.exit_code()
        }
    };

    std::process::exit(exit_code);
}

fn validate_post_parse(cli: &Cli, cmd: &mut clap::Command) -> std::result::Result<(), clap::Error> {
    if let Commands::Skills(args) = &cli.command {
        use std::io::IsTerminal;
        if let Some(message) = args.selection_validation_error(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        ) {
            let skills = cmd
                .find_subcommand_mut("skills")
                .expect("skills command must exist");
            return Err(skills.error(ErrorKind::MissingRequiredArgument, message));
        }
        return Ok(());
    }

    if let Commands::Local(args) = &cli.command {
        use std::io::IsTerminal;
        if let Some(message) = args.client_usage_validation_error(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        ) {
            let client = cmd
                .find_subcommand_mut("local")
                .and_then(|local| match &args.command {
                    crate::local::cli::LocalCommands::Postgres { .. } => {
                        local.find_subcommand_mut("postgres")
                    }
                    crate::local::cli::LocalCommands::Falkordb { .. } => {
                        local.find_subcommand_mut("falkordb")
                    }
                    _ => None,
                })
                .and_then(|engine| engine.find_subcommand_mut("client"))
                .expect("engine client command must exist");
            return Err(client.error(ErrorKind::ArgumentConflict, message));
        }
        let Some(message) = args
            .postgres_start_validation_error()
            .or_else(|| args.clickhouse_start_validation_error())
        else {
            if let Some(message) = args.falkor_start_validation_error() {
                let start = cmd
                    .find_subcommand_mut("local")
                    .and_then(|local| local.find_subcommand_mut("falkordb"))
                    .and_then(|falkordb| falkordb.find_subcommand_mut("start"))
                    .expect("local falkordb start command must exist");
                return Err(start.error(ErrorKind::ArgumentConflict, message));
            }
            return Ok(());
        };
        let start = cmd
            .find_subcommand_mut("local")
            .and_then(|local| local.find_subcommand_mut("postgres"))
            .and_then(|postgres| postgres.find_subcommand_mut("start"))
            .expect("local postgres start command must exist");
        return Err(start.error(ErrorKind::ArgumentConflict, message));
    }

    Ok(())
}

/// Run a successfully parsed invocation to completion and report the exit
/// code for `main`'s single exit. The `exec()` handoffs (`local client`,
/// host psql) replace the process image mid-run and are the sanctioned
/// departures from that invariant.
async fn run_parsed(cli: Cli) -> i32 {
    // Spawn a background task to refresh the update cache for non-update
    // commands. The refresh is gated to one network call per 24h; the notice
    // below is driven off whatever the cache currently holds.
    let is_update_cmd = matches!(cli.command, Commands::Update(_));
    let cache_refresh = if !is_update_cmd {
        Some(tokio::spawn(update::refresh_update_cache()))
    } else {
        None
    };

    // Decide whether to surface the update notice before `run` consumes the
    // command. Shown on every command that does not emit machine-readable JSON.
    let show_notice = should_show_update_notice(&cli.command);
    let local_json = match &cli.command {
        Commands::Local(args) => json_output(args.json),
        _ => false,
    };

    let result = run(cli.command).await;

    // Give the cache refresh a brief window to finish so short-lived commands
    // don't always drop it before the write completes. The background HTTP
    // request itself has a 400ms timeout, so 500ms here is enough headroom.
    if let Some(handle) = cache_refresh {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }

    let exit_code = match result {
        Ok(()) => 0,
        Err(e) => {
            let is_child_exit = matches!(&e, Error::ChildExit(_));
            if !is_child_exit {
                match &e {
                    _ if local_json => local::output::print_error(&e),
                    _ => {
                        use std::io::Write;
                        // Not `eprintln!`, which panics on a closed stderr.
                        let _ = writeln!(std::io::stderr(), "Error: {}", e);
                    }
                }
            }
            e.exit_code()
        }
    };

    // Always print the notice at the very end, after the command's own output
    // (stdout) and any error message.
    if show_notice {
        update::print_cached_update_notice();
    }

    exit_code
}

/// The explicit `--json` flag for a command, or `None` for commands that never
/// surface the update notice (the `update` command itself). Kept separate from
/// agent detection so the mapping is deterministic and unit-testable.
fn command_json_flag(cmd: &Commands) -> Option<bool> {
    match cmd {
        Commands::Update(_) => None,
        Commands::Local(args) => Some(args.json),
        Commands::Ledger(args) => Some(args.json),
        Commands::Skills(args) => Some(args.json),
    }
}

/// Whether to surface the cached update notice for this invocation. Shown for
/// every command that does not emit machine-readable JSON (`--json` or a
/// detected coding agent both suppress it), except the `update` command itself.
fn should_show_update_notice(cmd: &Commands) -> bool {
    match command_json_flag(cmd) {
        None => false,
        Some(flag) => !json_output(flag),
    }
}

/// Resolve whether to emit machine-readable JSON. True when `--json` was passed
/// or we're running under a known coding agent (same detection as the outbound
/// User-Agent in `user_agent.rs`). Pipes/redirects stay human-readable unless
/// `--json` is passed, matching `gh`/`kubectl` norms.
fn json_output(flag: bool) -> bool {
    flag || is_ai_agent::detect().is_some()
}

async fn run(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::Local(args) => local::run(args.command, json_output(args.json)).await,
        Commands::Ledger(args) => {
            // The shared ledger client is blocking HTTP with its own
            // embedded runtime; dropping that runtime inside this async
            // context panics, so the whole command rides a blocking thread.
            tokio::task::spawn_blocking(move || ledger::run(args.command, json_output(args.json)))
                .await
                .map_err(|join| {
                    crate::error::Error::Ledger(format!("ledger task failed: {join}"))
                })?
        }
        Commands::Skills(args) => run_skills(args).await,
        Commands::Update(args) => run_update(args).await,
    }
}

async fn run_update(args: UpdateArgs) -> Result<()> {
    let json = json_output(args.json);
    let result = if args.check {
        update::check_for_update().await?
    } else {
        update::perform_update(json).await?
    };
    result.write(&mut std::io::stdout(), json)
}

async fn run_skills(args: SkillsArgs) -> Result<()> {
    let json = json_output(args.json);
    skills::install(args, json).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn json_output_true_when_flag_set() {
        assert!(json_output(true));
    }

    fn parse(args: &[&str]) -> Commands {
        Cli::try_parse_from(args).unwrap().command
    }

    fn parse_and_validate(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        let mut cmd = Cli::command();
        let matches = cmd.try_get_matches_from_mut(args)?;
        let cli = Cli::from_arg_matches(&matches)
            .expect("Cli::from_arg_matches must accept matches from Cli::command()");
        validate_post_parse(&cli, &mut cmd)?;
        Ok(cli)
    }

    #[test]
    fn local_postgres_start_env_relationship_errors_are_clap_usage_errors() {
        for (extra, diagnostic) in [
            (
                ["--env", "APP_MODE=dev", "--env", "APP_MODE=test"].as_slice(),
                "APP_MODE",
            ),
            (
                [
                    "--password",
                    "from-flag",
                    "--env",
                    "POSTGRES_PASSWORD=from-env",
                ]
                .as_slice(),
                "both --password and --env",
            ),
        ] {
            let args: Vec<&str> = ["dctl", "local", "postgres", "start"]
                .iter()
                .chain(extra)
                .copied()
                .collect();
            let error = parse_and_validate(&args)
                .err()
                .expect("invalid environment relationship should fail validation");
            assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
            assert_eq!(error.exit_code(), 2);
            let message = error.to_string();
            assert!(message.contains(diagnostic), "{message}");
            assert!(message.contains("dctl local postgres start"), "{message}");
        }
    }

    #[test]
    fn command_json_flag_tracks_each_command() {
        // Human-readable commands report an explicit `false` flag.
        assert_eq!(
            command_json_flag(&parse(&["dctl", "local", "server", "list"])),
            Some(false)
        );
        // --json is picked up as a global flag.
        assert_eq!(
            command_json_flag(&parse(&["dctl", "local", "--json", "server", "list"])),
            Some(true)
        );
        // Management commands expose their explicit JSON flag.
        assert_eq!(command_json_flag(&parse(&["dctl", "skills"])), Some(false));
        assert_eq!(
            command_json_flag(&parse(&["dctl", "skills", "--json"])),
            Some(true)
        );
        // The update command never surfaces the notice.
        assert_eq!(command_json_flag(&parse(&["dctl", "update"])), None);
    }

    #[test]
    fn update_notice_suppressed_for_json_and_update() {
        // --json suppresses the notice so machine output stays clean,
        // regardless of agent detection.
        assert!(!should_show_update_notice(&parse(&[
            "dctl", "local", "--json", "server", "list"
        ])));
        // The update command never nags about itself.
        assert!(!should_show_update_notice(&parse(&["dctl", "update"])));
    }
}
