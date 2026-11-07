//! The sender driver: `/Discover` a receiver, then `/Ask` and `/Upload` on
//! one fresh TLS connection (HTTP-1), running a
//! [`proto::sender::Transfer`](crate::proto::sender::Transfer).
//!
//! The `/Ask` goes out at once with the files' names and sizes. Their archive
//! is packed alongside, on a thread of its own, and uploaded chunked as it comes
//! (UP-1a), as an iPhone sends: packing it into a file first, for a
//! `Content-Length` like Finder's, held a 1.26 GB video's `/Ask` back five
//! seconds.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::net::SocketAddrV6;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use flate2::Compression;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::tls::{self, Identity};
use crate::proto::archive::compress::{Compressor, Encoder};
use crate::proto::archive::cpio::{self, Entry};
use crate::proto::archive::extract::RelPath;
use crate::proto::mdns::SUPPORTS_DVZIP;
use crate::proto::messages::{AskFile, Offer};
use crate::proto::sender::{self, Action, Config, Outcome, Receiver, ResponseReader, Transfer};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(10);
/// A write the receiver doesn't take in this long fails the transfer, so a
/// receiver that stops reading can't hold the sender forever (SEC-10).
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// The packed archive reaches the upload in blocks of this size, at most
/// BODY_BLOCKS of them ahead of it (16 MB).
const BODY_BLOCK: usize = 256 << 10;
const BODY_BLOCKS: usize = 64;

/// What to send.
#[derive(Debug, Clone)]
pub enum Payload {
    Files(Vec<PathBuf>),
    Urls(Vec<String>),
}

/// Upload progress, in bytes of the compressed body. `total` is the files'
/// size, which a DVZip archive of photos and videos matches within its
/// headers; `sent` stops there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub sent: u64,
    pub total: u64,
}

/// What a transfer reports while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The `/Ask` is going out; for files, packing has started alongside.
    Asking,
    /// The receiver accepted the `/Ask`. A failure after this one is the
    /// upload's.
    Accepted,
    Progress(Progress),
}

/// Clears its flag when dropped: a send whose future is dropped (cancelled) stops packing.
struct Alive(Arc<AtomicBool>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// An archive being packed for the upload.
struct Body {
    blocks: mpsc::Receiver<io::Result<Vec<u8>>>,
    total: u64,
    _alive: Alive,
}

/// Hands what it is given to the upload in blocks of BODY_BLOCK, waiting while
/// BODY_BLOCKS of them are unsent. Fails once the upload is gone.
struct BlockWriter {
    tx: mpsc::Sender<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
}

impl BlockWriter {
    fn send_block(&mut self) -> io::Result<()> {
        let block = std::mem::replace(&mut self.buf, Vec::with_capacity(BODY_BLOCK + (64 << 10)));
        self.tx
            .blocking_send(Ok(block))
            .map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "the send was cancelled"))
    }
}

impl Write for BlockWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= BODY_BLOCK {
            self.send_block()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.send_block()
    }
}

/// Why an upload stopped short.
enum UploadError {
    /// Packing the archive failed: a file went away or shrank.
    Pack(io::Error),
    /// Writing to the receiver failed, which may have refused and closed.
    Write(io::Error),
}

pub struct Sender {
    config: Config,
    connector: TlsConnector,
}

impl Sender {
    pub fn new(config: Config) -> io::Result<Self> {
        Self::with_client_identity(config, None)
    }

    /// A sender that presents a client certificate, which Apple senders
    /// don't (TLS-3). For tests.
    #[doc(hidden)]
    pub fn with_client_identity(config: Config, identity: Option<&Identity>) -> io::Result<Self> {
        Ok(Sender {
            config,
            connector: TlsConnector::from(tls::client_config(identity)?),
        })
    }

