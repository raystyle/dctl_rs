//! Handlers for `dctl local falkordb ...` subcommands.
//!
//! All Docker work goes through `local::docker`. State is reused from
//! `local::server` — FalkorDB entries land in the same metadata directory and
//! show up alongside ClickHouse and Postgres in `local server list`.
//!
//! FalkorDB is a Redis-module graph database. The managed container always
//! runs with `--requirepass` (a generated 24-char password unless `--password`
//! is given), so the readiness probe and the client both authenticate; the
//! container's env (REDIS_ARGS) is the credential source of truth across
//! resumes, exactly like Postgres' POSTGRES_* env.

use crate::error::{Error, PortKind, Result, StartupKind};
use crate::local::cli::FalkorCommands;
use crate::local::docker::{self, FalkorRunOpts};
use crate::local::output;
use crate::local::server::{self, Engine, ServerInfo};
use rand::distr::{Alphanumeric, SampleString};
use std::collections::HashSet;
use std::future::Future;
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const DEFAULT_FK_PORT: u16 = 6379;
const DEFAULT_FK_BROWSER_PORT: u16 = 3000;
/// Default image tag when `--version` is not given. FalkorDB publishes only
/// full X.Y.Z tags (plus `latest`), so the default is pinned to a concrete
/// version; upgrading is an explicit `--version` / `local install` action.
pub const DEFAULT_FK_TAG: &str = "4.20.6";
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const READINESS_LOG_LINES: usize = 50;
const READINESS_LOG_BYTES: usize = 16 * 1024;
const READINESS_LOG_TIMEOUT: Duration = Duration::from_secs(2);

/// Full image reference for a validated tag: `falkordb/falkordb:vX.Y.Z`
/// (the `v` is part of the published tag) or `falkordb/falkordb:latest`.
pub(crate) fn fk_image_ref(tag: &str) -> String {
    if tag == "latest" {
        "falkordb/falkordb:latest".to_string()
    } else {
        format!("falkordb/falkordb:v{tag}")
    }
}

/// Accept FalkorDB image tags: `latest` or a full `X.Y.Z` version. The image
/// publishes no major-only or minor-only tags, so anything else cannot exist.
pub(crate) fn validate_fk_tag(tag: &str) -> Result<()> {
    let valid = tag == "latest" || {
        tag.len() <= 128
            && tag.is_ascii()
            && tag.split('.').count() == 3
            && tag
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    };

    if !valid {
        return Err(Error::FalkorUsage(format!(
            "invalid or unsupported FalkorDB version '{tag}'. Use a full X.Y.Z version (for \
             example: 4.20.6) or latest.",
        )));
    }
    Ok(())
}

pub(crate) fn parse_fk_tag_arg(tag: &str) -> std::result::Result<String, String> {
    validate_fk_tag(tag)
        .map(|()| tag.to_string())
        .map_err(|error| error.to_string())
}

pub(crate) fn parse_fk_port_arg(value: &str) -> std::result::Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| format!("invalid port '{value}': expected an integer from 1 to 65535"))?;
    if port == 0 {
        return Err("--port 0 is not allowed; pick a specific port or omit the flag".into());
    }
    Ok(port)
}

fn validate_fk_env_assignment(assignment: &str) -> std::result::Result<(&str, &str), String> {
    let Some((key, value)) = assignment.split_once('=') else {
        return Err(format!(
            "invalid environment variable '{assignment}': expected KEY=VALUE"
        ));
    };
    let mut chars = key.chars();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(format!(
            "invalid environment variable key '{key}': use letters, digits, and underscores, and do not start with a digit"
        ));
    }
    match key {
        // Authentication is managed through --password; the whole REDIS_ARGS
        // argv is ours, so a user-provided copy could silently drop it.
        "REDIS_ARGS" => {
            Err("REDIS_ARGS is managed by dctl; use --password instead of --env".into())
        }
        _ => Ok((key, value)),
    }
}

pub(crate) fn parse_fk_env_arg(assignment: &str) -> std::result::Result<String, String> {
    validate_fk_env_assignment(assignment).map(|_| assignment.to_string())
}

pub(crate) fn validate_fk_start_env_args(extra_env: &[String]) -> std::result::Result<(), String> {
    let mut seen = HashSet::new();
    for assignment in extra_env {
        let (key, _) = validate_fk_env_assignment(assignment)?;
        if !seen.insert(key) {
            return Err(format!(
                "environment variable '{key}' was provided more than once; pass each --env key only once"
            ));
        }
    }
    Ok(())
}

/// The stored `ServerInfo.version` form for a tag: `falkordb:v4.20.6` or
/// `falkordb:latest` (latest carries no v — it is not part of that tag).
pub(crate) fn stored_version_form(tag: &str) -> String {
    if tag == "latest" {
        "falkordb:latest".to_string()
    } else {
        format!("falkordb:v{tag}")
    }
}

/// The tag stored in `ServerInfo.version` back to its bare form:
/// `falkordb:v4.20.6` or `falkordb:latest` to `4.20.6` / `latest`.
fn tag_from_stored_version(stored: &str) -> &str {
    stored
        .strip_prefix("falkordb:")
        .map(|tag| tag.strip_prefix('v').unwrap_or(tag))
        .unwrap_or(stored)
}

struct StartPreflight {
    host_port: Option<u16>,
    browser_port: Option<u16>,
    extra_env: Vec<String>,
}

