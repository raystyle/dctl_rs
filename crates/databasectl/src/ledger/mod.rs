//! Handlers for `dctl ledger ...`: this repo's issue and artifact flows on
//! ledger.ohmygh.com, on the shared `ledger-client` crate (the fleet's
//! single signing/HTTP implementation, REQ-063 / REQ-006).
//!
//! Boundary (user ruling 2026-09-20): this CLI only ADDS issues and
//! artifacts plus verification attestations; closing (status moves) and
//! deletion belong to the omc workbench over herdr delegation.
//!
//! The shared client is blocking HTTP; `run` is accordingly synchronous.

pub(crate) mod cli;
mod keys;
mod output;

use crate::error::{Error, Result};
use cli::{ArtifactCommands, IssueCommands, LedgerCommands};
use ledger_client::Ledger;
use serde_json::{Value, json};

pub(crate) fn run(cmd: LedgerCommands, json: bool) -> Result<()> {
    match cmd {
        LedgerCommands::Issue { command } => run_issue(command, json),
        LedgerCommands::Artifact { command } => run_artifact(command, json),
        LedgerCommands::Key => {
            let out = output::KeyOutput {
                jwk: keys::PUBLIC_JWK.to_string(),
                kid: keys::key_id(),
                repo_id: keys::REPO_ID.to_string(),
            };
            output::print(&out, json);
            Ok(())
        }
    }
}

/// Writes: the real signing identity from env/archive.
fn client() -> Result<Ledger> {
    Ok(Ledger::new(keys::REPO_ID, keys::load_key_pair()?))
}

/// Reads need no credentials; the client type still wants a key, so reads
/// carry a throwaway one that is never used (GETs are unsigned).
fn read_client() -> Ledger {
    Ledger::new(keys::REPO_ID, ledger_client::KeyPair::generate())
}

fn map_ledger_error(error: ledger_client::LedgerError) -> Error {
    match error {
        ledger_client::LedgerError::Http(source) => {
            Error::Ledger(format!("ledger request failed: {source}"))
        }
        ledger_client::LedgerError::Api { status, message } => {
            Error::Ledger(format!("ledger {status}: {message}"))
        }
        ledger_client::LedgerError::Key(message) => Error::Ledger(message),
    }
}

