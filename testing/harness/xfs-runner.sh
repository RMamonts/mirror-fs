#!/bin/bash
# Runs xfstests one test at a time inside the guest, with a per-test timeout and
# fresh NFS mounts for every test. Resumable: skips tests already in status file.
W=/opt/vm/work
export HOST_OPTIONS=$W/xfs-local.config
. $HOST_OPTIONS
LIST=$W/tests.txt; STATUS=$W/out/xfs-status.txt; LOGD=$W/out/xfs-logs
mkdir -p $LOGD /mnt/xtest /mnt/xscratch; touch $STATUS
cd /opt/src/xfstests
for t in $(cat $LIST); do
  grep -q "^$t " $STATUS && continue
  umount -f -l $TEST_DIR $SCRATCH_MNT 2>/dev/null
  dmesg -C
  s=$(date +%s)
  timeout -k 60 ${TEST_TIMEOUT:-2400} ./check $t > $LOGD/${t//\//_}.out 2>&1; rc=$?
  d=$(( $(date +%s) - s ))
  out=$LOGD/${t//\//_}.out
  if [ $rc -eq 124 -o $rc -eq 137 ]; then st=TIMEOUT
  elif grep -q "^Not run: .*$t" $out; then st=NOTRUN
  elif grep -q "^Failures: .*$t" $out; then st=FAIL
  elif grep -q "^Passed all" $out; then st=PASS
  else st=UNKNOWN; fi
  reason=$(grep -E "^$t " $out | head -1 | sed -e "s|^$t *||" | tr -s ' ' | cut -c1-200)
  echo "$t $st ${d}s rc=$rc | $reason" >> $STATUS
  dmesg | grep -iE 'nfs|rpc|hung|BUG|WARN|stale' | tail -20 > $LOGD/${t//\//_}.dmesg
  [ -s $LOGD/${t//\//_}.dmesg ] || rm -f $LOGD/${t//\//_}.dmesg
done
umount -f -l $TEST_DIR $SCRATCH_MNT 2>/dev/null
echo ALL_DONE >> $STATUS
