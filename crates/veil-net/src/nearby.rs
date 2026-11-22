//! Opt-in LAN discovery. Beacons contain only a fresh service key and port, never an invite,
//! name, room identifier or permanent device identity. A person explicitly chooses a beacon;
//! its full signed invitation is fetched through a pinned, local-only QUIC connection.
//!
//! The invitation names the host's permanent key, its addresses and a join token, so it is
//! handed out only with the [`Code`] shown on the host's screen. Someone on the network who
//! cannot see that screen learns nothing past the beacon. Guessing is bounded: one attempt
//! per source per [`PER_SOURCE_GAP`] and [`MAX_FETCHES`] attempts in all, so a stranger has
//! at most 64 tries at a million codes. Spending those tries can still stop nearby
//! discovery for this call; the full link and QR keep working.
//!
//! This is a small Veil multicast protocol, not mDNS. IPv4 interfaces are joined separately,
//! so discovery does not need a default route. Multicast may be disabled by the LAN; full
//! invitations and QR remain independent alternatives.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use veil_security::invite::{Invite, MAX_LINK_LEN};
use veil_security::{Identity, PublicId};

use crate::{NetError, Node, local};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 77, 71);
const PORT: u16 = 56711;
const MAGIC: &[u8] = b"VEIL-NEARBY\x01";
const BEACON_LEN: usize = MAGIC.len() + 32 + 2;
const INTERVAL: Duration = Duration::from_secs(2);
const FETCH_WAIT: Duration = Duration::from_secs(10);
const SERVICE_WAIT: Duration = Duration::from_secs(3);
pub const EXPIRY: Duration = Duration::from_secs(8);
/// One invitation per source address in this window.
const PER_SOURCE_GAP: Duration = Duration::from_secs(10);
/// Invitations one advertiser hands out in its whole life. A call needing more is unusual;
/// the host can still share the full link or QR.
const MAX_FETCHES: usize = 64;
/// Source addresses remembered for [`PER_SOURCE_GAP`]. Past this, new sources wait.
const MAX_SOURCES: usize = 256;
/// Most beacons a scan remembers.
const MAX_KNOWN: usize = 64;

/// Untrusted nearby service. Its key authenticates the fetch, not the person hosting a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub key: PublicId,
    pub addr: SocketAddr,
}

impl Found {
    /// A temporary label. It identifies this call announcement, never a verified person.
    pub fn label(&self) -> String {
        self.key.0[..4]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn beacon(key: PublicId, port: u16) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(BEACON_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&key.0);
    bytes.extend_from_slice(&port.to_be_bytes());
    bytes
}

fn decode(bytes: &[u8], source: SocketAddr) -> Option<Found> {
    if bytes.len() != BEACON_LEN || !bytes.starts_with(MAGIC) {
        return None;
    }
    let key: [u8; 32] = bytes.get(MAGIC.len()..MAGIC.len() + 32)?.try_into().ok()?;
    let port = u16::from_be_bytes(bytes.get(MAGIC.len() + 32..)?.try_into().ok()?);
    if port == 0 {
        return None;
    }
    Some(Found {
        key: PublicId(key),
        addr: SocketAddr::new(source.ip(), port),
    })
}

fn multicast_socket() -> Result<UdpSocket, NetError> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    let mut joined = false;
    for ip in local::ipv4_interfaces() {
        joined |= socket.join_multicast_v4(&GROUP, &ip).is_ok();
    }
    if !joined {
        return Err(NetError::Config(
            "No IPv4 LAN interface is available for nearby discovery".into(),
        ));
    }
    socket.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(socket.into())?)
}

/// Listen only: browsing sends no beacon or public request. Dropping stops the listener.
pub struct Scanner {
    found: mpsc::Receiver<Found>,
    task: JoinHandle<()>,
}

