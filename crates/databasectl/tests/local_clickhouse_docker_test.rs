//! Subprocess coverage for the Docker-managed ClickHouse engine: start,
//! resume, readiness rollback, stop/remove, dotenv and the HTTP client.

use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Serializes `server start` tests: port auto-selection probes real sockets,
/// and parallel starts would race each other's reserved ports.
static START_COMMAND_LOCK: Mutex<()> = Mutex::new(());

const TAG: &str = "26.8";
const CONTAINER: &str = "ch-id";
const IMAGE: &str = "clickhouse/clickhouse-server:26.8";

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

#[derive(Clone, Debug)]
struct DockerRequest {
    method: String,
    path: String,
    body: String,
}

#[derive(Clone)]
struct ChScenario {
    /// GET /images/<image>/json status: 200 keeps the image local, 404 forces a pull.
    image_status: u16,
    /// Whether the fake container reports itself running on inspect.
    running: bool,
    /// Whether a stopped-container metadata file pre-exists (resume path).
    existing_metadata: bool,
    /// POST /containers/ch-id/start status (204 success).
    start_status: u16,
    /// Log lines served for the readiness-failure diagnostics.
    logs: Vec<String>,
    /// When true (default), inspect reports `running` immediately. When
    /// false, the container only looks alive after a successful start —
    /// the fresh/resume flows must see it stopped while resolving the
    /// server name and prior state.
    live_before_start: bool,
    /// Serve this container in `GET /containers/json` for clickhouse-filtered
    /// listing (label recovery). Ports are omitted (a stopped container does
    /// not publish them) so recovery stores 0/0 — the F1 regression state.
    discovered_stopped: bool,
    /// Port bindings the inspect response reports for the container
    /// (AtomicU16 so a test can reserve ports after the daemon starts).
    inspect_http_port: u16,
    inspect_native_port: u16,
}

impl Default for ChScenario {
    fn default() -> Self {
        Self {
            image_status: 200,
            running: false,
            existing_metadata: false,
            start_status: 204,
            logs: Vec::new(),
            live_before_start: true,
            discovered_stopped: false,
            inspect_http_port: 0,
            inspect_native_port: 0,
        }
    }
}

