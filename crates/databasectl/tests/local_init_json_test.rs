//! Subprocess coverage for `local init` and the ADR-0012 state layout: the
//! `--json` payload must report the scaffolds the command created, nothing
//! runtime-written may land under the working directory, and a legacy
//! `.dctl/servers` directory migrates into the app-data bucket on first
//! contact.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

fn run(project: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(dctl_binary())
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(project)
        .args(args)
        .output()
        .expect("run dctl")
}

fn stdout_json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is a single JSON object")
}

/// The per-project state bucket under HOME (ADR-0012), mirrored from the
/// binary's `~/.dctl/projects/<id>/servers` address for staging and
/// assertions.
fn bucket_servers(home: &Path, project: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let canonical = project.canonicalize().expect("canonical project path");
    let digest = Sha256::digest(canonical.display().to_string().as_bytes());
    let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    home.join(".dctl").join("projects").join(id).join("servers")
}

#[test]
fn init_json_reports_both_scaffolds_and_no_cwd_state_on_first_run() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    let output = run(project.path(), home.path(), &["local", "init", "--json"]);
    let json = stdout_json(&output);

    assert_eq!(
        json["paths"],
        serde_json::json!(["clickhouse/", "postgres/", "falkordb/"])
    );

    assert!(project.path().join("clickhouse/tables").is_dir());
    assert!(project.path().join("postgres/tables").is_dir());
    assert!(project.path().join("falkordb/queries").is_dir());
    // ADR-0012: runtime state never lands under the working directory.
    assert!(!project.path().join(".dctl").exists());
}

#[test]
fn init_json_on_second_run_reports_no_created_paths() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    run(project.path(), home.path(), &["local", "init", "--json"]);
    let output = run(project.path(), home.path(), &["local", "init", "--json"]);
    let json = stdout_json(&output);

    assert_eq!(json["paths"], serde_json::json!([]));
}

#[test]
fn init_human_output_reports_each_created_path_exactly_once() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    let output = run(project.path(), home.path(), &["local", "init"]);
    assert!(output.status.success());

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("Initialized")),
        "first line should announce initialization: {combined}"
    );
    for path in ["clickhouse/", "postgres/", "falkordb/"] {
        let mentions = combined
            .lines()
            .filter(|line| line.ends_with(&format!(" {path}")))
            .count();
        assert_eq!(
            mentions, 1,
            "expected one line mentioning {path}: {combined}"
        );
    }
}

#[test]
fn init_human_output_second_run_reports_already_initialized() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    run(project.path(), home.path(), &["local", "init"]);
    let output = run(project.path(), home.path(), &["local", "init"]);

    assert!(output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("Already initialized"));
    assert!(!combined.contains("Created project scaffold"));
}

#[test]
fn init_human_output_reports_a_scaffold_only_repair() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    run(project.path(), home.path(), &["local", "init"]);
    std::fs::remove_dir_all(project.path().join("postgres/views"))
        .expect("remove one scaffold directory");
    let output = run(project.path(), home.path(), &["local", "init"]);

    assert!(output.status.success());
    let combined = String::from_utf8_lossy(&output.stdout);
    // A partial repair creates something, so the run counts as initializing;
    // only the repaired scaffold gets a mention line.
    assert!(
        combined
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("Initialized")),
        "first line should announce initialization: {combined}"
    );
    assert_eq!(
        combined
            .lines()
            .filter(|line| line.ends_with(" postgres/"))
            .count(),
        1,
        "expected one scaffold repair line: {combined}"
    );
    assert_eq!(
        combined
            .lines()
            .filter(|line| line.ends_with(" clickhouse/"))
            .count(),
        0,
        "untouched scaffolds get no mention: {combined}"
    );
}

#[test]
fn server_list_leaves_the_working_directory_clean() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    let list = run(project.path(), home.path(), &["local", "server", "list"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    // The lock lives in the app-data bucket, not beside the user's files.
    assert!(
        bucket_servers(home.path(), project.path())
            .join(".metadata.lock")
            .is_file()
    );
    assert!(!project.path().join(".dctl").exists());

    let git_init = Command::new("git")
        .arg("init")
        .current_dir(project.path())
        .output()
        .expect("initialize temporary Git repository");
    assert!(git_init.status.success());
    let status = Command::new("git")
        .args(["status", "--short", "--untracked-files=all"])
        .current_dir(project.path())
        .output()
        .expect("inspect temporary Git repository");
    assert!(status.status.success());
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "git must see no dctl-written path: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn legacy_dctl_servers_migrates_into_the_bucket_on_first_contact() {
    let project = tempfile::tempdir().expect("create project");
    let home = tempfile::tempdir().expect("create home");

    // Stage a legacy layout: one stopped instance with data, plus the
    // stateless lock file that must not move.
    let legacy_servers = project.path().join(".dctl/servers");
    std::fs::create_dir_all(legacy_servers.join("default-ch26.8/data")).unwrap();
    let cwd = project.path().canonicalize().unwrap();
    std::fs::write(
        legacy_servers.join("default-ch26.8.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "name": "default-ch26.8", "pid": 0, "version": "clickhouse:26.8",
            "http_port": 0, "tcp_port": 0, "started_at": "legacy",
            "cwd": cwd, "engine": "clickhouse"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(legacy_servers.join(".metadata.lock"), b"stale").unwrap();
    std::fs::write(project.path().join(".dctl/.gitignore"), "*\n").unwrap();

    let list = run(project.path(), home.path(), &["local", "server", "list"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );

    let bucket = bucket_servers(home.path(), project.path());
    assert!(
        bucket.join("default-ch26.8.json").is_file(),
        "metadata moved into the bucket"
    );
    assert!(
        bucket.join("default-ch26.8/data").is_dir(),
        "data directory moved into the bucket"
    );
    // Emptied legacy shells are removed so the working directory is clean.
    assert!(!project.path().join(".dctl").exists());
}
