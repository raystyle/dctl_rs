//! Docker integration for `local postgres` and `local falkordb`.
//!
//! All Docker work goes through the async Docker API — there is no shell-out
//! to the `docker` CLI anywhere in this crate, including for interactive
//! `psql`/`redis-cli` exec (which uses an attached exec stream + crossterm
//! raw mode and forwards SIGWINCH as `resize_exec`).
//!
//! Containers we create are tagged with these labels so we can later discover
//! them even if the local metadata file is missing:
//!
//!  * `dctl.engine=postgres|falkordb|clickhouse`
//!  * `dctl.name=<server-name>`
//!  * `dctl.major=<major-or-full-version>`
//!  * `dctl.project=<canonical project cwd>`
//!  * `created_by=dctl_<crate-version>`

use crate::error::{Error, Result};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::io::{self, IsTerminal, Write};

pub const LABEL_ENGINE: &str = "dctl.engine";
/// Engine label values (G8: shared constants, so filter strings and label
/// writes cannot drift apart).
pub const ENGINE_POSTGRES: &str = "postgres";
pub const ENGINE_FALKORDB: &str = "falkordb";
pub const ENGINE_CLICKHOUSE: &str = "clickhouse";
pub const LABEL_NAME: &str = "dctl.name";
pub const LABEL_MAJOR: &str = "dctl.major";
pub const LABEL_PROJECT: &str = "dctl.project";
pub const LABEL_CREATED_BY: &str = "created_by";

/// Value of the `created_by` label — `dctl_<crate version>`.
pub fn created_by_value() -> String {
    format!("dctl_{}", env!("CARGO_PKG_VERSION"))
}

/// Container name for a Postgres instance: `dctl-pg-<name>-<major>`.
/// Distinct (name, major) pairs always get distinct container names.
pub fn pg_container_name(user_name: &str, major: &str) -> String {
    format!("dctl-pg-{}-{}", user_name, major)
}

/// Container name for a FalkorDB instance: `dctl-fk-<name>-<version>`.
/// Distinct (name, version) pairs always get distinct container names.
pub fn fk_container_name(user_name: &str, version: &str) -> String {
    format!("dctl-fk-{}-{}", user_name, version)
}

/// Container name for a ClickHouse instance: `dctl-ch-<name>-<version>`.
pub fn ch_container_name(user_name: &str, version: &str) -> String {
    format!("dctl-ch-{}-{}", user_name, version)
}

/// Connect to the local Docker daemon and verify it's reachable.
pub async fn connect() -> Result<Docker> {
    let docker = Docker::connect_with_defaults()
        .map_err(|error| docker_unavailable(DockerConnectStage::Constructor, &error))?;
    docker
        .ping()
        .await
        .map_err(|error| docker_unavailable(DockerConnectStage::Ping, &error))?;
    Ok(docker)
}

