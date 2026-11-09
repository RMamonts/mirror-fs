use std::io::ErrorKind;
use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;

use nfs_mamont::vfs::file::Path as VfsPath;
use nfs_mamont::{handle_forever, ServerContext};

use nfs_mamont_util::{MountService, NlmService, PoolAllocator};

pub mod args;
pub mod config;
pub mod fs;
pub mod fs_map;

pub mod auth;
#[cfg(test)]
mod tests;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("nfs_mamont=debug")),
        )
        .init();

    let args = args::Args::parse();

    let config = config::load_config(&args.config_path)?;
    let root_squash = !args.no_root_squash;
    let fs = Arc::new(fs::MirrorFS::new(config.export_root.clone(), root_squash));

    let context = ServerContext::new(
        Arc::new(PoolAllocator::new(config.allocator.buffer_size, config.allocator.buffer_count)),
        config.vfs_pool_size,
    );
    let backend_id =
        context.add_backend(fs.clone()).expect("no free backend slot for the mirror file system");
    fs.set_backend_id(backend_id).await;

    info!(export_root = %config.export_root.display(), bind = %args.addr, "mirrorfs startup");

    let listener = TcpListener::bind(&args.addr).await?;

    let mount_service = Arc::new(MountService::new());

    for export in &config.exports {
        let root_handle = fs.handle_for_path(&export.local_path).await.map_err(|error| {
            std::io::Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "failed to resolve export handle for {}: {error:?}",
                    export.local_path.display()
                ),
            )
        })?;

        mount_service.add_export(VfsPath::new(export.mount_path.clone())?, root_handle).await;
    }

    let nlm_service = Arc::new(NlmService::new());
    handle_forever(listener, context, mount_service, nlm_service).await
}
