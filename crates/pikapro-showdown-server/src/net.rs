//! The network layer: websocket connections feeding one [`Hub`] task.
//!
//! Showdown's server takes SockJS connections; its raw-websocket endpoint
//! (`/showdown/websocket`, what poke-env connects to) sends each message
//! as one text frame, as here. Any path is accepted.
//!
//! What a connection may cost is bounded ([`Limits`]): the number of
//! connections, the time to finish the websocket handshake, the size of a
//! message, and the bytes waiting to be sent, to one client and to all of
//! them (a client that stops reading is dropped). The hub's inbox is
//! bounded too: when it is full, connections stop reading until it drains.
//! An error accepting a connection (running out of file descriptors, say)
//! is logged, and the server waits a little and goes on accepting.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::hub::{Config, Hub};
use crate::users::ConnId;

/// What a connection may cost.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Open connections at most; more are closed at once.
    pub max_connections: usize,
    /// How long a new connection has to finish the websocket handshake.
    pub handshake_timeout: Duration,
    /// The largest websocket message (and frame) read, in bytes; a larger
    /// one closes the connection. Showdown drops messages over 100 KB (with
    /// a popup, see `Hub::message`); this is a hard cap above that.
    pub max_message_bytes: usize,
    /// Bytes waiting to be sent to one client at most: past them, the
    /// client (which is not reading) is dropped.
    pub max_queued_bytes: usize,
    /// Bytes waiting to be sent to all clients together. Past them, a
    /// client is dropped rather than have more than 64 KiB waiting, so the
    /// most ever waiting is this plus 64 KiB per connection.
    pub max_total_queued_bytes: usize,
}

/// What a client may have waiting once [`Limits::max_total_queued_bytes`]
/// is reached.
const KEEPING_UP: usize = 64 << 10;

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_connections: 1000,
            handshake_timeout: Duration::from_secs(10),
            // Up to 100 * 1024 UTF-16 units, each at most 3 bytes in UTF-8.
            max_message_bytes: 3 * crate::hub::MAX_MESSAGE_LENGTH + 1024,
            max_queued_bytes: 4 << 20,
            max_total_queued_bytes: 256 << 20,
        }
    }
}

/// Events from the connections to the hub.
enum Event {
    Connect(ConnId, Outbox),
    Message(ConnId, String),
    Disconnect(ConnId),
}

/// The bytes waiting to be sent to one connection, counted in the
/// server-wide total too. The hub's [`Outbox`] and the connection's writer
/// share it; when both are gone (the connection closed, killed or timed
/// out), dropping it takes whatever was still waiting off the total.
struct Queued {
    mine: AtomicUsize,
    total: Arc<AtomicUsize>,
}

impl Queued {
    fn new(total: Arc<AtomicUsize>) -> Queued {
        Queued { mine: AtomicUsize::new(0), total }
    }

    fn add(&self, len: usize) {
        self.mine.fetch_add(len, Ordering::Relaxed);
        self.total.fetch_add(len, Ordering::Relaxed);
    }

    fn sub(&self, len: usize) {
        self.mine.fetch_sub(len, Ordering::Relaxed);
        self.total.fetch_sub(len, Ordering::Relaxed);
    }
}

impl Drop for Queued {
    fn drop(&mut self) {
        self.total.fetch_sub(*self.mine.get_mut(), Ordering::Relaxed);
    }
}

/// The hub's end of a connection: what to send it, with a budget of
/// bytes waiting.
struct Outbox {
    sender: mpsc::UnboundedSender<String>,
    queued: Arc<Queued>,
    /// Tells the connection to close (it is not reading).
    kill: Arc<Notify>,
    limit: usize,
    total_limit: usize,
}

impl Outbox {
    /// Queues a message; `false` if the client would have more than its
    /// budget waiting, or more than [`KEEPING_UP`] with all clients
    /// together over theirs (or is gone).
    fn push(&self, message: String) -> bool {
        let len = message.len();
        let mine = self.queued.mine.load(Ordering::Relaxed) + len;
        if mine > self.limit {
            return false;
        }
        if mine > KEEPING_UP && self.queued.total.load(Ordering::Relaxed) + len > self.total_limit {
            return false;
        }
        self.queued.add(len);
        if self.sender.send(message).is_err() {
            self.queued.sub(len);
            return false;
        }
        true
    }
}