#[derive(Clone, Copy)]
enum DockerConnectStage {
    Constructor,
    Ping,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DockerFailureKind {
    MissingSocket,
    PermissionDenied,
    ConnectionRefused,
    TimedOut,
    InvalidHost,
    HttpStatus(u16),
    Other,
}

#[derive(Clone, Copy)]
enum HostPlatform {
    MacOs,
    Linux,
    Windows,
    Other,
}

impl HostPlatform {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

fn docker_unavailable(stage: DockerConnectStage, error: &BollardError) -> Error {
    let kind = classify_docker_failure(error);
    let platform = HostPlatform::current();
    let prefix = match stage {
        DockerConnectStage::Constructor => "could not initialize Docker client",
        DockerConnectStage::Ping => "Docker daemon is not reachable",
    };
    Error::DockerNotAvailable(format!(
        "{prefix}: {}.\n{}",
        docker_failure_cause(stage, kind, platform),
        docker_guidance(platform)
    ))
}

fn classify_docker_failure(error: &BollardError) -> DockerFailureKind {
    match error {
        BollardError::SocketNotFoundError(_) => return DockerFailureKind::MissingSocket,
        BollardError::UnsupportedURISchemeError { .. }
        | BollardError::URLParseError { .. }
        | BollardError::InvalidURIError { .. }
        | BollardError::InvalidURIPartsError { .. } => return DockerFailureKind::InvalidHost,
        BollardError::RequestTimeoutError => return DockerFailureKind::TimedOut,
        BollardError::IOError { err } => match err.kind() {
            io::ErrorKind::NotFound => return DockerFailureKind::MissingSocket,
            io::ErrorKind::PermissionDenied => return DockerFailureKind::PermissionDenied,
            io::ErrorKind::ConnectionRefused => return DockerFailureKind::ConnectionRefused,
            io::ErrorKind::TimedOut => return DockerFailureKind::TimedOut,
            _ => {}
        },
        BollardError::DockerResponseServerError { status_code, .. } => {
            return DockerFailureKind::HttpStatus(*status_code);
        }
        _ => {}
    }

    let mut descriptions = Vec::new();
    let mut source: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = source {
        descriptions.push(current.to_string().to_ascii_lowercase());
        if let Some(io_error) = current.downcast_ref::<io::Error>() {
            match io_error.kind() {
                io::ErrorKind::NotFound => return DockerFailureKind::MissingSocket,
                io::ErrorKind::PermissionDenied => return DockerFailureKind::PermissionDenied,
                io::ErrorKind::ConnectionRefused => return DockerFailureKind::ConnectionRefused,
                io::ErrorKind::TimedOut => return DockerFailureKind::TimedOut,
                _ => {}
            }
        }
        source = current.source();
    }

    // Some connector errors do not expose their underlying io::Error through
    // Error::source. Inspect their text only for classification; never render it.
    let chain = descriptions.join(": ");
    if chain.contains("permission denied") {
        DockerFailureKind::PermissionDenied
    } else if chain.contains("connection refused") {
        DockerFailureKind::ConnectionRefused
    } else if chain.contains("no such file") || chain.contains("not found") {
        DockerFailureKind::MissingSocket
    } else if chain.contains("timed out") || chain.contains("timeout") {
        DockerFailureKind::TimedOut
    } else {
        DockerFailureKind::Other
    }
}

fn docker_failure_cause(
    stage: DockerConnectStage,
    kind: DockerFailureKind,
    platform: HostPlatform,
) -> String {
    let endpoint = match platform {
        HostPlatform::Windows => "Docker named pipe",
        _ => "Docker socket",
    };
    match kind {
        DockerFailureKind::MissingSocket => format!("{endpoint} was not found"),
        DockerFailureKind::PermissionDenied => {
            format!("permission denied while opening the {endpoint}")
        }
        DockerFailureKind::ConnectionRefused => "the Docker daemon refused the connection".into(),
        DockerFailureKind::TimedOut => "the Docker daemon connection timed out".into(),
        DockerFailureKind::InvalidHost => {
            "DOCKER_HOST is invalid or uses an unsupported scheme".into()
        }
        DockerFailureKind::HttpStatus(status) => {
            format!("the Docker API returned HTTP status {status}")
        }
        DockerFailureKind::Other => match stage {
            DockerConnectStage::Constructor => "Docker client initialization failed".into(),
            DockerConnectStage::Ping => "the Docker API ping failed".into(),
        },
    }
}

fn docker_guidance(platform: HostPlatform) -> &'static str {
    match platform {
        HostPlatform::MacOs => {
            "On macOS, start Docker Desktop and check access to its socket. Verify the active context with `docker context show`; for a non-default context, set `DOCKER_HOST` to its endpoint."
        }
        HostPlatform::Linux => {
            "On Linux, start Docker Engine or Docker Desktop and check that your user can access the Docker socket. Verify `docker context show`; for rootless Docker or a non-default context, set `DOCKER_HOST` to its Unix socket (rootless Engine commonly uses `unix://$XDG_RUNTIME_DIR/docker.sock`)."
        }
        HostPlatform::Windows => {
            "On Windows, start Docker Desktop or Docker Engine and check named-pipe permissions. Verify `docker context show`; for a non-default context, set `DOCKER_HOST` to its endpoint."
        }
        HostPlatform::Other => {
            "Start Docker Engine and check socket permissions. Verify `docker context show`; for rootless Docker or a non-default context, set `DOCKER_HOST` to its endpoint."
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PullProgressMode {
    Interactive,
    Collapsed,
    Silent,
}

fn pull_progress_mode(
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
    structured_output: bool,
) -> PullProgressMode {
    if structured_output {
        PullProgressMode::Silent
    } else if stdout_is_terminal && stderr_is_terminal {
        PullProgressMode::Interactive
    } else {
        PullProgressMode::Collapsed
    }
}

struct PullReporter {
    image: String,
    mode: PullProgressMode,
}

impl PullReporter {
    fn new(image: String, mode: PullProgressMode) -> Self {
        Self { image, mode }
    }

    fn start(&self, output: &mut impl Write) {
        match self.mode {
            PullProgressMode::Interactive => {
                let _ = writeln!(output, "Pulling {}...", self.image);
            }
            PullProgressMode::Collapsed => {
                let _ = write!(output, "Pulling {}...", self.image);
                let _ = output.flush();
            }
            PullProgressMode::Silent => {}
        }
    }

    fn event(&self, info: &bollard::models::CreateImageInfo, output: &mut impl Write) {
        if self.mode != PullProgressMode::Interactive {
            return;
        }

        let Some(status) = info.status.as_deref() else {
            return;
        };
        let _ = write!(output, "  ");
        if let Some(id) = info.id.as_deref() {
            let _ = write!(output, "{id}: ");
        }
        let _ = write!(output, "{status}");
        if let Some((current, total)) = info.progress_detail.as_ref().and_then(|progress| {
            progress
                .current
                .zip(progress.total)
                .filter(|(_, total)| *total > 0)
        }) {
            let percent = current.saturating_mul(100) / total;
            let _ = write!(output, " ({current}/{total} bytes, {percent}%)");
        }
        let _ = writeln!(output);
    }

    fn finish(&self, output: &mut impl Write) {
        match self.mode {
            PullProgressMode::Interactive => {
                let _ = writeln!(output, "Pulled {}", self.image);
            }
            PullProgressMode::Collapsed => {
                let _ = writeln!(output, " done");
            }
            PullProgressMode::Silent => {}
        }
    }

    fn fail(&self, output: &mut impl Write) {
        match self.mode {
            PullProgressMode::Interactive => {
                let _ = writeln!(output, "Failed to pull {}", self.image);
            }
            PullProgressMode::Collapsed => {
                let _ = writeln!(output, " failed");
            }
            PullProgressMode::Silent => {}
        }
    }
}

/// Pull an image through the ADR-0010 chain: the private registry first
/// (native v2, OCI layout + docker load), then the daemon's Docker Hub
/// pull, then the local cache tar. The private-registry error is what
/// surfaces; every fallback step announces itself and its failure reason
/// on stderr so the chain stays observable end to end.
pub async fn pull_image(
    docker: &Docker,
    image_ref: &str,
    structured_output: bool,
    registry_override: Option<&str>,
) -> Result<()> {
    // An explicit --registry bypasses the chain: that source, then cache.
    if let Some(endpoint) = registry_override {
        eprintln!("pulling {image_ref} from {endpoint}");
        return match crate::local::registry::pull_via_registry_from(docker, image_ref, endpoint)
            .await
        {
            Ok(()) => Ok(()),
            Err(reason) => {
                eprintln!("registry pull from {endpoint} failed: {reason}");
                match crate::local::registry::load_from_cache(docker, image_ref).await {
                    Ok(true) => {
                        eprintln!("loaded {image_ref} from the local registry cache");
                        Ok(())
                    }
                    Ok(false) => Err(reason),
                    Err(cache_reason) => {
                        eprintln!("local cache load failed: {cache_reason}");
                        Err(reason)
                    }
                }
            }
        };
    }
    match crate::local::registry::pull_via_registry(docker, image_ref).await {
        Ok(()) => Ok(()),
        Err(primary) => {
            eprintln!(
                "private registry pull failed ({primary}); trying Docker Hub then the local cache"
            );
            match hub_pull(docker, image_ref, structured_output).await {
                Ok(()) => return Ok(()),
                Err(reason) => eprintln!("Docker Hub pull failed: {reason}"),
            }
            match crate::local::registry::load_from_cache(docker, image_ref).await {
                Ok(true) => {
                    eprintln!("loaded {image_ref} from the local registry cache");
                    return Ok(());
                }
                Ok(false) => {}
                Err(reason) => eprintln!("local cache load failed: {reason}"),
            }
            Err(primary)
        }
    }
}

/// Pull an image by full reference (`postgres:18`, `falkordb/falkorddb:v4.20.6`),
/// keeping full progress for interactive terminals and collapsing it to one
/// bounded summary line for redirected or structured output.
async fn hub_pull(docker: &Docker, image_ref: &str, structured_output: bool) -> Result<()> {
    use bollard::query_parameters::CreateImageOptionsBuilder;
    let from = image_ref.to_string();
    let mode = pull_progress_mode(
        io::stdout().is_terminal(),
        io::stderr().is_terminal(),
        structured_output,
    );
    let reporter = PullReporter::new(from.clone(), mode);
    let stderr = io::stderr();
    reporter.start(&mut stderr.lock());

    let opts = CreateImageOptionsBuilder::default()
        .from_image(&from)
        .build();
    let mut stream = docker.create_image(Some(opts), None, None);
    while let Some(item) = stream.next().await {
        let info = match item {
            Ok(info) => info,
            Err(error) => {
                reporter.fail(&mut stderr.lock());
                return Err(Error::Download(error.to_string()));
            }
        };
        reporter.event(&info, &mut stderr.lock());
    }
    reporter.finish(&mut stderr.lock());
    Ok(())
}

/// Check whether an image reference is already present locally (no pull).
pub async fn image_exists(docker: &Docker, image_ref: &str) -> Result<bool> {
    use bollard::errors::Error as BErr;
    match docker.inspect_image(image_ref).await {
        Ok(_) => Ok(true),
        Err(BErr::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(false),
        Err(e) => Err(Error::DockerError(e.to_string())),
    }
}

pub struct PostgresRunOpts<'a> {
    /// User-facing instance name (e.g. `dev`).
    pub user_name: &'a str,
    /// Major version digits (e.g. `16`).
    pub major: &'a str,
    pub tag: &'a str,
    pub host_port: u16,
    pub data_dir: &'a std::path::Path,
    pub project_cwd: &'a str,
    pub user: &'a str,
    pub password: &'a str,
    pub database: &'a str,
    pub extra_env: Vec<String>,
}

/// Create a Postgres container without starting it; return its ID.
///
/// Keeping creation separate gives the caller the exact container ID needed
/// to roll back every later startup step.
pub async fn create_postgres(docker: &Docker, opts: PostgresRunOpts<'_>) -> Result<String> {
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
    use bollard::query_parameters::CreateContainerOptionsBuilder;

    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    port_bindings.insert(
        "5432/tcp".to_string(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(opts.host_port.to_string()),
        }]),
    );

    let canonical_data = opts
        .data_dir
        .canonicalize()
        .map_err(|e| Error::DockerError(format!("data dir canonicalize: {e}")))?;
    let bind = format!("{}:/var/lib/postgresql/data", canonical_data.display());

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        binds: Some(vec![bind]),
        ..Default::default()
    };

