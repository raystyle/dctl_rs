use clap::{Args, Subcommand};
use std::str::FromStr;

fn parse_server_name_arg(name: &str) -> Result<String, String> {
    crate::local::server::validate_server_name(name)
        .map(|()| name.to_string())
        .map_err(|error| error.to_string())
}

const INSTALL_AFTER_HELP: &str = "\
CONTEXT FOR AGENTS:
  Every selector pulls a Docker image (needs Docker running); nothing is set as a default.
  ClickHouse accepts an image tag (26.8, 26.8.9.10, latest); `postgres@<tag>` and
  `falkordb@<version>` select the other engines.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallVersionArg {
    ClickHouse(String),
    Postgres(String),
    Falkordb(String),
}

impl FromStr for InstallVersionArg {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if let Some(tag) = input
            .strip_prefix("postgres@")
            .or_else(|| input.strip_prefix("postgres:"))
        {
            return Ok(Self::Postgres(tag.to_string()));
        }
        if let Some(version) = input
            .strip_prefix("falkordb@")
            .or_else(|| input.strip_prefix("falkordb:"))
        {
            return Ok(Self::Falkordb(version.to_string()));
        }

        // ClickHouse: an image tag, validated so bad shapes fail at clap time.
        crate::local::clickhouse::validate_ch_tag(input).map_err(|e| e.to_string())?;
        Ok(Self::ClickHouse(input.to_string()))
    }
}

#[derive(Args)]
pub struct LocalArgs {
    /// Output as JSON
    #[arg(long, global = true, display_order = crate::cli::help_order::JSON)]
    pub json: bool,

    #[command(subcommand)]
    pub command: LocalCommands,
}

impl LocalArgs {
    pub(crate) fn postgres_start_validation_error(&self) -> Option<String> {
        let LocalCommands::Postgres {
            command: PostgresCommands::Start { password, env, .. },
        } = &self.command
        else {
            return None;
        };
        crate::local::postgres::validate_pg_start_env_args(password.as_deref(), env).err()
    }

    /// Duplicate `-e KEY=...` assignments would silently let Docker pick the
    /// last one; reject them at parse time like pg does.
    pub(crate) fn clickhouse_start_validation_error(&self) -> Option<String> {
        let LocalCommands::Server {
            command: ServerCommands::Start { env, .. },
        } = &self.command
        else {
            return None;
        };
        let mut seen = std::collections::BTreeSet::new();
        for assignment in env {
            let key = assignment.split('=').next().unwrap_or_default();
            if !seen.insert(key.to_string()) {
                return Some(format!(
                    "--env key '{key}' is passed more than once; keep one assignment per key"
                ));
            }
        }
        None
    }

    pub(crate) fn falkor_start_validation_error(&self) -> Option<String> {
        let LocalCommands::Falkordb {
            command: FalkorCommands::Start { env, .. },
        } = &self.command
        else {
            return None;
        };
        crate::local::falkordb::validate_fk_start_env_args(env).err()
    }
}

#[derive(Subcommand)]
pub enum LocalCommands {
    /// Install a ClickHouse version or a database engine image
    #[command(after_help = INSTALL_AFTER_HELP)]
    Install {
        /// ClickHouse image tag (26.8, 26.8.9.10, latest) or selector (postgres@18, falkordb@4.20.6)
        version: InstallVersionArg,

        /// Re-install even if already installed
        #[arg(long)]
        force: bool,
    },

    /// Initialize a project directory for ClickHouse, Postgres and FalkorDB
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  `.dctl/` holds runtime data and is git-ignored; the `clickhouse/`, `postgres/` and
  `falkordb/` scaffolds are meant to be committed.
  Idempotent — re-running only creates what is missing.
  Next: `dctl local server start`")]
    Init,

