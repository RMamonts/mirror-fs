//! Closed-loop load generator speaking raw ONC RPC over TCP to an NFSv3 server.
//!
//! Every connection keeps exactly one request outstanding: it sends a call, waits
//! for the reply, records the latency and (optionally) sleeps `--think-us` before
//! the next call. Results are printed as one JSON line.

use std::io;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use hdrhistogram::Histogram;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const NFS_PROGRAM: u32 = 100003;
const MOUNT_PROGRAM: u32 = 100005;
const AUTH_SYS: u32 = 1;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Op {
    Null,
    Getattr,
    Read,
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:2049")]
    addr: String,
    #[arg(long, default_value = "/fs01")]
    export: String,
    /// File (relative to the export root) GETATTR and READ go to.
    #[arg(long, default_value = "data")]
    file: String,
    #[arg(long, value_enum)]
    op: Op,
    #[arg(long, default_value_t = 1)]
    conns: usize,
    /// Seconds of measured load.
    #[arg(long, default_value_t = 10.0)]
    duration: f64,
    /// Seconds of load before measuring starts.
    #[arg(long, default_value_t = 2.0)]
    warmup: f64,
    /// Bytes per READ.
    #[arg(long, default_value_t = 65536)]
    read_size: u32,
    /// Pause between two calls of one connection, in microseconds.
    #[arg(long, default_value_t = 0)]
    think_us: u64,
    /// Label copied into the output.
    #[arg(long, default_value = "")]
    label: String,
    /// Worker threads of the client runtime.
    #[arg(long, default_value_t = 2)]
    threads: usize,
}

/// XDR encoder.
#[derive(Default)]
struct Xdr(Vec<u8>);

impl Xdr {
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn opaque(&mut self, bytes: &[u8]) -> &mut Self {
        self.u32(bytes.len() as u32);
        self.0.extend_from_slice(bytes);
        let pad = (4 - bytes.len() % 4) % 4;
        self.0.extend(std::iter::repeat(0).take(pad));
        self
    }
}

/// XDR decoder.
struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn u32(&mut self) -> io::Result<u32> {
        if self.0.len() < 4 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short reply"));
        }
        let v = u32::from_be_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        Ok(v)
    }
    fn opaque(&mut self) -> io::Result<&'a [u8]> {
        let len = self.u32()? as usize;
        let padded = len + (4 - len % 4) % 4;
        if self.0.len() < padded {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short opaque"));
        }
        let v = &self.0[..len];
        self.0 = &self.0[padded..];
        Ok(v)
    }
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

struct Conn {
    stream: TcpStream,
    xid: u32,
    reply: Vec<u8>,
}

impl Conn {
    async fn connect(addr: &str, xid_base: u32) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(Self { stream, xid: xid_base, reply: Vec::with_capacity(1 << 20) })
    }

    /// Builds a record-marked call; the xid is patched in by [`Self::call`].
    fn request(prog: u32, vers: u32, proc_: u32, args: &[u8]) -> Vec<u8> {
        let mut cred = Xdr::default();
        cred.u32(0).opaque(b"nfs-load").u32(0).u32(0).u32(0);
        let mut x = Xdr::default();
        x.u32(0) // record mark, patched below
            .u32(0) // xid
            .u32(0) // CALL
            .u32(2) // RPC version
            .u32(prog)
            .u32(vers)
            .u32(proc_)
            .u32(AUTH_SYS)
            .opaque(&cred.0)
            .u32(0) // verifier: AUTH_NONE
            .u32(0);
        x.0.extend_from_slice(args);
        let len = (x.0.len() - 4) as u32;
        x.0[..4].copy_from_slice(&(0x8000_0000 | len).to_be_bytes());
        x.0
    }

    /// Sends `req` and returns the procedure results of the accepted reply.
    async fn call(&mut self, req: &mut [u8]) -> io::Result<&[u8]> {
        self.xid = self.xid.wrapping_add(1);
        req[4..8].copy_from_slice(&self.xid.to_be_bytes());
        self.stream.write_all(req).await?;

        self.reply.clear();
        loop {
            let mark = self.stream.read_u32().await?;
            let len = (mark & 0x7fff_ffff) as usize;
            let start = self.reply.len();
            self.reply.resize(start + len, 0);
            self.stream.read_exact(&mut self.reply[start..]).await?;
            if mark & 0x8000_0000 != 0 {
                break;
            }
        }

        let mut r = Rd(&self.reply);
        if r.u32()? != self.xid {
            return Err(bad("xid mismatch"));
        }
        if r.u32()? != 1 {
            return Err(bad("not a REPLY"));
        }
        if r.u32()? != 0 {
            return Err(bad("call denied"));
        }
        r.u32()?; // verifier flavor
        r.opaque()?; // verifier body
        let accept = r.u32()?;
        if accept != 0 {
            return Err(bad(format!("accept_stat {accept}")));
        }
        Ok(r.0)
    }
}

/// Mounts the export and looks up `file`, returning its file handle.
async fn resolve(args: &Args) -> io::Result<Vec<u8>> {
    let mut conn = Conn::connect(&args.addr, 0x1000_0000).await?;

    let mut mnt = Xdr::default();
    mnt.opaque(args.export.as_bytes());
    let mut req = Conn::request(MOUNT_PROGRAM, 3, 1, &mnt.0);
    let mut r = Rd(conn.call(&mut req).await?);
    let status = r.u32()?;
    if status != 0 {
        return Err(bad(format!("MNT failed: {status}")));
    }
    let mut handle = r.opaque()?.to_vec();

    for name in args.file.split('/').filter(|n| !n.is_empty()) {
        let mut lookup = Xdr::default();
        lookup.opaque(&handle).opaque(name.as_bytes());
        let mut req = Conn::request(NFS_PROGRAM, 3, 3, &lookup.0);
        let mut r = Rd(conn.call(&mut req).await?);
        let status = r.u32()?;
        if status != 0 {
            return Err(bad(format!("LOOKUP {name} failed: {status}")));
        }
        handle = r.opaque()?.to_vec();
    }
    Ok(handle)
}

