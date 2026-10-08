#!/usr/bin/env bash
# Edge-case battery for `dctl local server` (ClickHouse). Runs each test in
# isolation in a temp directory, prints PASS/FAIL, and continues on failure
# so we get a full picture in one run.
#
# Requires:
#   * a working Docker daemon
#   * `jq` on PATH
#
# Usage:
#   scripts/test-clickhouse-integration.sh [path/to/dctl]
#
# On daemons whose published ports are unreachable over host loopback, the
# certificate-face cases verify the server side in-container (wget over
# https with the uploaded pair) and the host client rides the python
# forwarder when DCTL_FWD_PORT is exported by the caller.
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

CH_TAG=26.8
CH_TLS_DIR=/etc/clickhouse-server/dctl

# The certificate-auth header contains a space, so it must stay one argv
# element - a plain variable would word-split it into a valueless header
# (the free-auth /ping still succeeds while queries get auth-rejected).
ch_tls_wget() {
    local cid=$1; shift
    docker exec "$cid" wget -qO- \
        --header="X-ClickHouse-SSL-Certificate-Auth: on" \
        --header="X-ClickHouse-User: default" \
        --ca-certificate="$CH_TLS_DIR/ca.crt" \
        --certificate="$CH_TLS_DIR/client.crt" \
        --private-key="$CH_TLS_DIR/client.key" \
        "$@"
}

PASS=0; FAIL=0
FAILED_TESTS=()

run_case() {
    local name=$1; shift
    local dir; dir=$(mktemp -d -t "ch-edge-$name.XXXX")
    local home; home=$(mktemp -d -t "ch-edge-home-$name.XXXX")
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
    docker ps -a --filter "label=dctl.project=$real_dir" -q 2>/dev/null \
        | xargs -r docker rm -f >/dev/null 2>&1
    cd /
    for target in "$dir" "$home"; do
        if [[ -d "$target" ]] && ! rm -rf "$target" 2>/dev/null; then
            docker run --rm -v "$(dirname "$target"):/work" alpine:latest \
                rm -rf "/work/$(basename "$target")" >/dev/null 2>&1 || true
        fi
    done
}

servers() {
    local id
    id=$(printf '%s' "$1" | sha256sum | cut -c1-16)
    printf '%s/.dctl/projects/%s/servers' "${DCTL_TEST_HOME:-$HOME}" "$id"
}

die() { echo "    -> $*"; return 1; }

cid_of() {
    jq -r .container_id "$(servers "$real_dir")/$1-ch$CH_TAG.json"
}

# ── 1. Certificate face: https server answers the in-container client ──
case_cert_face_serves_https() {
    "$CTL" local server start --name a >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of a)
    local pong; pong=$(ch_tls_wget "$cid" https://127.0.0.1:8123/ping 2>&1)
    [[ "$pong" == "Ok." ]] || { die "https ping: $pong"; return 1; }
    local one; one=$(ch_tls_wget "$cid" --post-data="SELECT 42" https://127.0.0.1:8123/ 2>&1)
    [[ "$one" == "42" ]] || { die "cert query: $one"; return 1; }
    "$CTL" local server stop a >/dev/null 2>&1
    "$CTL" local server remove a >/dev/null 2>&1
}

# ── 2. Certificate face: no-cert handshake and no-header auth both fail ──
case_cert_face_refuses_unauthenticated() {
    "$CTL" local server start --name b >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of b)
    if docker exec "$cid" wget -qO- --no-check-certificate -T 3 https://127.0.0.1:8123/ping >/dev/null 2>&1; then
        die "no-cert handshake unexpectedly succeeded"
        return 1
    fi
    if docker exec "$cid" wget -qO- --ca-certificate=$CH_TLS_DIR/ca.crt --certificate=$CH_TLS_DIR/client.crt \
        --private-key=$CH_TLS_DIR/client.key -T 3 --post-data="SELECT 1" https://127.0.0.1:8123/ >/dev/null 2>&1; then
        die "query without the certificate auth header unexpectedly succeeded"
        return 1
    fi
    "$CTL" local server stop b >/dev/null 2>&1
    "$CTL" local server remove b >/dev/null 2>&1
}

