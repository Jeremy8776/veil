//! A Veil node's QUIC endpoint and the links it opens.
//!
//! One UDP socket both listens and dials. That is what makes hole punching work: when two
//! nodes behind NAT dial each other at the same moment (timed by a rendezvous message), each
//! outgoing packet opens the mapping the other side's packet needs.
//!
//! Dialling tries every candidate address and keeps the first link that completes. IPv6
//! candidates start first and IPv4 ones a moment later (`family::dial_order`), so IPv6 wins
//! when both ends have it.
//!
//! A node bound to `[::]` is dual-stack on every system (`family::dual_stack`): one socket
//! for both families, as hole punching needs. [`Node::bind_any`] binds that way and falls
//! back to IPv4 where the system has no IPv6.
//!
//! Media paths: each video frame travels on its own unidirectional stream (a 4K keyframe sent
//! as a burst of datagrams lost about 15% of its fragments even on localhost). Audio frames
//! are small and loss-tolerant, so they travel as single datagrams.
//!
//! A listener shows its device key only to diallers that already know it (`knock.rs`), so a
//! port scan finds no key to follow across calls. [`Node::admit_strangers`] opens a public
//! service to everyone.
//!
//! Invariant: what a peer can make this node hold on one link without the application reading
//! it is a small constant (`LINK_WINDOW` plus `DATAGRAM_BUFFER`), whatever streams it opens.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use quinn::{IdleTimeout, TransportConfig};
use rustls::pki_types::CertificateDer;
use rustls::sign::CertifiedKey;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use veil_security::{Identity, PublicId};

use crate::{NetError, accept, control, family, tls};

/// Largest sealed video frame: the security layer's frame limit plus its header and tag.
pub const MAX_SEALED_FRAME: usize = veil_security::frame::MAX_FRAME_LEN
    + veil_security::frame::HEADER_LEN
    + veil_security::frame::TAG_LEN;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const KEEP_ALIVE: Duration = Duration::from_secs(4);
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Peer-opened bidirectional streams: every Veil protocol uses one, the control stream.
/// One spare lets a replacement open while the old one is closing.
const MAX_BIDI_STREAMS: u32 = 2;
/// Peer-opened unidirectional streams: video frames (`MAX_FRAMES_IN_FLIGHT` at a time) and
/// the relay's stream path, one stream per packet. More only queue at the sender.
const MAX_UNI_STREAMS: u32 = 64;
/// Bytes one stream may have in flight before the reader takes them. A frame larger than this
/// still crosses: the window moves forward as `FrameReader` reads.
const STREAM_WINDOW: u32 = 1024 * 1024;
/// Bytes a whole link may have in flight before the application reads them, across every
/// stream. This is the most a peer can make this node hold on a link it does not read
/// (GHSA-g7j3-q88p-63cf); quinn's default has no limit. 4 MiB per round trip is about
/// 160 Mbit/s at 200 ms, above a forwarder link carrying a full call's video.
const LINK_WINDOW: u32 = 4 * 1024 * 1024;
/// Datagrams queued each way. Audio and relayed packets are read as they come, and quinn
/// drops the oldest when the queue is full, as real-time media wants.
const DATAGRAM_BUFFER: usize = 1024 * 1024;
/// Mobile networks often carry less than 1,400 bytes per packet, and QUIC finds any larger size
/// by itself, so calls start small rather than losing their first packets.
const INITIAL_MTU: u16 = 1_280;
/// Relayed (inner) links stay at QUIC's minimum so each packet fits one outer datagram.
const RELAYED_MTU: u16 = 1_200;
const MAX_CANDIDATES: usize = 16;
/// TLS server name. Ignored by verification, which pins the device key instead.
const SERVER_NAME: &str = "veil";

