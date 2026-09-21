//! Private-registry fallback coverage (ADR-0008, REQ-005): a stub v2
//! registry over plain HTTP, a fake Docker daemon that records /images/load,
//! and the digest/platform/auth contract of the native client.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let sum = sha2::Sha256::digest(data);
    sum.iter().map(|b| format!("{b:02x}")).collect()
}

// ── stub registry: plain HTTP over a local TCP listener ────────────────────

struct StubRegistry {
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    auths: Arc<Mutex<Vec<Option<String>>>>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

/// Everything the stub serves, keyed by request path.
struct Fixtures {
    catalog: Value,
    /// (manifest-json, media-type) by reference
    manifests: Vec<(String, String, Value)>,
    /// blobs by digest
    blobs: Vec<(String, Vec<u8>)>,
    /// the image's layer digest, so corruption tests can target exactly it
    layer_digest: String,
}

impl Fixtures {
    /// A minimal single-platform image: one config, one layer.
    fn image(name: &str, tag: &str, layer: &[u8]) -> Self {
        let config = json!({
            "architecture": "stub",
            "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": []},
        });
        let config_bytes = serde_json::to_vec(&config).unwrap();
        let config_digest = format!("sha256:{}", sha256_hex(&config_bytes));
        let layer_digest = format!("sha256:{}", sha256_hex(layer));
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config_bytes.len(),
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": layer_digest,
                "size": layer.len(),
            }],
        });
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = format!("sha256:{}", sha256_hex(&manifest_bytes));
        let list = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": manifest_digest,
                "size": manifest_bytes.len(),
                "platform": {"os": "linux", "architecture": registry_host_arch()},
            }],
        });
        Self {
            catalog: json!({"repositories": [name]}),
            manifests: vec![
                (
                    format!("/{name}/manifests/{tag}"),
                    "application/vnd.oci.image.index.v1+json".into(),
                    list,
                ),
                (
                    format!("/{name}/manifests/{manifest_digest}"),
                    "application/vnd.oci.image.manifest.v1+json".into(),
                    manifest,
                ),
            ],
            blobs: vec![
                (config_digest, config_bytes),
                (layer_digest.clone(), layer.to_vec()),
                (manifest_digest, manifest_bytes),
            ],
            layer_digest,
        }
    }
}

/// The arch dctl will select for, mirroring the client mapping (tests run on
/// the same host, so this is std::env::consts::ARCH mapped identically).
fn registry_host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

impl StubRegistry {
    fn start(fixtures: Arc<Fixtures>, corrupt_layer: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind stub registry");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let auths = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let thread_auths = Arc::clone(&auths);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept stub registry: {error}"),
                };
                stream.set_nonblocking(false).ok();
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                thread_requests
                    .lock()
                    .unwrap()
                    .push(format!("{} {}", request.method, request.path));
                thread_auths
                    .lock()
                    .unwrap()
                    .push(request.authorization.clone());
                respond(&mut stream, &fixtures, &request, corrupt_layer);
            }
        });
        Self {
            stop,
            requests,
            auths,
            thread: Some(thread),
            port,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn auth_headers(&self) -> Vec<Option<String>> {
        self.auths.lock().unwrap().clone()
    }
}

impl Drop for StubRegistry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join stub registry");
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    authorization: Option<String>,
}

fn read_request(stream: &mut TcpStream) -> Option<HttpRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let request_line = headers.lines().next()?.to_string();
    let authorization = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_string())
    });
    let mut parts = request_line.split_whitespace();
    Some(HttpRequest {
        method: parts.next()?.to_string(),
        path: parts.next()?.to_string(),
        authorization,
    })
}