    // Pin PGDATA to the legacy path. Postgres 18+ default-stores data at
    // /var/lib/postgresql/<major>/docker; older majors use /var/lib/postgresql/data.
    // We bind-mount a single host directory per server, so we force one
    // consistent path regardless of major version. Each managed server is
    // pinned to a single image tag (changing tag requires `remove`), so we
    // never need pg_upgrade-style cross-version layout.
    let mut env: Vec<String> = vec![
        format!("POSTGRES_USER={}", opts.user),
        format!("POSTGRES_PASSWORD={}", opts.password),
        format!("POSTGRES_DB={}", opts.database),
        "PGDATA=/var/lib/postgresql/data".to_string(),
    ];
    env.extend(opts.extra_env);

    let mut labels: HashMap<String, String> = HashMap::new();
    labels.insert(LABEL_ENGINE.into(), ENGINE_POSTGRES.into());
    labels.insert(LABEL_NAME.into(), opts.user_name.into());
    labels.insert(LABEL_MAJOR.into(), opts.major.into());
    labels.insert(LABEL_PROJECT.into(), opts.project_cwd.into());
    labels.insert(LABEL_CREATED_BY.into(), created_by_value());

    let container_config = ContainerCreateBody {
        image: Some(format!("postgres:{}", opts.tag)),
        env: Some(env),
        host_config: Some(host_config),
        labels: Some(labels),
        ..Default::default()
    };

    let create_opts = CreateContainerOptionsBuilder::default()
        .name(&pg_container_name(opts.user_name, opts.major))
        .build();

    let created = docker
        .create_container(Some(create_opts), container_config)
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;
    Ok(created.id)
}

/// If a container with one of our managed names (`dctl-pg-<name>-<major>` or
/// `dctl-fk-<name>-<version>`) exists in any state, remove it — but only when
/// it carries our labels for the current project. Returns Ok(()) if the name
/// is free or was cleaned up, or an actionable error if the name is held by an
/// unrelated container.
pub async fn ensure_name_free(
    docker: &Docker,
    container_name: &str,
    engine: &str,
    project_cwd: &str,
) -> Result<()> {
    use bollard::errors::Error as BErr;
    let cname = container_name.to_string();
    match docker.inspect_container(&cname, None).await {
        Ok(info) => {
            let labels_match = info
                .config
                .as_ref()
                .and_then(|c| c.labels.as_ref())
                .map(|l| {
                    l.get(LABEL_ENGINE).map(String::as_str) == Some(engine)
                        && l.get(LABEL_PROJECT).map(String::as_str) == Some(project_cwd)
                })
                .unwrap_or(false);
            if !labels_match {
                return Err(Error::ContainerNameConflict(cname));
            }
            let id = info.id.unwrap_or(cname.clone());
            let _ = stop_container(docker, &id).await;
            remove_container(docker, &id).await
        }
        Err(BErr::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(()),
        Err(e) => Err(Error::DockerError(e.to_string())),
    }
}

pub struct FalkorRunOpts<'a> {
    /// User-facing instance name (e.g. `dev`).
    pub user_name: &'a str,
    /// Full version digits (e.g. `4.20.6`).
    pub version: &'a str,
    /// Image reference (e.g. `falkordb/falkordb:v4.20.6`).
    pub image_ref: &'a str,
    pub host_port: u16,
    pub browser_port: u16,
    pub data_dir: &'a std::path::Path,
    pub project_cwd: &'a str,
    pub password: &'a str,
    pub extra_env: Vec<String>,
}

/// Create a FalkorDB container without starting it; return its ID. Same
/// create-then-start split as `create_postgres`, for identical rollback
/// guarantees.
pub async fn create_falkordb(docker: &Docker, opts: FalkorRunOpts<'_>) -> Result<String> {
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
    use bollard::query_parameters::CreateContainerOptionsBuilder;

    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    for (container_port, host_port) in [
        ("6379/tcp", opts.host_port),
        ("3000/tcp", opts.browser_port),
    ] {
        port_bindings.insert(
            container_port.to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(host_port.to_string()),
            }]),
        );
    }

    let canonical_data = opts
        .data_dir
        .canonicalize()
        .map_err(|e| Error::DockerError(format!("data dir canonicalize: {e}")))?;
    let bind = format!("{}:/var/lib/falkordb/data", canonical_data.display());

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        binds: Some(vec![bind]),
        ..Default::default()
    };

    // Authentication is Redis-layer: REDIS_ARGS reaches the server's argv.
    // FALKORDB_ARGS (module tuning) may arrive through extra_env.
    let mut env: Vec<String> = vec![format!("REDIS_ARGS=--requirepass {}", opts.password)];
    env.extend(opts.extra_env);

    let mut labels: HashMap<String, String> = HashMap::new();
    labels.insert(LABEL_ENGINE.into(), ENGINE_FALKORDB.into());
    labels.insert(LABEL_NAME.into(), opts.user_name.into());
    labels.insert(LABEL_MAJOR.into(), opts.version.into());
    labels.insert(LABEL_PROJECT.into(), opts.project_cwd.into());
    labels.insert(LABEL_CREATED_BY.into(), created_by_value());

    let container_config = ContainerCreateBody {
        image: Some(opts.image_ref.to_string()),
        env: Some(env),
        host_config: Some(host_config),
        labels: Some(labels),
        ..Default::default()
    };

    let create_opts = CreateContainerOptionsBuilder::default()
        .name(&fk_container_name(opts.user_name, opts.version))
        .build();

    let created = docker
        .create_container(Some(create_opts), container_config)
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;
    Ok(created.id)
}

pub struct ClickhouseRunOpts<'a> {
    pub user_name: &'a str,
    /// Image tag (e.g. `26.8` or `26.8.9.10`).
    pub version: &'a str,
    pub image_ref: &'a str,
    pub http_port: u16,
    pub native_port: u16,
    pub data_dir: &'a std::path::Path,
    pub project_cwd: &'a str,
    pub user: &'a str,
    pub password: &'a str,
    pub database: &'a str,
    /// Bind-mount for a partial config overlay (source on the host), if any.
    pub config_source: Option<&'a std::path::Path>,
    pub extra_env: Vec<String>,
}

