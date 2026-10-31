# pjdfstest in CI

`.github/workflows/pjdfstest.yml` builds mirror-fs and pjdfstest, mounts a
mirror-fs export over loopback NFSv3 on the runner and runs pjdfstest on it.

- `run-pjdfstest.sh` — starts mirror-fs (`--no-root-squash`), mounts the export,
  runs `prove -rv` and compares the result. Must run as root.
- `pjdfstest_compare.py` — compares the `prove -v` log with the known failures.
  It fails on any failure that is not listed and on test files that did not
  run all of their planned subtests; known failures that pass are reported.
- `pjdfstest-known-failures.txt` — the subtests expected to fail, grouped by
  cause, recorded against the pjdfstest revision pinned in the workflow.

## Running locally

On a Linux host with an NFS client (`nfs-common`):

```sh
cargo build --release
git clone https://github.com/pjd/pjdfstest
(cd pjdfstest && autoreconf -ifs && ./configure && make pjdfstest)
sudo testing/ci/run-pjdfstest.sh target/release/mirror-fs pjdfstest
```

`PJD_TESTS="chown chmod/00.t"` limits the run to a subset; logs go to
`pjdfstest-results/`.

## Updating the known failures

When a change fixes or intentionally changes behaviour, regenerate the list
from a full run and keep the grouping comments:

```sh
python3 testing/ci/pjdfstest_compare.py pjdfstest-results/pjdfstest.log \
    testing/ci/pjdfstest-known-failures.txt --emit-known
```
