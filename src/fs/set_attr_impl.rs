use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::file;
use nfs_mamont::vfs::{self, set_attr};

use crate::auth::{self, Access};

use super::MirrorFS;

impl set_attr::SetAttr for MirrorFS {
    async fn set_attr(
        &self,
        args: set_attr::Args,
        cred: &Credential,
    ) -> Result<set_attr::Success, set_attr::Fail> {
        let no_wcc = || vfs::WccData { before: None, after: None };

        let path = match self.path_for_handle(&args.file).await {
            Ok(path) => path,
            Err(error) => return Err(set_attr::Fail { error, wcc_data: no_wcc() }),
        };
        let meta = match Self::metadata(&path) {
            Ok(meta) => meta,
            Err(error) => return Err(set_attr::Fail { error, wcc_data: no_wcc() }),
        };
        let before = Some(Self::wcc_attr_from_metadata(&meta));
        let current_attr = Self::attr_from_metadata(&meta);

        if let Some(guard) = args.guard {
            if !Self::same_time(current_attr.ctime, guard.ctime) {
                return Err(set_attr::Fail {
                    error: vfs::Error::NotSync,
                    wcc_data: vfs::WccData { before, after: Some(current_attr) },
                });
            }
        }

        // Authorize each requested change under POSIX rules. Unlike data
        // writes, changing metadata is gated by *ownership*, not the write bit.
        let caller = self.credentials(cred);
        if let Err(error) =
            Self::authorize_set_attr(&self.authorizer, &caller, &current_attr, &args.new_attr)
        {
            return Err(set_attr::Fail {
                error,
                wcc_data: vfs::WccData { before, after: Some(current_attr) },
            });
        }

        let mut new_attr = args.new_attr;
        Self::drop_setgid_for_non_member(&caller, &current_attr, &mut new_attr);

        if let Err(error) = Self::apply_set_attr(&path, &new_attr) {
            return Err(set_attr::Fail { error, wcc_data: Self::wcc_data(&path, before) });
        }

        Ok(set_attr::Success { wcc_data: Self::wcc_data(&path, before) })
    }
}

impl MirrorFS {
    /// POSIX authorization for the individual fields of a `SETATTR` request.
    ///
    /// * `mode` (chmod): caller must own the object or be privileged.
    /// * `uid`/`gid` (chown): changing the owner requires privilege; changing
    ///   the group requires ownership and membership in the target group (or
    ///   privilege).
    /// * `size` (truncate): requires the write permission or ownership (the
    ///   owner override knfsd applies, since `ftruncate()` on an already open
    ///   file arrives as the same `SETATTR`), and the object must be a regular
    ///   file (`EISDIR` for a directory).
    /// * times to an explicit value: owner or privileged; times to "now"
    ///   (`UTIME_NOW`): write permission (or ownership / privilege).
    fn authorize_set_attr(
        authorizer: &std::sync::Arc<dyn auth::Authorizer>,
        caller: &auth::Credentials,
        attr: &file::Attr,
        new_attr: &set_attr::NewAttr,
    ) -> Result<(), vfs::Error> {
        let owner_or_priv = caller.privileged || caller.uid == attr.uid;

        if new_attr.mode.is_some() && !owner_or_priv {
            return Err(vfs::Error::Permission);
        }

        // Mirrors Linux `chown_ok()`/`chgrp_ok()`: specifying an owner or group
        // at all (even the current one) is reserved to the owner or a
        // privileged caller. The owner may only "set" the uid to its current
        // value and the gid to the current one or a group it belongs to.
        let is_owner = caller.uid == attr.uid;
        if let Some(want_uid) = new_attr.uid {
            let allowed = caller.privileged || (is_owner && want_uid == attr.uid);
            if !allowed {
                return Err(vfs::Error::Permission);
            }
        }
        if let Some(want_gid) = new_attr.gid {
            let member =
                want_gid == attr.gid || caller.gid == want_gid || caller.groups.contains(&want_gid);
            let allowed = caller.privileged || (is_owner && member);
            if !allowed {
                return Err(vfs::Error::Permission);
            }
        }

        if new_attr.size.is_some() {
            match attr.file_type {
                file::Type::Regular => {}
                file::Type::Directory => return Err(vfs::Error::IsDir),
                _ => return Err(vfs::Error::InvalidArgument),
            }
            if !is_owner && !authorizer.allows(caller, attr, Access::Modify) {
                return Err(vfs::Error::Access);
            }
        }

        let times_specific = matches!(new_attr.atime, set_attr::SetTime::ToClient(_))
            || matches!(new_attr.mtime, set_attr::SetTime::ToClient(_));
        let times_now = matches!(new_attr.atime, set_attr::SetTime::ToServer)
            || matches!(new_attr.mtime, set_attr::SetTime::ToServer);
        if times_specific && !owner_or_priv {
            return Err(vfs::Error::Permission);
        }
        if times_now
            && !times_specific
            && !owner_or_priv
            && !authorizer.allows(caller, attr, Access::Modify)
        {
            return Err(vfs::Error::Access);
        }

        Ok(())
    }

    /// Linux `setattr_copy()`: a mode change by a caller that is neither
    /// privileged nor a member of the file's group (after any group change in
    /// the same request) silently drops the set-group-ID bit.
    fn drop_setgid_for_non_member(
        caller: &auth::Credentials,
        attr: &file::Attr,
        new_attr: &mut set_attr::NewAttr,
    ) {
        if let Some(mode) = new_attr.mode {
            let gid = new_attr.gid.unwrap_or(attr.gid);
            let member = caller.gid == gid || caller.groups.contains(&gid);
            if !caller.privileged && !member {
                new_attr.mode = Some(mode & !0o2000);
            }
        }
    }
}