struct FakeDocker {
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<DockerRequest>>>,
    inspect_ports: Arc<Mutex<(u16, u16)>>,
    /// Set once the fake container has been started; readiness listeners
    /// bind their port only after this, so `--http-port` availability
    /// probing sees the port free.
    container_started: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeDocker {
    fn start(socket_path: &Path, project_path: &Path, scenario: ChScenario) -> Self {
        let listener = UnixListener::bind(socket_path).expect("bind fake Docker socket");
        listener
            .set_nonblocking(true)
            .expect("make fake Docker socket nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let container_started = Arc::new(AtomicBool::new(false));
        let inspect_ports = Arc::new(Mutex::new((
            scenario.inspect_http_port,
            scenario.inspect_native_port,
        )));
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let thread_started = Arc::clone(&container_started);
        let thread_inspect_ports = Arc::clone(&inspect_ports);
        let project = project_path.to_path_buf();
        let thread = thread::spawn(move || {
            let ChScenario {
                image_status,
                running,
                existing_metadata,
                start_status,
                logs,
                live_before_start,
                discovered_stopped,
                inspect_http_port,
                inspect_native_port,
            } = scenario;
            thread_inspect_ports.lock().unwrap().0 = inspect_http_port; // seeded; tests may override later
            thread_inspect_ports.lock().unwrap().1 = inspect_native_port;
            let mut started = false;
            let inspect_ports = Arc::clone(&thread_inspect_ports);
            let discovered_body = format!(
                r#"[{{"Id":"{CONTAINER}","Labels":{{"dctl.engine":"clickhouse","dctl.name":"default","dctl.major":"{TAG}","dctl.project":"{}"}},"Image":"{IMAGE}","Ports":[]}}]"#,
                project
                    .canonicalize()
                    .unwrap_or_else(|_| project.clone())
                    .display(),
            );
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept fake Docker request: {error}"),
                };
                stream
                    .set_nonblocking(false)
                    .expect("make fake Docker connection blocking");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("set fake Docker read timeout");
                let request = read_request(&mut stream);
                thread_requests.lock().unwrap().push(request.clone());

                match (request.method.as_str(), request.path.as_str()) {
                    ("GET", "/_ping") => write_response(&mut stream, 200, "text/plain", b"OK"),
                    ("GET", path) if path.starts_with("/images/") => {
                        if image_status == 200 {
                            write_json(&mut stream, 200, "{}");
                        } else {
                            write_json(&mut stream, 404, r#"{"message":"No such image"}"#);
                        }
                    }
                    ("POST", path) if path.starts_with("/images/create") => {
                        write_json(&mut stream, 200, r#"{"status":"Download complete"}"#);
                    }
                    ("GET", path) if path.starts_with("/containers/json") => {
                        // Only the clickhouse-engine listing sees the
                        // discovered container; pg/fk listings stay empty.
                        if discovered_stopped && path.contains("clickhouse") {
                            write_json(&mut stream, 200, &discovered_body);
                        } else {
                            write_json(&mut stream, 200, "[]");
                        }
                    }
                    ("GET", path) if path.starts_with("/containers/dctl-ch-") => {
                        // Inspect by container name during ensure_name_free.
                        write_json(&mut stream, 404, r#"{"message":"No such container"}"#);
                    }
                    ("POST", path) if path.starts_with("/containers/create") => {
                        assert!(
                            !existing_metadata,
                            "resumed start must not create a new container"
                        );
                        write_json(&mut stream, 201, r#"{"Id":"ch-id","Warnings":[]}"#);
                    }
                    ("POST", path) if path.starts_with("/containers/dctl-ch-") => {
                        // Start by container name (never issued by dctl today).
                        write_response(&mut stream, 204, "application/json", b"");
                    }
                    ("POST", path)
                        if path.starts_with(&format!("/containers/{CONTAINER}/start")) =>
                    {
                        if start_status == 204 {
                            started = true;
                            thread_started.store(true, Ordering::Relaxed);
                            write_response(&mut stream, 204, "application/json", b"");
                        } else {
                            write_json(
                                &mut stream,
                                start_status,
                                r#"{"message":"start failed by test"}"#,
                            );
                        }
                    }
                    ("GET", path) if path.starts_with(&format!("/containers/{CONTAINER}/json")) => {
                        // `running` is the scenario knob: inspect answers it
                        // directly, whether or not this session started the
                        // container (stop/client/list never start anything).
                        let live = if live_before_start {
                            running
                        } else {
                            started && running
                        };
                        let state = if live {
                            r#""Status":"running","Running":true,"Paused":false,"ExitCode":0,"OOMKilled":false"#
                        } else {
                            r#""Status":"exited","Running":false,"Paused":false,"ExitCode":0,"OOMKilled":false"#
                        };
                        let (inspect_http_port, inspect_native_port) =
                            *inspect_ports.lock().unwrap();
                        let body = format!(
                            r#"{{"Id":"{CONTAINER}","State":{{{state}}},"Config":{{"Env":["CLICKHOUSE_USER=app","CLICKHOUSE_PASSWORD=stored-secret","CLICKHOUSE_DB=events"]}},"HostConfig":{{"PortBindings":{{"8123/tcp":[{{"HostIp":"127.0.0.1","HostPort":"{inspect_http_port}"}}],"9000/tcp":[{{"HostIp":"127.0.0.1","HostPort":"{inspect_native_port}"}}]}}}}}}"#
                        );
                        write_json(&mut stream, 200, &body);
                    }
                    ("GET", path) if path.starts_with(&format!("/containers/{CONTAINER}/logs")) => {
                        let mut body = Vec::new();
                        for line in &logs {
                            let message = format!("{line}\n");
                            body.extend_from_slice(&[2, 0, 0, 0]);
                            body.extend_from_slice(&(message.len() as u32).to_be_bytes());
                            body.extend_from_slice(message.as_bytes());
                        }
                        write_response(
                            &mut stream,
                            200,
                            "application/vnd.docker.raw-stream",
                            &body,
                        );
                    }
                    ("POST", path)
                        if path.starts_with(&format!("/containers/{CONTAINER}/stop")) =>
                    {
                        write_response(&mut stream, 204, "application/json", b"")
                    }
                    ("DELETE", path) if path.starts_with(&format!("/containers/{CONTAINER}")) => {
                        write_response(&mut stream, 204, "application/json", b"")
                    }
                    ("GET", path)
                        if path.starts_with("/containers/") && path.ends_with("/json") =>
                    {
                        // Metadata for other engines (e.g. a Postgres entry in list).
                        write_json(
                            &mut stream,
                            200,
                            r#"{"Id":"other-id","State":{"Running":false},"Config":{}}"#,
                        );
                    }
                    ("POST", path)
                        if path.starts_with("/containers/") && path.ends_with("/stop") =>
                    {
                        write_response(&mut stream, 204, "application/json", b"")
                    }
                    ("DELETE", path) if path.starts_with("/containers/") => {
                        write_response(&mut stream, 204, "application/json", b"")
                    }
                    _ => {
                        eprintln!("UNEXPECTED fake Docker request: {request:?}");
                        write_json(&mut stream, 404, r#"{"message":"unexpected by test"}"#);
                    }
                }
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Self {
            stop,
            requests,
            inspect_ports,
            container_started,
            thread: Option::Some(thread),
        }
    }

    /// Point the container's reported port bindings at freshly reserved
    /// ports (tests reserve them only after the daemon is running).
    fn set_inspect_ports(&self, http_port: u16, native_port: u16) {
        *self.inspect_ports.lock().unwrap() = (http_port, native_port);
    }

    fn requests(&self) -> Vec<DockerRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn started_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.container_started)
    }
}

impl Drop for FakeDocker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join fake Docker daemon");
        }
    }
}

fn read_request(stream: &mut UnixStream) -> DockerRequest {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).expect("read fake Docker request");
        assert!(count > 0, "Docker request ended before its headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).expect("HTTP headers are UTF-8");
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::parse::<usize>)
        })
        .transpose()
        .expect("valid Content-Length")
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let count = stream.read(&mut buffer).expect("read fake Docker body");
        assert!(count > 0, "Docker request ended before its body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let request_line = headers.lines().next().expect("HTTP request line");
    let mut request_parts = request_line.split_whitespace();
    DockerRequest {
        method: request_parts.next().expect("HTTP method").to_string(),
        path: request_parts.next().expect("HTTP path").to_string(),
        body: String::from_utf8_lossy(&bytes[header_end..header_end + content_length]).into_owned(),
    }
}

fn write_json(stream: &mut UnixStream, status: u16, body: &str) {
    write_response(stream, status, "application/json", body.as_bytes());
}

fn write_response(stream: &mut UnixStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        404 => "Not Found",
        _ => "Response",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|()| stream.write_all(body))
        .expect("write fake Docker response");
}

