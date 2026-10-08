use crate::error::{Error, Result};
use crate::paths;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tar::Archive;

const GITHUB_REPO: &str = "raystyle/dctl_rs";
const RELEASES_BASE_URL: &str = "https://github.com/raystyle/dctl_rs/releases/download";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60; // 24 hours

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum UpdateAction {
    UpToDate,
    UpdateAvailable,
    Updated,
}

#[derive(Serialize)]
pub struct UpdateResult {
    current_version: String,
    latest_version: String,
    action: UpdateAction,
}

impl UpdateResult {
    fn checked(current: &str, latest: &str) -> Self {
        Self {
            current_version: current.to_owned(),
            latest_version: latest.strip_prefix('v').unwrap_or(latest).to_owned(),
            action: if is_newer(current, latest) {
                UpdateAction::UpdateAvailable
            } else {
                UpdateAction::UpToDate
            },
        }
    }

    pub fn write(&self, output: &mut dyn Write, json: bool) -> Result<()> {
        if json {
            writeln!(output, "{}", serde_json::to_string_pretty(self)?)?;
        } else {
            match self.action {
                UpdateAction::UpToDate => {
                    writeln!(output, "Already up to date (v{}).", self.current_version)?;
                }
                UpdateAction::UpdateAvailable => {
                    writeln!(
                        output,
                        "Update available: v{} → v{}",
                        self.current_version, self.latest_version
                    )?;
                    writeln!(output, "Run `dctl update` to upgrade.")?;
                }
                UpdateAction::Updated => {
                    writeln!(
                        output,
                        "Updated dctl: v{} → v{}",
                        self.current_version, self.latest_version
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// The platform target triple used in release artifact names.
fn target_triple() -> Result<&'static str> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        _ => Err(Error::UnsupportedPlatform {
            os: os.to_string(),
            arch: arch.to_string(),
        }),
    }
}

/// Parse a version tag like "v0.1.17" into a comparable tuple.
fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() == 3 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}

/// Returns true if `latest` is newer than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Fetch the latest release info from GitHub with configurable timeout.
async fn fetch_latest_release(timeout: std::time::Duration) -> Result<GitHubRelease> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        GITHUB_REPO
    );
    let client = crate::http::client_builder().timeout(timeout).build()?;

    let response = client
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| Error::Download(format!("GitHub API request failed: {}", e)))?;

    let release: GitHubRelease = response.json().await?;
    Ok(release)
}

/// Extract the `dctl` binary from a `.tar.gz` release archive.
///
/// The release workflow packages the binary at
/// `dctl-<target>-v<version>/dctl` inside the tarball, so we
/// match on the entry's file name rather than the full path.
/// Verify the downloaded archive against the release's SHA256SUMS asset.
/// Mismatch (or a sums file without an entry for this asset) is a hard
/// failure; a 404 means a pre-checksum-era release and degrades to a
/// warning so the update path stays usable across the transition.
async fn verify_release_checksum(
    client: &reqwest::Client,
    tag: &str,
    archive_name: &str,
    archive_bytes: &[u8],
) -> Result<()> {
    let sums_url = format!("{}/{}/SHA256SUMS", RELEASES_BASE_URL, tag);
    let response = client.get(&sums_url).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        // Version floor (fix-review G1, closed with v0.8.0): releases from
        // v0.8.0 on always publish SHA256SUMS, so a missing asset there is
        // tampering or a broken release, never "skip verification".
        // Pre-floor releases genuinely lack the asset and stay a warning.
        // Transport errors are not 404 and are not passed through.
        const CHECKSUM_FLOOR: (u32, u32, u32) = (0, 8, 0);
        if parse_version(tag).is_some_and(|v| v >= CHECKSUM_FLOOR) {
            return Err(Error::Download(format!(
                "release {tag} publishes no SHA256SUMS asset; refusing to update \
                 without integrity verification"
            )));
        }
        eprintln!(
            "warning: release {tag} publishes no SHA256SUMS asset (pre-v0.8.0); \
             continuing without integrity verification"
        );
        return Ok(());
    }
    let response = response
        .error_for_status()
        .map_err(|e| Error::Download(format!("Could not fetch SHA256SUMS: {}", e)))?;
    let sums = response.text().await?;
    let expected = find_expected_digest(&sums, archive_name).ok_or_else(|| {
        Error::Download(format!(
            "SHA256SUMS for {tag} has no entry for {archive_name}; refusing to update"
        ))
    })?;
    use sha2::{Digest, Sha256};
    let actual_hex: String = Sha256::digest(archive_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual_hex != expected {
        return Err(Error::Download(format!(
            "checksum mismatch for {archive_name}: expected {expected}, got {actual_hex}; \
             refusing to update"
        )));
    }
    Ok(())
}