fn respond(stream: &mut TcpStream, fixtures: &Fixtures, request: &HttpRequest, corrupt: bool) {
    // registry:2-style handshake on the API root: anonymous pings get a
    // Basic challenge, credentialed pings pass. The client library answers
    // the challenge and rides credentials on the real requests after.
    if request.path == "/v2/" {
        let head = if request.authorization.is_none() {
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"stub\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        } else {
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
        };
        let _ = stream.write_all(head.as_bytes());
        if request.authorization.is_some() {
            let _ = stream.write_all(b"{}");
        }
        return;
    }
    let (status, content_type, body) = if request.path == "/v2/_catalog" {
        (
            200,
            "application/json",
            serde_json::to_vec(&fixtures.catalog).unwrap(),
        )
    } else if let Some((_, media_type, manifest)) = fixtures
        .manifests
        .iter()
        .find(|(path, _, _)| request.path == format!("/v2{path}"))
    {
        let body = serde_json::to_vec(manifest).unwrap();
        let digest = format!("sha256:{}", sha256_hex(&body));
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {media_type}\r\nDocker-Content-Digest: {digest}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
        return;
    } else if let Some((digest, blob)) = fixtures.blobs.iter().find(|(digest, _)| {
        request.path.starts_with("/v2/") && request.path.ends_with(&format!("/blobs/{digest}"))
    }) {
        if corrupt && *digest == fixtures.layer_digest {
            (200, "application/octet-stream", b"corrupted!".to_vec())
        } else {
            (200, "application/octet-stream", blob.clone())
        }
    } else {
        (
            404,
            "application/json",
            br#"{"errors":[{"code":"NAME_UNKNOWN"}]}"#.to_vec(),
        )
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        if status == 200 { "OK" } else { "Not Found" },
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

// ── fake docker with /images/load ──────────────────────────────────────────

struct FakeLoadDocker {
    stop: Arc<AtomicBool>,
    loads: Arc<Mutex<Vec<usize>>>,
    thread: Option<JoinHandle<()>>,
    socket: PathBuf,
}

impl FakeLoadDocker {
    fn start(dir: &Path) -> Self {
        let socket = dir.join("docker.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind fake docker");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let loads = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_loads = Arc::clone(&loads);
        let thread = thread::spawn(move || {
            use std::io::Write as _;
            while !thread_stop.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).ok();
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 8192];
                // Read until the body arrives (headers + at least a header
                // byte of tar). Loop with WouldBlock tolerance.
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            if bytes.windows(512).any(|window| window.starts_with(b"oci-"))
                                || std::time::Instant::now() > deadline
                            {
                                break;
                            }
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
                let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
                let request_line = head.lines().next().unwrap_or_default().to_string();
                if request_line.contains("POST /images/load") {
                    thread_loads.lock().unwrap().push(bytes.len());
                    let body = br#"{"stream":"Loaded image"}"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.write_all(body);
                } else if request_line.contains("GET /_ping") {
                    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                    let _ = stream.write_all(response.as_bytes());
                } else {
                    let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                    let _ = stream.write_all(response.as_bytes());
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        Self {
            stop,
            loads,
            thread: Some(thread),
            socket,
        }
    }

    fn load_count(&self) -> usize {
        self.loads.lock().unwrap().len()
    }

    fn load_sizes(&self) -> Vec<usize> {
        self.loads.lock().unwrap().clone()
    }
}

impl Drop for FakeLoadDocker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join fake docker");
        }
    }
}

// ── subprocess harness ──────────────────────────────────────────────────────

struct Sandbox {
    home: tempfile::TempDir,
    docker: FakeLoadDocker,
}

impl Sandbox {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".dctl")).unwrap();
        let docker = FakeLoadDocker::start(home.path());
        Self { home, docker }
    }

    fn run(&self, registry_url: &str, args: &[&str]) -> std::process::Output {
        Command::new(dctl_binary())
            .env_clear()
            .env("HOME", self.home.path())
            .env("PATH", "/usr/bin:/bin")
            .env("DCTL_REGISTRY_URL", registry_url)
            .env(
                "DOCKER_HOST",
                format!("unix://{}", self.docker.socket.display()),
            )
            .current_dir(self.home.path())
            .args(args)
            .output()
            .expect("run dctl")
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[test]
fn pull_fetches_verifies_loads_and_caches() {
    let fixtures = Arc::new(Fixtures::image("db/tools", "1.0", b"layer-bytes-here"));
    let registry = StubRegistry::start(fixtures, false);
    let sandbox = Sandbox::new();

    let output = sandbox.run(
        &registry.url(),
        &["local", "registry", "pull", "db/tools:1.0"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("db/tools:1.0"), "{stdout}");
    assert!(stdout.contains("sha256:"), "{stdout}");

    // The chain: catalog not needed; manifest by tag, then by digest
    // (platform selection), then config + layer blobs, then docker load.
    let requests = registry.requests();
    assert!(
        requests
            .iter()
            .any(|line| line.ends_with("/db/tools/manifests/1.0")),
        "{requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|line| line.contains("/manifests/sha256:")),
        "platform selection must fetch the digest manifest: {requests:?}"
    );
    assert!(
        requests.iter().any(|line| line.contains("/blobs/sha256:")),
        "{requests:?}"
    );
    assert_eq!(sandbox.docker.load_count(), 1, "one docker load");
    assert!(
        sandbox.docker.load_sizes()[0] > 1024,
        "the load body carries a real tar"
    );

    // Cache tar refreshed next to the archive.
    let cache = sandbox
        .home
        .path()
        .join(".dctl/registry/cache/db_tools_1.0.tar");
    assert!(cache.is_file(), "cache tar at {}", cache.display());

    // The cache tar is an OCI layout: every blob content-addresses to its
    // own path, and the archive never packs itself in.
    let mut archive = tar::Archive::new(std::fs::File::open(&cache).unwrap());
    let mut entries = 0;
    let mut ref_name = None;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        assert_ne!(path, "image.tar", "the cache tar must not pack itself");
        if let Some(hex) = path.strip_prefix("blobs/sha256/") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert_eq!(
                sha256_hex(&bytes),
                hex,
                "blob {path} must hash to the digest it is stored under"
            );
        }
        if path == "index.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            let index: Value = serde_json::from_slice(&bytes).unwrap();
            ref_name = index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"]
                .as_str()
                .map(str::to_string);
        }
        entries += 1;
    }
    assert!(entries >= 4, "layout files in the cache tar: {entries}");
    // The load tag must be the fully-qualified form: containerd image
    // stores resolve references normalized, so a bare name never matches.
    assert_eq!(ref_name.as_deref(), Some("docker.io/db/tools:1.0"));
}