/// A real localhost HTTP server standing in for the ClickHouse HTTP
/// interface: `/ping` for readiness and `/` for query POSTs. This is the
/// only engine whose readiness can be probed from the host side.
struct FakeClickhouseHttp {
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakeClickhouseHttp {
    /// Serve on `port` as soon as `after` is set (the container has started),
    /// so the port is free while dctl probes its availability.
    fn start_after(port: u16, after: Arc<AtomicBool>) -> Self {
        Self::start_after_with(port, after, false)
    }

    fn start_after_with(port: u16, after: Arc<AtomicBool>, reject_auth: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let thread_reject_auth = reject_auth;
        let thread = thread::spawn(move || {
            // The port must stay free until dctl has probe-checked it, so the
            // bind waits for the container-start signal from the fake daemon.
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            let listener = loop {
                if after.load(Ordering::Relaxed)
                    && let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port))
                {
                    break listener;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "fake ClickHouse HTTP never saw the container start"
                );
                thread::sleep(Duration::from_millis(5));
            };
            listener
                .set_nonblocking(true)
                .expect("fake ClickHouse HTTP nonblocking");
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept fake ClickHouse HTTP: {error}"),
                };
                stream
                    .set_nonblocking(false)
                    .expect("fake ClickHouse HTTP blocking");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("fake ClickHouse HTTP read timeout");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let header_end = loop {
                    let count = stream.read(&mut buffer).expect("read fake CH request");
                    assert!(count > 0, "fake CH request ended before headers");
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers =
                    String::from_utf8(bytes[..header_end].to_vec()).expect("UTF-8 headers");
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .map(str::parse::<usize>)
                    })
                    .transpose()
                    .expect("valid Content-Length")
                    .unwrap_or(0);
                while bytes.len() - header_end < content_length {
                    let count = stream.read(&mut buffer).expect("read fake CH body");
                    assert!(count > 0, "fake CH request ended before body");
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request_line = headers.lines().next().expect("request line");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().expect("method").to_string();
                let path = parts.next().expect("path").to_string();
                let body = String::from_utf8_lossy(&bytes[header_end..header_end + content_length])
                    .into_owned();
                thread_requests
                    .lock()
                    .unwrap()
                    .push((format!("{method} {path}"), body.clone()));

                let has_auth = headers
                    .lines()
                    .any(|line| line.to_ascii_lowercase().starts_with("authorization:"));
                if path.starts_with("/ping") {
                    respond_tcp(&mut stream, 200, "Ok.\n");
                } else if thread_reject_auth && has_auth {
                    respond_tcp(&mut stream, 516, "Code: 516 Authentication failed");
                } else {
                    respond_tcp(&mut stream, 200, &format!("query-ack:{body}"));
                }
            }
        });
        Self {
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeClickhouseHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join fake ClickHouse HTTP");
        }
    }
}

