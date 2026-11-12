use nfs_mamont::auth::Credential;
use nfs_mamont::vfs;
use nfs_mamont::vfs::get_attr;

use super::MirrorFS;
use crate::bench_mode::{self, IoMode};

impl get_attr::GetAttr for MirrorFS {
    async fn get_attr(
        &self,
        args: get_attr::Args,
        _cred: &Credential,
    ) -> Result<get_attr::Success, get_attr::Fail> {
        let path = match self.path_for_handle(&args.file).await {
            Ok(path) => path,
            Err(error) => {
                return Err(get_attr::Fail { error });
            }
        };
        let meta = match bench_mode::get().io {
            IoMode::Inline => Self::metadata(&path),
            IoMode::Sync => {
                let meta = Self::metadata(&path);
                bench_mode::maybe_yield().await;
                meta
            }
            IoMode::Blocking => tokio::task::spawn_blocking(move || Self::metadata(&path))
                .await
                .unwrap_or(Err(vfs::Error::IO)),
        };
        match meta {
            Ok(meta) => Ok(get_attr::Success { object: Self::attr_from_metadata(&meta) }),
            Err(error) => Err(get_attr::Fail { error }),
        }
    }
}