/// Find this asset's line in a `sha256sum`-format sums file (digest, two
/// spaces, filename); the digest comes back lowercased.
fn find_expected_digest(sums: &str, archive_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.split_once(char::is_whitespace)?;
        name.trim()
            .eq_ignore_ascii_case(archive_name)
            .then(|| digest.trim().to_ascii_lowercase())
    })
}

/// Upper bound on the decompressed binary the updater will accept; a
/// malformed or bomb archive fails here instead of exhausting memory.
const MAX_EXTRACTED_BINARY_BYTES: u64 = 256 * 1024 * 1024;

fn extract_binary_from_archive(archive_bytes: &[u8]) -> Result<Vec<u8>> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = Archive::new(decoder);

    for entry in archive
        .entries()
        .map_err(|e| Error::Extract(format!("Failed to read release archive: {}", e)))?
    {
        let mut entry =
            entry.map_err(|e| Error::Extract(format!("Failed to read archive entry: {}", e)))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| Error::Extract(format!("Failed to read archive entry path: {}", e)))?;
        if path.file_name().and_then(|n| n.to_str()) == Some("dctl") {
            if entry.header().size().unwrap_or(u64::MAX) > MAX_EXTRACTED_BINARY_BYTES {
                return Err(Error::Extract(format!(
                    "release archive entry exceeds the {} byte limit; refusing to extract",
                    MAX_EXTRACTED_BINARY_BYTES
                )));
            }
            let mut buf = Vec::new();
            let mut limited = std::io::Read::take(&mut entry, MAX_EXTRACTED_BINARY_BYTES + 1);
            io::copy(&mut limited, &mut buf).map_err(|e| {
                Error::Extract(format!("Failed to extract binary from archive: {}", e))
            })?;
            if buf.len() as u64 > MAX_EXTRACTED_BINARY_BYTES {
                return Err(Error::Extract(
                    "release archive entry exceeds the extraction limit; refusing to extract"
                        .into(),
                ));
            }
            return Ok(buf);
        }
    }

    Err(Error::Extract(
        "Release archive did not contain a dctl binary".into(),
    ))
}

/// Timeout for explicit user-initiated commands (update, update --check).
const EXPLICIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Timeout for the implicit background cache refresh.
const BACKGROUND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

/// Check for updates and retain both versions even when no update is available.
/// Uses the explicit (longer) timeout since this is called from user-initiated commands.
pub async fn check_for_update() -> Result<UpdateResult> {
    let current = env!("CARGO_PKG_VERSION");
    let release = fetch_latest_release(EXPLICIT_TIMEOUT).await?;
    let latest = &release.tag_name;
    let display = latest.strip_prefix('v').unwrap_or(latest);

    // An explicit check always refreshes the cache and resets the staleness
    // timer, so subsequent commands reflect what we just learned.
    let _ = save_update_check(display);

    Ok(UpdateResult::checked(current, latest))
}

/// Download the latest release and replace the current binary.
pub async fn perform_update(json: bool) -> Result<UpdateResult> {
    let current = env!("CARGO_PKG_VERSION");
    let release = fetch_latest_release(EXPLICIT_TIMEOUT).await?;
    let latest = &release.tag_name;
    let mut result = UpdateResult::checked(current, latest);

    if matches!(result.action, UpdateAction::UpToDate) {
        let display = latest.strip_prefix('v').unwrap_or(latest);
        // Refresh the cache with the network truth so a stale "update available"
        // notice can't keep nagging after the user explicitly checked.
        let _ = save_update_check(display);
        return Ok(result);
    }

    let target = target_triple()?;
    let archive_name = format!("dctl-{}-{}.tar.gz", target, latest);
    // GitHub release asset URL: releases/download/<tag>/<archive>.
    let download_url = format!("{}/{}/{}", RELEASES_BASE_URL, latest, archive_name);

    let display = latest.strip_prefix('v').unwrap_or(latest);
    if !json {
        println!("Downloading dctl v{}...", display);
    }

    let client = crate::http::client_builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let response = client
        .get(&download_url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| Error::Download(format!("Download failed: {}", e)))?;

    let archive_bytes = response.bytes().await?;

    // Integrity: the release carries a SHA256SUMS asset next to the
    // archives. A present-but-mismatched checksum aborts before anything is
    // extracted or written; releases from before the checksum era (no asset
    // at all) proceed with a loud warning rather than bricking `dctl update`.
    verify_release_checksum(&client, latest, &archive_name, &archive_bytes).await?;

    let binary_bytes = extract_binary_from_archive(&archive_bytes)?;

    // Get the path to the currently running binary
    let current_exe = std::env::current_exe().map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("Could not determine current executable path: {}", e),
        ))
    })?;

    // Resolve symlinks to get the actual binary path
    let actual_path = fs::canonicalize(&current_exe).unwrap_or(current_exe);

    // Write to a temporary file next to the binary, then atomic-rename
    let tmp_path = actual_path.with_extension("tmp-update");
    fs::write(&tmp_path, &binary_bytes).map_err(|e| {
        // Same cleanup contract as the two arms below: a failed write may
        // leave a partial file next to the binary.
        let _ = fs::remove_file(&tmp_path);
        Error::Download(format!(
            "Failed to write update to {}: {}. Check file permissions.",
            tmp_path.display(),
            e
        ))
    })?;

    // Make it executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755)).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            Error::Download(format!("Failed to set executable permissions: {}", e))
        })?;
    }

    // Atomic rename
    fs::rename(&tmp_path, &actual_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        Error::Download(format!(
            "Failed to replace binary at {}: {}. Check file permissions.",
            actual_path.display(),
            e
        ))
    })?;

    // Clear the check cache so the update notice disappears immediately.
    let _ = clear_update_check();
    result.action = UpdateAction::Updated;
    Ok(result)
}

