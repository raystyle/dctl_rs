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

pub(crate) fn parse_ch_http_port_arg(value: &str) -> std::result::Result<u16, String> {
    let port = parse_ch_port_value(value)?;
    if port == 0 {
        return Err("--http-port 0 is not allowed; pick a specific port or omit the flag".into());
    }
    Ok(port)
}

pub(crate) fn parse_ch_native_port_arg(value: &str) -> std::result::Result<u16, String> {
    let port = parse_ch_port_value(value)?;
    if port == 0 {
        return Err("--native-port 0 is not allowed; pick a specific port or omit the flag".into());
    }
    Ok(port)
}

/// `--bind <IP>`: an extra host face for the published ports, stored in
/// canonical form (`192.168.88.175`, `::1`, `0.0.0.0`).
pub(crate) fn parse_ch_bind_arg(value: &str) -> std::result::Result<String, String> {
    parse_ch_bind(value)
        .map(|ip| ip.to_string())
        .map_err(|error| error.to_string())
}

fn parse_ch_bind(value: &str) -> Result<std::net::IpAddr> {
    value.parse().map_err(|_| {
        Error::ClickhouseUsage(format!(
            "invalid --bind address '{value}': expected an IPv4 or IPv6 host address \
             (for example: 192.168.88.175), or 0.0.0.0 to publish on all interfaces"
        ))
    })
}

fn parse_ch_port_value(value: &str) -> std::result::Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("invalid port '{value}': expected an integer from 1 to 65535"))
}

/// `-e KEY=VALUE` for `server start`: shape plus the managed-key guard, the
/// same contract pg/fk enforce at clap time.
pub(crate) fn parse_ch_env_arg(assignment: &str) -> std::result::Result<String, String> {
    let (key, _) = assignment
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VALUE, got '{assignment}'"))?;
    if key.is_empty() || key.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(format!(
            "invalid env key '{key}': must not be empty or start with a digit"
        ));
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "invalid env key '{key}': only [A-Za-z0-9_] allowed"
        ));
    }
    if matches!(
        key,
        "CLICKHOUSE_USER" | "CLICKHOUSE_PASSWORD" | "CLICKHOUSE_DB"
    ) {
        return Err(format!(
            "{key} is managed by dctl; use the corresponding flag instead of --env"
        ));
    }
    Ok(assignment.to_string())
}

/// Tag stored in `ServerInfo.version` back to bare form.
fn tag_from_stored_version(stored: &str) -> &str {
    stored.strip_prefix("clickhouse:").unwrap_or(stored)
}

#[derive(Debug)]
struct StartPreflight {
    http_port: Option<u16>,
    native_port: Option<u16>,
    bind_face: Option<std::net::IpAddr>,
    config_source: Option<std::path::PathBuf>,
    extra_env: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
fn validate_start_options(
    name: Option<&str>,
    version: Option<&str>,
    http_port: Option<u16>,
    native_port: Option<u16>,
    bind: Option<&str>,
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

    let bind_face = bind.map(parse_ch_bind).transpose()?;
    // A --bind face that is not present in this network namespace would
    // otherwise surface as "port already in use" (or degrade auto-pick);
    // fail with the real cause instead.
    if let Some(face) = bind_face
        && !face.is_unspecified()
        && let Err(error) = std::net::TcpListener::bind((face, 0))
        && error.kind() == std::io::ErrorKind::AddrNotAvailable
    {
        return Err(Error::ClickhouseUsage(format!(
            "--bind address {face} is not present on this host; run dctl where that \
             interface exists (the host network namespace)"
        )));
    }
    let http_port = http_port
        .map(|port| resolve_port(Some(port), PortKind::Http, bind_face))
        .transpose()?;
    let native_port = native_port
        .map(|port| resolve_port(Some(port), PortKind::Clickhouse, bind_face))
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
        bind_face,
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
    pub bind: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub config: Option<String>,
    pub extra_env: Vec<String>,
    pub wait_timeout: Duration,
    pub auth: crate::local::cli::AuthFaceArg,
    pub json: bool,
}

pub(crate) async fn start(cmd: StartCmd) -> Result<()> {
    let StartCmd {
        name,
        version,
        http_port,
        native_port,
        bind,
        user,
        password,
        database,
        config,
        extra_env,
        wait_timeout,
        auth,
        json,
    } = cmd;
    let preflight = validate_start_options(
        name.as_deref(),
        version.as_deref(),
        http_port,
        native_port,
        bind.as_deref(),
        password.as_deref(),
        config.as_deref(),
        extra_env,
    )?;
    let explicit_http_port = preflight.http_port;
    let explicit_native_port = preflight.native_port;
    let bind_face = preflight.bind_face;
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
                docker::pull_image(&docker, &image_ref, json, None).await?;
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
            None => match explicit_native_port {
                Some(native) => resolve_http_port_excluding(native, bind_face)?,
                None => resolve_port(None, PortKind::Http, bind_face)?,
            },
        };
        let native_port = match explicit_native_port {
            Some(port) => port,
            None => resolve_native_port_excluding(http_port, bind_face)?,
        };

