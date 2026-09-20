//! Clap definitions for `dctl ledger ...`.

use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub struct LedgerArgs {
    /// Output as JSON
    #[arg(long, global = true, display_order = crate::cli::help_order::JSON)]
    pub json: bool,

    #[command(subcommand)]
    pub command: LedgerCommands,
}

#[derive(Subcommand, Debug)]
pub enum LedgerCommands {
    /// Work with ledger issues for this repo
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Truth source: https://ledger.ohmygh.com (repo github.com/raystyle/dctl_rs).
  Reads need no key; writes sign with the local Ed25519 key (DCTL_LEDGER_KEY is
  only a carrier for the SAME keypair as the built-in kid — swapping in a
  different key fails verification until the new public JWK ships in the CLI).
  `issue close` posts result (digest reference) then status=done; the done event
  is what closes. Deterministic idempotency keys make reruns replay, not
  duplicate — the issue stays open until done lands.
  Typical flow: `ledger issue new` -> work -> `ledger artifact publish` -> `ledger issue close --digest <digest>`.")]
    Issue {
        #[command(subcommand)]
        command: IssueCommands,
    },

    /// Work with the shared artifact library for this repo
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Artifacts are information bodies (experience, lessons, research, records);
  digests are sha256 of the content, binaries are never uploaded.
  attest_dev and attest_prod are separate; promote requires prior attestation.
  Truth source: https://ledger.ohmygh.com.")]
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommands,
    },

    /// Show the embedded ledger public key JWK and its key id
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Prints the JWK constant built into this CLI and its kid (sha256 of the
  canonical JWK). The registry operator registers this kid once; writes then
  verify against it. The private key never appears here.")]
    Key,
}

#[derive(Subcommand, Debug)]
pub enum IssueCommands {
    /// Open a new issue (bug or improvement task)
    New {
        /// One-line title of the task
        #[arg(long)]
        title: String,

        /// Bug fix task or improvement task
        #[arg(long, default_value = "bug")]
        kind: IssueKindArg,

        /// Acceptance criteria: how done is judged
        #[arg(long)]
        acceptance: String,
    },

    /// List issues (family pagination: limit 100 + before cursor)
    List {
        /// Page size (the service caps at 100)
        #[arg(long, default_value_t = 100)]
        limit: u32,

        /// Cursor: list issues before this id (from the previous page's last row)
        #[arg(long)]
        before: Option<String>,
    },

    /// Show one issue with its event history
    Show {
        /// Issue number
        number: String,
    },

