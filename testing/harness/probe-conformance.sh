MO="vers=3,proto=tcp,port=2060,mountport=2060,mountproto=tcp,nolock"
mkdir -p /mnt/p; mount -t nfs -o $MO 10.10.15.1:/a /mnt/p || { echo MOUNT_FAIL; exit 1; }
cd /mnt/p; rm -rf ./* 2>/dev/null
R() { echo "--- $*"; "$@"; echo "   rc=$?"; }
echo "###### B1: FSSTAT (df) reports zero size"
R df -P -B1 /mnt/p
echo "###### B2: READDIR omits . and .. (POSIX requires them)"
mkdir d; R ls -a1 /mnt/p/d
R python3 -c "import os; print(sorted(os.listdir('/mnt/p/d')) or 'EMPTY'); print('dot entries:', [e for e in os.scandir('/mnt/p/d')])"
echo "###### B3: chown to SAME owner (no-op) should succeed but returns EINVAL"
touch f_chown; R chown 0:0 f_chown; R chown 65534:65534 f_chown; R chown 65534 f_chown; ls -ln f_chown
echo "###### B4: mknod / mkfifo / bind socket unsupported"
R mkfifo fifo1
R mknod blk1 b 1 2
R mknod chr1 c 1 2
R python3 -c "import socket;s=socket.socket(socket.AF_UNIX);s.bind('/mnt/p/sock1')"
echo "###### B5: SETATTR size on a directory -> should be EISDIR, mirror opens O_WRONLY"
R python3 -c "import os; os.truncate('/mnt/p/d', 0)"
echo "###### B6: owner can always chmod? non-privileged create then chmod"
install -m 700 -d od; cd od
R sh -c 'echo hi > g; chmod 0000 g; chmod 0644 g; echo ok'
cd /mnt/p
echo "###### B7: timestamps of freshly created file vs server clock"
date +'server_now=%s'; touch ts; stat -c 'ts atime=%X mtime=%Y ctime=%Z' ts
echo "###### B8: utimes with UTIME_NOW/OMIT and >u32 and negative"
R touch -d @-5 ts; stat -c 'neg mtime=%Y' ts
R python3 -c "import os; os.utime('/mnt/p/ts',(5000000000,5000000000)); print(os.stat('/mnt/p/ts').st_mtime)"
echo "###### B9: SETATTR mode bits above 0777 (suid/sgid/sticky) preserved?"
touch sb; R chmod 7755 sb; stat -c 'sb mode=%a' sb
echo "###### B10: hardlink count + nlink after link/unlink"
touch h; ln h h2; stat -c 'nlink=%h' h; rm h2; stat -c 'nlink_after=%h' h
echo "###### B11: rename dir onto empty dir / nonempty dir"
mkdir s1 t1; R mv -T s1 t1; mkdir s2 t2; touch t2/x; R mv -T s2 t2
echo "###### B12: large READDIR pagination correctness (cookie)"
mkdir big; for i in $(seq 1 500); do : > big/f$i; done; echo "created=500 listed=$(ls -1 big | wc -l) uniq=$(ls -1 big | sort -u | wc -l)"
echo "###### B13: WRITE then READ integrity w/ O_APPEND and holes"
R python3 -c "
import os
fd=os.open('/mnt/p/sp',os.O_CREAT|os.O_RDWR,0o644)
os.pwrite(fd,b'END',1048576); os.fsync(fd)
st=os.fstat(fd); print('size',st.st_size,'blocks',st.st_blocks)
os.lseek(fd,0,os.SEEK_SET); print('hole0',os.read(fd,4))
os.close(fd)"
echo "###### B14: ACCESS semantics - X on file without x bit (nobody)"
echo '#!/bin/sh' > scr; echo 'echo RAN' > scr; chmod 0644 scr; R setpriv --reuid=65534 --regid=65534 --clear-groups test -x scr && echo "x=yes" || echo "x=no"
cd /; umount /mnt/p
