#!/bin/bash
# Creates N xfstests shards: tap device, mirrorfs instance (port 205N), work dir.
for n in "$@"; do
  ip link show tap$n >/dev/null 2>&1 || { ip tuntap add dev tap$n mode tap; ip addr add 10.10.1$n.1/24 dev tap$n; ip link set tap$n up; }
  mkdir -p /srv/xfs$n/test /srv/xfs$n/scratch
  printf 'vfs_pool_size = 10\n\n[allocator]\nbuffer_size = 1048576\nbuffer_count = 256\n\n[exports]\nroot = "/srv/xfs%s"\npaths = ["test", "scratch"]\n' $n > /opt/mirrorfs/config-xfs$n.toml
  W=/opt/vm/xs$n-work
  mkdir -p $W/queue $W/out $W/done
  awk -v n=$n 'NR % 4 == (n-1)' /opt/vm/xfs-quick-tests.txt > $W/tests.txt
  cat > $W/xfs-local.config <<EOF
export FSTYP=nfs
export TEST_DEV=10.10.1$n.1:/test
export TEST_DIR=/mnt/xtest
export SCRATCH_DEV=10.10.1$n.1:/scratch
export SCRATCH_MNT=/mnt/xscratch
export NFS_MOUNT_OPTIONS="-o vers=3,proto=tcp,port=205$n,mountport=205$n,mountproto=tcp,nolock"
export RESULT_BASE=/opt/vm/work/xfs-results
# WORKAROUND: mirrorfs FSSTAT reports 0 total/free bytes; plain df hides the mount.
export DF_PROG="df -T -P -a"
EOF
  cp /opt/vm/xfs-runner.sh $W/xfs-runner.sh
  echo "bash /opt/vm/work/xfs-runner.sh" > $W/queue/500-xfs-run.sh
  cat > /opt/mirrorfs/supervise-xfs$n.sh <<EOF
#!/bin/bash
cd /opt/mirrorfs
while true; do
  echo "=== start \$(date)" >> /opt/logs/mirrorfs-xfs$n.log
  ./mirrorfs-0.2.0 -c config-xfs$n.toml -a 0.0.0.0:205$n --no-root-squash >> /opt/logs/mirrorfs-xfs$n.log 2>&1
  echo "=== EXIT rc=\$? \$(date)" >> /opt/logs/mirrorfs-xfs$n.log
  sleep 2
done
EOF
  chmod +x /opt/mirrorfs/supervise-xfs$n.sh
  setsid nohup /opt/mirrorfs/supervise-xfs$n.sh >/dev/null 2>&1 < /dev/null &
done
