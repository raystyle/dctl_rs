pub mod ca;
pub mod cli;
pub mod clickhouse;
pub mod config;
pub mod docker;
pub mod falkordb;
pub mod output;
pub mod postgres;
pub(crate) mod registry;
pub mod server;

use cli::{InstallVersionArg, LocalCommands, ServerCommands};

use crate::error::Result;
use crate::{init, paths};
use std::io::Write;

pub async fn run(cmd: LocalCommands, json: bool) -> Result<()> {
    match cmd {
        LocalCommands::Install {
            version,
            force,
            registry,
        } => install(version, force, registry.as_deref(), json).await,
        LocalCommands::Init => {
            let result = init::init()?;
            let mut paths = Vec::new();
            if result.clickhouse_dir_created {
                paths.push(".dctl/".to_string());
            } else if result.runtime_gitignore_created {
                paths.push(".dctl/.gitignore".to_string());
            }
            if result.clickhouse_scaffold_created {
                paths.push("clickhouse/".to_string());
            }
            if result.postgres_scaffold_created {
                paths.push("postgres/".to_string());
            }
            if result.falkordb_scaffold_created {
                paths.push("falkordb/".to_string());
            }
            let out = output::InitOutput {
                paths,
                already_initialized: !result.clickhouse_dir_created,
            };
            output::print_output(&out, json);
            Ok(())
        }
        LocalCommands::Client {
            name,
            name_flag,
            host,
            port,
            version,
            query,
            queries_file,
            database,
            user,
            password,
        } => {
            clickhouse::client(clickhouse::ClientCmd {
                name: name.or(name_flag),
                version,
                host,
                port,
                query,
                queries_file,
                database,
                direct: clickhouse::DirectCreds { user, password },
            })
            .await
        }
        LocalCommands::Server { command } => run_server_commands(command, json).await,
        LocalCommands::Postgres { command } => postgres::run(command, json).await,
        LocalCommands::Falkordb { command } => falkordb::run(command, json).await,
        LocalCommands::Registry { command } => registry::run(command, json).await,
    }
}

async fn install_postgres(
    tag: &str,
    force: bool,
    registry_override: Option<&str>,
    json: bool,
) -> Result<()> {
    postgres::validate_pg_tag(tag)?;
    let docker = docker::connect().await?;
    let image_ref = format!("postgres:{tag}");
    if !force && docker::image_exists(&docker, &image_ref).await? {
        let out = output::InstallOutput {
            version: format!("postgres@{tag}"),
            set_as_default: false,
        };
        if !json {
            eprintln!("postgres:{tag} is already pulled");
        }
        output::print_output(&out, json);
        return Ok(());
    }

    docker::pull_image(&docker, &image_ref, json, registry_override).await?;

    let out = output::InstallOutput {
        version: format!("postgres@{tag}"),
        set_as_default: false,
    };
    output::print_output(&out, json);
    Ok(())
}

async fn install_falkordb(
    tag: &str,
    force: bool,
    registry_override: Option<&str>,
    json: bool,
) -> Result<()> {
    falkordb::validate_fk_tag(tag)?;
    let docker = docker::connect().await?;
    let image_ref = falkordb::fk_image_ref(tag);
    if !force && docker::image_exists(&docker, &image_ref).await? {
        let out = output::InstallOutput {
            version: format!("falkordb@{tag}"),
            set_as_default: false,
        };
        if !json {
            eprintln!("{image_ref} is already pulled");
        }
        output::print_output(&out, json);
        return Ok(());
    }

    docker::pull_image(&docker, &image_ref, json, registry_override).await?;

    let out = output::InstallOutput {
        version: format!("falkordb@{tag}"),
        set_as_default: false,
    };
    output::print_output(&out, json);
    Ok(())
}

async fn install(
    version: InstallVersionArg,
    force: bool,
    registry_override: Option<&str>,
    json: bool,
) -> Result<()> {
    match version {
        InstallVersionArg::ClickHouse(tag) => {
            install_clickhouse(&tag, force, registry_override, json).await
        }
        InstallVersionArg::Postgres(tag) => {
            install_postgres(&tag, force, registry_override, json).await
        }
        InstallVersionArg::Falkordb(version) => {
            install_falkordb(&version, force, registry_override, json).await
        }
    }
}