    /// Connect to a running ClickHouse server
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Default mode looks up a Docker-managed server; queries run via the HTTP interface
  (no clickhouse-client binary needed). Interactive mode uses docker exec.
  Direct mode (--host/--port) connects via HTTP to any ClickHouse server; pass
  --user/--password when that server requires auth (dctl-managed instances do).
  The HTTP interface executes ONE statement per --query/--queries-file; split
  multi-statement files or use interactive mode for them.
  `--query` output stays native even with --json or a coding agent.")]
    Client {
        /// Server name to connect to (default: "default")
        #[arg(value_name = "NAME", conflicts_with_all = ["name_flag", "host", "port"])]
        #[arg(display_order = 0)]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            short = 'n',
            value_name = "NAME",
            hide = true,
            conflicts_with_all = ["name", "host", "port"]
        )]
        #[arg(display_order = 0)]
        name_flag: Option<String>,

        /// Host to connect to directly, bypassing managed lookup (HTTP port 8123)
        #[arg(long)]
        #[arg(display_order = 1)]
        host: Option<String>,

        /// HTTP port for direct connection (host 127.0.0.1)
        #[arg(
            long,
            short,
            value_parser = clap::value_parser!(u16).range(1..=65535)
        )]
        #[arg(display_order = 2)]
        port: Option<u16>,

        /// ClickHouse version to disambiguate when multiple share a name
        #[arg(long, short = 'v', conflicts_with_all = ["host", "port"])]
        #[arg(display_order = 3)]
        version: Option<String>,

        /// Execute a SQL query via the HTTP interface
        #[arg(long, short, conflicts_with = "queries_file")]
        #[arg(display_order = 4)]
        query: Option<String>,

        /// Execute queries from a SQL file ("-" for stdin)
        #[arg(long, conflicts_with = "query")]
        #[arg(display_order = 5)]
        queries_file: Option<String>,

        /// Database to use
        #[arg(long)]
        #[arg(display_order = 6)]
        database: Option<String>,

        /// User for direct mode (--host/--port) authentication
        #[arg(long, conflicts_with_all = ["name", "name_flag"])]
        #[arg(display_order = 7)]
        user: Option<String>,

        /// Password for direct mode (--host/--port) authentication
        #[arg(long, conflicts_with_all = ["name", "name_flag"])]
        #[arg(display_order = 8)]
        password: Option<String>,
    },

    /// Manage local server instances
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  `list` and `stop-all` cover ClickHouse and Docker-backed Postgres/FalkorDB; other subcommands
  are ClickHouse-only.
  Data persists across stop/start; only `remove` deletes it.
  Retain the name `start` returns (it may be generated) for later `stop`/`remove`.
  Custom configs inherit built-in defaults; find available names with `server configs`.
  Typical flow: `server start dev` -> `local client dev` -> `server stop dev`")]
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },

    /// Manage local Postgres instances (Docker-backed)
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Requires Docker installed and running.
  Each instance is keyed on (name, major version); pass --version when one name has two majors.
  There is no `postgres list` — `local server list` shows all engines together.
  Typical flow: `postgres start` -> `postgres client` -> `postgres dotenv --local` -> `postgres stop`")]
    Postgres {
        #[command(subcommand)]
        command: PostgresCommands,
    },

    /// Manage local FalkorDB graph instances (Docker-backed)
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  FalkorDB is a Redis-module graph database; queries are openCypher via GRAPH.QUERY.
  An existing stopped instance for the same (name, version) is resumed with its stored password;
  --port/--browser-port/--password/-e are ignored on a resume.
  Without --version, an existing instance selects the version; two versions under one name error.
  The generated password is printed once by start — re-read it later with `falkordb dotenv`.
  A failed fresh start rolls back the container and data it created; pre-existing data is kept.")]
    Falkordb {
        #[command(subcommand)]
        command: FalkorCommands,
    },
}

#[derive(Subcommand)]
pub enum FalkorCommands {
    /// Start a FalkorDB instance
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Ports: 6379 (Redis protocol) and 3000 (Browser UI); auto-picked when busy, never the same port.
  --query content is a redis command; quote the Cypher, e.g.
  `falkordb client -q 'GRAPH.QUERY g \"MATCH (n) RETURN n\"'`.
  FALKORDB_ARGS (module tuning) may be set with --env; REDIS_ARGS is managed.")]
    Start {
        /// Server name (default: "default", or random if default is already running)
        #[arg(value_name = "NAME", conflicts_with = "name_flag", value_parser = parse_server_name_arg)]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true,
            value_parser = parse_server_name_arg
        )]
        name_flag: Option<String>,

        /// FalkorDB version, full X.Y.Z (e.g. 4.20.6) or latest. Default: 4.20.6
        ///
        /// Pulls the image if it is not present locally.
        #[arg(long, short = 'v', value_parser = crate::local::falkordb::parse_fk_tag_arg)]
        version: Option<String>,

        /// Host TCP port for the Redis protocol; when omitted, 6379 if free else auto-selected
        ///
        /// An explicitly requested port that is already in use is rejected.
        #[arg(long, value_parser = crate::local::falkordb::parse_fk_port_arg)]
        port: Option<u16>,

        /// Host TCP port for the Browser UI; when omitted, 3000 if free else auto-selected
        #[arg(long, value_parser = crate::local::falkordb::parse_fk_port_arg)]
        browser_port: Option<u16>,

        /// Redis password (default: random 24-char alphanumeric)
        #[arg(long)]
        password: Option<String>,

        /// Extra container env vars; repeatable, each key at most once
        ///
        /// REDIS_ARGS is managed and rejected here — use --password. FALKORDB_ARGS
        /// (module tuning) is accepted.
        #[arg(
            short = 'e',
            long = "env",
            value_name = "KEY=VALUE",
            value_parser = crate::local::falkordb::parse_fk_env_arg
        )]
        env: Vec<String>,

        /// Seconds to wait for FalkorDB readiness (maximum: 600)
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u16).range(1..=600)
        )]
        wait_timeout: u16,
    },

    /// Stop a running FalkorDB instance
    Stop {
        /// Name of the instance to stop (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// FalkorDB version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,
    },

    /// Stop all FalkorDB instances in this project
    StopAll,

    /// Remove a stopped FalkorDB instance and its data
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Irreversible: removes the container and deletes its data directory. Stop the instance first —
  removing a running one errors.")]
    Remove {
        /// Name of the instance to remove (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// FalkorDB version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,
    },

    /// Connect to a running FalkorDB instance with redis-cli
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Managed mode (the default; NAME selects one) execs host `redis-cli` when it is on PATH, else
  runs redis-cli inside the container via `docker exec`; the stored password authenticates.
  Direct mode (--host/--port) requires `redis-cli` on PATH and reads no managed credentials —
  bring auth through the passthrough args.
  --query is a redis command: quote the Cypher, e.g. -q 'GRAPH.QUERY g \"MATCH (n) RETURN n\"'.
  Put wrapper options before `--`; all arguments after it go to redis-cli.
  Interactive and --query output stays native, even with --json or a coding agent.")]
    Client {
        /// Managed instance to connect to (default: "default")
        #[arg(value_name = "NAME", conflicts_with_all = ["name_flag", "host", "port"])]
        #[arg(display_order = 0)]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            short = 'n',
            value_name = "NAME",
            hide = true,
            conflicts_with_all = ["name", "host", "port"]
        )]
        #[arg(display_order = 0)]
        name_flag: Option<String>,

        /// FalkorDB version to disambiguate when multiple share a name
        #[arg(long, short = 'v', conflicts_with_all = ["host", "port"])]
        #[arg(display_order = 3)]
        version: Option<String>,

        /// Host to connect to directly, bypassing managed lookup (port 6379)
        #[arg(long)]
        #[arg(display_order = 1)]
        host: Option<String>,

        /// TCP port to connect to directly, bypassing managed lookup (host 127.0.0.1)
        #[arg(
            long,
            short,
            value_parser = clap::value_parser!(u16).range(1..=65535)
        )]
        #[arg(display_order = 2)]
        port: Option<u16>,

        /// Execute a single redis command (quote Cypher arguments)
        #[arg(long, short)]
        #[arg(display_order = 4)]
        query: Option<String>,

        /// Native redis-cli arguments (require --)
        #[arg(last = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Write FalkorDB connection env vars to a .env file
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Writes FALKORDB_HOST, FALKORDB_PORT, FALKORDB_PASSWORD, FALKORDB_BROWSER_URL.
  The instance must be running.
  Managed FALKORDB_* keys are replaced in place; other lines in the file are preserved.
  Contains the password in plaintext — prefer --local and keep it out of version control.")]
    Dotenv {
        /// Instance name (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// FalkorDB version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,

        /// Write to .env.local instead of .env
        #[arg(long)]
        local: bool,
    },
}

