//! A Veil node's QUIC endpoint and the links it opens.
//!
//! One UDP socket both listens and dials. That is what makes hole punching work: when two
//! nodes behind NAT dial each other at the same moment (timed by a rendezvous message), each
//! outgoing packet opens the mapping the other side's packet needs.
//!
//! Dialling tries every candidate address at once and keeps the first link that completes.
//!
//! Media paths: each video frame travels on its own unidirectional stream (a 4K keyframe sent
//! as a burst of datagrams lost about 15% of its fragments even on localhost). Audio frames
//! are small and loss-tolerant, so they travel as single datagrams.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use quinn::{IdleTimeout, TransportConfig};
use rustls::pki_types::CertificateDer;
use rustls::sign::CertifiedKey;
use tokio::task::JoinSet;
use veil_security::{Identity, PublicId};

use crate::{NetError, control, tls};

/// Largest sealed video frame: the security layer's frame limit plus its header and tag.
pub const MAX_SEALED_FRAME: usize = veil_security::frame::MAX_FRAME_LEN
    + veil_security::frame::HEADER_LEN
    + veil_security::frame::TAG_LEN;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const KEEP_ALIVE: Duration = Duration::from_secs(4);
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const DATAGRAM_BUFFER: usize = 16 * 1024 * 1024;
const INITIAL_MTU: u16 = 1_400;
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
    // Start above QUIC's 1,200-byte floor so a relayed inner packet (1,200 bytes plus a
    // 4-byte session header) fits one outer datagram. If the path is smaller, quinn's
    // black-hole detection falls back and the relay uses its stream path instead.
    t.initial_mtu(initial_mtu);
    // shortcut: quinn's default loss-based congestion control. Real-time video wants a
    // delay-based controller; replace once media runs over these links.
    Ok(Arc::new(t))
}

/// A bound endpoint for one device identity. Must be created inside a Tokio runtime.
pub struct Node {
    endpoint: quinn::Endpoint,
    key: Arc<CertifiedKey>,
    id: PublicId,
    mtu: u16,
}

impl Node {
    pub fn bind(identity: &Identity, addr: SocketAddr) -> Result<Self, NetError> {
        let key = tls::certified_key(identity)?;
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(tls::server_config(key.clone())?));
        server.transport_config(transport(INITIAL_MTU)?);
        let endpoint = quinn::Endpoint::server(server, addr)?;
        Ok(Self {
            endpoint,
            key,
            id: identity.public(),
            mtu: INITIAL_MTU,
        })
    }

    /// Binds to a virtual socket instead of UDP. Used for relayed links: the virtual socket
    /// carries this node's QUIC packets through a relay, so the link stays end-to-end
    /// encrypted and pinned exactly as a direct one.
    pub fn bind_virtual(
        identity: &Identity,
        socket: Arc<dyn quinn::AsyncUdpSocket>,
    ) -> Result<Self, NetError> {
        let key = tls::certified_key(identity)?;
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(tls::server_config(key.clone())?));
        server.transport_config(transport(RELAYED_MTU)?);
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            Some(server),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        Ok(Self {
            endpoint,
            key,
            id: identity.public(),
            mtu: RELAYED_MTU,
        })
    }

    pub fn id(&self) -> PublicId {
        self.id
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Dials every address at once and returns the first link that proves it holds `expected`.
    pub async fn connect(&self, addrs: &[SocketAddr], expected: PublicId) -> Result<Link, NetError> {
        if addrs.is_empty() {
            return Err(NetError::NoAddresses);
        }
        let mut config = quinn::ClientConfig::new(Arc::new(tls::client_config(
            self.key.clone(),
            expected,
        )?));
        config.transport_config(transport(self.mtu)?);

        let mut attempts = JoinSet::new();
        let mut last_error = String::from("no attempt started");
        for addr in addrs.iter().take(MAX_CANDIDATES) {
            match self.endpoint.connect_with(config.clone(), *addr, SERVER_NAME) {
                Ok(connecting) => {
                    attempts.spawn(async move {
                        tokio::time::timeout(CONNECT_TIMEOUT, connecting).await
                    });
                }
                Err(e) => last_error = e.to_string(),
            }
        }
        while let Some(result) = attempts.join_next().await {
            match result {
                Ok(Ok(Ok(conn))) => {
                    let link = Link::new(conn)?;
                    if link.peer != expected {
                        // The verifier already enforces this; checked again so a future
                        // verifier change cannot silently drop the pin.
                        return Err(NetError::Identity);
                    }
                    return Ok(link); // dropping `attempts` aborts the rest
                }
                Ok(Ok(Err(e))) => last_error = e.to_string(),
                Ok(Err(_)) => last_error = NetError::Timeout.to_string(),
                Err(e) => last_error = e.to_string(),
            }
        }
        Err(NetError::Connect(last_error))
    }

    /// Waits for the next incoming link. `None` once the endpoint is closed.
    pub async fn accept(&self) -> Option<Result<Link, NetError>> {
        let incoming = self.endpoint.accept().await?;
        Some(match incoming.await {
            Ok(conn) => Link::new(conn),
            Err(e) => Err(NetError::Connect(e.to_string())),
        })
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
    fn new(conn: quinn::Connection) -> Result<Self, NetError> {
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

    pub fn remote_addr(&self) -> SocketAddr {
        self.conn.remote_address()
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
    /// Stream format: frame id (u32), length (u32), both big endian, then the sealed frame.
    pub async fn send_frame(&self, frame_id: u32, sealed: &[u8]) -> Result<(), NetError> {
        if sealed.len() > MAX_SEALED_FRAME {
            return Err(NetError::TooLarge);
        }
        let len = u32::try_from(sealed.len()).map_err(|_| NetError::TooLarge)?;
        let mut stream = self
            .conn
            .open_uni()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        let mut head = [0u8; 8];
        head[..4].copy_from_slice(&frame_id.to_be_bytes());
        head[4..].copy_from_slice(&len.to_be_bytes());
        let closed = |e: quinn::WriteError| NetError::Closed(e.to_string());
        stream.write_all(&head).await.map_err(closed)?;
        stream.write_all(sealed).await.map_err(closed)?;
        stream.finish().map_err(|e| NetError::Closed(e.to_string()))
    }

    /// Receives the next video frame as `(frame_id, sealed)`.
    // shortcut: frames are read one stream at a time, so one stalled frame delays the next on
    // the receiver. Upgrade to concurrent readers that reset stale streams with the media
    // pipeline.
    pub async fn recv_frame(&self) -> Result<(u32, Vec<u8>), NetError> {
        let mut stream = self
            .conn
            .accept_uni()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        let closed = |e: quinn::ReadExactError| NetError::Closed(e.to_string());
        let mut head = [0u8; 8];
        stream.read_exact(&mut head).await.map_err(closed)?;
        let frame_id = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let len = usize::try_from(u32::from_be_bytes([head[4], head[5], head[6], head[7]]))
            .map_err(|_| NetError::TooLarge)?;
        if len > MAX_SEALED_FRAME {
            return Err(NetError::TooLarge);
        }
        let mut sealed = vec![0u8; len];
        stream.read_exact(&mut sealed).await.map_err(closed)?;
        Ok((frame_id, sealed))
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