fn validate_start_options(
    name: Option<&str>,
    version: Option<&str>,
    port: Option<u16>,
    browser_port: Option<u16>,
    password: Option<&str>,
    extra_env: Vec<String>,
) -> Result<StartPreflight> {
    if let Some(name) = name {
        server::validate_server_name(name)?;
    }
    if let Some(version) = version {
        validate_fk_tag(version)?;
    }
    validate_fk_start_env_args(&extra_env).map_err(Error::FalkorUsage)?;
    if let Some(password) = password {
        validate_fk_password(password)?;
    }

    let host_port = port
        .map(|port| resolve_port(Some(port), PortKind::Falkordb))
        .transpose()?;
    let browser_port = browser_port
        .map(|port| resolve_port(Some(port), PortKind::FalkordbBrowser))
        .transpose()?;
    // G4: two explicit ports must differ; Docker would only fail at start,
    // after pulling and creating, and roll back a fresh instance.
    if let (Some(host), Some(browser)) = (host_port, browser_port)
        && host == browser
    {
        return Err(Error::FalkorUsage(format!(
            "--port and --browser-port cannot both be {host}; pick distinct ports or omit them to auto-select"
        )));
    }
    Ok(StartPreflight {
        host_port,
        browser_port,
        extra_env,
    })
}

/// The password is spliced into the REDIS_ARGS argv text and read back by
/// whitespace tokenization, so whitespace or quote characters inside it
/// would silently change the effective credential (or break server argv).
pub(crate) fn validate_fk_password(password: &str) -> Result<()> {
    if password.is_empty()
        || password.chars().any(char::is_whitespace)
        || password.contains('"')
        || password.contains('\'')
        || password.contains('\\')
    {
        return Err(Error::FalkorUsage(
            "invalid --password: must be non-empty and contain no whitespace, quotes, or backslashes".into(),
        ));
    }
    Ok(())
}

