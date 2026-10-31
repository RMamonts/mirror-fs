#!/usr/bin/env bash
# Runs pjdfstest against mirror-fs over a loopback NFSv3 mount and compares the
# failing subtests with testing/ci/pjdfstest-known-failures.txt.
#
# Usage (as root):  run-pjdfstest.sh <mirror-fs binary> <built pjdfstest checkout>
#
# Environment:
#   PJD_TESTS    space-separated subset of pjdfstest/tests to run (default: all),
#                e.g. "chown chmod/00.t"
#   PORT         TCP port mirror-fs listens on (default: 20490)
#   OUT_DIR      where logs are written (default: ./pjdfstest-results)
#   PJD_TIMEOUT  overall time limit for prove (default: 40m)
#
# Root is required: pjdfstest switches uid/gid for its checks and mirror-fs
# needs to chown/mknod on the backing file system. mirror-fs runs with
# --no-root-squash so that root on the client is root on the export.
set -euo pipefail

usage="usage: $0 <mirror-fs binary> <pjdfstest dir>"
MIRRORFS=$(realpath "${1:?$usage}")
PJD=$(realpath "${2:?$usage}")
HERE=$(cd "$(dirname "$0")" && pwd)
PORT=${PORT:-20490}
OUT_DIR=${OUT_DIR:-$PWD/pjdfstest-results}
mkdir -p "$OUT_DIR"
OUT_DIR=$(cd "$OUT_DIR" && pwd)

if [ "$(id -u)" -ne 0 ]; then
    echo "error: must run as root" >&2
    exit 2
fi
[ -x "$PJD/pjdfstest" ] || { echo "error: $PJD/pjdfstest is not built" >&2; exit 2; }

# A short export path keeps pjdfstest's PATH_MAX tests comparable between
# environments (the export prefix is part of every path on the server).
WORK=$(mktemp -d /tmp/mfs.XXXXXX)
EXPORT=$WORK/export
MNT=$WORK/mnt
mkdir -p "$EXPORT/pjd" "$MNT"

# Distributions harden link/create behaviour via sysctl; pjdfstest expects
# the kernel defaults from the client VFS. Restore the values on exit.
declare -A SAVED_SYSCTL=()
for knob in protected_hardlinks protected_symlinks protected_fifos protected_regular; do
    file=/proc/sys/fs/$knob
    if [ -w "$file" ]; then
        SAVED_SYSCTL[$knob]=$(cat "$file")
        echo 0 >"$file"
    fi
done

SERVER_PID=
# shellcheck disable=SC2317  # invoked through the EXIT trap
cleanup() {
    cd /
    umount -f -l "$MNT" 2>/dev/null || true
    if [ -n "$SERVER_PID" ]; then
        kill -INT "$SERVER_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do kill -0 "$SERVER_PID" 2>/dev/null || break; sleep 0.1; done
        kill -KILL "$SERVER_PID" 2>/dev/null || true
    fi
    for knob in "${!SAVED_SYSCTL[@]}"; do
        echo "${SAVED_SYSCTL[$knob]}" >"/proc/sys/fs/$knob" || true
    done
    rm -rf -- "$WORK"
}
trap cleanup EXIT

cat >"$WORK/config.toml" <<EOF
vfs_pool_size = 10

[allocator]
buffer_size = 1048576
buffer_count = 256

[exports]
root = "$EXPORT"
paths = ["pjd"]
EOF

"$MIRRORFS" -c "$WORK/config.toml" -a "127.0.0.1:$PORT" --no-root-squash \
    >"$OUT_DIR/mirror-fs.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo "error: mirror-fs exited during startup:" >&2
        cat "$OUT_DIR/mirror-fs.log" >&2
        exit 1
    fi
    sleep 0.1
done

mount -t nfs -o "vers=3,proto=tcp,port=$PORT,mountport=$PORT,mountproto=tcp,nolock" \
    "127.0.0.1:/pjd" "$MNT"
grep " $MNT " /proc/mounts

tests=()
if [ -n "${PJD_TESTS:-}" ]; then
    for test in $PJD_TESTS; do tests+=("$PJD/tests/$test"); done
else
    tests=("$PJD/tests")
fi

# pjdfstest operates on the current directory.
cd "$MNT"
prove_rc=0
timeout -k 30s "${PJD_TIMEOUT:-40m}" prove -rv "${tests[@]}" >"$OUT_DIR/pjdfstest.log" 2>&1 \
    || prove_rc=$?
cd /
echo "prove exited with status $prove_rc"
if [ "$prove_rc" -eq 124 ]; then
    echo "error: pjdfstest timed out after ${PJD_TIMEOUT:-40m}" >&2
fi

python3 "$HERE/pjdfstest_compare.py" "$OUT_DIR/pjdfstest.log" \
    "$HERE/pjdfstest-known-failures.txt" | tee "$OUT_DIR/summary.txt"
exit "${PIPESTATUS[0]}"
