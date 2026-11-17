use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::file;
use nfs_mamont::vfs::{self, mk_node, set_attr};

use super::MirrorFS;

impl mk_node::MkNode for MirrorFS {
    async fn mk_node(
        &self,
        args: mk_node::Args,
        cred: &Credential,
    ) -> Result<mk_node::Success, mk_node::Fail> {
        let no_wcc = || vfs::WccData { before: None, after: None };

        if let Err(error) = Self::ensure_name_allowed(&args.object.name) {
            return Err(mk_node::Fail { error, dir_wcc: no_wcc() });
        }
        if let Err(error) = self.require_dir_modify(cred, &args.object.dir).await {
            return Err(mk_node::Fail { error, dir_wcc: no_wcc() });
        }

        let dir_path = match self.path_for_handle(&args.object.dir).await {
            Ok(path) => path,
            Err(error) => return Err(mk_node::Fail { error, dir_wcc: no_wcc() }),
        };
        let before = std::fs::symlink_metadata(&dir_path)
            .ok()
            .map(|meta| Self::wcc_attr_from_metadata(&meta));

        let mut child_path = dir_path.clone();
        child_path.push(args.object.name.as_str());

        // Pick the raw file-type bits and device for the requested node, and the
        // initial attributes (mode/times) carried in the request.
        let (type_bits, device, new_attr): (libc::mode_t, u64, &set_attr::NewAttr) =
            match &args.what {
                mk_node::What::Fifo(attr) => (libc::S_IFIFO, 0, attr),
                mk_node::What::Socket(attr) => (libc::S_IFSOCK, 0, attr),
                mk_node::What::Char(attr, dev) => (libc::S_IFCHR, makedev(dev), attr),
                mk_node::What::Block(attr, dev) => (libc::S_IFBLK, makedev(dev), attr),
            };

        if let Err(error) = mknod(&child_path, type_bits, device) {
            return Err(mk_node::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) });
        }

        // Reflect the caller's identity, then stamp the requested mode/times.
        let caller = self.credentials(cred);
        if let Err(error) = Self::chown_to_caller(&child_path, &dir_path, &caller) {
            return Err(mk_node::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) });
        }
        if let Err(error) = Self::apply_set_attr(&child_path, new_attr) {
            return Err(mk_node::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) });
        }

        let attr = match Self::metadata(&child_path) {
            Ok(meta) => Self::attr_from_metadata(&meta),
            Err(error) => {
                return Err(mk_node::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) })
            }
        };
        let handle = match self.handle_for_path(&child_path).await {
            Ok(handle) => handle,
            Err(error) => {
                return Err(mk_node::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) })
            }
        };

        Ok(mk_node::Success {
            file: Some(handle),
            attr: Some(attr),
            wcc_data: Self::wcc_data(&dir_path, before),
        })
    }
}

fn makedev(dev: &file::Device) -> u64 {
    libc::makedev(dev.major, dev.minor)
}

/// `mknod(2)` wrapper. The node is created with mode `0`; the caller applies the
/// requested permissions afterwards via `apply_set_attr`, so the server umask
/// cannot clobber them.
fn mknod(path: &Path, type_bits: libc::mode_t, device: u64) -> Result<(), vfs::Error> {
    let c_path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| vfs::Error::InvalidArgument)?;
    // SAFETY: `c_path` is a valid NUL-terminated string for the duration of the call.
    let rc = unsafe { libc::mknod(c_path.as_ptr(), type_bits, device as libc::dev_t) };
    if rc == 0 {
        Ok(())
    } else {
        Err(MirrorFS::io_error_to_vfs(&std::io::Error::last_os_error()))
    }
}
