//! Handlers for `dctl local server ...` when the engine is ClickHouse (Docker).
//!
//! All Docker work goes through `local::docker`. State is reused from
//! `local::server`. The client runs inside dctl (HTTP for queries, docker
//! exec for interactive), so no host `clickhouse-client` binary is needed.

use crate::error::{Error, PortKind, Result, StartupKind};
use crate::local::docker::{self, ClickhouseRunOpts};
use crate::local::output;
use crate::local::server::{self, Engine, ServerInfo};
use rand::distr::{Alphanumeric, SampleString};
use std::future::Future;
use std::path::Path;
use std::time::Duration;

const DEFAULT_CH_HTTP_PORT: u16 = 8123;
const DEFAULT_CH_NATIVE_PORT: u16 = 9000;
const DEFAULT_USER: &str = "default";
const DEFAULT_DATABASE: &str = "default";
/// Default image tag when `--version` is not given. The image publishes
/// minor-only rolling tags (like Postgres major tags), so this tracks the
/// current minor without pinning a patch.
pub const DEFAULT_CH_TAG: &str = "26.8";
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const READINESS_LOG_LINES: usize = 50;
const READINESS_LOG_BYTES: usize = 16 * 1024;
const READINESS_LOG_TIMEOUT: Duration = Duration::from_secs(2);

/// Full image reference for a validated tag.
pub(crate) fn ch_image_ref(tag: &str) -> String {
    format!("clickhouse/clickhouse-server:{tag}")
}

/// Accept ClickHouse image tags: `latest` or `N(.N){1,3}` (2-4 segments).
pub(crate) fn validate_ch_tag(tag: &str) -> Result<()> {
    let valid = tag == "latest" || {
        tag.len() <= 128
            && tag.is_ascii()
            && tag.split('.').count() >= 2
            && tag.split('.').count() <= 4
            && tag
                .split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    };
    if !valid {
        return Err(Error::ClickhouseUsage(format!(
            "invalid or unsupported ClickHouse version '{tag}'. Use a version like 26.8, \
             26.8.9, or 26.8.9.10, or latest."
        )));
    }
    Ok(())
}

pub(crate) fn parse_ch_tag_arg(tag: &str) -> std::result::Result<String, String> {
    validate_ch_tag(tag)
        .map(|()| tag.to_string())
        .map_err(|error| error.to_string())
}

pub(crate) fn parse_ch_port_arg(value: &str) -> std::result::Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| format!("invalid port '{value}': expected an integer from 1 to 65535"))?;
    if port == 0 {
        return Err("--port 0 is not allowed; pick a specific port or omit the flag".into());
    }
    Ok(port)
}

/// Tag stored in `ServerInfo.version` back to bare form.
fn tag_from_stored_version(stored: &str) -> &str {
    stored.strip_prefix("clickhouse:").unwrap_or(stored)
}

struct StartPreflight {
    http_port: Option<u16>,
    native_port: Option<u16>,
    config_source: Option<std::path::PathBuf>,
    extra_env: Vec<String>,
}

fn validate_start_options(
    name: Option<&str>,
    version: Option<&str>,
    http_port: Option<u16>,
    native_port: Option<u16>,
    password: Option<&str>,
    config: Option<&str>,
    extra_env: Vec<String>,
) -> Result<StartPreflight> {
    if let Some(name) = name {
        server::validate_server_name(name)?;
    }
    if let Some(version) = version {
        validate_ch_tag(version)?;
    }
    if let Some(password) = password
        && (password.is_empty() || password.chars().any(char::is_whitespace))
    {
        return Err(Error::ClickhouseUsage(
            "invalid --password: must be non-empty and contain no whitespace".into(),
        ));
    }
    for assignment in &extra_env {
        if let Some(key) = assignment.split('=').next()
            && matches!(
                key,
                "CLICKHOUSE_USER" | "CLICKHOUSE_PASSWORD" | "CLICKHOUSE_DB"
            )
        {
            return Err(Error::ClickhouseUsage(format!(
                "{key} is managed by dctl; use the corresponding flag instead of --env"
            )));
        }
    }

    let http_port = http_port
        .map(|port| resolve_port(Some(port), PortKind::Http))
        .transpose()?;
    let native_port = native_port
        .map(|port| resolve_port(Some(port), PortKind::Clickhouse))
        .transpose()?;
    if let (Some(http), Some(native)) = (http_port, native_port)
        && http == native
    {
        return Err(Error::ClickhouseUsage(format!(
            "--http-port and --native-port cannot both be {http}; pick distinct ports or omit them to auto-select"
        )));
    }

    let config_source = if let Some(config) = config {
        Some(crate::local::config::resolve_config(config)?)
    } else {
        None
    };

    Ok(StartPreflight {
        http_port,
        native_port,
        config_source,
        extra_env,
    })
}