/// Create a ClickHouse container without starting it; return its ID.
///
/// Dual ports (8123 HTTP + 9000 native TCP), data at /var/lib/clickhouse,
/// optional config overlay ro-mounted into config.d/, ulimit nofile 262144
/// per the official recommendation.
pub async fn create_clickhouse(docker: &Docker, opts: ClickhouseRunOpts<'_>) -> Result<String> {
    use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, ResourcesUlimits};
    use bollard::query_parameters::CreateContainerOptionsBuilder;

    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    for (container_port, host_port) in
        [("8123/tcp", opts.http_port), ("9000/tcp", opts.native_port)]
    {
        port_bindings.insert(
            container_port.to_string(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(host_port.to_string()),
            }]),
        );
    }

    let canonical_data = opts
        .data_dir
        .canonicalize()
        .map_err(|e| Error::DockerError(format!("data dir canonicalize: {e}")))?;
    let mut binds = vec![format!("{}:/var/lib/clickhouse", canonical_data.display())];
    if let Some(config_source) = opts.config_source {
        let canonical_config = config_source
            .canonicalize()
            .map_err(|e| Error::DockerError(format!("config canonicalize: {e}")))?;
        let ext = config_source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("xml");
        binds.push(format!(
            "{}:/etc/clickhouse-server/config.d/dctl-config.{}:ro",
            canonical_config.display(),
            ext
        ));
    }

    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        binds: Some(binds),
        ulimits: Some(vec![ResourcesUlimits {
            name: Some("nofile".to_string()),
            soft: Some(262144),
            hard: Some(262144),
        }]),
        ..Default::default()
    };

    let mut env: Vec<String> = vec![
        format!("CLICKHOUSE_USER={}", opts.user),
        format!("CLICKHOUSE_PASSWORD={}", opts.password),
        format!("CLICKHOUSE_DB={}", opts.database),
    ];
    env.extend(opts.extra_env);

    let mut labels: HashMap<String, String> = HashMap::new();
    labels.insert(LABEL_ENGINE.into(), ENGINE_CLICKHOUSE.into());
    labels.insert(LABEL_NAME.into(), opts.user_name.into());
    labels.insert(LABEL_MAJOR.into(), opts.version.into());
    labels.insert(LABEL_PROJECT.into(), opts.project_cwd.into());
    labels.insert(LABEL_CREATED_BY.into(), created_by_value());

    let container_config = ContainerCreateBody {
        image: Some(opts.image_ref.to_string()),
        env: Some(env),
        host_config: Some(host_config),
        labels: Some(labels),
        ..Default::default()
    };

    let create_opts = CreateContainerOptionsBuilder::default()
        .name(&ch_container_name(opts.user_name, opts.version))
        .build();

    let created = docker
        .create_container(Some(create_opts), container_config)
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;
    Ok(created.id)
}
/// `redis-cli ping` succeeds (exit 0, PONG) only once the server answers with
/// the password we provisioned. Connection failures exit non-zero.
pub async fn falkor_is_ready(docker: &Docker, id: &str, password: &str) -> Result<bool> {
    use bollard::exec::{StartExecOptions, StartExecResults};
    use bollard::models::ExecConfig;

    let exec = docker
        .create_exec(
            id,
            ExecConfig {
                attach_stdout: Some(false),
                attach_stderr: Some(false),
                attach_stdin: Some(false),
                tty: Some(false),
                cmd: Some(vec![
                    "redis-cli".to_string(),
                    "--no-auth-warning".to_string(),
                    "ping".to_string(),
                ]),
                env: Some(env_lines(redis_auth_env(password))),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| Error::DockerError(error.to_string()))?;
    let started = docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach: true,
                ..Default::default()
            }),
        )
        .await
        .map_err(|error| Error::DockerError(error.to_string()))?;
    if !matches!(started, StartExecResults::Detached) {
        return Err(Error::DockerError(
            "FalkorDB readiness probe unexpectedly attached".to_string(),
        ));
    }

    for _ in 0..75 {
        let inspect = docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|error| Error::DockerError(error.to_string()))?;
        if inspect.running != Some(true) {
            return Ok(inspect.exit_code == Some(0));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(Error::DockerError(
        "FalkorDB readiness probe did not exit within 1.5 seconds".to_string(),
    ))
}

/// A missing container is distinct from an inspection failure. Only Docker's
/// typed 404 response establishes absence; all other failures remain errors.
pub async fn inspect_container(
    docker: &Docker,
    id: &str,
) -> Result<Option<bollard::models::ContainerInspectResponse>> {
    match docker.inspect_container(id, None).await {
        Ok(response) => Ok(Some(response)),
        Err(BollardError::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(error) => Err(Error::DockerError(error.to_string())),
    }
}

pub fn inspected_container_running(
    response: &bollard::models::ContainerInspectResponse,
) -> Result<bool> {
    response
        .state
        .as_ref()
        .and_then(|state| state.running)
        .ok_or_else(|| Error::DockerError("container inspection omitted its running state".into()))
}

pub async fn is_container_running(docker: &Docker, id: &str) -> Result<bool> {
    match inspect_container(docker, id).await? {
        Some(response) => inspected_container_running(&response),
        None => Ok(false),
    }
}

pub struct ContainerState {
    pub running: bool,
    pub exited: bool,
    pub status: String,
    pub exit_code: Option<i64>,
    pub oom_killed: bool,
}

pub async fn container_state(docker: &Docker, id: &str) -> Result<ContainerState> {
    use bollard::models::ContainerStateStatusEnum;

    let inspect = docker
        .inspect_container(id, None)
        .await
        .map_err(|error| Error::DockerError(error.to_string()))?;
    let state = inspect.state.unwrap_or_default();
    let status = state
        .status
        .map(|status| status.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let exited = state.dead == Some(true)
        || matches!(
            state.status,
            Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD)
        );
    Ok(ContainerState {
        running: state.running == Some(true) && state.paused != Some(true),
        exited,
        status,
        exit_code: state.exit_code,
        oom_killed: state.oom_killed == Some(true),
    })
}

/// Run PostgreSQL's own readiness probe inside the container. The command uses
/// container-local TCP and does not receive a username, database, or password.
pub async fn postgres_is_ready(docker: &Docker, id: &str) -> Result<bool> {
    use bollard::exec::{StartExecOptions, StartExecResults};
    use bollard::models::ExecConfig;

    let exec = docker
        .create_exec(
            id,
            ExecConfig {
                attach_stdout: Some(false),
                attach_stderr: Some(false),
                attach_stdin: Some(false),
                tty: Some(false),
                cmd: Some(vec![
                    "pg_isready".to_string(),
                    "--quiet".to_string(),
                    "--host".to_string(),
                    "127.0.0.1".to_string(),
                    // `--port` only changes the host binding; the official
                    // image always listens on 5432 inside the container.
                    "--port".to_string(),
                    "5432".to_string(),
                    "--timeout".to_string(),
                    "1".to_string(),
                ]),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| Error::DockerError(error.to_string()))?;
    let started = docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach: true,
                ..Default::default()
            }),
        )
        .await
        .map_err(|error| Error::DockerError(error.to_string()))?;
    if !matches!(started, StartExecResults::Detached) {
        return Err(Error::DockerError(
            "PostgreSQL readiness probe unexpectedly attached".to_string(),
        ));
    }

    for _ in 0..75 {
        let inspect = docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|error| Error::DockerError(error.to_string()))?;
        if inspect.running != Some(true) {
            return Ok(inspect.exit_code == Some(0));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(Error::DockerError(
        "PostgreSQL readiness probe did not exit within 1.5 seconds".to_string(),
    ))
}

pub async fn stop_container(docker: &Docker, id: &str) -> Result<()> {
    use bollard::query_parameters::StopContainerOptionsBuilder;
    docker
        .stop_container(
            id,
            Some(StopContainerOptionsBuilder::default().t(10).build()),
        )
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;
    Ok(())
}