fn transport(initial_mtu: u16) -> Result<Arc<TransportConfig>, NetError> {
    let mut t = TransportConfig::default();
    t.keep_alive_interval(Some(KEEP_ALIVE));
    t.max_idle_timeout(Some(
        IdleTimeout::try_from(IDLE_TIMEOUT).map_err(|e| NetError::Config(e.to_string()))?,
    ));
    t.datagram_receive_buffer_size(Some(DATAGRAM_BUFFER));
    t.datagram_send_buffer_size(DATAGRAM_BUFFER);
    // Bounds what a peer can park in streams this node never reads (GHSA-g7j3-q88p-63cf).
    t.max_concurrent_bidi_streams(MAX_BIDI_STREAMS.into());
    t.max_concurrent_uni_streams(MAX_UNI_STREAMS.into());
    t.stream_receive_window(STREAM_WINDOW.into());
    t.receive_window(LINK_WINDOW.into());
    // Start above QUIC's 1,200-byte floor so a relayed inner packet (1,200 bytes plus a
    // 4-byte session header and the outer link's own overhead) fits one outer datagram. If the path is smaller, quinn's
    // black-hole detection falls back and the relay uses its stream path instead.
    t.initial_mtu(initial_mtu);
    // shortcut: quinn's default loss-based congestion control. Real-time video wants a
    // delay-based controller; replace once media runs over these links.
    Ok(Arc::new(t))
}

/// A bound endpoint for one device identity. Must be created inside a Tokio runtime.
/// Incoming handshakes run on their own tasks, with limits (`accept.rs`).
pub struct Node {
    known_paths: Mutex<crate::known_paths::KnownPaths>,
    endpoint: quinn::Endpoint,
    key: Arc<CertifiedKey>,
    id: PublicId,
    mtu: u16,
    local_policy: Arc<crate::local::Policy>,
    /// Whether the listener answers diallers that sent no knock (`knock.rs`).
    open: Arc<AtomicBool>,
    local_refresh: tokio::task::JoinHandle<()>,
    ready: tokio::sync::Mutex<mpsc::Receiver<Link>>,
    acceptor: tokio::task::JoinHandle<()>,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.acceptor.abort();
        self.local_refresh.abort();
    }
}

impl Node {
    pub fn bind(identity: &Identity, addr: SocketAddr) -> Result<Self, NetError> {
        let socket = match addr {
            // Windows and some BSDs make `[::]` IPv6 only; Veil wants both families on it.
            SocketAddr::V6(v6) if v6.ip().is_unspecified() => {
                family::dual_stack(addr.port()).or_else(|_| std::net::UdpSocket::bind(addr))?
            }
            _ => std::net::UdpSocket::bind(addr)?,
        };
        Self::on_socket(identity, socket, false)
    }

