//! Structured output types for local commands.
//!
//! Successful output types support both JSON serialization and human-readable
//! display. Runtime failures use the redacted stable envelope below.

use crate::error::{Error, PortKind};
use serde::Serialize;
use std::fmt;
use std::io::Write;
use std::path::Path;
use tabled::{Table, Tabled, settings::Style};

const ABSENT: &str = "-";

fn or_absent<T: fmt::Display>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| ABSENT.to_string())
}

/// Stable codes for local runtime failures. New codes may be added, but
/// existing spellings and meanings are part of the machine-output contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LocalErrorCode {
    ServerNotFound,
    ServerNotRunning,
    ServerRunning,
    InvalidServerName,
    ConfigNotFound,
    InvalidConfigName,
    UnsupportedPlatform,
    PortInUse,
    StartupExit,
    StartupTimeout,
    DownloadFailed,
    NetworkError,
    DockerUnavailable,
    DockerError,
    /// The container name is held by a container dctl does not
    /// manage. Distinct from [`Self::DockerError`], which covers daemon
    /// failures whose text is not rendered.
    ContainerNameConflict,
    /// A Postgres validation or state error whose text (and recovery
    /// guidance) dctl composes itself, rendered verbatim.
    PostgresError,
    /// A FalkorDB validation or state error; text is dctl's own.
    FalkorError,
    /// A Docker-managed ClickHouse validation or state error.
    ClickhouseError,
    SqlInputOpenFailed,
    /// A managed server metadata file contains invalid JSON. The structured
    /// body names the file and gives conservative recovery guidance without
    /// exposing serde's source text.
    ServerMetadataInvalid,
    IoError,
    LocalError,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct LocalErrorDetail {
    code: LocalErrorCode,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

/// How one [`Error`] variant renders into a [`LocalErrorDetail`].
///
/// Built with either [`Mapping::parity`] — the JSON message is the error's own
/// human text, verbatim — or [`Mapping::redacted`], which substitutes a curated
/// summary for errors that interpolate foreign text.
struct Mapping {
    code: LocalErrorCode,
    command: Option<String>,
    /// Curated replacement for the human text; `None` renders `Display`.
    redacted: Option<String>,
}

impl Mapping {
    /// The JSON message is the error's `Display` text, so machine output
    /// carries exactly the detail and remediation human output prints.
    fn parity(code: LocalErrorCode) -> Self {
        Self {
            code,
            command: None,
            redacted: None,
        }
    }

    /// The JSON message is `message`, not the error's `Display` text. For
    /// errors whose text interpolates foreign output.
    fn redacted(code: LocalErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            command: None,
            redacted: Some(message.into()),
        }
    }

    /// A safe, runnable recovery command for this failure.
    fn command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    fn into_detail(self, error: &Error) -> LocalErrorDetail {
        LocalErrorDetail {
            code: self.code,
            message: self.redacted.unwrap_or_else(|| error.to_string()),
            command: self.command,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalProjectScopeKind {
    ExactCurrentProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ServerProjectScope {
    kind: LocalProjectScopeKind,
    path: String,
    parent_projects_searched: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct LocalGuidance {
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct ServerMetadataParseErrorDetail {
    code: LocalErrorCode,
    message: &'static str,
    path: String,
    guidance: Vec<LocalGuidance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LocalGuidanceAction {
    ListProjectServers,
    ListGlobalServers,
    ReturnToProjectRoot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectServerGuidance {
    action: LocalGuidanceAction,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum LocalErrorBody {
    General(LocalErrorDetail),
    ServerMetadataParse(ServerMetadataParseErrorDetail),
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct LocalErrorOutput {
    error: LocalErrorBody,
}

impl LocalErrorOutput {
    /// Classify one local runtime failure.
    ///
    /// Two rules govern the `message` field, and every arm below picks one
    /// deliberately:
    ///
    /// * **Parity** ([`Mapping::parity`]) — the error's own human text is
    ///   rendered verbatim, so a JSON consumer gets exactly the detail and
    ///   remediation human mode prints. This is the default: these messages are
    ///   composed by this crate from its own fields.
    /// * **Redaction** ([`Mapping::redacted`]) — a curated summary replaces
    ///   text that interpolates foreign output (subprocess stderr, Docker
    ///   daemon or OS/serde source strings, download bodies), which can carry
    ///   paths, SQL or credentials and tells a machine consumer nothing.
    ///
    /// The match is exhaustive on purpose: a new [`Error`] variant must be
    /// classified here rather than silently collapsing to
    /// `local_error`/"Local command failed".
    fn from_error(error: &Error) -> Self {
        let mapping = match error {
            // ── structured bodies (their own DTOs, not code/message/command) ─
            Error::ServerMetadataParse { path, .. } => {
                return Self {
                    error: LocalErrorBody::ServerMetadataParse(
                        ServerMetadataParseErrorDetail::from_path(path),
                    ),
                };
            }
            // The rollback note is a cleanup detail on top of the failure that
            // actually stopped the command; classify by that primary failure.
            Error::PostgresStartupRollback { primary, .. } => {
                return Self::from_error(primary);
            }
            Error::FalkorStartupRollback { primary, .. } => {
                return Self::from_error(primary);
            }
            Error::ClickhouseStartupRollback { primary, .. } => {
                return Self::from_error(primary);
            }

            // ── servers ─────────────────────────────────────────────────────
            Error::ServerNotFound(_) => {
                Mapping::parity(LocalErrorCode::ServerNotFound).command("dctl local server list")
            }
            // Deliberately the list, not a `start` command: this variant is
            // also raised for Postgres servers and for global PID lookups,
            // where the name is not a `server start` argument.
            Error::ServerNotRunning(_) => {
                Mapping::parity(LocalErrorCode::ServerNotRunning).command("dctl local server list")
            }
            Error::ServerAlreadyRunning(_) => {
                Mapping::parity(LocalErrorCode::ServerRunning).command("dctl local server list")
            }
            // Stopping *this* server is the recovery; `server list` only
            // restates what the error already says.
            Error::ServerRunningCannotRemove { command, .. } => {
                Mapping::parity(LocalErrorCode::ServerRunning).command(command.clone())
            }
            Error::InvalidServerName(_) => {
                Mapping::parity(LocalErrorCode::InvalidServerName).command("dctl local server list")
            }
            // ── server configs ──────────────────────────────────────────────
            Error::ConfigNotFound(_) => {
                Mapping::parity(LocalErrorCode::ConfigNotFound).command("dctl local server configs")
            }
            Error::InvalidConfigName(_) => Mapping::parity(LocalErrorCode::InvalidConfigName)
                .command("dctl local server configs"),

            Error::UnsupportedPlatform { .. } => {
                Mapping::parity(LocalErrorCode::UnsupportedPlatform)
            }

            // ── ports and startup ───────────────────────────────────────────
            Error::PortInUse { kind, .. } | Error::PortUnavailable(kind) => {
                Mapping::parity(LocalErrorCode::PortInUse).command(match kind {
                    PortKind::Postgres => "dctl local postgres start --help",
                    PortKind::Falkordb | PortKind::FalkordbBrowser => {
                        "dctl local falkordb start --help"
                    }
                    PortKind::Clickhouse | PortKind::Http => "dctl local server start --help",
                })
            }
            // `details` is the managed server's own stderr or log tail: kept in
            // human output, summarized here.
            Error::StartupExit { kind, name, .. } => Mapping::redacted(
                LocalErrorCode::StartupExit,
                format!("{kind} server '{name}' exited before becoming ready"),
            )
            .command("dctl local server list"),
            Error::StartupTimeout {
                kind,
                name,
                seconds,
                ..
            } => Mapping::redacted(
                LocalErrorCode::StartupTimeout,
                format!("{kind} server '{name}' did not become ready within {seconds} seconds"),
            )
            .command("dctl local server list"),

            // ── network, downloads and extraction ───────────────────────────
            Error::Http(_) => {
                Mapping::redacted(LocalErrorCode::NetworkError, "HTTP request failed")
            }
            Error::Download(_) => {
                Mapping::redacted(LocalErrorCode::DownloadFailed, "Download failed")
            }
            Error::Extract(_) => {
                Mapping::redacted(LocalErrorCode::DownloadFailed, "Extraction failed")
            }

            // ── Docker ──────────────────────────────────────────────────────
            // The unavailability text is built from a classified failure kind
            // and platform guidance; the daemon's own message is used for
            // classification only and never rendered (see `local::docker`).
            Error::DockerNotAvailable(_) => Mapping::parity(LocalErrorCode::DockerUnavailable),
            Error::DockerError(_) => {
                Mapping::redacted(LocalErrorCode::DockerError, "Docker operation failed")
            }
            // Self-composed name-conflict guidance, unlike the daemon text
            // above.
            Error::ContainerNameConflict(_) => {
                Mapping::parity(LocalErrorCode::ContainerNameConflict)
            }

            // ── filesystem and metadata ─────────────────────────────────────
            Error::Io(_)
            | Error::Json(_)
            | Error::ServerMetadataPermission { .. }
            | Error::ServerMetadataRead { .. }
            | Error::ServerMetadataUtf8 { .. }
            | Error::ServerMetadataWrite { .. }
            | Error::ServerLock { .. } => {
                Mapping::redacted(LocalErrorCode::IoError, "Local I/O operation failed")
            }

            // ── registry (ADR-0008) ─────────────────────────────────────────
            Error::Registry(_) => {
                Mapping::redacted(LocalErrorCode::LocalError, "Registry operation failed")
            }

            // ── postgres ────────────────────────────────────────────────────
            // Self-composed validation and state guidance; the foreign-text
            // sibling `Error::Postgres` stays in the fallback below.
            Error::PostgresUsage(_) => Mapping::parity(LocalErrorCode::PostgresError),

            // ── falkordb ────────────────────────────────────────────────────
            Error::FalkorUsage(_) => Mapping::parity(LocalErrorCode::FalkorError),
            Error::ClickhouseUsage(_) => Mapping::parity(LocalErrorCode::ClickhouseError),
            // The body is engine output (may interpolate SQL/paths); parity
            // is reserved for text this crate composes itself.
            Error::ClickhouseHttp { status, .. } => Mapping::redacted(
                LocalErrorCode::ClickhouseError,
                format!("ClickHouse HTTP query failed with status {status}"),
            ),
            Error::SqlInputOpen { .. } => Mapping::redacted(
                LocalErrorCode::SqlInputOpenFailed,
                "Could not open SQL input file; check that --queries-file exists and is readable",
            ),

            // ── bounded fallback ────────────────────────────────────────────
            // Subprocess text and `Postgres` (OS text from a failed psql
            // exec) are foreign output. `Skills` and `Ledger` belong to other
            // command surfaces and are never printed through this envelope; `ChildExit` passes the child's status through
            // without an error object at all.
            Error::Postgres(_) | Error::Skills(_) | Error::Ledger(_) | Error::ChildExit(_) => {
                Mapping::redacted(LocalErrorCode::LocalError, "Local command failed")
            }
        };
        Self {
            error: LocalErrorBody::General(mapping.into_detail(error)),
        }
    }
}

impl ServerMetadataParseErrorDetail {
    fn from_path(path: &Path) -> Self {
        Self {
            code: LocalErrorCode::ServerMetadataInvalid,
            message: "Server metadata is not valid JSON",
            path: path.display().to_string(),
            guidance: vec![
                LocalGuidance {
                    message: "Repair the metadata file, then retry",
                    command: None,
                },
                LocalGuidance {
                    message: "For ClickHouse, if repair is not possible, confirm that the container is discoverable before moving the metadata file aside",
                    command: Some("docker ps --filter label=dctl.engine=clickhouse"),
                },
                LocalGuidance {
                    message: "For Postgres, verify the container state separately before moving the metadata file aside",
                    command: None,
                },
                LocalGuidance {
                    message: "Retry from the owning project; ClickHouse recovery requires the server to remain running and discoverable",
                    command: Some("dctl local server list"),
                },
            ],
        }
    }
}

pub(crate) fn exact_current_project_scope(project_dir: &Path) -> ServerProjectScope {
    ServerProjectScope {
        kind: LocalProjectScopeKind::ExactCurrentProject,
        path: project_dir.display().to_string(),
        parent_projects_searched: false,
    }
}

pub(crate) fn project_scope_guidance() -> Vec<ProjectServerGuidance> {
    let guidance = vec![
        ProjectServerGuidance {
            action: LocalGuidanceAction::ReturnToProjectRoot,
            message: "Change to the local project root where the server was started",
            command: Some("cd <project-root>"),
        },
        ProjectServerGuidance {
            action: LocalGuidanceAction::ListProjectServers,
            message: "List servers after returning to that exact project",
            command: Some("dctl local server list"),
        },
        ProjectServerGuidance {
            action: LocalGuidanceAction::ListGlobalServers,
            message: "List servers of every engine after returning to that project",
            command: Some("dctl local server list"),
        },
    ];
    guidance
}

/// Write exactly one local runtime error object to stderr. The serialized DTO
/// is allowlisted above and never includes an error source or arbitrary detail.
pub fn print_error(error: &Error) {
    let output = LocalErrorOutput::from_error(error);
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    if serde_json::to_writer_pretty(&mut stderr, &output).is_ok() {
        let _ = writeln!(stderr);
    }
}

// ── list (installed) ────────────────────────────────────────────────────────

// ── list --remote ───────────────────────────────────────────────────────────

// ── which ───────────────────────────────────────────────────────────────────

// ── install ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InstallOutput {
    pub version: String,
    pub set_as_default: bool,
}

impl fmt::Display for InstallOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Installed version {}", self.version)?;
        if self.set_as_default {
            write!(f, " (set as default)")?;
        }
        Ok(())
    }
}

// ── use ─────────────────────────────────────────────────────────────────────

// ── remove ──────────────────────────────────────────────────────────────────

// ── init ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InitOutput {
    /// Every project-local path this invocation created, e.g. `.dctl/`,
    /// `.dctl/.gitignore`, `clickhouse/`, or `postgres/`.
    pub paths: Vec<String>,
    /// Human-output detail only: the project dir already existed before this
    /// run. This affects human wording only, so it is not serialized.
    #[serde(skip)]
    pub already_initialized: bool,
}

impl fmt::Display for InitOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.already_initialized {
            write!(f, "Initialized ClickHouse project in .dctl/")?;
        } else if self.paths.iter().any(|path| path == ".dctl/.gitignore") {
            write!(f, "Restored runtime ignore at .dctl/.gitignore")?;
        } else {
            write!(f, "Already initialized at .dctl/")?;
        }
        for path in self.paths.iter().filter(|path| !path.starts_with(".dctl/")) {
            write!(f, "\nCreated project scaffold in {path}")?;
        }
        Ok(())
    }
}

// ── server configs ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ServerConfigsOutput {
    pub dir: String,
    pub configs: Vec<String>,
}

impl fmt::Display for ServerConfigsOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.configs.is_empty() {
            writeln!(f, "No config files in {}", self.dir)?;
            write!(
                f,
                "Create an XML or YAML file there with only the settings you want to change.\n\
                 Other settings inherit ClickHouse's built-in defaults.\n\
                 Start with: \
                 dctl local server start --config <NAME>"
            )?;
            return Ok(());
        }
        writeln!(f, "Config files in {}:", self.dir)?;
        for name in &self.configs {
            writeln!(f, "  {name}")?;
        }
        write!(f, "Use with: dctl local server start --config <NAME>")
    }
}

// ── server start ────────────────────────────────────────────────────────────

// ── server list ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ServerListEntry {
    pub name: String,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// "clickhouse", "postgres", or "falkordb".
    pub engine: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerListOutput {
    pub servers: Vec<ServerListEntry>,
    pub total_servers: usize,
    pub total_running_servers: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project_scope: Option<ServerProjectScope>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) guidance: Vec<ProjectServerGuidance>,
}