    async fn connect(&self, addr: SocketAddrV6) -> io::Result<TlsStream<TcpStream>> {
        let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no TCP answer"))??;
        stream.set_nodelay(true)?;
        timeout(
            CONNECT_TIMEOUT,
            self.connector.connect(tls::server_name(), stream),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TCP up, no TLS answer"))?
        .map_err(|e| io::Error::new(e.kind(), format!("TCP up, TLS failed: {e}")))
    }

    /// `/Discover` on a connection of its own (DISCOVERING in §8.1).
    /// `None` if the receiver doesn't want to be listed (DISC-1).
    pub async fn discover(&self, addr: SocketAddrV6) -> io::Result<Option<Receiver>> {
        let mut tls = self.connect(addr).await?;
        let response = timeout(DISCOVER_TIMEOUT, async {
            tls.write_all(&sender::discover_request(&self.config)).await?;
            tls.flush().await?;
            read_response(&mut tls).await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS up, no /Discover answer"))??;
        let _ = tls.shutdown().await;
        Ok(sender::parse_discover_response(&response))
    }

    /// Asks the receiver at `addr` to take `payload`, and uploads it if it
    /// does. `receiver_flags` is its TXT `flags`, which decides between
    /// DVZip and gzip.
    pub async fn send(
        &self,
        addr: SocketAddrV6,
        payload: &Payload,
        receiver_flags: u32,
        events: Option<mpsc::UnboundedSender<Event>>,
    ) -> io::Result<Outcome> {
        let compressor = if receiver_flags & SUPPORTS_DVZIP != 0 {
            Compressor::DvZip
        } else {
            Compressor::Gzip
        };
        let (offer, mut body) = match payload {
            Payload::Urls(urls) => (Offer::Urls(urls.clone()), None),
            Payload::Files(paths) => {
                let paths = paths.clone();
                let (offer, items) = tokio::task::spawn_blocking(move || offer_for(&paths))
                    .await
                    .map_err(io::Error::other)??;
                let total = match &offer {
                    Offer::Files(files) => files.iter().filter_map(|f| f.size).sum(),
                    Offer::Urls(_) => 0,
                };
                // Packing runs on a thread of its own, which a dropped future can't stop: it
                // looks at the flag before each file and block, and gives up once the send is
                // gone or the receiver declined (its blocks have nowhere to go).
                let alive = Arc::new(AtomicBool::new(true));
                let (tx, blocks) = mpsc::channel(BODY_BLOCKS);
                let flag = alive.clone();
                tokio::task::spawn_blocking(move || {
                    let check = || {
                        if flag.load(Ordering::Relaxed) {
                            Ok(())
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::Interrupted,
                                "the send was cancelled",
                            ))
                        }
                    };
                    let out = BlockWriter {
                        tx: tx.clone(),
                        buf: Vec::with_capacity(BODY_BLOCK + (64 << 10)),
                    };
                    if let Err(e) = pack(&items, compressor, &check, out) {
                        let _ = tx.blocking_send(Err(e));
                    }
                });
                let body = Body {
                    blocks,
                    total,
                    _alive: Alive(alive),
                };
                (offer, Some(body))
            }
        };
        if let Some(events) = &events {
            let _ = events.send(Event::Asking);
        }
        let transfer_id = uuid_v4()?;
        let framing = body.as_ref().map(|_| (compressor, None));
        let mut transfer = Transfer::new(&self.config, &transfer_id, &offer, framing);

        let mut tls = self.connect(addr).await?;
        let mut buf = vec![0; 16 << 10];
        let mut timer: Option<Instant> = None;
        let mut eof = false;
        loop {
            while let Some(action) = transfer.poll() {
                match action {
                    Action::Transmit(bytes) => {
                        // Flushed, or tokio-rustls may hold it back.
                        if let Err(e) = write(&mut tls, &bytes).await {
                            transfer.receive_eof();
                            tracing::debug!("write failed: {e}");
                        }
                    }
                    Action::SetTimer(after) => timer = after.map(|d| Instant::now() + d),
                    Action::Accepted => {
                        if let Some(events) = &events {
                            let _ = events.send(Event::Accepted);
                        }
                    }
                    Action::Reconnect => tls = self.connect(addr).await?,
                    Action::SendUpload(head) => {
                        let body = body.as_mut().expect("a file share has a body");
                        match upload(&mut tls, &head, body, events.as_ref()).await {
                            Ok(()) => transfer.upload_sent(),
                            Err(UploadError::Pack(e)) => {
                                let _ = tls.shutdown().await;
                                return Err(e);
                            }
                            Err(UploadError::Write(e)) => {
                                // The receiver may have refused and closed;
                                // its response, if any, says why.
                                tracing::debug!("upload write failed: {e}");
                                transfer.upload_sent();
                            }
                        }
                    }
                    Action::Done(outcome) => {
                        let _ = tls.shutdown().await;
                        return Ok(outcome);
                    }
                }
            }
            tokio::select! {
                read = tls.read(&mut buf), if !eof => match read {
                    Ok(0) | Err(_) => {
                        eof = true;
                        transfer.receive_eof();
                    }
                    Ok(n) => transfer.receive(&buf[..n]),
                },
                () = sleep_until(timer.unwrap_or_else(Instant::now)), if timer.is_some() => {
                    timer = None;
                    transfer.timeout();
                }
            }
        }
    }
}

async fn read_response<S: AsyncReadExt + Unpin>(stream: &mut S) -> io::Result<sender::Response> {
    let mut reader = ResponseReader::new();
    let mut buf = vec![0; 16 << 10];
    loop {
        let n = stream.read(&mut buf).await?;
        let result = if n == 0 {
            reader.receive_eof().map(Some)
        } else {
            reader.receive(&buf[..n])
        };
        match result {
            Ok(Some(response)) => return Ok(response),
            Ok(None) => {}
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
        }
    }
}

/// The chunked upload (UP-1a): each packed block as a chunk, then the last,
/// empty one.
async fn upload<S: AsyncWriteExt + Unpin>(
    stream: &mut S,
    head: &[u8],
    body: &mut Body,
    events: Option<&mpsc::UnboundedSender<Event>>,
) -> Result<(), UploadError> {
    write(stream, head).await.map_err(UploadError::Write)?;
    let mut sent = 0;
    let mut chunk = Vec::with_capacity(BODY_BLOCK + (64 << 10) + 16);
    while let Some(block) = body.blocks.recv().await {
        let block = block.map_err(UploadError::Pack)?;
        if block.is_empty() {
            continue;
        }
        chunk.clear();
        chunk.extend_from_slice(format!("{:x}\r\n", block.len()).as_bytes());
        chunk.extend_from_slice(&block);
        chunk.extend_from_slice(b"\r\n");
        write(stream, &chunk).await.map_err(UploadError::Write)?;
        sent += block.len() as u64;
        if let Some(events) = events {
            let progress = Progress {
                sent: sent.min(body.total),
                total: body.total,
            };
            let _ = events.send(Event::Progress(progress));
        }
    }
    write(stream, b"0\r\n\r\n").await.map_err(UploadError::Write)
}

/// Writes and flushes `bytes`, failing if the receiver stalls.
async fn write<S: AsyncWriteExt + Unpin>(stream: &mut S, bytes: &[u8]) -> io::Result<()> {
    let written = async {
        stream.write_all(bytes).await?;
        stream.flush().await
    };
    timeout(WRITE_TIMEOUT, written)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the receiver stopped reading"))?
}

/// A fresh transfer ID: an uppercase UUID v4 (§6.4).
fn uuid_v4() -> io::Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    b[6] = b[6] & 0x0f | 0x40;
    b[8] = b[8] & 0x3f | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// Builds the `/Ask` offer for `paths` and compresses their archive into a
/// temporary file, returning it rewound with its length.
pub fn prepare(paths: &[PathBuf], compressor: Compressor) -> io::Result<(Offer, File, u64)> {
    prepare_cancellable(paths, compressor, &|| Ok(()))
}

/// Checks cancellation before each filesystem operation and each bounded block.
pub fn prepare_cancellable(
    paths: &[PathBuf],
    compressor: Compressor,
    check: &dyn Fn() -> io::Result<()>,
) -> io::Result<(Offer, File, u64)> {
    let items = items(paths, check)?;
    let mut spool = tempfile::tempfile()?;
    let files = pack(&items, compressor, check, io::BufWriter::new(&mut spool))?;
    let len = spool.stream_position()?;
    spool.rewind()?;
    Ok((Offer::Files(files), spool, len))
}

/// The `/Ask` offer for `paths`, from their names and sizes without reading
/// them, and the items to [pack].
pub fn offer_for(paths: &[PathBuf]) -> io::Result<(Offer, Vec<(String, PathBuf)>)> {
    let items = items(paths, &|| Ok(()))?;
    let mut files = Vec::new();
    for (name, path) in &items {
        let meta = std::fs::symlink_metadata(path)?;
        let size = if meta.file_type().is_symlink() {
            return Err(invalid("symbolic links are not transferred".into()));
        } else if meta.is_dir() {
            tree_size(path, &format!("./{name}"))?
        } else if meta.is_file() {
            fits(path, &meta)?;
            meta.len()
        } else {
            return Err(invalid(format!(
                "{} isn't a file or directory",
                path.display()
            )));
        };
        files.push(ask_file(name, &meta, size));
    }
    Ok((Offer::Files(files), items))
}

/// The items to send, named as they will arrive.
fn items(
    paths: &[PathBuf],
    check: &dyn Fn() -> io::Result<()>,
) -> io::Result<Vec<(String, PathBuf)>> {
    check()?;
    let mut items = Vec::new();
    for path in paths {
        check()?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid(format!("{} has no UTF-8 file name", path.display())))?;
        RelPath::file_name(name).map_err(|e| invalid(e.to_string()))?;
        if items.iter().any(|(n, _): &(String, PathBuf)| n == name) {
            return Err(invalid(format!("two items are called {name}")));
        }
        items.push((name.to_string(), path.clone()));
    }
    if items.is_empty() {
        return Err(invalid("nothing to send".into()));
    }
    Ok(items)
}