/// `dctl local server start` flags, verbatim from clap.
pub(crate) struct StartCmd {
    pub name: Option<String>,
    pub version: Option<String>,
    pub http_port: Option<u16>,
    pub native_port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub config: Option<String>,
    pub extra_env: Vec<String>,
    pub wait_timeout: Duration,
    pub json: bool,
}

pub(crate) async fn start(cmd: StartCmd) -> Result<()> {
    let StartCmd {
        name,
        version,
        http_port,
        native_port,
        user,
        password,
        database,
        config,
        extra_env,
        wait_timeout,
        json,
    } = cmd;
    let preflight = validate_start_options(
        name.as_deref(),
        version.as_deref(),
        http_port,
        native_port,
        password.as_deref(),
        config.as_deref(),
        extra_env,
    )?;
    let explicit_http_port = preflight.http_port;
    let explicit_native_port = preflight.native_port;
    let config_source = preflight.config_source;
    let extra_env = preflight.extra_env;

    let docker = docker::connect().await?;
    let project_cwd = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    loop {
        let metadata_lock = server::lock_metadata()?;
        server::recover_current_project_servers_locked(&metadata_lock)?;
        let user_name = match name.as_deref() {
            Some(name) => name.to_string(),
            None => default_ch_name_locked(&metadata_lock)?,
        };
        let tag = resolve_ch_start_version_locked(&user_name, version.as_deref(), &metadata_lock)?;
        let key = server::ch_instance_key(&user_name, &tag);
        let prior = server::load_info_locked(&key, &metadata_lock)?;
        drop(metadata_lock);

        if prior.is_none() {
            let image_ref = ch_image_ref(&tag);
            if !docker::image_exists(&docker, &image_ref).await? {
                docker::pull_image(&docker, &image_ref, json).await?;
            }
            docker::ensure_name_free(
                &docker,
                &docker::ch_container_name(&user_name, &tag),
                docker::ENGINE_CLICKHOUSE,
                &project_cwd,
            )
            .await?;
        }

        let metadata_lock = server::lock_metadata()?;
        let current_tag =
            resolve_ch_start_version_locked(&user_name, version.as_deref(), &metadata_lock)?;
        let current_key = server::ch_instance_key(&user_name, &current_tag);
        let current = server::load_info_locked(&current_key, &metadata_lock)?;
        if current_tag != tag || current_key != key || current != prior {
            drop(metadata_lock);
            continue;
        }
        crate::init::ensure_runtime_gitignore()?;

        // Resume path.
        if let Some(prior) = prior {
            let cid = prior.container_id.as_deref().unwrap_or("");
            let inspected = if cid.is_empty() {
                None
            } else {
                docker::inspect_container(&docker, cid).await?
            };
            let Some(inspected) = inspected else {
                return Err(Error::ClickhouseUsage(format!(
                    "server '{}' (clickhouse:{tag}) has metadata but the container is gone. \
                     Run `dctl local server remove {user_name}` to clear the data dir \
                     and start fresh.",
                    user_name
                )));
            };
            if docker::inspected_container_running(&inspected)? {
                return Err(Error::ServerAlreadyRunning(user_name));
            }
            return resume_existing(&docker, prior, wait_timeout, json, metadata_lock).await;
        }

        // Fresh create.
        let http_port = match explicit_http_port {
            Some(port) => port,
            None => resolve_port(None, PortKind::Http)?,
        };
        let native_port = match explicit_native_port {
            Some(port) => port,
            None => resolve_native_port_excluding(http_port)?,
        };

        let instance_dir = server::servers_dir_join(&key);
        let remove_fresh_data_on_failure = fresh_instance_dir_is_disposable(&instance_dir);
        server::ensure_ch_data_dir(&user_name, &tag)?;
        let data_dir = server::ch_data_dir(&user_name, &tag);

        let user = user.unwrap_or_else(|| DEFAULT_USER.to_string());
        let database = database.unwrap_or_else(|| DEFAULT_DATABASE.to_string());
        let password = password.unwrap_or_else(generate_password);

        let opts = ClickhouseRunOpts {
            user_name: &user_name,
            version: &tag,
            image_ref: &ch_image_ref(&tag),
            http_port,
            native_port,
            data_dir: &data_dir,
            project_cwd: &project_cwd,
            user: &user,
            password: &password,
            database: &database,
            config_source: config_source.as_deref(),
            extra_env,
        };

        let container_id = docker::create_clickhouse(&docker, opts).await?;

        let info = ServerInfo {
            name: key.clone(),
            pid: 0,
            version: format!("clickhouse:{tag}"),
            http_port,
            tcp_port: native_port,
            started_at: server::now_timestamp(),
            cwd: project_cwd.clone(),
            engine: Engine::Clickhouse,
            container_id: Some(container_id.clone()),
        };
        let startup_result = async {
            docker::start_existing(&docker, &container_id).await?;
            server::save_server_info_locked(&info, &metadata_lock)
        }
        .await;

        if let Err(primary) = startup_result {
            return Err(rollback_failed_fresh_start(
                &docker,
                &container_id,
                &info,
                remove_fresh_data_on_failure,
                primary,
                &metadata_lock,
            )
            .await);
        }
        drop(metadata_lock);

        if let Err(failure) =
            wait_for_ch_ready(&docker, &container_id, http_port, wait_timeout).await
        {
            let primary =
                ch_readiness_error(&docker, &container_id, &user_name, wait_timeout, failure).await;
            let metadata_lock = server::lock_metadata()?;
            return Err(rollback_failed_fresh_start(
                &docker,
                &container_id,
                &info,
                remove_fresh_data_on_failure,
                primary,
                &metadata_lock,
            )
            .await);
        }

        let out = output::ClickhouseStartOutput {
            name: user_name,
            container_id,
            image: info.version,
            http_port,
            native_port,
            user,
            password,
            database,
        };
        output::print_output(&out, json);
        return Ok(());
    }
}