fn respond_tcp(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|()| stream.write_all(body.as_bytes()))
        .expect("write fake ClickHouse HTTP response");
}

/// Like `start_after`, but POSTs with an Authorization header get a 516 —
/// the server is up and healthy yet rejects these credentials.
fn http_listener_rejecting_auth(port: u16, after: Arc<AtomicBool>) -> (FakeClickhouseHttp, u16) {
    let ready = after;
    (
        FakeClickhouseHttp::start_after_with(port, ready, true),
        port,
    )
}

/// Eagerly bound query listener for tests that never run `server start`
/// (the managed client posts straight to the recorded port).
fn http_listener_now() -> (FakeClickhouseHttp, u16) {
    let port = reserve_port();
    let ready = Arc::new(AtomicBool::new(true));
    (FakeClickhouseHttp::start_after(port, ready), port)
}

fn reserve_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve port")
        .local_addr()
        .expect("reserved address")
        .port()
}

/// Two distinct ephemeral ports: the kernel can hand the same port back out
/// once the first probe socket drops, and the engine rejects equal ports.
fn reserve_port_pair() -> (u16, u16) {
    loop {
        let first = reserve_port();
        let second = reserve_port();
        if first != second {
            return (first, second);
        }
    }
}

fn run(project: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(dctl_binary())
        .env_clear()
        .env("DO_NOT_TRACK", "1")
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env(
            "DOCKER_HOST",
            format!("unix://{}/docker.sock", home.display()),
        )
        .current_dir(project)
        .args(args)
        .output()
        .expect("run dctl")
}