/// Writes the archive of `items` to `out`, returning their `/Ask` entries.
fn pack(
    items: &[(String, PathBuf)],
    compressor: Compressor,
    check: &dyn Fn() -> io::Result<()>,
    out: impl Write,
) -> io::Result<Vec<AskFile>> {
    let mut writer = ArchiveWriter {
        encoder: Encoder::new(compressor, Compression::fast()),
        out,
        ino: 1,
        check,
    };
    // UP-2a: the root entry first.
    writer.entry(
        &dir_entry(".", mtime(&std::fs::metadata(&items[0].1)?)),
        None,
    )?;
    let mut files = Vec::new();
    for (name, path) in items {
        check()?;
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(invalid("symbolic links are not transferred".into()));
        }
        let size = if meta.is_dir() {
            writer.tree(path, &format!("./{name}"))?
        } else if meta.is_file() {
            writer.file(path, &format!("./{name}"), &meta)?;
            meta.len()
        } else {
            return Err(invalid(format!(
                "{} isn't a file or directory",
                path.display()
            )));
        };
        files.push(ask_file(name, &meta, size));
    }
    writer.finish()?;
    Ok(files)
}

fn ask_file(name: &str, meta: &std::fs::Metadata, size: u64) -> AskFile {
    AskFile {
        name: name.into(),
        file_type: sender::uti(name, meta.is_dir()).into(),
        size: Some(size),
        bom_path: format!("./{name}"),
        is_directory: meta.is_dir(),
    }
}

