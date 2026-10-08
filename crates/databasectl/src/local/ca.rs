//! Local dctl CA and per-instance certificate issuance (ADR-0011).
//!
//! A single global CA at `~/.dctl/ca/` issues per-instance server
//! certificates and the dctl client certificate. The CA key persists on
//! disk; the ephemeral `Certificate` object is reconstructed from the same
//! key + params each time (same subject DN, so the signing chain holds).
//! Private keys are 0600; nothing enters argv, logs, or the repository.

use crate::error::{Error, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, Issuer, KeyPair};
use std::fs::File;
use std::path::{Path, PathBuf};

const CA_CN: &str = "dctl-local-ca";

fn ca_err(context: &str, source: impl std::fmt::Display) -> Error {
    Error::Postgres(format!("CA {context}: {source}"))
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, CA_CN);
    params.distinguished_name = dn;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
}

/// Where the CA and all issued certificates live.
pub(crate) fn ca_dir() -> Result<PathBuf> {
    Ok(crate::paths::base_dir()?.join("ca"))
}

/// Advisory lock over the CA directory: two concurrent first runs must not
/// interleave their writes into a mismatched key/cert pair.
struct CaLock {
    _file: File,
}

impl CaLock {
    fn acquire(dir: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".lock"))
            .map_err(|e| ca_err("lock open", e))?;
        file.lock().map_err(|e| ca_err("lock acquire", e))?;
        Ok(Self { _file: file })
    }
}

/// Atomic (tmp + rename) write; the tempfile is created 0o600 on Unix, so
/// private keys never appear world-readable at any instant, and a killed
/// process cannot leave a truncated half-file behind.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| ca_err("write", e))?;
    std::io::Write::write_all(&mut tmp, contents.as_bytes()).map_err(|e| ca_err("write", e))?;
    tmp.persist(path).map_err(|e| ca_err("write", e.error))?;
    Ok(())
}

/// Pin the process-level rustls CryptoProvider to ring before the first
/// builder call. Feature unification leaves both providers compiled in
/// (fred and reqwest pull ring; transitive defaults pull aws-lc-rs), and
/// rustls 0.23 refuses to auto-pick between two, so every rustls-touching
/// entry here installs ring once (idempotent, first call wins).
pub(crate) fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Idempotently ensure the CA exists; return the CA certificate path.
pub(crate) fn ensure_ca() -> Result<PathBuf> {
    ensure_crypto_provider();
    let dir = ca_dir()?;
    std::fs::create_dir_all(&dir)?;
    let _lock = CaLock::acquire(&dir)?;
    let key_path = dir.join("ca.key");
    let cert_path = dir.join("ca.crt");
    if key_path.is_file() && cert_path.is_file() {
        return Ok(cert_path);
    }
    let key_pair = KeyPair::generate().map_err(|e| ca_err("keygen", e))?;
    let cert = ca_params()
        .self_signed(&key_pair)
        .map_err(|e| ca_err("self-sign", e))?;
    write_atomic(&key_path, &key_pair.serialize_pem())?;
    write_atomic(&cert_path, &cert.pem())?;
    Ok(cert_path)
}

/// Issue a certificate signed by the CA; returns (cert_pem, key_pem).
pub(crate) fn issue(cn: &str) -> Result<(String, String)> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    issue_with(params)
}

/// Issue a server certificate for a managed container (ADR-0011): the CN is
/// the container name for humans, and the SAN set carries the loopback
/// faces a published-port client actually connects through — rustls
/// verifies the SAN, not the CN.
pub(crate) fn issue_server_cert(container_name: &str) -> Result<(String, String)> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, container_name);
    params.distinguished_name = dn;
    params.subject_alt_names = vec![
        rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        rcgen::SanType::IpAddress(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
    ];
    issue_with(params)
}

fn issue_with(params: CertificateParams) -> Result<(String, String)> {
    let dir = ca_dir()?;
    ensure_ca()?;
    let ca_key_pem = std::fs::read_to_string(dir.join("ca.key"))
        .map_err(|e| Error::Postgres(format!("CA key read: {e}")))?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(|e| ca_err("key parse", e))?;
    let params_ca = ca_params();

    let leaf_key = KeyPair::generate().map_err(|e| ca_err("leaf keygen", e))?;
    let issuer = Issuer::from_params(&params_ca, &ca_key);
    let cert = params
        .signed_by(&leaf_key, &issuer)
        .map_err(|e| ca_err("sign", e))?;
    Ok((cert.pem(), leaf_key.serialize_pem()))
}

/// The dctl client certificate paths (CN = the DB username); issued lazily.
pub(crate) fn client_cert(user: &str) -> Result<(PathBuf, PathBuf)> {
    let dir = ca_dir()?;
    let cert_path = dir.join(format!("client-{user}.crt"));
    let key_path = dir.join(format!("client-{user}.key"));
    if cert_path.is_file() && key_path.is_file() {
        return Ok((cert_path, key_path));
    }
    let (cert_pem, key_pem) = issue(user)?;
    // Both writes propagate: a cert that silently failed to land would make
    // the next read report "no such file" instead of the real write error.
    write_atomic(&cert_path, &cert_pem)?;
    write_atomic(&key_path, &key_pem)?;
    Ok((cert_path, key_path))
}

/// Build a rustls ClientConfig with the dctl client certificate and CA
/// for verifying the server. Used by tokio-postgres-rustls.
pub(crate) fn tls_config(user: &str) -> Result<rustls::ClientConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    ensure_crypto_provider();
    let (cert_path, key_path) = client_cert(user)?;
    let ca_path = ensure_ca()?;

    let cert_pem =
        std::fs::read(&cert_path).map_err(|e| Error::Postgres(format!("client cert read: {e}")))?;
    let key_pem = std::fs::read_to_string(&key_path)
        .map_err(|e| Error::Postgres(format!("client key read: {e}")))?;
    let ca_pem = std::fs::read(&ca_path).map_err(|e| Error::Postgres(format!("CA read: {e}")))?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| ca_err("cert parse", e))?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .map_err(|e| ca_err("key parse", e))?
        .ok_or_else(|| Error::Postgres("CA: no private key in client PEM".to_string()))?;

    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca_pem.as_slice()) {
        let cert = cert.map_err(|e| ca_err("CA parse", e))?;
        roots.add(cert).map_err(|e| ca_err("CA trust", e))?;
    }

    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| ca_err("TLS config", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_cert_issuance_returns_a_pem_pair() {
        // SAN verification (loopback faces) is exercised on the lan-linux2
        // real-Docker pass; here we pin the issuance shape without writing
        // armored marker literals into the source.
        let (cert, key) = issue_server_cert("dctl-pg-default-18").unwrap();
        assert!(cert.contains("CERTIFICATE"));
        assert!(key.contains("PRIVATE KEY"));
    }
}