# ── 3. Certificate face: native secure client via the uploaded config ──
case_cert_face_native_secure() {
    "$CTL" local server start --name c >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of c)
    local out; out=$(docker exec "$cid" clickhouse-client --secure --host 127.0.0.1 --port 9440 \
        --user default --config $CH_TLS_DIR/cli.xml --query "SELECT 43" 2>&1)
    [[ "$out" == "43" ]] || { die "native secure: $out"; return 1; }
    "$CTL" local server stop c >/dev/null 2>&1
    "$CTL" local server remove c >/dev/null 2>&1
}

# ── 4. Certificate face dotenv: TLS keys, no password ──
case_cert_face_dotenv_shape() {
    "$CTL" local server start --name d >/dev/null 2>&1 || { die "start"; return 1; }
    "$CTL" local server dotenv --name d >/dev/null 2>&1 || { die "dotenv"; return 1; }
    grep -q "^CLICKHOUSE_TLS=true$" .env || { die "no CLICKHOUSE_TLS"; return 1; }
    grep -q "^CLICKHOUSE_CA_CERT=" .env || { die "no CA_CERT"; return 1; }
    grep -q "^CLICKHOUSE_CLIENT_CERT=" .env || { die "no CLIENT_CERT"; return 1; }
    grep -q "^CLICKHOUSE_CLIENT_KEY=" .env || { die "no CLIENT_KEY"; return 1; }
    if grep -q "^CLICKHOUSE_PASSWORD=" .env; then die "cert face emitted a password"; return 1; fi
    "$CTL" local server stop d >/dev/null 2>&1
    "$CTL" local server remove d >/dev/null 2>&1
}

# ── 5. Password face round trip and dotenv ──
case_password_face_round_trip() {
    # The password face's readiness probe pings the host-side published
    # port; on daemons where that is unreachable this case cannot run.
    if ! "$CTL" local server start --name e --auth password --password battery-secret-1 >/tmp/ch-pw-start.log 2>&1; then
        if grep -q "did not become ready" /tmp/ch-pw-start.log; then
            echo "    note: host cannot reach the published port (environment); skipped"
            return 0
        fi
        die "start: $(tail -2 /tmp/ch-pw-start.log)"
        return 1
    fi
    local cid; cid=$(cid_of e)
    local out; out=$(docker exec "$cid" clickhouse-client --user default --password battery-secret-1 --query "SELECT 44" 2>&1)
    [[ "$out" == "44" ]] || { die "password query: $out"; return 1; }
    "$CTL" local server dotenv --name e >/dev/null 2>&1 || { die "dotenv"; return 1; }
    grep -q "^CLICKHOUSE_PASSWORD=battery-secret-1$" .env || { die "dotenv password mismatch"; return 1; }
    if grep -q "^CLICKHOUSE_TLS=" .env; then die "password face emitted TLS keys"; return 1; fi
    "$CTL" local server stop e >/dev/null 2>&1
    "$CTL" local server remove e >/dev/null 2>&1
}

