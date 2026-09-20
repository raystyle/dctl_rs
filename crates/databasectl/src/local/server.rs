use crate::error::{Error, Result};
use crate::init;
use crate::local::docker;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const METADATA_LOCK_FILE: &str = ".metadata.lock";
const METADATA_TEMP_PREFIX: &str = ".metadata-";

const ADJECTIVES: &[&str] = &[
    "bold", "calm", "dark", "fast", "gold", "keen", "loud", "neat", "pale", "red", "slim", "tall",
    "warm", "blue", "cool", "deep", "flat", "gray", "iron", "wild",
];

const NOUNS: &[&str] = &[
    "bear", "bird", "bolt", "crab", "crow", "dart", "fawn", "fish", "frog", "gull", "hare", "hawk",
    "lynx", "moth", "newt", "orca", "puma", "seal", "swan", "wolf",
];

/// Engine driving a server instance. ClickHouse is a managed binary process;
/// Postgres and FalkorDB are managed Docker containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Clickhouse,
    Postgres,
    Falkordb,
}

impl Engine {
    pub fn as_str(&self) -> &'static str {
        match self {
            Engine::Clickhouse => "clickhouse",
            Engine::Postgres => "postgres",
            Engine::Falkordb => "falkordb",
        }
    }
}

fn default_engine() -> Engine {
    Engine::Clickhouse
}

/// Metadata saved for each server instance.
///
/// `engine` and `container_id` are post-Postgres-support additions and default
/// to ClickHouse + None so existing `.dctl/servers/*.json` files keep
/// deserializing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    /// Active ClickHouse process PID; 0 when stopped or for Postgres.
    pub pid: u32,
    /// Running ClickHouse version like "25.12.5.44", empty when stopped,
    /// "postgres:<tag>" for Postgres, or "falkordb:v<X.Y.Z>" / "falkordb:latest"
    /// for FalkorDB — a logical display form, not a directly pullable image
    /// reference (build refs via `falkordb::fk_image_ref`).
    pub version: String,
    /// Running ClickHouse HTTP port; 0 when stopped or for Postgres.
    pub http_port: u16,
    /// Running ClickHouse TCP port, 0 when stopped, or mapped host port for Postgres.
    pub tcp_port: u16,
    pub started_at: String,
    pub cwd: String,
    #[serde(default = "default_engine")]
    pub engine: Engine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
}

/// A server entry shown in list output — may or may not be running.
pub struct ServerEntry {
    pub name: String,
    pub running: bool,
    pub info: Option<ServerInfo>,
}

/// Validate that a server name is safe for use in path operations.
/// Rejects names containing path separators, `..` components, or null bytes.
pub fn validate_server_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
        || name.contains("../")
        || name.contains("..\\")
    {
        return Err(Error::InvalidServerName(name.to_string()));
    }
    Ok(())
}

/// Directory where server tracking files and data live: .dctl/servers/
fn servers_dir() -> PathBuf {
    init::local_dir().join("servers")
}

/// The one project-wide metadata lock. Lifecycle operations hold this lock
/// from their final state read through their state-determining write. No code
/// holding it may acquire an install lock, and metadata helpers with a
/// `_locked` suffix never acquire it again.
pub(crate) struct MetadataLock {
    _file: File,
    dir: PathBuf,
}

impl MetadataLock {
    pub(crate) fn acquire_at(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|source| {
            server_lock_error(
                "create the server metadata lock directory",
                dir,
                "Check write access to the parent directory, then retry.",
                source,
            )
        })?;
        let path = dir.join(METADATA_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| {
                server_lock_error(
                    "open the server metadata lock file",
                    &path,
                    "Check read and write access to the lock file and its directory, then retry.",
                    source,
                )
            })?;
        file.lock().map_err(|source| {
            server_lock_error(
                "acquire the server metadata lock",
                &path,
                "Check that the filesystem supports advisory file locks, then retry.",
                source,
            )
        })?;
        Ok(Self {
            _file: file,
            dir: dir.to_path_buf(),
        })
    }
}