// --- Background update check with caching ---

fn update_check_path() -> Result<PathBuf> {
    Ok(paths::base_dir()?.join("last_update_check"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Save the update check result (timestamp + latest version).
fn save_update_check(latest_version: &str) -> Result<()> {
    let path = update_check_path()?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let content = format!("{}\n{}", now_secs(), latest_version);
    fs::write(&path, content)?;
    Ok(())
}

/// Read the cached update check. Returns Some((timestamp, latest_version)) if valid.
fn read_update_check() -> Option<(u64, String)> {
    let path = update_check_path().ok()?;
    let content = fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let ts: u64 = lines.next()?.parse().ok()?;
    let version = lines.next()?.to_string();
    Some((ts, version))
}

/// Remove the cached update check. Used after a successful self-update so the
/// notice disappears immediately. Missing file is not an error.
fn clear_update_check() -> Result<()> {
    let path = update_check_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Whether the cache is stale enough to warrant a network refresh. A missing
/// cache is always stale; a present one is stale once it is older than the
/// check interval.
fn cache_is_stale(cache: Option<(u64, String)>, now: u64) -> bool {
    match cache {
        Some((ts, _)) => now.saturating_sub(ts) >= CHECK_INTERVAL_SECS,
        None => true,
    }
}

/// Print an update notice from cached data only. No network, no async.
/// Called at the very end of the command, after its output, so the notice
/// never interleaves with command output or JSON.
pub fn print_cached_update_notice() {
    if let Some((_, cached_version)) = read_update_check() {
        let current = env!("CARGO_PKG_VERSION");
        if is_newer(current, &cached_version) {
            use std::io::Write;
            // Not `eprintln!`, which panics on a closed stderr.
            let _ = writeln!(
                std::io::stderr(),
                "\nThere is a new version of dctl. Update with `dctl update`."
            );
        }
    }
}

/// Hit the network, refresh the cache, and reset the staleness timer. Never
/// prints. On any failure (timeout, network error, etc.) the timestamp is still
/// reset so we back off for another 24h, but a previously-cached "update
/// available" version is preserved so we don't hide a known update.
async fn do_refresh_update_cache(timeout: std::time::Duration) {
    let current = env!("CARGO_PKG_VERSION");
    match fetch_latest_release(timeout).await {
        Ok(r) => {
            let latest = r.tag_name;
            let display = latest.strip_prefix('v').unwrap_or(&latest);
            let _ = save_update_check(display);
        }
        Err(_) => {
            // Preserve any previously-cached latest version; fall back to the
            // current version when there is nothing cached yet.
            let version = read_update_check()
                .map(|(_, v)| v)
                .unwrap_or_else(|| current.to_string());
            let _ = save_update_check(&version);
        }
    }
}

/// Refresh the update cache in the background if stale. Never prints. Skips the
/// network entirely when the cache is still fresh (within 24h).
pub async fn refresh_update_cache() {
    if !cache_is_stale(read_update_check(), now_secs()) {
        return;
    }
    do_refresh_update_cache(BACKGROUND_TIMEOUT).await;
}

/// Force a network check and refresh the cache + timer regardless of staleness.
/// Used by explicit user actions (e.g. `--version`) that should always reflect
/// the freshest state. Uses the longer explicit timeout. Never prints.
pub async fn force_refresh_update_cache() {
    do_refresh_update_cache(EXPLICIT_TIMEOUT).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256sums_entry_lookup_matches_this_asset_only() {
        let sums = "\
b3a1… dctl-x86_64-unknown-linux-musl-v0.8.0.tar.gz\n\
7f09d1c2aa  dctl-aarch64-apple-darwin-v0.8.0.tar.gz\n\
0000  dctl-x86_64-pc-windows-gnu-v0.8.0.zip\n";
        assert_eq!(
            find_expected_digest(sums, "dctl-aarch64-apple-darwin-v0.8.0.tar.gz"),
            Some("7f09d1c2aa".to_string()),
            "the asset's own line wins regardless of separators"
        );
        assert_eq!(
            find_expected_digest(sums, "DCTL-X86_64-UNKNOWN-LINUX-MUSL-V0.8.0.TAR.GZ"),
            Some("b3a1…".to_string()),
            "name matching is case-insensitive"
        );
        assert_eq!(
            find_expected_digest(sums, "dctl-sparc-unknown-linux-gnu-v0.8.0.tar.gz"),
            None,
            "an absent asset yields no digest (hard failure upstream)"
        );
    }

    #[test]
    fn json_update_check_preserves_the_current_and_actual_latest_versions() {
        for (latest, action) in [
            ("v0.5.0", "update_available"),
            ("v0.4.2", "up_to_date"),
            ("v0.4.1", "up_to_date"),
        ] {
            let result = UpdateResult::checked("0.4.2", latest);
            let mut output = Vec::new();
            result.write(&mut output, true).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value["current_version"], "0.4.2");
            assert_eq!(value["latest_version"], latest.trim_start_matches('v'));
            assert_eq!(value["action"], action);
        }
    }

    #[test]
    fn json_update_completion_reports_the_upgrade() {
        let mut result = UpdateResult::checked("0.4.2", "v0.5.0");
        result.action = UpdateAction::Updated;
        let mut output = Vec::new();
        result.write(&mut output, true).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["action"], "updated");
        assert_eq!(value["current_version"], "0.4.2");
        assert_eq!(value["latest_version"], "0.5.0");
    }

    #[test]
    fn checksum_floor_rejects_modern_tags_without_sums() {
        // The floor is a pure tag comparison on the 404 path; exercising it
        // through HTTP needs a release-shaped server, so pin the decision
        // helper the same way the arm reads it.
        let above = parse_version("v0.8.0").unwrap() >= (0, 8, 0);
        let below = parse_version("v0.7.0").unwrap() >= (0, 8, 0);
        assert!(above, "v0.8.0 is at or above the floor");
        assert!(!below, "v0.7.0 stays below the floor (warning path)");
    }

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("v0.1.17"), Some((0, 1, 17)));
        assert_eq!(parse_version("0.1.17"), Some((0, 1, 17)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.1.17", "v0.2.0"));
        assert!(is_newer("0.1.17", "0.1.18"));
        assert!(is_newer("0.1.17", "1.0.0"));
        assert!(!is_newer("0.1.17", "0.1.17"));
        assert!(!is_newer("0.1.17", "0.1.16"));
        assert!(!is_newer("0.2.0", "0.1.99"));
    }

    #[test]
    fn test_cache_is_stale() {
        let now = 1_000_000;
        // Missing cache is always stale.
        assert!(cache_is_stale(None, now));
        // Fresh cache (just written) is not stale.
        assert!(!cache_is_stale(Some((now, "0.2.0".into())), now));
        // Cache one second short of the interval is not stale.
        assert!(!cache_is_stale(
            Some((now - (CHECK_INTERVAL_SECS - 1), "0.2.0".into())),
            now
        ));
        // Cache exactly at the interval is stale.
        assert!(cache_is_stale(
            Some((now - CHECK_INTERVAL_SECS, "0.2.0".into())),
            now
        ));
        // Older cache is stale.
        assert!(cache_is_stale(
            Some((now - 2 * CHECK_INTERVAL_SECS, "0.2.0".into())),
            now
        ));
    }

    #[test]
    fn test_target_triple() {
        // Should return something valid on macOS/Linux test hosts
        let target = target_triple().unwrap();
        assert!(target.contains('-'));
    }

    fn build_release_archive(inner_dir: &str, binary_bytes: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use tar::Builder;

        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_path(format!("{}/dctl", inner_dir)).unwrap();
        header.set_size(binary_bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, binary_bytes).unwrap();

        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_dctl_binary_from_release_archive() {
        let payload = b"\x7fELF fake binary contents".as_slice();
        let archive = build_release_archive("dctl-aarch64-apple-darwin-v0.0.1", payload);

        let extracted = extract_binary_from_archive(&archive).unwrap();
        assert_eq!(extracted, payload);
    }

    #[test]
    fn extract_fails_when_archive_has_no_dctl_entry() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use tar::Builder;
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let builder = Builder::new(encoder);
        let empty = builder.into_inner().unwrap().finish().unwrap();

        let err = extract_binary_from_archive(&empty).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("did not contain"), "got: {}", msg);
    }
}