        let instance_dir = server::servers_dir_join(&key)?;
        let remove_fresh_data_on_failure = fresh_instance_dir_is_disposable(&instance_dir);
        server::ensure_ch_data_dir(&user_name, &tag)?;
        let data_dir = server::ch_data_dir(&user_name, &tag)?;

        let tls = auth == crate::local::cli::AuthFaceArg::Cert;
        let user = user.unwrap_or_else(|| DEFAULT_USER.to_string());
        // The certificate user is the image's only user; a named user only
        // exists through the entrypoint's password flow (S005).
        if tls && user != DEFAULT_USER {
            return Err(Error::ClickhouseUsage(
                "the certificate face authenticates as the default user; pass --auth \
                 password to start a named user"
                    .into(),
            ));
        }
        let database = database.unwrap_or_else(|| DEFAULT_DATABASE.to_string());
        let password = if tls {
            String::new()
        } else {
            password.unwrap_or_else(generate_password)
        };

        let opts = ClickhouseRunOpts {
            user_name: &user_name,
            version: &tag,
            image_ref: &ch_image_ref(&tag),
            http_port,
            native_port,
            bind: bind_face,
            data_dir: &data_dir,
            project_cwd: &project_cwd,
            user: &user,
            password: &password,
            database: &database,
            config_source: config_source.as_deref(),
            extra_env,
            tls,
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
            tls: Some(tls),
        };
        // Issuance and upload live inside the rollback-covered block (the
        // material rides in the container layer, so removing the container
        // cleans it). The client pair is uploaded too: the in-container
        // clickhouse-client legs (readiness probe and the interactive REPL)
        // ride it; the certificate-face programmatic path connects from the
        // host with the CA material instead (REQ-0017).
        let startup_result = async {
            if tls {
                let cname = docker::ch_container_name(&user_name, &tag);
                let (server_cert, server_key) = crate::local::ca::issue_server_cert(&cname)?;
                let ca_cert =
                    std::fs::read_to_string(crate::local::ca::ensure_ca()?).map_err(|e| {
                        Error::ClickhouseUsage(format!(
                            "could not read the dctl CA certificate: {e}"
                        ))
                    })?;
                let (client_cert, client_key) = crate::local::ca::issue(DEFAULT_USER)?;
                docker::upload_clickhouse_tls_material(
                    &docker,
                    &container_id,
                    &server_cert,
                    &server_key,
                    &client_cert,
                    &client_key,
                    &ca_cert,
                    DEFAULT_USER,
                )
                .await?;
            }
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
            wait_for_ch_ready(&docker, &container_id, http_port, tls, wait_timeout).await
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

        let bootstrap = if tls {
            // The entrypoint skipped its database bootstrap (no CLICKHOUSE_DB
            // on this face, S005); the certificate client creates it instead.
            // A failure rolls the fresh start back like a readiness failure:
            // leaving a running server without the requested database would
            // be a half-product the next resume never repairs (review F4).
            if database != DEFAULT_DATABASE {
                Some(
                    https_query(
                        "127.0.0.1",
                        http_port,
                        DEFAULT_USER,
                        None,
                        &format!("CREATE DATABASE IF NOT EXISTS {database}"),
                    )
                    .await
                    .map(|_| ()),
                )
            } else {
                None
            }
        } else {
            warn_if_credentials_rejected(http_port, &user, &password, &database).await;
            None
        };
        if let Some(Err(primary)) = bootstrap {
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
    let mut diagnostics = Vec::new();
    // Rollback runs after the primary failure; a broken bucket address must
    // not panic the cleanup, it only skips the data-directory steps.
    let instance_dir = match server::servers_dir_join(&info.name) {
        Ok(dir) => Some(dir),
        Err(error) => {
            diagnostics.push(format!(
                "could not resolve the state dir for '{}': {error}",
                info.name
            ));
            None
        }
    };

    let container_removed = match docker::remove_container(docker, container_id).await {
        Ok(()) => true,
        Err(error) => {
            diagnostics.push(format!(
                "failed to remove container '{container_id}': {error}"
            ));
            false
        }
    };

    let instance_removed = if let (true, true, Some(instance_dir)) = (
        remove_fresh_data_on_failure,
        container_removed,
        instance_dir.as_ref(),
    ) {
        match docker::remove_host_dir_blocking(instance_dir) {
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
        let reason = if instance_dir.is_none() {
            "the state dir could not be resolved"
        } else if remove_fresh_data_on_failure {
            "the container could not be removed"
        } else {
            "the directory contained data before this start attempt"
        };
        match &instance_dir {
            Some(instance_dir) => diagnostics.push(format!(
                "retained ClickHouse data '{}' because {reason}",
                instance_dir.display()
            )),
            None => diagnostics.push(format!("retained ClickHouse data because {reason}")),
        }
        false
    };

    if container_removed && instance_removed {
        match server::try_remove_server_info_locked(&info.name, metadata_lock) {
            Ok(()) => return primary,
            Err(error) => diagnostics.push(format!("failed to remove metadata: {error}")),
        }
    } else if container_removed {
        // The container is gone but the data directory pre-existed this
        // attempt and is retained. Metadata pointing at the deleted
        // container must not survive — the next `start` would hit
        // "container is gone" and its guided exit, `remove`, would delete
        // this data. The fresh path only runs without prior metadata, so
        // removing this attempt's own file is always safe; the call
        // tolerates absence (the save may never have happened).
        match server::try_remove_server_info_locked(&info.name, metadata_lock) {
            Ok(()) => diagnostics.push(
                "no recovery metadata kept; the next start re-creates the container \
                 and reuses the retained data"
                    .to_string(),
            ),
            Err(error) => diagnostics.push(format!("failed to remove recovery metadata: {error}")),
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

/// A binary-era ClickHouse entry (no container): dctl can no longer manage
/// the process itself, but stop/remove must still be able to clear its
/// metadata and data directory (the promise in server.rs's is_alive docs).
fn legacy_ch_info_locked(
    user_name: &str,
    lock: &server::MetadataLock,
) -> Result<Option<ServerInfo>> {
    Ok(server::load_info_locked(user_name, lock)?
        .filter(|info| info.engine == Engine::Clickhouse && info.container_id.is_none()))
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

    // The identity comes from the container's effective env — the same source
    // dotenv and client read — so a resume reports what is actually
    // provisioned, not the defaults. The password is intentionally not
    // reprinted (it is recoverable via `dotenv`).
    let (user, _password, database) = read_ch_env(docker, &container_id).await?;

    docker::start_existing(docker, &container_id).await?;

    // Refresh both host ports from the container's own bindings: a recovered
    // instance (or metadata predating a port change) can carry 0 or stale
    // values, and every later client/dotenv call trusts them.
    let inspected = docker::inspect_container(docker, &container_id)
        .await
        .ok()
        .flatten();
    // A HostPort of "0" means "not bound here"; keep the prior value
    // rather than probing an invalid port.
    let http_port = docker::host_port_from_inspect(inspected.as_ref(), "8123/tcp")
        .filter(|port| *port != 0)
        .unwrap_or(prior.http_port);
    let native_key = if prior.tls == Some(true) {
        "9440/tcp"
    } else {
        "9000/tcp"
    };
    let tcp_port = docker::host_port_from_inspect(inspected.as_ref(), native_key)
        .filter(|port| *port != 0)
        .unwrap_or(prior.tcp_port);
    if http_port == 0 {
        return Err(Error::ClickhouseUsage(format!(
            "cannot determine the HTTP port of container '{container_id}'; \
             run `dctl local server remove {display_name}` and start fresh"
        )));
    }
    let info = ServerInfo {
        started_at: server::now_timestamp(),
        http_port,
        tcp_port,
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

    if let Err(failure) = wait_for_ch_ready(
        docker,
        &container_id,
        info.http_port,
        prior.tls == Some(true),
        wait_timeout,
    )
    .await
    {
        let error =
            ch_readiness_error(docker, &container_id, &display_name, wait_timeout, failure).await;
        let _ = docker::stop_container(docker, &container_id).await;
        return Err(error);
    }
    // The credentials warning probes the plaintext HTTP port; on the
    // certificate face that port is https and no password exists, so the
    // probe would report a rejection that cannot happen (review F2).
    if prior.tls != Some(true) {
        warn_if_credentials_rejected(info.http_port, &user, &_password, &database).await;
    }

    let out = output::ClickhouseStartOutput {
        name: display_name.clone(),
        container_id,
        image: info.version.clone(),
        http_port: info.http_port,
        native_port: info.tcp_port,
        user,
        password: String::new(),
        database,
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

/// `/ping` answers without authentication, so a server whose data directory
/// predates this start may be "ready" with credentials the printed env does
/// not match (the image only applies CLICKHOUSE_PASSWORD on first init).
/// One authenticated SELECT 1 turns that silent mismatch into a warning.
async fn warn_if_credentials_rejected(http_port: u16, user: &str, password: &str, database: &str) {
    if let Err(error) = http_query(
        "127.0.0.1",
        http_port,
        Some(user),
        Some(password),
        Some(database),
        "SELECT 1",
    )
    .await
    {
        eprintln!(
            "Warning: the server is up but rejected the printed credentials ({error}). \
             An existing data directory keeps the password from its first initialization."
        );
    }
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
    tls: bool,
    timeout: Duration,
) -> std::result::Result<(), ReadinessFailure> {
    if tls {
        // The certificate face probes in-container over https with the
        // uploaded pair (S005): the published port stays out of the loop, so
        // this works even where daemon port publishing is unreachable over
        // host loopback.
        let mut last_probe_error = None;
        match tokio::time::timeout(
            timeout,
            poll_ch_tls_readiness(docker, container_id, &mut last_probe_error),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ReadinessFailure::TimedOut { last_probe_error }),
        }
    } else {
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
}

/// Certificate-face readiness loop: container state plus the in-container
/// https probe with the uploaded client pair.
async fn poll_ch_tls_readiness(
    docker: &bollard::Docker,
    container_id: &str,
    last_probe_error: &mut Option<String>,
) -> std::result::Result<(), ReadinessFailure> {
    loop {
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
            Ok(ReadinessState::Running) => {
                match docker::clickhouse_tls_is_ready(docker, container_id, DEFAULT_USER).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => *last_probe_error = Some(error.to_string()),
                }
            }
            Ok(ReadinessState::Pending) => {}
            Err(error) => return Err(ReadinessFailure::Probe(error)),
        }
        tokio::time::sleep(READINESS_POLL_INTERVAL).await;
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

fn resolve_port(
    explicit: Option<u16>,
    kind: PortKind,
    bind_face: Option<std::net::IpAddr>,
) -> Result<u16> {
    // A port already failing the face probes fails without a daemon
    // round-trip; only a socket-free candidate needs the Docker-published
    // set (REQ-0015).
    if let Some(port) = explicit
        && !port_free_with(port, bind_face, &[])
    {
        return Err(Error::PortInUse { kind, port });
    }
    resolve_port_with(
        explicit,
        kind,
        bind_face,
        &crate::local::docker::published_host_ports_blocking(),
    )
}

/// The decision core of [`resolve_port`] with the Docker-published port set
/// injected, so tests pin the NAT-blindness fix without a daemon (REQ-0015).
fn resolve_port_with(
    explicit: Option<u16>,
    kind: PortKind,
    bind_face: Option<std::net::IpAddr>,
    published: &[u16],
) -> Result<u16> {
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
        Some(port) if port_free_with(port, bind_face, published) => return Ok(port),
        Some(port) => return Err(Error::PortInUse { kind, port }),
        None => {}
    }
    if port_free_with(default_port, bind_face, published) {
        return Ok(default_port);
    }
    for p in (default_port + 1)..=(default_port + 100) {
        if port_free_with(p, bind_face, published) {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(kind))
}

/// A port is usable when it is free on loopback and on the extra `--bind`
/// face. A v4 wildcard (`0.0.0.0`) replaces the loopback probe with a
/// wildcard probe (it covers loopback and conflicts with any existing
/// binding on the port); a v6 wildcard (`::`) probes like a specific face
/// because Docker publishes it v6-only. Docker-published ports are rejected
/// outright: iptables-NAT publishing leaves no host listener, so the socket
/// probes cannot see them (REQ-0015).
fn port_free_with(port: u16, bind_face: Option<std::net::IpAddr>, published: &[u16]) -> bool {
    if published.contains(&port) {
        return false;
    }
    match bind_face {
        // A v4 wildcard covers loopback, so it replaces it. A v6 wildcard
        // does not (Docker publishes [::] v6-only), so it probes like a
        // specific face.
        Some(face) if face.is_ipv4() && face.is_unspecified() => {
            std::net::TcpListener::bind((face, port)).is_ok()
        }
        Some(face) => {
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
                && std::net::TcpListener::bind((face, port)).is_ok()
        }
        None => std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
    }
}

fn resolve_native_port_excluding(
    http_port: u16,
    bind_face: Option<std::net::IpAddr>,
) -> Result<u16> {
    let published = crate::local::docker::published_host_ports_blocking();
    let picked = resolve_port_with(None, PortKind::Clickhouse, bind_face, &published)?;
    if picked != http_port {
        return Ok(picked);
    }
    for p in (DEFAULT_CH_NATIVE_PORT + 1)..=(DEFAULT_CH_NATIVE_PORT + 101) {
        if p != http_port && port_free_with(p, bind_face, &published) {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(PortKind::Clickhouse))
}

/// Mirror of [`resolve_native_port_excluding`]: the auto-picked HTTP port
/// must steer around an explicitly requested native port, or the two would
/// collide only at Docker bind time after a full pull/create cycle.
fn resolve_http_port_excluding(
    native_port: u16,
    bind_face: Option<std::net::IpAddr>,
) -> Result<u16> {
    let published = crate::local::docker::published_host_ports_blocking();
    let picked = resolve_port_with(None, PortKind::Http, bind_face, &published)?;
    if picked != native_port {
        return Ok(picked);
    }
    for p in (DEFAULT_CH_HTTP_PORT + 1)..=(DEFAULT_CH_HTTP_PORT + 101) {
        if p != native_port && port_free_with(p, bind_face, &published) {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(PortKind::Http))
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
    // Half-supplied credentials would silently send no auth header at all
    // (or none where one is expected) — fail the usage instead.
    if user.is_some() != password.is_some() {
        return Err(Error::ClickhouseUsage(
            "--user and --password go together; pass both or neither".into(),
        ));
    }
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
        let mut body = response.text().await.unwrap_or_default();
        if body.contains("Multi-statements are not allowed") {
            body.push_str(
                "\n(The HTTP interface executes one statement per request; \
                 split the file or query, or use interactive mode.)",
            );
        }
        return Err(Error::ClickhouseHttp {
            status: status.as_u16(),
            body,
        });
    }
    Ok(response.text().await.unwrap_or_default())
}

/// Execute a SQL query over the certificate face (REQ-0017): https with
/// the dctl client identity, trusting only the dctl CA, authenticating the
/// default user through the certificate-auth header pair (S005). The URL
/// carries no user/password parameters - mixing them with certificate auth
/// is rejected by the server.
pub(crate) async fn https_query(
    host: &str,
    port: u16,
    user: &str,
    database: Option<&str>,
    sql: &str,
) -> Result<String> {
    use reqwest::tls::{Certificate, Identity};

    let (cert_path, key_path) = crate::local::ca::client_cert(user)?;
    let key_pem = std::fs::read_to_string(&key_path)
        .map_err(|e| Error::ClickhouseUsage(format!("could not read the client key: {e}")))?;
    let cert_pem = std::fs::read_to_string(&cert_path).map_err(|e| {
        Error::ClickhouseUsage(format!("could not read the client certificate: {e}"))
    })?;
    let ca_pem = std::fs::read(crate::local::ca::ensure_ca()?).map_err(|e| {
        Error::ClickhouseUsage(format!("could not read the dctl CA certificate: {e}"))
    })?;

    let identity_pem = format!("{key_pem}{cert_pem}");
    let identity = Identity::from_pem(identity_pem.as_bytes()).map_err(|e| {
        Error::ClickhouseUsage(format!("could not load the client certificate: {e}"))
    })?;
    // tls_certs_only (not the deprecated add_root_certificate): the private
    // CA must fully replace the platform verifier, or rustls reports
    // UnknownIssuer for the self-signed chain (S005).
    let client = crate::http::client_builder()
        .tls_certs_only([Certificate::from_pem(&ca_pem)
            .map_err(|e| Error::ClickhouseUsage(format!("could not load the CA: {e}")))?])
        .identity(identity)
        .timeout(Duration::from_secs(120))
        .no_proxy()
        .build()?;

    let url = format!("https://{host}:{port}/");
    let mut request = client
        .post(&url)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("X-ClickHouse-SSL-Certificate-Auth", "on")
        .header("X-ClickHouse-User", user);
    if let Some(database) = database {
        request = request.query(&[("database", database)]);
    }
    let response = request.body(sql.to_string()).send().await.map_err(|e| {
        eprintln!("certificate query to {host}:{port} failed: {e}");
        Error::ClickhouseUsage(format!(
            "the query to {host}:{port} over the client certificate failed; the \
                 transport's message is on stderr"
        ))
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::ClickhouseHttp {
            status: status.as_u16(),
            body,
        });
    }
    Ok(response.text().await.unwrap_or_default())
}

/// Read the provisioned credentials from the container's effective env.
/// Fails loudly when the container cannot be inspected: fabricating the
/// defaults instead would silently connect (and write .env files) with a
/// wrong identity.
async fn read_ch_env(docker: &bollard::Docker, id: &str) -> Result<(String, String, String)> {
    let inspect = docker.inspect_container(id, None).await.map_err(|error| {
        Error::DockerError(format!(
            "could not read the credentials of container '{id}': {error}"
        ))
    })?;
    let env: Vec<String> = inspect.config.and_then(|c| c.env).unwrap_or_default();
    let get = |k: &str| -> Option<String> {
        env.iter()
            .find_map(|e| e.strip_prefix(&format!("{k}=")).map(str::to_string))
    };
    Ok((
        get("CLICKHOUSE_USER").unwrap_or_else(|| DEFAULT_USER.into()),
        get("CLICKHOUSE_PASSWORD").unwrap_or_default(),
        get("CLICKHOUSE_DB").unwrap_or_else(|| DEFAULT_DATABASE.into()),
    ))
}

/// Direct-mode (`--host/--port`) credentials; managed mode reads them from
/// the container env instead.
pub(crate) struct DirectCreds {
    pub user: Option<String>,
    pub password: Option<String>,
}

/// `dctl local client` flags, verbatim from clap.
pub(crate) struct ClientCmd {
    pub name: Option<String>,
    pub version: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub query: Option<String>,
    pub queries_file: Option<String>,
    pub database: Option<String>,
    pub direct: DirectCreds,
}

pub(crate) async fn client(cmd: ClientCmd) -> Result<()> {
    let ClientCmd {
        name,
        version,
        host,
        port,
        query,
        queries_file,
        database,
        direct,
    } = cmd;
    // Direct mode: HTTP to the given host/port, with optional credentials
    // (dctl-managed instances always require them).
    if host.is_some() || port.is_some() {
        let h = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let p = port.unwrap_or(DEFAULT_CH_HTTP_PORT);
        let sql = read_query_input(query.as_deref(), queries_file.as_deref())?;
        let result = http_query(
            &h,
            p,
            direct.user.as_deref(),
            direct.password.as_deref(),
            database.as_deref(),
            &sql,
        )
        .await?;
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
    let (user, password, db) = read_ch_env(&docker, container_id).await?;
    let tls = info.tls == Some(true);

    if query.is_some() || queries_file.is_some() {
        let sql = read_query_input(query.as_deref(), queries_file.as_deref())?;
        let result = if tls {
            // Certificate face (REQ-0017): https with the client identity
            // and the certificate-auth header pair (S005); no password
            // exists to send.
            https_query(
                "127.0.0.1",
                info.http_port,
                DEFAULT_USER,
                database.as_deref().or(Some(db.as_str())),
                &sql,
            )
            .await?
        } else {
            http_query(
                "127.0.0.1",
                info.http_port,
                Some(&user),
                Some(&password),
                database.as_deref().or(Some(db.as_str())),
                &sql,
            )
            .await?
        };
        print!("{result}");
        return Ok(());
    }

    // Interactive: docker exec clickhouse-client with TTY. The certificate
    // face rides the secure port with the in-container client config; the
    // password face keeps the credential flags.
    let cli_args: Vec<String> = if tls {
        vec![
            "--secure".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            "9440".into(),
            "--user".into(),
            DEFAULT_USER.to_string(),
            "--config".into(),
            "/etc/clickhouse-server/dctl/cli.xml".into(),
            "--database".into(),
            database.clone().unwrap_or(db),
            "--interactive".into(),
        ]
    } else {
        vec![
            "--user".into(),
            user.clone(),
            "--password".into(),
            password.clone(),
            "--database".into(),
            database.clone().unwrap_or(db),
            "--interactive".into(),
        ]
    };
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
    let target = match resolve_ch_target_locked(name, version, &metadata_lock) {
        Ok(target) => Some(target),
        Err(Error::ServerNotFound(_)) => None,
        Err(error) => return Err(error),
    };
    let already_stopped = match &target {
        Some(target) if server::is_server_running_locked(&target.name, &metadata_lock)? => {
            if !json {
                println!(
                    "Stopping ClickHouse {} ({})...",
                    ch_user_name_from_key(&target.name),
                    target.version
                );
            }
            server::kill_server_locked(&target.name, &metadata_lock)?;
            false
        }
        // Stopped Docker instance: idempotent success (README promises it).
        Some(_) => true,
        None => {
            // Binary-era entry: the process itself is beyond dctl's reach;
            // zero the stale pid so the entry reads cleanly as stopped.
            let legacy = legacy_ch_info_locked(name, &metadata_lock)?
                .ok_or_else(|| Error::ServerNotFound(name.to_string()))?;
            server::mark_server_stopped_locked(name, legacy.pid, &metadata_lock)?;
            if !json {
                println!(
                    "Note: '{}' is a binary-era instance without a container; \
                     stop the old process manually if it still runs.",
                    name
                );
            }
            true
        }
    };
    let out = output::ServerStopOutput {
        name: target
            .as_ref()
            .map(|info| ch_user_name_from_key(&info.name).to_string())
            .unwrap_or_else(|| name.to_string()),
        already_stopped,
        selection: None,
    };
    output::print_output(&out, json);
    Ok(())
}

pub(crate) fn remove(name: &str, version: Option<&str>, json: bool) -> Result<()> {
    server::validate_server_name(name)?;
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;

    let target = match resolve_ch_target_locked(name, version, &metadata_lock) {
        Ok(target) => target,
        Err(Error::ServerNotFound(_)) => {
            // Binary-era entry: clear the metadata and data directory; the
            // old process (if any) has to be stopped by hand.
            let legacy = legacy_ch_info_locked(name, &metadata_lock)?
                .ok_or_else(|| Error::ServerNotFound(name.to_string()))?;
            let legacy_dir = server::servers_dir_join(&legacy.name)?;
            // Directory first, metadata second — the main path below uses the
            // same order so a failure leaves a retryable state (metadata still
            // points at the leftover directory) instead of an unreachable one.
            docker::remove_host_dir_blocking(&legacy_dir)?;
            server::try_remove_server_info_locked(&legacy.name, &metadata_lock)?;
            if !json {
                println!(
                    "Note: removed binary-era metadata for '{}' (no container); \
                     stop the old process manually if it still runs.",
                    name
                );
            }
            let out = output::ServerRemoveOutput {
                name: name.to_string(),
                selection: None,
            };
            output::print_output(&out, json);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let key = target.name.clone();
    if server::is_server_running_locked(&key, &metadata_lock)? {
        let tag = tag_from_stored_version(&target.version);
        return Err(Error::ServerRunningCannotRemove {
            name: name.to_string(),
            command: format!("dctl local server stop {name} --version {tag}"),
        });
    }

    if let Some(cid) = target.container_id.as_deref() {
        // Fail closed: deleting the data directory while the container still
        // exists would leave a container bind-mounting a deleted path, and
        // reporting success on top of that misleads. The metadata and data
        // stay intact, so the remove is retryable once Docker cooperates.
        docker::stop_and_remove_blocking(cid).map_err(|error| {
            Error::Cleanup(format!(
                "could not remove container '{cid}'; nothing was deleted — \
                 retry `dctl local server remove {name}` once Docker is reachable: {error}"
            ))
        })?;
    }

    let ch_dir = server::servers_dir_join(&key)?;
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
    ))?;

    // The face decides the shape (ADR-0011): certificate instances get the
    // TLS material paths and no password; password instances keep the
    // credential line (S005).
    let vars: Vec<(&str, String)> = if info.tls == Some(true) {
        let (cert_path, key_path) = crate::local::ca::client_cert(DEFAULT_USER)?;
        vec![
            ("CLICKHOUSE_HOST", "127.0.0.1".to_string()),
            ("CLICKHOUSE_HTTP_PORT", info.http_port.to_string()),
            ("CLICKHOUSE_PORT", info.tcp_port.to_string()),
            ("CLICKHOUSE_USER", DEFAULT_USER.to_string()),
            ("CLICKHOUSE_DATABASE", database),
            ("CLICKHOUSE_TLS", "true".to_string()),
            (
                "CLICKHOUSE_CA_CERT",
                crate::local::ca::ensure_ca()?.display().to_string(),
            ),
            ("CLICKHOUSE_CLIENT_CERT", cert_path.display().to_string()),
            ("CLICKHOUSE_CLIENT_KEY", key_path.display().to_string()),
        ]
    } else {
        vec![
            ("CLICKHOUSE_HOST", "127.0.0.1".to_string()),
            ("CLICKHOUSE_HTTP_PORT", info.http_port.to_string()),
            ("CLICKHOUSE_PORT", info.tcp_port.to_string()),
            ("CLICKHOUSE_USER", user),
            ("CLICKHOUSE_PASSWORD", password),
            ("CLICKHOUSE_DATABASE", database),
        ]
    };

    let filename = if use_local { ".env.local" } else { ".env" };
    let path = std::path::Path::new(filename);

    let content = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        // All CLICKHOUSE_* keys share the managed prefix, so update_dotenv
        // replaces in place; the cross-face keys need exact-key stripping
        // so a switch cannot leave both shapes behind.
        let strip_password = info.tls == Some(true);
        let existing = existing
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                let bare = trimmed
                    .strip_prefix("export")
                    .map(str::trim_start)
                    .unwrap_or(trimmed);
                let key = bare.split('=').next().unwrap_or("").trim_end();
                let face_key = matches!(
                    key,
                    "CLICKHOUSE_TLS"
                        | "CLICKHOUSE_CA_CERT"
                        | "CLICKHOUSE_CLIENT_CERT"
                        | "CLICKHOUSE_CLIENT_KEY"
                ) || (strip_password && key == "CLICKHOUSE_PASSWORD");
                !face_key
            })
            .chain(std::iter::once(""))
            .collect::<Vec<_>>()
            .join("\n");
        crate::local::update_dotenv(&existing, "CLICKHOUSE_", &vars)
    } else {
        vars.iter()
            .map(|(k, v)| crate::local::format_dotenv_line("", k, v))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    crate::local::write_dotenv_file(path, &content)?;

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

async fn read_ch_env_for_dotenv(container_id: &str) -> Result<(String, String, String)> {
    if container_id.is_empty() {
        return Ok((DEFAULT_USER.into(), String::new(), DEFAULT_DATABASE.into()));
    }
    // A failed connect must not degrade into fabricated credentials: the
    // dotenv file would carry them with a success exit code.
    let docker = docker::connect().await?;
    read_ch_env(&docker, container_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_arg_accepts_and_normalizes_addresses() {
        assert_eq!(
            parse_ch_bind_arg("192.168.88.175").unwrap(),
            "192.168.88.175"
        );
        assert_eq!(parse_ch_bind_arg("0.0.0.0").unwrap(), "0.0.0.0");
        assert_eq!(parse_ch_bind_arg("::0001").unwrap(), "::1");
    }

    #[test]
    fn bind_arg_rejects_non_addresses() {
        for value in ["lan-linux", "999.999.1.1", ""] {
            assert!(
                parse_ch_bind_arg(value).is_err(),
                "expected `{value}` to be rejected"
            );
        }
    }

    #[test]
    fn resolve_port_skips_docker_published_ports() {
        // REQ-0015: published ports have no host listener under iptables
        // NAT, so the injected set alone must reject them.
        assert!(!port_free_with(
            DEFAULT_CH_HTTP_PORT,
            None,
            &[DEFAULT_CH_HTTP_PORT]
        ));
        let picked = resolve_port_with(None, PortKind::Http, None, &[DEFAULT_CH_HTTP_PORT])
            .expect("picker steers around the published port");
        assert_ne!(picked, DEFAULT_CH_HTTP_PORT);
    }

    #[test]
    fn resolve_port_without_bind_face_probes_loopback_only() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(
            resolve_port(Some(port), PortKind::Http, None).unwrap(),
            port
        );
    }

    #[test]
    fn resolve_port_wildcard_face_rejects_port_held_on_loopback() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = resolve_port(
            Some(port),
            PortKind::Http,
            Some(std::net::IpAddr::from([0, 0, 0, 0])),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::PortInUse { port: error_port, .. } if error_port == port),
            "{error}"
        );
    }

    #[test]
    fn resolve_port_specific_face_rejects_port_held_on_wildcard() {
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = resolve_port(
            Some(port),
            PortKind::Http,
            Some("127.0.0.1".parse().unwrap()),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::PortInUse { port: error_port, .. } if error_port == port),
            "{error}"
        );
    }

    #[test]
    fn resolve_port_v6_wildcard_face_still_probes_loopback() {
        // Docker publishes [::] v6-only, so a v6 wildcard face keeps the
        // loopback companion probe: a port held on loopback stays unusable.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error =
            resolve_port(Some(port), PortKind::Http, Some("::".parse().unwrap())).unwrap_err();
        assert!(
            matches!(error, Error::PortInUse { port: error_port, .. } if error_port == port),
            "{error}"
        );
    }

    #[test]
    fn validate_start_options_rejects_bind_face_missing_from_host() {
        // TEST-NET-1 is never a local address, so the pre-flight surfaces
        // the real cause instead of folding it into "port already in use".
        let error = validate_start_options(
            None,
            None,
            None,
            None,
            Some("192.0.2.1"),
            None,
            None,
            Vec::new(),
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::ClickhouseUsage(ref message) if message.contains("not present on this host")),
            "{error}"
        );
    }
}
