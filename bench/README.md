# Runtime layout benchmark

Compares running the nfs-mamont network side and the mirror-fs VFS workers in one
tokio runtime versus two, together with three ways of doing the file system I/O.
Benchmark only: the knobs are environment variables read by `src/bench_mode.rs`;
with none of them set, the server behaves as before.

| Variant        | `MIRRORFS_RT` | `MIRRORFS_IO` | Meaning |
|----------------|---------------|---------------|---------|
| `one:inline`   | `one`         | `inline`      | Current code: one runtime, `std::fs` inside futures, `tokio::fs` for READ |
| `one:blocking` | `one`         | `blocking`    | One runtime, the whole syscall part of GETATTR/READ in one `spawn_blocking`, `pread` into the buffer |
| `one:sync`     | `one`         | `sync`        | One runtime, the same syscalls directly on the worker thread |
| `two:inline`   | `two`         | `inline`      | VFS workers in a second runtime (`vfs_pool_size` threads), current I/O code |
| `two:sync`     | `two`         | `sync`        | Second runtime plus direct syscalls |
| `two:sync:y`   | `two`         | `sync`        | As above, plus `yield_now()` after each procedure (`MIRRORFS_YIELD=1`) |

`MIRRORFS_STAT_DELAY_US` emulates slow storage by sleeping the calling thread
before each metadata lookup.

## Running

```sh
cargo build --release
(cd bench/nfs-load && cargo build --release)
mkdir -p /tmp/nfs-bench/fs01
dd if=/dev/urandom of=/tmp/nfs-bench/fs01/data bs=1M count=512
bench/run.sh                      # writes results.jsonl
bench/summarize.py results.jsonl  # medians over repetitions
```

`nfs-load` is a closed-loop client that speaks ONC RPC directly (MOUNT, LOOKUP,
GETATTR, READ, NULL), so no kernel NFS client is needed. The server is pinned to
`SERVER_CPUS` (default `0,1`) and the client to `CLIENT_CPUS` (default `2,3`).

## Scenarios

- `meta-fast`: 32 connections doing GETATTR, metadata cached in RAM.
- `meta-lat`: 1 connection doing GETATTR (cost of one request).
- `read-fast`: 16 connections doing random 64 KiB READs of a page-cached 512 MiB file.
- `read-lat`: 1 connection doing READ.
- `read-fast-probe`: `read-fast`, plus a probe connection sending NULL every 1 ms.
- `meta-slow`: every stat costs 1 ms; 32 GETATTR connections, plus probes sending NULL
  and GETATTR every 1 ms.

## Results (4 vCPU VM, server on 2 cores, `vfs_pool_size = 10`, medians of 3 × 8 s)

| Scenario / metric                    | one:inline | one:blocking | one:sync | two:inline | two:sync |
|--------------------------------------|-----------:|-------------:|---------:|-----------:|---------:|
| meta-fast, GETATTR ops/s             | 148 558 | 56 489 | 150 544 | 108 708 | 110 794 |
| meta-lat, GETATTR p50 µs             | 49 | 79 | 51 | 86 | 81 |
| read-fast, MiB/s                     | 1 002 | 1 853 | 2 636 | 1 149 | 2 294 |
| read-lat, READ p50 µs                | 169 | 111 | 80 | 212 | 122 |
| read-fast-probe, NULL p50 / p99 µs   | 474 / 1302 | 357 / 1075 | 359 / 903 | 154 / 1070 | 155 / 658 |
| meta-slow, GETATTR ops/s             | 1 671 | 7 956 | 1 680 | 8 601 | 8 529 |
| meta-slow, NULL probe p50 / p99 µs   | 18 223 / 30 159 | 116 / 530 | 17 839 / 29 535 | 102 / 457 | 102 / 522 |
| meta-slow, GETATTR probe p50 µs      | 19 583 | 3 995 | 19 343 | 3 541 | 3 529 |
| meta-slow, heavy max µs              | 45 919 | 8 927 | 45 951 | 152 191 | 147 967 |

`yield.jsonl` (a separate session) compares `two:sync` and `two:sync:y`. Without the
yield, the worst latency in `meta-slow` was 147–296 ms in 7 of 9 runs, always a
multiple of about 148 ms ≈ 128 × 1.15 ms, which is the size of tokio's cooperative
budget. With the yield it stayed at or below 16 ms. The yield costs about 19% of
`read-fast` throughput. Between sessions the throughput varies by about 10%.
