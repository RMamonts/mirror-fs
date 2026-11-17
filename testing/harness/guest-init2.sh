#!/bin/bash
# PID 1 inside the QEMU guest. Host root is mounted read-write via virtio-9p.
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
mkdir -p /dev/pts /dev/shm
mount -t devpts devpts /dev/pts
mount -t tmpfs tmpfs /dev/shm
mount -t tmpfs tmpfs /run
mount -t tmpfs -o size=2g tmpfs /tmp
mount -t tmpfs tmpfs /mnt
# Coherent (uncached) share for job control/logs.
mount -t 9p -o trans=virtio,version=9p2000.L,msize=524288,cache=none,access=client work /opt/vm/work
hostname nfs-client-vm
ip link set lo up
ip link set eth0 up
VMIP=10.10.10.2/24; for a in $(cat /proc/cmdline); do case "$a" in vmip=*) VMIP="${a#vmip=}";; esac; done
ip addr add $VMIP dev eth0
modprobe nfsv3 && echo "nfsv3 module loaded"
sysctl -q -w kernel.hung_task_timeout_secs=0 kernel.panic_on_oops=0
echo "GUEST READY $(uname -r) $(date)" | tee /opt/vm/work/out/ready
W=/opt/vm/work
while true; do
  for j in $(ls $W/queue 2>/dev/null | sort); do
    f=$W/queue/$j
    [ "$j" = "poweroff" ] && { rm -f $f; sync; echo o > /proc/sysrq-trigger; sleep 30; }
    mv $f $W/done/$j
    echo "=== job $j start $(date)" > /dev/console
    ( cd /; setsid bash $W/done/$j > $W/out/$j.log 2>&1 < /dev/null; echo $? > $W/out/$j.rc )
    echo "=== job $j end rc=$(cat $W/out/$j.rc) $(date)" > /dev/console
  done
  sleep 2
done