fn fresh_instance_dir_is_disposable(path: &Path) -> bool {
    let mut entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let entry = match entries.next() {
        None => return true,
        Some(Ok(entry)) => entry,
        Some(Err(_)) => return false,
    };
    if entries.next().is_some()
        || entry.file_name() != "data"
        || !entry.file_type().is_ok_and(|ft| ft.is_dir())
    {
        return false;
    }
    match std::fs::read_dir(entry.path()) {
        Ok(mut data_entries) => data_entries.next().is_none(),
        Err(_) => false,
    }
}

async fn rollback_failed_fresh_start(
    docker: &bollard::Docker,
    container_id: &str,
    info: &ServerInfo,
    remove_fresh_data_on_failure: bool,
    primary: Error,
    metadata_lock: &server::MetadataLock,
) -> Error {
    let instance_dir = server::servers_dir_join(&info.name);
    let mut diagnostics = Vec::new();

    let container_removed = match docker::remove_container(docker, container_id).await {
        Ok(()) => true,
        Err(error) => {
            diagnostics.push(format!(
                "failed to remove container '{container_id}': {error}"
            ));
            false
        }
    };

    let instance_removed = if remove_fresh_data_on_failure && container_removed {
        match docker::remove_host_dir_blocking(&instance_dir) {
            Ok(()) if !instance_dir.exists() => true,
            Ok(()) => {
                diagnostics.push(format!(
                    "failed to remove fresh ClickHouse data '{}': path still exists",
                    instance_dir.display()
                ));
                false
            }
            Err(error) => {
                diagnostics.push(format!(
                    "failed to remove fresh ClickHouse data '{}': {error}",
                    instance_dir.display()
                ));
                false
            }
        }
    } else {
        let reason = if remove_fresh_data_on_failure {
            "the container could not be removed"
        } else {
            "the directory contained data before this start attempt"
        };
        diagnostics.push(format!(
            "retained ClickHouse data '{}' because {reason}",
            instance_dir.display()
        ));
        false
    };

    if container_removed && instance_removed {
        match server::try_remove_server_info_locked(&info.name, metadata_lock) {
            Ok(()) => return primary,
            Err(error) => diagnostics.push(format!("failed to remove metadata: {error}")),
        }
    } else {
        match server::save_server_info_locked(info, metadata_lock) {
            Ok(()) => diagnostics.push(format!(
                "recovery metadata retained; run `dctl local server remove {}` to clean up",
                ch_user_name_from_key(&info.name)
            )),
            Err(error) => {
                diagnostics.push(format!("failed to preserve recovery metadata: {error}"))
            }
        }
    }

    Error::ClickhouseStartupRollback {
        primary: Box::new(primary),
        cleanup: diagnostics.join("; "),
    }
}