fn write_ch_metadata(project: &Path, http_port: u16, native_port: u16) {
    let servers = project.join(".dctl/servers");
    std::fs::create_dir_all(servers.join("default-ch26.8/data")).expect("create server data dir");
    let cwd = project.canonicalize().expect("canonical project path");
    let metadata = serde_json::json!({
        "name": "default-ch26.8",
        "pid": 0,
        "version": format!("clickhouse:{TAG}"),
        "http_port": http_port,
        "tcp_port": native_port,
        "started_at": "before-test",
        "cwd": cwd,
        "engine": "clickhouse",
        "container_id": CONTAINER
    });
    std::fs::write(
        servers.join("default-ch26.8.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .expect("write server metadata");
}

fn read_metadata(project: &Path, key: &str) -> serde_json::Value {
    let path = project.join(format!(".dctl/servers/{key}.json"));
    serde_json::from_slice(&std::fs::read(&path).expect("metadata file exists"))
        .expect("metadata JSON")
}

#[test]
fn fresh_start_creates_container_writes_metadata_and_prints_credentials() {
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            live_before_start: false,
            ..Default::default()
        },
    );
    let (http_port, native_port) = reserve_port_pair();
    let http = FakeClickhouseHttp::start_after(http_port, docker.started_flag());

    let output = run(
        project.path(),
        home.path(),
        &[
            "local",
            "--json",
            "server",
            "start",
            "--http-port",
            &http_port.to_string(),
            "--native-port",
            &native_port.to_string(),
            "--user",
            "app",
            "--database",
            "events",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("start JSON");
    assert_eq!(result["name"], "default");
    assert_eq!(result["container_id"], CONTAINER);
    assert_eq!(result["image"], format!("clickhouse:{TAG}"));
    assert_eq!(result["http_port"], http_port);
    assert_eq!(result["native_port"], native_port);
    assert_eq!(result["user"], "app");
    assert_eq!(result["database"], "events");
    assert!(
        result["password"].as_str().is_some_and(|p| !p.is_empty()),
        "start prints the generated password once: {result}"
    );

    let requests = docker.requests();
    let create = requests
        .iter()
        .find(|request| request.path.starts_with("/containers/create"))
        .expect("container create request");
    let body: serde_json::Value = serde_json::from_str(&create.body).expect("create body JSON");
    assert_eq!(body["Image"], IMAGE);
    let env: Vec<&str> = body["Env"]
        .as_array()
        .expect("Env array")
        .iter()
        .map(|v| v.as_str().expect("env string"))
        .collect();
    assert!(env.iter().any(|e| e.starts_with("CLICKHOUSE_USER=app")));
    assert!(
        env.iter()
            .any(|e| e.starts_with("CLICKHOUSE_PASSWORD=") && *e != "CLICKHOUSE_PASSWORD=")
    );
    assert!(env.contains(&"CLICKHOUSE_DB=events"));
    let binds: Vec<&str> = body["HostConfig"]["Binds"]
        .as_array()
        .expect("Binds array")
        .iter()
        .map(|v| v.as_str().expect("bind string"))
        .collect();
    let data_bind = binds
        .iter()
        .find(|b| b.ends_with(":/var/lib/clickhouse"))
        .expect("data dir bind to /var/lib/clickhouse");
    assert!(
        data_bind.starts_with(
            &project
                .path()
                .join(".dctl/servers/default-ch26.8/data")
                .display()
                .to_string()
        ),
        "bind mounts the instance data dir: {data_bind}"
    );
    let ulimits = body["HostConfig"]["Ulimits"]
        .as_array()
        .expect("Ulimits array");
    assert!(
        ulimits
            .iter()
            .any(|u| u["Name"] == "nofile" && u["Hard"] == 262144),
        "official-image nofile ulimit: {ulimits:?}"
    );
    assert_eq!(body["Labels"]["dctl.engine"], "clickhouse");
    assert_eq!(body["Labels"]["dctl.name"], "default");
    assert_eq!(body["Labels"]["dctl.major"], TAG);

    let metadata = read_metadata(project.path(), "default-ch26.8");
    assert_eq!(metadata["container_id"], CONTAINER);
    assert_eq!(metadata["engine"], "clickhouse");
    assert!(
        project
            .path()
            .join(".dctl/servers/default-ch26.8/data")
            .is_dir()
    );
    assert_eq!(http.requests()[0].0, "GET /ping");
    drop(docker);
}

#[test]
fn start_pulls_missing_image_before_creating() {
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            image_status: 404,
            running: true,
            live_before_start: false,
            ..Default::default()
        },
    );
    let http_port = reserve_port();
    let _http = FakeClickhouseHttp::start_after(http_port, docker.started_flag());
    let native_port = reserve_port();

    let output = run(
        project.path(),
        home.path(),
        &[
            "local",
            "--json",
            "server",
            "start",
            "--http-port",
            &http_port.to_string(),
            "--native-port",
            &native_port.to_string(),
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = docker.requests();
    let pull = requests
        .iter()
        .find(|request| request.path.starts_with("/images/create"))
        .expect("image pull request");
    assert!(
        pull.path.contains("clickhouse"),
        "pull targets the ClickHouse image: {pull:?}"
    );
    assert!(
        requests
            .iter()
            .any(|request| request.path.starts_with("/containers/create")),
        "container is created after the pull"
    );
    drop(docker);
}

#[test]
fn resume_starts_existing_container_without_creating() {
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    let native_port = reserve_port();
    write_ch_metadata(project.path(), http_port, native_port);
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            existing_metadata: true,
            running: true,
            live_before_start: false,
            ..Default::default()
        },
    );
    docker.set_inspect_ports(http_port, native_port);
    let http = FakeClickhouseHttp::start_after(http_port, docker.started_flag());

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "start"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("resume JSON");
    assert_eq!(result["name"], "default");
    assert_eq!(result["image"], format!("clickhouse:{TAG}"));
    assert!(
        result["password"].is_null(),
        "resume never reprints a password it cannot know: {result}"
    );
    // user/database are read back from the container env — the same source
    // dotenv/client use — so resume reports the provisioned identity.
    assert_eq!(result["user"], "app", "{result}");
    assert_eq!(result["database"], "events", "{result}");

    let requests = docker.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.path.starts_with("/containers/create")),
        "resume must reuse the existing container"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                request.method == "POST"
                    && request
                        .path
                        .starts_with(&format!("/containers/{CONTAINER}/start"))
            })
            .count(),
        1
    );
    assert!(http.requests().iter().any(|(line, _)| line == "GET /ping"));
    drop(docker);
}

