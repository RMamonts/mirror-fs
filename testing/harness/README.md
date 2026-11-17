# Test harness (NFSv3 client in a VM)

The session container has no in-kernel NFS client and no KVM, so the NFSv3
client runs inside a QEMU/TCG guest booting an Ubuntu HWE 7.0 kernel, with the
host root shared over virtio-9p and host<->guest networking over a tap device.

- `start-vm2.sh <name> <tap> <guest-ip/cidr> [smp] [mem]` — boot a client VM.
  Jobs are shell scripts dropped into `/opt/vm/<name>-work/queue/`; stdout/rc
  land in `/opt/vm/<name>-work/out/`.
- `guest-init2.sh` — guest PID 1: mounts 9p shares, brings up networking,
  runs queued jobs serially.
- `setup-xfs-shards.sh N...` — create tap + a mirror-fs instance (port 205N) +
  work dir per shard, to run xfstests in parallel.
- `xfs-runner2.sh` — run a list of xfstests in one `./check` with a watchdog.
- `xfs-runner.sh` — per-test, resumable (status file), slower startup.
- `baseline-knfsd-pjd.sh` — real Linux knfsd NFSv3 baseline (ext4 over loop).
- `probe-conformance.sh`, `probe-permissions.sh` — isolated semantic probes.
- `config-example.toml` — mirror-fs export config used on the host.

mirror-fs on the host:
    mirror-fs -c <config.toml> -a 0.0.0.0:<port> --no-root-squash
