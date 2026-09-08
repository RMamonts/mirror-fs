use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::access;

use crate::auth::Authorization;
use super::MirrorFS;

impl access::Access for MirrorFS {
    async fn access(&self, cred: Credential, args: access::Args) -> Result<access::Success, access::Fail> {
        let path = match self.path_for_handle(&args.file).await {
            Ok(path) => path,
            Err(error) => return Err(access::Fail { error, object_attr: None }),
        };
        let meta = match Self::metadata(&path) {
            Ok(meta) => meta,
            Err(error) => return Err(access::Fail { error, object_attr: None }),
        };
        let attr = Self::attr_from_metadata(&meta);
        let granted = Authorization::from_credential(cred).grant(&attr, args.mask);
        Ok(access::Success { object_attr: Some(attr), access: granted })
    }
}