pub(crate) fn lock_metadata() -> Result<MetadataLock> {
    MetadataLock::acquire_at(&servers_dir())
}

/// Disk identifier for a Postgres instance: `<name>-pg<major>`. Used in the
/// metadata filename, the data dir name, and the container name so that
/// distinct (name, major) pairs never share state.
pub fn pg_instance_key(name: &str, major: &str) -> String {
    format!("{}-pg{}", name, major)
}

/// Join a child name onto the servers directory. Exposed so handlers can
/// remove a whole `<key>/` wrapper without poking at internals.
pub fn servers_dir_join(child: &str) -> PathBuf {
    servers_dir().join(child)
}

/// Data directory for a Postgres instance.
pub fn pg_data_dir(name: &str, major: &str) -> PathBuf {
    servers_dir()
        .join(pg_instance_key(name, major))
        .join("data")
}

/// Disk identifier for a FalkorDB instance: `<name>-fk<version>` (full
/// X.Y.Z, because the image publishes no major-only tag). Used in the
/// metadata filename, the data dir name, and the container name so that
/// distinct (name, version) pairs never share state.
pub fn fk_instance_key(name: &str, version: &str) -> String {
    format!("{}-fk{}", name, version)
}

/// The version grammar of an fk instance-key suffix: a full `X.Y.Z` (the
/// image publishes no major-only tags) or the literal `latest` (F1: both
/// spellings must be first-class, or latest instances become invisible to
/// discovery and a second start silently creates another container).
pub(crate) fn is_fk_version_suffix(version: &str) -> bool {
    version == "latest"
        || version.split('.').count() == 3
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
pub(crate) fn is_fk_instance_key(name: &str) -> bool {
    name.rsplit_once("-fk")
        .is_some_and(|(name, version)| !name.is_empty() && is_fk_version_suffix(version))
}

/// Data directory for a FalkorDB instance.
pub fn fk_data_dir(name: &str, version: &str) -> PathBuf {
    servers_dir()
        .join(fk_instance_key(name, version))
        .join("data")
}

/// Disk identifier for a Docker-managed ClickHouse instance:
/// `<name>-ch<version>`.
pub fn ch_instance_key(name: &str, version: &str) -> String {
    format!("{}-ch{}", name, version)
}

pub(crate) fn is_ch_instance_key(name: &str) -> bool {
    name.rsplit_once("-ch").is_some_and(|(name, version)| {
        !name.is_empty()
            && (version == "latest"
                || (version.split('.').count() >= 2
                    && version
                        .split('.')
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))))
    })
}

/// Data directory for a Docker-managed ClickHouse instance.
pub fn ch_data_dir(name: &str, version: &str) -> PathBuf {
    servers_dir()
        .join(ch_instance_key(name, version))
        .join("data")
}

/// Ensure the data directory for a ClickHouse instance exists.
pub fn ensure_ch_data_dir(name: &str, version: &str) -> Result<bool> {
    ensure_servers_dir()?;
    let instance_dir = servers_dir().join(ch_instance_key(name, version));
    let created = match std::fs::create_dir(&instance_dir) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = std::fs::create_dir_all(instance_dir.join("data")) {
        if created {
            let _ = std::fs::remove_dir(&instance_dir);
        }
        return Err(error.into());
    }
    Ok(created)
}