#[derive(Tabled)]
struct ServerListRow {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "PID")]
    pid: String,
    #[tabled(rename = "Version")]
    version: String,
    #[tabled(rename = "HTTP Port")]
    http_port: String,
    #[tabled(rename = "TCP Port")]
    tcp_port: String,
}

#[derive(Tabled)]
struct ServerListRowWithEngine {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Engine")]
    engine: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "ID")]
    pid_or_container: String,
    #[tabled(rename = "Version")]
    version: String,
    #[tabled(rename = "HTTP Port")]
    http_port: String,
    #[tabled(rename = "TCP Port")]
    tcp_port: String,
}

#[derive(Tabled)]
struct ServerListRowGlobal {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "PID")]
    pid: String,
    #[tabled(rename = "Version")]
    version: String,
    #[tabled(rename = "HTTP Port")]
    http_port: String,
    #[tabled(rename = "TCP Port")]
    tcp_port: String,
    #[tabled(rename = "Project")]
    project: String,
}

impl fmt::Display for ServerListOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.servers.is_empty() {
            if let Some(scope) = &self.project_scope {
                writeln!(f, "No servers found in project '{}'.", scope.path)?;
                writeln!(
                    f,
                    "Project-local server list uses the exact current working directory; parent `.dctl` directories are not searched."
                )?;
                return write!(
                    f,
                    "Return to the local project root where the server was started and run `dctl local server list`; containers can also be located with `docker ps --filter label=created_by`."
                );
            }
            write!(f, "No servers")?;
            return Ok(());
        }

        let has_project = self.servers.iter().any(|e| e.project.is_some());
        let has_docker_engine = self.servers.iter().any(|e| e.engine != "clickhouse");

        if !has_project && has_docker_engine {
            // Show an engine-aware table that combines PID (ClickHouse) and
            // container short-id (Docker engines) into a single "ID" column.
            let rows: Vec<ServerListRowWithEngine> = self
                .servers
                .iter()
                .map(|e| {
                    let id = if e.engine != "clickhouse" {
                        e.container_id
                            .as_deref()
                            .map(|s| s.chars().take(12).collect::<String>())
                            .unwrap_or_else(|| ABSENT.to_string())
                    } else {
                        or_absent(e.pid)
                    };
                    ServerListRowWithEngine {
                        name: e.name.clone(),
                        engine: e.engine.clone(),
                        status: if e.running {
                            "running".into()
                        } else {
                            "stopped".into()
                        },
                        pid_or_container: id,
                        version: or_absent(e.version.as_deref()),
                        http_port: or_absent(e.http_port),
                        tcp_port: or_absent(e.tcp_port),
                    }
                })
                .collect();
            let table = Table::new(rows).with(Style::markdown()).to_string();
            writeln!(f, "{table}")?;
            return write!(
                f,
                "\n{} server{}, {} running",
                self.total_servers,
                if self.total_servers == 1 { "" } else { "s" },
                self.total_running_servers
            );
        }

        if has_project {
            let rows: Vec<ServerListRowGlobal> = self
                .servers
                .iter()
                .map(|e| ServerListRowGlobal {
                    name: e.name.clone(),
                    status: if e.running {
                        "running".to_string()
                    } else {
                        "stopped".to_string()
                    },
                    pid: or_absent(e.pid),
                    version: or_absent(e.version.as_deref()),
                    http_port: or_absent(e.http_port),
                    tcp_port: or_absent(e.tcp_port),
                    project: or_absent(e.project.as_deref()),
                })
                .collect();
            let table = Table::new(rows).with(Style::markdown()).to_string();
            writeln!(f, "{table}")?;
        } else {
            let rows: Vec<ServerListRow> = self
                .servers
                .iter()
                .map(|e| ServerListRow {
                    name: e.name.clone(),
                    status: if e.running {
                        "running".to_string()
                    } else {
                        "stopped".to_string()
                    },
                    pid: or_absent(e.pid),
                    version: or_absent(e.version.as_deref()),
                    http_port: or_absent(e.http_port),
                    tcp_port: or_absent(e.tcp_port),
                })
                .collect();
            let table = Table::new(rows).with(Style::markdown()).to_string();
            writeln!(f, "{table}")?;
        }

        write!(
            f,
            "\n{} server{}, {} running",
            self.total_servers,
            if self.total_servers == 1 { "" } else { "s" },
            self.total_running_servers
        )
    }
}