#[derive(Subcommand)]
pub enum ServerCommands {
    /// Start a ClickHouse server instance (Docker-backed)
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Docker-managed: same container lifecycle as Postgres and FalkorDB.
  Ports: 8123 (HTTP) and 9000 (native); auto-picked when busy, never the same.
  The generated password is printed once by start; query later with `local client -q`.
  An existing stopped instance is resumed; --user/--password/--database/--config/--env and
  the port flags are ignored on a resume (the container keeps its settings).
  A failed fresh start rolls back container and data; pre-existing data is kept.")]
    Start {
        /// Server name (default: "default", or random if default is already running)
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the server name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// ClickHouse image tag: 26.8, 26.8.9, 26.8.9.10, or latest. Default: 26.8
        #[arg(long, short = 'v', value_parser = crate::local::clickhouse::parse_ch_tag_arg)]
        version: Option<String>,

        /// HTTP port; when omitted, 8123 if free else auto-selected
        #[arg(long, value_parser = crate::local::clickhouse::parse_ch_http_port_arg)]
        http_port: Option<u16>,

        /// Native TCP port; when omitted, 9000 if free else auto-selected
        #[arg(long, value_parser = crate::local::clickhouse::parse_ch_native_port_arg)]
        native_port: Option<u16>,

        /// CLICKHOUSE_USER (default: default)
        #[arg(long)]
        user: Option<String>,

        /// CLICKHOUSE_PASSWORD (default: random 24-char alphanumeric)
        #[arg(long)]
        password: Option<String>,

        /// CLICKHOUSE_DB (default: default)
        #[arg(long)]
        database: Option<String>,

        /// Overlay defaults with a named partial config (see `server configs`)
        #[arg(long = "config", alias = "config-file", value_name = "NAME")]
        config_file: Option<String>,

        /// Extra container env vars; repeatable, each key at most once
        ///
        /// CLICKHOUSE_USER/PASSWORD/DB are managed; use the corresponding flags.
        #[arg(
            short = 'e',
            long = "env",
            value_name = "KEY=VALUE",
            value_parser = crate::local::clickhouse::parse_ch_env_arg
        )]
        env: Vec<String>,

        /// Seconds to wait for readiness (maximum: 600)
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(1..=600))]
        wait_timeout: u16,
    },

    /// List custom config files available to `server start --config`
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Run this command to find the config directory, then add an XML or YAML file.
  Include only the server, user, profile or quota settings you want to change.
  Select one file with `server start --config <name>`; its extension is optional.
  If names share a stem, specify the extension.")]
    Configs,

    /// List all server instances (running and stopped)
    List,

    /// Stop a running server
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Docker-managed engines stop their container; resume preserves data and credentials.")]
    Stop {
        /// Name of the server to stop (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the server name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// ClickHouse version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,
    },

    /// Stop all servers of every engine in this project
    StopAll,

    /// Remove a stopped server and its data
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Irreversible: removes the container and deletes its data directory. Stop the server first.")]
    Remove {
        /// Name of the server to remove (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the server name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// ClickHouse version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,
    },

    /// Write ClickHouse connection env vars to a .env file
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Requires a running server; reads ports and credentials from the container env.
  Writes CLICKHOUSE_HOST, CLICKHOUSE_HTTP_PORT, CLICKHOUSE_PORT, CLICKHOUSE_USER,
  CLICKHOUSE_PASSWORD, CLICKHOUSE_DATABASE.
  Contains the password in plaintext — prefer --local.")]
    Dotenv {
        /// Server name (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// ClickHouse version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,

        /// Write to .env.local instead of .env
        #[arg(long)]
        local: bool,
    },
}

