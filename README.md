# dctl

**dctl** (DataBase Control) is a CLI for managing local database servers:
ClickHouse from official binaries, and Postgres in Docker containers. It is a
fork of [ClickHouse's clickhousectl](https://github.com/ClickHouse/clickhousectl)
(Apache-2.0) with the Cloud surface removed and the local and Docker engine
lifecycle kept as the core.

Run a database in your project directory with one command, no config files:

```console
$ dctl local server start          # installs `latest` on first run, spawns, waits for readiness
$ dctl local client -q 'SELECT 1'  # exec into the matching clickhouse-client
$ dctl local server stop
```

The same lifecycle works for Postgres, Docker-backed:

```console
$ dctl local postgres start        # pulls postgres:18 if needed, prints the generated password
$ dctl local postgres client -q 'SELECT version();'
$ dctl local postgres stop
```

dctl is agent-friendly by design: it detects coding agents and switches to JSON
output automatically, every command carries a `CONTEXT FOR AGENTS` block in
`--help`, and errors render as stable machine-readable envelopes.

## Deployment

Install the prebuilt binary (Linux musl static, macOS):

```console
$ curl -fsSL https://raw.githubusercontent.com/raystyle/dctl_rs/main/install.sh | sh
```

Or with cargo-binstall, which reads the release metadata:

```console
$ cargo binstall databasectl
```

Or build from source (Rust stable, edition 2024):

```console
$ git clone https://github.com/raystyle/dctl_rs
$ cargo build --release -p databasectl
```

Direct downloads live on the [GitHub Releases](https://github.com/raystyle/dctl_rs/releases)
page as `dctl-<target>-v<version>.tar.gz`.

`dctl update` self-updates from the latest GitHub release (`dctl update --check`
only checks). There is no crates.io / npm / PyPI channel; GitHub Releases is
the single distribution point.

Requirements: Linux or macOS; Docker only for the Postgres engine; network
access to builds.clickhouse.com / packages.clickhouse.com for ClickHouse
binary downloads (the product download servers, unchanged from upstream).

## Configuration

dctl keeps state in two places:

| Path | Scope | Contents |
| --- | --- | --- |
| `<project>/.dctl/` | per project | server metadata (`servers/*.json`), server data dirs; gitignored by `dctl local init` |
| `~/.dctl/` | global | installed versions (`versions/<v>/clickhouse`), the default version marker, named partial configs (`configs/`) |
| `~/.local/bin/clickhouse` | global | symlink to the default ClickHouse binary, kept by `dctl local use` |

Project-scoped commands use `.dctl/` under the exact current directory; parent
directories are not searched. Run them from the project root.

Environment variables:

- `DO_NOT_TRACK=1` fully silences telemetry, without touching config.
- `DCTL_TELEMETRY_URL` points the optional telemetry sender at a collector you
  own. Telemetry is compiled out by default (`telemetry` cargo feature); when
  the feature is enabled, nothing is sent unless this variable is set. This
  fork never reports to the upstream endpoint.

## Usage

Top-level surface: `dctl local`, `dctl skills`, `dctl update`. `--json` is
accepted everywhere; agents get it automatically. Exit codes: `0` success,
`1` error, `2` usage error (clap), `3` cancelled.

### ClickHouse versions

```console
$ dctl local install 25.12      # exact build, minor series, or latest/stable/lts
$ dctl local list               # installed exact versions
$ dctl local list --remote      # downloadable minor series
$ dctl local use 25.12          # set default + symlink ~/.local/bin/clickhouse
$ dctl local which              # show the default version
$ dctl local remove 25.12       # guarded: refuses versions in use or default
```

### ClickHouse servers

```console
$ dctl local init                       # scaffold .dctl/, clickhouse/, postgres/ dirs
$ dctl local server start               # default server, auto port pick if busy
$ dctl local server start dev --http-port 8333
$ dctl local server status              # also --global across projects
$ dctl local server stop [NAME]         # idempotent; stop-all for every scope
$ dctl local server remove NAME         # must be stopped first; deletes data
$ dctl local client [-q 'SELECT 1']     # exec the matching clickhouse-client
```

Start overlays `~/.dctl/configs/<name>` partial configs onto the managed
server config when passed `--config <name>`. Orphaned servers (started in a
project, then the metadata moved) are discovered by process cwd scanning.

### Postgres via Docker

```console
$ dctl local postgres start [NAME] [--user U --database D]
$ dctl local postgres client -q 'SELECT 1;'
$ dctl local postgres dotenv            # write .env connection variables
$ dctl local postgres stop [NAME]
```

Stopping keeps the container for resume; `remove` deletes it. The generated
password is printed once by start and re-readable via `dotenv`.

### Agent skills

```console
$ dctl skills --agent claude    # install ClickHouse agent skills into coding agents
```

### For contributors

Development discipline lives in [AGENTS.md](AGENTS.md) (commands, invariants,
test taxonomy, review gate). The fork tracks upstream
`ClickHouse/clickhousectl` through the `upstream` remote and selectively
backports local-engine improvements.

## License

Apache-2.0. dctl is derived from ClickHouse clickhousectl; see [LICENSE](LICENSE)
and the upstream repository for the original copyright notices.