fn default_ch_name_locked(metadata_lock: &server::MetadataLock) -> Result<String> {
    for info in server::find_ch_instances_locked("default", metadata_lock)? {
        if let Some(id) = info.container_id.as_deref()
            && docker::is_container_running_blocking(id)?
        {
            return server::generate_random_name_locked(metadata_lock);
        }
    }
    Ok("default".into())
}

fn resolve_ch_start_version_locked(
    user_name: &str,
    version: Option<&str>,
    metadata_lock: &server::MetadataLock,
) -> Result<String> {
    if let Some(version) = version {
        return Ok(version.to_string());
    }
    let existing = server::find_ch_instances_locked(user_name, metadata_lock)?;
    match existing.as_slice() {
        [] => Ok(DEFAULT_CH_TAG.to_string()),
        [info] => Ok(tag_from_stored_version(&info.version).to_string()),
        _ => {
            let versions: Vec<&str> = existing.iter().map(|i| i.version.as_str()).collect();
            Err(Error::ClickhouseUsage(format!(
                "multiple ClickHouse instances named '{}' ({}); pass --version to select one",
                user_name,
                versions.join(", ")
            )))
        }
    }
}

/// Resolve `[NAME] [--version <V>]` to a single Docker-managed ClickHouse instance.
fn resolve_ch_target_locked(
    user_name: &str,
    version: Option<&str>,
    metadata_lock: &server::MetadataLock,
) -> Result<server::ServerInfo> {
    if let Some(v) = version {
        validate_ch_tag(v)?;
        let key = server::ch_instance_key(user_name, v);
        return server::load_info_locked(&key, metadata_lock)?
            .filter(|i| i.engine == Engine::Clickhouse && i.container_id.is_some())
            .ok_or_else(|| Error::ServerNotFound(format!("{user_name} (clickhouse:{v})")));
    }
    let instances = server::find_ch_instances_locked(user_name, metadata_lock)?;
    match instances.len() {
        0 => Err(Error::ServerNotFound(user_name.to_string())),
        1 => Ok(instances.into_iter().next().unwrap()),
        _ => {
            let versions: Vec<String> = instances.iter().map(|i| i.version.clone()).collect();
            Err(Error::ClickhouseUsage(format!(
                "multiple ClickHouse instances named '{}' ({}); pass --version to select one",
                user_name,
                versions.join(", ")
            )))
        }
    }
}

