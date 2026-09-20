# AGENTS.md

`CLAUDE.md` is a symlink to this file. Edit `AGENTS.md`; never replace the symlink.

dctl (`databasectl` crate, binary `dctl`) is the DataBase Control CLI: local ClickHouse
(official binaries) and Postgres (Docker) lifecycle management. It is a fork of
ClickHouse/clickhousectl with the Cloud surface removed. Use `--help` to learn the
current command surface; `README.md` documents the CLI. Do not duplicate user-facing
documentation here.

## Collaboration model (who does what)

Three agents work this repo through the herdr review gate:

- **Claude authors PRs.** Branch per feature/issue, follow the invariants below,
  keep every commit independently green (fmt, both clippy configs, tests).
- **Codex reviews** in the herdr pane before anything reaches `main`. Receipts are
  three-valued: `F` must-fix, `G` suggestion, `CONFIRM` final approval. Rounds
  iterate (fix, re-review, quick second pass) until `CONFIRM`; only then push.
  Verify every finding before fixing it: a high-severity claim may be partly wrong,
  and every fix itself gets re-reviewed.
- **Kimi owns tests.** Follow the test taxonomy in Tests below; never write
  wording-pin tests (`help.contains("some sentence")`, `include_str!` on README,
  whole-screen equality). Test structure: parse outcomes, `ErrorKind`, defaults,
  value names, hidden flags staying hidden.

CI is the fourth, sleepless reviewer: clippy with `-D warnings` in both feature
configurations, fmt, the fail-closed install classifier, and the docker-backed
suites. Human/agent review focuses on what machines cannot judge (design trade-offs,
invariant bypasses).

## Commands

- `cargo fmt --all` before every commit (`fmt.yml` runs `cargo fmt --all --check`).
- `cargo clippy -p databasectl --all-targets --features telemetry -- -D warnings`
  and `cargo clippy -p databasectl --all-targets --no-default-features -- -D warnings`.
- `cargo test -p databasectl` (default features; telemetry tests are gated behind
  the feature) and `cargo test -p databasectl --features telemetry` when touching
  `src/telemetry.rs` or `src/failure.rs`.
- `python3 scripts/tests/test_classify_install_integration.py` when the install
  classifier or its path map changes.

**Done** means: `cargo fmt --all`; both clippy configurations clean; tests pass;
classifier mappings updated if a source or test file was added or renamed; README
updated for user-visible behaviour; work on a branch, with an associated issue and
a PR that passed the review gate.

## Workspace

- Single crate: `crates/databasectl/` (binary `dctl`). Everything is local engine
  logic; there is no cloud stack and no API library anymore.
- Project-local data lives in `.dctl/`; global state (versions, default marker,
  named configs) in `~/.dctl/`. `~/.local/bin/clickhouse` is the global product
  symlink and keeps its name.
- `src/telemetry.rs` and `src/failure.rs` are feature-gated (`telemetry`, off by
  default) and on the removal list; do not build new functionality on them.

## CLI invariants

- `main.rs` has a single exit path (the telemetry tail); do not add exit paths.
  Exit codes: `0` success, `1` error, `3` cancelled, clap `2` for usage errors;
  `ChildExit(code)` passes a spawned child's status through.
- Every successful output type implements both `Serialize` and `Display` and is
  printed through `local::output::print_output(&out, json)`. JSON mode is
  `flag || is_ai_agent::detect().is_some()` (`json_output()` in `main.rs`).
- Runtime failures render through the stable local error envelope
  (`local/output.rs`): closed `LocalErrorCode` vocabulary, `parity` (JSON message
  equals human text) or `redacted` (curated summary for foreign subprocess text).
- Cross-flag constraints clap cannot express go in `validate_post_parse`
  (`main.rs`), reported as the owning subcommand's usage error (exit 2).

## Adding a command

1. Add a variant to the relevant enum in `src/local/cli.rs` using clap derive macros.
2. Add the match arm in `run()` in `src/local/mod.rs`; `main.rs` delegates to that
   boundary. Implement the handler in a dedicated module under `src/local/` - never
   pile logic into `main.rs`.
3. Add `Cli::try_parse_from` coverage next to the command definition, asserting
   parsed values, defaults, and hidden flags staying hidden.

## Writing help text

- Help lives in `#[command(about/after_help)]` and arg doc comments in `src/cli.rs`
  and `src/local/cli.rs`. A help screen has only: one-line `about`, clap's
  `Usage:`/`Arguments:`/`Options:`/`Commands:`, and an optional trailing
  `CONTEXT FOR AGENTS:` block (hard cap 8 content lines, one fact per line).
- `about`: imperative verb phrase, no trailing period, keep siblings parallel.
  Flag help: one line, include units/format, never repeat clap's
  `[default: ...]` or `[possible values: ...]` in prose.
- Shared flags (`--json`) read identically everywhere; `help_order::JSON` keeps
  them in a final ordered block.
- Content users still need but help must not carry goes to `README.md` as a short
  example or a note of at most 3 lines.

## Tests

Test coverage is non-negotiable.

- **Clap parsing** - `Cli::try_parse_from` tests next to each command definition;
  assert flag names, types, defaults, repeatability.
- **Local subprocess** - one binary per concern under `crates/databasectl/tests/`
  (the `local_*` files): spawn the real binary against fake Docker sockets, fake
  `clickhouse`/`psql`/`pgrep`/`lsof` in an isolated `PATH`, `env_clear()` plus
  temp `HOME`. Add a new file for a new concern.
- **Pure logic** - inline `mod tests` blocks across `src/` for version resolution,
  output formatting, platform detection, module-local helpers.
- **Help and README text** - structural assertions only. No wording pins.
- **Docker-dependent** - `scripts/test-postgres-integration.sh` runs the real
  container boundary cases; run it where a Docker socket exists.

## CI gates

- Pin all GitHub Actions deps to SHA hashes, not tags.
- The install classifier (`scripts/classify-install-integration.py`) fails closed;
  it needs an entry when a source or test file is added or renamed, or CI breaks.
  `scripts/tests/test_classify_install_integration.py` keeps its path map honest.
- Workflows: `fmt.yml`, `test-cli.yml`, `test-postgres-integration.yml`,
  `test-install.yml`, `release.yml` (GitHub Releases only; no registry publishes).

## Dependencies

Use `cargo add` with the latest version and an explicit crate,
e.g. `cargo add -p databasectl url`.

## Releases

- Push a version tag (`git tag v0.2.3 && git push origin v0.2.3`) to run the
  release workflow: build matrix (musl static verified), 8-distro smoke test,
  GitHub Release. Distribution: install.sh + cargo-binstall + release assets.
- Bump `crates/databasectl/Cargo.toml` (`version`) only; there is no lockstep
  with other packages anymore.

## Upstream

`upstream` points at `ClickHouse/clickhousectl` (very active; most changes land
in the removed cloud stack). Selectively backport local-engine improvements:
`git fetch upstream`, cherry-pick or port the commit, adapt identity
(`clickhousectl`/`chctl` -> `dctl`, `.clickhouse` -> `.dctl`), keep the original
commit reference in the message, then run the full gate and review cycle.

## Git workflow and documentation

- Branch per feature/issue and use the PR workflow with the review gate above.
- Root `README.md` documents CLI capabilities and behaviour; update it only for
  functionality exposed through the CLI.
- Keep `AGENTS.md` up to date when development practice changes materially.
