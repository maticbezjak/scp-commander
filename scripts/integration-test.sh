#!/bin/bash
# End-to-end integration tests: drives scp-cli against real servers in Docker
# (SFTP, FTP, MinIO/S3). Requires docker with the compose plugin.
#
#   ./scripts/integration-test.sh          # full run
#   KEEP=1 ./scripts/integration-test.sh   # leave the containers running
set -euo pipefail

cd "$(dirname "$0")/.."
COMPOSE="docker compose -f tests/integration/docker-compose.yml"
WORK=$(mktemp -d)
# Run scp-cli with an isolated HOME (fresh known_hosts every run): containers
# get a new host key on each recreation, which would otherwise trip the
# fail-closed mismatch protection. Everything else (docker, cargo) keeps the
# real HOME.
FAKE_HOME="$WORK/home"
mkdir -p "$FAKE_HOME"
cli() { HOME="$FAKE_HOME" target/debug/scp-cli "$@"; }
CLI=cli
PASS=0
FAIL=0

cleanup() {
    [ "${KEEP:-0}" = "1" ] || $COMPOSE down -v >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

check() { # check <label> <command...>
    local label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        echo "  ok   $label"
        PASS=$((PASS + 1))
    else
        echo "  FAIL $label"
        FAIL=$((FAIL + 1))
    fi
}

echo "==> Building scp-cli (with S3)"
cargo build -p scp-core --features s3 --bin scp-cli


echo "==> Starting servers"
$COMPOSE up -d sftp ftp minio ssh
$COMPOSE up mc # one-shot: creates the S3 test bucket

# Wait until each server actually answers (cold container starts take a
# few seconds; a blind sleep flakes).
wait_ready() { # wait_ready <label> <probe...>
    local label=$1
    shift
    for _ in $(seq 1 30); do
        if "$@" >/dev/null 2>&1; then
            echo "  $label ready"
            return 0
        fi
        sleep 1
    done
    echo "  $label NOT ready after 30s" >&2
    return 1
}
wait_ready sftp cli --accept-new ls "sftp://demo@127.0.0.1:2222/upload" demopass
wait_ready ftp  cli ls "ftp://demo@127.0.0.1:2121/ftp/demo" demopass
wait_ready ssh  cli --accept-new exec "sftp://demo@127.0.0.1:2223/" "echo ready" demopass

# Test fixtures: a tree with a subdirectory and a dotfile.
mkdir -p "$WORK/src/sub"
echo "hello integration" > "$WORK/src/a.txt"
echo "nested" > "$WORK/src/sub/b.txt"
echo "dot" > "$WORK/src/.hidden"
dd if=/dev/urandom of="$WORK/src/big.bin" bs=1k count=512 2>/dev/null

run_matrix() { # run_matrix <label> <url-base> <password> [mv-root-prefix] <extra-flags...>
    local label=$1 base=$2 pass=$3
    shift 3
    # Optional 4th positional: the absolute-path prefix used for the mv target.
    # SFTP/FTP need the chroot prefix ("/$root"); S3 paths are already
    # relative to the bucket so the prefix is "".
    local mv_pfx_set=0
    local mv_prefix=""
    if [ "$#" -gt 0 ] && [[ "${1:-}" != --* ]]; then
        mv_prefix="$1"
        mv_pfx_set=1
        shift
    fi
    # ($@ is already shifted; assignment of an empty array is fine under
    # set -u — only unguarded expansion is not, hence the FL indirection.)
    local flags=("$@")
    local FL=""
    if [ "$#" -gt 0 ]; then FL="${flags[*]}"; fi
    # The rename target must stay inside the same (chrooted) tree.
    local root="${base#*//*/}"      # path part of the url
    echo "== $label =="
    check "$label mkdir"     $CLI $FL mkdir "$base/it" "$pass"
    check "$label put"       $CLI $FL put "$base/it/a.txt" "$WORK/src/a.txt" "$pass"
    check "$label ls"        $CLI $FL ls "$base/it" "$pass"
    check "$label get"       $CLI $FL get "$base/it/a.txt" "$WORK/$label-a.txt" "$pass"
    check "$label roundtrip" diff "$WORK/src/a.txt" "$WORK/$label-a.txt"
    check "$label put -r"    $CLI $FL put -r "$base/it/tree" "$WORK/src" "$pass"
    check "$label get -r"    $CLI $FL get -r "$base/it/tree" "$WORK/$label-tree" "$pass"
    check "$label tree diff" diff -r "$WORK/src" "$WORK/$label-tree"
    check "$label sync up"   $CLI $FL sync up "$base/it/sync" "$WORK/src" "$pass"
    check "$label sync down" $CLI $FL sync down "$base/it/sync" "$WORK/$label-sync" "$pass"
    check "$label sync diff" diff -r "$WORK/src" "$WORK/$label-sync"
    check "$label plan"      $CLI $FL plan up "$base/it/sync" "$WORK/src" "$pass"
    check "$label find"      $CLI $FL find "$base/it" "*.txt" "$pass"
    local mv_dst
    if [ "$mv_pfx_set" -eq 1 ]; then
        mv_dst="${mv_prefix}/it/renamed.txt"
    else
        mv_dst="/$root/it/renamed.txt"
    fi
    check "$label mv"        $CLI $FL mv "$base/it/a.txt" "$mv_dst" "$pass"
    check "$label rm"        $CLI $FL rm "$base/it/renamed.txt" "$pass"
    check "$label rm -r"     $CLI $FL rm -r "$base/it/tree" "$pass"
}

# SFTP — also exercises host-key trust-on-first-use and chmod.
run_matrix sftp "sftp://demo@127.0.0.1:2222/upload" demopass "/upload" --accept-new
check "sftp chmod" $CLI --accept-new chmod 600 "sftp://demo@127.0.0.1:2222/upload/it/sync/a.txt" demopass

# FTP
run_matrix ftp "ftp://demo@127.0.0.1:2121/ftp/demo" demopass "/ftp/demo"

wait_ready s3 cli ls "s3://demo@127.0.0.1:9000/test" demopass123

# S3 — full matrix against MinIO. S3 paths are already relative to the bucket
# (no mv-root prefix needed — pass empty string so ${mv_prefix:-...} is overridden).
run_matrix s3 "s3://demo@127.0.0.1:9000/test" demopass123 ""

# SFTP exec: smoke-test the remote command channel on a full SSH server
# (atmoz/sftp uses ForceCommand internal-sftp and can't run arbitrary commands).
echo "== sftp exec =="
check "sftp exec echo"  $CLI --accept-new exec "sftp://demo@127.0.0.1:2223/" "echo hello" demopass
check "sftp exec exit1" bash -c '! '"$CLI"' --accept-new exec sftp://demo@127.0.0.1:2223/ "exit 1" demopass'

# Mirror sync: upload a tree, remove one local file, re-sync with --delete,
# verify the remote file was removed.
echo "== sftp mirror sync =="
mkdir -p "$WORK/mirror"
echo "keep" > "$WORK/mirror/keep.txt"
echo "gone" > "$WORK/mirror/gone.txt"
check "mirror init"   $CLI --accept-new sync up  "sftp://demo@127.0.0.1:2222/upload/mirror" "$WORK/mirror" demopass
rm "$WORK/mirror/gone.txt"
check "mirror --delete" $CLI --accept-new --delete sync up "sftp://demo@127.0.0.1:2222/upload/mirror" "$WORK/mirror" demopass
check "mirror verify"   bash -c '! '"$CLI"' --accept-new ls sftp://demo@127.0.0.1:2222/upload/mirror demopass | grep gone.txt'

echo
echo "Result: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