/// Where connections come from: a [`TcpListener`] (a trait so tests can
/// make accepting fail).
pub(crate) trait Accept: Send + 'static {
    fn accept(&mut self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send;
}

impl Accept for TcpListener {
    fn accept(&mut self) -> impl Future<Output = std::io::Result<(TcpStream, SocketAddr)>> + Send {
        TcpListener::accept(self)
    }
}

/// Serves forever: each connection gets a task; the hub runs in its own,
/// ticking every 250 ms for timers.
pub async fn serve(listener: TcpListener, config: Config, limits: Limits) {
    serve_from(listener, config, limits).await;
}

pub(crate) async fn serve_from(mut listener: impl Accept, config: Config, limits: Limits) {
    let (events, inbox) = mpsc::channel(1024);
    tokio::spawn(run_hub(Hub::new(config), inbox));
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let total = Arc::new(AtomicUsize::new(0));
    let mut next_conn: ConnId = 0;
    let mut backoff = Duration::from_millis(10);
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                log(&format!("accepting a connection failed: {e}; retrying in {backoff:?}"));
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(1));
                continue;
            }
        };
        backoff = Duration::from_millis(10);
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            log(&format!("{addr}: refused, {} connections open", limits.max_connections));
            drop(stream);
            continue;
        };
        next_conn += 1;
        tokio::spawn(connection(stream, addr, next_conn, events.clone(), limits, total.clone(), slot));
    }
}

async fn run_hub(mut hub: Hub, mut inbox: mpsc::Receiver<Event>) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut outboxes: HashMap<ConnId, Outbox> = HashMap::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            event = inbox.recv() => {
                let Some(event) = event else { return };
                match event {
                    Event::Connect(conn, outbox) => {
                        outboxes.insert(conn, outbox);
                        guarded(&mut hub, "a connection", now(), |hub| hub.connect(conn));
                    }
                    Event::Message(conn, text) => {
                        guarded(&mut hub, "a message", now(), |hub| hub.message(conn, &text, now()));
                    }
                    Event::Disconnect(conn) => {
                        outboxes.remove(&conn);
                        guarded(&mut hub, "a disconnection", now(), |hub| hub.disconnect(conn, now()));
                    }
                }
            }
            _ = ticker.tick() => guarded(&mut hub, "a tick", now(), |hub| hub.tick(now())),
        }
        for (conn, message) in hub.take_outbox() {
            let Some(outbox) = outboxes.get(&conn) else { continue };
            if !outbox.push(message) {
                // Not reading: the connection closes, and its task tells
                // the hub it has gone.
                log(&format!("connection {conn}: too many bytes unsent, dropped"));
                outbox.kill.notify_one();
                outboxes.remove(&conn);
            }
        }
    }
}

/// Showdown's `crashguard`: a panic in the hub outside a battle (rooms
/// catch their own, see `room.rs`) is logged, what the handler left
/// half-done is put back (`Hub::recover`), and the server goes on.
fn guarded(hub: &mut Hub, what: &str, now: u64, f: impl FnOnce(&mut Hub)) {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(hub)));
    if run.is_err() {
        log(&format!("crash guard: a panic while handling {what}"));
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hub.recover(now))).is_err() {
            log("crash guard: a panic while recovering");
        }
    }
}