pub async fn run(cmd: FalkorCommands, json: bool) -> Result<()> {
    match cmd {
        FalkorCommands::Start {
            name,
            name_flag,
            version,
            port,
            browser_port,
            password,
            env,
            wait_timeout,
        } => {
            start(
                name.or(name_flag),
                version,
                port,
                browser_port,
                password,
                env,
                Duration::from_secs(wait_timeout.into()),
                json,
            )
            .await
        }
        FalkorCommands::Stop {
            name,
            name_flag,
            version,
        } => {
            stop(
                name.or(name_flag).as_deref().unwrap_or("default"),
                version.as_deref(),
                json,
            )
            .await
        }
        FalkorCommands::StopAll => stop_all(json).await,
        FalkorCommands::Remove {
            name,
            name_flag,
            version,
        } => remove(
            name.or(name_flag).as_deref().unwrap_or("default"),
            version.as_deref(),
            json,
        ),
        FalkorCommands::Client {
            name,
            name_flag,
            version,
            host,
            port,
            query,
            args,
        } => client(name.or(name_flag), version, host, port, query, args).await,
        FalkorCommands::Dotenv {
            name,
            name_flag,
            version,
            local,
        } => dotenv(
            name.or(name_flag).as_deref(),
            version.as_deref(),
            local,
            json,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn start(
    name: Option<String>,
    version: Option<String>,
    port: Option<u16>,
    browser_port: Option<u16>,
    password: Option<String>,
    extra_env: Vec<String>,
    wait_timeout: Duration,
    json: bool,
) -> Result<()> {
    let has_extra_env = !extra_env.is_empty();
    let preflight = validate_start_options(
        name.as_deref(),
        version.as_deref(),
        port,
        browser_port,
        password.as_deref(),
        extra_env,
    )?;
    let explicit_host_port = preflight.host_port;
    let explicit_browser_port = preflight.browser_port;
    let extra_env = preflight.extra_env;

    let docker = docker::connect().await?;
    let project_cwd = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    loop {
        // Resolve an optimistic target, then release the project-wide lock
        // before potentially slow fresh-image inspection and pulling.
        let metadata_lock = server::lock_metadata()?;
        server::recover_current_project_servers_locked(&metadata_lock)?;
        let user_name = match name.as_deref() {
            Some(name) => name.to_string(),
            None => default_fk_name_locked(&metadata_lock)?,
        };
        let tag = resolve_fk_start_version_locked(&user_name, version.as_deref(), &metadata_lock)?;
        let key = server::fk_instance_key(&user_name, &tag);
        let prior = server::load_info_locked(&key, &metadata_lock)?;
        drop(metadata_lock);

        if prior.is_none() {
            let image_ref = fk_image_ref(&tag);
            if !docker::image_exists(&docker, &image_ref).await? {
                docker::pull_image(&docker, &image_ref, json).await?;
            }
            docker::ensure_name_free(
                &docker,
                &docker::fk_container_name(&user_name, &tag),
                docker::ENGINE_FALKORDB,
                &project_cwd,
            )
            .await?;
        }

        // The optimistic target may have changed while Docker work was in
        // progress. Re-resolve it before any state-determining mutation.
        let metadata_lock = server::lock_metadata()?;
        let current_tag =
            resolve_fk_start_version_locked(&user_name, version.as_deref(), &metadata_lock)?;
        let current_key = server::fk_instance_key(&user_name, &current_tag);
        let current = server::load_info_locked(&current_key, &metadata_lock)?;
        if current_tag != tag || current_key != key || current != prior {
            drop(metadata_lock);
            continue;
        }
        crate::init::ensure_runtime_gitignore()?;

        // Resume path: an instance for this exact (name, tag) already exists.
        if let Some(prior) = prior {
            let cid = prior.container_id.as_deref().unwrap_or("");
            let inspected = if cid.is_empty() {
                None
            } else {
                docker::inspect_container(&docker, cid).await?
            };
            let Some(inspected) = inspected else {
                return Err(Error::FalkorUsage(format!(
                    "server '{}' (falkordb:{}) has metadata but the container is gone. \
                     Run `dctl local falkordb remove {}` to clear the data dir \
                     and start fresh.",
                    user_name, tag, user_name
                )));
            };
            if docker::inspected_container_running(&inspected)? {
                return Err(Error::ServerAlreadyRunning(user_name));
            }
            if !json
                && (port.is_some() || browser_port.is_some() || password.is_some() || has_extra_env)
            {
                eprintln!(
                    "Note: falkordb:{tag} '{}' already exists; resuming with stored settings. \
                     Run `local falkordb remove {}` to start over.",
                    user_name, user_name
                );
            }
            return resume_existing(&docker, prior, wait_timeout, json, metadata_lock).await;
        }

        // Fresh create.
        let host_port = match explicit_host_port {
            Some(port) => port,
            None => resolve_port(None, PortKind::Falkordb)?,
        };
        let browser_port = match explicit_browser_port {
            Some(port) => port,
            None => resolve_browser_port_excluding(host_port)?,
        };

        let instance_dir = server::servers_dir_join(&key);
        let remove_fresh_data_on_failure = fresh_instance_dir_is_disposable(&instance_dir);
        server::ensure_fk_data_dir(&user_name, &tag)?;
        let data_dir = server::fk_data_dir(&user_name, &tag);

        let password = password.unwrap_or_else(generate_password);

        let opts = FalkorRunOpts {
            user_name: &user_name,
            version: &tag,
            image_ref: &fk_image_ref(&tag),
            host_port,
            browser_port,
            data_dir: &data_dir,
            project_cwd: &project_cwd,
            password: &password,
            extra_env,
        };

        let container_id = docker::create_falkordb(&docker, opts).await?;

        let info = ServerInfo {
            name: key.clone(),
            pid: 0,
            version: stored_version_form(&tag),
            // The browser port rides the http_port slot for FalkorDB; the
            // graph protocol port is tcp_port, matching the list columns.
            http_port: browser_port,
            tcp_port: host_port,
            started_at: server::now_timestamp(),
            cwd: project_cwd.clone(),
            engine: Engine::Falkordb,
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
            wait_for_falkor_ready(&docker, &container_id, &password, wait_timeout).await
        {
            let primary =
                falkor_readiness_error(&docker, &container_id, &user_name, wait_timeout, failure)
                    .await;
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

        let out = output::FalkorStartOutput {
            name: user_name,
            container_id,
            image: info.version,
            port: host_port,
            browser_port,
            password,
        };
        output::print_output(&out, json);
        return Ok(());
    }
}

/// A fresh attempt owns an absent or empty instance directory, including one
/// containing only an empty `data/` from an earlier pre-container step.
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
        || !entry.file_type().is_ok_and(|file_type| file_type.is_dir())
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
    let metadata_path = server::servers_dir_join(&format!("{}.json", info.name));
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
                    "failed to remove fresh FalkorDB data '{}': path still exists",
                    instance_dir.display()
                ));
                false
            }
            Err(error) => {
                diagnostics.push(format!(
                    "failed to remove fresh FalkorDB data '{}': {error}",
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
            "retained FalkorDB data '{}' because {reason}",
            instance_dir.display()
        ));
        false
    };

    if container_removed && instance_removed {
        match server::try_remove_server_info_locked(&info.name, metadata_lock) {
            Ok(()) => return primary,
            Err(error) => diagnostics.push(format!(
                "failed to remove metadata '{}': {error}",
                metadata_path.display()
            )),
        }
    } else {
        match server::save_server_info_locked(info, metadata_lock) {
            Ok(()) => diagnostics.push(format!(
                "recovery metadata retained at '{}'; run `dctl local falkordb remove {}` to clean up",
                metadata_path.display(),
                user_name_from_key(&info.name)
            )),
            Err(error) => diagnostics.push(format!(
                "failed to preserve recovery metadata '{}': {error}",
                metadata_path.display()
            )),
        }
    }

    Error::FalkorStartupRollback {
        primary: Box::new(primary),
        cleanup: diagnostics.join("; "),
    }
}

/// Default user-facing name when NAME is omitted: `"default"` if no
/// falkordb "default" is running, otherwise a random adjective-noun.
fn default_fk_name_locked(metadata_lock: &server::MetadataLock) -> Result<String> {
    default_fk_name_locked_with(metadata_lock, docker::is_container_running_blocking)
}

fn default_fk_name_locked_with(
    metadata_lock: &server::MetadataLock,
    is_container_running: impl Fn(&str) -> Result<bool>,
) -> Result<String> {
    for info in server::find_fk_instances_locked("default", metadata_lock)? {
        if let Some(id) = info.container_id.as_deref()
            && is_container_running(id)?
        {
            return server::generate_random_name_locked(metadata_lock);
        }
    }
    Ok("default".into())
}

/// Resolve the image tag for a user-facing name. The caller invokes this both
/// before Docker image preparation and under the final metadata lock.
fn resolve_fk_start_version_locked(
    user_name: &str,
    version: Option<&str>,
    metadata_lock: &server::MetadataLock,
) -> Result<String> {
    if let Some(version) = version {
        return Ok(version.to_string());
    }

    let existing = server::find_fk_instances_locked(user_name, metadata_lock)?;
    match existing.as_slice() {
        [] => Ok(DEFAULT_FK_TAG.to_string()),
        [info] => Ok(tag_from_stored_version(&info.version).to_string()),
        _ => {
            let versions: Vec<&str> = existing.iter().map(|info| info.version.as_str()).collect();
            Err(Error::FalkorUsage(format!(
                "multiple FalkorDB instances named '{}' ({}); pass --version to select one",
                user_name,
                versions.join(", ")
            )))
        }
    }
}

/// Resolve `[NAME] [--version <V>]` to a single FalkorDB instance on disk.
/// If `version` is given, target the (name, tag) pair directly. Otherwise:
/// 0 instances → ServerNotFound; 1 → use it; >1 → ask for `--version`.
fn resolve_fk_target_locked(
    user_name: &str,
    version: Option<&str>,
    metadata_lock: &server::MetadataLock,
) -> Result<server::ServerInfo> {
    if let Some(v) = version {
        validate_fk_tag(v)?;
        let key = server::fk_instance_key(user_name, v);
        return server::load_info_locked(&key, metadata_lock)?
            .filter(|i| i.engine == Engine::Falkordb)
            .ok_or_else(|| Error::ServerNotFound(format!("{user_name} (falkordb:{v})")));
    }
    let instances = server::find_fk_instances_locked(user_name, metadata_lock)?;
    match instances.len() {
        0 => Err(Error::ServerNotFound(user_name.to_string())),
        1 => Ok(instances.into_iter().next().unwrap()),
        _ => {
            let versions: Vec<String> = instances.iter().map(|i| i.version.clone()).collect();
            Err(Error::FalkorUsage(format!(
                "multiple FalkorDB instances named '{}' ({}); pass --version to select one",
                user_name,
                versions.join(", ")
            )))
        }
    }
}

/// Resume an existing stopped FalkorDB container. Credentials come from the
/// container's persisted env (the source of truth — REDIS_ARGS was set for
/// them) and the metadata is refreshed.
async fn resume_existing(
    docker: &bollard::Docker,
    prior: ServerInfo,
    wait_timeout: Duration,
    json: bool,
    metadata_lock: server::MetadataLock,
) -> Result<()> {
    let container_id = prior.container_id.clone().expect("checked by caller");
    let display_name = user_name_from_key(&prior.name).to_string();

    // The readiness probe authenticates, so the password must be read before
    // the container starts (inspect works on stopped containers too).
    let password = read_fk_password(docker, &container_id).await;

    docker::start_existing(docker, &container_id).await?;

    // Refresh both host ports from the container's own bindings: a recovered
    // instance (or one whose metadata predates a port change) can carry 0 or
    // stale values, and dotenv would write them out verbatim.
    let inspected = docker::inspect_container(docker, &container_id)
        .await
        .ok()
        .flatten();
    let info = ServerInfo {
        started_at: server::now_timestamp(),
        tcp_port: docker::host_port_from_inspect(inspected.as_ref(), "6379/tcp")
            .unwrap_or(prior.tcp_port),
        http_port: docker::host_port_from_inspect(inspected.as_ref(), "3000/tcp")
            .unwrap_or(prior.http_port),
        ..prior
    };
    if let Err(primary) = server::save_server_info_locked(&info, &metadata_lock) {
        drop(metadata_lock);
        return match docker::stop_container(docker, &container_id).await {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(Error::FalkorStartupRollback {
                primary: Box::new(primary),
                cleanup: format!(
                    "could not stop resumed container '{container_id}' after metadata failure: {cleanup}"
                ),
            }),
        };
    }
    drop(metadata_lock);

    if let Err(failure) =
        wait_for_falkor_ready(docker, &container_id, &password, wait_timeout).await
    {
        let error =
            falkor_readiness_error(docker, &container_id, &display_name, wait_timeout, failure)
                .await;
        let _ = docker::stop_container(docker, &container_id).await;
        return Err(error);
    }

    let out = output::FalkorStartOutput {
        name: display_name,
        container_id,
        image: info.version,
        port: info.tcp_port,
        browser_port: info.http_port,
        password,
    };
    output::print_output(&out, json);
    Ok(())
}

/// Extract the user-facing name from a disk key. `dev-fk4.20.6` and
/// `dev-fklatest` → `dev`; anything that doesn't match the suffix shape
/// passes through unchanged.
pub(crate) fn user_name_from_key(key: &str) -> &str {
    if let Some(idx) = key.rfind("-fk")
        && server::is_fk_version_suffix(&key[idx + 3..])
    {
        return &key[..idx];
    }
    key
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ContainerReadinessState {
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

trait ReadinessProbe {
    async fn container_state(&mut self) -> Result<ContainerReadinessState>;
    async fn falkor_is_ready(&mut self) -> Result<bool>;
}

struct DockerReadinessProbe<'a> {
    docker: &'a bollard::Docker,
    container_id: &'a str,
    password: &'a str,
}

impl ReadinessProbe for DockerReadinessProbe<'_> {
    async fn container_state(&mut self) -> Result<ContainerReadinessState> {
        let state = docker::container_state(self.docker, self.container_id).await?;
        if state.running {
            Ok(ContainerReadinessState::Running)
        } else if state.exited {
            Ok(ContainerReadinessState::Exited {
                status: state.status,
                exit_code: state.exit_code,
                oom_killed: state.oom_killed,
            })
        } else {
            Ok(ContainerReadinessState::Pending)
        }
    }

    async fn falkor_is_ready(&mut self) -> Result<bool> {
        docker::falkor_is_ready(self.docker, self.container_id, self.password).await
    }
}

async fn poll_falkor_readiness<P, S, SFut>(
    probe: &mut P,
    max_checks: usize,
    mut sleep: S,
    last_probe_error: &mut Option<String>,
) -> std::result::Result<(), ReadinessFailure>
where
    P: ReadinessProbe,
    S: FnMut() -> SFut,
    SFut: Future<Output = ()>,
{
    for check in 0..max_checks {
        match probe
            .container_state()
            .await
            .map_err(ReadinessFailure::Probe)?
        {
            ContainerReadinessState::Exited {
                status,
                exit_code,
                oom_killed,
            } => {
                return Err(ReadinessFailure::Exited {
                    status,
                    exit_code,
                    oom_killed,
                });
            }
            ContainerReadinessState::Running => match probe.falkor_is_ready().await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    *last_probe_error = Some(error.to_string());
                    if let Ok(ContainerReadinessState::Exited {
                        status,
                        exit_code,
                        oom_killed,
                    }) = probe.container_state().await
                    {
                        return Err(ReadinessFailure::Exited {
                            status,
                            exit_code,
                            oom_killed,
                        });
                    }
                }
            },
            ContainerReadinessState::Pending => {}
        }

        if check + 1 < max_checks {
            sleep().await;
        }
    }
    Err(ReadinessFailure::TimedOut {
        last_probe_error: last_probe_error.take(),
    })
}