#[test]
fn start_when_container_running_reports_already_running() {
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_ch_metadata(project.path(), reserve_port(), reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            existing_metadata: true,
            running: true,
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "start", "default"],
    );
    assert!(!output.status.success(), "already-running must fail");
    let result: serde_json::Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(result["error"]["code"], "server_running");
    drop(docker);
}

#[test]
fn readiness_timeout_rolls_back_fresh_container_and_data() {
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // No FakeClickhouseHttp: the container "runs" but never answers /ping.
    let http_port = reserve_port();
    let native_port = reserve_port();
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            logs: vec!["<Fatal> Application: DB::Exception: cannot read config".into()],
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &[
            "local",
            "--json",
            "server",
            "start",
            "--http-port",
            &http_port.to_string(),
            "--native-port",
            &native_port.to_string(),
            "--wait-timeout",
            "1",
        ],
    );
    assert!(!output.status.success(), "readiness timeout must fail");
    let result: serde_json::Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(result["error"]["code"], "startup_timeout");
    assert!(
        result["error"]["message"]
            .as_str()
            .expect("message")
            .contains("did not become ready"),
        "{}",
        result
    );

    let requests = docker.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.method == "DELETE" && request.path.contains(CONTAINER)),
        "the failed fresh container is removed"
    );
    assert!(
        !project
            .path()
            .join(".dctl/servers/default-ch26.8.json")
            .exists(),
        "fresh metadata is rolled back"
    );
    assert!(
        !project.path().join(".dctl/servers/default-ch26.8").exists(),
        "fresh data directory is rolled back"
    );
    drop(docker);
}

