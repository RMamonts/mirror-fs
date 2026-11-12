use std::io::ErrorKind;
use std::sync::Arc;

use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;

use nfs_mamont::vfs::file::Path as VfsPath;
use nfs_mamont::{handle_forever, ServerContext};

use nfs_mamont_util::{MountService, NlmService, PoolAllocator};

pub mod args;
pub mod bench_mode;
pub mod config;
pub mod fs;
pub mod fs_map;

pub mod auth;
#[cfg(test)]
mod tests;

fn main() -> std::io::Result<()> {
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

    let net_threads = bench_mode::env_usize("MIRRORFS_NET_THREADS")
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    let net_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(net_threads)
        .thread_name("nfs-net")
        .enable_all()
        .build()?;
    let vfs_rt = match std::env::var("MIRRORFS_RT").as_deref() {
        Ok("two") => Some(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(
                    bench_mode::env_usize("MIRRORFS_VFS_THREADS")
                        .unwrap_or(config.vfs_pool_size.get()),
                )
                .thread_name("mirrorfs-vfs")
                .enable_all()
                .build()?,
        ),
        Ok("one") | Err(_) => None,
        Ok(other) => panic!("unknown MIRRORFS_RT={other}"),
    };
    info!(
        runtimes = if vfs_rt.is_some() { 2 } else { 1 },
        net_threads,
        io = ?bench_mode::get().io,
        stat_delay = ?bench_mode::get().stat_delay,
        "runtime layout"
    );

    // `ServerContext::new` spawns the VFS workers into the runtime it is called in.
    let context = {
        let _guard = match &vfs_rt {
            Some(rt) => rt.enter(),
            None => net_rt.enter(),
        };
        ServerContext::new(
            Arc::new(PoolAllocator::new(
                config.allocator.buffer_size,
                config.allocator.buffer_count,
            )),
            config.vfs_pool_size,
        )
    };

    let result = net_rt.block_on(serve(args, config, fs, context));
    if let Some(rt) = vfs_rt {
        rt.shutdown_background();
    }
    result
}

async fn serve<A, B>(
    args: args::Args,
    config: config::Config,
    fs: Arc<fs::MirrorFS>,
    context: ServerContext<A, fs::MirrorFS, B>,
) -> std::io::Result<()>
where
    A: nfs_mamont::Allocator<Buffer = B> + Send + Sync + 'static,
    B: nfs_mamont::Buffer + 'static,
    fs::MirrorFS: nfs_mamont::vfs::Vfs<B>,
{
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
