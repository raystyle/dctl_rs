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
use std::time::Duration;

const DEFAULT_FK_PORT: u16 = 6379;
/// The ACL user certificate faces authenticate as (the CN of the client
/// certificate); FalkorDB instances have no user axis, so it is always
/// the redis default user.
pub(crate) const FK_ACL_USER: &str = "default";
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
            auth,
        } => {
            start(
                name.or(name_flag),
                version,
                port,
                browser_port,
                password,
                env,
                auth,
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
            graph,
            password,
            args,
        } => {
            client(
                name.or(name_flag),
                version,
                host,
                port,
                query,
                graph,
                password,
                args,
            )
            .await
        }
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
    auth: crate::local::cli::AuthFaceArg,
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
                docker::pull_image(&docker, &image_ref, json, None).await?;
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
                && (port.is_some()
                    || browser_port.is_some()
                    || password.is_some()
                    || has_extra_env
                    || auth == crate::local::cli::AuthFaceArg::Password)
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
            None => match explicit_browser_port {
                Some(browser) => resolve_host_port_excluding(browser)?,
                None => resolve_port(None, PortKind::Falkordb)?,
            },
        };
        let browser_port = match explicit_browser_port {
            Some(port) => port,
            None => resolve_browser_port_excluding(host_port)?,
        };

        let instance_dir = server::servers_dir_join(&key)?;
        let remove_fresh_data_on_failure = fresh_instance_dir_is_disposable(&instance_dir);
        server::ensure_fk_data_dir(&user_name, &tag)?;
        let data_dir = server::fk_data_dir(&user_name, &tag)?;

        let tls = auth == crate::local::cli::AuthFaceArg::Cert;
        // The certificate face provisions no password; the start output
        // prints none rather than a decorative one (contract across the
        // three engines).
        let password = if tls {
            String::new()
        } else {
            password.unwrap_or_else(generate_password)
        };

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
            tls,
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
            tls: Some(tls),
        };
        // Issuance and upload live inside the rollback-covered block (the
        // material rides in the container layer, so removing the container
        // cleans it). The client pair is uploaded too: the in-container
        // redis-cli legs (readiness probe and the interactive REPL) ride
        // it; the certificate-face programmatic path connects from the
        // host with the CA material instead (fred, REQ-0017).
        let startup_result = async {
            if tls {
                let cname = docker::fk_container_name(&user_name, &tag);
                let (server_cert, server_key) = crate::local::ca::issue_server_cert(&cname)?;
                let (client_cert, client_key) = crate::local::ca::issue(FK_ACL_USER)?;
                let ca_cert = std::fs::read_to_string(crate::local::ca::ensure_ca()?)
                    .map_err(|e| Error::Postgres(format!("CA cert read: {e}")))?;
                docker::upload_falkordb_tls_material(
                    &docker,
                    &container_id,
                    &server_cert,
                    &server_key,
                    &client_cert,
                    &client_key,
                    &ca_cert,
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
            wait_for_falkor_ready_with_face(&docker, &container_id, &password, tls, wait_timeout)
                .await
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

/// Display helper for rollback diagnostics: the resolved path when the
/// bucket address works, otherwise the instance key so the message stays
/// actionable without a path.
fn metadata_display(resolved: &Result<std::path::PathBuf>, key: &str) -> String {
    match resolved {
        Ok(path) => path.display().to_string(),
        Err(_) => key.to_string(),
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
    // not panic the cleanup, it only skips the data-directory steps and
    // degrades path diagnostics to the instance key.
    let (instance_dir, metadata_path) = (
        server::servers_dir_join(&info.name),
        server::servers_dir_join(&format!("{}.json", info.name)),
    );
    if let Err(error) = &instance_dir {
        diagnostics.push(format!(
            "could not resolve the state dir for '{}': {error}",
            info.name
        ));
    }

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
        instance_dir.as_ref().ok(),
    ) {
        match docker::remove_host_dir_blocking(instance_dir) {
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
        let reason = if instance_dir.is_err() {
            "the state dir could not be resolved"
        } else if remove_fresh_data_on_failure {
            "the container could not be removed"
        } else {
            "the directory contained data before this start attempt"
        };
        diagnostics.push(format!(
            "retained FalkorDB data '{}' because {reason}",
            metadata_display(&instance_dir, &info.name)
        ));
        false
    };

    if container_removed && instance_removed {
        match server::try_remove_server_info_locked(&info.name, metadata_lock) {
            Ok(()) => return primary,
            Err(error) => diagnostics.push(format!(
                "failed to remove metadata '{}': {error}",
                metadata_display(&metadata_path, &info.name)
            )),
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
            Err(error) => diagnostics.push(format!(
                "failed to remove recovery metadata '{}': {error}",
                metadata_display(&metadata_path, &info.name)
            )),
        }
    } else {
        match server::save_server_info_locked(info, metadata_lock) {
            Ok(()) => diagnostics.push(format!(
                "recovery metadata retained at '{}'; run `dctl local falkordb remove {}` to clean up",
                metadata_display(&metadata_path, &info.name),
                user_name_from_key(&info.name)
            )),
            Err(error) => diagnostics.push(format!(
                "failed to preserve recovery metadata '{}': {error}",
                metadata_display(&metadata_path, &info.name)
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
    let password = read_fk_password(docker, &container_id).await?;

    docker::start_existing(docker, &container_id).await?;

    // Refresh both host ports from the container's own bindings: a recovered
    // instance (or one whose metadata predates a port change) can carry 0 or
    // stale values, and dotenv would write them out verbatim.
    let inspected = docker::inspect_container(docker, &container_id)
        .await
        .ok()
        .flatten();
    // A HostPort of "0" means "not bound here"; keep the prior value rather
    // than probing an invalid port, and fail like the pg/ch resumes when the
    // protocol port stays unknowable (G1: parity across engines).
    let tcp_port = docker::host_port_from_inspect(inspected.as_ref(), "6379/tcp")
        .filter(|port| *port != 0)
        .unwrap_or(prior.tcp_port);
    let http_port = docker::host_port_from_inspect(inspected.as_ref(), "3000/tcp")
        .filter(|port| *port != 0)
        .unwrap_or(prior.http_port);
    if tcp_port == 0 {
        return Err(Error::FalkorUsage(format!(
            "cannot determine the TCP port of container '{container_id}'; \
             run `dctl local falkordb remove {display_name}` and start fresh"
        )));
    }
    let info = ServerInfo {
        started_at: server::now_timestamp(),
        tcp_port,
        http_port,
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

    if let Err(failure) = wait_for_falkor_ready_with_face(
        docker,
        &container_id,
        &password,
        prior.tls == Some(true),
        wait_timeout,
    )
    .await
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
    tls: bool,
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
        docker::falkor_is_ready_with_face(self.docker, self.container_id, self.password, self.tls)
            .await
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

async fn wait_for_falkor_ready_with_face(
    docker: &bollard::Docker,
    container_id: &str,
    password: &str,
    tls: bool,
    timeout: Duration,
) -> std::result::Result<(), ReadinessFailure> {
    let mut probe = DockerReadinessProbe {
        docker,
        container_id,
        password,
        tls,
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
    // A port already bound locally fails without a daemon round-trip; only
    // a socket-free candidate needs the Docker-published set (REQ-0015).
    if let Some(port) = explicit
        && std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
    {
        return Err(Error::PortInUse { kind, port });
    }
    resolve_port_with(
        explicit,
        kind,
        &crate::local::docker::published_host_ports_blocking(),
    )
}

/// The decision core of [`resolve_port`] with the Docker-published port set
/// injected, so tests pin the NAT-blindness fix without a daemon (REQ-0015):
/// a candidate needs a free socket AND no container publishing it.
fn resolve_port_with(explicit: Option<u16>, kind: PortKind, published: &[u16]) -> Result<u16> {
    let default_port = match kind {
        PortKind::FalkordbBrowser => DEFAULT_FK_BROWSER_PORT,
        _ => DEFAULT_FK_PORT,
    };
    let free = |port: u16| {
        !published.contains(&port) && std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    };
    match explicit {
        Some(0) => {
            return Err(Error::FalkorUsage(
                "--port 0 is not allowed; pick a specific port or omit the flag".into(),
            ));
        }
        Some(port) if free(port) => {
            return Ok(port);
        }
        Some(port) => {
            return Err(Error::PortInUse { kind, port });
        }
        None => {}
    }
    if free(default_port) {
        return Ok(default_port);
    }
    for p in (default_port + 1)..=(default_port + 100) {
        if free(p) {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(kind))
}

/// Auto-pick the browser port, never colliding with the already-resolved
/// protocol port (each free-port probe releases its socket before Docker
/// binds both, so an unguarded pick can take the same port twice).
fn resolve_browser_port_excluding(host_port: u16) -> Result<u16> {
    let published = crate::local::docker::published_host_ports_blocking();
    let picked = resolve_port_with(None, PortKind::FalkordbBrowser, &published)?;
    if picked != host_port {
        return Ok(picked);
    }
    for p in (DEFAULT_FK_BROWSER_PORT + 1)..=(DEFAULT_FK_BROWSER_PORT + 101) {
        if p != host_port
            && !published.contains(&p)
            && std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
        {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(PortKind::FalkordbBrowser))
}

/// Mirror of [`resolve_browser_port_excluding`]: the auto-picked protocol
/// port must steer around an explicitly requested Browser port, or the two
/// would collide only at Docker bind time after a full pull/create cycle.
fn resolve_host_port_excluding(browser_port: u16) -> Result<u16> {
    let published = crate::local::docker::published_host_ports_blocking();
    let picked = resolve_port_with(None, PortKind::Falkordb, &published)?;
    if picked != browser_port {
        return Ok(picked);
    }
    for p in (DEFAULT_FK_PORT + 1)..=(DEFAULT_FK_PORT + 101) {
        if p != browser_port
            && !published.contains(&p)
            && std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
        {
            return Ok(p);
        }
    }
    Err(Error::PortUnavailable(PortKind::Falkordb))
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
/// resume keeps using the credential the data was created with. Fails
/// loudly when the container cannot be inspected: an empty password would
/// fail authentication later with a message pointing at the wrong cause.
async fn read_fk_password(docker: &bollard::Docker, id: &str) -> Result<String> {
    let inspect = docker.inspect_container(id, None).await.map_err(|error| {
        Error::DockerError(format!(
            "could not read the credentials of container '{id}': {error}"
        ))
    })?;
    Ok(inspect
        .config
        .and_then(|c| c.env)
        .unwrap_or_default()
        .iter()
        .find_map(|e| e.strip_prefix("REDIS_ARGS=").map(password_from_redis_args))
        .unwrap_or_default())
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
        // Fail closed: deleting the data directory while the container still
        // exists would leave a container bind-mounting a deleted path, and
        // reporting success on top of that misleads. The metadata and data
        // stay intact, so the remove is retryable once Docker cooperates.
        docker::stop_and_remove_blocking(cid).map_err(|error| {
            Error::Cleanup(format!(
                "could not remove container '{cid}'; nothing was deleted — \
                 retry `dctl local falkordb remove {name}` once Docker is reachable: {error}"
            ))
        })?;
    }

    // FalkorDB data dir lives at <bucket>/servers/<key>/data/. Remove the
    // <key>/ wrapper so the (name, version) pair leaves no on-disk state.
    // Files inside were written by the container user, so removal goes
    // through the privileged-container fallback when a plain rm fails.
    let fk_dir = server::servers_dir_join(&key)?;
    docker::remove_host_dir_blocking(&fk_dir)?;
    server::try_remove_server_info_locked(&key, &metadata_lock)?;
    let out = output::ServerRemoveOutput {
        name: name.to_string(),
        selection: None,
    };
    output::print_output(&out, json);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn client(
    name: Option<String>,
    version: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    query: Option<String>,
    graph: Option<String>,
    direct_password: Option<String>,
    extra_args: Vec<String>,
) -> Result<()> {
    let graph_name = graph.unwrap_or_else(|| "g".to_string());
    if host.is_some() || port.is_some() {
        // Direct connect, native client (ADR-0009): any Redis-protocol graph
        // server, no managed lookup, optional password. No native REPL: an
        // interactive session needs a managed instance.
        let h = host.unwrap_or_else(|| "127.0.0.1".to_string());
        let p = port.unwrap_or(DEFAULT_FK_PORT);
        // The query requirement and the interactive-only passthrough args
        // are clap-level constraints now (validate_post_parse, exit 2).
        let cypher = query.expect("clap rejects direct mode without --query");
        return run_native_cypher(&h, p, direct_password.as_deref(), &graph_name, &cypher).await;
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
    let password = read_fk_password(&docker, container_id).await?;
    let tls = info.tls == Some(true);

    // Same shape as the Postgres client: `--`-passthrough arguments reach
    // redis-cli only in interactive mode, so their presence must not force
    // the native branch (which would blind-read stdin on a TTY and drop them).
    let interactive =
        query.is_none() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive {
        // The redis-cli REPL cannot be rebuilt from a library: interactive
        // mode keeps the container's redis-cli over docker exec (ADR-0009),
        // authenticating through REDISCLI_AUTH (password face) or the
        // in-container client pair (certificate face).
        let mut cli_args: Vec<String> = vec!["--no-auth-warning".into()];
        cli_args.extend(extra_args);
        return docker::exec_redis_cli_in_container_with_face(
            &docker,
            container_id,
            &cli_args,
            &password,
            tls,
        )
        .await;
    }
    let cypher = match query {
        Some(cypher) => cypher,
        // Piped stdin is one Cypher statement (the retired raw-command
        // stream is part of the ADR-0009 breaking change).
        None => {
            let mut buffer = String::new();
            // An unreadable stdin is not an empty query.
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer).map_err(Error::Io)?;
            buffer
        }
    };
    if tls {
        // Certificate-face programmatic path (REQ-0017): fred carries the
        // native mTLS identity redis-rs does not expose (S004), so the query
        // rides the published TLS port straight from the host instead of the
        // in-container redis-cli exec detour.
        return run_tls_cypher("127.0.0.1", info.tcp_port, &graph_name, &cypher).await;
    }
    run_native_cypher(
        "127.0.0.1",
        info.tcp_port,
        Some(&password),
        &graph_name,
        &cypher,
    )
    .await
}

/// The certificate-face programmatic leg (REQ-0017): one Cypher statement
/// over the published TLS port with the dctl client certificate. fred is on
/// this leg because its TLS connector takes a full rustls ClientConfig — the
/// client-certificate injection redis-rs never exposed (S004) — so no exec
/// detour through the container's redis-cli is needed.
async fn run_tls_cypher(host: &str, port: u16, graph_name: &str, cypher: &str) -> Result<()> {
    print!(
        "{}",
        tls_cypher_table(host, port, graph_name, cypher).await?
    );
    Ok(())
}

/// The rendered-table core of the certificate-face leg, split out so the
/// opt-in live test can assert on the decoded output without capturing
/// stdout.
async fn tls_cypher_table(host: &str, port: u16, graph_name: &str, cypher: &str) -> Result<String> {
    use fred::clients::Client;
    use fred::interfaces::ClientLike;
    use fred::types::config::{Config as FredConfig, ServerConfig, TlsConfig};

    let tls = crate::local::ca::tls_config(FK_ACL_USER)?;
    let config = FredConfig {
        server: ServerConfig::new_centralized(host, port),
        tls: Some(TlsConfig::from(tls)),
        ..Default::default()
    };
    let client = Client::new(config, None, None, None);
    client.init().await.map_err(|error| {
        // The library's text is human-only context; the parity envelope
        // keeps the self-composed sentence (the upload path's split).
        eprintln!("client certificate connection to {host}:{port} failed: {error}");
        Error::FalkorUsage(format!(
            "could not connect to FalkorDB at {host}:{port} with the client certificate; \
             check that the instance is running and the dctl CA material is intact"
        ))
    })?;
    let outcome = compact_cypher_table(
        &mut FredProcedureCaller(&client),
        host,
        port,
        graph_name,
        cypher,
    )
    .await;
    let _ = client.quit().await;
    outcome
}

/// The transport the compact-protocol decoder talks to: GRAPH.QUERY for
/// rows and for the db.* schema listings (the official client's refresh
/// uses the same command). Split out so the decoder is drivable from tests
/// with canned replies.
trait ProcedureCaller {
    async fn query(
        &mut self,
        graph: &str,
        cypher: &str,
    ) -> std::result::Result<fred::types::Value, String>;

    async fn list_schema(
        &mut self,
        graph: &str,
        procedure: &str,
    ) -> std::result::Result<Vec<String>, String>;
}

/// The fred-backed caller: custom commands on a connected client.
struct FredProcedureCaller<'a>(&'a fred::clients::Client);

impl ProcedureCaller for FredProcedureCaller<'_> {
    async fn query(
        &mut self,
        graph: &str,
        cypher: &str,
    ) -> std::result::Result<fred::types::Value, String> {
        use fred::interfaces::ClientLike;
        use fred::types::{ClusterHash, CustomCommand};
        self.0
            .custom(
                CustomCommand::new("GRAPH.QUERY", ClusterHash::FirstKey, false),
                vec![
                    graph.to_string(),
                    cypher.to_string(),
                    "--compact".to_string(),
                ],
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn list_schema(
        &mut self,
        graph: &str,
        procedure: &str,
    ) -> std::result::Result<Vec<String>, String> {
        use fred::interfaces::ClientLike;
        let reply = self
            .0
            .custom::<fred::types::Value, _>(
                fred::types::CustomCommand::new(
                    "GRAPH.QUERY",
                    fred::types::ClusterHash::FirstKey,
                    false,
                ),
                vec![graph.to_string(), format!("CALL {procedure}()")],
            )
            .await
            .map_err(|error| error.to_string())?;
        parse_procedure_rows(reply)
    }
}

/// Rows of a `CALL db.*()` listing: the reply is `[header, rows, stats]` and
/// each row's first slot holds the bare name (the official client's refresh
/// reads the same shape).
fn parse_procedure_rows(reply: fred::types::Value) -> std::result::Result<Vec<String>, String> {
    use fred::types::Value;
    let decode = |message: &str| format!("could not decode the schema listing: {message}");
    let Value::Array(mut parts) = reply else {
        return Err(decode("expected a top-level array"));
    };
    let Some(rows) = parts.get_mut(1) else {
        return Err(decode("expected header, rows and stats sections"));
    };
    let Value::Array(rows) = std::mem::replace(rows, Value::Null) else {
        return Err(decode("expected the rows section to be an array"));
    };
    let mut names = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Array(mut slots) = row else {
            return Err(decode("expected each row to be an array"));
        };
        let Some(first) = slots.drain(..).next() else {
            return Err(decode("expected at least one slot per row"));
        };
        match first {
            Value::String(name) => names.push(name.to_string()),
            Value::Bytes(name) => names.push(
                String::from_utf8(name.to_vec())
                    .map_err(|_| decode("a schema name is not valid UTF-8"))?,
            ),
            _ => return Err(decode("expected the first row slot to be a string")),
        }
    }
    Ok(names)
}

/// Run one compact-protocol GRAPH.QUERY and render the reply through the
/// shared table renderer, decoding with the same shapes the official client
/// parses (type-marker pairs, schema-resolved entity ids) so both faces of
/// the same instance print identically.
async fn compact_cypher_table<C: ProcedureCaller>(
    caller: &mut C,
    host: &str,
    port: u16,
    graph_name: &str,
    cypher: &str,
) -> Result<String> {
    let reply = caller.query(graph_name, cypher).await.map_err(|message| {
        // Transport/server text is human-only; the parity envelope keeps
        // the self-composed sentence.
        eprintln!("GRAPH.QUERY on {host}:{port} failed: {message}");
        Error::FalkorUsage(format!(
            "the Cypher query failed on FalkorDB at {host}:{port}; the server's message \
             is on stderr"
        ))
    })?;
    let mut schema = GraphSchemaMaps::default();
    let (columns, rows) = decode_query_reply(reply, caller, graph_name, &mut schema).await?;
    Ok(render_falkor_table(&columns, &rows))
}

/// Which db.* listing backs a schema id namespace.
#[derive(Clone, Copy, Debug)]
enum SchemaKind {
    Labels,
    Relationships,
    Properties,
}

impl SchemaKind {
    /// The procedure names the official client's refresh sends, verbatim
    /// (upper-case, over GRAPH.QUERY): keeping the identical wire shape
    /// removes any server-side parsing difference from the equation.
    fn procedure(self) -> &'static str {
        match self {
            SchemaKind::Labels => "DB.LABELS",
            SchemaKind::Relationships => "DB.RELATIONSHIPTYPES",
            SchemaKind::Properties => "DB.PROPERTYKEYS",
        }
    }
}

/// The label, relationship-type and property-key id tables a compact reply
/// resolves against, refreshed lazily from the server on a miss (mirroring
/// the official client's GraphSchema).
#[derive(Default)]
struct GraphSchemaMaps {
    labels: std::collections::HashMap<i64, String>,
    relationships: std::collections::HashMap<i64, String>,
    properties: std::collections::HashMap<i64, String>,
}

impl GraphSchemaMaps {
    fn map(&mut self, kind: SchemaKind) -> &mut std::collections::HashMap<i64, String> {
        match kind {
            SchemaKind::Labels => &mut self.labels,
            SchemaKind::Relationships => &mut self.relationships,
            SchemaKind::Properties => &mut self.properties,
        }
    }

    async fn resolve<C: ProcedureCaller>(
        &mut self,
        caller: &mut C,
        graph: &str,
        kind: SchemaKind,
        id: i64,
    ) -> Result<String> {
        if let Some(name) = self.map(kind).get(&id) {
            return Ok(name.clone());
        }
        self.refresh(caller, graph, kind).await?;
        self.map(kind).get(&id).cloned().ok_or_else(|| {
            Error::FalkorUsage(format!(
                "the graph schema has no {kind:?} id {id} after a refresh"
            ))
        })
    }

    async fn refresh<C: ProcedureCaller>(
        &mut self,
        caller: &mut C,
        graph: &str,
        kind: SchemaKind,
    ) -> Result<()> {
        let names = caller
            .list_schema(graph, kind.procedure())
            .await
            .map_err(|message| {
                // Human-only context; the parity envelope stays self-composed.
                let procedure = kind.procedure();
                eprintln!("schema listing {procedure} failed: {message}");
                Error::FalkorUsage(
                    "could not refresh the graph schema over FalkorDB; the server's message \
                     is on stderr"
                        .to_string(),
                )
            })?;
        *self.map(kind) = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| (index as i64, name))
            .collect();
        Ok(())
    }
}

/// Split a compact-protocol value into its type marker and payload.
fn split_type_marker(value: fred::types::Value) -> Result<(i64, fred::types::Value)> {
    use fred::types::Value;
    let Value::Array(mut pair) = value else {
        return Err(Error::FalkorUsage(
            "expected a [type marker, value] pair in the reply".into(),
        ));
    };
    if pair.len() != 2 {
        return Err(Error::FalkorUsage(format!(
            "expected exactly 2 elements for a type marker pair, got {}",
            pair.len()
        )));
    }
    let payload = pair.pop().expect("length checked above");
    let Value::Integer(marker) = pair.pop().expect("length checked above") else {
        return Err(Error::FalkorUsage(
            "expected an integer type marker in the reply".into(),
        ));
    };
    Ok((marker, payload))
}

fn value_into_string(value: fred::types::Value) -> Result<String> {
    use fred::types::Value;
    match value {
        Value::String(text) => Ok(text.to_string()),
        Value::Bytes(bytes) => String::from_utf8(bytes.to_vec())
            .map_err(|_| Error::FalkorUsage("a reply string is not valid UTF-8".into())),
        other => Err(Error::FalkorUsage(format!(
            "expected a string in the reply, got {:?}",
            ValueKind::of(&other)
        ))),
    }
}

fn value_as_int(value: &fred::types::Value) -> Result<i64> {
    match value {
        fred::types::Value::Integer(number) => Ok(*number),
        other => Err(Error::FalkorUsage(format!(
            "expected an integer in the reply, got {:?}",
            ValueKind::of(other)
        ))),
    }
}

fn value_into_vec(value: fred::types::Value) -> Result<Vec<fred::types::Value>> {
    match value {
        fred::types::Value::Array(items) => Ok(items),
        other => Err(Error::FalkorUsage(format!(
            "expected an array in the reply, got {:?}",
            ValueKind::of(&other)
        ))),
    }
}

/// The compact protocol's marker vocabulary (the official client's
/// ParserTypeMarker); the kind label doubles as the decode error context.
#[derive(Clone, Copy, Debug)]
enum CellKind {
    None,
    String,
    I64,
    Bool,
    F64,
    Array,
    Edge,
    Node,
    Path,
    Map,
    Point,
    Vec32,
    DateTime,
    Date,
    Time,
    Duration,
}

impl CellKind {
    fn from_marker(marker: i64) -> Result<Self> {
        Ok(match marker {
            1 => Self::None,
            2 => Self::String,
            3 => Self::I64,
            4 => Self::Bool,
            5 => Self::F64,
            6 => Self::Array,
            7 => Self::Edge,
            8 => Self::Node,
            9 => Self::Path,
            10 => Self::Map,
            11 => Self::Point,
            12 => Self::Vec32,
            13 => Self::DateTime,
            14 => Self::Date,
            15 => Self::Time,
            16 => Self::Duration,
            other => {
                return Err(Error::FalkorUsage(format!(
                    "unknown type marker {other} in the reply"
                )));
            }
        })
    }
}

/// The value kind label for decode errors (fred's Value has no kind method).
struct ValueKind;

impl ValueKind {
    fn of(value: &fred::types::Value) -> &'static str {
        use fred::types::Value;
        match value {
            Value::Boolean(_) => "boolean",
            Value::Integer(_) => "integer",
            Value::Double(_) => "double",
            Value::String(_) => "string",
            Value::Bytes(_) => "bytes",
            Value::Null => "nil",
            Value::Queued => "queued",
            Value::Map(_) => "map",
            Value::Array(_) => "array",
        }
    }
}

/// Decode a `[header, rows, stats]` GRAPH.QUERY reply into rendered table
/// rows. Stats-only and header-only replies (no matching rows) yield empty
/// row sets, matching the official client's dispatch.
async fn decode_query_reply<C: ProcedureCaller>(
    reply: fred::types::Value,
    caller: &mut C,
    graph: &str,
    schema: &mut GraphSchemaMaps,
) -> Result<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let mut parts = value_into_vec(reply)?;
    // The stats trailer always closes the reply; it is not rendered.
    if parts.pop().is_none() {
        return Err(Error::FalkorUsage(
            "the GRAPH.QUERY reply has no stats section".into(),
        ));
    }
    let rows_raw = if parts.len() == 2 {
        value_into_vec(parts.pop().expect("length checked above"))?
    } else {
        Vec::new()
    };
    let columns = match parts.pop() {
        Some(header) => parse_header(header)?,
        None => Vec::new(),
    };

    let mut rows: Vec<Vec<Option<String>>> = Vec::with_capacity(rows_raw.len());
    for row in rows_raw {
        let cells = value_into_vec(row)?;
        if cells.len() != columns.len() {
            return Err(Error::FalkorUsage(format!(
                "a reply row has {} values for {} columns",
                cells.len(),
                columns.len()
            )));
        }
        let mut rendered = Vec::with_capacity(cells.len());
        for cell in cells {
            let value = decode_falkor_value(cell, caller, graph, schema).await?;
            rendered.push(Some(render_falkor_value(&value)));
        }
        rows.push(rendered);
    }
    Ok((columns, rows))
}

/// Parse the header section: each column arrives as a one-element array, or
/// a `[type, name]` pair for typed columns.
fn parse_header(header: fred::types::Value) -> Result<Vec<String>> {
    let items = value_into_vec(header)?;
    let mut columns = Vec::with_capacity(items.len());
    for item in items {
        let slots = value_into_vec(item)?;
        let key = if slots.len() == 2 {
            slots.into_iter().nth(1).expect("length checked above")
        } else {
            slots
                .into_iter()
                .next()
                .ok_or_else(|| Error::FalkorUsage("a header column is empty".into()))?
        };
        columns.push(value_into_string(key)?);
    }
    Ok(columns)
}

/// Decode one reply cell (a type-marker pair) into a FalkorValue, the same
/// shapes the official client produces so the shared renderer prints both
/// faces identically.
async fn decode_falkor_value<C: ProcedureCaller>(
    cell: fred::types::Value,
    caller: &mut C,
    graph: &str,
    schema: &mut GraphSchemaMaps,
) -> Result<falkordb::FalkorValue> {
    let (marker, payload) = split_type_marker(cell)?;
    decode_typed(marker, payload, caller, graph, schema).await
}

async fn decode_typed<C: ProcedureCaller>(
    marker: i64,
    payload: fred::types::Value,
    caller: &mut C,
    graph: &str,
    schema: &mut GraphSchemaMaps,
) -> Result<falkordb::FalkorValue> {
    use falkordb::FalkorValue;
    match CellKind::from_marker(marker)? {
        CellKind::None => Ok(FalkorValue::None),
        CellKind::String => Ok(FalkorValue::String(value_into_string(payload)?)),
        CellKind::I64 => Ok(FalkorValue::I64(value_as_int(&payload)?)),
        CellKind::Bool => match value_into_string(payload)?.as_str() {
            "true" => Ok(FalkorValue::Bool(true)),
            "false" => Ok(FalkorValue::Bool(false)),
            other => Err(Error::FalkorUsage(format!(
                "expected true or false for a boolean cell, got {other:?}"
            ))),
        },
        CellKind::F64 => {
            let text = value_into_string(payload)?;
            let number = text
                .parse::<f64>()
                .map_err(|_| Error::FalkorUsage(format!("expected a float cell, got {text:?}")))?;
            Ok(FalkorValue::F64(number))
        }
        CellKind::Array => {
            let items = value_into_vec(payload)?;
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                // Recursive decode through decode_falkor_value needs the
                // pin; the type marker vocabulary is fixed, so the depth
                // follows the reply's nesting, which the server bounds.
                values.push(Box::pin(decode_falkor_value(item, caller, graph, schema)).await?);
            }
            Ok(FalkorValue::Array(values))
        }
        CellKind::Edge => {
            let edge = decode_edge(payload, caller, graph, schema).await?;
            Ok(FalkorValue::Edge(edge))
        }
        CellKind::Node => {
            let node = decode_node(payload, caller, graph, schema).await?;
            Ok(FalkorValue::Node(node))
        }
        CellKind::Path => {
            let mut slots = value_into_vec(payload)?;
            if slots.len() != 2 {
                return Err(Error::FalkorUsage(format!(
                    "expected exactly 2 elements for a path, got {}",
                    slots.len()
                )));
            }
            let edges_raw = slots.pop().expect("length checked above");
            let nodes_raw = slots.pop().expect("length checked above");
            let mut nodes = Vec::new();
            for item in value_into_vec(nodes_raw)? {
                nodes.push(decode_node(item, caller, graph, schema).await?);
            }
            let mut relationships = Vec::new();
            for item in value_into_vec(edges_raw)? {
                relationships.push(decode_edge(item, caller, graph, schema).await?);
            }
            Ok(FalkorValue::Path(falkordb::Path {
                nodes,
                relationships,
            }))
        }
        CellKind::Map => {
            let slots = value_into_vec(payload)?;
            if slots.len() % 2 != 0 {
                return Err(Error::FalkorUsage(format!(
                    "expected an even number of map slots, got {}",
                    slots.len()
                )));
            }
            let mut map = std::collections::HashMap::with_capacity(slots.len() / 2);
            let mut slots = slots.into_iter();
            while let (Some(key), Some(value)) = (slots.next(), slots.next()) {
                let key = value_into_string(key)?;
                let value = Box::pin(decode_falkor_value(value, caller, graph, schema)).await?;
                map.insert(key, value);
            }
            Ok(FalkorValue::Map(map))
        }
        CellKind::Point => {
            let mut slots = value_into_vec(payload)?;
            if slots.len() != 2 {
                return Err(Error::FalkorUsage(format!(
                    "expected latitude and longitude for a point, got {}",
                    slots.len()
                )));
            }
            let longitude = slots.pop().expect("length checked above");
            let latitude = slots.pop().expect("length checked above");
            Ok(FalkorValue::Point(falkordb::Point {
                latitude: parse_float_cell(latitude)?,
                longitude: parse_float_cell(longitude)?,
            }))
        }
        CellKind::Vec32 => {
            let items = value_into_vec(payload)?;
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let text = value_into_string(item)?;
                values.push(text.parse::<f32>().map_err(|_| {
                    Error::FalkorUsage(format!("expected a float vector member, got {text:?}"))
                })?);
            }
            // The Vec32 type is not exported by the crate, so the password
            // face's renderer only ever shows the enum's Debug form; mirror
            // that text exactly (outer variant name included) to keep both
            // faces printing identically.
            Ok(FalkorValue::String(format!(
                "Vec32(Vec32 {{ values: {values:?} }})"
            )))
        }
        kind @ (CellKind::DateTime | CellKind::Date | CellKind::Time | CellKind::Duration) => {
            let secs = value_as_int(&payload)?;
            Ok(match kind {
                CellKind::DateTime => FalkorValue::DateTime(falkordb::DateTime::new(secs)),
                CellKind::Date => FalkorValue::Date(falkordb::Date::new(secs)),
                CellKind::Time => FalkorValue::Time(falkordb::Time::new(secs)),
                CellKind::Duration => FalkorValue::Duration(falkordb::Duration::new(secs)),
                _ => unreachable!("the outer arm pinned the temporal kinds"),
            })
        }
    }
}

fn parse_float_cell(value: fred::types::Value) -> Result<f64> {
    let text = value_into_string(value)?;
    text.parse::<f64>()
        .map_err(|_| Error::FalkorUsage(format!("expected a float cell, got {text:?}")))
}

/// Node payload: `[id, label ids, [key id, marker, value] triples]`, with
/// both id namespaces resolved against the live schema.
async fn decode_node<C: ProcedureCaller>(
    payload: fred::types::Value,
    caller: &mut C,
    graph: &str,
    schema: &mut GraphSchemaMaps,
) -> Result<falkordb::Node> {
    let mut slots = value_into_vec(payload)?;
    if slots.len() != 3 {
        return Err(Error::FalkorUsage(format!(
            "expected exactly 3 elements for a node, got {}",
            slots.len()
        )));
    }
    let properties_raw = slots.pop().expect("length checked above");
    let labels_raw = slots.pop().expect("length checked above");
    let entity_id = value_as_int(&slots.pop().expect("length checked above"))?;
    let mut labels = Vec::new();
    for label in value_into_vec(labels_raw)? {
        let id = value_as_int(&label)?;
        labels.push(
            schema
                .resolve(caller, graph, SchemaKind::Labels, id)
                .await?,
        );
    }
    let properties = decode_properties(properties_raw, caller, graph, schema).await?;
    Ok(falkordb::Node {
        entity_id,
        labels,
        properties,
    })
}

/// Edge payload: `[id, relationship id, source id, destination id, property
/// triples]`.
async fn decode_edge<C: ProcedureCaller>(
    payload: fred::types::Value,
    caller: &mut C,
    graph: &str,
    schema: &mut GraphSchemaMaps,
) -> Result<falkordb::Edge> {
    let mut slots = value_into_vec(payload)?;
    if slots.len() != 5 {
        return Err(Error::FalkorUsage(format!(
            "expected exactly 5 elements for an edge, got {}",
            slots.len()
        )));
    }
    let properties_raw = slots.pop().expect("length checked above");
    let dst_node_id = value_as_int(&slots.pop().expect("length checked above"))?;
    let src_node_id = value_as_int(&slots.pop().expect("length checked above"))?;
    let relationship_id = value_as_int(&slots.pop().expect("length checked above"))?;
    let entity_id = value_as_int(&slots.pop().expect("length checked above"))?;
    let relationship_type = schema
        .resolve(caller, graph, SchemaKind::Relationships, relationship_id)
        .await?;
    let properties = decode_properties(properties_raw, caller, graph, schema).await?;
    Ok(falkordb::Edge {
        entity_id,
        relationship_type,
        src_node_id,
        dst_node_id,
        properties,
    })
}

/// Property triples: `[key id, type marker, value]`, keys resolved against
/// the property-key schema and values decoded like top-level cells.
async fn decode_properties<C: ProcedureCaller>(
    payload: fred::types::Value,
    caller: &mut C,
    graph: &str,
    schema: &mut GraphSchemaMaps,
) -> Result<std::collections::HashMap<String, falkordb::FalkorValue>> {
    let triples = value_into_vec(payload)?;
    let mut map = std::collections::HashMap::with_capacity(triples.len());
    for triple in triples {
        let mut slots = value_into_vec(triple)?;
        if slots.len() != 3 {
            return Err(Error::FalkorUsage(format!(
                "expected exactly 3 elements for a property, got {}",
                slots.len()
            )));
        }
        let value_raw = slots.pop().expect("length checked above");
        let marker = value_as_int(&slots.pop().expect("length checked above"))?;
        let key_id = value_as_int(&slots.pop().expect("length checked above"))?;
        let key = schema
            .resolve(caller, graph, SchemaKind::Properties, key_id)
            .await?;
        // Boxed to break the decode_typed → node/edge → properties →
        // decode_typed recursion the compiler cannot size.
        let value = Box::pin(decode_typed(marker, value_raw, caller, graph, schema)).await?;
        map.insert(key, value);
    }
    Ok(map)
}

/// Percent-encode every byte outside the URL unreserved set. The redis URL
/// parser decodes the userinfo component back, so this is what keeps a
/// password containing `%`, `/`, `#`, `?`, `@` … intact across the URL
/// round-trip instead of silently changing the credential.
fn percent_encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The native Cypher leg (ADR-0009): connect with the official falkordb
/// client over the Redis protocol, run one Cypher statement, render the
/// result set as an aligned table.
async fn run_native_cypher(
    host: &str,
    port: u16,
    password: Option<&str>,
    graph_name: &str,
    cypher: &str,
) -> Result<()> {
    use falkordb::FalkorClientBuilder;
    use futures_util::StreamExt;

    let url = match password {
        // The redis URL grammar carries the password with an empty username.
        // Every byte outside the unreserved set is percent-encoded: the URL
        // parser decodes them back on the other side, so a literal `%` or
        // delimiter in the password would otherwise silently change the
        // credential (or truncate the authority).
        Some(password) => format!(
            "redis://:{}@{host}:{port}",
            percent_encode_component(password)
        ),
        None => format!("redis://{host}:{port}"),
    };
    let connection_info = url
        .as_str()
        .try_into()
        .map_err(|error: falkordb::FalkorDBError| {
            // The message must not embed the URL: it carries the password.
            Error::FalkorUsage(format!("invalid FalkorDB endpoint {host}:{port}: {error}"))
        })?;
    let client = FalkorClientBuilder::new_async()
        .with_connection_info(connection_info)
        .build()
        .await
        .map_err(|error| {
            Error::FalkorUsage(format!(
                "could not connect to FalkorDB at {host}:{port}: {error}"
            ))
        })?;
    let mut graph = client.select_graph(graph_name);
    let result = graph
        .query(cypher)
        .execute()
        .await
        .map_err(|error| Error::FalkorUsage(format!("{error}")))?;

    let columns: Vec<String> = result.header.iter().cloned().collect();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut stream = result.data;
    while let Some(row) = stream.next().await {
        let row = row.map_err(|error| Error::FalkorUsage(format!("{error}")))?;
        rows.push(
            row.into_values()
                .iter()
                .map(|value| Some(render_falkor_value(value)))
                .collect(),
        );
    }
    print!("{}", render_falkor_table(&columns, &rows));
    Ok(())
}

/// Render one FalkorDB value for the table: scalars in their natural text,
/// graph entities as a compact-but-faithful summary with id and properties
/// (the crate exposes no Display for FalkorValue).
fn render_falkor_value(value: &falkordb::FalkorValue) -> String {
    use falkordb::FalkorValue;
    match value {
        FalkorValue::String(text) => text.clone(),
        FalkorValue::I64(number) => number.to_string(),
        FalkorValue::F64(number) => number.to_string(),
        FalkorValue::Bool(flag) => flag.to_string(),
        FalkorValue::None => String::new(),
        FalkorValue::Node(node) => {
            let mut out = format!("(:{} #{}", node.labels.join(":"), node.entity_id);
            if !node.properties.is_empty() {
                out.push_str(&render_entity_properties(&node.properties));
            }
            out.push(')');
            out
        }
        FalkorValue::Edge(edge) => {
            let mut out = format!("-[{} #{}", edge.relationship_type, edge.entity_id);
            if !edge.properties.is_empty() {
                out.push_str(&render_entity_properties(&edge.properties));
            }
            out.push_str("]->");
            out
        }
        FalkorValue::Path(path) => {
            format!("[path {} nodes]", path.nodes.len())
        }
        other => format!("{other:?}"),
    }
}

fn render_entity_properties(
    properties: &std::collections::HashMap<String, falkordb::FalkorValue>,
) -> String {
    let mut pairs: Vec<String> = properties
        .iter()
        .map(|(key, value)| format!("{key}: {}", render_falkor_value(value)))
        .collect();
    pairs.sort();
    format!(" {{{}}}", pairs.join(", "))
}

/// Aligned table rendering matching the Postgres client's shape (shared
/// renderer; numeric-looking cells right-align) plus the row-count footer.
fn render_falkor_table(columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let mut out = output::render_aligned_table(columns, rows, true);
    let count = rows.len();
    out.push_str(&format!(
        "({count} row{})\n",
        if count == 1 { "" } else { "s" }
    ));
    out
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
    ))?;

    // A recovered instance can carry an unknown (0) browser port; writing
    // http://127.0.0.1:0 into a user's .env would be a lie. Clobber any
    // stale value with an explicit empty one instead.
    let browser_url = if info.http_port == 0 {
        String::new()
    } else {
        format!("http://127.0.0.1:{}", info.http_port)
    };
    // Same policy for the protocol port: an empty value says "unknown"
    // more honestly than 0, which a consumer would try to dial.
    let tcp_port = if info.tcp_port == 0 {
        String::new()
    } else {
        info.tcp_port.to_string()
    };
    // The face decides the shape (ADR-0011): the certificate face emits the
    // TLS material paths with no password; the password face keeps the
    // historical four.
    let vars: Vec<(&str, String)> = if info.tls == Some(true) {
        let ca = crate::local::ca::ensure_ca()?;
        let (cert, key) = crate::local::ca::client_cert(FK_ACL_USER)?;
        vec![
            ("FALKORDB_HOST", "127.0.0.1".to_string()),
            ("FALKORDB_PORT", tcp_port),
            ("FALKORDB_TLS", "true".to_string()),
            ("FALKORDB_CA_CERT", ca.display().to_string()),
            ("FALKORDB_CLIENT_CERT", cert.display().to_string()),
            ("FALKORDB_CLIENT_KEY", key.display().to_string()),
            ("FALKORDB_BROWSER_URL", browser_url),
        ]
    } else {
        vec![
            ("FALKORDB_HOST", "127.0.0.1".to_string()),
            ("FALKORDB_PORT", tcp_port),
            ("FALKORDB_PASSWORD", password),
            ("FALKORDB_BROWSER_URL", browser_url),
        ]
    };

    let filename = if use_local { ".env.local" } else { ".env" };
    let path = std::path::Path::new(filename);

    let content = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        // All FALKORDB_* keys share the managed prefix, so update_dotenv
        // replaces in place; only the cross-face key needs exact-key
        // stripping so a switch cannot leave both shapes behind.
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
                    "FALKORDB_TLS"
                        | "FALKORDB_CA_CERT"
                        | "FALKORDB_CLIENT_CERT"
                        | "FALKORDB_CLIENT_KEY"
                ) || (strip_password && key == "FALKORDB_PASSWORD");
                !face_key
            })
            .chain(std::iter::once(""))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        crate::local::update_dotenv(&existing, "FALKORDB_", &vars)
    } else {
        vars.iter()
            .map(|(k, v)| crate::local::format_dotenv_line("", k, v))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };

    crate::local::write_dotenv_file(path, &content)?;

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

async fn read_fk_password_for_dotenv(container_id: &str) -> Result<String> {
    if container_id.is_empty() {
        return Ok(String::new());
    }
    // A failed connect must not degrade into an empty password: the dotenv
    // file would carry it with a success exit code.
    let docker = docker::connect().await?;
    read_fk_password(&docker, container_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn resolve_port_skips_docker_published_ports() {
        // REQ-0015: an iptables-NAT published port has no host listener, so
        // only the injected set can see it; the pick must steer around it.
        let error = resolve_port_with(
            Some(DEFAULT_FK_PORT),
            PortKind::Falkordb,
            &[DEFAULT_FK_PORT],
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::PortInUse { port, .. } if port == DEFAULT_FK_PORT),
            "explicit collision with a published port: {error:?}"
        );
        let picked = resolve_port_with(None, PortKind::Falkordb, &[DEFAULT_FK_PORT]).unwrap();
        assert_ne!(picked, DEFAULT_FK_PORT);
        assert!(((DEFAULT_FK_PORT + 1)..=(DEFAULT_FK_PORT + 100)).contains(&picked));
    }

    #[test]
    fn browser_port_exclusion_skips_docker_published_ports() {
        // The exclusion loop must honor the published set too.
        let published = [DEFAULT_FK_BROWSER_PORT];
        assert!(
            !resolve_port_with(
                Some(DEFAULT_FK_BROWSER_PORT),
                PortKind::FalkordbBrowser,
                &published
            )
            .is_ok()
        );
    }

    #[test]
    fn browser_port_auto_pick_defaults_to_3000() {
        // The auto-picked Browser port must come from the FalkordbBrowser arm
        // (3000), not the Redis protocol default (6379): the docs promise
        // "3000 if free" for an omitted --browser-port.
        let picked = resolve_port_with(None, PortKind::FalkordbBrowser, &[]).unwrap();
        assert_eq!(picked, DEFAULT_FK_BROWSER_PORT);
    }

    #[test]
    fn percent_encoded_password_is_url_safe() {
        // The URL grammar percent-decodes the userinfo component (redis 1.7
        // decodes it back), so every delimiter must be encoded on the way in
        // or the credential silently changes (F-21). The expected string is
        // the RFC 3986 unreserved-set encoding: everything else becomes
        // %XX, which the URL parser reverses losslessly.
        assert_eq!(
            percent_encode_component("p@ss%41/#?"),
            "p%40ss%2541%2F%23%3F"
        );
        // Unreserved characters pass through untouched.
        assert_eq!(percent_encode_component("aZ09-._~"), "aZ09-._~");
    }

    // ── compact-protocol decoder (certificate face, REQ-0017) ────────────

    use fred::types::Value as FredValue;

    fn v_int(i: i64) -> FredValue {
        FredValue::Integer(i)
    }

    fn v_str(text: &str) -> FredValue {
        FredValue::String(text.into())
    }

    fn v_arr(items: Vec<FredValue>) -> FredValue {
        FredValue::Array(items)
    }

    /// A canned transport: one fixed GRAPH.QUERY reply plus fixed schema
    /// listings, so the decoder runs without a server.
    struct CannedProcedureCaller {
        reply: FredValue,
        schema: std::collections::HashMap<&'static str, Vec<String>>,
    }

    impl ProcedureCaller for CannedProcedureCaller {
        async fn query(
            &mut self,
            _graph: &str,
            _cypher: &str,
        ) -> std::result::Result<FredValue, String> {
            Ok(self.reply.clone())
        }

        async fn list_schema(
            &mut self,
            _graph: &str,
            procedure: &str,
        ) -> std::result::Result<Vec<String>, String> {
            Ok(self.schema.get(procedure).cloned().unwrap_or_default())
        }
    }

    fn canned_schema() -> std::collections::HashMap<&'static str, Vec<String>> {
        std::collections::HashMap::from([
            ("DB.LABELS", vec!["Person".to_string()]),
            ("DB.RELATIONSHIPTYPES", vec!["KNOWS".to_string()]),
            (
                "DB.PROPERTYKEYS",
                vec!["name".to_string(), "age".to_string(), "note".to_string()],
            ),
        ])
    }

    /// A one-row reply with one node, one edge, one path and two scalars,
    /// exercising every schema namespace and the recursive property decode.
    fn sample_compact_reply() -> FredValue {
        let node = v_arr(vec![
            v_int(0),
            v_arr(vec![v_int(0)]),
            v_arr(vec![
                v_arr(vec![v_int(0), v_int(2), v_str("alice")]),
                v_arr(vec![v_int(1), v_int(3), v_int(42)]),
            ]),
        ]);
        let edge = v_arr(vec![
            v_int(0),
            v_int(0),
            v_int(0),
            v_int(1),
            v_arr(vec![v_arr(vec![v_int(2), v_int(2), v_str("since 2020")])]),
        ]);
        let path = v_arr(vec![v_arr(vec![node.clone()]), v_arr(vec![edge.clone()])]);
        let row = v_arr(vec![
            v_arr(vec![v_int(8), node]),
            v_arr(vec![v_int(7), edge]),
            v_arr(vec![v_int(9), path]),
            v_arr(vec![v_int(3), v_int(7)]),
            v_arr(vec![v_int(2), v_str("scalar")]),
        ]);
        v_arr(vec![
            v_arr(vec![
                v_arr(vec![v_str("n")]),
                v_arr(vec![v_str("e")]),
                v_arr(vec![v_str("p")]),
                v_arr(vec![v_str("i")]),
                v_arr(vec![v_str("s")]),
            ]),
            v_arr(vec![row]),
            v_arr(vec![v_str("Query internal execution time: 0.1")]),
        ])
    }

    #[tokio::test]
    async fn compact_reply_decodes_entities_scalars_and_schema_ids() {
        let mut caller = CannedProcedureCaller {
            reply: sample_compact_reply(),
            schema: canned_schema(),
        };
        let mut schema = GraphSchemaMaps::default();
        let (columns, rows) =
            decode_query_reply(caller.reply.clone(), &mut caller, "g", &mut schema)
                .await
                .expect("the sample reply decodes");
        assert_eq!(
            columns,
            ["n", "e", "p", "i", "s"].map(String::from).to_vec()
        );
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        // The node resolves label id 0 and property ids 0/1 through the
        // canned schema, mirroring the official client's rendering.
        assert_eq!(
            row[0].as_deref(),
            Some("(:Person #0 {age: 42, name: alice})")
        );
        assert_eq!(row[1].as_deref(), Some("-[KNOWS #0 {note: since 2020}]->"));
        assert_eq!(row[2].as_deref(), Some("[path 1 nodes]"));
        assert_eq!(row[3].as_deref(), Some("7"));
        assert_eq!(row[4].as_deref(), Some("scalar"));
        // The schema refresh ran lazily: the maps started empty and were
        // populated from the canned listings during the decode.
        assert_eq!(schema.labels.get(&0).map(String::as_str), Some("Person"));
    }

    #[tokio::test]
    async fn compact_reply_rejects_rows_that_do_not_match_the_header() {
        let short_row = v_arr(vec![v_arr(vec![v_int(3), v_int(1)])]);
        let reply = v_arr(vec![
            v_arr(vec![v_arr(vec![v_str("a")]), v_arr(vec![v_str("b")])]),
            v_arr(vec![short_row]),
            v_arr(vec![v_str("Query internal execution time: 0.1")]),
        ]);
        let mut caller = CannedProcedureCaller {
            reply: reply.clone(),
            schema: canned_schema(),
        };
        let error = decode_query_reply(reply, &mut caller, "g", &mut GraphSchemaMaps::default())
            .await
            .expect_err("a one-cell row for a two-column header must fail");
        assert!(
            error
                .to_string()
                .contains("a reply row has 1 values for 2 columns"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn compact_reply_errors_when_a_schema_id_stays_unknown() {
        // The listing answers, but with a table that has no id 5: the
        // official client surfaces MissingSchemaId; so does this decode.
        let node = v_arr(vec![v_int(0), v_arr(vec![v_int(5)]), v_arr(vec![])]);
        let reply = v_arr(vec![
            v_arr(vec![v_arr(vec![v_str("n")])]),
            v_arr(vec![v_arr(vec![v_arr(vec![v_int(8), node])])]),
            v_arr(vec![v_str("Query internal execution time: 0.1")]),
        ]);
        let mut caller = CannedProcedureCaller {
            reply: reply.clone(),
            schema: canned_schema(),
        };
        let error = decode_query_reply(reply, &mut caller, "g", &mut GraphSchemaMaps::default())
            .await
            .expect_err("an unresolvable label id must fail");
        assert!(error.to_string().contains("no Labels id 5"), "{error}");
    }

    #[test]
    fn header_pairs_carry_the_name_in_the_second_slot() {
        let header = v_arr(vec![
            v_arr(vec![v_int(2), v_str("typed")]),
            v_arr(vec![v_str("plain")]),
        ]);
        assert_eq!(
            parse_header(header).expect("the header parses"),
            ["typed", "plain"].map(String::from).to_vec()
        );
    }

    #[test]
    fn procedure_listings_take_the_first_slot_of_each_row() {
        let reply = v_arr(vec![
            v_arr(vec![v_str("label")]),
            v_arr(vec![
                v_arr(vec![v_str("Person")]),
                v_arr(vec![v_str("Movie")]),
            ]),
            v_arr(vec![v_str("Query internal execution time: 0.1")]),
        ]);
        assert_eq!(
            parse_procedure_rows(reply).expect("the listing parses"),
            vec!["Person".to_string(), "Movie".to_string()]
        );
    }

    #[test]
    fn unknown_type_markers_are_rejected() {
        let error = CellKind::from_marker(99).expect_err("marker 99 is not in the vocabulary");
        assert!(
            error.to_string().contains("unknown type marker 99"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn vec32_cells_mirror_the_password_faces_debug_rendering() {
        // The crate does not export the Vec32 type, so the certificate face
        // carries a String that must print exactly what the password face's
        // Debug fallback prints for a real FalkorValue::Vec32.
        let reply = v_arr(vec![
            v_arr(vec![v_arr(vec![v_str("v")])]),
            v_arr(vec![v_arr(vec![v_arr(vec![
                v_int(12),
                v_arr(vec![v_str("1.5"), v_str("2.5")]),
            ])])]),
            v_arr(vec![v_str("Query internal execution time: 0.1")]),
        ]);
        let mut caller = CannedProcedureCaller {
            reply: reply.clone(),
            schema: canned_schema(),
        };
        let (columns, rows) =
            decode_query_reply(reply, &mut caller, "g", &mut GraphSchemaMaps::default())
                .await
                .expect("the vector reply decodes");
        assert_eq!(columns, ["v"].map(String::from).to_vec());
        let expected = format!(
            "{:?}",
            Vec32Mirror::Vec32(Vec32 {
                values: vec![1.5_f32, 2.5]
            })
        );
        assert_eq!(rows[0][0].as_deref(), Some(expected.as_str()));
    }

    /// Stand-ins with the same Debug shape as the crate's unexported
    /// `FalkorValue::Vec32(Vec32 { values })`, so the mirrored text can be
    /// asserted without naming the real type.
    #[allow(dead_code)]
    #[derive(Debug)]
    enum Vec32Mirror {
        Vec32(Vec32),
    }

    #[allow(dead_code)]
    #[derive(Debug)]
    struct Vec32 {
        values: Vec<f32>,
    }

    /// Opt-in live round trip for the certificate-face client leg, for
    /// machines whose Docker daemon publishes ports that host loopback
    /// cannot reach (the lan-linux2 limitation): point the address at the
    /// container's bridge IP instead, with the same HOME that started the
    /// instance (the CA material lives under `$HOME/.dctl/ca/`).
    ///
    /// ```text
    /// DCTL_FK_TLS_TEST_ADDR=172.17.0.5:6379 \
    ///   cargo test -p databasectl --bin dctl tls_cypher_live -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "opt-in live leg; needs a running certificate-face instance"]
    async fn tls_cypher_live_round_trip() {
        let address =
            std::env::var("DCTL_FK_TLS_TEST_ADDR").expect("DCTL_FK_TLS_TEST_ADDR=<host>:<port>");
        let (host, port) = address.rsplit_once(':').expect("address shaped host:port");
        let port: u16 = port.parse().expect("numeric port");

        tls_cypher_table(host, port, "g", "CREATE (n:LiveProbe {name: 'roundtrip'})")
            .await
            .expect("create over mTLS");
        let table = tls_cypher_table(host, port, "g", "MATCH (n:LiveProbe) RETURN n")
            .await
            .expect("match over mTLS");
        assert!(
            table.contains("(:LiveProbe") && table.contains("name: roundtrip"),
            "rendered table: {table}"
        );
        // A scalar round trip proves the header/rows decode without any
        // schema refresh in the path.
        let scalar = tls_cypher_table(host, port, "g", "RETURN 7 as seven")
            .await
            .expect("scalar over mTLS");
        assert!(scalar.contains("7"), "scalar table: {scalar}");
    }

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
    fn tag_from_stored_version_maps_latest() {
        assert_eq!(tag_from_stored_version("falkordb:latest"), "latest");
        assert_eq!(tag_from_stored_version("falkordb:v4.20.6"), "4.20.6");
    }

    #[test]
    fn instance_keys_and_metadata_names_agree() {
        assert_eq!(server::fk_instance_key("dev", "4.20.6"), "dev-fk4.20.6");
        assert_eq!(
            docker::fk_container_name("dev", "4.20.6"),
            "dctl-fk-dev-4.20.6"
        );
    }
}

#[cfg(test)]
mod renderer_tests {
    use super::{render_falkor_table, render_falkor_value};
    use falkordb::FalkorValue;

    #[test]
    fn table_rendering_matches_the_postgres_shape() {
        let rendered = render_falkor_table(
            &["id".to_string(), "name".to_string()],
            &[
                vec![Some("1".to_string()), Some("root".to_string())],
                vec![Some("22".to_string()), None],
            ],
        );
        assert!(rendered.contains("id | name\n"), "{rendered}");
        assert!(rendered.contains(" 1 | root"), "{rendered}");
        assert!(rendered.contains("22 |"), "{rendered}");
        assert!(rendered.ends_with("(2 rows)\n"), "{rendered}");
    }

    #[test]
    fn entities_render_with_id_and_properties() {
        let mut node = falkordb::Node {
            entity_id: 7,
            labels: vec!["Person".into()],
            properties: std::collections::HashMap::new(),
        };
        node.properties
            .insert("name".into(), FalkorValue::String("ada".into()));
        assert_eq!(
            render_falkor_value(&FalkorValue::Node(node)),
            "(:Person #7 {name: ada})"
        );

        let edge = falkordb::Edge {
            entity_id: 9,
            relationship_type: "KNOWS".into(),
            src_node_id: 1,
            dst_node_id: 2,
            properties: std::collections::HashMap::new(),
        };
        assert_eq!(
            render_falkor_value(&FalkorValue::Edge(edge)),
            "-[KNOWS #9]->"
        );
        assert_eq!(render_falkor_value(&FalkorValue::None), "");
        assert_eq!(render_falkor_value(&FalkorValue::I64(42)), "42");
    }
}