#[test]
fn recovery_from_deleted_metadata_resumes_with_correct_ports() {
    // The F1 regression chain: metadata gone (git clean -xdf clears
    // .dctl/), a stopped labelled container remains. Recovery via labels
    // stores 0/0 ports (a stopped container publishes none); the resume
    // must refresh BOTH ports from the container's own bindings before
    // probing readiness — http://127.0.0.1:0/ping never becomes ready.
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            existing_metadata: true, // a create after recovery is a bug
            running: true,
            live_before_start: false,
            discovered_stopped: true,
            inspect_http_port: 0, // filled after reserving, see below
            inspect_native_port: 0,
            ..Default::default()
        },
    );
    let http_port = reserve_port();
    let native_port = reserve_port();
    // Point the fake inspect at the reserved ports (bindings live in the
    // scenario thread; set them through the shared request log is not
    // possible, so re-bind via a dedicated scenario field setter).
    docker.set_inspect_ports(http_port, native_port);
    let http = FakeClickhouseHttp::start_after(http_port, docker.started_flag());

    // 1) `server list` triggers label recovery and writes metadata.
    let listed = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "list"],
    );
    assert!(
        listed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let recovered_path = project.path().join(".dctl/servers/default-ch26.8.json");
    assert!(recovered_path.exists(), "recovery writes the metadata file");
    let recovered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&recovered_path).unwrap()).unwrap();
    assert_eq!(recovered["container_id"], CONTAINER);
    // A stopped container publishes no ports: recovery stores 0/0 and the
    // resume is on the hook to refresh them (the F1 bug stored the HTTP
    // port in tcp_port and left http_port at 0 forever).
    assert_eq!(recovered["http_port"], 0, "{recovered}");
    assert_eq!(recovered["tcp_port"], 0, "{recovered}");

    // 2) `server start` resumes the discovered container.
    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "start"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("resume JSON");
    assert_eq!(result["http_port"], http_port, "{result}");
    assert_eq!(result["native_port"], native_port, "{result}");
    assert!(
        http.requests().iter().any(|(line, _)| line == "GET /ping"),
        "readiness probed the refreshed HTTP port"
    );

    // 3) The metadata now carries the refreshed ports.
    let refreshed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&recovered_path).unwrap()).unwrap();
    assert_eq!(refreshed["http_port"], http_port);

    // 4) The recovered instance is queryable.
    let queried = run(
        project.path(),
        home.path(),
        &["local", "client", "--query", "SELECT 1"],
    );
    assert!(
        queried.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&queried.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&queried.stdout),
        "query-ack:SELECT 1"
    );
    drop(docker);
}

#[test]
fn stop_when_already_stopped_is_idempotent_success() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario::default(), // container reports stopped
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "stop"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stop JSON");
    assert_eq!(result["name"], "default");
    assert_eq!(result["already_stopped"], true);
    drop(docker);
}

#[test]
fn legacy_binary_era_metadata_can_be_stopped_and_removed() {
    // F2: a pid-only entry (binary era, no container_id) must be disposable —
    // stop reports already-stopped, remove clears metadata and data dir.
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let servers = project.path().join(".dctl/servers");
    std::fs::create_dir_all(servers.join("dev/data")).unwrap();
    let metadata = serde_json::json!({
        "name": "dev",
        "pid": 4242,
        "version": "25.12.9.61",
        "http_port": 8123,
        "tcp_port": 9000,
        "started_at": "binary-era",
        "cwd": project.path().canonicalize().unwrap(),
        "engine": "clickhouse"
    });
    std::fs::write(
        servers.join("dev.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario::default(),
    );

    // A read-only list must NOT wipe the version any more.
    let listed = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "list"],
    );
    assert!(listed.status.success());
    let after_list: serde_json::Value =
        serde_json::from_slice(&std::fs::read(servers.join("dev.json")).unwrap()).unwrap();
    assert_eq!(
        after_list["version"], "25.12.9.61",
        "list preserved the identity"
    );

    let stopped = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "stop", "dev"],
    );
    assert!(
        stopped.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let stop_json: serde_json::Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert_eq!(stop_json["already_stopped"], true);

    let removed = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "remove", "dev"],
    );
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!servers.join("dev.json").exists(), "metadata cleared");
    assert!(!servers.join("dev").exists(), "data dir cleared");
    drop(docker);
}

#[test]
fn list_without_docker_degrades_to_stopped_with_a_warning() {
    // G6: the read-only entry point stays usable when the daemon is gone.
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());

    // DOCKER_HOST points at a socket nobody serves: connect fails fast.
    let output = std::process::Command::new(dctl_binary())
        .env_clear()
        .env("DO_NOT_TRACK", "1")
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env(
            "DOCKER_HOST",
            format!("unix://{}/missing.sock", home.path().display()),
        )
        .current_dir(project.path())
        .args(["local", "--json", "server", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "list must not fail without Docker: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["servers"][0]["name"], "default");
    assert_eq!(result["servers"][0]["running"], false);
    assert_eq!(result["servers"][0]["version"], "clickhouse:26.8");
}

#[test]
fn start_warns_when_existing_data_rejects_the_printed_credentials() {
    // G5: /ping is unauthenticated; one SELECT 1 turns a silently-wrong
    // password (existing data dir keeps its first-init password) into a
    // warning. Start still succeeds — the server itself is healthy.
    let _guard = START_COMMAND_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            live_before_start: false,
            ..Default::default()
        },
    );
    let http_port = reserve_port();
    let native_port = reserve_port();
    let (http, _bound) = http_listener_rejecting_auth(http_port, docker.started_flag());

    let output = run(
        project.path(),
        home.path(),
        &[
            "local",
            "server",
            "start",
            "--http-port",
            &http_port.to_string(),
            "--native-port",
            &native_port.to_string(),
        ],
    );
    assert!(
        output.status.success(),
        "an auth mismatch is a warning, not a failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rejected the printed credentials"),
        "warning surfaces in stderr: {stderr}"
    );
    drop(http);
    drop(docker);
}