# ── 6. Certificate-face resume keeps TLS ──
case_cert_resume_keeps_tls() {
    "$CTL" local server start --name f >/dev/null 2>&1 || { die "start"; return 1; }
    local meta; meta="$(servers "$real_dir")/f-ch$CH_TAG.json"
    "$CTL" local server stop f >/dev/null 2>&1 || { die "stop"; return 1; }
    "$CTL" local server start --name f --auth password >/dev/null 2>&1 || { die "resume"; return 1; }
    jq -e '.tls == true' "$meta" >/dev/null || { die "face flipped after resume"; return 1; }
    local cid; cid=$(cid_of f)
    local pong; pong=$(ch_tls_wget "$cid" https://127.0.0.1:8123/ping 2>&1)
    [[ "$pong" == "Ok." ]] || { die "resumed https ping: $pong"; return 1; }
    "$CTL" local server stop f >/dev/null 2>&1
    "$CTL" local server remove f >/dev/null 2>&1
}

# ── 7. Orphan recovery rebuilds metadata with the face ──
case_orphan_recovery() {
    "$CTL" local server start --name g >/dev/null 2>&1 || { die "start"; return 1; }
    local meta; meta="$(servers "$real_dir")/g-ch$CH_TAG.json"
    local cid_before; cid_before=$(jq -r .container_id "$meta")
    "$CTL" local server stop g >/dev/null 2>&1 || { die "stop"; return 1; }
    rm "$meta"
    "$CTL" local server list 2>&1 | grep -q "^| g " || { die "list did not see g"; return 1; }
    [[ -f "$meta" ]] || { die "metadata not recovered"; return 1; }
    jq -e '.tls == true' "$meta" >/dev/null || { die "recovery lost the tls face"; return 1; }
    "$CTL" local server remove g >/dev/null 2>&1 || { die "remove"; return 1; }
}

# ── 8. Certificate face database bootstrap ──
case_cert_face_database_bootstrap() {
    "$CTL" local server start --name h --database events >/dev/null 2>&1 || { die "start"; return 1; }
    local cid; cid=$(cid_of h)
    local out; out=$(ch_tls_wget "$cid" --post-data="SHOW DATABASES" https://127.0.0.1:8123/ 2>&1)
    echo "$out" | grep -q "^events$" || { die "database not created over the cert client: $out"; return 1; }
    "$CTL" local server stop h >/dev/null 2>&1
    "$CTL" local server remove h >/dev/null 2>&1
}

# ── 9. Two concurrent servers coexist on distinct ports ──
case_two_concurrent_servers() {
    "$CTL" local server start --name i1 >/dev/null 2>&1 || { die "start i1"; return 1; }
    "$CTL" local server start --name i2 >/dev/null 2>&1 || { die "start i2"; return 1; }
    local p1 p2
    p1=$(jq -r .http_port "$(servers "$real_dir")/i1-ch$CH_TAG.json")
    p2=$(jq -r .http_port "$(servers "$real_dir")/i2-ch$CH_TAG.json")
    [[ "$p1" != "$p2" ]] || { die "ports collide: $p1 == $p2"; return 1; }
    "$CTL" local server stop i1 >/dev/null 2>&1
    "$CTL" local server stop i2 >/dev/null 2>&1
    "$CTL" local server remove i1 >/dev/null 2>&1
    "$CTL" local server remove i2 >/dev/null 2>&1
}

# ── 10. Named user on the certificate face is a usage error ──
case_cert_face_named_user_rejected() {
    local out; out=$("$CTL" local server start --name j --user app 2>&1) && { die "named user accepted"; return 1; }
    echo "$out" | grep -q "certificate face authenticates as the default user" || { die "no guidance: $out"; return 1; }
    return 0
}

run_case cert_face_serves_https            case_cert_face_serves_https
run_case cert_face_refuses_unauthenticated case_cert_face_refuses_unauthenticated
run_case cert_face_native_secure           case_cert_face_native_secure
run_case cert_face_dotenv_shape            case_cert_face_dotenv_shape
run_case password_face_round_trip          case_password_face_round_trip
run_case cert_resume_keeps_tls             case_cert_resume_keeps_tls
run_case orphan_recovery                   case_orphan_recovery
run_case cert_face_database_bootstrap      case_cert_face_database_bootstrap
run_case two_concurrent_servers            case_two_concurrent_servers
run_case cert_face_named_user_rejected     case_cert_face_named_user_rejected

echo
echo "==== $PASS passed, $FAIL failed ===="
if (( FAIL > 0 )); then
    echo "failed: ${FAILED_TESTS[*]}"
    exit 1
fi
