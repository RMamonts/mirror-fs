#!/bin/bash
# Usage: start-vm2.sh <name> <tap> <guest-ip/cidr> [smp] [mem]
# Boots an NFS client VM (TCG). Job dir: /opt/vm/<name>-work, console: /opt/vm/<name>-console.log
NAME=$1; TAP=$2; VMIP=$3; SMP=${4:-2}; MEM=${5:-4096}
KV=7.0.0-42-generic
W=/opt/vm/$NAME-work; mkdir -p $W/queue $W/out $W/done
exec qemu-system-x86_64 -machine q35 -accel tcg,thread=multi,tb-size=1024 -cpu max -smp $SMP -m $MEM \
  -kernel /opt/vm/kernel/boot/vmlinuz-$KV -initrd /opt/vm/initrd2.img \
  -append "console=ttyS0 rdinit=/init loglevel=4 nokaslr mitigations=off tsc=reliable ginit=/opt/vm/guest-init2.sh vmip=$VMIP" \
  -fsdev local,id=fs0,path=/,security_model=passthrough,multidevs=remap -device virtio-9p-pci,fsdev=fs0,mount_tag=hostroot \
  -fsdev local,id=fs1,path=$W,security_model=passthrough -device virtio-9p-pci,fsdev=fs1,mount_tag=work \
  -netdev tap,id=n0,ifname=$TAP,script=no,downscript=no -device virtio-net-pci,netdev=n0 \
  -display none -serial file:/opt/vm/$NAME-console.log -monitor none -no-reboot
