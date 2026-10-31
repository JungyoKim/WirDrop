//! The receiver driver: accepts TLS connections and runs a
//! [`proto::receiver::Connection`](crate::proto::receiver::Connection) on
//! each, asking the frontend about every `/Ask` and writing uploads through
//! [`Staging`].

use std::collections::HashMap;
use std::io;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use tokio_rustls::TlsAcceptor;

use super::staging::Staging;
use super::tls::{self, Identity};
use crate::proto::apple_id;
use crate::proto::messages::{AskRequest, DiscoverRequest, Offer};
use crate::proto::receiver::{Answer, Config, Connection, Event};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// A connection with no traffic for this long, and no prompt up, is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Most connections from one address at a time.
const MAX_PER_PEER: usize = 4;
/// An upload must bring at least this much in every `RATE_WINDOW`, about
/// 1 KiB/s, or it's ended: a trickle would otherwise hold it open for good.
const MIN_UPLOAD_BYTES: u64 = 32 << 10;
const RATE_WINDOW: Duration = Duration::from_secs(30);
/// iOS may send a DVZip upload with no length and keep the connection open after the archive,
/// waiting for the answer. With `allow_dvzip_archive_boundary`, the upload ends at the archive's
/// end once nothing more has arrived for this long.
const BOUNDARY_QUIET: Duration = Duration::from_millis(50);
/// How often an upload reports [`ReceiverEvent::Progress`].
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct Options {
    pub config: Config,
    /// Where received files go.
    pub destination: PathBuf,
    pub max_connections: usize,
    /// Where requests get their ids: shared by a host's receivers, so that no two requests
    /// have the same.
    pub ids: Arc<AtomicU64>,
}

impl Options {
    pub fn new(config: Config, destination: PathBuf) -> Self {
        Options {
            config,
            destination,
            max_connections: 16,
            ids: Arc::new(AtomicU64::new(1)),
        }
    }
}

/// An `/Ask` waiting for the user. Dropping it declines.
#[derive(Debug)]
pub struct Request {
    pub id: u64,
    pub peer: SocketAddrV6,
    pub ask: AskRequest,
    /// The Apple ID the sender proved with its record and TLS certificate
    /// (proto::apple_id::verified_account), or None. Only this says who sent it: the record in
    /// the /Ask alone could be anyone's.
    pub account: Option<String>,
    reply: oneshot::Sender<Answer>,
}

impl Request {
    pub fn accept(self) {
        let _ = self.reply.send(Answer::Accept);
    }

    pub fn decline(self) {
        let _ = self.reply.send(Answer::Decline);
    }
}

/// What happened, for the frontend. `id` ties a transfer's events together.
#[derive(Debug)]
pub enum ReceiverEvent {
    Discovered {
        peer: SocketAddrV6,
        request: DiscoverRequest,
        /// The Apple ID the sender proved (proto::apple_id::verified_account).
        account: Option<String>,
    },
    Request(Request),
    /// The sender left, or the prompt timed out, before an answer.
    Withdrawn {
        id: u64,
    },
    Urls {
        id: u64,
        urls: Vec<String>,
    },
    Received {
        id: u64,
        paths: Vec<PathBuf>,
    },
    /// File data written so far for an accepted request, a few times a second.
    /// Dropped when nobody keeps up; the next one carries the total.
    Progress {
        id: u64,
        written: u64,
    },
    Failed {
        id: u64,
        reason: String,
    },
}

pub struct Receiver {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    options: Arc<Options>,
    config: Arc<Config>,
    events: mpsc::Sender<ReceiverEvent>,
    ids: Arc<AtomicU64>,
    slots: Arc<Mutex<Slots>>,
}

/// The connections being served. When every slot is taken, a new connection
/// takes the place of the one that has gone longest without finishing a
/// request, unless that one has an `/Ask` open, so connections that trickle
/// or say nothing can't lock real senders out (SEC-10).
#[derive(Default)]
struct Slots {
    next: u64,
    held: HashMap<u64, Slot>,
}

struct Slot {
    peer: Ipv6Addr,
    /// When the connection opened or last answered a request.
    progress: Instant,
    /// An `/Ask` is open on it: prompting, or accepted and not yet uploaded.
    busy: bool,
    /// The request it carries (the latest `/Ask` on it).
    request: Option<u64>,
    abort: Option<AbortHandle>,
}