// ── postgres start ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PostgresStartOutput {
    pub name: String,
    pub container_id: String,
    pub image: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
}

impl fmt::Display for PostgresStartOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = self.container_id.chars().take(12).collect::<String>();
        writeln!(f, "Postgres '{}' running (container: {})", self.name, short)?;
        writeln!(f, "  Image:    {}", self.image)?;
        writeln!(f, "  Port:     {}", self.port)?;
        writeln!(f, "  User:     {}", self.user)?;
        writeln!(f, "  Password: {}", self.password)?;
        writeln!(f, "  Database: {}", self.database)?;
        write!(f, "  Connect:  dctl local postgres client {}", self.name)
    }
}

// ── falkordb start ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FalkorStartOutput {
    pub name: String,
    pub container_id: String,
    pub image: String,
    pub port: u16,
    pub browser_port: u16,
    pub password: String,
}

impl fmt::Display for FalkorStartOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = self.container_id.chars().take(12).collect::<String>();
        writeln!(f, "FalkorDB '{}' running (container: {})", self.name, short)?;
        writeln!(f, "  Image:   {}", self.image)?;
        writeln!(f, "  Port:    {}", self.port)?;
        writeln!(f, "  Browser: http://127.0.0.1:{}", self.browser_port)?;
        writeln!(f, "  Password: {}", self.password)?;
        write!(
            f,
            "  Connect:  dctl local falkordb client {} -q 'PING'",
            self.name
        )
    }
}

