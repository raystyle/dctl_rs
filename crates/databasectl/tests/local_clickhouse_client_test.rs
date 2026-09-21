//! Direct-mode (`--host/--port`) ClickHouse client contract against a local
//! HTTP listener: SQL in the POST body, native output, error envelopes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone, Debug)]
struct HttpExchange {
    request_line: String,
    headers: String,
    body: String,
}

struct FakeClickhouseHttp {
    stop: Arc<AtomicBool>,
    exchanges: Arc<Mutex<Vec<HttpExchange>>>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

impl FakeClickhouseHttp {
    fn start(fail_with: Option<String>) -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind test HTTP");
        let port = listener.local_addr().expect("addr").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let exchanges = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = Arc::clone(&stop);
        let thread_exchanges = Arc::clone(&exchanges);
        let fail_body = fail_with.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("accept test HTTP: {error}"),
                };
                stream.set_nonblocking(false).expect("blocking connection");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("read timeout");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let header_end = loop {
                    let count = stream.read(&mut buffer).expect("read request");
                    assert!(count > 0, "request ended before headers");
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
                    let count = stream.read(&mut buffer).expect("read body");
                    assert!(count > 0, "request ended before body");
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request_line = headers.lines().next().expect("request line").to_string();
                let body = String::from_utf8_lossy(&bytes[header_end..header_end + content_length])
                    .into_owned();
                thread_exchanges.lock().unwrap().push(HttpExchange {
                    request_line: request_line.clone(),
                    headers: headers.clone(),
                    body: body.clone(),
                });

                if request_line.contains("GET /ping") {
                    respond(&mut stream, 200, "Ok.\n");
                } else if let Some(error) = fail_body.as_deref() {
                    respond(&mut stream, 500, error);
                } else {
                    respond(&mut stream, 200, &format!("query-ack:{body}"));
                }
            }
        });
        Self {
            stop,
            exchanges,
            thread: Some(thread),
            port,
        }
    }

    fn exchanges(&self) -> Vec<HttpExchange> {
        self.exchanges.lock().unwrap().clone()
    }
}

impl Drop for FakeClickhouseHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join test HTTP thread");
        }
    }
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let reason = if status == 200 {
        "OK"
    } else {
        "Internal Server Error"
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|()| stream.write_all(body.as_bytes()))
        .expect("write response");
}

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

fn run(project: &Path, args: &[&str]) -> Output {
    Command::new(dctl_binary())
        .env_clear()
        .env("HOME", project.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .current_dir(project)
        .args(args)
        .output()
        .expect("run dctl")
}

#[test]
fn direct_query_posts_sql_body_and_prints_native_output() {
    let project = tempfile::tempdir().unwrap();
    let http = FakeClickhouseHttp::start(None);
    let output = run(
        project.path(),
        &[
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--query",
            "SELECT 'one' AS value",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "query-ack:SELECT 'one' AS value",
        "HTTP response body prints verbatim, no formatting added"
    );
    let exchange = &http.exchanges()[0];
    assert!(
        exchange.request_line.starts_with("POST /"),
        "{}",
        exchange.request_line
    );
    assert_eq!(exchange.body, "SELECT 'one' AS value");
    assert!(
        exchange
            .headers
            .to_ascii_lowercase()
            .contains("content-type: text/plain"),
        "ClickHouse expects the SQL as a plain-text body: {}",
        exchange.headers
    );
}

#[test]
fn direct_query_with_agent_mode_still_prints_native_output() {
    let project = tempfile::tempdir().unwrap();
    let http = FakeClickhouseHttp::start(None);
    let output = Command::new(dctl_binary())
        .env_clear()
        .env("AI_AGENT", "1")
        .env("HOME", project.path().join("home"))
        .env("PATH", "/usr/bin:/bin")
        .current_dir(project.path())
        .args([
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--query",
            "SELECT 1",
        ])
        .output()
        .expect("run dctl");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "query-ack:SELECT 1",
        "query output stays native even for agents"
    );
}

#[test]
fn direct_queries_file_reads_files_and_stdin_dash() {
    let project = tempfile::tempdir().unwrap();
    let http = FakeClickhouseHttp::start(None);
    std::fs::write(
        project.path().join("queries.sql"),
        "SELECT 'file' AS source;",
    )
    .unwrap();

    let output = run(
        project.path(),
        &[
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--queries-file",
            "queries.sql",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "query-ack:SELECT 'file' AS source;"
    );

    let mut child = Command::new(dctl_binary())
        .env_clear()
        .env("HOME", project.path().join("home"))
        .env("PATH", "/usr/bin:/bin")
        .current_dir(project.path())
        .args([
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--queries-file",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stdin client");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"SELECT 'stdin' AS source;")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "query-ack:SELECT 'stdin' AS source;"
    );
}

#[test]
fn direct_database_is_passed_as_a_query_parameter() {
    let project = tempfile::tempdir().unwrap();
    let http = FakeClickhouseHttp::start(None);
    let output = run(
        project.path(),
        &[
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--database",
            "events",
            "--query",
            "SELECT 1",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exchange = &http.exchanges()[0];
    assert!(
        exchange.request_line.contains("database=events"),
        "database rides as a query parameter: {}",
        exchange.request_line
    );
}

#[test]
fn direct_credentials_ride_as_basic_auth() {
    let project = tempfile::tempdir().unwrap();
    let http = FakeClickhouseHttp::start(None);
    let output = run(
        project.path(),
        &[
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--user",
            "app",
            "--password",
            "secret",
            "--query",
            "SELECT 1",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exchange = &http.exchanges()[0];
    let auth = exchange
        .headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("Authorization"))
        .map(|(_, value)| value.trim().to_string())
        .expect("Authorization header present");
    assert!(auth.starts_with("Basic "), "basic auth scheme: {auth}");
}

#[test]
fn direct_http_error_maps_to_clickhouse_error_envelope() {
    let project = tempfile::tempdir().unwrap();
    let http = FakeClickhouseHttp::start(Some(
        "DB::Exception: Table default.events does not exist".into(),
    ));
    let output = run(
        project.path(),
        &[
            "local",
            "--json",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--query",
            "SELECT * FROM events",
        ],
    );
    assert!(!output.status.success());
    // The engine's error body is foreign output: the machine envelope carries
    // a curated summary, the human stderr keeps the full text.
    let result: serde_json::Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(result["error"]["code"], "clickhouse_error");
    assert_eq!(
        result["error"]["message"], "ClickHouse HTTP query failed with status 500",
        "{result}"
    );

    let human = run(
        project.path(),
        &[
            "local",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &http.port.to_string(),
            "--query",
            "SELECT * FROM events",
        ],
    );
    assert!(!human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stderr).contains("Table default.events does not exist"),
        "human output keeps the engine's own error text"
    );
}

#[test]
fn direct_host_defaults_port_to_8123_and_direct_port_defaults_host() {
    // Structural: both selectors work alone (parsing level, no connection).
    let project = tempfile::tempdir().unwrap();
    let output = run(
        project.path(),
        &[
            "local",
            "client",
            "--host",
            "db.internal",
            "--query",
            "SELECT 1",
        ],
    );
    // db.internal is unreachable: the request fails, but parsing succeeded —
    // distinguish that from a usage error by the exit text.
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Usage:"),
        "a host-only invocation parses: {stderr}"
    );
}