/// Find every Docker-managed ClickHouse instance named `name`.
pub(crate) fn find_ch_instances_locked(name: &str, lock: &MetadataLock) -> Result<Vec<ServerInfo>> {
    let prefix = format!("{}-ch", name);
    let dir = match std::fs::read_dir(&lock.dir) {
        Ok(d) => d,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for entry in dir {
        let entry = entry?;
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let stem = match fname.strip_suffix(".json") {
            Some(s) => s,
            None => continue,
        };
        if !stem.starts_with(&prefix) || !is_ch_instance_key(stem) {
            continue;
        }
        if let Some(info) = load_info_locked(stem, lock)?
            && info.engine == Engine::Clickhouse
            && info.container_id.is_some()
        {
            out.push(info);
        }
    }
    Ok(out)
}

/// Ensure the data directory for a FalkorDB instance exists. Returns whether
/// this call created the instance directory, for transactional startup cleanup.
pub fn ensure_fk_data_dir(name: &str, version: &str) -> Result<bool> {
    ensure_servers_dir()?;
    let instance_dir = servers_dir().join(fk_instance_key(name, version));
    let created = match std::fs::create_dir(&instance_dir) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = std::fs::create_dir_all(instance_dir.join("data")) {
        if created {
            let _ = std::fs::remove_dir(&instance_dir);
        }
        return Err(error.into());
    }
    Ok(created)
}

/// Find every FalkorDB instance whose user-facing name is `name`. Returns
/// one entry per full version that has a metadata file on disk.
pub(crate) fn find_fk_instances_locked(name: &str, lock: &MetadataLock) -> Result<Vec<ServerInfo>> {
    let prefix = format!("{}-fk", name);
    let dir = match std::fs::read_dir(&lock.dir) {
        Ok(d) => d,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for entry in dir {
        let entry = entry?;
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let stem = match fname.strip_suffix(".json") {
            Some(s) => s,
            None => continue,
        };
        if !stem.starts_with(&prefix) {
            continue;
        }
        // The suffix must be a full X.Y.Z or latest — guards against e.g.
        // `dev-fk-foo` or a bare `prod-fk2` matching when `name = "dev"`.
        let version = &stem[prefix.len()..];
        if !is_fk_version_suffix(version) {
            continue;
        }
        if let Some(info) = load_info_locked(stem, lock)?
            && info.engine == Engine::Falkordb
        {
            out.push(info);
        }
    }
    Ok(out)
}

/// Ensure the project-local server and ignore paths exist. Idempotent.
fn ensure_servers_dir() -> Result<()> {
    let dir = servers_dir();
    std::fs::create_dir_all(&dir)?;
    init::ensure_runtime_gitignore()?;
    Ok(())
}

/// Ensure the data directory for a Postgres instance exists. Returns whether
/// this call created the instance directory, for transactional startup cleanup.
pub fn ensure_pg_data_dir(name: &str, major: &str) -> Result<bool> {
    ensure_servers_dir()?;
    let instance_dir = servers_dir().join(pg_instance_key(name, major));
    let created = match std::fs::create_dir(&instance_dir) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = std::fs::create_dir_all(instance_dir.join("data")) {
        if created {
            let _ = std::fs::remove_dir(&instance_dir);
        }
        return Err(error.into());
    }
    Ok(created)
}

fn metadata_write_error(path: &Path, source: std::io::Error) -> Error {
    Error::ServerMetadataWrite {
        path: path.to_path_buf(),
        source,
    }
}

fn server_lock_error(
    operation: &'static str,
    path: &Path,
    remediation: &'static str,
    source: std::io::Error,
) -> Error {
    Error::ServerLock {
        operation,
        path: path.to_path_buf(),
        remediation,
        source,
    }
}

fn sync_directory(dir: &Path, metadata_path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| metadata_write_error(metadata_path, error))?;
    Ok(())
}

fn save_server_info_at(dir: &Path, info: &ServerInfo) -> Result<()> {
    save_server_info_at_with_sync(dir, info, sync_directory)
}

fn save_server_info_at_with_sync(
    dir: &Path,
    info: &ServerInfo,
    sync: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<()> {
    let path = dir.join(format!("{}.json", info.name));
    let json = serde_json::to_vec_pretty(info)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(METADATA_TEMP_PREFIX)
        .tempfile_in(dir)
        .map_err(|error| metadata_write_error(&path, error))?;
    temporary
        .write_all(&json)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| metadata_write_error(&path, error))?;
    temporary
        .persist(&path)
        .map_err(|error| metadata_write_error(&path, error.error))?;
    // The rename has committed metadata at this point. A directory sync error
    // must not make callers treat the child as untracked and terminate it.
    let _ = sync(dir, &path);
    Ok(())
}

pub(crate) fn save_server_info_locked(info: &ServerInfo, lock: &MetadataLock) -> Result<()> {
    validate_server_name(&info.name)?;
    save_server_info_at(&lock.dir, info)
}

pub(crate) fn try_remove_server_info_locked(name: &str, lock: &MetadataLock) -> Result<()> {
    let path = lock.dir.join(format!("{name}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(metadata_write_error(&path, error)),
    }
    sync_directory(&lock.dir, &path)
}

/// Mark a ClickHouse server as stopped without discarding its metadata.
///
/// The PID match avoids overwriting metadata when a newer process was already
/// recorded before this transition began.
pub(crate) fn mark_server_stopped_locked(name: &str, pid: u32, lock: &MetadataLock) -> Result<()> {
    let Some(mut info) = load_info_locked(name, lock)? else {
        return Ok(());
    };
    if info.engine == Engine::Clickhouse && info.pid == pid {
        info.pid = 0;
        info.version.clear();
        info.http_port = 0;
        info.tcp_port = 0;
        save_server_info_locked(&info, lock)?;
    }
    Ok(())
}

/// Engine-aware liveness check.
fn is_alive(info: &ServerInfo) -> Result<bool> {
    match info.container_id.as_deref() {
        // Docker-managed (all new instances): ask the daemon.
        Some(id) => docker::is_container_running_blocking(id),
        // Legacy binary-era entries have no container; dctl can no longer
        // manage them, so they read as stopped. `server remove` still clears
        // the metadata and data directory.
        None => Ok(false),
    }
}

fn load_info_at(path: &Path) -> Result<Option<ServerInfo>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(Error::ServerMetadataPermission {
                path: path.to_path_buf(),
                source,
            });
        }
        Err(source) => {
            return Err(Error::ServerMetadataRead {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let content = String::from_utf8(bytes).map_err(|source| Error::ServerMetadataUtf8 {
        path: path.to_path_buf(),
        source,
    })?;
    let info = serde_json::from_str(&content).map_err(|source| Error::ServerMetadataParse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(info))
}

pub(crate) fn load_info_locked(name: &str, lock: &MetadataLock) -> Result<Option<ServerInfo>> {
    validate_server_name(name)?;
    load_info_at(&lock.dir.join(format!("{name}.json")))
}

/// Find every Postgres instance whose user-facing name is `name`. Returns
/// one entry per major version that has a metadata file on disk.
pub(crate) fn find_pg_instances_locked(name: &str, lock: &MetadataLock) -> Result<Vec<ServerInfo>> {
    let prefix = format!("{}-pg", name);
    let dir = match std::fs::read_dir(&lock.dir) {
        Ok(d) => d,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for entry in dir {
        let entry = entry?;
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let stem = match fname.strip_suffix(".json") {
            Some(s) => s,
            None => continue,
        };
        if !stem.starts_with(&prefix) {
            continue;
        }
        // Major must be all digits to match — guards against e.g. `dev-pg-foo`
        // matching when `name = "dev"`.
        let major = &stem[prefix.len()..];
        if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Some(info) = load_info_locked(stem, lock)?
            && info.engine == Engine::Postgres
        {
            out.push(info);
        }
    }
    Ok(out)
}

/// Load server metadata only if the underlying process/container is alive.
/// Does not update stale metadata. `list_all_servers` is the single place that
/// marks ClickHouse entries stopped when their PID is gone, so callers like
/// `is_server_running` and `resolve_name` can read metadata without side effects.
fn load_running_info_locked(name: &str, lock: &MetadataLock) -> Result<Option<ServerInfo>> {
    let Some(info) = load_info_locked(name, lock)? else {
        return Ok(None);
    };
    if is_alive(&info)? {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

/// List all known servers (both running and stopped).
///
/// Scans `.dctl/servers/*.json` for metadata. Each metadata file is one
/// entry — for ClickHouse the disk id is the user-facing name; for Postgres
/// it's `<name>-pg<major>`. Also runs process/container discovery so
/// orphaned instances reappear.
pub fn list_all_servers() -> Result<Vec<ServerEntry>> {
    let lock = lock_metadata()?;
    recover_current_project_servers_locked(&lock)?;
    list_all_servers_locked(&lock)
}

pub(crate) fn list_all_servers_locked(lock: &MetadataLock) -> Result<Vec<ServerEntry>> {
    list_all_servers_locked_inner(lock, false)
}

fn list_all_servers_locked_inner(
    lock: &MetadataLock,
    skip_entry_errors: bool,
) -> Result<Vec<ServerEntry>> {
    let dir = &lock.dir;
    let mut entries = Vec::new();

    let dir_entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(error.into()),
    };

    for entry in dir_entries {
        let entry = entry?;
        if !entry.path().is_file() {
            continue;
        }
        let fname = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let stem = match fname.strip_suffix(".json") {
            Some(s) => s,
            None => continue,
        };
        let entry = match server_entry_locked(stem, lock) {
            Ok(entry) => entry,
            Err(_) if skip_entry_errors => continue,
            Err(error) => return Err(error),
        };
        let Some(entry) = entry else {
            // The file was removed after read_dir; absence is not corruption
            // and must not produce a phantom stopped entry.
            continue;
        };
        entries.push(entry);
    }

    entries.sort_by(|a, b| b.running.cmp(&a.running).then(a.name.cmp(&b.name)));
    Ok(entries)
}

pub(crate) fn server_entry_locked(name: &str, lock: &MetadataLock) -> Result<Option<ServerEntry>> {
    server_entry_locked_with(name, lock, || {})
}

fn server_entry_locked_with(
    name: &str,
    lock: &MetadataLock,
    before_stale_write: impl FnOnce(),
) -> Result<Option<ServerEntry>> {
    let Some(mut info) = load_info_locked(name, lock)? else {
        return Ok(None);
    };
    let mut running = is_alive(&info)?;

    // Keep the lock across liveness, comparison, and replacement. A restart
    // either commits before this read or waits and commits after normalization.
    if !running && info.engine == Engine::Clickhouse && info.pid != 0 {
        before_stale_write();
        mark_server_stopped_locked(name, info.pid, lock)?;
        info =
            load_info_locked(name, lock)?.ok_or_else(|| Error::ServerNotFound(name.to_string()))?;
        running = is_alive(&info)?;
    }

    Ok(Some(ServerEntry {
        name: name.to_string(),
        running,
        info: Some(info),
    }))
}

pub(crate) fn list_running_servers_locked(lock: &MetadataLock) -> Result<Vec<ServerInfo>> {
    Ok(list_all_servers_locked(lock)?
        .into_iter()
        .filter(|entry| entry.running)
        .filter_map(|entry| entry.info)
        .collect())
}

pub(crate) fn is_server_running_locked(name: &str, lock: &MetadataLock) -> Result<bool> {
    Ok(load_running_info_locked(name, lock)?.is_some())
}

/// Stop a running server by name.
///
/// * ClickHouse: SIGTERM (then SIGKILL on timeout); metadata is retained with
///   PID 0 so the stopped instance remains discoverable.
pub(crate) fn kill_server_locked(name: &str, lock: &MetadataLock) -> Result<()> {
    let info = load_running_info_locked(name, lock)?
        .ok_or_else(|| Error::ServerNotRunning(name.to_string()))?;

    let id = info.container_id.as_deref().ok_or_else(|| {
        Error::DockerError(format!("server '{}' has no container_id in metadata", name))
    })?;
    // Stop the container only; metadata + container are preserved so `start`
    // can resume with the same credentials and data.
    docker::stop_blocking(id)?;
    Ok(())
}

pub(crate) fn generate_random_name_locked(lock: &MetadataLock) -> Result<String> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mixed = seed ^ (std::process::id() as u128);
    let adj = ADJECTIVES[(mixed % ADJECTIVES.len() as u128) as usize];
    let noun = NOUNS[((mixed / ADJECTIVES.len() as u128) % NOUNS.len() as u128) as usize];
    let tag = format!("{}-{}", adj, noun);

    unique_generated_name_locked(&tag, lock)
}

fn unique_generated_name_locked(tag: &str, lock: &MetadataLock) -> Result<String> {
    if load_info_locked(tag, lock)?.is_none() && find_pg_instances_locked(tag, lock)?.is_empty() {
        return Ok(tag.to_string());
    }
    for i in 2_u64.. {
        let candidate = format!("{}-{}", tag, i);
        if load_info_locked(&candidate, lock)?.is_none()
            && find_pg_instances_locked(&candidate, lock)?.is_empty()
        {
            return Ok(candidate);
        }
    }
    unreachable!("a finite metadata directory cannot exhaust generated names")
}

/// Format a timestamp for now.
pub fn now_timestamp() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", duration.as_secs())
}

/// Recover orphaned engine containers for the current project via Docker
/// labels: a container whose `.dctl/servers/<key>.json` metadata file is
/// missing gets one written, so the instance shows up in `server list` and
/// can be stopped/removed normally.
///
/// Covers all three engines. Safe to call repeatedly in one invocation; when
/// Docker is unreachable the recovery is skipped silently (the per-engine
/// helpers return `Ok(())` on connect failure).
pub(crate) fn recover_current_project_servers_locked(lock: &MetadataLock) -> Result<()> {
    let project_cwd = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    docker::recover_project_postgres_blocking(&project_cwd, lock)?;
    docker::recover_project_falkor_blocking(&project_cwd, lock)?;
    docker::recover_project_clickhouse_blocking(&project_cwd, lock)?;
    Ok(())
}

#[cfg(test)]
mod fk_key_tests {
    use super::*;

    fn temp_lock() -> (tempfile::TempDir, MetadataLock) {
        let dir = tempfile::tempdir().expect("temp servers dir");
        let lock = MetadataLock::acquire_at(dir.path()).expect("acquire lock");
        (dir, lock)
    }

    fn info(key: &str, engine: Engine) -> ServerInfo {
        ServerInfo {
            name: key.to_string(),
            pid: 0,
            version: format!(
                "{}:v{}",
                engine.as_str(),
                key.rsplit("-fk").next().unwrap_or("")
            ),
            http_port: 0,
            tcp_port: 6379,
            started_at: "test".into(),
            cwd: "/tmp".into(),
            engine,
            container_id: Some("cid".into()),
        }
    }

    #[test]
    fn fk_version_suffix_accepts_full_versions_and_latest_only() {
        assert!(is_fk_version_suffix("4.20.6"));
        assert!(is_fk_version_suffix("latest"));
        // Bare numbers and partials are not fk keys (G8 tightening).
        assert!(!is_fk_version_suffix("2"));
        assert!(!is_fk_version_suffix("4.20"));
        assert!(!is_fk_version_suffix(""));
        assert!(!is_fk_version_suffix("4.20.6-alpine"));
    }

    #[test]
    fn fk_instance_keys_cover_latest() {
        assert!(is_fk_instance_key("default-fk4.20.6"));
        assert!(is_fk_instance_key("default-fklatest"));
        assert!(!is_fk_instance_key("default-fk"));
        assert!(!is_fk_instance_key("prod-fk2"));
        assert!(!is_fk_instance_key("plain"));
    }

    /// F1 regression: a `latest` instance must be discoverable by name, or a
    /// second `start` silently creates another container under the default tag.
    #[test]
    fn find_fk_instances_returns_latest_instances() {
        let (_dir, lock) = temp_lock();
        save_server_info_locked(&info("default-fklatest", Engine::Falkordb), &lock).unwrap();
        let found = find_fk_instances_locked("default", &lock).unwrap();
        assert_eq!(found.len(), 1, "the latest instance must be found");
        assert_eq!(found[0].name, "default-fklatest");
        // A different name sees nothing.
        assert!(find_fk_instances_locked("other", &lock).unwrap().is_empty());
    }

    #[test]
    fn fk_latest_does_not_masquerade_as_clickhouse_directory() {
        // The CH legacy-name scan must keep excluding fk keys, latest included.
        assert!(is_fk_instance_key("dev-fklatest"));
        assert!(crate::local::falkordb::user_name_from_key("dev-fklatest") == "dev");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info(pid: u32, version: &str) -> ServerInfo {
        ServerInfo {
            name: "default".into(),
            pid,
            version: version.into(),
            http_port: 8123,
            tcp_port: 9000,
            started_at: "1700000000".into(),
            cwd: "/tmp/project".into(),
            engine: Engine::Clickhouse,
            container_id: None,
        }
    }

    #[test]
    fn engine_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Engine::Clickhouse).unwrap(),
            "\"clickhouse\""
        );
        assert_eq!(
            serde_json::to_string(&Engine::Postgres).unwrap(),
            "\"postgres\""
        );
    }

    #[test]
    fn server_info_legacy_json_deserializes_as_clickhouse() {
        // Legacy JSON written before the engine field existed.
        let legacy = r#"{
            "name": "default",
            "pid": 12345,
            "version": "25.12.5.44",
            "http_port": 8123,
            "tcp_port": 9000,
            "started_at": "1700000000",
            "cwd": "/tmp/proj"
        }"#;
        let info: ServerInfo = serde_json::from_str(legacy).expect("legacy JSON should parse");
        assert_eq!(info.engine, Engine::Clickhouse);
        assert!(info.container_id.is_none());
    }

    #[test]
    fn server_info_postgres_round_trip() {
        let info = ServerInfo {
            name: "dev".into(),
            pid: 0,
            version: "postgres:17".into(),
            http_port: 0,
            tcp_port: 5432,
            started_at: "1700000000".into(),
            cwd: "/tmp/proj".into(),
            engine: Engine::Postgres,
            container_id: Some("abc123".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: ServerInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.engine, Engine::Postgres);
        assert_eq!(parsed.container_id.as_deref(), Some("abc123"));
        assert!(json.contains("\"engine\":\"postgres\""));
    }

    #[test]
    fn generated_name_avoids_postgres_metadata_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        let mut info = test_info(0, "postgres:18");
        info.name = pg_instance_key("calm-bird", "18");
        info.engine = Engine::Postgres;
        info.container_id = Some("stopped-container".into());
        save_server_info_locked(&info, &lock).unwrap();

        assert_eq!(
            unique_generated_name_locked("calm-bird", &lock).unwrap(),
            "calm-bird-2"
        );
    }

    #[test]
    fn selected_metadata_reports_partial_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.json");
        std::fs::write(&path, br#"{"name":"default","pid":12"#).unwrap();

        let error = load_info_at(&path).unwrap_err();
        assert!(matches!(error, Error::ServerMetadataParse { .. }));
        assert!(error.to_string().contains("not valid JSON"));
        assert!(error.to_string().contains("default.json"));
    }

    #[test]
    fn selected_metadata_reports_invalid_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.json");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();

        let error = load_info_at(&path).unwrap_err();
        assert!(matches!(error, Error::ServerMetadataUtf8 { .. }));
        assert!(error.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn listing_ignores_json_directories() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.json");
        std::fs::create_dir(&path).unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();

        assert!(list_all_servers_locked(&lock).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn selected_metadata_reports_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.json");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let error = load_info_at(&path).unwrap_err();
        let Error::ServerMetadataPermission { source, .. } = error else {
            panic!("expected metadata permission error");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn metadata_lock_directory_failure_is_actionable() {
        let directory = tempfile::tempdir().unwrap();
        let lock_directory = directory.path().join("servers");
        std::fs::write(&lock_directory, b"not a directory").unwrap();

        let error = match MetadataLock::acquire_at(&lock_directory) {
            Ok(_) => panic!("metadata lock acquisition unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(std::error::Error::source(&error).is_some());
        assert!(matches!(
            error,
            Error::ServerLock {
                operation: "create the server metadata lock directory",
                path,
                source,
                ..
            } if path == lock_directory
                && source.kind() == std::io::ErrorKind::AlreadyExists
        ));
    }

    #[test]
    fn atomic_save_ignores_interrupted_sibling_temp_files() {
        let directory = tempfile::tempdir().unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        let stale_temp = directory.path().join(".metadata-interrupted-write");
        std::fs::write(&stale_temp, br#"{"name":"default""#).unwrap();

        save_server_info_locked(&test_info(0, "25.12.1.1"), &lock).unwrap();
        let entries = list_all_servers_locked(&lock).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "default");
        assert_eq!(entries[0].info.as_ref().unwrap().version, "25.12.1.1");
        assert_eq!(
            std::fs::read_to_string(stale_temp).unwrap(),
            r#"{"name":"default""#
        );
    }

    #[test]
    fn directory_sync_failure_after_persist_keeps_committed_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let info = test_info(0, "committed");

        save_server_info_at_with_sync(directory.path(), &info, |_, path| {
            Err(metadata_write_error(
                path,
                std::io::Error::other("injected directory sync failure"),
            ))
        })
        .unwrap();

        let stored = load_info_at(&directory.path().join("default.json"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.version, "committed");
    }

    #[test]
    fn concurrent_unlocked_readers_never_observe_partial_json() {
        let directory = tempfile::tempdir().unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        save_server_info_locked(&test_info(0, "initial"), &lock).unwrap();
        drop(lock);
        let writer_dir = directory.path().to_path_buf();
        let metadata_path = directory.path().join("default.json");

        let writer = std::thread::spawn(move || {
            for generation in 0..200 {
                let lock = MetadataLock::acquire_at(&writer_dir).unwrap();
                save_server_info_locked(&test_info(0, &format!("generation-{generation}")), &lock)
                    .unwrap();
            }
        });
        for _ in 0..1_000 {
            let bytes = std::fs::read(&metadata_path).unwrap();
            serde_json::from_slice::<ServerInfo>(&bytes).unwrap();
        }
        writer.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn metadata_write_permission_failure_is_not_discarded() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        save_server_info_locked(&test_info(0, "preserved"), &lock).unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let error = save_server_info_locked(&test_info(0, "blocked"), &lock).unwrap_err();
        assert!(matches!(error, Error::ServerMetadataWrite { .. }));

        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            load_info_locked("default", &lock).unwrap().unwrap().version,
            "preserved"
        );
    }

    #[test]
    fn stale_pid_normalization_is_durable() {
        let directory = tempfile::tempdir().unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        save_server_info_locked(&test_info(u32::MAX, "25.12.1.1"), &lock).unwrap();

        let entry = server_entry_locked("default", &lock).unwrap().unwrap();
        let normalized = entry.info.unwrap();
        assert!(!entry.running);
        assert_eq!(normalized.pid, 0);
        assert!(normalized.version.is_empty());
        assert_eq!(load_info_locked("default", &lock).unwrap().unwrap().pid, 0);
    }

    #[test]
    fn restart_waiting_during_normalization_commits_last() {
        let directory = tempfile::tempdir().unwrap();
        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        save_server_info_locked(&test_info(u32::MAX, "stale"), &lock).unwrap();
        let restart_dir = directory.path().to_path_buf();
        let mut restart = None;

        let normalized = server_entry_locked_with("default", &lock, || {
            restart = Some(std::thread::spawn(move || {
                let restart_lock = MetadataLock::acquire_at(&restart_dir).unwrap();
                save_server_info_locked(&test_info(std::process::id(), "restarted"), &restart_lock)
                    .unwrap();
            }));
        })
        .unwrap()
        .unwrap();
        assert_eq!(normalized.info.unwrap().pid, 0);
        drop(lock);
        restart.unwrap().join().unwrap();

        let lock = MetadataLock::acquire_at(directory.path()).unwrap();
        let final_info = load_info_locked("default", &lock).unwrap().unwrap();
        assert_eq!(final_info.pid, std::process::id());
        assert_eq!(final_info.version, "restarted");
    }
}