impl Scanner {
    pub fn start() -> Result<Self, NetError> {
        let mut socket = multicast_socket()?;
        let mut interfaces = local::ipv4_interfaces();
        let (tx, found) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            let mut bytes = [0; 256];
            // Process a bounded batch once per second, so hostile beacons cannot trigger
            // unbounded OS enumeration or notifications.
            let mut known = std::collections::HashMap::<SocketAddr, std::time::Instant>::new();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut recent = Vec::<Found>::new();
            loop {
                tokio::select! {
                    packet = socket.recv_from(&mut bytes) => {
                        let Ok((length, source)) = packet else { break };
                        let Some(found) = decode(&bytes[..length], source) else { continue };
                        if recent.len() < 32 && !recent.contains(&found) { recent.push(found); }
                    }
                    _ = tick.tick() => {
                        let now_interfaces = local::ipv4_interfaces();
                        if now_interfaces != interfaces && let Ok(replacement) = multicast_socket() {
                            socket = replacement;
                            interfaces = now_interfaces;
                        }
                        let now = std::time::Instant::now();
                        known.retain(|_, seen| now.duration_since(*seen) < EXPIRY);
                        // At most 32 interface checks and notifications each second.
                        for found in recent.drain(..) {
                            if !local::is_on_link(found.addr) {
                                continue;
                            }
                            // A flood of forged sources must not hide real calls: the oldest
                            // entry makes room rather than the newcomer being refused.
                            if known.len() >= MAX_KNOWN && !known.contains_key(&found.addr)
                                && let Some(oldest) = known.iter().min_by_key(|(_, at)| **at).map(|(a, _)| *a)
                            {
                                known.remove(&oldest);
                            }
                            known.insert(found.addr, now);
                            let _ = tx.try_send(found);
                        }
                    }
                }
            }
        });
        Ok(Self { found, task })
    }
    pub async fn next(&mut self) -> Option<Found> {
        self.found.recv().await
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The six digits the host shows, which the guest types to get the invitation.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code([u8; 6]);

impl Code {
    /// A fresh code from the system's secure random source.
    pub fn generate() -> Result<Self, NetError> {
        let random = rustls::crypto::ring::default_provider().secure_random;
        loop {
            let mut bytes = [0u8; 4];
            random
                .fill(&mut bytes)
                .map_err(|_| NetError::Config("no secure random source".into()))?;
            let n = u32::from_le_bytes(bytes);
            // Rejection keeps every code equally likely.
            if n < 4_294_000_000 {
                return Ok(Self::from_number(n % 1_000_000));
            }
        }
    }

    fn from_number(mut n: u32) -> Self {
        let mut digits = [0u8; 6];
        for digit in digits.iter_mut().rev() {
            *digit = b'0' + (n % 10) as u8;
            n /= 10;
        }
        Self(digits)
    }

    /// Six digits, spaces allowed (as shown: "123 456"). Anything else is not a code.
    pub fn parse(text: &str) -> Option<Self> {
        let digits: Vec<u8> = text.bytes().filter(|b| *b != b' ').collect();
        let digits: [u8; 6] = digits.try_into().ok()?;
        digits.iter().all(u8::is_ascii_digit).then_some(Self(digits))
    }

    /// Equal without an early exit, so timing does not reveal how many digits match.
    fn matches(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = std::str::from_utf8(&self.0).map_err(|_| std::fmt::Error)?;
        write!(f, "{} {}", &text[..3], &text[3..])
    }
}

/// Never printed in logs.
impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Code(..)")
    }
}

#[derive(Serialize, Deserialize)]
enum Request {
    Invite(Code),
}

/// Holds the temporary invitation service and its beacons for the host's call lifetime.
pub struct Advertiser {
    tasks: Vec<JoinHandle<()>>,
    code: Code,
}
impl Drop for Advertiser {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Advertiser {
    /// Call only after the host opted in. The invitation goes only to someone who gives
    /// [`Self::code`]; the host still decides admission. `invite` reads the latest link.
    pub fn start(invite: Arc<dyn Fn() -> Option<String> + Send + Sync>) -> Result<Self, NetError> {
        let code = Code::generate()?;
        let identity = Identity::generate();
        let node = Node::bind_local(&identity, 0)?;
        let bytes = beacon(identity.public(), node.local_addr()?.port());
        let mut interfaces = local::ipv4_interfaces();
        let mut sockets = senders(&interfaces);
        if sockets.is_empty() {
            return Err(NetError::Config(
                "No IPv4 LAN interface is available for nearby discovery".into(),
            ));
        }
        let current = invite.clone();
        let announce = tokio::spawn(async move {
            let mut tick = tokio::time::interval(INTERVAL);
            loop {
                tick.tick().await;
                let now_interfaces = local::ipv4_interfaces();
                if now_interfaces != interfaces {
                    sockets = senders(&now_interfaces);
                    interfaces = now_interfaces;
                }
                // No valid invitation means no open call should be announced.
                if current().is_none() {
                    continue;
                }
                for socket in &sockets {
                    let _ = socket
                        .send_to(&bytes, SocketAddr::from((GROUP, PORT)))
                        .await;
                }
            }
        });
        // A distinct callback clone is supplied below; both callbacks are read-only.
        Ok(Self {
            tasks: vec![announce, serve(node, invite, code)],
            code,
        })
    }

