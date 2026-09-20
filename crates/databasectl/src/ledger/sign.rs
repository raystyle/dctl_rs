//! The ledger write-path signing scheme.
//!
//! Every write POST carries five required headers: `Idempotency-Key`,
//! `X-Key-Id`, `X-Timestamp` (unix seconds, ±60s window server-side),
//! `X-Nonce` (single-use within 10 minutes), and `X-Signature`. The
//! signature base is the newline-joined `v1` scheme string over the method,
//! path, timestamp, nonce, idempotency key, and the sha256 hex of the exact
//! request body bytes; the signature is Ed25519 over that base, base64url
//! encoded. The body is serialized once and those exact bytes are both
//! hashed and sent, so a serialization drift cannot desynchronize the
//! signature from the payload.

use crate::error::{Error, Result};
use ed25519_dalek::Signer;
use sha2::{Digest, Sha256};

pub(crate) const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";
pub(crate) const KEY_ID_HEADER: &str = "X-Key-Id";
pub(crate) const TIMESTAMP_HEADER: &str = "X-Timestamp";
pub(crate) const NONCE_HEADER: &str = "X-Nonce";
pub(crate) const SIGNATURE_HEADER: &str = "X-Signature";

/// The `v1` signing base: seven lines, newline-joined (no trailing newline).
pub(crate) fn signing_base(
    method: &str,
    path: &str,
    timestamp: &str,
    nonce: &str,
    idempotency_key: &str,
    body_sha256_hex: &str,
) -> String {
    format!("v1\n{method}\n{path}\n{timestamp}\n{nonce}\n{idempotency_key}\n{body_sha256_hex}")
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The complete set of headers for one signed POST. `path` is the URL path
/// with query (the server signs over `url.pathname` — path only, no query;
/// POST bodies carry everything, so signed writes use bare paths).
pub(crate) struct SignedRequest {
    pub path: String,
    pub body: Vec<u8>,
    pub headers: Vec<(&'static str, String)>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_post(
    key: &ed25519_dalek::SigningKey,
    kid: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
    timestamp: u64,
    nonce: String,
    idempotency_key: String,
) -> Result<SignedRequest> {
    let timestamp = timestamp.to_string();
    let body_hash = sha256_hex(&body);
    let base = signing_base(
        method,
        path,
        &timestamp,
        &nonce,
        &idempotency_key,
        &body_hash,
    );
    let signature = key.sign(base.as_bytes());
    use base64::Engine;
    let signature_b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
    Ok(SignedRequest {
        path: path.to_string(),
        body,
        headers: vec![
            (IDEMPOTENCY_HEADER, idempotency_key),
            (KEY_ID_HEADER, kid.to_string()),
            (TIMESTAMP_HEADER, timestamp),
            (NONCE_HEADER, nonce),
            (SIGNATURE_HEADER, signature_b64),
        ],
    })
}

/// A fresh random idempotency key / nonce. The server replays same-key +
/// same-content writes and rejects same-key + different-content with 409;
/// a fresh key per invocation makes every run a distinct submission.
pub(crate) fn new_idempotency_key() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn new_nonce() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Digest inputs must be `sha256:` followed by exactly 64 lowercase hex
/// characters; the ledger stores content hashes, never binaries.
pub(crate) fn validate_digest(digest: &str) -> Result<()> {
    let valid = digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    });
    if valid {
        Ok(())
    } else {
        Err(Error::Ledger(format!(
            "invalid digest '{digest}': expected sha256 followed by a colon and 64 lowercase hex characters"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify a base64url signature over a signing base. Test-facing helper:
    /// production code only signs.
    fn verify_signature(
        public: &ed25519_dalek::VerifyingKey,
        base: &str,
        signature_b64url: &str,
    ) -> bool {
        use base64::Engine;
        use ed25519_dalek::Verifier;
        let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(signature_b64url)
        else {
            return false;
        };
        let Ok(signature) = ed25519_dalek::Signature::from_slice(&bytes) else {
            return false;
        };
        public.verify(base.as_bytes(), &signature).is_ok()
    }

    #[test]
    fn signing_base_is_seven_newline_joined_lines() {
        assert_eq!(
            signing_base(
                "POST",
                "/repos/r/issues",
                "1700000000",
                "nonce-1",
                "idem-1",
                "aa00",
            ),
            "v1\nPOST\n/repos/r/issues\n1700000000\nnonce-1\nidem-1\naa00"
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn signed_post_headers_verify_against_the_base() {
        let signing = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let body = br#"{"title":"t"}"#.to_vec();
        let kid = "kid-test";
        let signed = sign_post(
            &signing,
            kid,
            "POST",
            "/repos/r/issues",
            body.clone(),
            1700000000,
            "nonce-9".into(),
            "idem-9".into(),
        )
        .unwrap();

        assert_eq!(signed.body, body);
        let headers: std::collections::HashMap<_, _> = signed.headers.iter().cloned().collect();
        assert_eq!(headers.get(IDEMPOTENCY_HEADER).unwrap(), "idem-9");
        assert_eq!(headers.get(KEY_ID_HEADER).unwrap(), kid);
        assert_eq!(headers.get(TIMESTAMP_HEADER).unwrap(), "1700000000");
        assert_eq!(headers.get(NONCE_HEADER).unwrap(), "nonce-9");
        let signature = headers.get(SIGNATURE_HEADER).unwrap();
        let base = signing_base(
            "POST",
            "/repos/r/issues",
            "1700000000",
            "nonce-9",
            "idem-9",
            &sha256_hex(&body),
        );
        assert!(verify_signature(&signing.verifying_key(), &base, signature));
        // Any tampering with one field breaks verification.
        let wrong = signing_base(
            "POST",
            "/repos/r/issues",
            "1700000001",
            "nonce-9",
            "idem-9",
            &sha256_hex(&body),
        );
        assert!(!verify_signature(
            &signing.verifying_key(),
            &wrong,
            signature
        ));
    }

    #[test]
    fn idempotency_keys_are_fresh_and_well_formed() {
        let a = new_idempotency_key();
        let b = new_idempotency_key();
        assert_ne!(a, b, "each invocation must mint a new key");
        assert_eq!(a.len(), 36, "uuid v4 hyphenated form");
        let nonce = new_nonce();
        assert_eq!(nonce.len(), 36);
    }

    #[test]
    fn digest_validation_accepts_only_sha256_colon_64_lowercase_hex() {
        validate_digest("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            .unwrap();
        for bad in [
            "sha256:ABC",
            "sha256:",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "md5:ba7816bf8f01cfea414140de5dae2223",
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015a",
        ] {
            assert!(validate_digest(bad).is_err(), "{bad} should be rejected");
        }
    }
}
