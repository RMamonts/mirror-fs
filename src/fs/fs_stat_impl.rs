use std::ffi::CString;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::fs_stat;

use super::MirrorFS;

impl fs_stat::FsStat for MirrorFS {
    async fn fs_stat(
        &self,
        args: fs_stat::Args,
        _cred: &Credential,
    ) -> Result<fs_stat::Success, fs_stat::Fail> {
        let path = match self.path_for_handle(&args.root).await {
            Ok(path) => path,
            Err(error) => return Err(fs_stat::Fail { error, root_attr: None }),
        };

        // Report the backing file system's real capacity so clients (and `df`)
        // see accurate free space instead of zeroes.
        let capacity = statvfs(&path).map(|s| Capacity::from_statvfs(&s)).unwrap_or_default();

        Ok(fs_stat::Success {
            root_attr: Self::file_attr(&path),
            total_bytes: capacity.total_bytes,
            free_bytes: capacity.free_bytes,
            available_bytes: capacity.available_bytes,
            total_files: capacity.total_files,
            free_files: capacity.free_files,
            available_files: capacity.available_files,
            invarsec: 0,
        })
    }
}

/// File system capacity in the units `FSSTAT` reports (bytes and file slots).
#[derive(Default)]
struct Capacity {
    total_bytes: u64,
    free_bytes: u64,
    available_bytes: u64,
    total_files: u64,
    free_files: u64,
    available_files: u64,
}

impl Capacity {
    // The `statvfs` field types (`c_ulong`, `fsblkcnt_t`, `fsfilcnt_t`) are
    // 64-bit on x86_64 Linux but narrower on some 32-bit targets, so the casts
    // are only redundant on some platforms.
    #[allow(clippy::unnecessary_cast)]
    fn from_statvfs(s: &libc::statvfs) -> Self {
        let unit = if s.f_frsize != 0 { s.f_frsize as u64 } else { s.f_bsize as u64 };
        Self {
            total_bytes: (s.f_blocks as u64).saturating_mul(unit),
            free_bytes: (s.f_bfree as u64).saturating_mul(unit),
            available_bytes: (s.f_bavail as u64).saturating_mul(unit),
            total_files: s.f_files as u64,
            free_files: s.f_ffree as u64,
            available_files: s.f_favail as u64,
        }
    }
}

fn statvfs(path: &Path) -> Option<libc::statvfs> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated path; `buf` is writable and
    // only read after a successful (zero) return.
    if unsafe { libc::statvfs(c_path.as_ptr(), buf.as_mut_ptr()) } == 0 {
        Some(unsafe { buf.assume_init() })
    } else {
        None
    }
}