/// Returns the file size from a GETATTR of `handle`.
async fn file_size(args: &Args, handle: &[u8]) -> io::Result<u64> {
    let mut conn = Conn::connect(&args.addr, 0x2000_0000).await?;
    let mut x = Xdr::default();
    x.opaque(handle);
    let mut req = Conn::request(NFS_PROGRAM, 3, 1, &x.0);
    let mut r = Rd(conn.call(&mut req).await?);
    if r.u32()? != 0 {
        return Err(bad("GETATTR failed"));
    }
    // fattr3: type, mode, nlink, uid, gid, then size (u64)
    for _ in 0..5 {
        r.u32()?;
    }
    let hi = r.u32()? as u64;
    let lo = r.u32()? as u64;
    Ok(hi << 32 | lo)
}

struct Stats {
    hist: Histogram<u64>,
    ops: u64,
    bytes: u64,
    errors: u64,
}

async fn worker(
    args: &Args,
    id: usize,
    handle: &[u8],
    size: u64,
    measure_from: Instant,
    until: Instant,
) -> io::Result<Stats> {
    let mut conn = Conn::connect(&args.addr, (id as u32) << 20).await?;
    let mut stats = Stats {
        hist: Histogram::new_with_bounds(1, 60_000_000, 3).unwrap(),
        ops: 0,
        bytes: 0,
        errors: 0,
    };
    let mut fh = Xdr::default();
    fh.opaque(handle);
    let mut req = match args.op {
        Op::Null => Conn::request(NFS_PROGRAM, 3, 0, &[]),
        Op::Getattr => Conn::request(NFS_PROGRAM, 3, 1, &fh.0),
        Op::Read => {
            let mut x = Xdr::default();
            x.0.extend_from_slice(&fh.0);
            x.u64(0).u32(args.read_size);
            Conn::request(NFS_PROGRAM, 3, 6, &x.0)
        }
    };
    let offset_at = req.len() - 12;
    let blocks = (size / args.read_size as u64).max(1);
    let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (id as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);

    loop {
        let now = Instant::now();
        if now >= until {
            break;
        }
        if let Op::Read = args.op {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let offset = (rng % blocks) * args.read_size as u64;
            req[offset_at..offset_at + 8].copy_from_slice(&offset.to_be_bytes());
        }
        let start = Instant::now();
        let res = conn.call(&mut req).await;
        let elapsed = start.elapsed();
        let mut bytes = 0u64;
        let ok = match (&res, args.op) {
            (Ok(_), Op::Null) => true,
            (Ok(body), _) => {
                let mut r = Rd(body);
                let status = r.u32().unwrap_or(u32::MAX);
                if status == 0 {
                    if let Op::Read = args.op {
                        // post_op_attr: present flag + fattr3 (84 bytes), then count
                        if r.u32().unwrap_or(0) == 1 {
                            r.0 = &r.0[84.min(r.0.len())..];
                        }
                        bytes = r.u32().unwrap_or(0) as u64;
                    }
                }
                status == 0
            }
            (Err(_), _) => false,
        };
        if res.is_err() {
            return Err(res.err().unwrap());
        }
        if start >= measure_from {
            if ok {
                stats.hist.record((elapsed.as_micros() as u64).max(1)).unwrap();
                stats.ops += 1;
                stats.bytes += bytes;
            } else {
                stats.errors += 1;
            }
        }
        if args.think_us > 0 {
            tokio::time::sleep(Duration::from_micros(args.think_us)).await;
        }
    }
    Ok(stats)
}

fn main() -> io::Result<()> {
    let args: &'static Args = Box::leak(Box::new(Args::parse()));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.threads)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let (handle, size) = match args.op {
            Op::Null => (Vec::new(), 0),
            _ => {
                let handle = resolve(args).await?;
                let size = file_size(args, &handle).await?;
                (handle, size)
            }
        };
        let handle: &'static [u8] = Box::leak(handle.into_boxed_slice());

        let begin = Instant::now();
        let measure_from = begin + Duration::from_secs_f64(args.warmup);
        let until = measure_from + Duration::from_secs_f64(args.duration);

        let tasks: Vec<_> = (0..args.conns)
            .map(|id| {
                tokio::spawn(async move { worker(args, id + 1, handle, size, measure_from, until).await })
            })
            .collect();

        let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
        let (mut ops, mut bytes, mut errors) = (0u64, 0u64, 0u64);
        for task in tasks {
            let stats = task.await.expect("worker panicked")?;
            hist.add(&stats.hist).unwrap();
            ops += stats.ops;
            bytes += stats.bytes;
            errors += stats.errors;
        }

        let secs = args.duration;
        println!(
            "{{\"label\":\"{}\",\"op\":\"{:?}\",\"conns\":{},\"ops\":{},\"errors\":{},\"ops_s\":{:.0},\"mib_s\":{:.1},\"mean_us\":{:.1},\"p50_us\":{},\"p90_us\":{},\"p99_us\":{},\"p999_us\":{},\"max_us\":{}}}",
            args.label,
            args.op,
            args.conns,
            ops,
            errors,
            ops as f64 / secs,
            bytes as f64 / secs / (1024.0 * 1024.0),
            hist.mean(),
            hist.value_at_quantile(0.5),
            hist.value_at_quantile(0.9),
            hist.value_at_quantile(0.99),
            hist.value_at_quantile(0.999),
            hist.max(),
        );
        Ok(())
    })
}