fn run_issue(cmd: IssueCommands, json: bool) -> Result<()> {
    match cmd {
        IssueCommands::New {
            title,
            kind,
            acceptance,
        } => {
            let ledger = client()?;
            let number = ledger
                .issue_new(&title, kind.as_str(), &acceptance, None)
                .map_err(map_ledger_error)?;
            let registered = json!({ "ok": true, "issue": number });
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        IssueCommands::List { limit, before } => {
            let ledger = read_client();
            let page = ledger
                .issue_list(limit, before.and_then(|before| before.parse().ok()))
                .map_err(map_ledger_error)?;
            let rows = page["rows"].as_array().cloned().unwrap_or_default();
            let count = rows.len();
            let has_more = page["has_more"].as_bool().unwrap_or(false);
            let next_before = rows.last().and_then(|row| match &row["issue_n"] {
                Value::String(text) => Some(text.clone()),
                other if !other.is_null() => Some(other.to_string()),
                _ => None,
            });
            output::print(
                &output::IssueListOutput {
                    issues: rows,
                    count,
                    has_more,
                    next_before,
                },
                json,
            );
            Ok(())
        }
        IssueCommands::Show { number } => {
            let ledger = read_client();
            let n: u64 = number.parse().map_err(|_| {
                Error::Ledger(format!("issue number must be numeric, got '{number}'"))
            })?;
            let detail = ledger.issue_show(n).map_err(map_ledger_error)?;
            output::print(&output::DetailOutput { detail }, json);
            Ok(())
        }
    }
}

fn run_artifact(cmd: ArtifactCommands, json: bool) -> Result<()> {
    match cmd {
        ArtifactCommands::Publish {
            name,
            kind,
            digest,
            version,
            git_range,
            deps,
        } => {
            validate_digest(&digest)?;
            for dep in &deps {
                validate_digest(dep)?;
            }
            let ledger = client()?;
            let artifact_id = ledger
                .artifact_publish(
                    &name,
                    kind.as_str(),
                    &digest,
                    version.as_deref(),
                    git_range.as_deref(),
                    &deps,
                    None,
                )
                .map_err(map_ledger_error)?;
            let registered = json!({ "ok": true, "artifact_id": artifact_id, "digest": digest });
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        ArtifactCommands::Attest { id, kind } => {
            let ledger = client()?;
            let registered = ledger
                .artifact_attest(&id, kind.as_str(), json!({}), None)
                .map_err(map_ledger_error)?;
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        ArtifactCommands::List { current, env } => {
            let ledger = read_client();
            let page = ledger
                .artifact_list(current, env.map(|env| env.as_str()))
                .map_err(map_ledger_error)?;
            let rows = page["rows"].as_array().cloned().unwrap_or_default();
            let count = rows.len();
            let has_more = page["has_more"].as_bool().unwrap_or(false);
            let next_before = rows.last().and_then(|row| match &row["artifact_id"] {
                Value::String(text) => Some(text.clone()),
                _ => None,
            });
            let out = ArtifactListOutput {
                artifacts: rows,
                count,
                has_more,
                next_before,
            };
            output::print(&out, json);
            Ok(())
        }
    }
}

/// `sha256:` + 64 hex — the ledger's digest identity for both content and
/// dependencies. Checked client-side so a typo fails before the round trip.
fn validate_digest(digest: &str) -> Result<()> {
    let valid = digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()));
    if valid {
        Ok(())
    } else {
        Err(Error::Ledger(format!(
            "digest must be sha256:<64 hex>, got '{digest}'"
        )))
    }
}

#[derive(serde::Serialize)]
struct ArtifactListOutput {
    artifacts: Vec<Value>,
    count: usize,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_before: Option<String>,
}

impl std::fmt::Display for ArtifactListOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.artifacts.is_empty() {
            return write!(f, "No artifacts (count {})", self.count);
        }
        #[derive(tabled::Tabled)]
        struct Row {
            #[tabled(rename = "Id")]
            id: String,
            #[tabled(rename = "Name")]
            name: String,
            #[tabled(rename = "Kind")]
            kind: String,
            #[tabled(rename = "Dev")]
            dev: String,
            #[tabled(rename = "Prod")]
            prod: String,
            #[tabled(rename = "Cur")]
            current: String,
            #[tabled(rename = "Digest")]
            digest: String,
        }
        fn flag(row: &Value, key: &str) -> String {
            match &row[key] {
                Value::Bool(true) => "yes".to_string(),
                _ => "-".to_string(),
            }
        }
        fn field(row: &Value, key: &str) -> String {
            match &row[key] {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            }
        }
        let rows: Vec<Row> = self
            .artifacts
            .iter()
            .map(|row| Row {
                id: field(row, "artifact_id"),
                name: field(row, "name"),
                kind: field(row, "kind"),
                dev: flag(row, "dev_verified"),
                prod: flag(row, "prod_verified"),
                current: flag(row, "current"),
                digest: field(row, "digest"),
            })
            .collect();
        let table = tabled::Table::new(rows)
            .with(tabled::settings::Style::markdown())
            .to_string();
        writeln!(f, "{table}")?;
        write!(f, "count {} (this page)", self.count)?;
        if self.has_more
            && let Some(id) = &self.next_before
        {
            write!(f, ", more available: rerun with --before {id}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_shape_is_enforced_before_the_round_trip() {
        assert!(
            validate_digest(
                "sha256:a3f5b8e4d2c90f17e8a6b5d4c3b2a1908f7e6d5c4b3a2918f7e6d5c4b3a29180"
            )
            .is_ok()
        );
        for bad in [
            "sha256:short",
            "sha256:zzzz",
            "md5:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:",
        ] {
            assert!(validate_digest(bad).is_err(), "{bad}");
        }
    }
}
