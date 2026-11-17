set -x
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# Real Linux knfsd NFSv3 server exporting a loopback ext4, mounted over loopback.
modprobe nfsd
mount -t nfsd nfsd /proc/fs/nfsd 2>/dev/null
rpcbind -w || rpcbind
mkdir -p /srv/base /mnt/base
dd if=/dev/zero of=/tmp/base.img bs=1M count=2048 status=none
mkfs.ext4 -q -F /tmp/base.img
mount -o loop /tmp/base.img /srv/base
chmod 777 /srv/base
echo "/srv/base 127.0.0.1(rw,sync,no_root_squash,no_subtree_check,insecure,fsid=7)" > /etc/exports
exportfs -ra
exportfs -v
rpc.nfsd --no-nfs-version 4 --nfs-version 3 8
rpc.mountd --no-nfs-version 4
sleep 2
rpcinfo -p 127.0.0.1 | grep -E 'nfs|mount'
mount -t nfs -o vers=3,proto=tcp,nolock 127.0.0.1:/srv/base /mnt/base
echo "mount rc=$?"; grep /mnt/base /proc/mounts
cd /mnt/base
start=$(date +%s)
prove -r --timer /opt/src/pjdfstest/tests > /opt/vm/work/out/pjd-knfsd.tap 2>&1
echo "prove rc=$? elapsed=$(( $(date +%s) - start ))s"
tail -25 /opt/vm/work/out/pjd-knfsd.tap
cd /; umount /mnt/base; umount /srv/base
