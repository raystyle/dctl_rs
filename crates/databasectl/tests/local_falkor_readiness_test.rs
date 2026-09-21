//! Subprocess coverage for local FalkorDB readiness through a fake Docker API.

use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

static START_COMMAND_LOCK: Mutex<()> = Mutex::new(());

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

const IMAGE_REF: &str = "falkordb/falkordb:v4.20.6";
const INSTANCE_KEY: &str = "default-fk4.20.6";
const CONTAINER_NAME: &str = "dctl-fk-default-4.20.6";

#[derive(Clone, Copy)]
enum ContainerOutcome {
    Running,
    ImmediateExit,
}

struct DockerScenario {
    existing: bool,
    outcome: ContainerOutcome,
    remove_statuses: Vec<u16>,
    readiness_exit_codes: Vec<i64>,
    logs: Vec<String>,
}

impl Default for DockerScenario {
    fn default() -> Self {
        Self {
            existing: false,
            outcome: ContainerOutcome::Running,
            remove_statuses: Vec::new(),
            readiness_exit_codes: Vec::new(),
            logs: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct DockerRequest {
    method: String,
    path: String,
    body: String,
    metadata_lock_available: bool,
}

struct FakeDocker {
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<DockerRequest>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakeDocker {
    fn start(socket_path: &Path, project_path: &Path, scenario: DockerScenario) -> Self {
        let listener = UnixListener::bind(socket_path).expect("bind fake Docker socket");
        listener
            .set_nonblocking(true)
            .expect("make fake Docker socket nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let project = project_path.to_path_buf();
        let thread = thread::spawn(move || {
            let DockerScenario {
                existing,
                outcome,
                remove_statuses,
                readiness_exit_codes,
                logs,
            } = scenario;
            let mut started = false;
            let mut next_exec = 0_usize;
            let mut remove_statuses: VecDeque<u16> = remove_statuses.into();
            let mut readiness_exit_codes: VecDeque<i64> = readiness_exit_codes.into();
            let mut exec_exit_codes = HashMap::new();

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
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .expect("set fake Docker read timeout");
                let mut request = read_request(&mut stream);
                request.metadata_lock_available = metadata_lock_available(&project);
                thread_requests.lock().unwrap().push(request.clone());

                match (request.method.as_str(), request.path.as_str()) {
                    ("GET", "/_ping") => write_response(&mut stream, 200, "text/plain", b"OK"),
                    ("GET", path) if path.starts_with("/containers/json?") => {
                        write_json(&mut stream, 200, "[]")
                    }
                    ("GET", path) if path.starts_with("/images/alpine:latest/json") => {
                        write_json(&mut stream, 200, "{}")
                    }
                    ("GET", path) if path.starts_with(&format!("/images/{IMAGE_REF}/json")) => {
                        write_json(&mut stream, 200, "{}")
                    }
                    ("GET", path) if path.starts_with("/images/") => {
                        write_json(&mut stream, 404, r#"{"message":"No such image"}"#)
                    }
                    ("GET", path)
                        if path.starts_with(&format!("/containers/{CONTAINER_NAME}/json")) =>
                    {
                        write_json(&mut stream, 404, r#"{"message":"No such container"}"#)
                    }
                    ("POST", path) if path.starts_with("/containers/create?") => {
                        let body: serde_json::Value = serde_json::from_str(&request.body)
                            .expect("container create body JSON");
                        if body["Image"] == "alpine:latest" {
                            let instance_dir =
                                project.join(format!(".dctl/servers/{INSTANCE_KEY}"));
                            match std::fs::remove_dir_all(instance_dir) {
                                Ok(()) => {}
                                Err(error) if error.kind() == ErrorKind::NotFound => {}
                                Err(error) => panic!("simulate privileged data cleanup: {error}"),
                            }
                            write_json(&mut stream, 201, r#"{"Id":"cleanup-id","Warnings":[]}"#);
                        } else {
                            assert!(!existing, "resumed start created a new container");
                            started = false;
                            write_json(&mut stream, 201, r#"{"Id":"fk-id","Warnings":[]}"#);
                        }
                    }
                    ("POST", "/containers/fk-id/start") => {
                        started = true;
                        let data_dir = project.join(format!(".dctl/servers/{INSTANCE_KEY}/data"));
                        std::fs::create_dir_all(&data_dir).expect("create simulated data dir");
                        write_response(&mut stream, 204, "application/json", b"");
                    }
                    ("GET", "/containers/fk-id/json") => {
                        let running = started && matches!(outcome, ContainerOutcome::Running);
                        let state = if running {
                            r#"{"Status":"running","Running":true,"Paused":false,"ExitCode":0,"OOMKilled":false}"#
                        } else {
                            r#"{"Status":"exited","Running":false,"Paused":false,"ExitCode":1,"OOMKilled":false}"#
                        };
                        let body = format!(
                            r#"{{"Id":"fk-id","State":{state},"Config":{{"Env":["REDIS_ARGS=--requirepass stored-secret"]}}}}"#
                        );
                        write_json(&mut stream, 200, &body);
                    }
                    ("POST", "/containers/fk-id/exec") => {
                        let exit_code = readiness_exit_codes.pop_front().unwrap_or(1);
                        let exec_id = format!("exec-{next_exec}");
                        next_exec += 1;
                        exec_exit_codes.insert(exec_id.clone(), exit_code);
                        write_json(&mut stream, 201, &format!(r#"{{"Id":"{exec_id}"}}"#));
                    }
                    ("POST", path) if path.starts_with("/exec/") && path.ends_with("/start") => {
                        write_response(&mut stream, 200, "application/json", b"");
                    }
                    ("GET", path) if path.starts_with("/exec/") && path.ends_with("/json") => {
                        let exec_id = path.trim_start_matches("/exec/").trim_end_matches("/json");
                        let exit_code = exec_exit_codes
                            .get(exec_id)
                            .expect("inspect unknown fake exec");
                        write_json(
                            &mut stream,
                            200,
                            &format!(
                                r#"{{"ID":"{exec_id}","Running":false,"ExitCode":{exit_code}}}"#
                            ),
                        );
                    }
                    ("GET", path) if path.starts_with("/containers/fk-id/logs?") => {
                        write_response(
                            &mut stream,
                            200,
                            "application/vnd.docker.raw-stream",
                            &docker_log_stream(&logs),
                        );
                    }
                    ("POST", path) if path.starts_with("/containers/fk-id/stop?") => {
                        write_response(&mut stream, 204, "application/json", b"")
                    }
                    ("POST", "/containers/cleanup-id/start") => {
                        write_response(&mut stream, 204, "application/json", b"")
                    }
                    ("POST", "/containers/cleanup-id/wait") => {
                        write_json(&mut stream, 200, r#"{"StatusCode":0}"#)
                    }
                    ("DELETE", path) if path.starts_with("/containers/fk-id?") => {
                        let status = remove_statuses.pop_front().unwrap_or(204);
                        if status == 204 {
                            started = false;
                            write_response(&mut stream, 204, "application/json", b"");
                        } else {
                            write_json(
                                &mut stream,
                                status,
                                r#"{"message":"remove failed by test"}"#,
                            )
                        }
                    }
                    _ => panic!("unexpected fake Docker request: {request:?}"),
                }
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Self {
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<DockerRequest> {
        self.requests.lock().unwrap().clone()
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
        metadata_lock_available: false,
    }
}

fn metadata_lock_available(project: &Path) -> bool {
    let path = project.join(".dctl/servers/.metadata.lock");
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
    else {
        return false;
    };
    file.try_lock().is_ok()
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
        500 => "Internal Server Error",
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

fn docker_log_stream(lines: &[String]) -> Vec<u8> {
    let mut body = Vec::new();
    for line in lines {
        let message = format!("{line}\n");
        body.extend_from_slice(&[2, 0, 0, 0]);
        body.extend_from_slice(&(message.len() as u32).to_be_bytes());
        body.extend_from_slice(message.as_bytes());
    }
    body
}

fn reserve_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve falkordb port")
        .local_addr()
        .expect("reserved address")
        .port()
}

struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
    socket: PathBuf,
    docker: FakeDocker,
    port: u16,
    browser_port: u16,
}

fn setup(scenario: DockerScenario) -> Project {
    let dir = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");
    let socket = dir.path().join("docker.sock");
    let docker = FakeDocker::start(&socket, dir.path(), scenario);
    Project {
        dir,
        home,
        socket,
        docker,
        port: reserve_port(),
        browser_port: reserve_port(),
    }
}

impl Project {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(dctl_binary())
            .env_clear()
            .env("HOME", self.home.path())
            .env("PATH", "/usr/bin:/bin")
            .env("DOCKER_HOST", format!("unix://{}", self.socket.display()))
            .current_dir(self.dir.path())
            .args(args)
            .output()
            .expect("run dctl subprocess")
    }

    fn metadata_path(&self) -> PathBuf {
        self.dir
            .path()
            .join(format!(".dctl/servers/{INSTANCE_KEY}.json"))
    }

    fn start_args(&self, extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = [
            "local",
            "--json",
            "falkordb",
            "start",
            "--port",
            &self.port.to_string(),
            "--browser-port",
            &self.browser_port.to_string(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        args
    }
}

fn start_guard() -> std::sync::MutexGuard<'static, ()> {
    START_COMMAND_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn fresh_start_waits_for_authenticated_ping_then_succeeds() {
    let _guard = start_guard();
    let project = setup(DockerScenario {
        readiness_exit_codes: vec![1, 0],
        ..DockerScenario::default()
    });

    let output = project.run(
        &project
            .start_args(&[])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("start JSON output");
    assert_eq!(json["name"], "default");
    assert_eq!(json["image"], "falkordb:v4.20.6");
    assert_eq!(json["port"], project.port);
    assert_eq!(json["browser_port"], project.browser_port);
    let password = json["password"].as_str().expect("password in output");
    assert_eq!(password.len(), 24, "generated password is 24 chars");

    // Metadata carries the falkordb engine, both ports, and the container id.
    let metadata: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.metadata_path()).expect("metadata file"),
    )
    .expect("metadata JSON");
    assert_eq!(metadata["engine"], "falkordb");
    assert_eq!(metadata["tcp_port"], project.port);
    assert_eq!(metadata["http_port"], project.browser_port);
    assert_eq!(metadata["container_id"], "fk-id");

    let requests = project.docker.requests();
    // The create body binds both container ports on loopback with our labels.
    let create = requests
        .iter()
        .find(|r| r.method == "POST" && r.path.starts_with("/containers/create?"))
        .expect("container create request");
    let body: serde_json::Value = serde_json::from_str(&create.body).expect("create body JSON");
    assert_eq!(body["Image"], IMAGE_REF);
    let env = body["Env"].as_array().expect("env array");
    let redis_args = env
        .iter()
        .find_map(|e| e.as_str().and_then(|s| s.strip_prefix("REDIS_ARGS=")))
        .expect("REDIS_ARGS env present");
    assert_eq!(redis_args, format!("--requirepass {password}"));
    let port_bindings = &body["HostConfig"]["PortBindings"];
    assert_eq!(
        port_bindings["6379/tcp"][0]["HostPort"],
        project.port.to_string()
    );
    assert_eq!(
        port_bindings["3000/tcp"][0]["HostPort"],
        project.browser_port.to_string()
    );
    assert_eq!(body["Labels"]["dctl.engine"], "falkordb");
    assert_eq!(body["Labels"]["dctl.major"], "4.20.6");

    // The readiness probe is an authenticated redis-cli ping.
    let exec_create = requests
        .iter()
        .find(|r| r.method == "POST" && r.path == "/containers/fk-id/exec")
        .expect("exec create request");
    let exec_body: serde_json::Value =
        serde_json::from_str(&exec_create.body).expect("exec body JSON");
    let cmd = exec_body["Cmd"].as_array().expect("cmd array");
    let cmd: Vec<&str> = cmd.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(cmd.first(), Some(&"redis-cli"));
    assert!(cmd.contains(&"--no-auth-warning"));
    assert_eq!(cmd.last(), Some(&"ping"));
    // G3: authentication rides the exec env (REDISCLI_AUTH), never argv —
    // argv would leak the password into the container's process listing.
    assert!(
        !cmd.contains(&"-a"),
        "probe argv must not carry the password: {cmd:?}"
    );
    let exec_env = exec_body["Env"].as_array().expect("exec env array");
    assert!(
        exec_env
            .iter()
            .any(|e| e.as_str() == Some(&format!("REDISCLI_AUTH={password}"))),
        "probe env carries the password: {exec_env:?}"
    );

    // Slow Docker work (image inspect / pull) happens outside the metadata
    // lock; create and start are fast state mutations that hold it by design.
    for request in requests.iter().filter(|r| {
        (r.method == "GET" && r.path.starts_with(&format!("/images/{IMAGE_REF}/json")))
            || (r.method == "POST" && r.path.starts_with("/images/create"))
    }) {
        assert!(
            request.metadata_lock_available,
            "metadata lock held during {}: {:?}",
            request.path, request
        );
    }
}

#[test]
fn immediate_exit_rolls_back_container_and_metadata() {
    let _guard = start_guard();
    let project = setup(DockerScenario {
        outcome: ContainerOutcome::ImmediateExit,
        logs: vec!["falkordb: module load failed".to_string()],
        ..DockerScenario::default()
    });

    let output = project.run(
        &project
            .start_args(&[])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_eq!(output.status.code(), Some(1));

    // JSON mode renders the structured envelope: the redacted startup_exit
    // summary names the engine; the raw log tail stays in the human output.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("startup_exit"), "code: {stderr}");
    assert!(
        stderr.contains("FalkorDB"),
        "engine named in error: {stderr}"
    );

    let requests = project.docker.requests();
    assert!(
        requests
            .iter()
            .any(|r| r.method == "DELETE" && r.path.starts_with("/containers/fk-id?")),
        "rollback removed the failed container"
    );
    assert!(
        !project.metadata_path().exists(),
        "rollback removed the fresh metadata"
    );
}

#[test]
fn startup_timeout_rolls_back_fresh_start() {
    let _guard = start_guard();
    let project = setup(DockerScenario::default());

    let args = project.start_args(&["--wait-timeout", "1"]);
    let output = project.run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(output.status.code(), Some(1));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not become ready within 1 seconds"),
        "timeout message: {stderr}"
    );
    assert!(
        !project.metadata_path().exists(),
        "rollback removed the fresh metadata"
    );
}

#[test]
fn resume_reuses_container_and_reads_stored_password() {
    let _guard = start_guard();
    let project = setup(DockerScenario {
        existing: true,
        readiness_exit_codes: vec![0],
        ..DockerScenario::default()
    });

    // Pre-existing stopped instance metadata pointing at fk-id.
    let servers = project.dir.path().join(".dctl/servers");
    std::fs::create_dir_all(&servers).expect("create servers dir");
    let metadata = serde_json::json!({
        "name": INSTANCE_KEY,
        "pid": 0,
        "version": "falkordb:v4.20.6",
        "http_port": project.browser_port,
        "tcp_port": project.port,
        "started_at": "earlier",
        "cwd": project.dir.path().canonicalize().unwrap(),
        "engine": "falkordb",
        "container_id": "fk-id"
    });
    std::fs::write(
        servers.join(format!("{INSTANCE_KEY}.json")),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .expect("write existing metadata");

    let output = project.run(
        &project
            .start_args(&[])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("resume JSON output");
    assert_eq!(json["password"], "stored-secret", "password read from env");

    let requests = project.docker.requests();
    assert!(
        requests
            .iter()
            .any(|r| r.method == "POST" && r.path == "/containers/fk-id/start"),
        "resume started the existing container"
    );
    assert!(
        !requests
            .iter()
            .any(|r| r.method == "POST" && r.path.starts_with("/containers/create?")),
        "resume did not create a new container"
    );
    assert!(
        !requests.iter().any(|r| r.method == "DELETE"),
        "resume removed nothing"
    );
}

#[test]
fn remove_running_instance_is_refused() {
    let _guard = start_guard();
    let project = setup(DockerScenario {
        existing: true,
        readiness_exit_codes: vec![0],
        ..DockerScenario::default()
    });

    let servers = project.dir.path().join(".dctl/servers");
    std::fs::create_dir_all(&servers).expect("create servers dir");
    let metadata = serde_json::json!({
        "name": INSTANCE_KEY,
        "pid": 0,
        "version": "falkordb:v4.20.6",
        "http_port": project.browser_port,
        "tcp_port": project.port,
        "started_at": "earlier",
        "cwd": project.dir.path().canonicalize().unwrap(),
        "engine": "falkordb",
        "container_id": "fk-id"
    });
    std::fs::write(
        servers.join(format!("{INSTANCE_KEY}.json")),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .expect("write running metadata");

    // Bring the instance up through a resume so liveness (container running)
    // is real, then refuse the removal.
    let output = project.run(
        &project
            .start_args(&[])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert!(output.status.success(), "resume for running state");

    let remove = project.run(&["local", "--json", "falkordb", "remove"]);
    assert_eq!(remove.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&remove.stderr);
    assert!(stderr.contains("server_running"), "code: {stderr}");
    assert!(
        stderr.contains("dctl local falkordb stop"),
        "recovery command: {stderr}"
    );
}