pub async fn remove_container(docker: &Docker, id: &str) -> Result<()> {
    use bollard::query_parameters::RemoveContainerOptionsBuilder;
    remove_container_result(
        docker
            .remove_container(
                id,
                Some(
                    RemoveContainerOptionsBuilder::default()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await,
    )
}

fn remove_container_result(result: std::result::Result<(), BollardError>) -> Result<()> {
    match result {
        Ok(())
        | Err(BollardError::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(()),
        Err(e) => Err(Error::DockerError(e.to_string())),
    }
}

pub async fn container_logs_tail(
    docker: &Docker,
    id: &str,
    n: usize,
    max_bytes: usize,
) -> Result<String> {
    use bollard::query_parameters::LogsOptionsBuilder;
    let opts = LogsOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .tail(&n.to_string())
        .build();
    let mut stream = docker.logs(id, Some(opts));
    let mut buf = Vec::new();
    let mut truncated = false;
    while let Some(line) = stream.next().await {
        let l = line.map_err(|e| Error::DockerError(e.to_string()))?;
        truncated |= append_bounded_tail(&mut buf, &l.into_bytes(), max_bytes);
    }
    let logs = String::from_utf8_lossy(&buf);
    if truncated {
        Ok(format!("[earlier log output truncated]\n{logs}"))
    } else if logs.is_empty() {
        Ok("(no container logs)".to_string())
    } else {
        Ok(logs.into_owned())
    }
}

fn append_bounded_tail(buffer: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> bool {
    if max_bytes == 0 {
        let truncated = !buffer.is_empty() || !chunk.is_empty();
        buffer.clear();
        return truncated;
    }
    if chunk.len() >= max_bytes {
        buffer.clear();
        buffer.extend_from_slice(&chunk[chunk.len() - max_bytes..]);
        return true;
    }

    let overflow = buffer
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(max_bytes);
    if overflow > 0 {
        buffer.drain(..overflow);
    }
    buffer.extend_from_slice(chunk);
    overflow > 0
}

pub struct DiscoveredContainer {
    pub container_id: String,
    /// User-facing instance name from the `dctl.name` label.
    pub user_name: String,
    /// Major-version digits from the `dctl.major` label.
    pub major: String,
    pub image: String,
    pub host_port: Option<u16>,
    /// Host port mapped to the engine's secondary container port when it has
    /// one (FalkorDB's browser on 3000; ClickHouse's native TCP on 9000);
    /// None for Postgres and for stopped containers (the list API omits
    /// published ports when not running).
    pub secondary_port: Option<u16>,
}

/// Find Postgres containers we created in `project_cwd`. Filtered on the
/// engine + project labels — both unique to containers this CLI created — but
/// **not** on the version-stamped `created_by` label, so containers created by
/// older releases of the CLI remain manageable after upgrade.
pub async fn list_project_postgres(
    docker: &Docker,
    project_cwd: &str,
) -> Result<Vec<DiscoveredContainer>> {
    list_project_engine(docker, project_cwd, ENGINE_POSTGRES, 5432).await
}

/// Find FalkorDB containers we created in `project_cwd`; same label contract
/// as `list_project_postgres`, with the protocol port 6379.
pub async fn list_project_falkor(
    docker: &Docker,
    project_cwd: &str,
) -> Result<Vec<DiscoveredContainer>> {
    list_project_engine(docker, project_cwd, ENGINE_FALKORDB, 6379).await
}

/// Find ClickHouse containers we created in `project_cwd`; protocol port 8123.
pub async fn list_project_clickhouse(
    docker: &Docker,
    project_cwd: &str,
) -> Result<Vec<DiscoveredContainer>> {
    list_project_engine(docker, project_cwd, ENGINE_CLICKHOUSE, 8123).await
}

async fn list_project_engine(
    docker: &Docker,
    project_cwd: &str,
    engine: &str,
    protocol_port: u16,
) -> Result<Vec<DiscoveredContainer>> {
    use bollard::query_parameters::ListContainersOptionsBuilder;

    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    filters.insert(
        "label".to_string(),
        vec![
            format!("{}={}", LABEL_ENGINE, engine),
            format!("{}={}", LABEL_PROJECT, project_cwd),
        ],
    );

    let opts = ListContainersOptionsBuilder::default()
        .all(true)
        .filters(&filters)
        .build();

    let containers = docker
        .list_containers(Some(opts))
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;

    let mut out = Vec::new();
    for c in containers {
        let id = match c.id {
            Some(s) => s,
            None => continue,
        };
        let labels = c.labels.unwrap_or_default();
        let user_name = match labels.get(LABEL_NAME) {
            Some(s) => s.clone(),
            None => continue,
        };
        // Skip containers from older versions of this CLI that didn't write a
        // major label — we can't reconstruct their disk key safely.
        let major = match labels.get(LABEL_MAJOR) {
            Some(s) => s.clone(),
            None => continue,
        };
        let image = c.image.unwrap_or_default();
        let host_port = c.ports.as_ref().and_then(|ports| {
            ports
                .iter()
                .find(|p| p.private_port == protocol_port)
                .and_then(|p| p.public_port)
        });
        let secondary_private = match engine {
            ENGINE_FALKORDB => 3000,
            ENGINE_CLICKHOUSE => 9000,
            _ => 0,
        };
        let secondary_port = if secondary_private == 0 {
            None
        } else {
            c.ports.as_ref().and_then(|ports| {
                ports
                    .iter()
                    .find(|p| p.private_port == secondary_private)
                    .and_then(|p| p.public_port)
            })
        };
        out.push(DiscoveredContainer {
            container_id: id,
            user_name,
            major,
            image,
            host_port,
            secondary_port,
        });
    }
    Ok(out)
}

/// Run `psql` inside a container with a full interactive TTY:
/// host stdin/stdout are wired to the docker exec stream, the host terminal
/// goes into raw mode, and SIGWINCH is forwarded as `resize_exec`.
pub async fn exec_psql_in_container(
    docker: &Docker,
    container_id: &str,
    psql_args: &[String],
) -> Result<()> {
    let mut cmd = vec!["psql".to_string()];
    cmd.extend(psql_args.iter().cloned());
    exec_command_tty(docker, container_id, cmd, Vec::new()).await
}

/// Run `redis-cli` inside a container with a full interactive TTY; same
/// contract as the psql variant, with REDISCLI_AUTH carried in the exec env.
/// Host port bound to `port_key` in the container's own HostConfig, read from
/// an inspect response. Used by resume to refresh ports the metadata may
/// carry as 0 or stale (recovered instances).
pub(crate) fn host_port_from_inspect(
    inspected: Option<&bollard::models::ContainerInspectResponse>,
    port_key: &str,
) -> Option<u16> {
    inspected?
        .host_config
        .as_ref()?
        .port_bindings
        .as_ref()?
        .get(port_key)?
        .as_ref()?
        .first()?
        .host_port
        .as_deref()?
        .parse()
        .ok()
}

/// Run `clickhouse-client` interactively inside the container (TTY + raw
/// mode, same exec stream as the psql/redis-cli shells). Credentials are
/// passed by the caller as regular `--user/--password` arguments.
pub async fn exec_clickhouse_client_in_container(
    docker: &Docker,
    container_id: &str,
    cli_args: &[String],
) -> Result<()> {
    let mut cmd = vec!["clickhouse-client".to_string()];
    cmd.extend(cli_args.iter().cloned());
    exec_command_tty(docker, container_id, cmd, Vec::new()).await
}

pub async fn exec_redis_cli_in_container(
    docker: &Docker,
    container_id: &str,
    cli_args: &[String],
    password: &str,
) -> Result<()> {
    let mut cmd = vec!["redis-cli".to_string()];
    cmd.extend(cli_args.iter().cloned());
    exec_command_tty(docker, container_id, cmd, redis_auth_env(password)).await
}

fn redis_auth_env(password: &str) -> Vec<(String, String)> {
    vec![("REDISCLI_AUTH".to_string(), password.to_string())]
}

/// Exec env is a Vec of KEY=VALUE strings on the wire.
fn env_lines(pairs: Vec<(String, String)>) -> Vec<String> {
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Run a command inside a container with a full interactive TTY:
/// host stdin/stdout are wired to the docker exec stream, the host terminal
/// goes into raw mode, and SIGWINCH is forwarded as `resize_exec`.
async fn exec_command_tty(
    docker: &Docker,
    container_id: &str,
    cmd: Vec<String>,
    env: Vec<(String, String)>,
) -> Result<()> {
    use bollard::exec::StartExecResults;
    use bollard::models::ExecConfig;
    use bollard::query_parameters::ResizeExecOptionsBuilder;
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let exec = docker
        .create_exec(
            container_id,
            ExecConfig {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                attach_stdin: Some(true),
                tty: Some(true),
                cmd: Some(cmd),
                env: Some(env_lines(env)),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;
    let exec_id = exec.id;

    let started = docker
        .start_exec(&exec_id, None)
        .await
        .map_err(|e| Error::DockerError(e.to_string()))?;
    let (mut output, mut input) = match started {
        StartExecResults::Attached { output, input } => (output, input),
        StartExecResults::Detached => return Ok(()),
    };

    // Initial size resize.
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let _ = docker
            .resize_exec(
                &exec_id,
                ResizeExecOptionsBuilder::default()
                    .h(rows as i32)
                    .w(cols as i32)
                    .build(),
            )
            .await;
    }

    enable_raw_mode().map_err(|e| Error::DockerError(format!("raw mode: {e}")))?;
    struct RawModeGuard;
    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawModeGuard;

    #[cfg(unix)]
    let resize_task = {
        use tokio::signal::unix::{SignalKind, signal};
        let docker_clone = docker.clone();
        let exec_id_clone = exec_id.clone();
        tokio::spawn(async move {
            if let Ok(mut sig) = signal(SignalKind::window_change()) {
                while sig.recv().await.is_some() {
                    if let Ok((cols, rows)) = crossterm::terminal::size() {
                        let _ = docker_clone
                            .resize_exec(
                                &exec_id_clone,
                                ResizeExecOptionsBuilder::default()
                                    .h(rows as i32)
                                    .w(cols as i32)
                                    .build(),
                            )
                            .await;
                    }
                }
            }
        })
    };

    // Pump stdin into the exec stream.
    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if input.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = input.flush().await;
                }
                Err(_) => break,
            }
        }
    });

    // Pump exec output to stdout.
    let mut stdout = tokio::io::stdout();
    while let Some(chunk) = output.next().await {
        match chunk {
            Ok(out) => {
                let bytes = out.into_bytes();
                let _ = stdout.write_all(&bytes).await;
                let _ = stdout.flush().await;
            }
            Err(_) => break,
        }
    }

    stdin_task.abort();
    #[cfg(unix)]
    resize_task.abort();
    drop(_guard);

    if let Ok(info) = docker.inspect_exec(&exec_id).await
        && let Some(code) = info.exit_code
        && code != 0
    {
        return Err(Error::ChildExit(code as i32));
    }
    Ok(())
}

// ── Blocking shims (callable from sync `server.rs`) ─────────────────────────

/// Drive an async task to completion from sync code.
///
/// Inside a multi-thread tokio runtime (the CLI's default `#[tokio::main]`),
/// uses `block_in_place` so we don't deadlock the executor. Outside any
/// runtime (rare — basically only from non-tokio tests), spins up a fresh
/// runtime. Will panic if called inside a `current_thread` runtime; we don't
/// use one anywhere.
pub(crate) fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use tokio::runtime::Handle;
    match Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(f)),
        Err(_) => {
            let rt = tokio::runtime::Runtime::new()
                .expect("failed to build tokio runtime for docker blocking shim");
            rt.block_on(f)
        }
    }
}