async fn resume_existing(
    docker: &bollard::Docker,
    prior: ServerInfo,
    wait_timeout: Duration,
    json: bool,
    metadata_lock: server::MetadataLock,
) -> Result<()> {
    let container_id = prior.container_id.clone().expect("checked by caller");
    let display_name = ch_user_name_from_key(&prior.name).to_string();

    docker::start_existing(docker, &container_id).await?;

    let info = ServerInfo {
        started_at: server::now_timestamp(),
        ..prior
    };
    if let Err(primary) = server::save_server_info_locked(&info, &metadata_lock) {
        drop(metadata_lock);
        return match docker::stop_container(docker, &container_id).await {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::ClickhouseStartupRollback {
                primary: Box::new(primary),
                cleanup: format!(
                    "could not stop resumed container '{container_id}' after metadata failure: {cleanup}"
                ),
            }),
        };
    }
    drop(metadata_lock);

    if let Err(failure) =
        wait_for_ch_ready(docker, &container_id, info.http_port, wait_timeout).await
    {
        let error =
            ch_readiness_error(docker, &container_id, &display_name, wait_timeout, failure).await;
        let _ = docker::stop_container(docker, &container_id).await;
        return Err(error);
    }

    let out = output::ClickhouseStartOutput {
        name: display_name,
        container_id,
        image: info.version,
        http_port: info.http_port,
        native_port: info.tcp_port,
        user: DEFAULT_USER.to_string(),
        password: String::new(),
        database: DEFAULT_DATABASE.to_string(),
    };
    output::print_output(&out, json);
    Ok(())
}

pub(crate) fn ch_user_name_from_key(key: &str) -> &str {
    if let Some(idx) = key.rfind("-ch")
        && server::is_ch_instance_key(key)
    {
        return &key[..idx];
    }
    key
}

// ── readiness: host-side GET /ping ─────────────────────────────────────────

#[derive(Debug, Clone, Eq, PartialEq)]
enum ReadinessState {
    Pending,
    Running,
    Exited {
        status: String,
        exit_code: Option<i64>,
        oom_killed: bool,
    },
}

#[derive(Debug)]
enum ReadinessFailure {
    Exited {
        status: String,
        exit_code: Option<i64>,
        oom_killed: bool,
    },
    Probe(Error),
    TimedOut {
        last_probe_error: Option<String>,
    },
}

/// Host-side HTTP ping: the only engine of the three that can be probed
/// without entering the container.
async fn ch_is_ready(http_port: u16) -> Result<bool> {
    let url = format!("http://127.0.0.1:{http_port}/ping");
    let client = crate::http::client_builder()
        .timeout(Duration::from_millis(500))
        .no_proxy()
        .build()?;
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            let body = response.text().await.unwrap_or_default();
            Ok(body.trim() == "Ok.")
        }
        _ => Ok(false),
    }
}

async fn probe_container_state(
    docker: &bollard::Docker,
    container_id: &str,
) -> Result<ReadinessState> {
    let state = docker::container_state(docker, container_id).await?;
    if state.running {
        Ok(ReadinessState::Running)
    } else if state.exited {
        Ok(ReadinessState::Exited {
            status: state.status,
            exit_code: state.exit_code,
            oom_killed: state.oom_killed,
        })
    } else {
        Ok(ReadinessState::Pending)
    }
}

async fn poll_ch_readiness(
    docker: &bollard::Docker,
    container_id: &str,
    http_port: u16,
    max_checks: usize,
    mut sleep: impl FnMut() -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>>,
    last_probe_error: &mut Option<String>,
) -> std::result::Result<(), ReadinessFailure> {
    for check in 0..max_checks {
        match probe_container_state(docker, container_id).await {
            Ok(ReadinessState::Exited {
                status,
                exit_code,
                oom_killed,
            }) => {
                return Err(ReadinessFailure::Exited {
                    status,
                    exit_code,
                    oom_killed,
                });
            }
            Ok(ReadinessState::Running) => match ch_is_ready(http_port).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    *last_probe_error = Some(error.to_string());
                }
            },
            Ok(ReadinessState::Pending) => {}
            Err(error) => return Err(ReadinessFailure::Probe(error)),
        }
        if check + 1 < max_checks {
            sleep().await;
        }
    }
    Err(ReadinessFailure::TimedOut {
        last_probe_error: last_probe_error.take(),
    })
}

