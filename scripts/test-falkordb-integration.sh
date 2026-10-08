#!/usr/bin/env bash
# Edge-case battery for `dctl local falkordb`. Runs each test in
# isolation in a temp directory, prints PASS/FAIL, and continues on
# failure so we get a full picture in one run.
#
# Requires:
#   * a working Docker daemon
#   * `jq` on PATH
#
# Usage:
#   scripts/test-falkordb-integration.sh [path/to/dctl]
#
# If no argument is given, falls back to $CLICKHOUSECTL or the debug build
# at target/debug/dctl relative to the repo root.
#
# On daemons whose published ports are unreachable over host loopback
# (no userland proxy, NAT not covering lo), every case verifies the server
# side through in-container redis-cli over docker exec; the host-side
# certificate client has its own ignored live test (see
# crates/databasectl/src/local/falkordb.rs, tls_cypher_live_round_trip).
set -u


CTL="${1:-${CLICKHOUSECTL:-}}"
if [[ -z "$CTL" ]]; then
    repo_root=$(cd "$(dirname "$0")/.." && pwd)
    CTL="$repo_root/target/debug/dctl"
fi
if [[ ! -x "$CTL" ]]; then
    echo "dctl binary not found at: $CTL" >&2
    echo "Run 'cargo build -p databasectl' first, or pass the path as argument." >&2
    exit 2
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required" >&2
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    echo "Docker daemon is not reachable" >&2
    exit 2
fi

FK_VERSION=4.20.6
FK_TLS_CLI="redis-cli --tls --cert /var/lib/falkordb/tls/client.crt --key /var/lib/falkordb/tls/client.key --cacert /var/lib/falkordb/tls/ca.crt --no-auth-warning"

PASS=0; FAIL=0
FAILED_TESTS=()

# Make a clean temp dir per test, run the body, then clean up containers
# the test created.
run_case() {
    local name=$1; shift
    local dir; dir=$(mktemp -d -t "fk-edge-$name.XXXX")
    # Isolated HOME so the per-project state bucket (ADR-0012) lands in a
    # scratch dir instead of the real user's ~/.dctl/projects/.
    local home; home=$(mktemp -d -t "fk-edge-home-$name.XXXX")
    # The CLI canonicalizes the project path before stamping it into
    # container labels, so we match against the realpath here.
    local real_dir; real_dir=$(cd "$dir" && pwd -P)
    cd "$dir" || { echo "[FAIL] $name: tempdir"; FAIL=$((FAIL+1)); return; }
    if HOME="$home" "$@"; then
        echo "[PASS] $name"
        PASS=$((PASS+1))
    else
        echo "[FAIL] $name"
        FAIL=$((FAIL+1))
        FAILED_TESTS+=("$name")
    fi
    # Best-effort cleanup of any containers this test left behind.
    docker ps -a --filter "label=dctl.project=$real_dir" -q 2>/dev/null \
        | xargs -r docker rm -f >/dev/null 2>&1
    cd /
    # Residual falkordb data dirs are owned by the container user; a plain
    # `rm` may fail. Try host-side first; fall back to a privileged Alpine
    # container. The scratch HOME gets the same treatment.
    for target in "$dir" "$home"; do
        if [[ -d "$target" ]] && ! rm -rf "$target" 2>/dev/null; then
            docker run --rm -v "$(dirname "$target"):/work" alpine:latest \
                rm -rf "/work/$(basename "$target")" >/dev/null 2>&1 || true
        fi
    done
}

# The per-project state bucket (ADR-0012), mirroring the binary's
# ~/.dctl/projects/<first-16-sha256-hex-of-canonical-cwd>/servers address.
# $1 = canonical project dir (run_case exports real_dir via dynamic scope).
servers() {
    local id
    id=$(printf '%s' "$1" | sha256sum | cut -c1-16)
    printf '%s/.dctl/projects/%s/servers' "${DCTL_TEST_HOME:-$HOME}" "$id"
}

# Helper: fail the case
die() { echo "    -> $*"; return 1; }

# Container id of instance $1 (name), for docker exec probes.
cid_of() {
    jq -r .container_id "$(servers "$real_dir")/$1-fk$FK_VERSION.json"
}

# ── 1. Certificate face: TLS-only server answers the in-container client ──
case_cert_face_serves_tls() {
    "$CTL" local falkordb start --name a >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of a)
    local pong; pong=$(docker exec "$cid" $FK_TLS_CLI ping 2>&1)
    [[ "$pong" == "PONG" ]] || { die "TLS ping: $pong"; return 1; }
    local one; one=$(docker exec "$cid" $FK_TLS_CLI GRAPH.QUERY g "RETURN 1" 2>&1 | tr -d '\r')
    # Non-interactive redis-cli prints a bare integer line for scalar
    # replies (no "(integer)" decoration).
    echo "$one" | grep -qx 1 || { die "GRAPH.QUERY over TLS: $one"; return 1; }
    # Recorded, not failed (review G5): the Browser UI's own listener on
    # 3000 — the server's plaintext redis listener is off on this face, so
    # whether the browser still works is an open product question.
    if ! docker exec "$cid" timeout 3 bash -c 'echo > /dev/tcp/127.0.0.1/3000' >/dev/null 2>&1; then
        echo "    note: browser port 3000 not reachable in-container (recorded)"
    fi
    "$CTL" local falkordb stop a >/dev/null 2>&1
    "$CTL" local falkordb remove a >/dev/null 2>&1
}

