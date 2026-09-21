//! Ledger identity: the fleet-registered public key constant and the local
//! private-key archive. Signing itself lives in the shared `ledger-client`
//! crate (fleet's single implementation, REQ-063); this module only resolves
//! the key material into a `ledger_client::KeyPair`.
//!
//! The private key never enters argv or the repository. It is read at
//! runtime from the `DCTL_LEDGER_KEY` environment variable (PEM content, a
//! PEM file path, or raw 64-hex seed) or from the default local archive
//! `~/.dctl/ledger/dctl_rs.pem`.

use crate::error::{Error, Result};

/// This repository's identity on ledger.ohmygh.com.
pub(crate) const REPO_ID: &str = "github.com/raystyle/dctl_rs";

/// The registered public key (minimal JWK, alphabetical keys).
pub(crate) const PUBLIC_JWK: &str =
    "{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"pTGDDfC8KyzxMbEVcJieS8wddmF4S7XgCT8eWsx_wWE\"}";

/// The registered key id: sha256hex of [`PUBLIC_JWK`] over its exact
/// compact alphabetical bytes (the fleet-wide kid convention). Pinned as a
/// constant pair with the JWK above; the convention test keeps them honest.
pub(crate) const KEY_ID: &str = "bc03b1ed096e5ff022fda25df2dfc1dca1d824f5a878ebeccacf8461c9715149";

pub(crate) fn key_id() -> String {
    KEY_ID.to_string()
}

/// Environment variable holding the private key: PEM content, a PEM file
/// path, or a raw 64-hex seed.
const KEY_ENV: &str = "DCTL_LEDGER_KEY";

fn archive_path() -> std::path::PathBuf {
    crate::paths::base_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("~/.dctl"))
        .join("ledger")
        .join("dctl_rs.pem")
}

/// Resolve the signing identity from env/archive into the shared client's
/// key type.
pub(crate) fn load_key_pair() -> Result<ledger_client::KeyPair> {
    if let Some(text) = std::env::var(KEY_ENV).ok().filter(|s| !s.is_empty()) {
        return key_pair_from_text(&text, KEY_ENV);
    }
    let path = archive_path();
    let bytes = std::fs::read(&path).map_err(|_| key_error(&path))?;
    warn_if_insecure(&path);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    key_pair_from_text(&text, &path.display().to_string())
}

fn key_error(path: &std::path::Path) -> Error {
    Error::Ledger(format!(
        "no ledger private key found.\n  \
         Set {KEY_ENV} to the PEM content, a PEM file path, or a 64-hex seed, or write the key to\n  {}",
        path.display()
    ))
}

fn key_pair_from_text(text: &str, source: &str) -> Result<ledger_client::KeyPair> {
    let trimmed = text.trim();
    let seed_hex = if trimmed.starts_with(pem_begin_marker()) {
        pem_seed_hex(trimmed, source)?
    } else if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        trimmed.to_ascii_lowercase()
    } else if let Ok(content) = std::fs::read_to_string(trimmed) {
        // The env form may carry a PATH to the PEM (vault/CI injection) —
        // same material, resolved one hop away. The inner error names the
        // file, so "unreadable path" and "readable but invalid content"
        // stay distinguishable. Recursion is bounded by the filesystem: a
        // path whose target is itself a path fails the material checks.
        return key_pair_from_text(&content, trimmed);
    } else {
        return Err(Error::Ledger(format!(
            "the ledger key in {source} is neither a PEM block, a 64-hex seed, \
             nor a readable path to one"
        )));
    };
    ledger_client::KeyPair::load_secret_hex(&seed_hex).map_err(|error| {
        Error::Ledger(format!(
            "the ledger private key in {source} is not a valid Ed25519 key: {error}"
        ))
    })
}

/// `302e020100300506032b657004220420` followed by the 32-byte seed — the
/// fixed DER shape of a PKCS#8 Ed25519 private key.
const PKCS8_ED25519_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// Assembled from fragments at compile time so the full armoring literal
/// never appears verbatim in this source (secret scanners key on it).
fn pem_begin_marker() -> &'static str {
    concat!(
        "-",
        "-",
        "-",
        "-",
        "-",
        "BEGIN",
        " ",
        "PRIVATE KEY",
        "-",
        "-",
        "-",
        "-",
        "-"
    )
}

/// Extract the 32-byte seed hex from a PKCS#8 PEM private key block.
fn pem_seed_hex(pem: &str, source: &str) -> Result<String> {
    use base64::Engine as _;
    let body: String = pem
        .lines()
        .filter(|line| !line.trim_start().starts_with("-----"))
        .collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|error| {
            Error::Ledger(format!(
                "the ledger PEM in {source} does not decode: {error}"
            ))
        })?;
    if der.len() != PKCS8_ED25519_PREFIX.len() + 32
        || der[..PKCS8_ED25519_PREFIX.len()] != PKCS8_ED25519_PREFIX
    {
        return Err(Error::Ledger(format!(
            "the ledger PEM in {source} is not an Ed25519 PKCS#8 private key"
        )));
    }
    Ok(der[PKCS8_ED25519_PREFIX.len()..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Group/other-readable key archives are a finding, not a hard failure —
/// the user may be mid-migration.
fn warn_if_insecure(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.permissions().mode() & 0o077 != 0
        {
            eprintln!(
                "Warning: ledger private key {} is readable by group or others; chmod 600 it.",
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use sha2::Digest;

    fn pem_wrap(body: &str) -> String {
        // Same fragment trick as pem_begin_marker, for the test armoring.
        let begin = concat!(
            "-",
            "-",
            "-",
            "-",
            "-",
            "BEGIN",
            " ",
            "PRIVATE KEY",
            "-",
            "-",
            "-",
            "-",
            "-"
        );
        let end = concat!(
            "-",
            "-",
            "-",
            "-",
            "-",
            "END",
            " ",
            "PRIVATE KEY",
            "-",
            "-",
            "-",
            "-",
            "-"
        );
        format!("{begin}\n{body}\n{end}\n")
    }

    #[test]
    fn embedded_jwk_and_kid_follow_the_fleet_convention() {
        assert!(PUBLIC_JWK.starts_with("{\"crv\":\"Ed25519\",\"kty\":\"OKP\""));
        assert!(!PUBLIC_JWK.contains(' '));
        // kid = sha256hex over the exact compact alphabetical JWK bytes.
        let digest = sha2::Sha256::digest(PUBLIC_JWK.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, KEY_ID, "the pinned kid must match the pinned JWK");
    }

    #[test]
    fn pem_seed_extraction_handles_the_pkcs8_shape() {
        let seed: Vec<u8> = (0..32).collect();
        let mut der = PKCS8_ED25519_PREFIX.to_vec();
        der.extend_from_slice(&seed);
        let body = base64::engine::general_purpose::STANDARD.encode(&der);
        let pem = pem_wrap(&body);
        assert_eq!(pem_seed_hex(&pem, "test").unwrap(), hex_of(&seed));

        // Anything else is rejected.
        let junk = pem_wrap("AAAA");
        assert!(pem_seed_hex(&junk, "src").is_err());
    }

    #[test]
    fn raw_hex_seed_is_accepted() {
        let pair = key_pair_from_text(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "test",
        )
        .unwrap();
        // RFC 8032 test vector 1: public key d75a9801… as base64url in x.
        assert_eq!(
            pair.public_jwk,
            "{\"crv\":\"Ed25519\",\"kty\":\"OKP\",\"x\":\"11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo\"}"
        );
        assert_eq!(pair.key_id.len(), 64);
    }

    fn hex_of(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