async fn wait_for_ch_ready(
    docker: &bollard::Docker,
    container_id: &str,
    http_port: u16,
    timeout: Duration,
) -> std::result::Result<(), ReadinessFailure> {
    let mut last_probe_error = None;
    match tokio::time::timeout(
        timeout,
        poll_ch_readiness(
            docker,
            container_id,
            http_port,
            usize::MAX,
            || Box::pin(tokio::time::sleep(READINESS_POLL_INTERVAL)),
            &mut last_probe_error,
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ReadinessFailure::TimedOut { last_probe_error }),
    }
}

async fn ch_readiness_error(
    docker: &bollard::Docker,
    container_id: &str,
    display_name: &str,
    timeout: Duration,
    failure: ReadinessFailure,
) -> Error {
    let logs = match tokio::time::timeout(
        READINESS_LOG_TIMEOUT,
        docker::container_logs_tail(
            docker,
            container_id,
            READINESS_LOG_LINES,
            READINESS_LOG_BYTES,
        ),
    )
    .await
    {
        Ok(Ok(logs)) if !logs.trim().is_empty() && logs != "(no container logs)" => logs,
        _ => format!("Run `docker logs {container_id}` for diagnostics."),
    };
    let diagnostics = format!(
        "--- last {READINESS_LOG_LINES} container log lines (max {READINESS_LOG_BYTES} bytes) ---\n{logs}"
    );
    match failure {
        ReadinessFailure::Exited {
            status,
            exit_code,
            oom_killed,
        } => {
            let exit_code = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            let oom = if oom_killed { "; out of memory" } else { "" };
            Error::StartupExit {
                kind: StartupKind::ClickHouse,
                name: display_name.to_string(),
                details: format!(
                    "ClickHouse container '{display_name}' exited before becoming ready \
                     (status: {status}, exit code: {exit_code}{oom}).\n{diagnostics}"
                ),
            }
        }
        ReadinessFailure::Probe(error) => Error::DockerError(format!(
            "Could not check ClickHouse readiness in container '{display_name}': {error}.\n{diagnostics}"
        )),
        ReadinessFailure::TimedOut { last_probe_error } => {
            let probe_context = last_probe_error
                .map(|e| format!(" Last readiness probe error: {e}."))
                .unwrap_or_default();
            Error::StartupTimeout {
                kind: StartupKind::ClickHouse,
                name: display_name.to_string(),
                seconds: timeout.as_secs(),
                details: format!(
                    "ClickHouse in container '{display_name}' did not become ready within \
                     {} seconds.{probe_context}\n{diagnostics}",
                    timeout.as_secs()
                ),
            }
        }
    }
}

// ── ports ──────────────────────────────────────────────────────────────────

fn resolve_port(explicit: Option<u16>, kind: PortKind) -> Result<u16> {
    let default_port = match kind {
        PortKind::Clickhouse => DEFAULT_CH_NATIVE_PORT,
        _ => DEFAULT_CH_HTTP_PORT,
    };
    match explicit {
        Some(0) => {
            return Err(Error::ClickhouseUsage(
                "--port 0 is not allowed; pick a specific port or omit the flag".into(),
            ));
        }
        Some(port) if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() => return Ok(port),
        Some(port) => return Err(Error::PortInUse { kind, port }),
        None => {}
    }
    if std::net::TcpListener::bind(("127.0.0.1", default_port)).is_ok() {
        return Ok(default_port);
    }
    for p in (default_port + 1)..=(default_port + 100) {
        if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(kind))
}

fn resolve_native_port_excluding(http_port: u16) -> Result<u16> {
    let picked = resolve_port(None, PortKind::Clickhouse)?;
    if picked != http_port {
        return Ok(picked);
    }
    for p in (DEFAULT_CH_NATIVE_PORT + 1)..=(DEFAULT_CH_NATIVE_PORT + 101) {
        if p != http_port && std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(PortKind::Clickhouse))
}

fn generate_password() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 24)
}

// ── client: HTTP for queries, docker exec for interactive ─────────────────