    /// The code to show on the host's screen.
    pub fn code(&self) -> Code {
        self.code
    }
}

fn senders(interfaces: &[Ipv4Addr]) -> Vec<UdpSocket> {
    interfaces
        .iter()
        .filter_map(|ip| {
            let make = || -> std::io::Result<UdpSocket> {
                let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
                socket.bind(&SocketAddr::from((*ip, 0)).into())?;
                socket.set_multicast_if_v4(ip)?;
                socket.set_multicast_ttl_v4(1)?;
                socket.set_nonblocking(true)?;
                UdpSocket::from_std(socket.into())
            };
            make().ok()
        })
        .collect()
}

/// Whether `source` may have an invitation now. Records it if so.
fn admit_source(
    sources: &mut std::collections::HashMap<std::net::IpAddr, std::time::Instant>,
    served: &mut usize,
    source: std::net::IpAddr,
    now: std::time::Instant,
) -> bool {
    sources.retain(|_, at| now.duration_since(*at) < PER_SOURCE_GAP);
    if *served >= MAX_FETCHES || sources.contains_key(&source) || sources.len() >= MAX_SOURCES {
        return false;
    }
    sources.insert(source, now);
    *served += 1;
    true
}

fn serve(
    node: Node,
    invite: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    code: Code,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(8));
        let mut serving = JoinSet::new();
        let mut sources = std::collections::HashMap::new();
        let mut served = 0;
        // One failed accept must not end the service while the beacon and code stay up.
        while let Some(accepted) = node.accept().await {
            let Ok(link) = accepted else { continue };
            while serving.try_join_next().is_some() {}
            let source = link.remote_addr().ip().to_canonical();
            if !admit_source(&mut sources, &mut served, source, std::time::Instant::now()) {
                link.close("nearby service busy");
                continue;
            }
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                link.close("nearby service busy");
                continue;
            };
            let invite = invite.clone();
            serving.spawn(async move {
                let _slot = slot;
                let answer = async {
                    let (mut out, mut input) = link.accept_control().await?.split();
                    let Request::Invite(given) = input.recv::<Request>().await?;
                    // A wrong code gets the same answer as no open call.
                    let full = if given.matches(&code) { invite() } else { None }
                        .filter(|full| full.len() <= MAX_LINK_LEN);
                    out.send(&full).await
                };
                let _ = tokio::time::timeout(SERVICE_WAIT, answer).await;
                // Let the answer arrive, but do not let a silent peer hold the slot long.
                let _ = tokio::time::timeout(Duration::from_secs(1), link.quic().closed()).await;
                link.close("nearby invitation complete");
            });
        }
    })
}