# ── 2. Certificate face: plaintext is refused (--port 0) ──
case_cert_face_refuses_plaintext() {
    "$CTL" local falkordb start --name b >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of b)
    if docker exec "$cid" redis-cli --no-auth-warning -p 6379 ping >/dev/null 2>&1; then
        die "plaintext ping unexpectedly succeeded"
        return 1
    fi
    "$CTL" local falkordb stop b >/dev/null 2>&1
    "$CTL" local falkordb remove b >/dev/null 2>&1
}

# ── 3. Certificate face dotenv: TLS material keys, no password ──
case_cert_face_dotenv_shape() {
    "$CTL" local falkordb start --name c >/dev/null 2>&1 || { die "start"; return 1; }
    "$CTL" local falkordb dotenv --name c >/dev/null 2>&1 || { die "dotenv"; return 1; }
    grep -q "^FALKORDB_TLS=true$" .env || { die "no FALKORDB_TLS"; return 1; }
    grep -q "^FALKORDB_CA_CERT=" .env || { die "no FALKORDB_CA_CERT"; return 1; }
    grep -q "^FALKORDB_CLIENT_CERT=" .env || { die "no FALKORDB_CLIENT_CERT"; return 1; }
    grep -q "^FALKORDB_CLIENT_KEY=" .env || { die "no FALKORDB_CLIENT_KEY"; return 1; }
    if grep -q "^FALKORDB_PASSWORD=" .env; then die "cert face emitted a password"; return 1; fi
    # The Browser UI's backend speaks plaintext redis, which the TLS-only
    # listener closed: the face emits no dead URL.
    if grep -q "^FALKORDB_BROWSER_URL=" .env; then die "cert face emitted a browser URL"; return 1; fi
    "$CTL" local falkordb stop c >/dev/null 2>&1
    "$CTL" local falkordb remove c >/dev/null 2>&1
}

# ── 4. Password face: requirepass round trip and dotenv ──
case_password_face_round_trip() {
    "$CTL" local falkordb start --name d --auth password --password battery-secret-1 >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of d)
    local pong; pong=$(docker exec -e REDISCLI_AUTH=battery-secret-1 "$cid" redis-cli --no-auth-warning ping 2>&1)
    [[ "$pong" == "PONG" ]] || { die "password ping: $pong"; return 1; }
    "$CTL" local falkordb dotenv --name d >/dev/null 2>&1 || { die "dotenv"; return 1; }
    grep -q "^FALKORDB_PASSWORD=battery-secret-1$" .env || { die "dotenv password mismatch"; return 1; }
    if grep -q "^FALKORDB_TLS=" .env; then die "password face emitted TLS keys"; return 1; fi
    "$CTL" local falkordb stop d >/dev/null 2>&1
    "$CTL" local falkordb remove d >/dev/null 2>&1
}

# ── 5. Resume keeps the face and the credential ──
case_resume_keeps_face() {
    "$CTL" local falkordb start --name e --auth password --password battery-secret-2 >/dev/null 2>&1 || { die "start"; return 1; }
    local meta; meta="$(servers "$real_dir")/e-fk$FK_VERSION.json"
    "$CTL" local falkordb stop e >/dev/null 2>&1 || { die "stop"; return 1; }
    # A resume without --auth keeps the password face (flags are ignored on
    # resume; the stored face wins).
    "$CTL" local falkordb start --name e >/dev/null 2>&1 || { die "resume"; return 1; }
    jq -e '.tls == false' "$meta" >/dev/null || { die "face flipped after resume"; return 1; }
    local cid; cid=$(cid_of e)
    local pong; pong=$(docker exec -e REDISCLI_AUTH=battery-secret-2 "$cid" redis-cli --no-auth-warning ping 2>&1)
    [[ "$pong" == "PONG" ]] || { die "resumed password ping: $pong"; return 1; }
    "$CTL" local falkordb stop e >/dev/null 2>&1
    "$CTL" local falkordb remove e >/dev/null 2>&1
}

# ── 6. Certificate-face resume keeps TLS ──
case_cert_resume_keeps_tls() {
    "$CTL" local falkordb start --name f >/dev/null 2>&1 || { die "start"; return 1; }
    local meta; meta="$(servers "$real_dir")/f-fk$FK_VERSION.json"
    "$CTL" local falkordb stop f >/dev/null 2>&1 || { die "stop"; return 1; }
    "$CTL" local falkordb start --name f --auth password >/dev/null 2>&1 || { die "resume"; return 1; }
    jq -e '.tls == true' "$meta" >/dev/null || { die "face flipped after resume"; return 1; }
    local cid; cid=$(cid_of f)
    local pong; pong=$(docker exec "$cid" $FK_TLS_CLI ping 2>&1)
    [[ "$pong" == "PONG" ]] || { die "resumed TLS ping: $pong"; return 1; }
    "$CTL" local falkordb stop f >/dev/null 2>&1
    "$CTL" local falkordb remove f >/dev/null 2>&1
}

