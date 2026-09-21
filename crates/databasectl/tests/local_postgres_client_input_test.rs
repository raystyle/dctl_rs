//! Native-client coverage for the Postgres query leg (ADR-0009): input
//! resolution failures and dead endpoints degrade to clean errors. The
//! live query round-trip runs on lan-linux against a real daemon.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

fn run_dctl(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(dctl_binary())
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .args(args)
        .output()
        .expect("run dctl")
}

#[test]
fn direct_connect_to_a_dead_port_reports_a_connection_error() {
    // Bind then drop: a guaranteed-closed port on loopback.
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let home = tempfile::tempdir().unwrap();
    let output = run_dctl(
        home.path(),
        &[
            "local",
            "postgres",
            "client",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "-q",
            "SELECT 1",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not connect to Postgres"), "{stderr}");
}

#[test]
fn extra_psql_arguments_are_rejected_outside_interactive_mode() {
    let home = tempfile::tempdir().unwrap();
    let output = run_dctl(
        home.path(),
        &[
            "local",
            "postgres",
            "client",
            "--host",
            "127.0.0.1",
            "-q",
            "SELECT 1",
            "--",
            "-v",
            "ON_ERROR_STOP=1",
        ],
    );
    // Cross-flag usage rejections ride clap's usage face: exit 2.
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("interactive mode"), "{stderr}");
}

#[test]
fn missing_queries_file_fails_before_any_connection() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("no-such-file.sql");
    let output = run_dctl(
        home.path(),
        &[
            "local",
            "postgres",
            "client",
            "--host",
            "127.0.0.1",
            "--queries-file",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no-such-file.sql"), "{stderr}");
}