    /// Binds `port` (0 for any) for both IPv4 and IPv6 on one socket, or IPv4 only where the
    /// system cannot make a dual-stack socket. For participants, who dial and are dialled
    /// in either family.
    pub fn bind_any(identity: &Identity, port: u16) -> Result<Self, NetError> {
        match family::dual_stack(port) {
            Ok(socket) => Self::on_socket(identity, socket, false),
            Err(_) => Self::bind(identity, SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))),
        }
    }

    /// Binds a Local only endpoint with its packet policy enabled before accepting traffic.
    pub fn bind_local(identity: &Identity, port: u16) -> Result<Self, NetError> {
        let socket = family::dual_stack(port).or_else(|_| {
            std::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        })?;
        Self::on_socket(identity, socket, true)
    }

    fn on_socket(
        identity: &Identity,
        socket: std::net::UdpSocket,
        local_only: bool,
    ) -> Result<Self, NetError> {
        let key = tls::certified_key(identity)?;
        let open = Arc::new(AtomicBool::new(false));
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(tls::server_config(
            key.clone(),
            identity.public(),
            open.clone(),
        )?));
        server.transport_config(transport(INITIAL_MTU)?);
        let local_policy = Arc::new(crate::local::Policy::default());
        local_policy.set(local_only);
        let socket = quinn::Runtime::wrap_udp_socket(&quinn::TokioRuntime, socket)?;
        let socket = Arc::new(crate::local::LocalSocket {
            socket,
            policy: local_policy.clone(),
        });
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(server),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        Ok(Self::new(
            endpoint,
            key,
            identity.public(),
            INITIAL_MTU,
            local_policy,
            open,
        ))
    }

    fn new(
        endpoint: quinn::Endpoint,
        key: Arc<CertifiedKey>,
        id: PublicId,
        mtu: u16,
        local_policy: Arc<crate::local::Policy>,
        open: Arc<AtomicBool>,
    ) -> Self {
        let (ready, acceptor) = accept::start(endpoint.clone());
        let policy = local_policy.clone();
        let local_refresh = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                if policy.enabled() {
                    policy.refresh();
                }
            }
        });
        Self {
            known_paths: Mutex::default(),
            endpoint,
            key,
            id,
            mtu,
            local_policy,
            open,
            local_refresh,
            ready: tokio::sync::Mutex::new(ready),
            acceptor,
        }
    }

    /// Binds to a virtual socket instead of UDP. Used for relayed links: the virtual socket
    /// carries this node's QUIC packets through a relay, so the link stays end-to-end
    /// encrypted and pinned exactly as a direct one.
    pub fn bind_virtual(
        identity: &Identity,
        socket: Arc<dyn quinn::AsyncUdpSocket>,
    ) -> Result<Self, NetError> {
        let local_policy = Arc::new(crate::local::Policy::default());
        let socket = Arc::new(crate::local::LocalSocket {
            socket,
            policy: local_policy.clone(),
        });
        let key = tls::certified_key(identity)?;
        let open = Arc::new(AtomicBool::new(false));
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(tls::server_config(
            key.clone(),
            identity.public(),
            open.clone(),
        )?));
        server.transport_config(transport(RELAYED_MTU)?);
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(server),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        Ok(Self::new(
            endpoint,
            key,
            identity.public(),
            RELAYED_MTU,
            local_policy,
            open,
        ))
    }

    pub fn id(&self) -> PublicId {
        self.id
    }

    /// Lets diallers that do not know this node's key complete a handshake and see it. Only
    /// for public services whose clients may not know the key (a rendezvous serving short
    /// invite links). Call nodes stay closed: a stranger who finds their port gets no key.
    pub fn admit_strangers(&self) {
        self.open.store(true, Ordering::Release);
    }

    /// Restricts this endpoint to loopback and connected non-point-to-point subnets.
    /// Applies to every UDP packet (including incoming handshakes and QUIC migration),
    /// and to verified/unverified dials. Enable before starting any connection.
    pub fn set_local_only(&self, enabled: bool) {
        self.local_policy.set(enabled);
    }

    /// Whether this endpoint is restricted to connected local networks.
    pub fn is_local_only(&self) -> bool {
        self.local_policy.enabled()
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Whether this node can dial and be dialled over IPv6.
    pub fn ipv6(&self) -> bool {
        self.endpoint.local_addr().is_ok_and(|a| a.is_ipv6())
    }

    /// Dials every address, IPv6 first and IPv4 a moment later (`family::dial_order`), and
    /// returns the first link that proves it holds `expected`.
    pub async fn connect(
        &self,
        addrs: &[SocketAddr],
        expected: PublicId,
    ) -> Result<Link, NetError> {
        let rejected = Arc::new(AtomicBool::new(false));
        let crypto = tls::client_config(self.key.clone(), expected, rejected.clone())?;
        let link = match self.dial(addrs, crypto).await {
            Ok(link) => link,
            Err(_) if rejected.load(Ordering::Relaxed) => return Err(NetError::Identity),
            Err(error) => return Err(error),
        };
        if link.peer != expected {
            // The verifier already enforces this; checked again so a future verifier change
            // cannot silently drop the pin.
            return Err(NetError::Identity);
        }
        self.remember(&link);
        Ok(link)
    }

    /// Dials without knowing the peer's key: whoever answers at `addrs` is accepted. Only for
    /// fetching something that proves itself, such as a sealed invite, and from a node with a
    /// throwaway identity, since the peer sees this node's key. Everything else uses
    /// [`Self::connect`].
    pub async fn connect_unverified(&self, addrs: &[SocketAddr]) -> Result<Link, NetError> {
        self.dial(addrs, tls::unverified_client_config(self.key.clone())?)
            .await
    }

    async fn dial(
        &self,
        addrs: &[SocketAddr],
        crypto: quinn::crypto::rustls::QuicClientConfig,
    ) -> Result<Link, NetError> {
        if self.local_policy.enabled() {
            self.local_policy.refresh();
        }
        let addrs: Vec<_> = addrs
            .iter()
            .copied()
            .flat_map(crate::local::scoped_candidates)
            .filter(|addr| self.local_policy.allows(*addr))
            .collect();
        if addrs.is_empty() {
            return Err(NetError::NoAddresses);
        }
        let mut config = quinn::ClientConfig::new(Arc::new(crypto));
        config.transport_config(transport(self.mtu)?);

        let mut attempts = JoinSet::new();
        let mut last_error = String::new();
        for (addr, wait) in family::dial_order(&addrs, self.ipv6(), MAX_CANDIDATES) {
            let endpoint = self.endpoint.clone();
            let config = config.clone();
            attempts.spawn(async move {
                tokio::time::sleep(wait).await;
                let connecting = endpoint
                    .connect_with(config, addr, SERVER_NAME)
                    .map_err(|e| e.to_string())?;
                match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
                    Ok(Ok(conn)) => Ok(conn),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(_) => Err(NetError::Timeout.to_string()),
                }
            });
        }
        if attempts.is_empty() {
            return Err(NetError::Connect(String::from(
                "no address in a family this node can reach",
            )));
        }
        while let Some(result) = attempts.join_next().await {
            match result {
                // Dropping `attempts` aborts the rest.
                Ok(Ok(conn)) => return Link::new(conn),
                Ok(Err(e)) => last_error = e,
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(NetError::Connect(last_error))
    }

    /// Waits for the next link whose handshake finished. `None` once the endpoint is closed.
    /// Failed handshakes are dropped by the accept task, so every item is `Ok`.
    pub async fn accept(&self) -> Option<Result<Link, NetError>> {
        let link = self.ready.lock().await.recv().await?;
        self.remember(&link);
        Some(Ok(link))
    }

    fn remember(&self, link: &Link) {
        if let Ok(mut paths) = self.known_paths.lock() {
            paths.record(link.peer(), link.remote_addr(), std::time::Instant::now());
        }
    }

    /// Whether this exact endpoint authenticated as `peer` on THIS node in the last 60s.
    /// Used to constrain automatic helper notices before any dial sends a packet. It is
    /// bounded historical evidence, not membership, current reachability or device trust.
    /// Unverified outgoing connections never populate it; relayed virtual nodes are separate.
    pub fn authenticated_at(&self, peer: PublicId, addr: SocketAddr) -> bool {
        self.known_paths
            .lock()
            .is_ok_and(|paths| paths.contains(peer, addr, std::time::Instant::now()))
    }

    /// Takes no more incoming links, and closes any not yet returned by [`Self::accept`]. Links
    /// already accepted, and dialling, go on working. `accept` then returns `None`.
    pub async fn stop_accepting(&self) {
        self.acceptor.abort();
        self.endpoint.set_server_config(None);
        let mut ready = self.ready.lock().await;
        ready.close();
        while let Ok(link) = ready.try_recv() {
            link.close("not accepting");
        }
    }

    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"bye");
    }
}

