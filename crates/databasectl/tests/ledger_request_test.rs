//! Subprocess coverage for the ledger command family against a wiremock
//! server: signed write shapes, family pagination reads, and client-side
//! validation. The real binary runs with DCTL_LEDGER_URL pointed at the
//! mock and DCTL_LEDGER_KEY pointing at a throwaway Ed25519 PEM.

use std::path::PathBuf;
use std::process::{Command, Output};

fn dctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dctl"))
}

/// Generate the throwaway keypair exactly the way production archives are
/// made (openssl PEM), and rebuild the verifying key from the raw public
/// bytes so the tests never depend on encode trait plumbing.
fn write_key() -> (tempfile::TempDir, ed25519_dalek::SigningKey) {
    use std::process::Command;
    let dir = tempfile::tempdir().expect("create key dir");
    let pem_path = dir.path().join("key.pem");
    let status = Command::new("openssl")
        .args(["genpkey", "-algorithm", "ed25519"])
        .arg("-out")
        .arg(&pem_path)
        .status()
        .expect("spawn openssl");
    assert!(status.success(), "openssl generated the key");

    let der = Command::new("openssl")
        .args(["pkey", "-in"])
        .arg(&pem_path)
        .args(["-pubout", "-outform", "DER"])
        .output()
        .expect("spawn openssl for the public key");
    assert!(der.status.success());
    let raw = &der.stdout[der.stdout.len() - 32..];
    let public =
        ed25519_dalek::VerifyingKey::from_bytes(raw.try_into().unwrap()).expect("valid public key");
    // The signing key is only needed for its verifying half in these tests.
    // Recover the seed from the PEM via the same loader production uses.
    let signing = signing_key_from_pem(&std::fs::read_to_string(&pem_path).unwrap());
    assert_eq!(signing.verifying_key(), public, "openssl key round-trips");
    (dir, signing)
}

fn signing_key_from_pem(pem: &str) -> ed25519_dalek::SigningKey {
    use ed25519_dalek::pkcs8::DecodePrivateKey;
    ed25519_dalek::SigningKey::from_pkcs8_pem(pem).expect("valid PEM")
}

struct Sandbox {
    home: tempfile::TempDir,
    key_dir: tempfile::TempDir,
    signing: ed25519_dalek::SigningKey,
    mock: wiremock::MockServer,
}

async fn sandbox() -> Sandbox {
    let (key_dir, signing) = write_key();
    Sandbox {
        home: tempfile::tempdir().expect("create home"),
        key_dir,
        signing,
        mock: wiremock::MockServer::start().await,
    }
}

impl Sandbox {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(dctl_binary())
            .env_clear()
            .env("DO_NOT_TRACK", "1")
            .env("HOME", self.home.path())
            .env("PATH", "/usr/bin:/bin")
            .env("DCTL_LEDGER_URL", self.mock.uri())
            .env(
                "DCTL_LEDGER_KEY",
                self.key_dir.path().join("key.pem").display().to_string(),
            )
            .args(args)
            .output()
            .expect("run dctl subprocess")
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Verify the X-Signature header against the v1 base rebuilt from the wire.
fn signature_verifies(signing: &ed25519_dalek::SigningKey, request: &wiremock::Request) -> bool {
    use base64::Engine;
    use ed25519_dalek::Verifier;
    let header = |name: &str| {
        request
            .headers
            .get(name)
            .map(|value| value.to_str().unwrap_or_default().to_string())
    };
    let Some(signature) = header("x-signature") else {
        return false;
    };
    let parts = [
        "v1".to_string(),
        "POST".to_string(),
        request.url.path().to_string(),
        header("x-timestamp").unwrap_or_default(),
        header("x-nonce").unwrap_or_default(),
        header("idempotency-key").unwrap_or_default(),
        sha256_hex(&request.body),
    ];
    let base = parts.join("\n");
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&signature) else {
        return false;
    };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(&bytes) else {
        return false;
    };
    signing
        .verifying_key()
        .verify(base.as_bytes(), &signature)
        .is_ok()
}

const ISSUES_PATH: &str = "/repos/github.com/raystyle/dctl_rs/issues";

#[tokio::test]
async fn issue_new_posts_signed_five_header_request() {
    let sandbox = sandbox().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path(ISSUES_PATH))
        .respond_with(
            wiremock::ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "ok": true, "issue": 7,
                "event": {"event_id": "e1", "seq": 1, "type": "issue_open"},
            })),
        )
        .mount(&sandbox.mock)
        .await;

    let output = sandbox.run(&[
        "ledger",
        "--json",
        "issue",
        "new",
        "--title",
        "Graph query hangs under load",
        "--kind",
        "bug",
        "--acceptance",
        "Query completes within 1s",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["registered"]["issue"], 7);

    let requests = sandbox.mock.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.url.path(), ISSUES_PATH);
    // All five headers present; idempotency key and nonce are uuid-shaped.
    let header = |name: &str| {
        request
            .headers
            .get(name)
            .map(|value| value.to_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    };
    assert_eq!(header("idempotency-key").len(), 36);
    assert_eq!(header("x-nonce").len(), 36);
    header("x-timestamp")
        .parse::<u64>()
        .expect("timestamp is unix seconds");
    // X-Key-Id is the embedded repo identity kid, not the test key's.
    assert_eq!(header("x-key-id"), expected_kid());
    assert!(signature_verifies(&sandbox.signing, request));
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["title"], "Graph query hangs under load");
    assert_eq!(body["kind"], "bug");
    assert_eq!(body["acceptance"], "Query completes within 1s");
}