    /// Close an issue as done, referencing a registered digest
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  Posts a result event referencing the digest, then a status=done event — done
  is what closes the issue. Both events carry deterministic idempotency keys:
  rerunning after a partial failure replays both events instead of appending
  duplicates. Retrying with the same digest must reuse the same --note (or
  none) — a different note is different content under the same key (409).
  Publish the artifact first (`ledger artifact publish`).")]
    Close {
        /// Issue number
        number: String,

        /// Registered artifact/content digest (sha256:<64 hex>)
        #[arg(long)]
        digest: String,

        /// Optional human note carried by the result event
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum IssueKindArg {
    Bug,
    Improvement,
}

impl IssueKindArg {
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueKindArg::Bug => "bug",
            IssueKindArg::Improvement => "improvement",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum ArtifactCommands {
    /// Publish an artifact to the shared library
    #[command(after_help = "\
CONTEXT FOR AGENTS:
  digest = sha256 of the artifact's content (the ledger stores hashes and
  metadata only). Use kind=experience for distilled successes, lesson for
  distilled failures, research for study outcomes; prototype records carry
  git_range and version.")]
    Publish {
        /// Artifact name (stable identifier for current(name) projections)
        #[arg(long)]
        name: String,

        /// Artifact category
        #[arg(long)]
        kind: ArtifactKindArg,

        /// Content digest, sha256 followed by a colon and 64 hex characters
        #[arg(long)]
        digest: String,

        /// Version of the recorded artifact (for records and prototypes)
        #[arg(long)]
        version: Option<String>,

        /// Git commit range the record covers (for example 4c40ed6..96ece81)
        #[arg(long)]
        git_range: Option<String>,

        /// Dependency digests this artifact builds on (comma-separated)
        #[arg(long, value_delimiter = ',')]
        deps: Vec<String>,
    },

    /// Attach an attestation or lifecycle event to an artifact
    Attest {
        /// Artifact id
        id: String,

        /// Attestation type
        #[arg(long)]
        kind: AttestKindArg,
    },

    /// Promote an artifact to production (sugar for attest --kind promote)
    Promote {
        /// Artifact id
        id: String,
    },

    /// List artifacts (family pagination: limit 100 + before cursor)
    List {
        /// Page size (the service caps at 100)
        #[arg(long, default_value_t = 100)]
        limit: u32,

        /// Cursor: list artifacts before this id
        #[arg(long)]
        before: Option<String>,

        /// Only each name's current entry
        #[arg(long)]
        current: bool,

        /// Filter by attestation environment
        #[arg(long)]
        env: Option<AttestEnvArg>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ArtifactKindArg {
    Experience,
    Lesson,
    Research,
    Prototype,
    Binary,
    Image,
    Wasm,
    Sbom,
    Schema,
    Openapi,
    EvalSet,
    Benchmark,
    Runbook,
    Decision,
    AttestedReport,
}

impl ArtifactKindArg {
    pub fn as_str(&self) -> &'static str {
        use ArtifactKindArg::*;
        match self {
            Experience => "experience",
            Lesson => "lesson",
            Research => "research",
            Prototype => "prototype",
            Binary => "binary",
            Image => "image",
            Wasm => "wasm",
            Sbom => "sbom",
            Schema => "schema",
            Openapi => "openapi",
            EvalSet => "eval-set",
            Benchmark => "benchmark",
            Runbook => "runbook",
            Decision => "decision",
            AttestedReport => "attested-report",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum AttestKindArg {
    AttestDev,
    AttestProd,
    VerificationFailed,
    Promote,
    Demote,
    Supersede,
}

impl AttestKindArg {
    pub fn as_str(&self) -> &'static str {
        use AttestKindArg::*;
        match self {
            AttestDev => "attest_dev",
            AttestProd => "attest_prod",
            VerificationFailed => "verification_failed",
            Promote => "promote",
            Demote => "demote",
            Supersede => "supersede",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum AttestEnvArg {
    Dev,
    Prod,
}

impl AttestEnvArg {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttestEnvArg::Dev => "dev",
            AttestEnvArg::Prod => "prod",
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn parses_issue_new_minimal_and_full() {
        let cli = Cli::try_parse_from([
            "dctl",
            "ledger",
            "issue",
            "new",
            "--title",
            "Fix graph query hang",
            "--acceptance",
            "Query returns within 1s",
        ])
        .unwrap();
        let Commands::Ledger(args) = cli.command else {
            panic!("ledger command");
        };
        let super::LedgerCommands::Issue {
            command:
                super::IssueCommands::New {
                    title,
                    kind,
                    acceptance,
                },
        } = args.command
        else {
            panic!("issue new");
        };
        assert_eq!(title, "Fix graph query hang");
        assert_eq!(kind, super::IssueKindArg::Bug);
        assert_eq!(acceptance, "Query returns within 1s");

        let cli = Cli::try_parse_from([
            "dctl",
            "ledger",
            "issue",
            "new",
            "--title",
            "t",
            "--kind",
            "improvement",
            "--acceptance",
            "a",
        ])
        .unwrap();
        let Commands::Ledger(args) = cli.command else {
            panic!("ledger command");
        };
        let super::LedgerCommands::Issue {
            command: super::IssueCommands::New { kind, .. },
        } = args.command
        else {
            panic!("issue new");
        };
        assert_eq!(kind, super::IssueKindArg::Improvement);
    }

    #[test]
    fn rejects_unknown_issue_and_artifact_kinds() {
        assert!(
            Cli::try_parse_from([
                "dctl",
                "ledger",
                "issue",
                "new",
                "--title",
                "t",
                "--kind",
                "feature",
                "--acceptance",
                "a",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "dctl", "ledger", "artifact", "publish", "--name", "n", "--kind", "poem",
                "--digest", "sha256:x",
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_artifact_publish_full() {
        let cli = Cli::try_parse_from([
            "dctl",
            "ledger",
            "artifact",
            "publish",
            "--name",
            "falkordb-smoke",
            "--kind",
            "experience",
            "--digest",
            "sha256:aa",
            "--version",
            "0.5.0",
            "--git-range",
            "6d9beeb..96ece81",
            "--deps",
            "sha256:bb,sha256:cc",
        ])
        .unwrap();
        let Commands::Ledger(args) = cli.command else {
            panic!("ledger command");
        };
        let super::LedgerCommands::Artifact {
            command:
                super::ArtifactCommands::Publish {
                    name,
                    kind,
                    digest,
                    version,
                    git_range,
                    deps,
                },
        } = args.command
        else {
            panic!("artifact publish");
        };
        assert_eq!(name, "falkordb-smoke");
        assert_eq!(kind.as_str(), "experience");
        assert_eq!(digest, "sha256:aa");
        assert_eq!(version.as_deref(), Some("0.5.0"));
        assert_eq!(git_range.as_deref(), Some("6d9beeb..96ece81"));
        assert_eq!(deps, vec!["sha256:bb".to_string(), "sha256:cc".to_string()]);
    }

    #[test]
    fn promote_parses_and_attest_kind_maps() {
        let cli = Cli::try_parse_from(["dctl", "ledger", "artifact", "promote", "art-7"]).unwrap();
        let Commands::Ledger(args) = cli.command else {
            panic!("ledger command");
        };
        let super::LedgerCommands::Artifact {
            command: super::ArtifactCommands::Promote { id },
        } = args.command
        else {
            panic!("promote");
        };
        assert_eq!(id, "art-7");
        assert_eq!(super::AttestKindArg::AttestDev.as_str(), "attest_dev");
        assert_eq!(
            super::AttestKindArg::VerificationFailed.as_str(),
            "verification_failed"
        );
        assert_eq!(super::ArtifactKindArg::EvalSet.as_str(), "eval-set");
        assert_eq!(
            super::ArtifactKindArg::AttestedReport.as_str(),
            "attested-report"
        );
    }

    #[test]
    fn list_parses_pagination_and_env_filters() {
        let cli = Cli::try_parse_from([
            "dctl", "ledger", "issue", "list", "--limit", "10", "--before", "42",
        ])
        .unwrap();
        let Commands::Ledger(args) = cli.command else {
            panic!("ledger command");
        };
        let super::LedgerCommands::Issue {
            command: super::IssueCommands::List { limit, before },
        } = args.command
        else {
            panic!("list");
        };
        assert_eq!(limit, 10);
        assert_eq!(before.as_deref(), Some("42"));

        let cli = Cli::try_parse_from([
            "dctl",
            "ledger",
            "artifact",
            "list",
            "--current",
            "--env",
            "prod",
        ])
        .unwrap();
        let Commands::Ledger(args) = cli.command else {
            panic!("ledger command");
        };
        let super::LedgerCommands::Artifact {
            command: super::ArtifactCommands::List { current, env, .. },
        } = args.command
        else {
            panic!("artifact list");
        };
        assert!(current);
        assert_eq!(env, Some(super::AttestEnvArg::Prod));
    }
}