/// Execute a SQL query via the ClickHouse HTTP interface.
pub(crate) async fn http_query(
    host: &str,
    port: u16,
    user: Option<&str>,
    password: Option<&str>,
    database: Option<&str>,
    sql: &str,
) -> Result<String> {
    let url = format!("http://{host}:{port}/");
    let mut request = crate::http::client_builder()
        .timeout(Duration::from_secs(120))
        .no_proxy()
        .build()?
        .post(&url)
        .header("Content-Type", "text/plain; charset=utf-8");
    if let (Some(user), Some(password)) = (user, password) {
        request = request.basic_auth(user, Some(password));
    }
    if let Some(database) = database {
        request = request.query(&[("database", database)]);
    }
    let response = request
        .body(sql.to_string())
        .send()
        .await
        .map_err(|e| Error::ClickhouseUsage(format!("ClickHouse HTTP query failed: {e}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::ClickhouseUsage(format!(
            "ClickHouse HTTP {status}: {body}"
        )));
    }
    Ok(response.text().await.unwrap_or_default())
}

/// Read the provisioned credentials from the container's effective env.
async fn read_ch_env(docker: &bollard::Docker, id: &str) -> (String, String, String) {
    let inspect = docker.inspect_container(id, None).await.ok();
    let env: Vec<String> = inspect
        .and_then(|c| c.config)
        .and_then(|c| c.env)
        .unwrap_or_default();
    let get = |k: &str| -> Option<String> {
        env.iter()
            .find_map(|e| e.strip_prefix(&format!("{k}=")).map(str::to_string))
    };
    (
        get("CLICKHOUSE_USER").unwrap_or_else(|| DEFAULT_USER.into()),
        get("CLICKHOUSE_PASSWORD").unwrap_or_default(),
        get("CLICKHOUSE_DB").unwrap_or_else(|| DEFAULT_DATABASE.into()),
    )
}

pub(crate) async fn client(
    name: Option<String>,
    version: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    query: Option<String>,
    queries_file: Option<String>,
    database: Option<String>,
) -> Result<()> {
    // Direct mode: HTTP to the given host/port.
    if host.is_some() || port.is_some() {
        let h = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let p = port.unwrap_or(DEFAULT_CH_HTTP_PORT);
        let sql = read_query_input(query.as_deref(), queries_file.as_deref())?;
        let result = http_query(&h, p, None, None, database.as_deref(), &sql).await?;
        print!("{result}");
        return Ok(());
    }

    // Managed mode: look up the instance, use its port and credentials.
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let server_name = name.as_deref().unwrap_or("default");
    let info = resolve_ch_target_locked(server_name, version.as_deref(), &metadata_lock)?;
    if !server::is_server_running_locked(&info.name, &metadata_lock)? {
        return Err(Error::ServerNotRunning(server_name.to_string()));
    }
    drop(metadata_lock);

    let docker = docker::connect().await?;
    let container_id = info
        .container_id
        .as_deref()
        .ok_or_else(|| Error::DockerError("missing container_id".into()))?;
    let (user, password, db) = read_ch_env(&docker, container_id).await;

    if query.is_some() || queries_file.is_some() {
        let sql = read_query_input(query.as_deref(), queries_file.as_deref())?;
        let result = http_query(
            "127.0.0.1",
            info.http_port,
            Some(&user),
            Some(&password),
            database.as_deref().or(Some(db.as_str())),
            &sql,
        )
        .await?;
        print!("{result}");
        return Ok(());
    }

    // Interactive: docker exec clickhouse-client with TTY.
    let cli_args: Vec<String> = vec![
        "--user".into(),
        user.clone(),
        "--password".into(),
        password.clone(),
        "--database".into(),
        database.clone().unwrap_or(db),
        "--interactive".into(),
    ];
    docker::exec_clickhouse_client_in_container(&docker, container_id, &cli_args).await
}

fn read_query_input(query: Option<&str>, queries_file: Option<&str>) -> Result<String> {
    if let Some(query) = query {
        return Ok(query.to_string());
    }
    if let Some(file) = queries_file {
        if file == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| Error::ClickhouseUsage(format!("could not read stdin: {e}")))?;
            return Ok(buf);
        }
        return std::fs::read_to_string(file).map_err(|e| {
            Error::ClickhouseUsage(format!("could not read queries file {file}: {e}"))
        });
    }
    Ok(String::new())
}