async fn wait_for_falkor_ready_with_probe<P: ReadinessProbe>(
    probe: &mut P,
    timeout: Duration,
) -> std::result::Result<(), ReadinessFailure> {
    let mut last_probe_error = None;
    match tokio::time::timeout(
        timeout,
        poll_falkor_readiness(
            probe,
            usize::MAX,
            || tokio::time::sleep(READINESS_POLL_INTERVAL),
            &mut last_probe_error,
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ReadinessFailure::TimedOut { last_probe_error }),
    }
}

async fn wait_for_falkor_ready(
    docker: &bollard::Docker,
    container_id: &str,
    password: &str,
    timeout: Duration,
) -> std::result::Result<(), ReadinessFailure> {
    let mut probe = DockerReadinessProbe {
        docker,
        container_id,
        password,
    };
    wait_for_falkor_ready_with_probe(&mut probe, timeout).await
}

async fn falkor_readiness_error(
    docker: &bollard::Docker,
    container_id: &str,
    display_name: &str,
    timeout: Duration,
    failure: ReadinessFailure,
) -> Error {
    let logs = collect_falkor_readiness_logs(
        container_id,
        READINESS_LOG_TIMEOUT,
        docker::container_logs_tail(
            docker,
            container_id,
            READINESS_LOG_LINES,
            READINESS_LOG_BYTES,
        ),
    )
    .await;
    format_falkor_readiness_error(display_name, timeout, failure, &logs)
}

