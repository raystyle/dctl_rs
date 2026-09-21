//! Ledger surface on the shared ledger-client crate (REQ-006): key
//! resolution, pre-flight digest validation, the permission lockdown, and
//! the identity command — all offline-deterministic. The signed HTTP
//! shapes live in ledger-client itself; dctl's write paths get their live
//! proof in the fleet's write-face run (key registered by the registry
//! operator), not against a stub.

use std::path::PathBuf;
use std::process::Command;

fn dctl() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dctl"));
    command
        .env_clear()
        .env("DO_NOT_TRACK", "1")
        .env("PATH", "/usr/bin:/bin");
    command
}

fn home_without_key() -> PathBuf {
    // An empty HOME: no ~/.dctl/ledger/dctl_rs.pem archive to fall back on.
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".dctl")).unwrap();
    home.keep()
}

#[test]
fn key_command_prints_the_registered_identity() {
    let output = dctl()
        .env("HOME", home_without_key())
        .args(["ledger", "key", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        body["jwk"]
            .as_str()
            .unwrap()
            .starts_with("{\"crv\":\"Ed25519\",\"kty\":\"OKP\"")
    );
    assert_eq!(body["kid"].as_str().unwrap().len(), 64);
    assert_eq!(body["repo_id"], "github.com/raystyle/dctl_rs");
}

#[test]
fn missing_key_points_at_the_carrier_and_archive() {
    let output = dctl()
        .env("HOME", home_without_key())
        .args([
            "ledger",
            "issue",
            "new",
            "--title",
            "t",
            "--acceptance",
            "a",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("DCTL_LEDGER_KEY"), "{stderr}");
    assert!(stderr.contains("dctl_rs.pem"), "{stderr}");
}

#[test]
fn garbage_key_fails_before_any_request() {
    for bad in ["not-a-key", "zzzz", "sha256:123"] {
        let output = dctl()
            .env("HOME", home_without_key())
            .env("DCTL_LEDGER_KEY", bad)
            .args([
                "ledger",
                "issue",
                "new",
                "--title",
                "t",
                "--acceptance",
                "a",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{bad}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("neither a PEM block nor a 64-hex seed"),
            "{bad}: {stderr}"
        );
    }
}

#[test]
fn valid_hex_seed_is_accepted_by_the_parser() {
    // RFC 8032 test vector seed: parses, so the failure (if any) is the
    // network round trip, never the key material. Pointing at an unroutable
    // proxy makes the HTTP layer fail fast and deterministically offline.
    let output = dctl()
        .env("HOME", home_without_key())
        .env(
            "DCTL_LEDGER_KEY",
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        )
        .env("https_proxy", "http://127.0.0.1:9")
        .args([
            "ledger",
            "issue",
            "new",
            "--title",
            "t",
            "--acceptance",
            "a",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ledger request failed"),
        "the key parsed; the failure is the (unroutable) request: {stderr}"
    );
    assert!(
        !stderr.contains("neither a PEM block"),
        "key parsing must not be the failure: {stderr}"
    );
}

#[test]
fn reads_do_not_require_the_private_key() {
    // Reads are unsigned GETs; demanding the key for them would lock
    // read-only users out. Offline the request fails, online it answers —
    // either way the key guidance must not appear.
    let output = dctl()
        .env("HOME", home_without_key())
        .args(["ledger", "issue", "list", "--limit", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("no ledger private key found"),
        "reads must stay credential-free: {stderr}"
    );
}

#[test]
fn publish_rejects_malformed_digests_before_anything_else() {
    for bad_digest in ["sha256:short", "md5:aaaa", "sha256:"] {
        let output = dctl()
            .env("HOME", home_without_key())
            .args([
                "ledger",
                "artifact",
                "publish",
                "--name",
                "n",
                "--kind",
                "experience",
                "--digest",
                bad_digest,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{bad_digest}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("digest must be sha256:<64 hex>"),
            "{bad_digest}"
        );
    }
}

/// 权限收口 (REQ-006): the lifecycle moves parse nowhere in this CLI.
#[test]
fn lifecycle_moves_are_not_part_of_the_command_surface() {
    for args in [
        vec!["ledger", "issue", "close", "3"],
        vec!["ledger", "artifact", "promote", "art-7"],
        vec!["ledger", "artifact", "attest", "art-7", "--kind", "demote"],
        vec![
            "ledger",
            "artifact",
            "attest",
            "art-7",
            "--kind",
            "supersede",
        ],
    ] {
        let output = dctl()
            .env("HOME", home_without_key())
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Usage:") || stderr.contains("invalid value"),
            "{args:?}: {stderr}"
        );
    }
}
