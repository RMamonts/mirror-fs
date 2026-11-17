MO="vers=3,proto=tcp,port=2060,mountport=2060,mountproto=tcp,nolock"
mkdir -p /mnt/p; mount -t nfs -o $MO 10.10.15.1:/a /mnt/p || exit 1
cd /mnt/p; rm -rf ./* 2>/dev/null
NB="setpriv --reuid=65534 --regid=65534 --clear-groups"
R() { echo "--- $*"; "$@" 2>&1; echo "   rc=$?"; }
echo "###### P1: file created by UNPRIVILEGED client (uid 65534) -> owned by?"
$NB sh -c 'echo hi > /mnt/p/byNobody' 2>&1; ls -ln /mnt/p/byNobody
echo "###### P2: root (no_root_squash) write to a 0444 file it owns"
echo a > ro; chmod 0444 ro; R dd if=/dev/zero of=ro bs=1 count=1 conv=notrunc
echo "###### P3: root read a 0000 file"
echo secret > z; chmod 0000 z; R cat z
echo "###### P4: owner chmod own 0000 file (root here, privileged)"
touch m; chmod 0000 m; R chmod 0644 m; stat -c 'm mode=%a' m
echo "###### P5: client uid 65534 creates file in 0777 dir, then chmods own file"
chmod 777 /mnt/p; $NB sh -c 'umask 022; echo x > /mnt/p/own; chmod 0600 /mnt/p/own' 2>&1; ls -ln /mnt/p/own
echo "###### P6: SETATTR size (truncate) on 0444 file by owner-root"
echo abcdef > tr; chmod 0444 tr; R truncate -s 2 tr; stat -c 'tr size=%s' tr
echo "###### P7: ACCESS(X_OK) of dir vs file for nobody"
mkdir -m 0711 pub; R $NB test -x /mnt/p/pub && echo dir_x=yes
cd /; umount /mnt/p