#[test]
fn stop_stops_the_running_container() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "stop"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stop JSON");
    assert_eq!(result["name"], "default");
    assert_eq!(result["already_stopped"], false);
    assert_eq!(
        docker
            .requests()
            .iter()
            .filter(|request| {
                request.method == "POST"
                    && request
                        .path
                        .starts_with(&format!("/containers/{CONTAINER}/stop"))
            })
            .count(),
        1
    );
    drop(docker);
}

#[test]
fn remove_deletes_container_data_and_metadata() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario::default(),
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "remove"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !project
            .path()
            .join(".dctl/servers/default-ch26.8.json")
            .exists(),
        "metadata removed"
    );
    assert!(
        !project.path().join(".dctl/servers/default-ch26.8").exists(),
        "data removed"
    );
    assert!(
        docker
            .requests()
            .iter()
            .any(|request| request.method == "DELETE" && request.path.contains(CONTAINER)),
        "container removed"
    );
    drop(docker);
}

#[test]
fn remove_running_server_is_refused_with_stop_hint() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "remove"],
    );
    assert!(
        !output.status.success(),
        "running server must not be removed"
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(result["error"]["code"], "server_running");
    assert!(
        result["error"]["command"]
            .as_str()
            .expect("command")
            .contains("server stop"),
        "{}",
        result
    );
    drop(docker);
}

#[test]
fn dotenv_writes_clickhouse_env_vars() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "dotenv"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("dotenv JSON");
    assert_eq!(result["file"], ".env");
    assert_eq!(result["server"], "default");
    let content = std::fs::read_to_string(project.path().join(".env")).expect(".env written");
    assert!(content.contains("CLICKHOUSE_HOST=127.0.0.1"));
    assert!(content.contains(&format!("CLICKHOUSE_HTTP_PORT={http_port}")));
    assert!(content.contains("CLICKHOUSE_USER=app"));
    assert!(content.contains("CLICKHOUSE_PASSWORD=stored-secret"));
    assert!(content.contains("CLICKHOUSE_DATABASE=events"));
    drop(docker);
}

#[test]
fn client_query_runs_over_http_with_container_credentials() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (http, http_port) = http_listener_now();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "client", "--query", "SELECT 1"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "query-ack:SELECT 1",
        "query output stays native, routed through the HTTP interface"
    );
    let requests = http.requests();
    let post = requests
        .iter()
        .find(|(line, _)| line.starts_with("POST /"))
        .expect("query POST");
    assert_eq!(post.1, "SELECT 1", "the SQL is the HTTP body");
    drop(docker);
}

#[test]
fn client_managed_requires_running_server() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario::default(),
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "client", "--query", "SELECT 1"],
    );
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(result["error"]["code"], "server_not_running");
    drop(docker);
}

#[test]
fn list_reports_running_and_stopped_instances() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let http_port = reserve_port();
    write_ch_metadata(project.path(), http_port, reserve_port());
    let docker = FakeDocker::start(
        &home.path().join("docker.sock"),
        project.path(),
        ChScenario {
            running: true,
            ..Default::default()
        },
    );

    let output = run(
        project.path(),
        home.path(),
        &["local", "--json", "server", "list"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("list JSON");
    let servers = result["servers"].as_array().expect("servers array");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["name"], "default");
    assert_eq!(servers[0]["engine"], "clickhouse");
    assert_eq!(servers[0]["running"], true);
    assert_eq!(servers[0]["container_id"], CONTAINER);
    drop(docker);
}