async fn connection(
    stream: TcpStream,
    addr: SocketAddr,
    conn: ConnId,
    events: mpsc::Sender<Event>,
    limits: Limits,
    total: Arc<AtomicUsize>,
    _slot: OwnedSemaphorePermit,
) {
    let _ = stream.set_nodelay(true);
    let config = WebSocketConfig::default()
        .max_message_size(Some(limits.max_message_bytes))
        .max_frame_size(Some(limits.max_message_bytes));
    let handshake = tokio_tungstenite::accept_async_with_config(stream, Some(config));
    let ws = match tokio::time::timeout(limits.handshake_timeout, handshake).await {
        Ok(Ok(ws)) => ws,
        Ok(Err(e)) => return log(&format!("{addr}: websocket handshake failed: {e}")),
        Err(_) => return log(&format!("{addr}: websocket handshake timed out")),
    };
    let (mut sink, mut source) = ws.split();
    let (sender, mut outgoing) = mpsc::unbounded_channel::<String>();
    let queued = Arc::new(Queued::new(total));
    let kill = Arc::new(Notify::new());
    let outbox = Outbox {
        sender,
        queued: queued.clone(),
        kill: kill.clone(),
        limit: limits.max_queued_bytes,
        total_limit: limits.max_total_queued_bytes,
    };
    if events.send(Event::Connect(conn, outbox)).await.is_err() {
        return;
    }
    let mut writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            let len = message.len();
            let sent = sink.send(Message::text(message)).await;
            queued.sub(len);
            if sent.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut killed = false;
    loop {
        tokio::select! {
            message = source.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if events.send(Event::Message(conn, text.as_str().to_owned())).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            () = kill.notified() => {
                killed = true;
                break;
            }
        }
    }
    let _ = events.send(Event::Disconnect(conn)).await;
    if killed {
        // The writer may be stuck sending to a client that doesn't read.
        writer.abort();
        return;
    }
    // The hub drops this connection's sender, which ends the writer: it
    // closes the socket, answering the client's close frame.
    if tokio::time::timeout(Duration::from_secs(5), &mut writer).await.is_err() {
        writer.abort();
        log(&format!("{addr}: closing timed out"));
    }
}

