//! Private-registry fallback for image pulls (ADR-0008, REQ-005).
//!
//! A native registry v2 client: manifests (with manifest-list platform
//! selection), blob downloads verified by digest, and assembly of an OCI
//! image layout that `docker load` accepts. Credentials come from the local
//! archive (`~/.dctl/registry/auth`) or the `DCTL_REGISTRY_AUTH` carrier —
//! never from argv, never logged.
//!
//! Pulls flow through the fallback chain decided in the ADR: daemon pull
//! from Docker Hub first, then this client against registry.ohmygh.com,
//! then the local cache tar. Successful private pulls refresh the cache.

use crate::error::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const DEFAULT_REGISTRY: &str = "https://registry.ohmygh.com";
const AUTH_ENV: &str = "DCTL_REGISTRY_AUTH";
const URL_ENV: &str = "DCTL_REGISTRY_URL";

/// Manifest media types we accept and can consume, across the OCI and
/// Docker-registry families.
const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.docker.distribution.manifest.v2+json";

fn registry_error(context: &str, source: reqwest::Error) -> Error {
    Error::Registry(format!("{context}: {source}"))
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let sum = sha2::Sha256::digest(data);
    sum.iter().map(|b| format!("{b:02x}")).collect()
}

/// Basic credentials for the private registry, resolved from the carrier
/// env or the local archive. Absent credentials are fine — the registry
/// decides what needs auth.
pub(crate) struct RegistryAuth(String);

impl RegistryAuth {
    pub(crate) fn load() -> Result<Self> {
        if let Some(text) = std::env::var(AUTH_ENV).ok().filter(|s| !s.is_empty()) {
            return Ok(Self(Self::normalize(&text)?));
        }
        let path = crate::paths::base_dir()
            .map(|base| base.join("registry").join("auth"))
            .map_err(|error| {
                Error::Registry(format!(
                    "no home directory for the registry archive: {error}"
                ))
            })?;
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Self(String::new()));
        };
        warn_if_insecure(&path);
        Ok(Self(Self::normalize(text.trim())?))
    }

    /// Accept `user:password` or a ready basic token; anything else is an
    /// explicit error naming the expected shape.
    fn normalize(text: &str) -> Result<String> {
        use base64::Engine as _;
        if text.is_empty() {
            return Ok(String::new());
        }
        if text.contains(':') {
            let raw = base64::engine::general_purpose::STANDARD.encode(text);
            return Ok(format!("Basic {raw}"));
        }
        if text.starts_with("Basic ") {
            return Ok(text.to_string());
        }
        Err(Error::Registry(
            "the registry credential must be 'user:password' or a 'Basic <token>' line".to_string(),
        ))
    }

    fn as_header(&self) -> Option<(&'static str, &str)> {
        if self.0.is_empty() {
            None
        } else {
            Some(("Authorization", self.0.as_str()))
        }
    }
}