impl Slots {
    /// A slot for a connection from `peer`, evicting another if all are
    /// taken, or `None` if there's no room.
    fn admit(&mut self, peer: Ipv6Addr, max: usize) -> Option<u64> {
        if self.held.values().filter(|s| s.peer == peer).count() >= MAX_PER_PEER {
            return None;
        }
        if self.held.len() >= max {
            let (&key, _) = self
                .held
                .iter()
                .filter(|(_, s)| !s.busy)
                .min_by_key(|(_, s)| s.progress)?;
            let slot = self.held.remove(&key).unwrap();
            tracing::debug!(peer = %slot.peer, "closing a quiet connection to make room");
            if let Some(abort) = slot.abort {
                abort.abort();
            }
        }
        let key = self.next;
        self.next += 1;
        self.held.insert(
            key,
            Slot {
                peer,
                progress: Instant::now(),
                busy: false,
                request: None,
                abort: None,
            },
        );
        Some(key)
    }
}

/// A connection's slot, given back when its task ends or is aborted.
struct Held {
    slots: Arc<Mutex<Slots>>,
    key: u64,
}

impl Held {
    fn update(&self, f: impl FnOnce(&mut Slot)) {
        if let Some(slot) = self.slots.lock().unwrap().held.get_mut(&self.key) {
            f(slot);
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.slots.lock().unwrap().held.remove(&self.key);
    }
}

/// Stops the transfers of a [Receiver]'s requests.
#[derive(Clone)]
pub struct Canceller {
    slots: Arc<Mutex<Slots>>,
}

impl Canceller {
    /// Ends the connection that carries request id, as it is: a prompt goes unanswered, an
    /// upload stops (and what it had written goes with its staging). No event follows. False
    /// when there's no such connection (it's over).
    pub fn cancel(&self, id: u64) -> bool {
        let mut slots = self.slots.lock().unwrap();
        let Some(key) = slots.held.iter().find(|(_, s)| s.request == Some(id)).map(|(k, _)| *k) else {
            return false;
        };
        let slot = slots.held.remove(&key).expect("found just now");
        if let Some(abort) = slot.abort {
            abort.abort();
        }
        true
    }
}

impl Receiver {
    /// Listens on `addr`, a link-local address with its scope (NET-2).
    pub async fn bind(
        addr: SocketAddrV6,
        identity: &Identity,
        options: Options,
    ) -> io::Result<(Self, mpsc::Receiver<ReceiverEvent>)> {
        let listener = TcpListener::bind(addr).await?;
        let acceptor = TlsAcceptor::from(tls::server_config(identity)?);
        let (events, rx) = mpsc::channel(64);
        let config = Arc::new(options.config.clone());
        let ids = options.ids.clone();
        Ok((
            Receiver {
                listener,
                acceptor,
                options: Arc::new(options),
                config,
                events,
                ids,
                slots: Arc::default(),
            },
            rx,
        ))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddrV6> {
        match self.listener.local_addr()? {
            std::net::SocketAddr::V6(addr) => Ok(addr),
            std::net::SocketAddr::V4(_) => Err(io::Error::other("not IPv6")),
        }
    }

    /// What stops a request's transfer from elsewhere (see [Canceller::cancel]); take it before
    /// [Receiver::run].
    pub fn canceller(&self) -> Canceller {
        Canceller { slots: self.slots.clone() }
    }

    /// Serves until the event receiver is dropped. A failed connection never
    /// stops the server (SEC-8).
    pub async fn run(self) {
        loop {
            let accepted = tokio::select! {
                accepted = self.listener.accept() => accepted,
                () = self.events.closed() => return,
            };
            let Ok((stream, peer)) = accepted else {
                // Out of descriptors, say: back off rather than spin.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            let std::net::SocketAddr::V6(peer) = peer else {
                continue;
            };
            let max = self.options.max_connections;
            let Some(key) = self.slots.lock().unwrap().admit(*peer.ip(), max) else {
                continue;
            };
            let held = Held {
                slots: self.slots.clone(),
                key,
            };
            let task = Task {
                acceptor: self.acceptor.clone(),
                options: self.options.clone(),
                config: self.config.clone(),
                events: self.events.clone(),
                ids: self.ids.clone(),
                peer,
            };
            let handle = tokio::spawn(async move {
                if let Err(e) = task.run(stream, &held).await {
                    tracing::debug!(%peer, "connection ended: {e}");
                }
            });
            let abort = handle.abort_handle();
            if let Some(slot) = self.slots.lock().unwrap().held.get_mut(&key) {
                slot.abort = Some(abort);
            }
        }
    }
}

/// The request or status line, for the debug log. The peer's bytes, so
/// escaped (SEC-4).
fn first_line(bytes: &[u8]) -> String {
    let line = bytes
        .split(|&b| b == b'\r' || b == b'\n')
        .next()
        .unwrap_or_default();
    format!(
        "{:?}",
        String::from_utf8_lossy(&line[..line.len().min(200)])
    )
}

struct Task {
    acceptor: TlsAcceptor,
    options: Arc<Options>,
    config: Arc<Config>,
    events: mpsc::Sender<ReceiverEvent>,
    ids: Arc<AtomicU64>,
    peer: SocketAddrV6,
}

impl Task {
    async fn run(self, stream: TcpStream, held: &Held) -> io::Result<()> {
        stream.set_nodelay(true)?;
        let mut tls = timeout(HANDSHAKE_TIMEOUT, self.acceptor.accept(stream))
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        let mut conn = Connection::new(self.config.clone());
        let peer_account = tls.get_ref().1.peer_certificates().and_then(crate::proto::verify::tls_account);
        if peer_account.is_some() {
            tracing::debug!(peer = %self.peer, "TLS client certificate: an Apple ID one, chained to Apple Root CA");
        }
        conn.set_peer_account(peer_account.clone());
        let mut buf = vec![0; 64 << 10];
        let mut timer: Option<Instant> = None;
        let mut idle = Instant::now() + IDLE_TIMEOUT;
        let mut answer: Option<oneshot::Receiver<Answer>> = None;
        let mut staging: Option<Staging> = None;
        let mut id = 0;
        let mut eof = false;
        // Accepted, and no upload yet: a connection that ends now has withdrawn its request (an
        // iPhone tapped twice sends two /Asks and uploads on one).
        let mut accepted_idle = false;
        // Upload bytes in the current rate window, and when it ends.
        let mut window = (0u64, Instant::now() + RATE_WINDOW);
        // File data written, and when it was last reported.
        let mut written = 0u64;
        let mut reported = Instant::now();
        loop {
            while let Some(event) = conn.poll() {
                match event {
                    Event::Transmit(bytes) => {
                        if bytes.starts_with(b"HTTP/") {
                            tracing::debug!(peer = %self.peer, "-> {}", first_line(&bytes));
                            held.update(|s| s.progress = Instant::now());
                        }
                        // Flushed: tokio-rustls may hold a record back
                        // when the socket is busy, and the peer would wait
                        // on it for good.
                        tls.write_all(&bytes).await?;
                        tls.flush().await?;
                    }
                    Event::SetTimer(after) => timer = after.map(|d| Instant::now() + d),
                    Event::Discovered(request) => {
                        let account = apple_id::verified_account(request.sender_record.as_deref(), peer_account.as_deref());
                        let _ = self.events.try_send(ReceiverEvent::Discovered {
                            peer: self.peer,
                            request,
                            account,
                        });
                    }
                    Event::Ask(ask) => {
                        tracing::debug!(
                            peer = %self.peer,
                            transfer_type = ?ask.transfer_type,
                            "/Ask {}",
                            ask.described
                        );
                        if ask.transfer_type.as_deref() == Some("links")
                            && matches!(ask.offer, Offer::Files(_))
                        {
                            tracing::warn!(
                                peer = %self.peer,
                                "a link offered as a file: Apple senders don't upload it to a \
                                 receiver without SUPPORTS_URL (MDNS-5)"
                            );
                        }
                        id = self.ids.fetch_add(1, Ordering::Relaxed);
                        held.update(|s| {
                            s.busy = true;
                            s.request = Some(id);
                        });
                        let (reply, rx) = oneshot::channel();
                        answer = Some(rx);
                        let account = apple_id::verified_account(ask.sender_record.as_deref(), peer_account.as_deref());
                        let request = Request {
                            id,
                            peer: self.peer,
                            ask,
                            account,
                            reply,
                        };
                        // Nobody listening, or too busy: the prompt goes
                        // unanswered and times out as a decline.
                        let _ = self.events.try_send(ReceiverEvent::Request(request));
                    }
                    Event::AskWithdrawn => {
                        answer = None;
                        held.update(|s| s.busy = false);
                        let _ = self.events.try_send(ReceiverEvent::Withdrawn { id });
                    }
                    Event::Urls(urls) => {
                        // The share is done; nothing is to come for it.
                        accepted_idle = false;
                        let _ = self.events.send(ReceiverEvent::Urls { id, urls }).await;
                    }
                    Event::UploadStarted { compressor } => {
                        accepted_idle = false;
                        let length = conn.upload_progress().and_then(|(_, n)| n);
                        tracing::info!(peer = %self.peer, "upload started: {compressor:?}, length {length:?}");
                        match Staging::new(&self.options.destination).await {
                            Ok(s) => {
                                staging = Some(s);
                                window = (0, Instant::now() + RATE_WINDOW);
                            }
                            Err(e) => return self.fail(&mut tls, id, e).await,
                        }
                    }
                    Event::CreateDir(path) => {
                        if let Err(e) = staging.as_mut().unwrap().create_dir(&path).await {
                            return self.fail(&mut tls, id, e).await;
                        }
                    }
                    Event::CreateFile(path) => {
                        if let Err(e) = staging.as_mut().unwrap().create_file(&path).await {
                            return self.fail(&mut tls, id, e).await;
                        }
                    }
                    Event::Data(data) => {
                        if let Err(e) = staging.as_mut().unwrap().write(&data).await {
                            return self.fail(&mut tls, id, e).await;
                        }
                        written += data.len() as u64;
                        if reported.elapsed() >= PROGRESS_EVERY {
                            reported = Instant::now();
                            let _ = self.events.try_send(ReceiverEvent::Progress { id, written });
                        }
                    }
                    Event::EndFile => {
                        if let Err(e) = staging.as_mut().unwrap().end_file().await {
                            return self.fail(&mut tls, id, e).await;
                        }
                    }
                    Event::UploadComplete => {
                        held.update(|s| s.busy = false);
                        let result = staging.take().unwrap().finish().await;
                        conn.finish_upload(result.as_ref().map(|_| ()).map_err(|e| e.to_string()));
                        let event = match result {
                            Ok(paths) => ReceiverEvent::Received { id, paths },
                            Err(e) => ReceiverEvent::Failed {
                                id,
                                reason: e.to_string(),
                            },
                        };
                        let _ = self.events.send(event).await;
                    }
                    Event::UploadFailed(reason) => {
                        // Dropping staging removes it and everything in it.
                        staging = None;
                        held.update(|s| s.busy = false);
                        let _ = self.events.send(ReceiverEvent::Failed { id, reason }).await;
                    }
                    Event::Close => {
                        if accepted_idle {
                            let _ = self.events.try_send(ReceiverEvent::Withdrawn { id });
                        }
                        let _ = tls.shutdown().await;
                        return Ok(());
                    }
                }
            }
            let prompting = answer.is_some();
            let boundary = self.config.allow_dvzip_archive_boundary && staging.is_some() && !eof;
            tokio::select! {
                read = tls.read(&mut buf), if !eof => {
                    match read {
                        Ok(0) => {
                            tracing::debug!(peer = %self.peer, "<- end of stream");
                            eof = true;
                            conn.receive_eof();
                        }
                        Ok(n) => {
                            if buf[..n].starts_with(b"POST ") || buf[..n].starts_with(b"GET ") {
                                tracing::debug!(peer = %self.peer, "<- {}", first_line(&buf[..n]));
                            }
                            let now = Instant::now();
                            idle = now + IDLE_TIMEOUT;
                            if staging.is_some() {
                                window.0 += n as u64;
                                if now >= window.1 {
                                    if window.0 < MIN_UPLOAD_BYTES {
                                        let e = io::Error::new(io::ErrorKind::TimedOut, "upload too slow");
                                        return self.fail(&mut tls, id, e).await;
                                    }
                                    window = (0, now + RATE_WINDOW);
                                }
                            }
                            conn.receive(&buf[..n]);
                        }
                        // A reset or TLS error ends the connection, and with
                        // it any prompt or upload.
                        Err(e) => {
                            if prompting || accepted_idle {
                                let _ = self.events.try_send(ReceiverEvent::Withdrawn { id });
                            }
                            if staging.is_some() {
                                let _ = self.events.send(ReceiverEvent::Failed { id, reason: e.to_string() }).await;
                            }
                            return Err(e);
                        }
                    }
                }
                () = sleep_until(timer.unwrap_or(idle)), if timer.is_some() => {
                    timer = None;
                    if answer.take().is_some() {
                        held.update(|s| s.busy = false);
                        let _ = self.events.try_send(ReceiverEvent::Withdrawn { id });
                    }
                    conn.timeout();
                }
                () = sleep_until(idle), if !prompting && timer.is_none() => {
                    if accepted_idle {
                        let _ = self.events.try_send(ReceiverEvent::Withdrawn { id });
                    }
                    if staging.is_some() {
                        let reason = "the sender stopped sending".to_string();
                        let _ = self.events.send(ReceiverEvent::Failed { id, reason }).await;
                    }
                    return Err(io::ErrorKind::TimedOut.into());
                }
                () = sleep(BOUNDARY_QUIET), if boundary => {
                    if conn.finish_validated_dvzip_boundary() {
                        tracing::info!(peer = %self.peer, "upload ended at the DVZip archive boundary");
                    }
                }
                reply = async { answer.as_mut().unwrap().await }, if prompting => {
                    answer = None;
                    // A dropped request is a decline.
                    let reply = reply.unwrap_or(Answer::Decline);
                    if reply == Answer::Decline {
                        held.update(|s| s.busy = false);
                    }
                    accepted_idle = reply == Answer::Accept;
                    conn.answer(reply);
                }
            }
        }
    }

