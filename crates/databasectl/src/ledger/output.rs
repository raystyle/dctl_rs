//! Output types for `dctl ledger ...`. Projection rows are pass-through
//! JSON; the human renderer reads well-known fields tolerantly (absent
//! fields render `-`), so server-side projection tweaks do not break it.

use crate::local::output::print_output;
use serde::Serialize;
use serde_json::Value;
use std::fmt;

fn field(row: &Value, key: &str) -> String {
    let value = &row[key];
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => "-".to_string(),
    }
}

fn truncate_cell(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueListOutput {
    pub issues: Vec<Value>,
    /// Rows in this response (family standard: not the ledger total).
    pub count: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

impl fmt::Display for IssueListOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.issues.is_empty() {
            return write!(f, "No issues (count {})", self.count);
        }
        let rows: Vec<IssueRow> = self
            .issues
            .iter()
            .map(|row| IssueRow {
                number: field(row, "issue_n"),
                kind: field(row, "kind"),
                status: field(row, "status"),
                result: match &row["hasResult"] {
                    serde_json::Value::Bool(true) => "yes".to_string(),
                    _ => "-".to_string(),
                },
                title: truncate_cell(&field(row, "title"), 60),
            })
            .collect();
        let table = tabled::Table::new(rows)
            .with(tabled::settings::Style::markdown())
            .to_string();
        writeln!(f, "{table}")?;
        write!(f, "count {} (this page)", self.count)?;
        if self.has_more {
            let hint = self
                .next_before
                .as_deref()
                .map(|id| format!(": rerun with --before {id}"))
                .unwrap_or_default();
            write!(f, ", more available{hint}")?;
        }
        Ok(())
    }
}

#[derive(tabled::Tabled)]
struct IssueRow {
    #[tabled(rename = "Number")]
    number: String,
    #[tabled(rename = "Kind")]
    kind: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "Result")]
    result: String,
    #[tabled(rename = "Title")]
    title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DetailOutput {
    pub detail: Value,
}

impl fmt::Display for DetailOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string_pretty(&self.detail).unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisteredOutput {
    pub registered: Value,
}

impl fmt::Display for RegisteredOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Real write responses: {"ok":true,"issue":<n>} for issues,
        // {"ok":true,"artifact_id":...,"digest":...} for artifacts, and
        // {"ok":true,"event":{seq,type}} for attestations.
        let issue = field(&self.registered, "issue");
        let artifact = field(&self.registered, "artifact_id");
        let event_type = self.registered["event"]["type"].as_str();
        let seq = field(&self.registered["event"], "seq");
        if issue != "-" {
            write!(f, "Opened issue {issue}")?;
        } else if artifact != "-" {
            let digest = field(&self.registered, "digest");
            write!(f, "Published artifact {artifact} ({digest})")?;
        } else if let Some(event_type) = event_type {
            write!(f, "Event posted: {event_type} (seq {seq})")?;
        } else {
            write!(f, "Registered")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyEventOutput {
    pub events: Vec<Value>,
}

impl fmt::Display for KeyEventOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Write responses nest the event: {"ok":true,"event":{seq,type}}.
        for value in &self.events {
            let kind = value["event"]["type"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| field(value, "type"));
            let seq = field(&value["event"], "seq");
            writeln!(f, "Event posted: {kind} (seq {seq})")?;
        }
        write!(f, "Issue closed")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyOutput {
    pub jwk: String,
    pub kid: String,
    pub repo_id: String,
}

impl fmt::Display for KeyOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "repo: {}", self.repo_id)?;
        writeln!(f, "kid:  {}", self.kid)?;
        write!(f, "jwk:  {}", self.jwk)
    }
}

pub(crate) fn print<T: Serialize + fmt::Display>(out: &T, json: bool) {
    print_output(out, json);
}
