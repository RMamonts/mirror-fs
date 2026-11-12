use std::io::SeekFrom;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use nfs_mamont::auth::Credential;
use nfs_mamont::vfs::read;
use nfs_mamont::Buffer;

use super::MirrorFS;
use crate::auth;
use crate::bench_mode::{self, IoMode};

impl<B: Buffer + 'static> read::Read<B> for MirrorFS {
    async fn read(
        &self,
        args: read::Args,
        mut data: B,
        cred: &Credential,
    ) -> Result<read::Success<B>, read::Fail> {
        let io = bench_mode::get().io;
        if io != IoMode::Inline {
            let path = match self.path_for_handle(&args.file).await {
                Ok(path) => path,
                Err(error) => return Err(read::Fail { error, file_attr: None }),
            };
            let authorizer = Arc::clone(&self.authorizer);
            let creds = self.credentials(cred);
            return match io {
                IoMode::Blocking => tokio::task::spawn_blocking(move || {
                    Self::read_direct(&path, &args, data, authorizer.as_ref(), &creds)
                })
                .await
                .unwrap_or(Err(read::Fail { error: nfs_mamont::vfs::Error::IO, file_attr: None })),
                _ => {
                    let result = Self::read_direct(&path, &args, data, authorizer.as_ref(), &creds);
                    bench_mode::maybe_yield().await;
                    result
                }
            };
        }

        if let Err(error) = self.require_read(cred, &args.file).await {
            return Err(read::Fail { error, file_attr: None });
        }
        let path = match self.path_for_handle(&args.file).await {
            Ok(path) => path,
            Err(error) => {
                return Err(read::Fail { error, file_attr: None });
            }
        };
        let meta = match Self::metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                return Err(read::Fail { error, file_attr: None });
            }
        };
        let attr = Self::attr_from_metadata(&meta);
        if let Err(error) = Self::validate_regular(&attr) {
            return Err(read::Fail { error, file_attr: Some(attr) });
        }

        let mut file = match File::open(&path).await {
            Ok(file) => file,
            Err(error) => {
                return Err(read::Fail {
                    error: Self::io_error_to_vfs(&error),
                    file_attr: Some(attr),
                });
            }
        };

        let file_len = meta.len();
        let start = args.offset.min(file_len);
        let end = args.offset.saturating_add(args.count as u64).min(file_len);
        let requested = end.saturating_sub(start) as usize;
        let mut remaining = requested;
        let mut read_count = 0usize;
        if let Err(error) = file.seek(SeekFrom::Start(start)).await {
            return Err(read::Fail { error: Self::io_error_to_vfs(&error), file_attr: Some(attr) });
        }

        if remaining > 0 {
            for chunk in data.chunks_mut() {
                if remaining == 0 {
                    break;
                }

                let to_read = chunk.len().min(remaining);
                let mut chunk_offset = 0usize;

                while chunk_offset < to_read {
                    match file.read(&mut chunk[chunk_offset..to_read]).await {
                        Ok(0) => {
                            remaining = 0;
                            break;
                        }
                        Ok(bytes) => {
                            chunk_offset += bytes;
                            read_count += bytes;
                        }
                        Err(error) => {
                            return Err(read::Fail {
                                error: Self::io_error_to_vfs(&error),
                                file_attr: Some(attr),
                            });
                        }
                    }
                }

                if chunk_offset < to_read {
                    break;
                }

                remaining -= to_read;
            }
        }

        Ok(read::Success {
            head: read::SuccessHeader {
                file_attr: Some(attr),
                count: read_count as u32,
                eof: start.saturating_add(read_count as u64) >= file_len,
            },
            data,
        })
    }
}

impl MirrorFS {
    /// READ done with plain syscalls: the same checks as the async path (two
    /// metadata lookups, one for the access check and one for the attributes), then
    /// `pread` straight into the chunks of `data`.
    fn read_direct<B: Buffer>(
        path: &PathBuf,
        args: &read::Args,
        mut data: B,
        authorizer: &dyn auth::Authorizer,
        creds: &auth::Credentials,
    ) -> Result<read::Success<B>, read::Fail> {
        let access_meta =
            Self::metadata(path).map_err(|error| read::Fail { error, file_attr: None })?;
        if !authorizer.allows(creds, &Self::attr_from_metadata(&access_meta), auth::Access::Read) {
            return Err(read::Fail { error: nfs_mamont::vfs::Error::Access, file_attr: None });
        }
        let meta = Self::metadata(path).map_err(|error| read::Fail { error, file_attr: None })?;
        let attr = Self::attr_from_metadata(&meta);
        if let Err(error) = Self::validate_regular(&attr) {
            return Err(read::Fail { error, file_attr: Some(attr) });
        }

        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) => {
                return Err(read::Fail {
                    error: Self::io_error_to_vfs(&error),
                    file_attr: Some(attr),
                })
            }
        };

        let file_len = meta.len();
        let start = args.offset.min(file_len);
        let end = args.offset.saturating_add(args.count as u64).min(file_len);
        let mut remaining = end.saturating_sub(start) as usize;
        let mut offset = start;
        let mut read_count = 0usize;

        'chunks: for chunk in data.chunks_mut() {
            if remaining == 0 {
                break;
            }
            let to_read = chunk.len().min(remaining);
            let mut filled = 0usize;
            while filled < to_read {
                match file.read_at(&mut chunk[filled..to_read], offset) {
                    Ok(0) => break 'chunks,
                    Ok(bytes) => {
                        filled += bytes;
                        offset += bytes as u64;
                        read_count += bytes;
                    }
                    Err(error) => {
                        return Err(read::Fail {
                            error: Self::io_error_to_vfs(&error),
                            file_attr: Some(attr),
                        });
                    }
                }
            }
            remaining -= to_read;
        }

        Ok(read::Success {
            head: read::SuccessHeader {
                file_attr: Some(attr),
                count: read_count as u32,
                eof: start.saturating_add(read_count as u64) >= file_len,
            },
            data,
        })
    }
}
