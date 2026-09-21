#!/usr/bin/env python3
"""Classify paths that can affect live local ClickHouse install checks."""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Keep this exact: local Docker/Postgres changes must not start the live install
# matrix. The static test inventories shared/local Rust sources and every
# subprocess test file, so a new or renamed candidate fails closed until it is
# classified here.
INSTALL_EXACT_PATHS = frozenset(
    {
        ".github/workflows/test-cli.yml",
        ".github/workflows/test-install.yml",
        "Cargo.lock",
        "Cargo.toml",
        "crates/databasectl/Cargo.toml",
        "crates/databasectl/src/cli.rs",
        "crates/databasectl/src/error.rs",
        "crates/databasectl/src/http.rs",
        "crates/databasectl/src/init.rs",
        "crates/databasectl/src/local/cli.rs",
        "crates/databasectl/src/local/clickhouse.rs",
        "crates/databasectl/src/local/mod.rs",
        "crates/databasectl/src/local/output.rs",
        "crates/databasectl/src/local/server.rs",
        "crates/databasectl/src/main.rs",
        "crates/databasectl/src/paths.rs",
        "crates/databasectl/src/user_agent.rs",
        "crates/databasectl/tests/local_clickhouse_docker_test.rs",
        "scripts/classify-install-integration.py",
        "scripts/tests/test_classify_install_integration.py",
    }
)

INSTALL_PREFIXES = ()

# Explicit non-install mappings make the scope reviewable while allowing the
# inventory test to reject an unclassified new shared/local source or test.
NON_INSTALL_EXACT_PATHS = frozenset(
    {
        "crates/databasectl/src/local/config.rs",
        "crates/databasectl/src/local/docker.rs",
        "crates/databasectl/src/local/falkordb.rs",
        "crates/databasectl/src/local/postgres.rs",
        "crates/databasectl/src/ledger/cli.rs",
        "crates/databasectl/src/ledger/keys.rs",
        "crates/databasectl/src/ledger/mod.rs",
        "crates/databasectl/src/ledger/output.rs",
        "crates/databasectl/src/skills.rs",
        "crates/databasectl/src/update.rs",
        "crates/databasectl/tests/local_docker_diagnostics_test.rs",
        "crates/databasectl/tests/local_docker_status_test.rs",
        "crates/databasectl/tests/local_docker_pull_progress_test.rs",
        "crates/databasectl/tests/local_init_json_test.rs",
        "crates/databasectl/tests/local_postgres_client_input_test.rs",
        "crates/databasectl/tests/local_postgres_readiness_test.rs",
        "crates/databasectl/tests/local_postgres_start_validation_test.rs",
        "crates/databasectl/tests/ledger_request_test.rs",
        "crates/databasectl/tests/local_clickhouse_client_test.rs",
        "crates/databasectl/tests/local_clickhouse_docker_test.rs",
        "crates/databasectl/tests/local_falkor_readiness_test.rs",
        "crates/databasectl/tests/skills_usage_test.rs",
    }
)

CANDIDATE_SOURCE_PREFIX = "crates/databasectl/src/"
CANDIDATE_TEST_PREFIX = "crates/databasectl/tests/"
CLOUD_SOURCE_PREFIX = "crates/databasectl/src/cloud/"


def workflow_path_patterns() -> frozenset[str]:
    """Return the exact pull-request path filter represented by this mapping."""
    return INSTALL_EXACT_PATHS | frozenset(f"{prefix}**" for prefix in INSTALL_PREFIXES)


def classify_path(path: str) -> bool | None:
    """Return run/skip, or None for an unclassified installer candidate."""
    if path in INSTALL_EXACT_PATHS or path.startswith(INSTALL_PREFIXES):
        return True
    if path in NON_INSTALL_EXACT_PATHS or path.startswith(CLOUD_SOURCE_PREFIX):
        return False
    if (
        path.endswith(".rs") and path.startswith(CANDIDATE_SOURCE_PREFIX)
    ) or path.startswith(CANDIDATE_TEST_PREFIX):
        return None
    return False