// ── clickhouse start ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ClickhouseStartOutput {
    pub name: String,
    pub container_id: String,
    pub image: String,
    pub http_port: u16,
    pub native_port: u16,
    pub user: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password: String,
    pub database: String,
}

impl fmt::Display for ClickhouseStartOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = self.container_id.chars().take(12).collect::<String>();
        writeln!(
            f,
            "ClickHouse '{}' running (container: {})",
            self.name, short
        )?;
        writeln!(f, "  Image:    {}", self.image)?;
        writeln!(f, "  HTTP:     {}", self.http_port)?;
        writeln!(f, "  Native:   {}", self.native_port)?;
        if !self.password.is_empty() {
            writeln!(f, "  Password: {}", self.password)?;
        }
        write!(f, "  Query:    dctl local client -q 'SELECT 1'")
    }
}

// ── clickhouse dotenv ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ClickhouseDotenvOutput {
    pub file: String,
    pub server: String,
    pub vars: Vec<DotenvVar>,
}

impl fmt::Display for ClickhouseDotenvOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Wrote to {} (clickhouse '{}')", self.file, self.server)?;
        for var in &self.vars {
            writeln!(f, "  {}={}", var.key, var.value)?;
        }
        Ok(())
    }
}

// ── falkordb dotenv ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FalkorDotenvOutput {
    pub file: String,
    pub server: String,
    pub vars: Vec<DotenvVar>,
}

