//! Benchmark-only knobs, read once from the environment.
//!
//! They exist to compare runtime layouts and I/O strategies of the server; with
//! none of the variables set the server behaves exactly as before.
//!
//! - `MIRRORFS_RT` --- `one` (default): network and VFS tasks share one runtime;
//!   `two`: VFS workers get a runtime of their own.
//! - `MIRRORFS_NET_THREADS` --- worker threads of the network runtime (default: the
//!   available parallelism).
//! - `MIRRORFS_VFS_THREADS` --- worker threads of the VFS runtime (default:
//!   `vfs_pool_size`).
//! - `MIRRORFS_IO` --- how GETATTR and READ touch the file system: `inline`
//!   (default, the original code: `std::fs` inside the future plus `tokio::fs`),
//!   `blocking` (the whole syscall part of the procedure in one `spawn_blocking`,
//!   `pread` straight into the buffer) or `sync` (the same code as `blocking`, run
//!   directly on the worker thread).
//! - `MIRRORFS_YIELD` --- `1`: in the `blocking` and `sync` modes GETATTR and READ
//!   yield to the scheduler once before returning.
//! - `MIRRORFS_STAT_DELAY_US` --- emulates slow storage: every metadata lookup
//!   sleeps the calling thread this long, as a stat on a cold disk would.

use std::sync::OnceLock;
use std::time::Duration;

/// How GETATTR and READ perform their syscalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoMode {
    Inline,
    Blocking,
    Sync,
}

/// Benchmark settings of the process.
#[derive(Debug)]
pub struct BenchMode {
    pub io: IoMode,
    pub stat_delay: Option<Duration>,
    pub yield_after_op: bool,
}

/// Returns the settings, reading the environment on first use.
pub fn get() -> &'static BenchMode {
    static MODE: OnceLock<BenchMode> = OnceLock::new();
    MODE.get_or_init(|| {
        let io = match std::env::var("MIRRORFS_IO").as_deref() {
            Ok("blocking") => IoMode::Blocking,
            Ok("sync") => IoMode::Sync,
            Ok("inline") | Err(_) => IoMode::Inline,
            Ok(other) => panic!("unknown MIRRORFS_IO={other}"),
        };
        let stat_delay = std::env::var("MIRRORFS_STAT_DELAY_US")
            .ok()
            .map(|value| value.parse::<u64>().expect("MIRRORFS_STAT_DELAY_US must be a number"))
            .filter(|&us| us > 0)
            .map(Duration::from_micros);
        let yield_after_op = std::env::var("MIRRORFS_YIELD").is_ok_and(|v| v == "1");
        BenchMode { io, stat_delay, yield_after_op }
    })
}

/// Blocks the calling thread for the emulated metadata latency, if any.
pub fn emulate_slow_stat() {
    if let Some(delay) = get().stat_delay {
        std::thread::sleep(delay);
    }
}

/// Reads a `usize` from the environment variable `name`.
pub fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .map(|value| value.parse().unwrap_or_else(|_| panic!("{name} must be a positive number")))
}

/// Yields once if `MIRRORFS_YIELD=1`, see the module docs.
pub async fn maybe_yield() {
    if get().yield_after_op {
        tokio::task::yield_now().await;
    }
}
