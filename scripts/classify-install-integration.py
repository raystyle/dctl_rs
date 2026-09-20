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
        "crates/databasectl/src/local/discovery.rs",
        "crates/databasectl/src/local/mod.rs",
        "crates/databasectl/src/local/output.rs",
        "crates/databasectl/src/local/server.rs",
        "crates/databasectl/src/local/symlink.rs",
        "crates/databasectl/src/main.rs",
        "crates/databasectl/src/paths.rs",
        "crates/databasectl/src/user_agent.rs",
        "crates/databasectl/tests/local_install_local_first_test.rs",
        "crates/databasectl/tests/local_version_error_test.rs",
        "scripts/classify-install-integration.py",
        "scripts/tests/test_classify_install_integration.py",
    }
)

INSTALL_PREFIXES = ("crates/databasectl/src/version_manager/",)

# Explicit non-install mappings make the scope reviewable while allowing the
# inventory test to reject an unclassified new shared/local source or test.
NON_INSTALL_EXACT_PATHS = frozenset(
    {
        "crates/clickhouse-cloud-api/src/client/query_api_endpoints.rs",
        "crates/clickhouse-cloud-api/src/models/query_api_endpoints.rs",
        "crates/databasectl/src/cloud/clickstack.rs",
        "crates/databasectl/src/cloud/config.rs",
        "crates/databasectl/src/cloud/query_api_endpoints.rs",
        "crates/databasectl/src/cloud/udfs.rs",
        "crates/databasectl/src/dotenv.rs",
        "crates/databasectl/src/failure.rs",
        "crates/databasectl/src/local/config.rs",
        "crates/databasectl/src/local/docker.rs",
        "crates/databasectl/src/local/postgres.rs",
        "crates/databasectl/src/skills.rs",
        "crates/databasectl/src/telemetry.rs",
        "crates/databasectl/src/update.rs",
        "crates/databasectl/tests/cli_request_shape_test.rs",
        "crates/databasectl/tests/local_client_project_scope_errors_test.rs",
        "crates/databasectl/tests/local_client_selectors_test.rs",
        "crates/databasectl/tests/local_client_output_contract_test.rs",
        "crates/databasectl/tests/local_docker_diagnostics_test.rs",
        "crates/databasectl/tests/local_docker_status_test.rs",
        "crates/databasectl/tests/local_docker_pull_progress_test.rs",
        "crates/databasectl/tests/local_init_json_test.rs",
        "crates/databasectl/tests/local_postgres_client_input_test.rs",
        "crates/databasectl/tests/local_postgres_readiness_test.rs",
        "crates/databasectl/tests/local_postgres_start_validation_test.rs",
        "crates/databasectl/tests/local_remove_default_test.rs",
        "crates/databasectl/tests/local_remove_global_guard_test.rs",
        "crates/databasectl/tests/local_server_metadata_test.rs",
        "crates/databasectl/tests/local_server_name_compatibility_test.rs",
        "crates/databasectl/tests/local_server_project_scope_errors_test.rs",
        "crates/databasectl/tests/local_server_readiness_test.rs",
        "crates/databasectl/tests/local_server_selection_test.rs",
        "crates/databasectl/tests/local_server_start_args_test.rs",
        "crates/databasectl/tests/local_server_state_machine_test.rs",
        "crates/databasectl/tests/local_server_stopped_test.rs",
        "crates/databasectl/tests/local_server_watchdog_pid_test.rs",
        "crates/databasectl/tests/local_structured_errors_test.rs",
        "crates/databasectl/tests/telemetry_test.rs",
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