#[tokio::test]
async fn issue_close_posts_result_then_status_done_in_order() {
    let sandbox = sandbox().await;
    let events_path = format!("{ISSUES_PATH}/3/events");
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path(&events_path))
        .respond_with(
            wiremock::ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "ok": true, "event": {"seq": 2},
            })),
        )
        .mount(&sandbox.mock)
        .await;

    let digest = format!("sha256:{}", "a".repeat(64));
    let output = sandbox.run(&[
        "ledger",
        "--json",
        "issue",
        "close",
        "3",
        "--digest",
        &digest,
        "--note",
        "verified on lan-linux",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = sandbox.mock.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "result then status chain");
    // Server shape: events carry a nested payload (workers/ledger
    // index.ts parsePayload); free text rides the top-level body.
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(first["type"], "result");
    assert_eq!(first["payload"]["digest"], digest);
    assert_eq!(first["body"], "verified on lan-linux");
    assert_eq!(second["type"], "status");
    assert_eq!(second["payload"]["to"], "done");
    // parsePayload requires an object (or absent) — neither may be null.
    assert!(first["payload"].is_object());
    assert!(second["payload"].is_object());
    // Deterministic chain keys: distinct per event type, and a rerun of the
    // same close would send the same pair (server replays instead of
    // appending duplicates).
    let key = |request: &wiremock::Request| {
        request
            .headers
            .get("idempotency-key")
            .map(|value| value.to_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    };
    assert_ne!(key(&requests[0]), key(&requests[1]));
    for request in &requests {
        assert_eq!(key(request).len(), 64, "sha256 hex key: {}", key(request));
    }
}

#[tokio::test]
async fn invalid_digest_is_rejected_before_any_request() {
    let sandbox = sandbox().await;
    let output = sandbox.run(&[
        "ledger",
        "artifact",
        "publish",
        "--name",
        "x",
        "--kind",
        "experience",
        "--digest",
        "sha256:short",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid digest"), "{stderr}");
    assert!(sandbox.mock.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn issue_list_reads_family_pagination_without_signing() {
    let sandbox = sandbox().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(ISSUES_PATH))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issues": [
                    {"issue_n": 9, "kind": "bug", "status": "open", "title": "Third", "assignee": null, "hasResult": false},
                    {"issue_n": 4, "kind": "improvement", "status": "done", "title": "Fourth", "assignee": null, "hasResult": true},
                ],
                "has_more": true,
                "count": 2,
            })),
        )
        .mount(&sandbox.mock)
        .await;

    let output = sandbox.run(&["ledger", "issue", "list", "--limit", "10", "--before", "12"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Fourth"), "{stdout}");
    assert!(
        stdout.contains("more available"),
        "saturation hint: {stdout}"
    );
    assert!(
        stdout.contains("--before 4"),
        "cursor points at the last row: {stdout}"
    );
    assert!(stdout.contains("count 2 (this page)"), "{stdout}");
    // F2/F3: the projection's issue_n renders as the number column and
    // hasResult renders as the Result column (not "-").
    assert!(stdout.contains("| 9 "), "issue_n renders: {stdout}");
    assert!(stdout.contains(" yes "), "hasResult renders: {stdout}");

    let requests = sandbox.mock.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let query = requests[0].url.query().unwrap_or_default();
    assert!(query.contains("limit=10"), "{query}");
    assert!(query.contains("before=12"), "{query}");
    assert!(query.contains("more=1"), "has_more needs more=1: {query}");
    assert!(
        requests[0].headers.get("x-signature").is_none(),
        "reads are unsigned"
    );
}

#[tokio::test]
async fn artifact_publish_carries_metadata_and_409_surfaces_hint() {
    let sandbox = sandbox().await;
    let artifacts_path = "/repos/github.com/raystyle/dctl_rs/artifacts";
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path(artifacts_path))
        .respond_with(
            wiremock::ResponseTemplate::new(409)
                .set_body_string(r#"{"error":"idempotency key reused with different content"}"#),
        )
        .mount(&sandbox.mock)
        .await;

    let digest = format!("sha256:{}", "b".repeat(64));
    let output = sandbox.run(&[
        "ledger",
        "artifact",
        "publish",
        "--name",
        "falkordb-live-smoke",
        "--kind",
        "experience",
        "--digest",
        &digest,
        "--version",
        "0.5.0",
        "--git-range",
        "6d9beeb..96ece81",
        "--deps",
        format!("sha256:{}", "c".repeat(64)).as_str(),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("409"), "{stderr}");
    assert!(stderr.contains("idempotency key collision"), "{stderr}");

    let requests = sandbox.mock.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(signature_verifies(&sandbox.signing, &requests[0]));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["name"], "falkordb-live-smoke");
    assert_eq!(body["kind"], "experience");
    assert_eq!(body["digest"], digest);
    assert_eq!(body["version"], "0.5.0");
    assert_eq!(body["git_range"], "6d9beeb..96ece81");
    assert_eq!(
        body["deps"],
        serde_json::json!([format!("sha256:{}", "c".repeat(64))])
    );
}

#[tokio::test]
async fn ledger_key_prints_embedded_jwk_and_kid() {
    let sandbox = sandbox().await;
    let output = sandbox.run(&["ledger", "--json", "key"]);
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["kid"], expected_kid());
    let jwk: serde_json::Value = serde_json::from_str(json["jwk"].as_str().unwrap()).unwrap();
    assert_eq!(jwk["kty"], "OKP");
    assert_eq!(jwk["crv"], "Ed25519");
}

/// The kid is sha256 of the canonical compact JWK — recomputed here from the
/// `ledger key` output so the constant and the recipe cannot drift apart.
fn expected_kid() -> String {
    sha256_hex(
        br#"{"crv":"Ed25519","kty":"OKP","x":"pTGDDfC8KyzxMbEVcJieS8wddmF4S7XgCT8eWsx_wWE"}"#,
    )
}
