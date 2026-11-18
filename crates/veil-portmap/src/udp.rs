//! One request and its answer over UDP, with the retries PCP and NAT-PMP expect.
//!
//! Invariants:
//! - The socket is connected to the server, so the system drops datagrams from anyone else.
//! - A datagram the caller's `accept` does not recognise (wrong opcode, nonce or port) is
//!   ignored, not taken as the answer, and the wait goes on.
//! - The whole exchange is bounded: [`WAITS`] in total, about 1.75 s.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};

/// How long to wait after each send. RFC 6887 starts at 3 s; a router on the local network
/// answers in milliseconds, and the call should not wait long for one that never will.
const WAITS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(1000),
];

/// Largest datagram either protocol sends back (PCP allows up to 1100 bytes).
const MAX_ANSWER: usize = 1100;

/// Sends `request` from `local` to `server` until `accept` takes an answer, or the waits run
/// out (`Ok(None)`).
pub async fn exchange<T>(
    local: impl Into<IpAddr>,
    server: impl Into<SocketAddr>,
    request: &[u8],
    mut accept: impl FnMut(&[u8]) -> Option<T>,
) -> Result<Option<T>, String> {
    let server: SocketAddr = server.into();
    let socket = UdpSocket::bind(SocketAddr::new(local.into(), 0))
        .await
        .map_err(|e| format!("cannot open a socket: {e}"))?;
    socket
        .connect(server)
        .await
        .map_err(|e| format!("cannot reach {server}: {e}"))?;
    let mut buf = [0u8; MAX_ANSWER];
    for wait in WAITS {
        socket
            .send(request)
            .await
            .map_err(|e| format!("cannot send to {server}: {e}"))?;
        let deadline = Instant::now() + wait;
        loop {
            match timeout_at(deadline, socket.recv(&mut buf)).await {
                Err(_) => break,
                // Connection refused (an ICMP port unreachable) and the like: nobody listens.
                Ok(Err(_)) => return Ok(None),
                Ok(Ok(n)) => {
                    if let Some(answer) = buf.get(..n).and_then(&mut accept) {
                        return Ok(Some(answer));
                    }
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retries_until_a_recognised_answer() -> Result<(), String> {
        let server = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        let SocketAddr::V4(addr) = server.local_addr().map_err(|e| e.to_string())? else {
            return Err("not IPv4".into());
        };
        tokio::spawn(async move {
            let mut buf = [0u8; 16];
            // Ignore the first request, answer the second with junk then the real answer.
            for round in 0..2 {
                let Ok((_, from)) = server.recv_from(&mut buf).await else {
                    return;
                };
                if round == 1 {
                    let _ = server.send_to(b"junk", from).await;
                    let _ = server.send_to(b"ok", from).await;
                }
            }
        });
        let answer = exchange(std::net::Ipv4Addr::LOCALHOST, addr, b"hi", |d| {
            (d == b"ok").then_some(7)
        })
        .await?;
        assert_eq!(answer, Some(7));
        Ok(())
    }
}