/// Group/other-readable archives warn (like the ledger key), not fail.
fn warn_if_insecure(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.permissions().mode() & 0o077 != 0
        {
            eprintln!(
                "Warning: registry credentials {} are readable by group or others; chmod 600 them.",
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// The endpoint, with the operator/testing override knob.
pub(crate) fn registry_base() -> String {
    let endpoint = std::env::var(URL_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_string());
    strip_userinfo(&endpoint)
}

/// Strip any embedded `user:pass@` from an endpoint override: the knob is
/// endpoint-only by ADR-0008, and credentials embedded in it would echo
/// back through output and error text, breaking the zero-log boundary.
fn strip_userinfo(endpoint: &str) -> String {
    let Some(scheme_at) = endpoint.find("://") else {
        return endpoint.to_string();
    };
    let authority_from = scheme_at + 3;
    let authority_to = endpoint[authority_from..]
        .find('/')
        .map(|offset| authority_from + offset)
        .unwrap_or(endpoint.len());
    match endpoint[authority_from..authority_to].rsplit_once('@') {
        Some((_, host)) => format!(
            "{}{}{}",
            &endpoint[..authority_from],
            host,
            &endpoint[authority_to..]
        ),
        None => endpoint.to_string(),
    }
}

pub(crate) struct RegistryClient {
    base: String,
    http: reqwest::Client,
    auth: RegistryAuth,
}

impl RegistryClient {
    pub(crate) fn new() -> Result<Self> {
        let http = crate::http::client_builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|error| Error::Registry(format!("http client: {error}")))?;
        Ok(Self {
            base: registry_base().trim_end_matches('/').to_string(),
            http,
            auth: RegistryAuth::load()?,
        })
    }

    /// Repository names from `/v2/_catalog`.
    pub(crate) async fn catalog(&self) -> Result<Vec<String>> {
        let mut request = self.http.get(format!("{}/v2/_catalog", self.base));
        if let Some(header) = self.auth.as_header() {
            request = request.header(header.0, header.1);
        }
        let response = request
            .send()
            .await
            .map_err(|error| registry_error("registry catalog request failed", error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Registry(format!(
                "registry catalog {status}: {}",
                response.text().await.unwrap_or_default()
            )));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|error| registry_error("registry catalog decode failed", error))?;
        Ok(body["repositories"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Fetch a manifest by reference, selecting the platform image when the
    /// reference resolves to a manifest list. Returns the image manifest,
    /// its raw bytes (the layout blob must be the exact bytes the digest
    /// addresses, not a re-serialization), and its digest.
    async fn image_manifest(
        &self,
        name: &str,
        reference: &str,
    ) -> Result<(Value, Vec<u8>, String)> {
        let (manifest, bytes, digest) = self.fetch_manifest(name, reference).await?;
        let media_type = manifest["mediaType"].as_str();
        // A list announces itself by media type; some registries omit the
        // field, so a bare `manifests` array also counts.
        let is_list = matches!(
            media_type,
            Some("application/vnd.oci.image.index.v1+json")
                | Some("application/vnd.docker.distribution.manifest.list.v2+json")
        ) || (media_type.is_none()
            && manifest["manifests"]
                .as_array()
                .is_some_and(|entries| !entries.is_empty()));
        if !is_list {
            return Ok((manifest, bytes, digest));
        }
        let digest = select_platform(&manifest).ok_or_else(|| {
            Error::Registry(format!(
                "no linux/{} image in the manifest list for {name}:{reference}",
                host_arch()
            ))
        })?;
        self.fetch_manifest(name, &digest).await
    }

    async fn fetch_manifest(
        &self,
        name: &str,
        reference: &str,
    ) -> Result<(Value, Vec<u8>, String)> {
        let mut request = self
            .http
            .get(format!("{}/v2/{name}/manifests/{reference}", self.base))
            .header("Accept", MANIFEST_TYPES);
        if let Some(header) = self.auth.as_header() {
            request = request.header(header.0, header.1);
        }
        let response = request
            .send()
            .await
            .map_err(|error| registry_error("registry manifest request failed", error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Registry(format!(
                "registry manifest {name}:{reference} {status}: {}",
                response.text().await.unwrap_or_default()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| registry_error("registry manifest download failed", error))?
            .to_vec();
        let body: Value = serde_json::from_slice(&bytes).map_err(|error| {
            Error::Registry(format!("registry manifest decode failed: {error}"))
        })?;
        // Computed from the delivered bytes, so the layout's content
        // addressing always holds even when the header is absent.
        let digest = format!("sha256:{}", sha256_hex(&bytes));
        Ok((body, bytes, digest))
    }

    /// Download a blob by digest, verifying sha256 against the path it lands
    /// at in the layout (content addressing doubles as integrity check).
    async fn blob_to(&self, name: &str, digest: &str, dest: &Path) -> Result<()> {
        let Some(hex) = digest.strip_prefix("sha256:") else {
            return Err(Error::Registry(format!(
                "unsupported blob digest '{digest}' (only sha256)"
            )));
        };
        let mut request = self
            .http
            .get(format!("{}/v2/{name}/blobs/{digest}", self.base));
        if let Some(header) = self.auth.as_header() {
            request = request.header(header.0, header.1);
        }
        let response = request
            .send()
            .await
            .map_err(|error| registry_error("registry blob request failed", error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Registry(format!("registry blob {digest} {status}")));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| registry_error("registry blob download failed", error))?;
        let actual = sha256_hex(&bytes);
        if actual != hex {
            return Err(Error::Registry(format!(
                "blob digest mismatch for {digest}: downloaded content hashes to sha256:{actual}"
            )));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, &bytes)?;
        Ok(())
    }

    /// Pull `name:tag` (or `name@digest`) into an OCI layout directory.
    pub(crate) async fn pull_to_layout(
        &self,
        name: &str,
        reference: &str,
        layout_dir: &Path,
    ) -> Result<String> {
        let (manifest, manifest_bytes, manifest_digest) =
            self.image_manifest(name, reference).await?;
        let config_digest = manifest["config"]["digest"]
            .as_str()
            .ok_or_else(|| Error::Registry("manifest carries no config digest".into()))?
            .to_string();
        let layers: Vec<String> = manifest["layers"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|layer| layer["digest"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if layers.is_empty() {
            return Err(Error::Registry("manifest carries no layers".into()));
        }

        // blobs/sha256/<hex> for config and every layer.
        for digest in std::iter::once(config_digest.clone()).chain(layers.iter().cloned()) {
            let hex = digest.trim_start_matches("sha256:").to_string();
            self.blob_to(name, &digest, &layout_dir.join("blobs/sha256").join(hex))
                .await?;
        }

        // The image manifest itself is a blob too: the raw bytes, so the
        // layout stays content-addressed under the selected digest.
        let manifest_hex = manifest_digest.trim_start_matches("sha256:").to_string();
        std::fs::create_dir_all(layout_dir.join("blobs/sha256"))?;
        std::fs::write(
            layout_dir.join("blobs/sha256").join(&manifest_hex),
            &manifest_bytes,
        )?;

        std::fs::write(
            layout_dir.join("oci-layout"),
            r#"{"imageLayoutVersion":"1.0.0"}"#,
        )?;
        // The ref.name annotation is what docker load tags the image with.
        // On the containerd image store (Docker 28+) names are literal and
        // reference lookup normalizes bare names, so the annotation must be
        // the fully-qualified form or the loaded image never resolves.
        let ref_name = if reference.starts_with("sha256:") {
            format!("{}@{reference}", fully_qualified(name))
        } else {
            format!("{}:{reference}", fully_qualified(name))
        };
        let index = serde_json::json!({
            "schemaVersion": 2,
            "manifests": [{
                // Follow the manifest's own type (Docker v2 family included);
                // default to the OCI type when the registry omitted it.
                "mediaType": manifest["mediaType"].as_str()
                    .unwrap_or("application/vnd.oci.image.manifest.v1+json"),
                "digest": manifest_digest,
                "size": manifest_bytes.len(),
                "annotations": {
                    "org.opencontainers.image.ref.name": ref_name,
                },
            }],
        });
        std::fs::write(
            layout_dir.join("index.json"),
            serde_json::to_vec_pretty(&index)?,
        )?;
        Ok(manifest_digest)
    }
}

/// Map the host architecture onto OCI platform names.
pub(crate) fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Pick this platform's image digest out of a manifest list/index.
pub(crate) fn select_platform(list: &Value) -> Option<String> {
    list["manifests"]
        .as_array()?
        .iter()
        .find(|entry| {
            entry["platform"]["os"].as_str() == Some("linux")
                && entry["platform"]["architecture"].as_str() == Some(host_arch())
        })
        .and_then(|entry| entry["digest"].as_str().map(str::to_string))
}

/// The cache tar path for an image reference.
pub(crate) fn cache_tar(reference: &str) -> Result<PathBuf> {
    let slug: String = reference
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(crate::paths::base_dir()?
        .join("registry")
        .join("cache")
        .join(format!("{slug}.tar")))
}

/// Pack a layout directory into the cache tar (uncompressed; `docker load`
/// accepts it and layer blobs keep their own compression).
pub(crate) fn pack_layout_tar(layout_dir: &Path, tar_path: &Path) -> Result<()> {
    if let Some(parent) = tar_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(tar_path)?;
    let mut builder = tar::Builder::new(file);
    builder.follow_symlinks(false);
    for entry in walk(layout_dir)? {
        // The tar being packed lives inside the layout dir (staging); never
        // pack the archive into itself.
        if entry == tar_path {
            continue;
        }
        let relative = entry.strip_prefix(layout_dir).unwrap_or(&entry);
        if entry.is_file() {
            builder.append_path_with_name(&entry, relative)?;
        }
    }
    builder.finish()?;
    Ok(())
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Load a tar into the daemon via POST /images/load.
pub(crate) async fn docker_load(docker: &bollard::Docker, tar_path: &Path) -> Result<()> {
    use bollard::query_parameters::ImportImageOptionsBuilder;
    use futures_util::StreamExt;
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(tar_path).await.map_err(|error| {
        Error::Registry(format!(
            "cannot open image tar {}: {error}",
            tar_path.display()
        ))
    })?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await.map_err(|error| {
        Error::Registry(format!(
            "cannot read image tar {}: {error}",
            tar_path.display()
        ))
    })?;
    let options = ImportImageOptionsBuilder::default().build();
    let mut stream = docker.import_image(options, bollard::body_full(buffer.into()), None);
    while let Some(item) = stream.next().await {
        item.map_err(|error| Error::Registry(format!("docker load failed: {error}")))?;
    }
    Ok(())
}

// ── command handlers + fallback-chain pieces ───────────────────────────────

pub(crate) async fn run(cmd: crate::local::cli::RegistryCommands, json: bool) -> Result<()> {
    use crate::local::cli::RegistryCommands;
    match cmd {
        RegistryCommands::Pull { reference } => {
            let docker = crate::local::docker::connect().await?;
            let (name, tag) = split_reference(&reference);
            let digest = pull_named_to_layout(&docker, &name, &tag, true).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "image": display_reference(&name, &tag),
                        "digest": digest,
                        "source": registry_base(),
                    })
                );
            } else {
                println!("Pulled {} ({digest})", display_reference(&name, &tag));
            }
            Ok(())
        }
        RegistryCommands::Catalog => {
            let client = RegistryClient::new()?;
            let repositories = client.catalog().await?;
            if json {
                println!("{}", serde_json::json!({ "repositories": repositories }));
            } else if repositories.is_empty() {
                println!("No repositories (or credentials required)");
            } else {
                for name in repositories {
                    println!("{name}");
                }
            }
            Ok(())
        }
    }
}

/// Split `<name>[:tag][@digest]` into (name, reference-for-manifest).
/// Our references never carry a registry host, so the last `:` separates a
/// tag whenever what follows it is not a path (`host:port/name` would keep
/// its port); `@` always pins a digest.
fn split_reference(reference: &str) -> (String, String) {
    if let Some((name, digest)) = reference.split_once('@') {
        return (name.to_string(), digest.to_string());
    }
    match reference.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name.to_string(), tag.to_string()),
        _ => (reference.to_string(), "latest".to_string()),
    }
}

/// Render a name plus its manifest reference back as a human reference:
/// `name:tag`, or `name@digest` for digest-pinned pulls.
fn display_reference(name: &str, tag: &str) -> String {
    if tag.starts_with("sha256:") {
        format!("{name}@{tag}")
    } else {
        format!("{name}:{tag}")
    }
}

/// Fully-qualify a repository name the way the daemon resolves references
/// (`postgres` becomes `docker.io/library/postgres`), mirroring the
/// familiar-name rules of the reference grammar: a first component with a
/// dot, a colon, or `localhost` is already a registry host and stays put.
fn fully_qualified(name: &str) -> String {
    let first = name.split('/').next().unwrap_or(name);
    if first.contains('.') || first.contains(':') || first == "localhost" {
        name.to_string()
    } else if name.contains('/') {
        format!("docker.io/{name}")
    } else {
        format!("docker.io/library/{name}")
    }
}

/// Pull `name:tag` from the private registry into the daemon, staging the
/// layout in a temp dir and (optionally) refreshing the cache tar.
pub(crate) async fn pull_via_registry(docker: &bollard::Docker, image_ref: &str) -> Result<()> {
    let (name, tag) = split_reference(image_ref);
    pull_named_to_layout(docker, &name, &tag, true)
        .await
        .map(|_| ())
}

async fn pull_named_to_layout(
    docker: &bollard::Docker,
    name: &str,
    tag: &str,
    refresh_cache: bool,
) -> Result<String> {
    let client = RegistryClient::new()?;
    let staging = tempfile::tempdir()
        .map_err(|error| Error::Registry(format!("cannot create a staging directory: {error}")))?;
    let digest = client.pull_to_layout(name, tag, staging.path()).await?;
    // docker load consumes tars, not directories: pack the layout first.
    let tar_path = staging.path().join("image.tar");
    pack_layout_tar(staging.path(), &tar_path)?;
    docker_load(docker, &tar_path).await?;
    if refresh_cache && let Ok(cache) = cache_tar(&format!("{name}:{tag}")) {
        // The cache directory is created here, not at copy time: without
        // it the refresh fails with ENOENT and the offline fallback dies.
        let refreshed = std::fs::create_dir_all(cache.parent().unwrap_or(Path::new("")))
            .and_then(|()| std::fs::copy(&tar_path, &cache));
        if let Err(error) = refreshed {
            eprintln!(
                "Warning: cannot refresh the registry cache at {}: {error}",
                cache.display()
            );
        }
    }
    Ok(digest)
}

/// Best-effort load of the cached tar for an image; `Ok(false)` means no
/// cache entry existed.
pub(crate) async fn load_from_cache(docker: &bollard::Docker, image_ref: &str) -> Result<bool> {
    let (name, tag) = split_reference(image_ref);
    let Ok(cache) = cache_tar(&format!("{name}:{tag}")) else {
        return Ok(false);
    };
    if !cache.is_file() {
        return Ok(false);
    }
    docker_load(docker, &cache).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{fully_qualified, split_reference, strip_userinfo};

    #[test]
    fn endpoint_overrides_lose_embedded_userinfo() {
        assert_eq!(
            strip_userinfo("https://user:pass@registry.example.com/v2"),
            "https://registry.example.com/v2"
        );
    }

    #[test]
    fn endpoint_overrides_without_userinfo_pass_through() {
        assert_eq!(
            strip_userinfo("http://127.0.0.1:5000"),
            "http://127.0.0.1:5000"
        );
    }

    #[test]
    fn fully_qualifies_official_and_namespaced_names() {
        assert_eq!(fully_qualified("postgres"), "docker.io/library/postgres");
        assert_eq!(
            fully_qualified("falkordb/falkordb"),
            "docker.io/falkordb/falkordb"
        );
        assert_eq!(
            fully_qualified("clickhouse/clickhouse-server"),
            "docker.io/clickhouse/clickhouse-server"
        );
    }

    #[test]
    fn leaves_host_carrying_names_alone() {
        assert_eq!(
            fully_qualified("registry.ohmygh.com/db/tools"),
            "registry.ohmygh.com/db/tools"
        );
        assert_eq!(fully_qualified("localhost:5000/x"), "localhost:5000/x");
    }

    #[test]
    fn splits_a_bare_name_into_latest() {
        let (name, tag) = split_reference("db/tools");
        assert_eq!((name.as_str(), tag.as_str()), ("db/tools", "latest"));
    }

    #[test]
    fn splits_a_top_level_name_and_tag() {
        let (name, tag) = split_reference("postgres:18");
        assert_eq!((name.as_str(), tag.as_str()), ("postgres", "18"));
    }

    #[test]
    fn splits_a_nested_name_and_tag() {
        let (name, tag) = split_reference("db/tools:1.0");
        assert_eq!((name.as_str(), tag.as_str()), ("db/tools", "1.0"));
    }

    #[test]
    fn splits_a_pinned_digest_reference() {
        let (name, tag) = split_reference("db/tools@sha256:aaaaaaaaaaaaaaaa");
        assert_eq!(
            (name.as_str(), tag.as_str()),
            ("db/tools", "sha256:aaaaaaaaaaaaaaaa")
        );
    }
}