async fn collect_falkor_readiness_logs<F>(container_id: &str, timeout: Duration, logs: F) -> String
where
    F: Future<Output = Result<String>>,
{
    match tokio::time::timeout(timeout, logs).await {
        Ok(Ok(logs)) if logs.trim().is_empty() || logs == "(no container logs)" => format!(
            "No container logs were available. Run `docker logs {container_id}` for current diagnostics."
        ),
        Ok(Ok(logs)) => logs,
        Ok(Err(error)) => format!(
            "Could not read container logs ({error}). Run `docker logs {container_id}` for diagnostics."
        ),
        Err(_) => format!(
            "Timed out reading container logs. Run `docker logs {container_id}` for diagnostics."
        ),
    }
}

fn format_falkor_readiness_error(
    display_name: &str,
    timeout: Duration,
    failure: ReadinessFailure,
    logs: &str,
) -> Error {
    let diagnostics = |summary: &str| {
        format!(
            "{summary}\n--- last {READINESS_LOG_LINES} container log lines (maximum {READINESS_LOG_BYTES} bytes) ---\n{logs}"
        )
    };
    match failure {
        ReadinessFailure::Exited {
            status,
            exit_code,
            oom_killed,
        } => {
            let exit_code = exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let oom = if oom_killed { "; out of memory" } else { "" };
            let summary = format!(
                "FalkorDB container '{display_name}' exited before the server became ready \
                 (status: {status}, exit code: {exit_code}{oom})."
            );
            Error::StartupExit {
                kind: StartupKind::Falkordb,
                name: display_name.to_string(),
                details: format!("Docker error: {}", diagnostics(&summary)),
            }
        }
        ReadinessFailure::Probe(error) => {
            let summary = format!(
                "Could not check FalkorDB readiness in container '{display_name}': {error}."
            );
            Error::DockerError(diagnostics(&summary))
        }
        ReadinessFailure::TimedOut { last_probe_error } => {
            let probe_context = last_probe_error
                .map(|error| format!(" Last readiness probe error: {error}."))
                .unwrap_or_default();
            let summary = format!(
                "FalkorDB in container '{display_name}' did not become ready within {} seconds.{probe_context}",
                timeout.as_secs()
            );
            Error::StartupTimeout {
                kind: StartupKind::Falkordb,
                name: display_name.to_string(),
                seconds: timeout.as_secs(),
                details: format!("Docker error: {}", diagnostics(&summary)),
            }
        }
    }
}

