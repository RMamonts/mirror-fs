#!/usr/bin/env bash
# Runs the runtime-layout benchmark matrix; results go to $OUT as JSON lines.
# Server is pinned to SERVER_CPUS, load generators to CLIENT_CPUS.
set -u
SRV=${SRV:-$(dirname "$0")/../target/release/mirror-fs}
LOAD=${LOAD:-$(dirname "$0")/nfs-load/target/release/nfs-load}
CFG=${CFG:-$(dirname "$0")/config.toml}
OUT=${OUT:-results.jsonl}
SERVER_CPUS=${SERVER_CPUS:-0,1}
CLIENT_CPUS=${CLIENT_CPUS:-2,3}
NET_THREADS=${NET_THREADS:-2}
DUR=${DUR:-8}
WARM=${WARM:-2}
REPS=${REPS:-3}
VARIANTS=${VARIANTS:-"one:inline one:blocking one:sync two:inline two:sync"}
SCENARIOS=${SCENARIOS:-"meta-fast meta-lat read-fast read-lat read-fast-probe meta-slow"}

run_server() { # rt io delay yield
  RUST_LOG=error MIRRORFS_RT=$1 MIRRORFS_IO=$2 MIRRORFS_STAT_DELAY_US=$3 MIRRORFS_YIELD=$4 \
    MIRRORFS_NET_THREADS=$NET_THREADS taskset -c $SERVER_CPUS "$SRV" -c "$CFG" --no-root-squash \
    >/dev/null 2>&1 &
  SRV_PID=$!
  for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/2049) 2>/dev/null && return; sleep 0.1; done
  echo "server did not start" >&2
}

load() { # tag args...
  local tag=$1; shift
  taskset -c $CLIENT_CPUS "$LOAD" --duration $DUR --warmup $WARM "$@" \
    | sed "s/^{/{\"scenario\":\"$SCEN\",\"variant\":\"$VAR\",\"rep\":$REP,\"role\":\"$tag\",/" >>"$OUT"
}

for REP in $(seq $REPS); do
  for SCEN in $SCENARIOS; do
    for VAR in $VARIANTS; do
      IFS=: read -r rt io y <<<"$VAR"; [ "$y" = y ] && y=1 || y=0
      case $SCEN in
        meta-slow) delay=1000 ;;
        *) delay=0 ;;
      esac
      run_server $rt $io $delay $y
      case $SCEN in
        meta-fast)
          load heavy --op getattr --conns 32 --threads 2 ;;
        meta-lat)
          load single --op getattr --conns 1 --threads 1 ;;
        read-lat)
          load single --op read --conns 1 --read-size 65536 --threads 1 ;;
        read-fast)
          load heavy --op read --conns 16 --read-size 65536 --threads 2 ;;
        read-fast-probe)
          load heavy --op read --conns 16 --read-size 65536 --threads 2 & P1=$!
          load probe-null --op null --conns 1 --think-us 1000 --threads 1 & P2=$!
          wait $P1 $P2 ;;
        meta-slow)
          load heavy --op getattr --conns 32 --threads 1 & P1=$!
          load probe-null --op null --conns 1 --think-us 1000 --threads 1 & P2=$!
          load probe-getattr --op getattr --conns 1 --think-us 1000 --threads 1 & P3=$!
          wait $P1 $P2 $P3 ;;
      esac
      kill -INT $SRV_PID; wait $SRV_PID 2>/dev/null
      echo "done rep=$REP $SCEN $VAR" >&2
    done
  done
done
