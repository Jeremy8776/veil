//! A virtual UDP socket that carries a node's QUIC packets through a relay session.
//!
//! A relayed link is a normal Veil QUIC link whose packets ride inside the outer link to the
//! relay. The inner link is pinned and end-to-end encrypted between the two members; the
//! relay sees only the session id and ciphertext.
//!
//! Each inner packet goes out as one outer datagram when it fits, otherwise on a one-packet
//! outer stream. Incoming packets are fed in by [`crate::RendezvousClient`].

use std::io::{self, IoSliceMut};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use bytes::Bytes;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::sync::mpsc;

use crate::protocol::{session_packet, stream_header};

/// The address the relayed peer appears at. TEST-NET-1, never routed.
pub const RELAY_PEER: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 1));
const RELAY_LOCAL: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 1));

#[derive(Debug)]
pub struct RelaySocket {
    outer: quinn::Connection,
    session: u32,
    inbox: Mutex<mpsc::Receiver<Bytes>>,
}

impl RelaySocket {
    pub(crate) fn new(outer: quinn::Connection, session: u32, inbox: mpsc::Receiver<Bytes>) -> Self {
        Self {
            outer,
            session,
            inbox: Mutex::new(inbox),
        }
    }
}

/// The outer link buffers datagrams and streams itself, so the socket is always writable.
#[derive(Debug)]
struct AlwaysWritable;

impl UdpPoller for AlwaysWritable {
    fn poll_writable(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncUdpSocket for RelaySocket {
    fn create_io_poller(self: std::sync::Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(AlwaysWritable)
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        let packet = session_packet(self.session, transmit.contents);
        let fits = self
            .outer
            .max_datagram_size()
            .is_some_and(|max| packet.len() <= max);
        if fits {
            return self
                .outer
                .send_datagram(Bytes::from(packet))
                .map_err(io::Error::other);
        }
        // Too big for one outer datagram: send on its own stream. QUIC tolerates reordering.
        let Some(head) = stream_header(self.session, transmit.contents.len()) else {
            return Ok(()); // over 64 KiB: drop, as a real network would
        };
        let outer = self.outer.clone();
        let body = transmit.contents.to_vec();
        tokio::spawn(async move {
            if let Ok(mut s) = outer.open_uni().await
                && s.write_all(&head).await.is_ok()
                && s.write_all(&body).await.is_ok()
            {
                let _ = s.finish();
            }
        });
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let Ok(mut inbox) = self.inbox.lock() else {
            return Poll::Ready(Err(io::Error::other("relay inbox lock poisoned")));
        };
        match inbox.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "relay closed",
            ))),
            Poll::Ready(Some(packet)) => {
                let (Some(buf), Some(m)) = (bufs.first_mut(), meta.first_mut()) else {
                    return Poll::Ready(Ok(0));
                };
                let n = packet.len().min(buf.len());
                buf[..n].copy_from_slice(&packet[..n]);
                *m = RecvMeta {
                    addr: RELAY_PEER,
                    len: n,
                    stride: n,
                    ..RecvMeta::default()
                };
                Poll::Ready(Ok(1))
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(RELAY_LOCAL)
    }

    /// Reported as true so quinn skips path MTU discovery: inner packets stay at 1,200 bytes
    /// and fit one outer datagram.
    fn may_fragment(&self) -> bool {
        true
    }
}