pub fn is_container_running_blocking(id: &str) -> Result<bool> {
    let id = id.to_string();
    block_on(async move {
        let docker = connect().await?;
        is_container_running(&docker, &id).await
    })
}

/// Stop a container, leaving it on disk so it can be `docker start`ed again.
pub fn stop_blocking(id: &str) -> Result<()> {
    let id = id.to_string();
    block_on(async move {
        let docker = connect().await?;
        stop_container(&docker, &id).await
    })
}

/// Best-effort stop, then remove the container.
/// Remove a host directory and its contents, even when files inside are
/// owned by container UIDs (postgres' uid 999) the host user can't `rm`.
///
/// On non-macOS hosts, tries `std::fs::remove_dir_all` first and falls back to
/// a one-shot Alpine container for files owned by container UIDs. macOS Docker
/// backends can retain a stale bind-mount view after host-side removal, so they
/// always remove the directory through Docker's filesystem view.
pub fn remove_host_dir_blocking(host_path: &std::path::Path) -> Result<()> {
    if !host_path.exists() {
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    if std::fs::remove_dir_all(host_path).is_ok() {
        return Ok(());
    }
    let parent = host_path
        .parent()
        .ok_or_else(|| Error::DockerError("path has no parent".into()))?
        .canonicalize()?;
    let basename = host_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::DockerError("path has no basename".into()))?
        .to_string();
    let parent_str = parent.display().to_string();

    block_on(async move {
        use bollard::errors::Error as BErr;
        use bollard::models::{ContainerCreateBody, HostConfig};
        use bollard::query_parameters::{
            CreateContainerOptionsBuilder, CreateImageOptionsBuilder, StartContainerOptions,
            WaitContainerOptions,
        };

        let docker = connect().await?;

        // Pull alpine on first use.
        if let Err(BErr::DockerResponseServerError {
            status_code: 404, ..
        }) = docker.inspect_image("alpine:latest").await
        {
            let mut s = docker.create_image(
                Some(
                    CreateImageOptionsBuilder::default()
                        .from_image("alpine:latest")
                        .build(),
                ),
                None,
                None,
            );
            while let Some(item) = s.next().await {
                item.map_err(|e| Error::DockerError(e.to_string()))?;
            }
        }

        let bind = format!("{}:/work", parent_str);
        let cfg = ContainerCreateBody {
            image: Some("alpine:latest".into()),
            cmd: Some(privileged_remove_command(&basename)),
            host_config: Some(HostConfig {
                binds: Some(vec![bind]),
                auto_remove: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let created = docker
            .create_container(Some(CreateContainerOptionsBuilder::default().build()), cfg)
            .await
            .map_err(|e| Error::DockerError(e.to_string()))?;
        docker
            .start_container(&created.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| Error::DockerError(e.to_string()))?;
        let mut wait = docker.wait_container(&created.id, None::<WaitContainerOptions>);
        while let Some(item) = wait.next().await {
            // Ignore individual stream errors — auto_remove will reap on exit.
            let _ = item;
        }
        Ok::<(), Error>(())
    })?;

    // If anything is left (e.g. the dir itself remained empty), clean up host-side.
    if host_path.exists() {
        std::fs::remove_dir_all(host_path)?;
    }
    Ok(())
}

fn privileged_remove_command(basename: &str) -> Vec<String> {
    vec![
        "rm".into(),
        "-rf".into(),
        "--".into(),
        format!("/work/{basename}"),
    ]
}

pub fn stop_and_remove_blocking(id: &str) -> Result<()> {
    let id = id.to_string();
    block_on(async move {
        let docker = connect().await?;
        let _ = stop_container(&docker, &id).await;
        remove_container(&docker, &id).await
    })
}

/// `docker start` an existing stopped container.
pub async fn start_existing(docker: &Docker, id: &str) -> Result<()> {
    use bollard::query_parameters::StartContainerOptions;
    docker
        .start_container(id, None::<StartContainerOptions>)
        .await
        .map_err(|e| Error::DockerError(e.to_string()))
}

/// Discover Postgres containers belonging to this project that don't yet have
/// a metadata file under `.dctl/servers/`, and write a `ServerInfo` for
/// each so they show up in `local server list` and can be managed.
///
/// Safe to call multiple times in one CLI invocation. When Docker isn't
/// reachable, `connect()` fails fast (no socket → immediate error, no I/O
/// timeout) and we return silently.
pub fn recover_project_postgres_blocking(
    project_cwd: &str,
    lock: &crate::local::server::MetadataLock,
) -> Result<()> {
    use crate::local::server::{
        Engine, ServerInfo, ensure_pg_data_dir, load_info_locked, pg_instance_key,
        save_server_info_locked,
    };
    let cwd_owned = project_cwd.to_string();
    block_on(async move {
        let docker = match connect().await {
            Ok(d) => d,
            Err(_) => return Ok::<(), Error>(()),
        };
        let containers = match list_project_postgres(&docker, &cwd_owned).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        for c in containers {
            let key = pg_instance_key(&c.user_name, &c.major);
            if load_info_locked(&key, lock)?.is_some() {
                continue;
            }
            ensure_pg_data_dir(&c.user_name, &c.major)?;
            let info = ServerInfo {
                name: key,
                pid: 0,
                version: c.image.clone(),
                http_port: 0,
                tcp_port: c.host_port.unwrap_or(0),
                started_at: "recovered".to_string(),
                cwd: cwd_owned.clone(),
                engine: Engine::Postgres,
                container_id: Some(c.container_id.clone()),
            };
            save_server_info_locked(&info, lock)?;
        }
        Ok(())
    })
}

/// ClickHouse sibling of `recover_project_postgres_blocking`.
pub fn recover_project_clickhouse_blocking(
    project_cwd: &str,
    lock: &crate::local::server::MetadataLock,
) -> Result<()> {
    use crate::local::server::{
        Engine, ServerInfo, ch_instance_key, ensure_ch_data_dir, load_info_locked,
        save_server_info_locked,
    };
    let cwd_owned = project_cwd.to_string();
    block_on(async move {
        let docker = match connect().await {
            Ok(d) => d,
            Err(_) => return Ok::<(), Error>(()),
        };
        let containers = match list_project_clickhouse(&docker, &cwd_owned).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        for c in containers {
            let key = ch_instance_key(&c.user_name, &c.major);
            if load_info_locked(&key, lock)?.is_some() {
                continue;
            }
            ensure_ch_data_dir(&c.user_name, &c.major)?;
            let info = ServerInfo {
                name: key,
                pid: 0,
                version: format!("clickhouse:{}", c.major),
                http_port: c.host_port.unwrap_or(0),
                tcp_port: c.secondary_port.unwrap_or(0),
                started_at: "recovered".to_string(),
                cwd: cwd_owned.clone(),
                engine: Engine::Clickhouse,
                container_id: Some(c.container_id.clone()),
            };
            save_server_info_locked(&info, lock)?;
        }
        Ok(())
    })
}

/// FalkorDB sibling of `recover_project_postgres_blocking`: same contract,
/// keyed `<name>-fk<version>`; the browser port is not recoverable from the
/// label filter alone and stays 0 until the next start refreshes it.
pub fn recover_project_falkor_blocking(
    project_cwd: &str,
    lock: &crate::local::server::MetadataLock,
) -> Result<()> {
    use crate::local::server::{
        Engine, ServerInfo, ensure_fk_data_dir, fk_instance_key, load_info_locked,
        save_server_info_locked,
    };
    let cwd_owned = project_cwd.to_string();
    block_on(async move {
        let docker = match connect().await {
            Ok(d) => d,
            Err(_) => return Ok::<(), Error>(()),
        };
        let containers = match list_project_falkor(&docker, &cwd_owned).await {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        for c in containers {
            let key = fk_instance_key(&c.user_name, &c.major);
            if load_info_locked(&key, lock)?.is_some() {
                continue;
            }
            ensure_fk_data_dir(&c.user_name, &c.major)?;
            let info = ServerInfo {
                name: key,
                pid: 0,
                // Canonical stored form, matching what `start` writes, so a
                // later resume parses the same tag (the raw image ref would).
                version: crate::local::falkordb::stored_version_form(&c.major),
                // The browser port rides http_port. The list API only reports
                // published ports for running containers; a stopped container
                // recovers 0 here and the next resume refreshes both ports
                // from the container's port bindings.
                http_port: c.secondary_port.unwrap_or(0),
                tcp_port: c.host_port.unwrap_or(0),
                started_at: "recovered".to_string(),
                cwd: cwd_owned.clone(),
                engine: Engine::Falkordb,
                container_id: Some(c.container_id.clone()),
            };
            save_server_info_locked(&info, lock)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{CreateImageInfo, ProgressDetail};

    #[cfg(unix)]
    #[tokio::test]
    async fn remove_container_requests_anonymous_volume_cleanup() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().expect("create Docker mock directory");
        let socket_path = directory.path().join("docker.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind Docker mock socket");
        let request = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept Docker request");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).expect("read Docker request");
                assert!(count > 0, "Docker request ended before its headers");
                bytes.extend_from_slice(&buffer[..count]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write Docker response");
            String::from_utf8(bytes).expect("Docker request is UTF-8")
        });
        let docker = Docker::connect_with_unix(
            socket_path.to_str().expect("socket path is UTF-8"),
            5,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("connect to Docker mock");

        remove_container(&docker, "pg-id")
            .await
            .expect("remove container");

        let request = request.join().expect("join Docker mock");
        let request_line = request.lines().next().expect("Docker request line");
        let path = request_line
            .split_whitespace()
            .nth(1)
            .expect("Docker request path");
        let query = path.split_once('?').expect("Docker request query").1;
        let parameters: HashMap<_, _> = query
            .split('&')
            .filter_map(|parameter| parameter.split_once('='))
            .collect();
        assert_eq!(parameters.get("force"), Some(&"true"));
        assert_eq!(parameters.get("v"), Some(&"true"));
    }

    #[test]
    fn docker_failures_keep_safe_causes_without_endpoint_values() {
        let secret_path = "/tmp/docker-password-secret.sock";
        let missing = BollardError::SocketNotFoundError(secret_path.into());
        let Error::DockerNotAvailable(message) =
            docker_unavailable(DockerConnectStage::Constructor, &missing)
        else {
            panic!("expected DockerNotAvailable");
        };
        assert!(
            message
                .starts_with("could not initialize Docker client: Docker socket was not found.\n")
        );
        assert!(!message.contains(secret_path));

        let invalid = BollardError::UnsupportedURISchemeError {
            uri: "tcp+secret://user:password@example.test".into(),
        };
        let Error::DockerNotAvailable(message) =
            docker_unavailable(DockerConnectStage::Constructor, &invalid)
        else {
            panic!("expected DockerNotAvailable");
        };
        assert!(message.contains("DOCKER_HOST is invalid or uses an unsupported scheme"));
        assert!(!message.contains("user:password"));
        assert!(!message.contains("example.test"));
    }

    #[test]
    fn docker_ping_failures_preserve_actionable_io_causes() {
        for (kind, expected) in [
            (
                io::ErrorKind::PermissionDenied,
                "permission denied while opening the Docker socket",
            ),
            (
                io::ErrorKind::ConnectionRefused,
                "the Docker daemon refused the connection",
            ),
            (
                io::ErrorKind::TimedOut,
                "the Docker daemon connection timed out",
            ),
        ] {
            let error = BollardError::IOError {
                err: io::Error::new(kind, "sensitive endpoint details"),
            };
            let Error::DockerNotAvailable(message) =
                docker_unavailable(DockerConnectStage::Ping, &error)
            else {
                panic!("expected DockerNotAvailable");
            };
            assert!(
                message.starts_with(&format!("Docker daemon is not reachable: {expected}.\n")),
                "{message}"
            );
            assert!(!message.contains("sensitive endpoint details"));
        }
    }

    #[test]
    fn docker_endpoint_failure_causes_are_platform_aware() {
        assert_eq!(
            docker_failure_cause(
                DockerConnectStage::Constructor,
                DockerFailureKind::MissingSocket,
                HostPlatform::Windows,
            ),
            "Docker named pipe was not found"
        );
        assert_eq!(
            docker_failure_cause(
                DockerConnectStage::Ping,
                DockerFailureKind::PermissionDenied,
                HostPlatform::Windows,
            ),
            "permission denied while opening the Docker named pipe"
        );
        assert_eq!(
            docker_failure_cause(
                DockerConnectStage::Constructor,
                DockerFailureKind::MissingSocket,
                HostPlatform::Linux,
            ),
            "Docker socket was not found"
        );
    }

    #[test]
    fn docker_timeout_and_http_status_failures_are_classified() {
        let timeout = BollardError::RequestTimeoutError;
        assert_eq!(
            classify_docker_failure(&timeout),
            DockerFailureKind::TimedOut
        );

        let response = BollardError::DockerResponseServerError {
            status_code: 503,
            message: "sensitive daemon response".into(),
        };
        assert_eq!(
            classify_docker_failure(&response),
            DockerFailureKind::HttpStatus(503)
        );
        let Error::DockerNotAvailable(message) =
            docker_unavailable(DockerConnectStage::Ping, &response)
        else {
            panic!("expected DockerNotAvailable");
        };
        assert!(message.contains("the Docker API returned HTTP status 503"));
        assert!(!message.contains("sensitive daemon response"));
    }

    #[test]
    fn docker_guidance_is_platform_aware() {
        let macos = docker_guidance(HostPlatform::MacOs);
        assert!(macos.contains("Docker Desktop"));
        assert!(macos.contains("socket"));
        assert!(macos.contains("docker context show"));
        assert!(macos.contains("DOCKER_HOST"));

        let linux = docker_guidance(HostPlatform::Linux);
        assert!(linux.contains("Docker Engine or Docker Desktop"));
        assert!(linux.contains("socket"));
        assert!(linux.contains("docker context show"));
        assert!(linux.contains("rootless Docker"));
        assert!(linux.contains("DOCKER_HOST"));

        let windows = docker_guidance(HostPlatform::Windows);
        assert!(windows.contains("Docker Desktop or Docker Engine"));
        assert!(windows.contains("named-pipe permissions"));
        assert!(windows.contains("docker context show"));
        assert!(windows.contains("DOCKER_HOST"));
    }

    #[test]
    fn pull_progress_is_interactive_only_for_human_ttys() {
        assert_eq!(
            pull_progress_mode(true, true, false),
            PullProgressMode::Interactive
        );
        assert_eq!(
            pull_progress_mode(false, true, false),
            PullProgressMode::Collapsed
        );
        assert_eq!(
            pull_progress_mode(true, false, false),
            PullProgressMode::Collapsed
        );
        assert_eq!(
            pull_progress_mode(true, true, true),
            PullProgressMode::Silent
        );
    }

    #[test]
    fn collapsed_pull_progress_is_one_bounded_summary_line() {
        let mut output = Vec::new();
        let reporter = PullReporter::new("postgres:18".to_string(), PullProgressMode::Collapsed);
        reporter.start(&mut output);
        for (id, status) in [
            ("layer-a", "Pulling fs layer"),
            ("layer-a", "Downloading"),
            ("layer-b", "Pulling fs layer"),
            ("layer-b", "Pull complete"),
        ] {
            reporter.event(
                &CreateImageInfo {
                    id: Some(id.to_string()),
                    status: Some(status.to_string()),
                    ..Default::default()
                },
                &mut output,
            );
        }
        reporter.finish(&mut output);

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Pulling postgres:18... done\n"
        );
    }

    #[test]
    fn interactive_pull_progress_keeps_layer_and_byte_context() {
        let mut output = Vec::new();
        let reporter = PullReporter::new("postgres:18".to_string(), PullProgressMode::Interactive);
        reporter.start(&mut output);
        reporter.event(
            &CreateImageInfo {
                id: Some("layer-a".to_string()),
                status: Some("Downloading".to_string()),
                progress_detail: Some(ProgressDetail {
                    current: Some(50),
                    total: Some(100),
                }),
                ..Default::default()
            },
            &mut output,
        );
        reporter.finish(&mut output);

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Pulling postgres:18...\n  layer-a: Downloading (50/100 bytes, 50%)\nPulled postgres:18\n"
        );
    }

    #[test]
    fn collapsed_pull_failure_closes_the_summary_line() {
        let mut output = Vec::new();
        let reporter =
            PullReporter::new("postgres:missing".to_string(), PullProgressMode::Collapsed);
        reporter.start(&mut output);
        reporter.fail(&mut output);

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Pulling postgres:missing... failed\n"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn remove_host_dir_removes_normal_directory() {
        let tempdir = tempfile::tempdir().expect("create cleanup tempdir");
        let directory = tempdir.path().join("normal-pg18");
        std::fs::create_dir_all(directory.join("data")).expect("create data directory");
        std::fs::write(directory.join("data/PG_VERSION"), "18").expect("write data file");

        remove_host_dir_blocking(&directory).expect("remove host directory");

        assert!(!directory.exists());
    }

    #[test]
    fn privileged_remove_passes_metacharacters_as_one_argument() {
        let basename = "db; touch injected; $(whoami) *";

        assert_eq!(
            privileged_remove_command(basename),
            vec![
                "rm".to_string(),
                "-rf".to_string(),
                "--".to_string(),
                format!("/work/{basename}"),
            ]
        );
    }

    #[test]
    fn missing_container_is_already_removed() {
        assert!(
            remove_container_result(Err(BollardError::DockerResponseServerError {
                status_code: 404,
                message: "No such container".to_string(),
            }))
            .is_ok()
        );
    }

    #[test]
    fn log_tail_is_bounded_by_bytes() {
        let mut buffer = Vec::new();
        assert!(!append_bounded_tail(&mut buffer, b"first\n", 10));
        assert!(append_bounded_tail(&mut buffer, b"second\n", 10));
        assert_eq!(buffer, b"st\nsecond\n");

        assert!(append_bounded_tail(&mut buffer, b"0123456789abcdef", 10));
        assert_eq!(buffer, b"6789abcdef");
    }
}