/// Resolve one host port: an explicit port must be free (else PortInUse),
/// otherwise start from `default_port` and auto-pick within +100.
fn resolve_port(explicit: Option<u16>, kind: PortKind) -> Result<u16> {
    let default_port = match kind {
        PortKind::Http => DEFAULT_FK_BROWSER_PORT,
        _ => DEFAULT_FK_PORT,
    };
    match explicit {
        Some(0) => {
            return Err(Error::FalkorUsage(
                "--port 0 is not allowed; pick a specific port or omit the flag".into(),
            ));
        }
        Some(port) if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() => {
            return Ok(port);
        }
        Some(port) => {
            return Err(Error::PortInUse { kind, port });
        }
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

/// Auto-pick the browser port, never colliding with the already-resolved
/// protocol port (each free-port probe releases its socket before Docker
/// binds both, so an unguarded pick can take the same port twice).
fn resolve_browser_port_excluding(host_port: u16) -> Result<u16> {
    let picked = resolve_port(None, PortKind::FalkordbBrowser)?;
    if picked != host_port {
        return Ok(picked);
    }
    for p in (DEFAULT_FK_BROWSER_PORT + 1)..=(DEFAULT_FK_BROWSER_PORT + 101) {
        if p != host_port && std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(PortKind::FalkordbBrowser))
}

fn generate_password() -> String {
    // 24 alphanumeric chars. The container's REDIS_ARGS env is the source of
    // truth; `dotenv` re-reads it from there.
    Alphanumeric.sample_string(&mut rand::rng(), 24)
}

/// Parse the requirepass value out of a REDIS_ARGS env assignment.
fn password_from_redis_args(value: &str) -> String {
    let mut password = String::new();
    let mut take_next = false;
    for token in value.split_whitespace() {
        if take_next {
            password = token.to_string();
            break;
        }
        if let Some(stripped) = token.strip_prefix("--requirepass=") {
            password = stripped.to_string();
            break;
        }
        if token == "--requirepass" {
            take_next = true;
        }
    }
    password
}

/// Read the provisioned password from the container's effective env so a
/// resume keeps using the credential the data was created with.
async fn read_fk_password(docker: &bollard::Docker, id: &str) -> String {
    let inspect = docker.inspect_container(id, None).await.ok();
    inspect
        .and_then(|c| c.config)
        .and_then(|c| c.env)
        .unwrap_or_default()
        .iter()
        .find_map(|e| e.strip_prefix("REDIS_ARGS=").map(password_from_redis_args))
        .unwrap_or_default()
}

async fn stop(name: &str, version: Option<&str>, json: bool) -> Result<()> {
    server::validate_server_name(name)?;
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let target = resolve_fk_target_locked(name, version, &metadata_lock)?;
    if !json {
        let display = format!("{} ({})", user_name_from_key(&target.name), target.version);
        println!("Stopping FalkorDB {}...", display);
    }
    server::kill_server_locked(&target.name, &metadata_lock)?;
    let out = output::ServerStopOutput {
        name: user_name_from_key(&target.name).to_string(),
        already_stopped: false,
        selection: None,
    };
    output::print_output(&out, json);
    Ok(())
}

async fn stop_all(json: bool) -> Result<()> {
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let servers: Vec<_> = server::list_running_servers_locked(&metadata_lock)?
        .into_iter()
        .filter(|s| s.engine == Engine::Falkordb)
        .collect();
    if !json && servers.is_empty() {
        println!("No running FalkorDB servers");
        return Ok(());
    }

    let out = super::stop_servers(&servers, json, |name| {
        server::kill_server_locked(name, &metadata_lock)
    });
    if json {
        output::print_output(&out, json);
    } else {
        println!("Done");
    }
    Ok(())
}

fn remove(name: &str, version: Option<&str>, json: bool) -> Result<()> {
    server::validate_server_name(name)?;
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;

    let target = resolve_fk_target_locked(name, version, &metadata_lock)?;
    let key = target.name.clone();
    if server::is_server_running_locked(&key, &metadata_lock)? {
        let tag = tag_from_stored_version(&target.version);
        return Err(Error::ServerRunningCannotRemove {
            name: name.to_string(),
            command: format!("dctl local falkordb stop {name} --version {tag}"),
        });
    }

    if let Some(cid) = target.container_id.as_deref() {
        let _ = docker::stop_and_remove_blocking(cid);
    }

    // FalkorDB data dir lives at .dctl/servers/<key>/data/. Remove the
    // <key>/ wrapper so the (name, version) pair leaves no on-disk state.
    // Files inside were written by the container user, so removal goes
    // through the privileged-container fallback when a plain rm fails.
    let fk_dir = server::servers_dir_join(&key);
    docker::remove_host_dir_blocking(&fk_dir)?;
    server::try_remove_server_info_locked(&key, &metadata_lock)?;
    let out = output::ServerRemoveOutput {
        name: name.to_string(),
        selection: None,
    };
    output::print_output(&out, json);
    Ok(())
}

/// Split a redis command line into argv tokens, honoring double quotes,
/// single quotes, and backslash escapes inside double quotes. Unquoted
/// whitespace separates tokens.
pub(crate) fn split_redis_command(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    // Tracks "inside a quoted or in-progress token" so an explicitly quoted
    // empty argument ("" or '') survives instead of being dropped.
    let mut in_token = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' if !in_token => {}
            ' ' | '\t' => {
                tokens.push(std::mem::take(&mut current));
                in_token = false;
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') | None => break,
                        Some('\\') => {
                            if let Some(escaped) = chars.next() {
                                current.push(escaped);
                            }
                        }
                        Some(c) => current.push(c),
                    }
                }
            }
            '\'' => {
                in_token = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    current.push(c);
                }
            }
            _ => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if !current.is_empty() || in_token {
        tokens.push(current);
    }
    tokens
}

async fn client(
    name: Option<String>,
    version: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    query: Option<String>,
    extra_args: Vec<String>,
) -> Result<()> {
    if host.is_some() || port.is_some() {
        // Direct connect — no managed lookup, no stored credentials; bring
        // your own auth through the passthrough args (e.g. `-a <password>`).
        if !host_has_redis_cli() {
            return Err(Error::FalkorUsage(
                "could not execute redis-cli: not found on PATH (install the Redis client tools)"
                    .to_string(),
            ));
        }
        let h = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let p = port.unwrap_or(DEFAULT_FK_PORT);
        let mut cli_args: Vec<String> = vec!["-h".into(), h, "-p".into(), p.to_string()];
        if let Some(q) = query {
            cli_args.extend(split_redis_command(&q));
        }
        cli_args.extend(extra_args);
        return exec_host_redis_cli(&cli_args, None);
    }

    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let server_name = name.as_deref().unwrap_or("default");
    let info = resolve_fk_target_locked(server_name, version.as_deref(), &metadata_lock)?;
    if !server::is_server_running_locked(&info.name, &metadata_lock)? {
        return Err(Error::ServerNotRunning(server_name.to_string()));
    }
    drop(metadata_lock);

    let docker = docker::connect().await?;
    let container_id = info
        .container_id
        .as_deref()
        .ok_or_else(|| Error::DockerError("missing container_id".into()))?;
    let password = read_fk_password(&docker, container_id).await;

    // Prefer host redis-cli; fall back to docker exec.
    if host_has_redis_cli() {
        let mut cli_args: Vec<String> = vec![
            "-h".into(),
            "127.0.0.1".into(),
            "-p".into(),
            info.tcp_port.to_string(),
        ];
        if let Some(q) = query {
            cli_args.extend(split_redis_command(&q));
        }
        cli_args.extend(extra_args);
        return exec_host_redis_cli(&cli_args, Some(&password));
    }

    let interactive = query.is_none()
        && extra_args.is_empty()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal();
    // The exec fallback authenticates through the exec's REDISCLI_AUTH env —
    // argv would leak the password into the container's process listing,
    // same trade the host path already makes.
    let mut cli_args: Vec<String> = vec!["--no-auth-warning".into()];
    if let Some(q) = query {
        cli_args.extend(split_redis_command(&q));
    }
    cli_args.extend(extra_args);

    if !interactive {
        // Without an explicit wrapper input, a non-terminal stdin is still
        // redis input (piped commands); Docker must attach it and see EOF.
        let input: Option<Box<dyn std::io::Read + Send>> =
            if interactive || (!std::io::stdin().is_terminal() && cli_args.len() == 1) {
                None
            } else {
                Some(Box::new(std::io::stdin()))
            };
        docker::exec_redis_cli_one_shot(&docker, container_id, &cli_args, &password, input).await
    } else {
        docker::exec_redis_cli_in_container(&docker, container_id, &cli_args, &password).await
    }
}