async fn install_clickhouse(
    tag: &str,
    force: bool,
    registry_override: Option<&str>,
    json: bool,
) -> Result<()> {
    clickhouse::validate_ch_tag(tag)?;
    let docker = docker::connect().await?;
    let image_ref = clickhouse::ch_image_ref(tag);
    if !force && docker::image_exists(&docker, &image_ref).await? {
        let out = output::InstallOutput {
            version: format!("clickhouse@{tag}"),
            set_as_default: false,
        };
        if !json {
            eprintln!("{image_ref} is already pulled");
        }
        output::print_output(&out, json);
        return Ok(());
    }
    docker::pull_image(&docker, &image_ref, json, registry_override).await?;
    let out = output::InstallOutput {
        version: format!("clickhouse@{tag}"),
        set_as_default: false,
    };
    output::print_output(&out, json);
    Ok(())
}

fn list_configs(json: bool) -> Result<()> {
    let dir = paths::configs_dir()?;
    let out = output::ServerConfigsOutput {
        dir: dir.display().to_string(),
        configs: config::list_configs()?,
    };
    output::print_output(&out, json);
    Ok(())
}

/// Format a dotenv line. Values that are plain alphanumeric tokens are written
/// bare; anything containing spaces, `#`, quotes, backslashes, or newlines is
/// double-quoted with inner `"`, `\`, and newlines escaped.
pub(crate) fn format_dotenv_line(prefix: &str, key: &str, val: &str) -> String {
    let needs_quoting = val.is_empty()
        || val
            .bytes()
            .any(|b| b == b' ' || b == b'#' || b == b'"' || b == b'\'' || b == b'\\' || b == b'\n');

    if needs_quoting {
        let escaped = val
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("{}{}=\"{}\"", prefix, key, escaped)
    } else {
        format!("{}{}={}", prefix, key, val)
    }
}

/// Extract a `<prefix>*` key from a dotenv line, handling optional `export`
/// prefix and whitespace around `=`. Returns the bare key (e.g. "CLICKHOUSE_HOST"
/// for prefix "CLICKHOUSE_") or None if the line isn't a matching assignment.
fn extract_dotenv_key<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let s = line.trim();
    let s = s
        .strip_prefix("export")
        .map(|rest| rest.trim_start())
        .unwrap_or(s);
    let eq_pos = s.find('=')?;
    let key = s[..eq_pos].trim_end();
    if key.starts_with(prefix) && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Some(key)
    } else {
        None
    }
}

