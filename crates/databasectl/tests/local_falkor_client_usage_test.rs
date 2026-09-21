//! FalkorDB client usage rejections ride clap's usage face (ADR-0009).

use std::path::PathBuf;
use std::process::Command;

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

#[test]
fn direct_mode_without_query_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(dctl_binary())
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home.path())
        .args(["local", "falkordb", "client", "--host", "127.0.0.1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires --query"), "{stderr}");
}

#[test]
fn passthrough_args_with_a_query_are_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(dctl_binary())
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home.path())
        .args([
            "local",
            "falkordb",
            "client",
            "-q",
            "MATCH (n) RETURN n",
            "--",
            "--scan",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("interactive mode"), "{stderr}");
}