#[derive(Subcommand)]
pub enum PostgresCommands {
    /// Start a Postgres instance
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  An existing stopped instance for the same (name, major) is resumed with its stored settings, so
  --port/--user/--password/--database/-e are ignored on a resume.
  Without --version, an existing instance selects the major; two majors under one name error.
  The generated password is printed once by start — re-read connection details later with
  `postgres dotenv` or `postgres client`.
  A failed fresh start rolls back the container and data it created; pre-existing data is kept.")]
    Start {
        /// Server name (default: "default", or random if default is already running)
        #[arg(value_name = "NAME", conflicts_with = "name_flag", value_parser = parse_server_name_arg)]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true,
            value_parser = parse_server_name_arg
        )]
        name_flag: Option<String>,

        /// Postgres image tag, major 17 or 18 (e.g. 17-alpine, 18.1). Default: 18
        ///
        /// Pulls the image if it is not present locally.
        #[arg(long, short = 'v', value_parser = crate::local::postgres::parse_pg_tag_arg)]
        version: Option<String>,

        /// Host TCP port; when omitted, 5432 if free else an auto-selected free port
        ///
        /// An explicitly requested port that is already in use is rejected.
        #[arg(long, value_parser = crate::local::postgres::parse_pg_port_arg)]
        port: Option<u16>,

        /// POSTGRES_USER (default: postgres)
        #[arg(long)]
        user: Option<String>,

        /// POSTGRES_PASSWORD (default: random 24-char alphanumeric)
        #[arg(long)]
        password: Option<String>,

        /// POSTGRES_DB (default: postgres)
        #[arg(long)]
        database: Option<String>,

        /// Extra container env vars; repeatable, each key at most once
        ///
        /// POSTGRES_USER, POSTGRES_DB and PGDATA are managed and rejected here — use
        /// --user/--database. POSTGRES_PASSWORD is accepted, but not together with --password.
        #[arg(
            short = 'e',
            long = "env",
            value_name = "KEY=VALUE",
            value_parser = crate::local::postgres::parse_pg_env_arg
        )]
        env: Vec<String>,

        /// Seconds to wait for PostgreSQL readiness (maximum: 600)
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u16).range(1..=600)
        )]
        wait_timeout: u16,
    },

    /// Stop a running Postgres instance
    Stop {
        /// Name of the instance to stop (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// Postgres version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,
    },

    /// Stop all Postgres instances in this project
    StopAll,

    /// Remove a stopped Postgres instance and its data
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Irreversible: removes the container and deletes its data directory. Stop the instance first —
  removing a running one errors.")]
    Remove {
        /// Name of the instance to remove (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// Postgres version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,
    },

    /// Connect to a running Postgres instance with psql
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Managed mode (the default; NAME selects one) execs host `psql` when it is on PATH, else runs
  psql inside the container via `docker exec`.
  Direct mode (--host/--port) requires `psql` on PATH and connects as user/database \"postgres\"
  with no password; it does not read managed credentials.
  Put wrapper options before `--`; all arguments after it go to psql.
  Interactive, --query and --queries-file output stays native, even with --json or a coding agent.")]
    Client {
        /// Managed instance to connect to (default: "default")
        #[arg(value_name = "NAME", conflicts_with_all = ["name_flag", "host", "port"])]
        #[arg(display_order = 0)]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            short = 'n',
            value_name = "NAME",
            hide = true,
            conflicts_with_all = ["name", "host", "port"]
        )]
        #[arg(display_order = 0)]
        name_flag: Option<String>,

        /// Postgres version to disambiguate when multiple share a name
        #[arg(long, short = 'v', conflicts_with_all = ["host", "port"])]
        #[arg(display_order = 3)]
        version: Option<String>,

        /// Host to connect to directly, bypassing managed lookup (port 5432)
        #[arg(long)]
        #[arg(display_order = 1)]
        host: Option<String>,

        /// TCP port to connect to directly, bypassing managed lookup (host 127.0.0.1)
        #[arg(
            long,
            short,
            value_parser = clap::value_parser!(u16).range(1..=65535)
        )]
        #[arg(display_order = 2)]
        port: Option<u16>,

        /// Execute a single SQL query
        #[arg(long, short)]
        #[arg(display_order = 4)]
        query: Option<String>,

        /// Execute queries from a SQL file
        #[arg(long)]
        #[arg(display_order = 5)]
        queries_file: Option<String>,

        /// Native psql arguments (require --)
        #[arg(last = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Write Postgres connection env vars to a .env file
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Writes POSTGRES_HOST, POSTGRES_PORT, POSTGRES_USER, POSTGRES_PASSWORD, POSTGRES_DATABASE.
  The instance must be running.
  Managed POSTGRES_* keys are replaced in place; other lines in the file are preserved.
  Contains the password in plaintext — prefer --local and keep it out of version control.")]
    Dotenv {
        /// Instance name (default: "default")
        #[arg(value_name = "NAME", conflicts_with = "name_flag")]
        name: Option<String>,

        /// Compatibility form for the instance name; prefer positional NAME
        #[arg(
            long = "name",
            value_name = "NAME",
            conflicts_with = "name",
            hide = true
        )]
        name_flag: Option<String>,

        /// Postgres version to disambiguate when multiple share a name
        #[arg(long, short = 'v')]
        version: Option<String>,

        /// Write to .env.local instead of .env
        #[arg(long)]
        local: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    fn local_args(args: &[&str]) -> LocalArgs {
        let mut argv = vec!["dctl", "local"];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).unwrap();
        let Commands::Local(local) = cli.command else {
            panic!("expected local command");
        };
        local
    }

    fn local_command(args: &[&str]) -> LocalCommands {
        local_args(args).command
    }

    fn local_parse_error(args: &[&str]) -> clap::Error {
        let mut argv = vec!["dctl", "local"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
            .err()
            .expect("invalid local arguments should fail during clap parsing")
    }

    fn rendered_help(args: &[&str]) -> String {
        let mut argv = vec!["dctl", "local"];
        argv.extend_from_slice(args);
        let error = Cli::try_parse_from(argv)
            .err()
            .expect("--help should stop parsing");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        error.to_string()
    }

    // ── install selectors ────────────────────────────────────────────────

    #[test]
    fn install_selectors_parse_by_engine() {
        for (input, expected) in [
            ("26.8", InstallVersionArg::ClickHouse("26.8".to_string())),
            (
                "26.8.9.10",
                InstallVersionArg::ClickHouse("26.8.9.10".to_string()),
            ),
            (
                "latest",
                InstallVersionArg::ClickHouse("latest".to_string()),
            ),
            ("postgres@18", InstallVersionArg::Postgres("18".to_string())),
            (
                "postgres:17-alpine",
                InstallVersionArg::Postgres("17-alpine".to_string()),
            ),
            (
                "falkordb@4.20.6",
                InstallVersionArg::Falkordb("4.20.6".to_string()),
            ),
            (
                "falkordb:latest",
                InstallVersionArg::Falkordb("latest".to_string()),
            ),
            (
                "  postgres@16  ",
                InstallVersionArg::Postgres("16".to_string()),
            ),
        ] {
            let LocalCommands::Install { version, .. } = local_command(&["install", input]) else {
                panic!("expected install version for {input}");
            };
            assert_eq!(version, expected, "{input}");
        }
    }

    #[test]
    fn install_rejects_malformed_clickhouse_tags_at_clap_time() {
        for tag in ["not.a.version", "25", "26.8.9.10.11", "26.8.x"] {
            let error = local_parse_error(&["install", tag]);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{tag}"
            );
            assert!(
                error
                    .to_string()
                    .contains("invalid or unsupported ClickHouse version"),
                "{tag}"
            );
        }
    }

    // ── server start (Docker flags) ──────────────────────────────────────

    #[test]
    fn parses_server_start_owned_options() {
        let LocalCommands::Server {
            command:
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
                },
        } = local_command(&[
            "server",
            "start",
            "--name",
            "analytics",
            "--version",
            "26.8",
            "--http-port",
            "18123",
            "--native-port",
            "19000",
            "--user",
            "app",
            "--password",
            "secret",
            "--database",
            "events",
            "--config",
            "analytics",
            "-e",
            "CLICKHOUSE_SKIP_USER_SETUP=1",
            "--wait-timeout",
            "75",
        ])
        else {
            panic!("expected server start");
        };
        assert_eq!(name.or(name_flag).as_deref(), Some("analytics"));
        assert_eq!(version.as_deref(), Some("26.8"));
        assert_eq!(http_port, Some(18123));
        assert_eq!(native_port, Some(19000));
        assert_eq!(user.as_deref(), Some("app"));
        assert_eq!(password.as_deref(), Some("secret"));
        assert_eq!(database.as_deref(), Some("events"));
        assert_eq!(config_file.as_deref(), Some("analytics"));
        assert_eq!(env, ["CLICKHOUSE_SKIP_USER_SETUP=1"]);
        assert_eq!(wait_timeout, 75);
    }

    #[test]
    fn server_start_defaults_to_no_explicit_ports_and_default_timeout() {
        let LocalCommands::Server {
            command:
                ServerCommands::Start {
                    name,
                    version,
                    http_port,
                    native_port,
                    user,
                    password,
                    database,
                    config_file,
                    env,
                    wait_timeout,
                    ..
                },
        } = local_command(&["server", "start"])
        else {
            panic!("expected server start");
        };
        assert_eq!(name, None);
        assert_eq!(version, None);
        assert_eq!(http_port, None);
        assert_eq!(native_port, None);
        assert_eq!(user, None);
        assert_eq!(password, None);
        assert_eq!(database, None);
        assert_eq!(config_file, None);
        assert!(env.is_empty());
        assert_eq!(wait_timeout, 60);
    }

    #[test]
    fn server_start_rejects_bad_tags_ports_and_timeout_at_clap_time() {
        for (args, expected) in [
            (
                vec!["--version", "latest!"],
                "invalid or unsupported ClickHouse version",
            ),
            (
                vec!["--version", "25"],
                "invalid or unsupported ClickHouse version",
            ),
            (vec!["--http-port", "0"], "--http-port 0 is not allowed"),
            (vec!["--native-port", "0"], "--native-port 0 is not allowed"),
            (vec!["--http-port", "not-a-port"], "expected an integer"),
            (vec!["--wait-timeout", "0"], "1..=600"),
            (vec!["--wait-timeout", "601"], "1..=600"),
        ] {
            let argv: Vec<&str> = ["server", "start"]
                .iter()
                .chain(args.iter())
                .copied()
                .collect();
            let error = local_parse_error(&argv);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{args:?}"
            );
            assert!(error.to_string().contains(expected), "{args:?}: {error}");
        }
    }

    #[test]
    fn server_start_help_renders_default_name_without_escaped_quotes() {
        let help = rendered_help(&["server", "start", "--help"]);
        assert!(help.contains(r#"(default: "default""#), "{help}");
        assert!(!help.contains(r#"\"default\""#), "{help}");
    }

    #[test]
    fn parses_server_start_config_and_legacy_alias() {
        for flag in ["--config", "--config-file"] {
            let LocalCommands::Server {
                command: ServerCommands::Start { config_file, .. },
            } = local_command(&["server", "start", flag, "analytics"])
            else {
                panic!("expected server start");
            };
            assert_eq!(config_file.as_deref(), Some("analytics"));
        }
    }

    #[test]
    fn server_start_rejects_bad_env_at_clap_time() {
        for (args, expected) in [
            (vec!["-e", "NO_EQUALS"], "expected KEY=VALUE"),
            (
                vec!["-e", "1KEY=value"],
                "must not be empty or start with a digit",
            ),
            (vec!["-e", "BAD-KEY=value"], "only [A-Za-z0-9_] allowed"),
            (
                vec!["-e", "CLICKHOUSE_PASSWORD=secret"],
                "CLICKHOUSE_PASSWORD is managed by dctl",
            ),
            (
                vec!["-e", "CLICKHOUSE_USER=admin"],
                "CLICKHOUSE_USER is managed by dctl",
            ),
        ] {
            let argv: Vec<&str> = ["server", "start"]
                .iter()
                .chain(args.iter())
                .copied()
                .collect();
            let error = local_parse_error(&argv);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{args:?}"
            );
            assert!(error.to_string().contains(expected), "{args:?}: {error}");
        }
    }

    #[test]
    fn server_start_rejects_duplicate_env_keys_after_parse() {
        // Duplicates pass clap (each assignment is well-formed) and are
        // rejected by the post-parse validation, like pg's password/env rule.
        let args = local_args(&["server", "start", "-e", "FOO=1", "--env", "FOO=2"]);
        let message = args
            .clickhouse_start_validation_error()
            .expect("duplicate keys must be rejected");
        assert!(
            message.contains("'FOO' is passed more than once"),
            "{message}"
        );

        let single = local_args(&["server", "start", "-e", "FOO=1", "-e", "BAR=2"]);
        assert_eq!(single.clickhouse_start_validation_error(), None);
    }

    #[test]
    fn client_direct_mode_accepts_user_and_password() {
        let LocalCommands::Client { user, password, .. } = local_command(&[
            "client",
            "--host",
            "db.example",
            "--user",
            "app",
            "--password",
            "secret",
        ]) else {
            panic!("expected client");
        };
        assert_eq!(user.as_deref(), Some("app"));
        assert_eq!(password.as_deref(), Some("secret"));

        // Credentials are direct-mode-only: they conflict with a NAME.
        assert_eq!(
            local_parse_error(&["client", "dev", "--user", "app"]).kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn parses_server_configs() {
        let LocalCommands::Server {
            command: ServerCommands::Configs,
        } = local_command(&["server", "configs"])
        else {
            panic!("expected server configs");
        };
    }

    #[test]
    fn server_takes_a_version_for_disambiguation() {
        for command in ["stop", "remove"] {
            let LocalCommands::Server { command: sub } =
                local_command(&["server", command, "dev", "--version", "26.8"])
            else {
                panic!("expected server {command}");
            };
            let (name, version) = match sub {
                ServerCommands::Stop { name, version, .. } => (name, version),
                ServerCommands::Remove { name, version, .. } => (name, version),
                _ => unreachable!("stop and remove are the only server subcommands with a name"),
            };
            assert_eq!(name.as_deref(), Some("dev"));
            assert_eq!(version.as_deref(), Some("26.8"));
        }
    }

    #[test]
    fn server_list_takes_no_flags() {
        let LocalCommands::Server {
            command: ServerCommands::List,
        } = local_command(&["server", "list"])
        else {
            panic!("expected server list");
        };
        assert_eq!(
            local_parse_error(&["server", "list", "--global"]).kind(),
            clap::error::ErrorKind::UnknownArgument
        );
    }

    // ── client (HTTP mode) ───────────────────────────────────────────────

    #[test]
    fn client_parses_query_file_and_database_independently() {
        let LocalCommands::Client {
            query,
            queries_file,
            database,
            ..
        } = local_command(&["client", "--query", "SELECT 1", "--database", "events"])
        else {
            panic!("expected client");
        };
        assert_eq!(query.as_deref(), Some("SELECT 1"));
        assert_eq!(queries_file, None);
        assert_eq!(database.as_deref(), Some("events"));
    }

    #[test]
    fn client_queries_file_accepts_dash_for_stdin() {
        let LocalCommands::Client { queries_file, .. } =
            local_command(&["client", "--queries-file", "-"])
        else {
            panic!("expected client");
        };
        assert_eq!(queries_file.as_deref(), Some("-"));
    }

    #[test]
    fn client_query_sources_are_mutually_exclusive() {
        assert_eq!(
            local_parse_error(&["client", "--query", "SELECT 1", "--queries-file", "q.sql"]).kind(),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn client_named_and_direct_selectors_conflict_in_every_order() {
        for selectors in [
            &["--name", "dev", "--host", "db.example"][..],
            &["--host", "db.example", "--name", "dev"][..],
            &["--name", "dev", "--port", "8123"][..],
            &["--port", "8123", "--name", "dev"][..],
            &["dev", "--host", "db.example"][..],
            &["--host", "db.example", "dev"][..],
        ] {
            let args: Vec<&str> = ["client"]
                .into_iter()
                .chain(selectors.iter().copied())
                .collect();
            assert_eq!(
                local_parse_error(&args).kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{selectors:?}"
            );
        }
    }

    #[test]
    fn client_direct_mode_defaults_the_other_selector() {
        let LocalCommands::Client { host, port, .. } =
            local_command(&["client", "--host", "db.example"])
        else {
            panic!("expected client");
        };
        assert_eq!(host.as_deref(), Some("db.example"));
        assert_eq!(port, None);

        let LocalCommands::Client { host, port, .. } = local_command(&["client", "--port", "8123"])
        else {
            panic!("expected client");
        };
        assert_eq!(host, None);
        assert_eq!(port, Some(8123));
    }

    #[test]
    fn client_version_disambiguates_named_mode() {
        // Unlike the binary-era client, --version works with a name: it picks
        // among Docker-managed instances sharing that name.
        let LocalCommands::Client { name, version, .. } =
            local_command(&["client", "dev", "--version", "26.8"])
        else {
            panic!("expected client");
        };
        assert_eq!(name.as_deref(), Some("dev"));
        assert_eq!(version.as_deref(), Some("26.8"));
    }

    #[test]
    fn client_rejects_zero_and_nonnumeric_ports() {
        for port in ["0", "not-a-port"] {
            let error = local_parse_error(&["client", "--port", port]);
            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
            assert!(error.to_string().contains("--port"), "{error}");
        }
    }

    // ── teardown name forms ──────────────────────────────────────────────

    #[test]
    fn server_teardown_name_forms_conflict() {
        for command in ["stop", "remove"] {
            let error = Cli::try_parse_from([
                "dctl",
                "local",
                "server",
                command,
                "positional",
                "--name",
                "flagged",
            ])
            .err()
            .expect("name forms should conflict");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
            assert!(error.to_string().contains("cannot be used with"), "{error}");
        }
    }

    #[test]
    fn server_teardown_help_hides_compatibility_name_flags() {
        for command in ["stop", "remove"] {
            let help = Cli::try_parse_from(["dctl", "local", "server", command, "--help"])
                .err()
                .expect("help should exit through clap")
                .to_string();

            assert!(!help.contains("--name"), "{help}");
        }
    }

    #[test]
    fn teardown_commands_preserve_explicit_names() {
        let LocalCommands::Server {
            command: ServerCommands::Stop {
                name, name_flag, ..
            },
        } = local_command(&["server", "stop", "analytics"])
        else {
            panic!("expected server stop");
        };
        assert_eq!(name.as_deref(), Some("analytics"));
        assert_eq!(name_flag, None);

        let LocalCommands::Postgres {
            command: PostgresCommands::Stop { name, .. },
        } = local_command(&["postgres", "stop", "warehouse"])
        else {
            panic!("expected postgres stop");
        };
        assert_eq!(name.as_deref(), Some("warehouse"));
    }

    // ── shared instance-command structure ────────────────────────────────

    #[test]
    fn postgres_start_rejects_invalid_name_tag_port_and_env_at_clap_time() {
        let cases: &[(&[&str], &str)] = &[
            (&["--name", "../unsafe"], "Invalid server name"),
            (
                &["--version", "18garbage"],
                "invalid or unsupported postgres version",
            ),
            (&["--port", "0"], "--port 0 is not allowed"),
            (&["--env", "NO_EQUALS"], "expected KEY=VALUE"),
            (
                &["--env", "POSTGRES_USER=admin"],
                "use --user instead of --env",
            ),
            (
                &["--env", "PGDATA=/tmp/postgres"],
                "PGDATA is managed by dctl",
            ),
        ];
        for (args, expected) in cases {
            let argv: Vec<&str> = ["postgres", "start"]
                .iter()
                .chain(args.iter())
                .copied()
                .collect();
            let error = local_parse_error(&argv);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{args:?}"
            );
            assert!(error.to_string().contains(expected), "{args:?}: {error}");
        }
    }

    #[test]
    fn all_instance_commands_accept_optional_positional_and_compatibility_names() {
        let commands = [
            &["server", "start"][..],
            &["server", "stop"][..],
            &["server", "remove"][..],
            &["server", "dotenv"][..],
            &["client"][..],
            &["postgres", "start"][..],
            &["postgres", "stop"][..],
            &["postgres", "remove"][..],
            &["postgres", "dotenv"][..],
            &["postgres", "client"][..],
            &["falkordb", "start"][..],
            &["falkordb", "stop"][..],
            &["falkordb", "remove"][..],
            &["falkordb", "dotenv"][..],
            &["falkordb", "client"][..],
        ];
        for command in commands {
            for name in ["default", "custom-name"] {
                for flag in [false, true] {
                    let mut args = command.to_vec();
                    if flag {
                        args.push("--name");
                    }
                    args.extend([name, "--json"]);
                    // Parsing succeeds with either form; the wrapper fields
                    // are consumed by the dispatch layer.
                    let argv: Vec<&str> = args.to_vec();
                    let parsed = local_args(&argv);
                    assert!(parsed.json, "{args:?}");
                }
            }
        }
    }

    #[test]
    fn all_instance_commands_reject_both_name_forms_even_when_equal() {
        let commands = [
            &["server", "start"][..],
            &["server", "stop"][..],
            &["server", "remove"][..],
            &["server", "dotenv"][..],
            &["client"][..],
            &["postgres", "start"][..],
            &["postgres", "stop"][..],
            &["postgres", "remove"][..],
            &["postgres", "dotenv"][..],
            &["postgres", "client"][..],
            &["falkordb", "start"][..],
            &["falkordb", "stop"][..],
            &["falkordb", "remove"][..],
            &["falkordb", "dotenv"][..],
            &["falkordb", "client"][..],
        ];
        for command in commands {
            for tail in [["dev", "--name", "dev"], ["--name", "dev", "dev"]] {
                let args: Vec<_> = command.iter().copied().chain(tail).collect();
                assert_eq!(
                    local_parse_error(&args).kind(),
                    clap::error::ErrorKind::ArgumentConflict,
                    "{args:?}"
                );
            }
        }
    }

    #[test]
    fn instance_help_advertises_optional_positional_and_hides_name_flags() {
        use clap::CommandFactory;
        let commands = [
            &["server", "start"][..],
            &["server", "stop"][..],
            &["server", "remove"][..],
            &["server", "dotenv"][..],
            &["client"][..],
            &["postgres", "start"][..],
            &["postgres", "stop"][..],
            &["postgres", "remove"][..],
            &["postgres", "dotenv"][..],
            &["postgres", "client"][..],
            &["falkordb", "start"][..],
            &["falkordb", "stop"][..],
            &["falkordb", "remove"][..],
            &["falkordb", "dotenv"][..],
            &["falkordb", "client"][..],
        ];
        let mut cli = Cli::command();
        cli.build();
        for path in commands {
            let mut command = cli.find_subcommand("local").unwrap();
            for part in path {
                command = command.find_subcommand(part).unwrap();
            }
            let name = command
                .get_arguments()
                .find(|arg| arg.get_id() == "name")
                .unwrap();
            assert_eq!(name.get_index(), Some(1), "{path:?}");
            assert!(!name.is_required_set(), "{path:?}");
            assert_eq!(name.get_value_names().unwrap()[0], "NAME", "{path:?}");
            let compatibility = command
                .get_arguments()
                .find(|arg| arg.get_id() == "name_flag")
                .unwrap();
            assert_eq!(compatibility.get_long(), Some("name"), "{path:?}");
            assert!(compatibility.is_hide_set(), "{path:?}");
            assert!(
                command
                    .clone()
                    .render_usage()
                    .to_string()
                    .contains("[NAME]"),
                "{path:?}"
            );
        }
    }

    #[test]
    fn client_short_name_compatibility_is_preserved() {
        for client in [&["client"][..], &["postgres", "client"][..]] {
            let argv: Vec<_> = client.iter().copied().chain(["-n", "dev"]).collect();
            let parsed = local_command(&argv);
            let (name, name_flag) = match parsed {
                LocalCommands::Client {
                    name, name_flag, ..
                } => (name, name_flag),
                LocalCommands::Postgres {
                    command:
                        PostgresCommands::Client {
                            name, name_flag, ..
                        },
                } => (name, name_flag),
                _ => unreachable!("client and postgres client are the only short-name commands"),
            };
            assert_eq!(name, None);
            assert_eq!(name_flag.as_deref(), Some("dev"));

            let argv: Vec<_> = client.iter().copied().chain(["dev", "-n", "dev"]).collect();
            assert_eq!(
                local_parse_error(&argv).kind(),
                clap::error::ErrorKind::ArgumentConflict
            );
        }
    }
}
