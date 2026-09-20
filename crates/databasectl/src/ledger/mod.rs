//! Handlers for `dctl ledger ...`: the repo's issue and artifact flows on
//! ledger.ohmygh.com. Reads are plain GETs; writes go through the Ed25519
//! five-header signing scheme in [`sign`].

pub(crate) mod cli;
pub(crate) mod client;
pub(crate) mod keys;
mod output;
pub(crate) mod sign;

use crate::error::Result;
use cli::{ArtifactCommands, IssueCommands, LedgerCommands};
use serde_json::json;

pub(crate) async fn run(cmd: LedgerCommands, json: bool) -> Result<()> {
    match cmd {
        LedgerCommands::Issue { command } => run_issue(command, json).await,
        LedgerCommands::Artifact { command } => run_artifact(command, json).await,
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

async fn run_issue(cmd: IssueCommands, json: bool) -> Result<()> {
    match cmd {
        IssueCommands::New {
            title,
            kind,
            acceptance,
        } => {
            let body = json!({
                "title": title,
                "kind": kind.as_str(),
                "acceptance": acceptance,
            });
            let registered = client::signed_post(&issues_path(), body).await?;
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        IssueCommands::List { limit, before } => {
            let page = client::list_issues(limit, before.as_deref()).await?;
            let next_before = page
                .rows
                .last()
                .and_then(|row| row.get("issue_n"))
                .map(|value| match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                });
            output::print(
                &output::IssueListOutput {
                    issues: page.rows,
                    count: page.count,
                    has_more: page.has_more,
                    next_before,
                },
                json,
            );
            Ok(())
        }
        IssueCommands::Show { number } => {
            let detail = client::show_issue(&number).await?;
            output::print(&output::DetailOutput { detail }, json);
            Ok(())
        }
        IssueCommands::Close {
            number,
            digest,
            note,
        } => {
            sign::validate_digest(&digest)?;
            // Server shape (workers/ledger/src/index.ts:486-504): events
            // carry a nested payload; free text belongs in the top-level
            // body, structured fields in payload.
            let mut result = json!({
                "type": "result",
                "payload": {"digest": digest},
            });
            if let Some(note) = note {
                result["body"] = json!(note);
            }
            let done = json!({"type": "status", "payload": {"to": "done"}});
            // Ordered chain per the contract: done is only accepted on top of
            // a result event referencing a registered digest.
            let first = client::signed_post(&events_path(&number), result).await?;
            let second = client::signed_post(&events_path(&number), done).await?;
            output::print(
                &output::KeyEventOutput {
                    events: vec![first, second],
                },
                json,
            );
            Ok(())
        }
    }
}

async fn run_artifact(cmd: ArtifactCommands, json: bool) -> Result<()> {
    match cmd {
        ArtifactCommands::Publish {
            name,
            kind,
            digest,
            version,
            git_range,
            deps,
        } => {
            sign::validate_digest(&digest)?;
            for dep in &deps {
                sign::validate_digest(dep)?;
            }
            let mut body = json!({
                "name": name,
                "kind": kind.as_str(),
                "digest": digest,
            });
            if let Some(version) = version {
                body["version"] = json!(version);
            }
            if let Some(git_range) = git_range {
                body["git_range"] = json!(git_range);
            }
            if !deps.is_empty() {
                body["deps"] = json!(deps);
            }
            let registered = client::signed_post(&artifacts_path(), body).await?;
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        ArtifactCommands::Attest { id, kind } => {
            let body = json!({"type": kind.as_str(), "payload": {}});
            let registered = client::signed_post(&attestations_path(&id), body).await?;
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        ArtifactCommands::Promote { id } => {
            let body = json!({"type": "promote", "payload": {}});
            let registered = client::signed_post(&attestations_path(&id), body).await?;
            output::print(&output::RegisteredOutput { registered }, json);
            Ok(())
        }
        ArtifactCommands::List {
            limit,
            before,
            current,
            env,
        } => {
            let page = client::list_artifacts(
                limit,
                before.as_deref(),
                current,
                env.map(|env| env.as_str()),
            )
            .await?;
            let rows: Vec<serde_json::Value> = page.rows;
            let count = rows.len();
            let has_more = page.has_more;
            let next_before = rows.last().and_then(|row| {
                row.get("artifact_id").and_then(|value| match value {
                    serde_json::Value::String(text) => Some(text.clone()),
                    _ => None,
                })
            });
            // The artifact list reuses the issue list renderer shape: rows
            // pass through as JSON; the human view is a compact digest table.
            let out = ArtifactListOutputShim {
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

#[derive(serde::Serialize)]
struct ArtifactListOutputShim {
    artifacts: Vec<serde_json::Value>,
    count: usize,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_before: Option<String>,
}

impl std::fmt::Display for ArtifactListOutputShim {
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
        fn flag(row: &serde_json::Value, key: &str) -> String {
            match &row[key] {
                serde_json::Value::Bool(true) => "yes".to_string(),
                serde_json::Value::Bool(false) => "-".to_string(),
                _ => "-".to_string(),
            }
        }
        fn field(row: &serde_json::Value, key: &str) -> String {
            match &row[key] {
                serde_json::Value::String(text) => text.clone(),
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

fn issues_path() -> String {
    format!("/repos/{}/issues", keys::REPO_ID)
}

fn events_path(number: &str) -> String {
    format!("/repos/{}/issues/{}/events", keys::REPO_ID, number)
}

fn artifacts_path() -> String {
    format!("/repos/{}/artifacts", keys::REPO_ID)
}

fn attestations_path(id: &str) -> String {
    format!("/repos/{}/artifacts/{}/attestations", keys::REPO_ID, id)
}