/// A line on stderr.
pub fn log(text: &str) {
    eprintln!("[pikapro-showdown-server] {text}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::Error as WsError;

    fn config() -> Config {
        Config { validate: false, seed: Some(1), ..Config::default() }
    }

    /// A listener whose first accepts fail, as they do when the process is
    /// out of file descriptors.
    struct Flaky {
        inner: TcpListener,
        failures: usize,
    }

    impl Accept for Flaky {
        async fn accept(&mut self) -> std::io::Result<(TcpStream, SocketAddr)> {
            if self.failures > 0 {
                self.failures -= 1;
                return Err(std::io::Error::from_raw_os_error(24));
            }
            self.inner.accept().await
        }
    }

    type Client = tokio_tungstenite::WebSocketStream<TcpStream>;

    async fn client(addr: SocketAddr) -> Result<Client, WsError> {
        let stream = TcpStream::connect(addr).await.map_err(WsError::Io)?;
        let url = format!("ws://{addr}/showdown/websocket");
        tokio_tungstenite::client_async(url, stream).await.map(|(ws, _)| ws)
    }

    /// The next text message, within a second.
    async fn next(ws: &mut Client) -> Option<String> {
        loop {
            match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => return Some(t.as_str().to_owned()),
                Ok(Some(Ok(_))) => continue,
                _ => return None,
            }
        }
    }

    /// Reads until the challstr (the connection's greeting).
    async fn greeted(ws: &mut Client) -> bool {
        for _ in 0..3 {
            if next(ws).await.is_some_and(|m| m.starts_with("|challstr|")) {
                return true;
            }
        }
        false
    }

    async fn start(limits: Limits, failures: usize) -> SocketAddr {
        let inner = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = inner.local_addr().unwrap();
        tokio::spawn(serve_from(Flaky { inner, failures }, config(), limits));
        addr
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accept_errors_do_not_stop_the_server() {
        let addr = start(Limits::default(), 3).await;
        let mut ws = client(addr).await.unwrap();
        assert!(greeted(&mut ws).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connections_are_capped_and_handshakes_time_out() {
        let limits = Limits { max_connections: 2, handshake_timeout: Duration::from_secs(1), ..Limits::default() };
        let addr = start(limits, 0).await;
        // A socket that never says anything is closed after the timeout.
        let idle = TcpStream::connect(addr).await.unwrap();
        let mut ws = client(addr).await.unwrap();
        assert!(greeted(&mut ws).await);
        // Two open: a third is refused.
        assert!(client(addr).await.is_err());
        let closed = async {
            let mut buf = [0u8; 16];
            loop {
                let _ = idle.readable().await;
                match idle.try_read(&mut buf) {
                    Ok(0) => return true,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => return true,
                    Ok(_) => return false,
                }
            }
        };
        assert!(tokio::time::timeout(Duration::from_secs(3), closed).await.unwrap_or(false), "the idle socket closes");
        drop(idle);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut again = client(addr).await.unwrap();
        assert!(greeted(&mut again).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn messages_are_limited_in_size() {
        let addr = start(Limits::default(), 0).await;
        let mut ws = client(addr).await.unwrap();
        assert!(greeted(&mut ws).await);
        // Over Showdown's 100 KB: a popup, and the connection stays.
        ws.send(Message::text(format!("|/utm {}", "x".repeat(110 * 1024)))).await.unwrap();
        assert_eq!(next(&mut ws).await.as_deref(), Some("|popup|Your message must be below 100KB"));
        ws.send(Message::text("|/cmd rooms")).await.unwrap();
        assert_eq!(next(&mut ws).await.as_deref(), Some("|queryresponse|rooms|null"));
        // Over the websocket limit: the connection closes.
        let huge = "x".repeat(Limits::default().max_message_bytes + 1);
        let _ = ws.send(Message::text(format!("|{huge}"))).await;
        assert_eq!(next(&mut ws).await, None);
    }

    fn outbox(total: &Arc<AtomicUsize>, limit: usize, total_limit: usize) -> (Outbox, mpsc::UnboundedReceiver<String>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let queued = Arc::new(Queued::new(total.clone()));
        (Outbox { sender, queued, kill: Arc::new(Notify::new()), limit, total_limit }, receiver)
    }

    #[test]
    fn a_client_that_does_not_read_is_dropped() {
        let total = Arc::new(AtomicUsize::new(0));
        let (outbox, mut receiver) = outbox(&total, 10, usize::MAX);
        assert!(outbox.push("12345".into()));
        assert!(outbox.push("12345".into()));
        assert!(!outbox.push("1".into()), "over the budget");
        // The writer sends one: room for it again.
        let sent = receiver.try_recv().unwrap();
        outbox.queued.sub(sent.len());
        assert!(outbox.push("12345".into()));
    }

    /// Past the server-wide budget, a client is refused anything that
    /// would leave more than [`KEEPING_UP`] waiting; a client below that
    /// is still served.
    #[test]
    fn the_total_budget_bounds_what_waits() {
        let total = Arc::new(AtomicUsize::new(0));
        let (slow, _slow_rx) = outbox(&total, 1 << 20, 2 * KEEPING_UP);
        let (fast, _fast_rx) = outbox(&total, 1 << 20, 2 * KEEPING_UP);
        assert!(slow.push("x".repeat(KEEPING_UP + 1)), "under both budgets");
        assert!(fast.push("y".repeat(KEEPING_UP - 1)), "under both budgets");
        assert_eq!(total.load(Ordering::Relaxed), 2 * KEEPING_UP);
        assert!(!slow.push("z".into()), "behind, with the total at its budget");
        assert!(fast.push("z".into()), "keeping up: served past the total");
        assert!(!fast.push("z".into()), "but not past KEEPING_UP");
        // One large message is refused too, however little was waiting.
        let (fresh, _fresh_rx) = outbox(&total, 1 << 20, 2 * KEEPING_UP);
        assert!(!fresh.push("w".repeat(KEEPING_UP + 1)), "a single push past KEEPING_UP");
        assert_eq!(total.load(Ordering::Relaxed), 2 * KEEPING_UP + 1);
    }

    /// What was still waiting when a connection ends leaves the total:
    /// a killed client, a writer that stopped, a send to a closed channel.
    #[test]
    fn a_closed_connection_gives_its_bytes_back() {
        let total = Arc::new(AtomicUsize::new(0));
        let (a, a_rx) = outbox(&total, 1 << 20, usize::MAX);
        let writer_side = a.queued.clone();
        assert!(a.push("x".repeat(1000)));
        assert!(a.push("y".repeat(24)));
        // The writer is aborted with both messages unsent, and the hub
        // drops the outbox.
        drop(a_rx);
        drop(writer_side);
        drop(a);
        assert_eq!(total.load(Ordering::Relaxed), 0);
        // A push to a connection whose writer is gone isn't counted.
        let (b, b_rx) = outbox(&total, 1 << 20, usize::MAX);
        drop(b_rx);
        assert!(!b.push("z".repeat(10)));
        assert_eq!(total.load(Ordering::Relaxed), 0);
    }
}