impl fmt::Display for FalkorDotenvOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Wrote to {} (falkordb '{}')", self.file, self.server)?;
        for var in &self.vars {
            writeln!(f, "  {}={}", var.key, var.value)?;
        }
        Ok(())
    }
}

// ── postgres dotenv ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct PostgresDotenvOutput {
    pub file: String,
    pub server: String,
    pub vars: Vec<DotenvVar>,
}

impl fmt::Display for PostgresDotenvOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Wrote to {} (postgres '{}')", self.file, self.server)?;
        for var in &self.vars {
            writeln!(f, "  {}={}", var.key, var.value)?;
        }
        Ok(())
    }
}

// ── server stop ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerSelection {
    Implicit,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStopOutput {
    pub name: String,
    /// True when the server existed but was already stopped (idempotent noop).
    pub already_stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<ServerSelection>,
}

impl fmt::Display for ServerStopOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.already_stopped {
            write!(f, "Server '{}' is already stopped", self.name)?;
        } else {
            write!(f, "Server '{}' stopped", self.name)?;
        }
        if self.selection == Some(ServerSelection::Implicit) {
            write!(f, " (selected automatically)")?;
        }
        Ok(())
    }
}

// ── server stop-all ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ServerStopEntry {
    pub name: String,
    /// "clickhouse" or "postgres".
    pub engine: String,
    /// Postgres image version, used to distinguish same-name major versions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStopAllOutput {
    pub servers: Vec<ServerStopEntry>,
}

