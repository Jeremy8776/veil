//! Short invite links: where to fetch the full invite, and the secret that opens it.
//!
//! Purpose: a full invite link is a few hundred characters. A short link carries only the
//! addresses of the call's rendezvous node and a random secret. The host seals its full, signed
//! link under that secret and leaves it on the node; a guest fetches it and opens it.
//!
//! Link format: `veil:~` + base64url(counts || IPv4 servers || IPv6 servers || secret). `counts`
//! holds the IPv4 count in its low four bits and the IPv6 count in its high four. A server is
//! its address then its port, big endian. The `~` is outside the base64url alphabet, so a short
//! link can never be read as a full one.
//!
//! Invariants:
//! - The node holds ciphertext and an id derived one way from the secret, so it cannot read an
//!   invite, or work out the link that opens it.
//! - Each secret seals exactly one invite ([`ShortInvite::create`] makes both together), which
//!   is why a fixed nonce is safe.
//! - The sealed text is the full link, so its host signature and expiry are checked as ever
//!   ([`ShortInvite::open`]). A node can withhold an invite but cannot change it.
//! - Links are bounded in size before decoding.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use rand_core::CryptoRng;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::SecurityError;
use crate::frame::TAG_LEN;
use crate::invite::{Invite, MAX_LINK_LEN};

pub const PREFIX: &str = "veil:~";
/// Most rendezvous addresses a short link carries.
pub const MAX_SERVERS: usize = 3;
pub const SECRET_LEN: usize = 16;
pub const ID_LEN: usize = 32;
/// Largest sealed invite: a full link and its tag.
pub const MAX_SEALED: usize = MAX_LINK_LEN + TAG_LEN;

const V4_LEN: usize = 6;
const V6_LEN: usize = 18;
/// The longest short link: three IPv6 servers. Base64url makes 4 characters of 3 bytes.
const MAX_SHORT_LEN: usize = PREFIX.len() + (1 + MAX_SERVERS * V6_LEN + SECRET_LEN).div_ceil(3) * 4;
const SALT: &[u8] = b"veil-short-invite-v1";

/// A sealed invite and the id it is stored under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedInvite {
    pub id: [u8; ID_LEN],
    pub bytes: Vec<u8>,
}

/// What a short link holds.
#[derive(Clone, PartialEq, Eq)]
pub struct ShortInvite {
    servers: Vec<SocketAddr>,
    secret: Zeroizing<[u8; SECRET_LEN]>,
}

// By hand, so the secret never reaches a log.
impl fmt::Debug for ShortInvite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShortInvite")
            .field("servers", &self.servers)
            .finish_non_exhaustive()
    }
}

struct Keys {
    id: [u8; ID_LEN],
    cipher: ChaCha20Poly1305,
}

fn derive(secret: &[u8; SECRET_LEN]) -> Result<Keys, SecurityError> {
    let hk = Hkdf::<Sha256>::new(Some(SALT), secret);
    let mut id = [0u8; ID_LEN];
    hk.expand(b"id", &mut id).map_err(|_| SecurityError::BadKey)?;
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand(b"key", key.as_mut()).map_err(|_| SecurityError::BadKey)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| SecurityError::BadKey)?;
    Ok(Keys { id, cipher })
}

/// One invite per key, so one nonce is enough (see the invariants above).
fn nonce() -> Nonce {
    Nonce::from([0u8; 12])
}

/// The first [`MAX_SERVERS`] different addresses, IPv4 first, as the link orders them.
fn normalise(servers: &[SocketAddr]) -> Vec<SocketAddr> {
    let mut unique: Vec<SocketAddr> = Vec::with_capacity(MAX_SERVERS);
    for server in servers {
        let plain = SocketAddr::new(server.ip().to_canonical(), server.port());
        if unique.len() < MAX_SERVERS && !unique.contains(&plain) {
            unique.push(plain);
        }
    }
    unique.sort_by_key(SocketAddr::is_ipv6);
    unique
}

