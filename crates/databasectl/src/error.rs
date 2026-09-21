use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    Http,
    Postgres,
    Falkordb,
    /// The ClickHouse native TCP port (9000).
    Clickhouse,
    /// The FalkorDB Browser UI port (3000); a distinct kind so machine
    /// envelopes point at `falkordb start --help`, not the ClickHouse hint.
    FalkordbBrowser,
}

impl PortKind {
    fn human_guidance(self) -> &'static str {
        match self {
            Self::Postgres => "; choose another --port or omit --port to auto-select a free port",
            Self::Falkordb | Self::FalkordbBrowser | Self::Clickhouse | Self::Http => {
                "; choose another port or omit it to auto-select a free port"
            }
        }
    }
}

impl fmt::Display for PortKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Http => "HTTP",
            Self::Postgres => "Postgres",
            Self::Falkordb => "FalkorDB",
            Self::FalkordbBrowser => "FalkorDB Browser",
            Self::Clickhouse => "ClickHouse",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupKind {
    ClickHouse,
    Postgres,
    Falkordb,
}

impl fmt::Display for StartupKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ClickHouse => "ClickHouse",
            Self::Postgres => "Postgres",
            Self::Falkordb => "FalkorDB",
        })
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    /// Self-update supports a bounded platform matrix; anything else lands
    /// here instead of an OS error at download time.
    #[error("Unsupported platform: {os}/{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    /// Self-update and image pulls report transport failures through this
    /// variant; the payload is dctl-composed context around the underlying
    /// transport error.
    #[error("Download failed: {0}")]
    Download(String),

    #[error("Extraction failed: {0}")]
    Extract(String),

    #[error("{kind} port {port} is already in use{}", kind.human_guidance())]
    PortInUse { kind: PortKind, port: u16 },

    #[error("Could not find a free {0} port")]
    PortUnavailable(PortKind),

    #[error("{details}")]
    StartupExit {
        kind: StartupKind,
        name: String,
        details: String,
    },

    #[error("{details}")]
    StartupTimeout {
        kind: StartupKind,
        name: String,
        seconds: u64,
        details: String,
    },

    /// Postgres failure text dctl does not control (currently the OS
    /// error from a failed `psql` exec). Summarized in structured output.
    #[error("Postgres error: {0}")]
    Postgres(String),

    /// SQL file paths and OS errors stay in human diagnostics only.
    #[error("could not open SQL file {path:?}: {source}")]
    SqlInputOpen {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read SQL input: {0}")]
    SqlInputRead(#[source] std::io::Error),

    /// A Postgres validation or state error whose text dctl composes
    /// itself, including its recovery guidance. Kept separate from
    /// [`Error::Postgres`] so structured output can render it verbatim.
    #[error("Postgres error: {0}")]
    PostgresUsage(String),

    /// A FalkorDB validation or state error whose text dctl composes
    /// itself, including its recovery guidance; rendered verbatim in
    /// structured output, like [`Error::PostgresUsage`].
    #[error("FalkorDB error: {0}")]
    FalkorUsage(String),

    /// A Docker-managed ClickHouse validation or state error.
    #[error("ClickHouse error: {0}")]
    ClickhouseUsage(String),

    /// A ClickHouse HTTP query failed with a non-success status. The body is
    /// the engine's own error text — kept for human output, summarized in
    /// structured output (the redacted arm in local::output), like the
    /// Docker daemon text in [`Error::DockerError`].
    #[error("ClickHouse HTTP {status}: {body}")]
    ClickhouseHttp { status: u16, body: String },

    /// A child process whose status must be returned unchanged. This is
    /// intentionally not printed as a dctl error by `run_parsed`.
    #[error("child process exited with code {0}")]
    ChildExit(i32),

    #[error("Server '{0}' is not running")]
    ServerNotRunning(String),

    #[error("Server '{0}' not found")]
    ServerNotFound(String),

    #[error("Server '{0}' is already running")]
    ServerAlreadyRunning(String),

    #[error("Server '{name}' is running; stop it first with `{command}`")]
    ServerRunningCannotRemove { name: String, command: String },

    #[error(
        "Permission denied reading server metadata '{}': {source}. Check ownership and file permissions, then retry.",
        path.display()
    )]
    ServerMetadataPermission {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "Failed to read server metadata '{}': {source}. Check that the file is readable, then retry.",
        path.display()
    )]
    ServerMetadataRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "Server metadata '{}' is not valid UTF-8: {source}. Repair or remove the metadata file, then retry.",
        path.display()
    )]
    ServerMetadataUtf8 {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },

    #[error(
        "Server metadata '{}' is not valid JSON: {source}. Repair the metadata file, then retry.",
        path.display()
    )]
    ServerMetadataParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("Failed to durably update server metadata '{}': {source}", path.display())]
    ServerMetadataWrite {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Could not {operation} at '{}': {source}. {remediation}", path.display())]
    ServerLock {
        operation: &'static str,
        path: PathBuf,
        remediation: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Skills(String),

    /// A ledger command failure whose text dctl composes itself (missing or
    /// unusable signing key, API error surfaces). Rendered verbatim.
    #[error("{0}")]
    Ledger(String),

    /// A private-registry (ADR-0008) failure: transport, auth, digest
    /// mismatch, or load errors. Self-composed, rendered verbatim.
    #[error("{0}")]
    Registry(String),

    #[error("Invalid server name '{0}': must not contain path separators or '..'")]
    InvalidServerName(String),

    #[error("{0}")]
    ConfigNotFound(String),

    #[error(
        "Invalid config name '{0}': must be a file in the configs dir, not a path (no '/', '\\', or '..')"
    )]
    InvalidConfigName(String),

    #[error("Docker is not available: {0}")]
    DockerNotAvailable(String),

    #[error("Docker error: {0}")]
    #[allow(clippy::enum_variant_names)]
    DockerError(String),

    /// A container name held by a container dctl does not manage.
    /// Kept separate from [`Error::DockerError`], whose payload is daemon text,
    /// so structured output can render this self-composed guidance verbatim.
    #[error(
        "Docker error: container '{0}' already exists but is not managed by dctl. \
         Remove it manually or pick a different name."
    )]
    ContainerNameConflict(String),

    #[error("{primary}\nPostgres startup rollback incomplete: {cleanup}")]
    PostgresStartupRollback {
        #[source]
        primary: Box<Error>,
        cleanup: String,
    },

    #[error("{primary}\nFalkorDB startup rollback incomplete: {cleanup}")]
    FalkorStartupRollback {
        #[source]
        primary: Box<Error>,
        cleanup: String,
    },

    #[error("{primary}\nClickHouse startup rollback incomplete: {cleanup}")]
    ClickhouseStartupRollback {
        #[source]
        primary: Box<Error>,
        cleanup: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Process exit code: `0` success, `1` error. Clap reserves `2` for
    /// usage errors; `ChildExit` passes a child's status through unchanged
    /// (so a 3 is a child exit code, not a dctl cancellation).
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::ChildExit(code) => *code,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_errors_map_to_1() {
        assert_eq!(Error::ClickhouseUsage("bad flag".into()).exit_code(), 1);
    }

    #[test]
    fn child_exit_codes_pass_through_without_changing_normal_mappings() {
        assert_eq!(Error::ChildExit(42).exit_code(), 42);
        assert_eq!(Error::ChildExit(255).exit_code(), 255);
    }
}