/// Update an existing .env file: replace `<prefix>*` vars in-place, append any
/// missing ones. Lines for the same prefix that aren't in `vars` are preserved
/// (e.g. a manually-set CLICKHOUSE_PASSWORD survives a host/port-only update).
pub(crate) fn update_dotenv(existing: &str, prefix: &str, vars: &[(&str, String)]) -> String {
    let mut result = String::new();
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for line in existing.lines() {
        if let Some(key) = extract_dotenv_key(line, prefix) {
            if let Some((_, val)) = vars.iter().find(|(k, _)| *k == key) {
                let line_prefix = if line.trim_start().starts_with("export") {
                    "export "
                } else {
                    ""
                };
                result.push_str(&format_dotenv_line(line_prefix, key, val));
                written.insert(key);
            } else {
                // A matching-prefix var we don't manage — keep as-is
                result.push_str(line);
            }
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }

    for (key, val) in vars {
        if !written.contains(key) {
            result.push_str(&format_dotenv_line("", key, val));
            result.push('\n');
        }
    }

    result
}

async fn run_server_commands(command: ServerCommands, json: bool) -> Result<()> {
    match command {
        ServerCommands::Start {
            name,
            name_flag,
            version,
            http_port,
            native_port,
            user,
            password,
            database,
            config_file,
            env,
            wait_timeout,
        } => {
            clickhouse::start(clickhouse::StartCmd {
                name: name.or(name_flag),
                version,
                http_port,
                native_port,
                user,
                password,
                database,
                config: config_file,
                extra_env: env,
                wait_timeout: std::time::Duration::from_secs(wait_timeout.into()),
                json,
            })
            .await
        }
        ServerCommands::Configs => list_configs(json),
        ServerCommands::List => list_servers_local(json),
        ServerCommands::Stop {
            name,
            name_flag,
            version,
        } => {
            clickhouse::stop(
                name.or(name_flag).as_deref().unwrap_or("default"),
                version.as_deref(),
                json,
            )
            .await
        }
        ServerCommands::StopAll => stop_all_servers_local(json),
        ServerCommands::Dotenv {
            name,
            name_flag,
            version,
            local,
        } => clickhouse::dotenv(
            name.or(name_flag).as_deref(),
            version.as_deref(),
            local,
            json,
        ),
        ServerCommands::Remove {
            name,
            name_flag,
            version,
        } => clickhouse::remove(
            name.or(name_flag).as_deref().unwrap_or("default"),
            version.as_deref(),
            json,
        ),
    }
}

fn list_servers_local(json: bool) -> Result<()> {
    let project_dir = init::canonical_project_dir()?;
    let entries = server::list_all_servers()?;
    let running_count = entries.iter().filter(|e| e.running).count();
    let total = entries.len();

    let out = output::ServerListOutput {
        servers: entries
            .into_iter()
            .map(|e| {
                let running = e.running;
                let (display_name, pid, version, http_port, tcp_port, engine, container_id) =
                    match e.info {
                        Some(info) => {
                            let is_ch = info.engine == server::Engine::Clickhouse;
                            let pid = if is_ch && running {
                                Some(info.pid)
                            } else {
                                None
                            };
                            // The version is persistent instance identity
                            // (needed to pick --version among same-name
                            // instances), so it shows while stopped; ports
                            // may be 0 for a stopped container and stay
                            // hidden until the next start refreshes them.
                            let version = Some(info.version);
                            let http_port = if running { Some(info.http_port) } else { None };
                            let tcp_port = if running { Some(info.tcp_port) } else { None };
                            // For the Docker engines the disk key carries a
                            // version suffix; show users the friendly name.
                            let display = match info.engine {
                                server::Engine::Clickhouse => {
                                    clickhouse::ch_user_name_from_key(&e.name).to_string()
                                }
                                server::Engine::Postgres => {
                                    postgres::user_name_from_key(&e.name).to_string()
                                }
                                server::Engine::Falkordb => {
                                    falkordb::user_name_from_key(&e.name).to_string()
                                }
                            };
                            (
                                display,
                                pid,
                                version,
                                http_port,
                                tcp_port,
                                info.engine.as_str().to_string(),
                                info.container_id,
                            )
                        }
                        None => (
                            e.name.clone(),
                            None,
                            None,
                            None,
                            None,
                            "clickhouse".to_string(),
                            None,
                        ),
                    };
                output::ServerListEntry {
                    name: display_name,
                    running,
                    pid,
                    version,
                    http_port,
                    tcp_port,
                    project: None,
                    engine,
                    container_id,
                }
            })
            .collect(),
        total_servers: total,
        total_running_servers: running_count,
        project_scope: Some(output::exact_current_project_scope(&project_dir)),
        guidance: if total == 0 {
            output::project_scope_guidance()
        } else {
            Vec::new()
        },
    };
    output::print_output(&out, json);
    Ok(())
}

fn stop_all_servers_local(json: bool) -> Result<()> {
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let servers = server::list_running_servers_locked(&metadata_lock)?;
    let out = stop_servers(&servers, json, |name| {
        server::kill_server_locked(name, &metadata_lock)
    });
    if json {
        output::print_output(&out, json);
    } else if servers.is_empty() {
        println!("No running servers");
    } else {
        println!("Done");
    }
    Ok(())
}

pub(crate) fn stop_servers<F>(
    servers: &[server::ServerInfo],
    json: bool,
    mut stop: F,
) -> output::ServerStopAllOutput
where
    F: FnMut(&str) -> Result<()>,
{
    let servers = servers
        .iter()
        .map(|server| {
            let (name, version) = match server.engine {
                server::Engine::Clickhouse => (server.name.clone(), None),
                server::Engine::Postgres => (
                    postgres::user_name_from_key(&server.name).to_string(),
                    Some(server.version.clone()),
                ),
                server::Engine::Falkordb => (
                    falkordb::user_name_from_key(&server.name).to_string(),
                    Some(server.version.clone()),
                ),
            };
            let engine = server.engine.as_str().to_string();
            if !json {
                match version.as_deref() {
                    Some(version) => print!("Stopping '{}' ({}, {})...", name, engine, version),
                    None => print!("Stopping '{}' ({})...", name, engine),
                }
                let _ = std::io::stdout().flush();
            }
            let result = stop(&server.name);
            if !json {
                match &result {
                    Ok(()) => println!(" stopped"),
                    Err(error) => println!(" error: {error}"),
                }
            }
            output::ServerStopEntry {
                name,
                engine,
                version,
                stopped: result.is_ok(),
                error: result.err().map(|error| error.to_string()),
            }
        })
        .collect();

    output::ServerStopAllOutput { servers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    fn server_info(name: &str, engine: server::Engine, version: &str) -> server::ServerInfo {
        server::ServerInfo {
            name: name.to_string(),
            pid: 1,
            version: version.to_string(),
            http_port: 0,
            tcp_port: 0,
            started_at: "test".to_string(),
            cwd: "/tmp/project".to_string(),
            engine,
            container_id: None,
        }
    }

    #[test]
    fn stop_servers_attempts_and_reports_both_engines() {
        let servers = vec![
            server_info("default", server::Engine::Clickhouse, "clickhouse:26.8"),
            server_info("default-pg17", server::Engine::Postgres, "postgres:17"),
            server_info("default-pg18", server::Engine::Postgres, "postgres:18"),
        ];
        let mut attempts = Vec::new();

        let output = stop_servers(&servers, true, |name| {
            attempts.push(name.to_string());
            if name == "default" {
                Err(Error::DockerError("container stop failed".to_string()))
            } else {
                Ok(())
            }
        });

        assert_eq!(attempts, ["default", "default-pg17", "default-pg18"]);
        assert_eq!(output.servers.len(), 3);
        assert_eq!(output.servers[0].name, "default");
        assert_eq!(output.servers[0].engine, "clickhouse");
        assert_eq!(output.servers[0].version, None);
        assert!(!output.servers[0].stopped);
        assert_eq!(
            output.servers[0].error.as_deref(),
            Some("Docker error: container stop failed")
        );
        assert_eq!(output.servers[1].name, "default");
        assert_eq!(output.servers[1].engine, "postgres");
        assert_eq!(output.servers[1].version.as_deref(), Some("postgres:17"));
        assert!(output.servers[1].stopped);
        assert_eq!(output.servers[1].error, None);
        assert_eq!(output.servers[2].name, "default");
        assert_eq!(output.servers[2].engine, "postgres");
        assert_eq!(output.servers[2].version.as_deref(), Some("postgres:18"));
        assert!(output.servers[2].stopped);
        assert_eq!(output.servers[2].error, None);
    }

    #[test]
    fn update_dotenv_postgres_prefix_isolates_clickhouse_vars() {
        let existing = "CLICKHOUSE_HOST=localhost\nCLICKHOUSE_PORT=9000\nDATABASE_URL=x\n";
        let vars = vec![
            ("POSTGRES_HOST", "localhost".to_string()),
            ("POSTGRES_PORT", "5432".to_string()),
        ];
        let result = update_dotenv(existing, "POSTGRES_", &vars);
        assert!(result.contains("CLICKHOUSE_HOST=localhost"));
        assert!(result.contains("CLICKHOUSE_PORT=9000"));
        assert!(result.contains("POSTGRES_HOST=localhost"));
        assert!(result.contains("POSTGRES_PORT=5432"));
    }

    #[test]
    fn extract_dotenv_key_postgres_prefix() {
        assert_eq!(
            extract_dotenv_key("POSTGRES_USER=postgres", "POSTGRES_"),
            Some("POSTGRES_USER")
        );
        assert_eq!(extract_dotenv_key("CLICKHOUSE_HOST=x", "POSTGRES_"), None);
    }

    #[test]
    fn update_dotenv_creates_fresh_content() {
        let vars = vec![
            ("CLICKHOUSE_HOST", "localhost".to_string()),
            ("CLICKHOUSE_PORT", "9000".to_string()),
        ];
        let result = update_dotenv("", "CLICKHOUSE_", &vars);
        assert_eq!(result, "CLICKHOUSE_HOST=localhost\nCLICKHOUSE_PORT=9000\n");
    }

    #[test]
    fn update_dotenv_replaces_existing_vars() {
        let existing =
            "CLICKHOUSE_HOST=oldhost\nDATABASE_URL=postgres://...\nCLICKHOUSE_PORT=1234\n";
        let vars = vec![
            ("CLICKHOUSE_HOST", "localhost".to_string()),
            ("CLICKHOUSE_PORT", "9000".to_string()),
        ];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("CLICKHOUSE_HOST=localhost"));
        assert!(result.contains("CLICKHOUSE_PORT=9000"));
        assert!(result.contains("DATABASE_URL=postgres://..."));
        assert!(!result.contains("oldhost"));
        assert!(!result.contains("1234"));
    }

    #[test]
    fn update_dotenv_preserves_non_clickhouse_vars() {
        let existing = "FOO=bar\nBAZ=qux\n";
        let vars = vec![("CLICKHOUSE_HOST", "localhost".to_string())];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("FOO=bar"));
        assert!(result.contains("BAZ=qux"));
        assert!(result.contains("CLICKHOUSE_HOST=localhost"));
    }

    #[test]
    fn update_dotenv_appends_missing_vars() {
        let existing = "CLICKHOUSE_HOST=localhost\n";
        let vars = vec![
            ("CLICKHOUSE_HOST", "localhost".to_string()),
            ("CLICKHOUSE_PORT", "9000".to_string()),
        ];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("CLICKHOUSE_HOST=localhost"));
        assert!(result.contains("CLICKHOUSE_PORT=9000"));
    }

    #[test]
    fn update_dotenv_handles_export_prefix() {
        let existing = "export CLICKHOUSE_HOST=oldhost\nexport CLICKHOUSE_PORT=1234\n";
        let vars = vec![
            ("CLICKHOUSE_HOST", "localhost".to_string()),
            ("CLICKHOUSE_PORT", "9000".to_string()),
        ];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("export CLICKHOUSE_HOST=localhost"));
        assert!(result.contains("export CLICKHOUSE_PORT=9000"));
        assert!(!result.contains("oldhost"));
        assert!(!result.contains("1234"));
    }

    #[test]
    fn update_dotenv_handles_spaces_around_equals() {
        let existing = "CLICKHOUSE_HOST = oldhost\n";
        let vars = vec![("CLICKHOUSE_HOST", "localhost".to_string())];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("CLICKHOUSE_HOST=localhost"));
        assert!(!result.contains("oldhost"));
    }

    #[test]
    fn update_dotenv_handles_export_with_spaces() {
        let existing = "export CLICKHOUSE_PORT = 1234\nDATABASE_URL=postgres://...\n";
        let vars = vec![("CLICKHOUSE_PORT", "9000".to_string())];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("export CLICKHOUSE_PORT=9000"));
        assert!(result.contains("DATABASE_URL=postgres://..."));
        assert!(!result.contains("1234"));
    }

    #[test]
    fn update_dotenv_preserves_unmanaged_clickhouse_vars() {
        let existing = "CLICKHOUSE_HOST=localhost\nCLICKHOUSE_PASSWORD=secret\n";
        // Only updating HOST — PASSWORD should be left alone
        let vars = vec![("CLICKHOUSE_HOST", "newhost".to_string())];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains("CLICKHOUSE_HOST=newhost"));
        assert!(result.contains("CLICKHOUSE_PASSWORD=secret"));
    }

    #[test]
    fn extract_dotenv_key_simple() {
        assert_eq!(
            extract_dotenv_key("CLICKHOUSE_HOST=localhost", "CLICKHOUSE_"),
            Some("CLICKHOUSE_HOST")
        );
    }

    #[test]
    fn extract_dotenv_key_with_export() {
        assert_eq!(
            extract_dotenv_key("export CLICKHOUSE_HOST=localhost", "CLICKHOUSE_"),
            Some("CLICKHOUSE_HOST")
        );
    }

    #[test]
    fn extract_dotenv_key_with_spaces() {
        assert_eq!(
            extract_dotenv_key("CLICKHOUSE_HOST = localhost", "CLICKHOUSE_"),
            Some("CLICKHOUSE_HOST")
        );
        assert_eq!(
            extract_dotenv_key("export CLICKHOUSE_HOST = localhost", "CLICKHOUSE_"),
            Some("CLICKHOUSE_HOST")
        );
    }

    #[test]
    fn extract_dotenv_key_non_clickhouse() {
        assert_eq!(
            extract_dotenv_key("DATABASE_URL=postgres://...", "CLICKHOUSE_"),
            None
        );
        assert_eq!(extract_dotenv_key("export FOO=bar", "CLICKHOUSE_"), None);
    }

    #[test]
    fn extract_dotenv_key_comment_and_blank() {
        assert_eq!(
            extract_dotenv_key("# CLICKHOUSE_HOST=localhost", "CLICKHOUSE_"),
            None
        );
        assert_eq!(extract_dotenv_key("", "CLICKHOUSE_"), None);
    }

    #[test]
    fn format_dotenv_line_plain_value() {
        assert_eq!(format_dotenv_line("", "KEY", "value"), "KEY=value");
    }

    #[test]
    fn format_dotenv_line_with_prefix() {
        assert_eq!(
            format_dotenv_line("export ", "KEY", "value"),
            "export KEY=value"
        );
    }

    #[test]
    fn format_dotenv_line_quotes_spaces() {
        assert_eq!(
            format_dotenv_line("", "CLICKHOUSE_PASSWORD", "my secret"),
            r#"CLICKHOUSE_PASSWORD="my secret""#
        );
    }

    #[test]
    fn format_dotenv_line_quotes_hash() {
        assert_eq!(
            format_dotenv_line("", "CLICKHOUSE_PASSWORD", "pass#123"),
            r#"CLICKHOUSE_PASSWORD="pass#123""#
        );
    }

    #[test]
    fn format_dotenv_line_escapes_quotes_and_backslashes() {
        assert_eq!(
            format_dotenv_line("", "CLICKHOUSE_PASSWORD", r#"a"b\c"#),
            r#"CLICKHOUSE_PASSWORD="a\"b\\c""#
        );
    }

    #[test]
    fn format_dotenv_line_escapes_newlines() {
        assert_eq!(
            format_dotenv_line("", "CLICKHOUSE_PASSWORD", "line1\nline2"),
            r#"CLICKHOUSE_PASSWORD="line1\nline2""#
        );
    }

    #[test]
    fn format_dotenv_line_quotes_empty_value() {
        assert_eq!(
            format_dotenv_line("", "CLICKHOUSE_PASSWORD", ""),
            r#"CLICKHOUSE_PASSWORD="""#
        );
    }

    #[test]
    fn update_dotenv_quotes_special_values() {
        let vars = vec![
            ("CLICKHOUSE_HOST", "localhost".to_string()),
            ("CLICKHOUSE_PASSWORD", "my secret#123".to_string()),
        ];
        let result = update_dotenv("", "CLICKHOUSE_", &vars);
        assert!(result.contains("CLICKHOUSE_HOST=localhost"));
        assert!(result.contains(r#"CLICKHOUSE_PASSWORD="my secret#123""#));
    }

    #[test]
    fn update_dotenv_quotes_when_replacing_in_place() {
        let existing = "CLICKHOUSE_PASSWORD=old\n";
        let vars = vec![("CLICKHOUSE_PASSWORD", "new pass".to_string())];
        let result = update_dotenv(existing, "CLICKHOUSE_", &vars);
        assert!(result.contains(r#"CLICKHOUSE_PASSWORD="new pass""#));
    }
}