impl ShortInvite {
    /// Makes a short link for `full_link`, reachable at `servers` (best first), and the sealed
    /// invite to leave there.
    pub fn create<R: CryptoRng + ?Sized>(
        full_link: &str,
        servers: &[SocketAddr],
        rng: &mut R,
    ) -> Result<(Self, SealedInvite), SecurityError> {
        if full_link.len() > MAX_LINK_LEN {
            return Err(SecurityError::TooLarge);
        }
        let servers = normalise(servers);
        if servers.is_empty() {
            return Err(SecurityError::Malformed("no servers"));
        }
        let mut secret = Zeroizing::new([0u8; SECRET_LEN]);
        rng.fill_bytes(secret.as_mut());
        let keys = derive(&secret)?;
        let bytes = keys
            .cipher
            .encrypt(
                &nonce(),
                Payload {
                    msg: full_link.as_bytes(),
                    aad: &keys.id,
                },
            )
            .map_err(|_| SecurityError::Encrypt)?;
        let sealed = SealedInvite { id: keys.id, bytes };
        Ok((Self { servers, secret }, sealed))
    }

    /// The rendezvous addresses to ask, all for the same node.
    pub fn servers(&self) -> &[SocketAddr] {
        &self.servers
    }

    /// The id the sealed invite is stored under.
    pub fn id(&self) -> Result<[u8; ID_LEN], SecurityError> {
        Ok(derive(&self.secret)?.id)
    }

    /// Opens a sealed invite, then verifies the host's signature and the expiry.
    pub fn open(&self, sealed: &[u8], now_unix: u64) -> Result<Invite, SecurityError> {
        if sealed.len() > MAX_SEALED {
            return Err(SecurityError::TooLarge);
        }
        let keys = derive(&self.secret)?;
        let plain = Zeroizing::new(
            keys.cipher
                .decrypt(
                    &nonce(),
                    Payload {
                        msg: sealed,
                        aad: &keys.id,
                    },
                )
                .map_err(|_| SecurityError::Decrypt)?,
        );
        let link = std::str::from_utf8(&plain).map_err(|_| SecurityError::Malformed("text"))?;
        Invite::from_link(link, now_unix)
    }

    pub fn to_link(&self) -> String {
        let v4 = self.servers.iter().filter(|s| s.is_ipv4()).count();
        let v6 = self.servers.len() - v4;
        let mut bytes = Vec::with_capacity(1 + v4 * V4_LEN + v6 * V6_LEN + SECRET_LEN);
        // Both counts are at most MAX_SERVERS, so each fits its four bits.
        bytes.push(((v6 as u8) << 4) | v4 as u8);
        for server in &self.servers {
            match server.ip() {
                IpAddr::V4(ip) => bytes.extend_from_slice(&ip.octets()),
                IpAddr::V6(ip) => bytes.extend_from_slice(&ip.octets()),
            }
            bytes.extend_from_slice(&server.port().to_be_bytes());
        }
        bytes.extend_from_slice(self.secret.as_ref());
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&bytes))
    }

    pub fn from_link(link: &str) -> Result<Self, SecurityError> {
        let link = link.trim();
        if link.len() > MAX_SHORT_LEN {
            return Err(SecurityError::TooLarge);
        }
        let body = link
            .strip_prefix(PREFIX)
            .ok_or(SecurityError::Malformed("prefix"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| SecurityError::Malformed("base64"))?;
        let (&counts, rest) = bytes.split_first().ok_or(SecurityError::Malformed("length"))?;
        let (v4, v6) = (usize::from(counts & 0x0f), usize::from(counts >> 4));
        if v4 + v6 == 0 || v4 + v6 > MAX_SERVERS {
            return Err(SecurityError::Malformed("server count"));
        }
        if rest.len() != v4 * V4_LEN + v6 * V6_LEN + SECRET_LEN {
            return Err(SecurityError::Malformed("length"));
        }
        let (v4_bytes, rest) = rest.split_at(v4 * V4_LEN);
        let (v6_bytes, secret_bytes) = rest.split_at(v6 * V6_LEN);
        let mut servers = Vec::with_capacity(v4 + v6);
        for entry in v4_bytes.as_chunks::<V4_LEN>().0 {
            let (ip, port) = entry.split_at(4);
            let ip: [u8; 4] = ip.try_into().map_err(|_| SecurityError::Malformed("address"))?;
            servers.push(server(Ipv4Addr::from(ip).into(), port)?);
        }
        for entry in v6_bytes.as_chunks::<V6_LEN>().0 {
            let (ip, port) = entry.split_at(16);
            let ip: [u8; 16] = ip.try_into().map_err(|_| SecurityError::Malformed("address"))?;
            servers.push(server(Ipv6Addr::from(ip).into(), port)?);
        }
        let secret: [u8; SECRET_LEN] = secret_bytes
            .try_into()
            .map_err(|_| SecurityError::Malformed("secret"))?;
        Ok(Self {
            servers,
            secret: Zeroizing::new(secret),
        })
    }
}