# ── 7. Orphan recovery: metadata rebuilt from container labels ──
case_orphan_recovery() {
    "$CTL" local falkordb start --name g >/dev/null 2>&1 || { die "start"; return 1; }
    local meta; meta="$(servers "$real_dir")/g-fk$FK_VERSION.json"
    local cid_before; cid_before=$(jq -r .container_id "$meta")
    "$CTL" local falkordb stop g >/dev/null 2>&1 || { die "stop"; return 1; }
    rm "$meta"
    "$CTL" local server list 2>&1 | grep -q "^| g " || { die "list did not see g"; return 1; }
    [[ -f "$meta" ]] || { die "metadata not recovered"; return 1; }
    local cid_after; cid_after=$(jq -r .container_id "$meta")
    [[ "$cid_before" == "$cid_after" ]] || { die "container id changed: $cid_before -> $cid_after"; return 1; }
    # The recovered face must be the certificate face (REDIS_ARGS carries
    # --tls-port), or a later client would authenticate the wrong way.
    jq -e '.tls == true' "$meta" >/dev/null || { die "recovery lost the tls face"; return 1; }
    "$CTL" local falkordb remove g >/dev/null 2>&1 || { die "remove"; return 1; }
}

# ── 8. Two named instances coexist on different ports ──
case_two_concurrent_instances() {
    "$CTL" local falkordb start --name h1 >/dev/null 2>&1 || { die "start h1"; return 1; }
    "$CTL" local falkordb start --name h2 >/dev/null 2>&1 || { die "start h2"; return 1; }
    local p1 p2
    p1=$(jq -r .tcp_port "$(servers "$real_dir")/h1-fk$FK_VERSION.json")
    p2=$(jq -r .tcp_port "$(servers "$real_dir")/h2-fk$FK_VERSION.json")
    [[ "$p1" != "$p2" ]] || { die "ports collide: $p1 == $p2"; return 1; }
    [[ "$p1" -gt 0 && "$p2" -gt 0 ]] || { die "ports invalid"; return 1; }
    "$CTL" local falkordb stop h1 >/dev/null 2>&1
    "$CTL" local falkordb stop h2 >/dev/null 2>&1
    "$CTL" local falkordb remove h1 >/dev/null 2>&1
    "$CTL" local falkordb remove h2 >/dev/null 2>&1
}

# ── 9. Same name, two versions: two isolated instances ──
case_per_version_isolation() {
    "$CTL" local falkordb start --name i --version 4.20.6 >/dev/null 2>&1 || { die "start 4.20.6"; return 1; }
    "$CTL" local falkordb stop i >/dev/null 2>&1
    "$CTL" local falkordb start --name i --version latest >/dev/null 2>&1 || { die "start latest"; return 1; }
    [[ -d "$(servers "$real_dir")/i-fk4.20.6/data" ]] || { die "4.20.6 data dir vanished"; return 1; }
    # Bare stop with two versions asks for --version.
    local out; out=$("$CTL" local falkordb stop i 2>&1) || true
    echo "$out" | grep -q "pass --version" || { die "no disambiguation message: $out"; return 1; }
    "$CTL" local falkordb stop i --version latest >/dev/null 2>&1 || { die "versioned stop failed"; return 1; }
    "$CTL" local falkordb remove i --version latest >/dev/null 2>&1 || { die "remove latest"; return 1; }
    "$CTL" local falkordb remove i --version 4.20.6 >/dev/null 2>&1 || { die "remove 4.20.6"; return 1; }
}

# ── 10. Remove of a running instance is rejected ──
case_remove_running_rejected() {
    "$CTL" local falkordb start --name j >/dev/null 2>&1 || { die "start"; return 1; }
    local out; out=$("$CTL" local falkordb remove j 2>&1) || true
    echo "$out" | grep -qE "already running|running" || { die "no rejection: $out"; return 1; }
    "$CTL" local falkordb stop j >/dev/null 2>&1
    "$CTL" local falkordb remove j >/dev/null 2>&1
}

run_case cert_face_serves_tls            case_cert_face_serves_tls
run_case cert_face_refuses_plaintext     case_cert_face_refuses_plaintext
run_case cert_face_dotenv_shape          case_cert_face_dotenv_shape
run_case password_face_round_trip        case_password_face_round_trip
run_case resume_keeps_face               case_resume_keeps_face
run_case cert_resume_keeps_tls           case_cert_resume_keeps_tls
run_case orphan_recovery                 case_orphan_recovery
run_case two_concurrent_instances        case_two_concurrent_instances
run_case per_version_isolation           case_per_version_isolation
run_case remove_running_rejected         case_remove_running_rejected

echo
echo "==== $PASS passed, $FAIL failed ===="
if (( FAIL > 0 )); then
    echo "failed: ${FAILED_TESTS[*]}"
    exit 1
fi