#[test]
fn catalog_lists_repositories() {
    let fixtures = Arc::new(Fixtures::image("db/tools", "1.0", b"x"));
    let registry = StubRegistry::start(fixtures, false);
    let sandbox = Sandbox::new();

    let output = sandbox.run(&registry.url(), &["local", "--json", "registry", "catalog"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["repositories"], json!(["db/tools"]));
}

#[test]
fn unknown_reference_reports_the_registry_error() {
    let fixtures = Arc::new(Fixtures::image("db/tools", "1.0", b"x"));
    let registry = StubRegistry::start(fixtures, false);
    let sandbox = Sandbox::new();

    let output = sandbox.run(
        &registry.url(),
        &["local", "registry", "pull", "db/absent:9"],
    );
    assert!(!output.status.success());
    // Structural discriminators only: the manifest lookup was attempted
    // (error origin is the registry, not usage) and nothing reached the
    // daemon.
    let requests = registry.requests();
    assert!(
        requests
            .iter()
            .any(|line| line.ends_with("/db/absent/manifests/9")),
        "{requests:?}"
    );
    assert_eq!(sandbox.docker.load_count(), 0);
}

#[test]
fn corrupted_layer_fails_the_digest_check_without_loading() {
    let fixtures = Arc::new(Fixtures::image("db/tools", "1.0", b"layer-bytes-here"));
    let registry = StubRegistry::start(fixtures, true);
    let sandbox = Sandbox::new();

    let output = sandbox.run(
        &registry.url(),
        &["local", "registry", "pull", "db/tools:1.0"],
    );
    assert!(!output.status.success());
    // The corrupted layer was fetched (the mismatch fired after download,
    // not as a transport failure) and nothing reached the daemon.
    let requests = registry.requests();
    assert!(
        requests.iter().any(|line| line.contains("/blobs/sha256:")),
        "{requests:?}"
    );
    assert_eq!(sandbox.docker.load_count(), 0);
}

#[test]
fn carrier_credentials_ride_as_basic_auth() {
    let fixtures = Arc::new(Fixtures::image("db/tools", "1.0", b"x"));
    let registry = StubRegistry::start(fixtures, false);
    let sandbox = Sandbox::new();

    let output = Command::new(dctl_binary())
        .env_clear()
        .env("HOME", sandbox.home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("DCTL_REGISTRY_URL", registry.url())
        .env("DCTL_REGISTRY_AUTH", "fleet-user:secret-pass")
        .env(
            "DOCKER_HOST",
            format!("unix://{}", sandbox.docker.socket.display()),
        )
        .current_dir(sandbox.home.path())
        .args(["local", "registry", "catalog"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The carrier rides as a basic auth header on every registry request.
    use base64::Engine as _;
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("fleet-user:secret-pass")
    );
    let auths = registry.auth_headers();
    assert!(
        auths
            .iter()
            .any(|header| header.as_deref() == Some(expected.as_str())),
        "Authorization headers seen: {auths:?}"
    );
}
