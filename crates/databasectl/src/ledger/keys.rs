//! Ledger identity: the embedded public key JWK and private-key loading.
//!
//! Per the ledger contract (upstream REQ-063) every repo carries one
//! Ed25519 keypair: the public JWK ships as a constant in the CLI (the
//! identity distribution surface — the CLI itself carries the material the
//! server verifies against), and the private key never enters the repo or
//! argv. It is read at runtime from the `DCTL_LEDGER_KEY` environment
//! variable (either PEM content or a path to a PEM file) or from the default
//! local archive `~/.dctl/ledger/dctl_rs.pem`.

use crate::error::{Error, Result};
use ed25519_dalek::SigningKey;

/// The repo this CLI files under: the normalized remote.
pub(crate) const REPO_ID: &str = "github.com/raystyle/dctl_rs";

/// Minimal public JWK for this repo's ledger identity: {"crv","kty","x"}.
pub(crate) const PUBLIC_JWK: &str =
    r#"{"crv":"Ed25519","kty":"OKP","x":"pTGDDfC8KyzxMbEVcJieS8wddmF4S7XgCT8eWsx_wWE"}"#;

/// The key id: sha256 over the canonical compact JSON with keys in
/// alphabetical order (crv, kty, x) — which is exactly how [`PUBLIC_JWK`]
/// is spelled, so the constant is already canonical.
pub(crate) fn key_id() -> String {
    kid_from_jwk(PUBLIC_JWK)
}

pub(crate) fn kid_from_jwk(jwk: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(jwk.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Environment variable holding the private key: PEM content, or a path to
/// a PEM file.
const KEY_ENV: &str = "DCTL_LEDGER_KEY";

/// Default private-key archive (0600).
fn default_key_path() -> Option<std::path::PathBuf> {
    crate::paths::base_dir()
        .ok()
        .map(|base| base.join("ledger").join("dctl_rs.pem"))
}

fn key_guidance() -> String {
    format!(
        "Set {KEY_ENV} to the PEM content or a PEM file path, or write the key to\n  {}",
        default_key_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<~/.dctl/ledger/dctl_rs.pem>".to_string())
    )
}

/// Load the repo's Ed25519 signing key. Never takes the key from argv.
pub(crate) fn load_signing_key() -> Result<SigningKey> {
    if let Some(value) = std::env::var_os(KEY_ENV)
        && !value.is_empty()
    {
        let text = value.to_string_lossy().to_string();
        if text.contains("BEGIN") {
            return parse_pem(&text);
        }
        let path = std::path::PathBuf::from(text);
        let bytes = std::fs::read_to_string(&path).map_err(|source| {
            Error::Ledger(format!(
                "could not read the ledger private key at {}: {source}. Check the \
                 DCTL_LEDGER_KEY path; the default archive lives at \
                 ~/.dctl/ledger/dctl_rs.pem",
                path.display()
            ))
        })?;
        return parse_pem(&bytes);
    }

    let Some(path) = default_key_path() else {
        return Err(Error::Ledger(key_guidance()));
    };
    let bytes = std::fs::read_to_string(&path).map_err(|source| {
        Error::Ledger(format!(
            "no ledger private key found ({}: {source}). {}",
            path.display(),
            key_guidance()
        ))
    })?;
    parse_pem(&bytes)
}

fn parse_pem(text: &str) -> Result<SigningKey> {
    use ed25519_dalek::pkcs8::DecodePrivateKey;
    SigningKey::from_pkcs8_pem(text).map_err(|source| {
        Error::Ledger(format!(
            "the ledger private key is not a valid Ed25519 PEM: {source}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_jwk_is_canonical_compact_and_alphabetical() {
        let value: serde_json::Value = serde_json::from_str(PUBLIC_JWK).unwrap();
        // Keys in alphabetical order, compact, no spaces: the canonical form
        // the kid hashes over.
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["crv", "kty", "x"]);
        assert_eq!(serde_json::to_string(&value).unwrap(), PUBLIC_JWK);
        assert_eq!(value["kty"], "OKP");
        assert_eq!(value["crv"], "Ed25519");
        // x is base64url of the 32-byte key.
        use base64::Engine;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value["x"].as_str().unwrap())
            .unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn kid_is_sha256_of_the_jwk_bytes() {
        use ed25519_dalek::pkcs8::EncodePublicKey;
        // Recompute the whole chain from a fresh keypair to pin the recipe.
        let signing = SigningKey::generate(&mut rand::rng());
        let der = signing.verifying_key().to_public_key_der().unwrap();
        let raw = &der.as_bytes()[der.as_bytes().len() - 32..];
        use base64::Engine;
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let jwk = format!(r#"{{"crv":"Ed25519","kty":"OKP","x":"{x}"}}"#);
        let expected = kid_from_jwk(&jwk);
        assert_eq!(expected.len(), 64);
        assert!(expected.chars().all(|c| c.is_ascii_hexdigit()));
        // Order matters: a differently-ordered JWK yields a different kid.
        let wrong = format!(r#"{{"kty":"OKP","crv":"Ed25519","x":"{x}"}}"#);
        assert_ne!(kid_from_jwk(&wrong), expected);
    }
}