/// A file the odc archive can hold.
fn fits(path: &Path, meta: &std::fs::Metadata) -> io::Result<()> {
    if meta.len() > cpio::ODC_MAX_SIZE {
        return Err(invalid(format!(
            "{} is too large for an archive",
            path.display()
        )));
    }
    Ok(())
}

/// A directory's entries to send, in name order. Links (symlinks and
/// junctions, which could lead outside it or round in a loop), special files
/// and, on Windows, files that are both hidden and system (desktop.ini,
/// Thumbs.db) are left out. Other reparse points, such as OneDrive's files
/// kept online, are read like any file.
fn children(
    path: &Path,
    check: &dyn Fn() -> io::Result<()>,
) -> io::Result<Vec<(String, PathBuf, std::fs::Metadata)>> {
    let mut listed = Vec::new();
    for child in std::fs::read_dir(path)? {
        check()?;
        if listed.len() >= 100_000 {
            return Err(invalid("directory has too many entries".into()));
        }
        listed.push(child?);
    }
    listed.sort_by_key(|c| c.file_name());
    let mut out = Vec::new();
    for child in listed {
        check()?;
        let Some(child_name) = child.file_name().to_str().map(String::from) else {
            return Err(invalid("file name is not UTF-8".into()));
        };
        if RelPath::file_name(&child_name).is_err() {
            return Err(invalid("unsafe file name".into()));
        }
        let meta = std::fs::symlink_metadata(child.path())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const HIDDEN_SYSTEM: u32 = 0x2 | 0x4;
            if meta.file_attributes() & HIDDEN_SYSTEM == HIDDEN_SYSTEM {
                continue;
            }
        }
        if meta.is_dir() || meta.is_file() {
            out.push((child_name, child.path(), meta));
        }
    }
    Ok(out)
}

