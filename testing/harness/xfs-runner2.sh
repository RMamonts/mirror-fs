#!/bin/bash
# One `check` process for the whole shard list (per-test check startup is ~90s
# under TCG). Watchdog kills a test that runs longer than TEST_TIMEOUT seconds.
W=/opt/vm/work
export HOST_OPTIONS=$W/xfs-local.config
. $HOST_OPTIONS
TIMEOUT=${TEST_TIMEOUT:-3000}
OUT=$W/out/check.out; WD=$W/out/watchdog.log
mkdir -p /mnt/xtest /mnt/xscratch
cd /opt/src/xfstests
killtree() { local p=$1 c; for c in $(pgrep -P $p); do killtree $c; done; kill -KILL $p 2>/dev/null; }
./check $(cat $W/tests.txt) > $OUT 2>&1 &
CPID=$!
last=""; since=$(date +%s)
while kill -0 $CPID 2>/dev/null; do
  sleep 15
  cur=$(tail -n 1 $OUT | awk '{print $1}')
  if [ "$cur" != "$last" ]; then last=$cur; since=$(date +%s); continue; fi
  if [ $(( $(date +%s) - since )) -gt $TIMEOUT ]; then
    echo "$(date) WATCHDOG timeout: $cur" >> $WD
    dmesg | tail -50 >> $WD
    for p in $(pgrep -f "tests/$cur\$"); do killtree $p; done
    since=$(date +%s)
  fi
done
wait $CPID; echo "check rc=$?" >> $WD
cp -a $RESULT_BASE/check.log $W/out/ 2>/dev/null
echo ALL_DONE >> $WD