    async fn fail<S: AsyncWriteExt + Unpin>(
        &self,
        tls: &mut S,
        id: u64,
        e: io::Error,
    ) -> io::Result<()> {
        let _ = tls
            .write_all(&crate::proto::http::response(
                500,
                &[("Connection", "close")],
                b"",
            ))
            .await;
        let _ = tls.flush().await;
        let _ = tls.shutdown().await;
        let _ = self
            .events
            .send(ReceiverEvent::Failed {
                id,
                reason: e.to_string(),
            })
            .await;
        Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u16) -> Ipv6Addr {
        Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, n)
    }

    // spec: SEC-10
    #[test]
    fn a_new_connection_replaces_the_quietest_idle_one() {
        let mut slots = Slots::default();
        let keys: Vec<u64> = (1..=3).map(|n| slots.admit(addr(n), 3).unwrap()).collect();
        // The oldest is prompting; of the others, the second answered a
        // request last.
        slots.held.get_mut(&keys[0]).unwrap().busy = true;
        slots.held.get_mut(&keys[1]).unwrap().progress += Duration::from_secs(5);
        let new = slots.admit(addr(4), 3).unwrap();
        let mut left: Vec<u64> = slots.held.keys().copied().collect();
        left.sort();
        assert_eq!(left, [keys[0], keys[1], new]);
    }

    // spec: SEC-10
    #[test]
    fn busy_connections_are_never_replaced() {
        let mut slots = Slots::default();
        for n in 1..=2 {
            let key = slots.admit(addr(n), 2).unwrap();
            slots.held.get_mut(&key).unwrap().busy = true;
        }
        assert_eq!(slots.admit(addr(3), 2), None);
        assert_eq!(slots.held.len(), 2);
    }

    // spec: SEC-10
    #[test]
    fn one_address_gets_a_few_connections() {
        let mut slots = Slots::default();
        for _ in 0..MAX_PER_PEER {
            assert!(slots.admit(addr(1), 16).is_some());
        }
        assert_eq!(slots.admit(addr(1), 16), None);
        assert!(slots.admit(addr(2), 16).is_some());
    }
}