fn deep(name: &str) -> io::Result<()> {
    if name.split('/').count() > 65 {
        return Err(invalid("directory nesting exceeds limit".into()));
    }
    Ok(())
}

/// The total file size [ArchiveWriter::tree] will pack under `path`.
fn tree_size(path: &Path, name: &str) -> io::Result<u64> {
    deep(name)?;
    let mut total = 0;
    for (child_name, child_path, meta) in children(path, &|| Ok(()))? {
        if meta.is_dir() {
            total += tree_size(&child_path, &format!("{name}/{child_name}"))?;
        } else {
            fits(&child_path, &meta)?;
            total += meta.len();
        }
    }
    Ok(total)
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn mtime(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The owner a Mac's archives carry; receivers ignore it.
const UID: u32 = 501;
const GID: u32 = 20;

/// UP-2: entries for an odc archive.
pub fn file_entry(name: &str, size: u64, ino: u32, mtime: u64) -> Entry {
    Entry {
        name: name.into(),
        mode: cpio::MODE_FILE | 0o644,
        size,
        nlink: 1,
        ino,
        uid: UID,
        gid: GID,
        mtime,
    }
}

pub fn dir_entry(name: &str, mtime: u64) -> Entry {
    Entry {
        name: name.into(),
        mode: cpio::MODE_DIR | 0o755,
        size: 0,
        nlink: 2,
        ino: 0,
        uid: UID,
        gid: GID,
        mtime,
    }
}

struct ArchiveWriter<'a, W: Write> {
    encoder: Encoder,
    out: W,
    ino: u32,
    check: &'a dyn Fn() -> io::Result<()>,
}

impl<W: Write> ArchiveWriter<'_, W> {
    fn entry(&mut self, entry: &Entry, data: Option<&mut dyn Read>) -> io::Result<()> {
        (self.check)()?;
        if self.ino > 100_000 {
            return Err(invalid("too many archive entries".into()));
        }
        let mut entry = entry.clone();
        entry.ino = self.ino;
        self.ino += 1;
        let header = self.encoder.write(&cpio::odc_header(&entry));
        self.out.write_all(&header)?;
        if let Some(data) = data {
            self.encoder.next_file();
            let mut left = entry.size;
            let mut buf = vec![0; 64 << 10];
            while left > 0 {
                (self.check)()?;
                let n = data.read(&mut buf[..(left as usize).min(64 << 10)])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "file shrank while sending",
                    ));
                }
                left -= n as u64;
                let compressed = self.encoder.write(&buf[..n]);
                self.out.write_all(&compressed)?;
            }
        }
        Ok(())
    }

    fn file(&mut self, path: &Path, name: &str, meta: &std::fs::Metadata) -> io::Result<()> {
        (self.check)()?;
        fits(path, meta)?;
        let mut file = File::open(path)?;
        self.entry(
            &file_entry(name, meta.len(), 0, mtime(meta)),
            Some(&mut file),
        )
    }

    /// A directory and everything under it, as [children] lists them.
    /// Returns the total file size.
    fn tree(&mut self, path: &Path, name: &str) -> io::Result<u64> {
        deep(name)?;
        self.entry(&dir_entry(name, mtime(&std::fs::metadata(path)?)), None)?;
        let mut total = 0;
        for (child_name, child_path, meta) in children(path, self.check)? {
            let child_name = format!("{name}/{child_name}");
            if meta.is_dir() {
                total += self.tree(&child_path, &child_name)?;
            } else {
                self.file(&child_path, &child_name, &meta)?;
                total += meta.len();
            }
        }
        Ok(total)
    }

    fn finish(mut self) -> io::Result<()> {
        (self.check)()?;
        let trailer = self.encoder.write(&cpio::odc_trailer());
        self.out.write_all(&trailer)?;
        self.out.write_all(&self.encoder.finish())?;
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: SEC-10
    #[tokio::test(start_paused = true)]
    async fn a_receiver_that_stops_reading_fails_the_upload() {
        // A pipe nobody reads: the write fills it, then waits.
        let (mut ours, _theirs) = tokio::io::duplex(1024);
        let started = Instant::now();
        let e = write(&mut ours, &[0; 4096]).await.unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert_eq!(started.elapsed(), WRITE_TIMEOUT);
    }
}
