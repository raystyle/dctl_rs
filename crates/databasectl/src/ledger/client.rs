//! HTTP client for ledger.ohmygh.com: unsigned GET projections, signed
//! POST writes, family-standard pagination.

use crate::error::{Error, Result};
use crate::ledger::keys;
use crate::ledger::sign;

pub(crate) const DEFAULT_BASE_URL: &str = "https://ledger.ohmygh.com";
const URL_ENV: &str = "DCTL_LEDGER_URL";

pub(crate) fn base_url() -> String {
    // A trailing slash would double up the path (`//repos/...`) and the
    // server does not normalize it — trim operator typos away.
    std::env::var(URL_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn repo_path(suffix: &str) -> String {
    format!("/repos/{}/{}", keys::REPO_ID, suffix)
}

/// A GET projection row / write response, kept as pass-through JSON.
pub(crate) type Json = serde_json::Value;

pub(crate) struct Page {
    pub rows: Vec<Json>,
    pub has_more: bool,
    /// count per the family standard: how many rows this response returned,
    /// not the total in the ledger.
    pub count: usize,
}

pub(crate) async fn get(path: &str, query: &[(&str, String)]) -> Result<Json> {
    let client = crate::http::client_builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let url = format!("{}{}", base_url(), path);
    let response = client
        .get(&url)
        .query(query)
        .send()
        .await
        .map_err(|e| Error::Ledger(format!("ledger request failed: {e}")))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status.as_u16(), &body));
    }
    serde_json::from_str(&body)
        .map_err(|e| Error::Ledger(format!("ledger response was not JSON: {e}")))
}

pub(crate) async fn get_page(path: &str, limit: u32, before: Option<&str>) -> Result<Page> {
    // The server only computes has_more when `before` is present or more=1;
    // always ask so the first page reports saturation too.
    let mut query = vec![("limit", limit.to_string()), ("more", "1".to_string())];
    if let Some(before) = before {
        query.push(("before", before.to_string()));
    }
    let value = get(path, &query).await?;
    let rows = value
        .get("issues")
        .or_else(|| value.get("artifacts"))
        .or_else(|| value.get("items"))
        .and_then(|rows| rows.as_array())
        .cloned()
        .unwrap_or_default();
    let count = rows.len();
    let has_more = value
        .get("has_more")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false);
    Ok(Page {
        rows,
        has_more,
        count,
    })
}

/// Issue list projection.
pub(crate) async fn list_issues(limit: u32, before: Option<&str>) -> Result<Page> {
    get_page(&repo_path("issues"), limit, before).await
}

/// Issue detail projection.
pub(crate) async fn show_issue(n: &str) -> Result<Json> {
    get(&repo_path(&format!("issues/{n}")), &[]).await
}

/// Artifact list projection; `current=1` filters to each name's current
/// entry, `env` selects dev/prod attestation state.
pub(crate) async fn list_artifacts(
    limit: u32,
    before: Option<&str>,
    current: bool,
    env: Option<&str>,
) -> Result<Page> {
    let path = repo_path("artifacts");
    let mut query = vec![("limit", limit.to_string()), ("more", "1".to_string())];
    if let Some(before) = before {
        query.push(("before", before.to_string()));
    }
    if current {
        query.push(("current", "1".to_string()));
    }
    if let Some(env) = env {
        query.push(("env", env.to_string()));
    }
    let value = get(&path, &query).await?;
    let rows = value
        .get("artifacts")
        .or_else(|| value.get("items"))
        .and_then(|rows| rows.as_array())
        .cloned()
        .unwrap_or_default();
    let count = rows.len();
    let has_more = value
        .get("has_more")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false);
    Ok(Page {
        rows,
        has_more,
        count,
    })
}

/// A signed POST write. Idempotency key and nonce are minted per call;
/// the same key with different content would be a 409 (server-side rule —
/// the CLI never reuses a key).
pub(crate) async fn signed_post(path: &str, body: serde_json::Value) -> Result<Json> {
    let key = keys::load_signing_key()?;
    let kid = keys::key_id();
    let body_bytes = serde_json::to_vec(&body)?;
    let signed = sign::sign_post(
        &key,
        &kid,
        "POST",
        path,
        body_bytes,
        sign::unix_timestamp(),
        sign::new_nonce(),
        sign::new_idempotency_key(),
    )?;

    let client = crate::http::client_builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let url = format!("{}{}", base_url(), signed.path);
    let mut request = client.post(&url).body(signed.body.clone());
    for (name, value) in &signed.headers {
        request = request.header(*name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Ledger(format!("ledger request failed: {e}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(status.as_u16(), &text));
    }
    serde_json::from_str(&text)
        .map_err(|e| Error::Ledger(format!("ledger response was not JSON: {e}")))
}

fn api_error(status: u16, body: &str) -> Error {
    let hint = match status {
        401 => {
            " (X-Timestamp drift, or the private key is not the pair of the built-in kid — the server verifies X-Key-Id against the registered public key; see `dctl ledger key`)"
        }
        409 => " (idempotency key collision with different content; rerun to mint a fresh key)",
        429 => " (per-key daily quota reached; writes resume next UTC day)",
        _ => "",
    };
    Error::Ledger(format!(
        "ledger returned HTTP {status}{hint}: {}",
        truncate(body, 400)
    ))
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}…", &text[..max])
    }
}