fn host_has_redis_cli() -> bool {
    Command::new("redis-cli")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// exec() into host redis-cli. The password travels through REDISCLI_AUTH
/// rather than `-a`, so it never shows in a process listing.
fn exec_host_redis_cli(cli_args: &[String], password: Option<&str>) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new("redis-cli");
    cmd.args(cli_args);
    if let Some(password) = password {
        cmd.env("REDISCLI_AUTH", password);
    }
    // `exec()` replaces the process image on success, so `main`'s telemetry
    // tail never runs for this invocation; record the censored handoff
    // attempt now (#320, #471). The caller probes for `redis-cli` first, so
    // only a race can fail below this line; `redis-cli` then inherits this
    // process's stdio, process group, session and controlling TTY unchanged,
    // which is why the handoff stays an `exec()`.
    #[cfg(feature = "telemetry")]
    crate::telemetry::finalize_before_exec();
    let err = cmd.exec();
    Err(Error::DockerError(format!(
        "could not execute redis-cli: {err}"
    )))
}

fn dotenv(name: Option<&str>, version: Option<&str>, use_local: bool, json: bool) -> Result<()> {
    let metadata_lock = server::lock_metadata()?;
    server::recover_current_project_servers_locked(&metadata_lock)?;
    let server_name = name.unwrap_or("default");
    let info = resolve_fk_target_locked(server_name, version, &metadata_lock)?;
    if !server::is_server_running_locked(&info.name, &metadata_lock)? {
        return Err(Error::ServerNotRunning(server_name.to_string()));
    }
    drop(metadata_lock);

    // Read the password from the container env so we always emit accurate
    // credentials, like the Postgres dotenv does.
    let password = docker::block_on(read_fk_password_for_dotenv(
        info.container_id.as_deref().unwrap_or_default(),
    ));

    // A recovered instance can carry an unknown (0) browser port; writing
    // http://127.0.0.1:0 into a user's .env would be a lie. Clobber any
    // stale value with an explicit empty one instead.
    let browser_url = if info.http_port == 0 {
        String::new()
    } else {
        format!("http://127.0.0.1:{}", info.http_port)
    };
    let vars: Vec<(&str, String)> = vec![
        ("FALKORDB_HOST", "127.0.0.1".to_string()),
        ("FALKORDB_PORT", info.tcp_port.to_string()),
        ("FALKORDB_PASSWORD", password),
        ("FALKORDB_BROWSER_URL", browser_url),
    ];

    let filename = if use_local { ".env.local" } else { ".env" };
    let path = std::path::Path::new(filename);

    let content = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        crate::local::update_dotenv(&existing, "FALKORDB_", &vars)
    } else {
        vars.iter()
            .map(|(k, v)| crate::local::format_dotenv_line("", k, v))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    std::fs::write(path, &content)?;

    let out = output::FalkorDotenvOutput {
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

async fn read_fk_password_for_dotenv(container_id: &str) -> String {
    if container_id.is_empty() {
        return String::new();
    }
    match docker::connect().await {
        Ok(d) => read_fk_password(&d, container_id).await,
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeReadinessProbe {
        states: VecDeque<ContainerReadinessState>,
        ready: VecDeque<Result<bool>>,
        readiness_checks: usize,
    }

    impl FakeReadinessProbe {
        fn new(states: Vec<ContainerReadinessState>, ready: Vec<Result<bool>>) -> Self {
            Self {
                states: states.into(),
                ready: ready.into(),
                readiness_checks: 0,
            }
        }
    }

    impl ReadinessProbe for FakeReadinessProbe {
        async fn container_state(&mut self) -> Result<ContainerReadinessState> {
            Ok(self
                .states
                .pop_front()
                .unwrap_or(ContainerReadinessState::Running))
        }

        async fn falkor_is_ready(&mut self) -> Result<bool> {
            self.readiness_checks += 1;
            match self.ready.pop_front() {
                Some(result) => result,
                None => Ok(true),
            }
        }
    }

    async fn never_sleep() {}

    #[tokio::test]
    async fn ready_once_running_is_reported() {
        let mut probe = FakeReadinessProbe::new(
            vec![
                ContainerReadinessState::Pending,
                ContainerReadinessState::Running,
            ],
            vec![Ok(true)],
        );
        let mut last_error = None;
        poll_falkor_readiness(&mut probe, 10, never_sleep, &mut last_error)
            .await
            .expect("ready after running");
        assert_eq!(probe.readiness_checks, 1);
    }

    #[tokio::test]
    async fn not_ready_polls_until_ready() {
        let mut probe = FakeReadinessProbe::new(
            vec![
                ContainerReadinessState::Running,
                ContainerReadinessState::Running,
                ContainerReadinessState::Running,
            ],
            vec![Ok(false), Ok(false), Ok(true)],
        );
        let mut last_error = None;
        poll_falkor_readiness(&mut probe, 10, never_sleep, &mut last_error)
            .await
            .expect("eventually ready");
        assert_eq!(probe.readiness_checks, 3);
    }

    #[tokio::test]
    async fn exited_container_fails_immediately() {
        let mut probe = FakeReadinessProbe::new(
            vec![ContainerReadinessState::Exited {
                status: "exited".into(),
                exit_code: Some(1),
                oom_killed: false,
            }],
            vec![],
        );
        let mut last_error = None;
        let failure = poll_falkor_readiness(&mut probe, 10, never_sleep, &mut last_error)
            .await
            .expect_err("exited fails");
        assert!(matches!(failure, ReadinessFailure::Exited { .. }));
        assert_eq!(probe.readiness_checks, 0);
    }

    #[tokio::test]
    async fn timeout_reports_last_probe_error() {
        let mut probe = FakeReadinessProbe::new(
            vec![ContainerReadinessState::Running],
            vec![Err(Error::DockerError("probe exploded".into())), Ok(false)],
        );
        let mut last_error = None;
        let failure = poll_falkor_readiness(&mut probe, 2, never_sleep, &mut last_error)
            .await
            .expect_err("never ready");
        match failure {
            ReadinessFailure::TimedOut { last_probe_error } => {
                assert_eq!(
                    last_probe_error.as_deref(),
                    Some("Docker error: probe exploded")
                );
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }

    #[test]
    fn falkor_tags_accept_full_versions_and_latest() {
        for tag in ["4.20.6", "4.18.10", "latest"] {
            validate_fk_tag(tag).unwrap_or_else(|error| panic!("{tag} should pass: {error}"));
        }
        for tag in [
            "4",
            "4.20",
            "v4.20.6",
            "4.20.6-alpine",
            "",
            "4.20.x",
            "latest-alpine",
        ] {
            assert!(validate_fk_tag(tag).is_err(), "{tag} should be rejected");
        }
    }

    #[test]
    fn image_refs_carry_the_v_prefix_only_for_versions() {
        assert_eq!(fk_image_ref("4.20.6"), "falkordb/falkordb:v4.20.6");
        assert_eq!(fk_image_ref("latest"), "falkordb/falkordb:latest");
    }

    #[test]
    fn stored_versions_round_trip_to_tags() {
        assert_eq!(tag_from_stored_version("falkordb:v4.20.6"), "4.20.6");
        assert_eq!(tag_from_stored_version("falkordb:latest"), "latest");
        assert_eq!(tag_from_stored_version("other"), "other");
    }

    #[test]
    fn falkor_env_rejects_managed_redis_args() {
        assert!(validate_fk_env_assignment("REDIS_ARGS=--requirepass x").is_err());
        assert!(validate_fk_env_assignment("FALKORDB_ARGS=CACHE_SIZE 10").is_ok());
        assert!(validate_fk_env_assignment("1BAD=x").is_err());
        assert!(validate_fk_env_assignment("NOEQUALS").is_err());
    }

    #[test]
    fn falkor_env_keys_must_be_unique() {
        let error = validate_fk_start_env_args(&[
            "FALKORDB_ARGS=A 1".to_string(),
            "FALKORDB_ARGS=B 2".to_string(),
        ])
        .expect_err("duplicate key");
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn redis_args_password_extraction_covers_both_spellings() {
        assert_eq!(
            password_from_redis_args("--requirepass secret123"),
            "secret123"
        );
        assert_eq!(
            password_from_redis_args("--requirepass=other456"),
            "other456"
        );
        assert_eq!(password_from_redis_args("--appendonly no"), "");
    }

    #[test]
    fn redis_command_splitting_honors_quotes() {
        assert_eq!(
            split_redis_command("GRAPH.QUERY g \"MATCH (n) RETURN n\""),
            vec!["GRAPH.QUERY", "g", "MATCH (n) RETURN n"]
        );
        assert_eq!(split_redis_command("ping"), vec!["ping"]);
        assert_eq!(
            split_redis_command("  GRAPH.RO_QUERY   'social'   \"it's one\"  "),
            vec!["GRAPH.RO_QUERY", "social", "it's one"]
        );
        assert_eq!(
            split_redis_command(r#"SET k "escaped \" quote""#),
            vec!["SET", "k", "escaped \" quote"]
        );
    }

    #[test]
    fn user_names_strip_the_fk_suffix_including_latest() {
        assert_eq!(user_name_from_key("dev-fklatest"), "dev");
        assert_eq!(user_name_from_key("dev-fk4.20.6"), "dev");
        assert_eq!(user_name_from_key("prod-fk2"), "prod-fk2");
        assert_eq!(user_name_from_key("plain"), "plain");
    }

    #[test]
    fn passwords_reject_whitespace_and_quotes() {
        validate_fk_password("plain-secret").unwrap();
        for bad in ["", "a b", "a\tb", "quote\"x", "back\\slash"] {
            assert!(
                validate_fk_password(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn redis_command_splitting_keeps_quoted_empty_arguments() {
        assert_eq!(split_redis_command("SET k \"\""), vec!["SET", "k", ""]);
        assert_eq!(split_redis_command("ping"), vec!["ping"]);
        // Unquoted trailing spaces still produce no phantom token.
        assert_eq!(split_redis_command("ping   "), vec!["ping"]);
    }

    #[test]
    fn tag_from_stored_version_maps_latest() {
        assert_eq!(tag_from_stored_version("falkordb:latest"), "latest");
        assert_eq!(tag_from_stored_version("falkordb:v4.20.6"), "4.20.6");
    }

    #[test]
    fn user_names_strip_the_fk_suffix() {}

    #[test]
    fn instance_keys_and_metadata_names_agree() {
        assert_eq!(server::fk_instance_key("dev", "4.20.6"), "dev-fk4.20.6");
        assert_eq!(
            docker::fk_container_name("dev", "4.20.6"),
            "dctl-fk-dev-4.20.6"
        );
    }
}