impl fmt::Display for ServerStopAllOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.servers.is_empty() {
            write!(f, "No running servers")?;
            return Ok(());
        }
        for s in &self.servers {
            let engine = match s.version.as_deref() {
                Some(version) => format!("{}, {}", s.engine, version),
                None => s.engine.clone(),
            };
            if s.stopped {
                writeln!(f, "Stopping '{}' ({})... stopped", s.name, engine)?;
            } else {
                writeln!(
                    f,
                    "Stopping '{}' ({})... error: {}",
                    s.name,
                    engine,
                    s.error.as_deref().unwrap_or("unknown")
                )?;
            }
        }
        write!(f, "Done")
    }
}

// ── server remove ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ServerRemoveOutput {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<ServerSelection>,
}

impl fmt::Display for ServerRemoveOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Server '{}' removed", self.name)?;
        if self.selection == Some(ServerSelection::Implicit) {
            write!(f, " (selected automatically)")?;
        }
        Ok(())
    }
}

// ── server dotenv ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DotenvVar {
    pub key: String,
    pub value: String,
}

// ── helper ──────────────────────────────────────────────────────────────────

/// Print output as JSON or human-readable text.
pub fn print_output(output: &(impl Serialize + fmt::Display), json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(output).expect("JSON serialization failed")
        );
    } else {
        println!("{}", output);
    }
}

