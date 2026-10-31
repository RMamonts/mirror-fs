use std::os::unix::fs::MetadataExt;

use tokio::fs;

use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::{self, mk_dir};

use super::MirrorFS;

impl mk_dir::MkDir for MirrorFS {
    async fn mk_dir(
        &self,
        args: mk_dir::Args,
        cred: &Credential,
    ) -> Result<mk_dir::Success, mk_dir::Fail> {
        if let Err(error) = Self::ensure_name_allowed(&args.object.name) {
            return Err(mk_dir::Fail {
                error,
                dir_wcc: vfs::WccData { before: None, after: None },
            });
        }
        if let Err(error) = self.require_dir_modify(cred, &args.object.dir).await {
            return Err(mk_dir::Fail {
                error,
                dir_wcc: vfs::WccData { before: None, after: None },
            });
        }
        let dir_path = match self.path_for_handle(&args.object.dir).await {
            Ok(path) => path,
            Err(error) => {
                return Err(mk_dir::Fail {
                    error,
                    dir_wcc: vfs::WccData { before: None, after: None },
                });
            }
        };
        let before = std::fs::symlink_metadata(&dir_path)
            .ok()
            .map(|meta| Self::wcc_attr_from_metadata(&meta));
        let mut child_path = dir_path.clone();
        child_path.push(args.object.name.as_str());
        if let Err(error) = fs::create_dir(&child_path).await {
            return Err(mk_dir::Fail {
                error: Self::io_error_to_vfs(&error),
                dir_wcc: Self::wcc_data(&dir_path, before),
            });
        }
        let caller = self.credentials(cred);
        if let Err(error) = Self::chown_to_caller(&child_path, &dir_path, &caller) {
            return Err(mk_dir::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) });
        }
        // A directory created inside a set-group-ID directory inherits the
        // set-group-ID bit (Linux `inode_init_owner()`); keep it when applying
        // the requested mode, which would otherwise clear it.
        let mut new_attr = args.attr;
        if let (Some(mode), Ok(parent)) = (new_attr.mode, Self::metadata(&dir_path)) {
            if parent.mode() & 0o2000 != 0 {
                new_attr.mode = Some(mode | 0o2000);
            }
        }
        if let Err(error) = Self::apply_set_attr(&child_path, &new_attr) {
            return Err(mk_dir::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) });
        }
        let attr = match Self::metadata(&child_path) {
            Ok(meta) => Self::attr_from_metadata(&meta),
            Err(error) => {
                return Err(mk_dir::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) })
            }
        };
        let handle = match self.handle_for_path(&child_path).await {
            Ok(handle) => handle,
            Err(error) => {
                return Err(mk_dir::Fail { error, dir_wcc: Self::wcc_data(&dir_path, before) })
            }
        };

        Ok(mk_dir::Success {
            file: Some(handle),
            attr: Some(attr),
            wcc_data: Self::wcc_data(&dir_path, before),
        })
    }
}