/// An authenticated QUIC connection to one peer.
pub struct Link {
    conn: quinn::Connection,
    peer: PublicId,
}

impl Link {
    pub(crate) fn new(conn: quinn::Connection) -> Result<Self, NetError> {
        let peer = conn
            .peer_identity()
            .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok())
            .and_then(|certs| certs.first().and_then(|c| tls::parse_spki(c)))
            .ok_or(NetError::Identity)?;
        Ok(Self { conn, peer })
    }

    /// The peer's device key, proven in the handshake.
    pub fn peer(&self) -> PublicId {
        self.peer
    }

    /// The peer's address. An IPv4 peer on a dual-stack socket shows as plain IPv4, not as
    /// an IPv4-mapped IPv6 address.
    pub fn remote_addr(&self) -> SocketAddr {
        let addr = self.conn.remote_address();
        match addr {
            SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_none() => addr,
            _ => SocketAddr::new(addr.ip().to_canonical(), addr.port()),
        }
    }

    /// Whether the authenticated remote endpoint deliberately closed with this reason.
    /// A timeout or reset does not count as an intentional departure.
    pub fn peer_closed_with(&self, reason: &str) -> bool {
        matches!(self.conn.close_reason(), Some(quinn::ConnectionError::ApplicationClosed(ref close))
            if close.reason.as_ref() == reason.as_bytes())
    }

    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// Opens the control stream (dialler side).
    pub async fn open_control(&self) -> Result<ControlStream, NetError> {
        let (send, recv) = self
            .conn
            .open_bi()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        Ok(ControlStream { send, recv })
    }

    /// Accepts the control stream (listener side).
    pub async fn accept_control(&self) -> Result<ControlStream, NetError> {
        let (send, recv) = self
            .conn
            .accept_bi()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        Ok(ControlStream { send, recv })
    }

    /// Sends one sealed video frame on its own stream: congestion controlled and retransmitted,
    /// with no head-of-line blocking between frames at the transport level.
    ///
    /// Cancelling the future (a timeout, an aborted task) resets the frame's stream, so the
    /// bytes already buffered for it are discarded and the receiver sees one lost frame.
    ///
    /// Stream format: frame id (u32), length (u32), both big endian, then the sealed frame.
    pub async fn send_frame(&self, frame_id: u32, sealed: &[u8]) -> Result<(), NetError> {
        if sealed.len() > MAX_SEALED_FRAME {
            return Err(NetError::TooLarge);
        }
        let len = u32::try_from(sealed.len()).map_err(|_| NetError::TooLarge)?;
        let stream = self
            .conn
            .open_uni()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        let mut stream = ResetOnDrop {
            stream,
            finished: false,
        };
        let mut head = [0u8; 8];
        head[..4].copy_from_slice(&frame_id.to_be_bytes());
        head[4..].copy_from_slice(&len.to_be_bytes());
        let closed = |e: quinn::WriteError| NetError::Closed(e.to_string());
        stream.stream.write_all(&head).await.map_err(closed)?;
        stream.stream.write_all(sealed).await.map_err(closed)?;
        stream.finish()
    }

    /// Receives the next video frame as `(frame_id, sealed)`, one stream at a time. For
    /// tests and simple tools: a peer that never finishes a stream stalls it. Media readers
    /// use [`crate::FrameReader`], which reads frames side by side with a deadline.
    pub async fn recv_frame(&self) -> Result<(u32, Vec<u8>), NetError> {
        let stream = self
            .conn
            .accept_uni()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        crate::frames::read_whole(stream)
            .await
            .ok_or(NetError::Protocol("bad video frame stream"))
    }

    /// Sends one small packet (an audio frame) as a single unreliable datagram.
    pub fn send_datagram(&self, payload: &[u8]) -> Result<(), NetError> {
        let max = self
            .conn
            .max_datagram_size()
            .ok_or(NetError::Protocol("peer does not accept datagrams"))?;
        if payload.len() > max {
            return Err(NetError::TooLarge);
        }
        self.conn
            .send_datagram(Bytes::copy_from_slice(payload))
            .map_err(|e| NetError::Closed(e.to_string()))
    }

    pub async fn recv_datagram(&self) -> Result<Bytes, NetError> {
        self.conn
            .read_datagram()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))
    }

    pub fn close(&self, reason: &str) {
        self.conn.close(0u32.into(), reason.as_bytes());
    }

    /// The underlying QUIC connection, for protocols that need raw datagrams or streams
    /// (the relay). Its peer identity is already proven.
    pub fn quic(&self) -> &quinn::Connection {
        &self.conn
    }
}