// ── stop / remove / dotenv ─────────────────────────────────────────────────

pub(crate) async fn stop(name: &str, version: Option<&str>, json: bool) -> Result<()> {
    server::validate_server_name(name)?;
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let target = resolve_ch_target_locked(name, version, &metadata_lock)?;
    if !json {
        let display = format!(
            "{} ({})",
            ch_user_name_from_key(&target.name),
            target.version
        );
        println!("Stopping ClickHouse {display}...");
    }
    server::kill_server_locked(&target.name, &metadata_lock)?;
    let out = output::ServerStopOutput {
        name: ch_user_name_from_key(&target.name).to_string(),
        already_stopped: false,
        selection: None,
    };
    output::print_output(&out, json);
    Ok(())
}

pub(crate) fn remove(name: &str, version: Option<&str>, json: bool) -> Result<()> {
    server::validate_server_name(name)?;
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;

    let target = resolve_ch_target_locked(name, version, &metadata_lock)?;
    let key = target.name.clone();
    if server::is_server_running_locked(&key, &metadata_lock)? {
        let tag = tag_from_stored_version(&target.version);
        return Err(Error::ServerRunningCannotRemove {
            name: name.to_string(),
            command: format!("dctl local server stop {name} --version {tag}"),
        });
    }

    if let Some(cid) = target.container_id.as_deref() {
        let _ = docker::stop_and_remove_blocking(cid);
    }

    let ch_dir = server::servers_dir_join(&key);
    docker::remove_host_dir_blocking(&ch_dir)?;
    server::try_remove_server_info_locked(&key, &metadata_lock)?;
    let out = output::ServerRemoveOutput {
        name: name.to_string(),
        selection: None,
    };
    output::print_output(&out, json);
    Ok(())
}

pub(crate) fn dotenv(
    name: Option<&str>,
    version: Option<&str>,
    use_local: bool,
    json: bool,
) -> Result<()> {
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let server_name = name.unwrap_or("default");
    let info = resolve_ch_target_locked(server_name, version, &metadata_lock)?;
    if !server::is_server_running_locked(&info.name, &metadata_lock)? {
        return Err(Error::ServerNotRunning(server_name.to_string()));
    }
    drop(metadata_lock);

    let (user, password, database) = docker::block_on(read_ch_env_for_dotenv(
        info.container_id.as_deref().unwrap_or_default(),
    ));

    let vars: Vec<(&str, String)> = vec![
        ("CLICKHOUSE_HOST", "127.0.0.1".to_string()),
        ("CLICKHOUSE_HTTP_PORT", info.http_port.to_string()),
        ("CLICKHOUSE_PORT", info.tcp_port.to_string()),
        ("CLICKHOUSE_USER", user),
        ("CLICKHOUSE_PASSWORD", password),
        ("CLICKHOUSE_DATABASE", database),
    ];

    let filename = if use_local { ".env.local" } else { ".env" };
    let path = std::path::Path::new(filename);

    let content = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        crate::local::update_dotenv(&existing, "CLICKHOUSE_", &vars)
    } else {
        vars.iter()
            .map(|(k, v)| crate::local::format_dotenv_line("", k, v))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    std::fs::write(path, &content)?;

    let out = output::ClickhouseDotenvOutput {
        file: filename.to_string(),
        server: server_name.to_string(),
        vars: vars
            .into_iter()
            .map(|(k, v)| output::DotenvVar {
                key: k.to_string(),
                value: v,
            })
            .collect(),
    };
    output::print_output(&out, json);
    Ok(())
}

async fn read_ch_env_for_dotenv(container_id: &str) -> (String, String, String) {
    if container_id.is_empty() {
        return (DEFAULT_USER.into(), String::new(), DEFAULT_DATABASE.into());
    }
    match docker::connect().await {
        Ok(d) => read_ch_env(&d, container_id).await,
        Err(_) => (DEFAULT_USER.into(), String::new(), DEFAULT_DATABASE.into()),
    }
}