/// Explicit user action only. A throwaway identity fetches locally; the signed invitation
/// is validated before it can be used to join. No public endpoint or DNS is consulted.
pub async fn fetch(found: &Found, code: Code, now_unix: u64) -> Result<Invite, NetError> {
    let fetch = async {
        let node = Node::bind_local(&Identity::generate(), 0)?;
        let link = node.connect(&[found.addr], found.key).await?;
        let (mut out, mut input) = link.open_control().await?.split();
        out.send(&Request::Invite(code)).await?;
        let full = input
            .recv::<Option<String>>()
            .await?
            .filter(|full| full.len() <= MAX_LINK_LEN)
            .ok_or(NetError::Protocol(
                "wrong code, or the call is no longer open",
            ))?;
        let invite = Invite::from_link(&full, now_unix)
            .map_err(|_| NetError::Protocol("nearby invitation is invalid or expired"))?;
        link.close("nearby invitation received");
        Ok(invite)
    };
    tokio::time::timeout(FETCH_WAIT, fetch)
        .await
        .map_err(|_| NetError::Timeout)?
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn announcement_is_fixed_size_and_contains_no_invitation() {
        let key = Identity::generate().public();
        let bytes = beacon(key, 7000);
        assert_eq!(bytes.len(), BEACON_LEN);
        let found = decode(&bytes, "192.168.1.2:9000".parse().unwrap()).unwrap();
        assert_eq!(found.key, key);
        assert_eq!(found.addr, "192.168.1.2:7000".parse().unwrap());
        assert_eq!(found.label().len(), 8);
        assert!(decode(&beacon(key, 0), found.addr).is_none());
        assert!(decode(&bytes[..bytes.len() - 1], found.addr).is_none());
        let mut appended = bytes;
        appended.push(1);
        assert!(decode(&appended, found.addr).is_none());
    }

    #[test]
    fn codes_are_six_digits_and_compare_exactly() {
        let code = Code::generate().unwrap();
        let shown = code.to_string();
        assert_eq!(shown.len(), 7, "shown as 123 456");
        assert_eq!(Code::parse(&shown), Some(code));
        assert_eq!(Code::from_number(42).to_string(), "000 042");
        for bad in ["12345", "1234567", "12a456", ""] {
            assert!(Code::parse(bad).is_none(), "{bad}");
        }
        let a = Code::parse("111111").unwrap();
        assert!(a.matches(&a) && !a.matches(&Code::parse("111112").unwrap()));
        assert_eq!(format!("{code:?}"), "Code(..)", "never logged");
    }

    #[test]
    fn each_source_gets_one_invitation_per_window_and_the_total_is_capped() {
        let mut sources = std::collections::HashMap::new();
        let mut served = 0;
        let now = std::time::Instant::now();
        let a: std::net::IpAddr = "192.168.1.5".parse().unwrap();
        let b: std::net::IpAddr = "192.168.1.6".parse().unwrap();
        assert!(admit_source(&mut sources, &mut served, a, now));
        assert!(!admit_source(&mut sources, &mut served, a, now), "same source waits");
        assert!(admit_source(&mut sources, &mut served, b, now));
        let later = now + PER_SOURCE_GAP;
        assert!(admit_source(&mut sources, &mut served, a, later), "the window passes");
        served = MAX_FETCHES;
        let c: std::net::IpAddr = "192.168.1.7".parse().unwrap();
        assert!(!admit_source(&mut sources, &mut served, c, later), "lifetime cap");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_fetch_gets_a_signed_invitation_through_a_temporary_service() {
        let service = Identity::generate();
        let host = Identity::generate();
        let now = 1_800_000_000;
        let invite = Invite {
            version: 1,
            room_id: [1; 16],
            host: host.public(),
            addrs: vec!["127.0.0.1:7000".into()],
            relay: None,
            expires_unix: now + 60,
            join_token: [2; 16],
        };
        let full = invite.to_link(&host).unwrap();
        let node = Node::bind_local(&service, 0).unwrap();
        let found = Found {
            key: service.public(),
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, node.local_addr().unwrap().port())),
        };
        let code = Code::parse("123 456").unwrap();
        let serving = serve(node, Arc::new(move || Some(full.clone())), code);
        let wrong = Code::parse("123457").unwrap();
        assert!(fetch(&found, wrong, now).await.is_err(), "a wrong code gets nothing");
        // One attempt per source per window, so the right code now has to wait as well.
        assert!(fetch(&found, code, now).await.is_err());
        serving.abort();
        let node = Node::bind_local(&service, 0).unwrap();
        let found = Found {
            key: service.public(),
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, node.local_addr().unwrap().port())),
        };
        let full = invite.to_link(&host).unwrap();
        let serving = serve(node, Arc::new(move || Some(full.clone())), code);
        let fetched = fetch(&found, code, now).await.unwrap();
        assert_eq!(fetched, invite);
        assert_ne!(
            found.key, fetched.host,
            "beacon never uses the permanent host key"
        );
        serving.abort();
    }
}