fn server(ip: IpAddr, port: &[u8]) -> Result<SocketAddr, SecurityError> {
    let port: [u8; 2] = port.try_into().map_err(|_| SecurityError::Malformed("port"))?;
    let port = u16::from_be_bytes(port);
    if port == 0 || ip.is_unspecified() {
        return Err(SecurityError::Malformed("address"));
    }
    Ok(SocketAddr::new(ip, port))
}

/// An invite link as pasted: complete, or short and still to be fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InviteLink {
    Full(Invite),
    Short(ShortInvite),
}

impl InviteLink {
    /// Reads either kind. A full link is verified now; a short one once it is fetched.
    pub fn parse(link: &str, now_unix: u64) -> Result<Self, SecurityError> {
        if link.trim_start().starts_with(PREFIX) {
            ShortInvite::from_link(link).map(Self::Short)
        } else {
            Invite::from_link(link, now_unix).map(Self::Full)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use getrandom::SysRng;
    use rand_core::UnwrapErr;

    use super::*;
    use crate::Identity;

    const NOW: u64 = 1_800_000_000;

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn full_link(host: &Identity) -> (Invite, String) {
        let invite = Invite::new(
            host,
            vec!["203.0.113.7:4433".into()],
            None,
            NOW + 3_600,
            &mut UnwrapErr(SysRng),
        )
        .unwrap();
        let link = invite.to_link(host).unwrap();
        (invite, link)
    }

    fn create(link: &str, servers: &[SocketAddr]) -> (ShortInvite, SealedInvite) {
        ShortInvite::create(link, servers, &mut UnwrapErr(SysRng)).unwrap()
    }

    #[test]
    fn a_link_round_trips_and_opens_its_invite() {
        let host = Identity::generate();
        let (invite, full) = full_link(&host);
        let servers = [addr("203.0.113.9:60593"), addr("192.168.1.20:5000")];
        let (short, sealed) = create(&full, &servers);
        let parsed = ShortInvite::from_link(&short.to_link()).unwrap();
        assert_eq!(parsed, short);
        assert_eq!(parsed.servers(), servers);
        assert_eq!(parsed.id().unwrap(), sealed.id);
        assert_eq!(parsed.open(&sealed.bytes, NOW).unwrap(), invite);
    }

    #[test]
    fn a_short_link_is_short() {
        let host = Identity::generate();
        let (_, full) = full_link(&host);
        let servers = [addr("203.0.113.9:60593"), addr("192.168.1.20:5000")];
        let link = create(&full, &servers).0.to_link();
        assert_eq!(link.len(), PREFIX.len() + 39, "{link}");
    }

    #[test]
    fn ipv6_and_mapped_addresses_are_kept_apart() {
        let servers = [
            addr("[2001:db8::1]:4433"),
            addr("[::ffff:203.0.113.9]:4433"),
            addr("203.0.113.9:4433"),
        ];
        let short = create("veil:x", &servers).0;
        assert_eq!(short.servers(), [addr("203.0.113.9:4433"), addr("[2001:db8::1]:4433")]);
        assert_eq!(ShortInvite::from_link(&short.to_link()).unwrap(), short);
    }

    #[test]
    fn only_the_first_servers_are_kept() {
        let servers: Vec<_> = (1..9).map(|p| SocketAddr::from(([203, 0, 113, 9], p))).collect();
        assert_eq!(create("veil:x", &servers).0.servers(), &servers[..MAX_SERVERS]);
    }

    #[test]
    fn a_changed_or_foreign_invite_does_not_open() {
        let host = Identity::generate();
        let (_, full) = full_link(&host);
        let (short, mut sealed) = create(&full, &[addr("203.0.113.9:1")]);
        let (_, other) = create(&full, &[addr("203.0.113.9:1")]);
        assert_eq!(short.open(&other.bytes, NOW), Err(SecurityError::Decrypt));
        sealed.bytes[0] ^= 1;
        assert_eq!(short.open(&sealed.bytes, NOW), Err(SecurityError::Decrypt));
        assert_eq!(
            short.open(&vec![0; MAX_SEALED + 1], NOW),
            Err(SecurityError::TooLarge)
        );
    }

    #[test]
    fn the_sealed_invite_is_still_checked() {
        let host = Identity::generate();
        let (_, full) = full_link(&host);
        let (short, sealed) = create(&full, &[addr("203.0.113.9:1")]);
        assert_eq!(short.open(&sealed.bytes, NOW + 3_600), Err(SecurityError::Expired));
        let (short, sealed) = create("veil:AAAA", &[addr("203.0.113.9:1")]);
        assert!(short.open(&sealed.bytes, NOW).is_err(), "unsigned text");
    }

    #[test]
    fn malformed_links_are_refused() {
        let good = create("veil:x", &[addr("203.0.113.9:1")]).0.to_link();
        let body = URL_SAFE_NO_PAD.decode(&good[PREFIX.len()..]).unwrap();
        let link = |bytes: &[u8]| format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));

        let mut no_servers = body.clone();
        no_servers[0] = 0;
        let mut too_many = body.clone();
        too_many[0] = 0x04;
        let mut port_zero = body.clone();
        port_zero[5..7].fill(0);
        let mut any_ip = body.clone();
        any_ip[1..5].fill(0);
        for bad in [
            link(&no_servers),
            link(&too_many),
            link(&port_zero),
            link(&any_ip),
            link(&body[..body.len() - 1]),
            link(&[]),
            format!("{PREFIX}!!!"),
            "veil:abc".to_string(),
        ] {
            assert!(ShortInvite::from_link(&bad).is_err(), "{bad}");
        }
        let long = format!("{PREFIX}{}", "A".repeat(MAX_SHORT_LEN));
        assert_eq!(ShortInvite::from_link(&long), Err(SecurityError::TooLarge));
    }

    #[test]
    fn nothing_prints_the_secret() {
        let short = create("veil:x", &[addr("203.0.113.9:1")]).0;
        let printed = format!("{short:?}");
        assert!(printed.contains("203.0.113.9") && !printed.contains("secret"), "{printed}");
    }

    #[test]
    fn either_kind_of_link_parses() {
        let host = Identity::generate();
        let (invite, full) = full_link(&host);
        let (short, _) = create(&full, &[addr("203.0.113.9:1")]);
        assert_eq!(InviteLink::parse(&full, NOW).unwrap(), InviteLink::Full(invite));
        let pasted = format!("  {}\n", short.to_link());
        assert_eq!(InviteLink::parse(&pasted, NOW).unwrap(), InviteLink::Short(short));
        assert!(InviteLink::parse("nonsense", NOW).is_err());
    }
}