/// A frame stream that is reset unless it was finished: quinn's own drop would finish it and
/// keep the buffered bytes queued for a peer that may never read them.
struct ResetOnDrop {
    stream: quinn::SendStream,
    finished: bool,
}

impl ResetOnDrop {
    fn finish(&mut self) -> Result<(), NetError> {
        self.stream
            .finish()
            .map_err(|e| NetError::Closed(e.to_string()))?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for ResetOnDrop {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.stream.reset(quinn::VarInt::from_u32(0));
        }
    }
}

/// The reliable, ordered control channel of a link.
pub struct ControlStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl ControlStream {
    pub async fn send(&mut self, message: &control::Control) -> Result<(), NetError> {
        control::write(&mut self.send, message).await
    }

    pub async fn recv(&mut self) -> Result<control::Control, NetError> {
        control::read(&mut self.recv).await
    }

    /// Splits into halves so one task can write while another reads. Reading inside a
    /// `select!` is not cancel safe; a dedicated reader task is.
    pub fn split(self) -> (FramedSend, FramedRecv) {
        (FramedSend(self.send), FramedRecv(self.recv))
    }
}

/// The sending half of a framed stream, for any message type.
pub struct FramedSend(quinn::SendStream);

impl FramedSend {
    pub async fn send<T: serde::Serialize>(&mut self, message: &T) -> Result<(), NetError> {
        control::write(&mut self.0, message).await
    }
}

/// The receiving half of a framed stream, for any message type.
pub struct FramedRecv(quinn::RecvStream);

impl FramedRecv {
    pub async fn recv<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, NetError> {
        control::read(&mut self.0).await
    }
}