/// Single aligned table block (header, dash separator, rows) shared by the
/// native Postgres and FalkorDB clients; lines are trimmed like psql output
/// and numeric-looking cells right-align when `numeric_align` is set. The
/// row-count footer belongs to the caller.
pub(crate) fn render_aligned_table(
    columns: &[String],
    rows: &[Vec<Option<String>>],
    numeric_align: bool,
) -> String {
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.as_deref().map_or(0, str::len));
        }
    }
    let mut out = String::new();
    let header: Vec<String> = columns
        .iter()
        .zip(&widths)
        .map(|(name, width)| format!("{name:<width$}"))
        .collect();
    out.push_str(header.join(" | ").trim_end());
    out.push('\n');
    let dashes: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    out.push_str(&dashes.join("-+-"));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(value, width)| match value {
                Some(text) if numeric_align && looks_numeric(text) => {
                    format!("{text:>width$}")
                }
                Some(text) => format!("{text:<width$}"),
                None => " ".repeat(*width),
            })
            .collect();
        out.push_str(cells.join(" | ").trim_end());
        out.push('\n');
    }
    out
}

/// psql right-aligns values that look like numbers; keep that affordance.
pub(crate) fn looks_numeric(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        && text.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_json(error: &Error) -> serde_json::Value {
        serde_json::to_value(LocalErrorOutput::from_error(error)).unwrap()
    }

    #[test]
    fn self_composed_errors_serialize_their_human_message_verbatim() {
        for (error, expected) in [
            (
                Error::ConfigNotFound("config 'x' not found".into()),
                "config_not_found",
            ),
            (
                Error::InvalidConfigName("../etc/passwd".into()),
                "invalid_config_name",
            ),
            (
                Error::InvalidServerName("../escape".into()),
                "invalid_server_name",
            ),
            (
                Error::DockerNotAvailable("Docker socket was not found.\nStart Docker.".into()),
                "docker_unavailable",
            ),
            (
                Error::ContainerNameConflict("dctl-pg-dev-17".into()),
                "container_name_conflict",
            ),
            (
                Error::PostgresUsage("raw postgres guidance".into()),
                "postgres_error",
            ),
            (
                Error::FalkorUsage("raw falkordb guidance".into()),
                "falkor_error",
            ),
            (
                Error::ClickhouseUsage("raw clickhouse guidance".into()),
                "clickhouse_error",
            ),
        ] {
            let json = error_json(&error);
            assert_eq!(json["error"]["code"], expected, "{error}");
            assert_eq!(
                json["error"]["message"],
                error.to_string(),
                "self-composed text must render at parity: {error}"
            );
        }
    }

    #[test]
    fn errors_carrying_foreign_output_stay_summarized() {
        for (error, expected) in [
            (
                Error::Postgres("psql: connection refused".into()),
                "local_error",
            ),
            (
                Error::Download("raw download details".into()),
                "download_failed",
            ),
            (
                Error::Extract("raw extraction details".into()),
                "download_failed",
            ),
            (
                Error::StartupExit {
                    kind: crate::error::StartupKind::ClickHouse,
                    name: "default".into(),
                    details: "raw startup details".into(),
                },
                "startup_exit",
            ),
            (
                Error::StartupTimeout {
                    kind: crate::error::StartupKind::Postgres,
                    name: "default".into(),
                    seconds: 60,
                    details: "raw timeout details".into(),
                },
                "startup_timeout",
            ),
            (
                Error::PortInUse {
                    kind: PortKind::Http,
                    port: 8123,
                },
                "port_in_use",
            ),
        ] {
            let json = error_json(&error);
            assert_eq!(json["error"]["code"], expected, "{error}");
        }
    }

    #[test]
    fn rollback_errors_classify_by_their_primary_failure() {
        let wrapped = Error::PostgresStartupRollback {
            primary: Box::new(Error::ServerNotRunning("default-pg18".into())),
            cleanup: "container removed; data kept".into(),
        };
        let json = error_json(&wrapped);
        assert_eq!(json["error"]["code"], "server_not_running");
    }

    #[test]
    fn server_metadata_parse_gets_a_structured_body_without_serde_text() {
        let error = Error::ServerMetadataParse {
            path: "/work/.dctl/servers/default.json".into(),
            source: serde_json::from_str::<serde_json::Value>("{")
                .expect_err("invalid fixture must fail to parse"),
        };
        let json = error_json(&error);
        assert_eq!(json["error"]["code"], "server_metadata_invalid");
        assert_eq!(json["error"]["path"], "/work/.dctl/servers/default.json");
        assert_eq!(
            json["error"]["message"],
            "Server metadata is not valid JSON"
        );
    }

    #[test]
    fn install_output_renders_engine_and_tag() {
        let out = InstallOutput {
            version: "clickhouse@26.8".into(),
            set_as_default: false,
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["version"], "clickhouse@26.8");
        assert_eq!(json["set_as_default"], false);
    }

    #[test]
    fn clickhouse_start_output_renders_ports_and_credentials() {
        let out = ClickhouseStartOutput {
            name: "default".into(),
            container_id: "abc123".into(),
            image: "clickhouse:26.8".into(),
            http_port: 8123,
            native_port: 9000,
            user: "default".into(),
            password: "generated".into(),
            database: "default".into(),
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["name"], "default");
        assert_eq!(json["http_port"], 8123);
        assert_eq!(json["native_port"], 9000);
        assert_eq!(json["user"], "default");
        assert_eq!(json["database"], "default");
    }

    #[test]
    fn clickhouse_dotenv_output_lists_managed_vars() {
        let out = ClickhouseDotenvOutput {
            file: ".env".into(),
            server: "default".into(),
            vars: vec![
                DotenvVar {
                    key: "CLICKHOUSE_HOST".into(),
                    value: "127.0.0.1".into(),
                },
                DotenvVar {
                    key: "CLICKHOUSE_HTTP_PORT".into(),
                    value: "8123".into(),
                },
            ],
        };
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["file"], ".env");
        assert_eq!(json["vars"][0]["key"], "CLICKHOUSE_HOST");
    }

    #[test]
    fn server_stop_output_display_names_the_server() {
        let out = ServerStopOutput {
            name: "analytics".into(),
            already_stopped: false,
            selection: None,
        };
        assert_eq!(out.to_string(), "Server 'analytics' stopped");
    }

    #[test]
    fn server_configs_output_lists_directory_and_names() {
        let out = ServerConfigsOutput {
            dir: "/home/dev/.dctl/configs".into(),
            configs: vec!["analytics.xml".into()],
        };
        assert_eq!(out.configs, ["analytics.xml"]);
    }

    #[test]
    fn server_list_output_reports_engine_counts() {
        let out = ServerListOutput {
            servers: vec![
                ServerListEntry {
                    name: "default".into(),
                    running: true,
                    pid: None,
                    version: Some("clickhouse:26.8".into()),
                    http_port: Some(8123),
                    tcp_port: Some(9000),
                    project: None,
                    engine: "clickhouse".into(),
                    container_id: Some("abc".into()),
                },
                ServerListEntry {
                    name: "default".into(),
                    running: false,
                    pid: None,
                    version: Some("postgres:18".into()),
                    http_port: None,
                    tcp_port: Some(5432),
                    project: None,
                    engine: "postgres".into(),
                    container_id: Some("def".into()),
                },
            ],
            total_servers: 2,
            total_running_servers: 1,
            project_scope: None,
            guidance: Vec::new(),
        };
        assert_eq!(out.total_servers, 2);
        assert_eq!(out.total_running_servers, 1);
    }
}
