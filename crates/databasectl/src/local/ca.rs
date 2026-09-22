//! Local dctl CA and per-instance certificate issuance (ADR-0011).
//!
//! A single global CA at `~/.dctl/ca/` issues per-instance server
//! certificates and the dctl client certificate. The CA key persists on
//! disk; the ephemeral `Certificate` object is reconstructed from the same
//! key + params each time (same subject DN, so the signing chain holds).
//! Private keys are 0600; nothing enters argv, logs, or the repository.

use crate::error::{Error, Result};
use rcgen::{CertificateParams, DistinguishedName, DnType, Issuer, KeyPair};
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

/// Idempotently ensure the CA exists; return the CA certificate path.
pub(crate) fn ensure_ca() -> Result<PathBuf> {
    let dir = ca_dir()?;
    std::fs::create_dir_all(&dir)?;
    let key_path = dir.join("ca.key");
    let cert_path = dir.join("ca.crt");
    if key_path.is_file() && cert_path.is_file() {
        return Ok(cert_path);
    }
    let key_pair = KeyPair::generate().map_err(|e| ca_err("keygen", e))?;
    let cert = ca_params()
        .self_signed(&key_pair)
        .map_err(|e| ca_err("self-sign", e))?;
    write_secret(&key_path, &key_pair.serialize_pem())?;
    std::fs::write(&cert_path, cert.pem())
        .map_err(|e| Error::Postgres(format!("CA write: {e}")))?;
    Ok(cert_path)
}

/// Issue a certificate signed by the CA; returns (cert_pem, key_pem).
pub(crate) fn issue(cn: &str) -> Result<(String, String)> {
    let dir = ca_dir()?;
    ensure_ca()?;
    let ca_key_pem = std::fs::read_to_string(dir.join("ca.key"))
        .map_err(|e| Error::Postgres(format!("CA key read: {e}")))?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).map_err(|e| ca_err("key parse", e))?;
    let params_ca = ca_params();

    let leaf_key = KeyPair::generate().map_err(|e| ca_err("leaf keygen", e))?;
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    let issuer = Issuer::from_params(&params_ca, &ca_key);
    let cert = params
        .signed_by(&leaf_key, &issuer)
        .map_err(|e| ca_err("sign", e))?;
    Ok((cert.pem(), leaf_key.serialize_pem()))
}

fn write_secret(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)
        .map_err(|e| Error::Postgres(format!("CA write {}: {e}", path.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::Postgres(format!("CA chmod {}: {e}", path.display())))?;
    }
    Ok(())
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
    let _ = std::fs::write(&cert_path, &cert_pem);
    write_secret(&key_path, &key_pem)?;
    Ok((cert_path, key_path))
}

/// Build a rustls ClientConfig with the dctl client certificate and CA
/// for verifying the server. Used by tokio-postgres-rustls.
pub(crate) fn tls_config(user: &str) -> Result<rustls::ClientConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

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
