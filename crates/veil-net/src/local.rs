//! Interface discovery and the Local only boundary. No route probe or network request.
//!
//! A private-looking address alone is insufficient: it must belong to a connected subnet.
//! Point-to-point interfaces, short prefixes and virtual or overlay networks (VPNs, ZeroTier,
//! Tailscale, VM and container switches, matched by name) are excluded. A bridge that carries
//! the real LAN still counts; this is an on-link IP boundary, not a physical-distance
//! guarantee. The boundary also wraps the actual QUIC UDP
//! socket, covering replies and migration as well as initial dials.

use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

use if_addrs::{IfAddr, Interface};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};

/// The system's current usable interfaces; failure is deliberately closed (loopback only).
/// Virtual networks ([`is_virtual`]) are left out, so Local only, address hints and nearby
/// discovery stay on the networks a person can see around them.
fn interfaces() -> Vec<Interface> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.is_oper_up() && !i.is_p2p() && !is_virtual(&i.name))
        .collect()
}

/// Parts of Windows' friendly adapter names that mark a virtual or overlay network.
/// Hyper-V's own switches are named; an external switch (`vEthernet (External)`, which
/// carries the real LAN once Hyper-V takes over the network card) is not.
const VIRTUAL_NAME_PARTS: &[&str] = &[
    "vethernet (default switch)",
    "vethernet (wsl",
    "zerotier",
    "tailscale",
    "virtualbox",
    "vmware",
    "wireguard",
    "openvpn",
    "nordlynx",
    "hamachi",
    "radmin vpn",
    "npcap",
];

/// Starts of Linux and macOS interface names for virtual or overlay networks. A plain bridge
/// (`br0`, often the real LAN on a VM host) is kept; Docker's `br-<id>` bridges are not.
const VIRTUAL_NAME_STARTS: &[&str] = &[
    "zt", "tailscale", "docker", "br-", "veth", "virbr", "vboxnet", "vmnet", "vmenet", "wg",
    "tun", "tap", "utun", "feth", "lxcbr", "lxdbr", "cni", "flannel", "podman", "bridge1",
];

/// Whether an interface name marks a virtual or overlay network (a VPN, ZeroTier, Tailscale,
/// a VM or container switch) rather than one the device is physically on.
///
/// shortcut: matched by name, since no system reports "virtual" reliably (Hyper-V and
/// ZeroTier adapters both report Ethernet). An unlisted or renamed adapter still counts as
/// local. Upgrade by letting the person choose networks in Connection if that turns up.
pub fn is_virtual(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    // Unix interface names have no spaces; Windows friendly names often do, and
    // `vEthernet (External)` must not match Linux's `veth`.
    let unix = !name.contains(char::is_whitespace);
    VIRTUAL_NAME_PARTS.iter().any(|part| name.contains(part))
        || (unix && VIRTUAL_NAME_STARTS.iter().any(|start| name.starts_with(start)))
}

/// All usable interface addresses at `port`, without needing a default route or DNS.
/// Link-local IPv6 scope IDs are local hints; dialling remaps them on the receiving device.
pub fn candidates(port: u16) -> Vec<SocketAddr> {
    let mut found = Vec::new();
    for interface in interfaces() {
        let ip = interface.ip();
        if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
            continue;
        }
        let addr = match ip {
            IpAddr::V6(ip) if ip.is_unicast_link_local() => {
                let Some(index) = interface.index else {
                    continue;
                };
                SocketAddr::V6(SocketAddrV6::new(ip, port, 0, index))
            }
            _ => SocketAddr::new(ip, port),
        };
        if !found.contains(&addr) {
            found.push(addr);
        }
    }
    // Routable local IPv4/ULA first: scoped link-local addresses need interface matching.
    found.sort_by_key(|addr| {
        (
            match addr.ip() {
                IpAddr::V4(ip) if ip.is_private() || ip.is_link_local() => 0,
                IpAddr::V6(ip) if ip.is_unique_local() => 1,
                IpAddr::V4(_) => 2,
                IpAddr::V6(ip) if !ip.is_unicast_link_local() => 3,
                _ => 4,
            },
            *addr,
        )
    });
    found
}

/// Addresses that only mean something on the network they belong to: private and
/// link-local IPv4, unique local and link-local IPv6. Public, carrier-grade NAT and
/// documentation addresses are not local scope.
pub fn is_local_scope(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// [`candidates`] restricted to [`is_local_scope`]. Use this wherever addresses are shared:
/// a full interface list (VPN, virtual adapters, every IPv6 address) fingerprints a machine.
pub fn local_candidates(port: u16) -> Vec<SocketAddr> {
    candidates(port)
        .into_iter()
        .filter(|addr| is_local_scope(addr.ip()))
        .collect()
}

/// IPv4 addresses on which local multicast can be sent without a default route.
pub fn ipv4_interfaces() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    for interface in interfaces().into_iter().filter(|i| !i.is_loopback()) {
        if let IpAddr::V4(ip) = interface.ip()
            && !ip.is_unspecified()
            && !found.contains(&ip)
        {
            found.push(ip);
        }
    }
    found.sort();
    found
}

/// Expands a received link-local IPv6 address using this device's interface indexes.
/// Sender-side scope numbers are never assumed to identify the same interface here.
pub fn scoped_candidates(addr: SocketAddr) -> Vec<SocketAddr> {
    let SocketAddr::V6(v6) = addr else {
        return vec![addr];
    };
    if !v6.ip().is_unicast_link_local() {
        return vec![addr];
    }
    let mut found = Vec::new();
    for interface in interfaces() {
        if interface.ip().is_ipv6()
            && let Some(index) = interface.index
        {
            let addr = SocketAddr::V6(SocketAddrV6::new(*v6.ip(), v6.port(), 0, index));
            if !found.contains(&addr) {
                found.push(addr);
            }
        }
    }
    found
}

/// Shortest prefix accepted as one local network. A shorter one (a broken or VPN adapter
/// claiming `/1`) would open a large part of the internet, so it counts as no network.
const MIN_PREFIX_V4: u32 = 8;
const MIN_PREFIX_V6: u32 = 48;

fn on_interface(addr: SocketAddr, interface: &Interface) -> bool {
    let ip = addr.ip().to_canonical();
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match (&interface.addr, ip) {
        (IfAddr::V4(local), IpAddr::V4(ip)) => {
            let mask = u32::from(local.netmask);
            mask.leading_ones() >= MIN_PREFIX_V4
                && ip != Ipv4Addr::BROADCAST
                && local.broadcast != Some(ip)
                && u32::from(ip) & mask == u32::from(local.ip) & mask
        }
        (IfAddr::V6(local), IpAddr::V6(ip)) => {
            let mask = u128::from(local.netmask);
            let scope_ok = !ip.is_unicast_link_local()
                || matches!(addr,
                SocketAddr::V6(v6) if v6.scope_id() != 0 && Some(v6.scope_id()) == interface.index);
            mask.leading_ones() >= MIN_PREFIX_V6
                && scope_ok
                && u128::from(ip) & mask == u128::from(local.ip) & mask
        }
        _ => false,
    }
}

fn allowed(addr: SocketAddr, interfaces: &[Interface]) -> bool {
    addr.ip().to_canonical().is_loopback() || interfaces.iter().any(|i| on_interface(addr, i))
}

/// Whether an address is on a connected local subnet right now. Never uses DNS.
pub fn is_on_link(addr: SocketAddr) -> bool {
    allowed(addr, &interfaces())
}

#[derive(Debug, Default)]
pub(crate) struct Policy {
    enabled: AtomicBool,
    interfaces: RwLock<Vec<Interface>>,
}

impl Policy {
    pub fn set(&self, enabled: bool) {
        if enabled {
            self.refresh();
        }
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    pub fn refresh(&self) {
        if let Ok(mut current) = self.interfaces.write() {
            *current = interfaces();
        }
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
    pub fn allows(&self, addr: SocketAddr) -> bool {
        !self.enabled() || self.interfaces.read().is_ok_and(|i| allowed(addr, &i))
    }
}

#[derive(Debug)]
pub(crate) struct LocalSocket {
    pub socket: Arc<dyn AsyncUdpSocket>,
    pub policy: Arc<Policy>,
}

impl AsyncUdpSocket for LocalSocket {
    fn create_io_poller(self: Arc<Self>) -> std::pin::Pin<Box<dyn UdpPoller>> {
        self.socket.clone().create_io_poller()
    }
    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        if self.policy.allows(transmit.destination) {
            self.socket.try_send(transmit)
        } else {
            Ok(())
        } // Drop blocked traffic without provoking a response.
    }
    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let count = match self.socket.poll_recv(cx, bufs, meta) {
            Poll::Ready(Ok(count)) => count,
            other => return other,
        };
        // quinn ignores zero-length datagrams. Preserve positions/stride for GRO batches.
        for row in meta.iter_mut().take(count) {
            if !self.policy.allows(row.addr) {
                row.len = 0;
            }
        }
        Poll::Ready(Ok(count))
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    fn max_transmit_segments(&self) -> usize {
        self.socket.max_transmit_segments()
    }
    fn max_receive_segments(&self) -> usize {
        self.socket.max_receive_segments()
    }
    fn may_fragment(&self) -> bool {
        self.socket.may_fragment()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn interface(ip: &str, prefixlen: u8) -> Interface {
        let ip: Ipv4Addr = ip.parse().unwrap();
        Interface {
            name: "test".into(),
            index: Some(7),
            oper_status: if_addrs::IfOperStatus::Up,
            is_p2p: false,
            #[cfg(windows)]
            adapter_name: "test".into(),
            addr: IfAddr::V4(if_addrs::Ifv4Addr {
                ip,
                prefixlen,
                netmask: Ipv4Addr::from(
                    u32::MAX.checked_shl(32 - u32::from(prefixlen)).unwrap_or(0),
                ),
                broadcast: None,
            }),
        }
    }
    #[test]
    fn locality_requires_a_connected_subnet_not_just_private_addressing() {
        let interfaces = vec![interface("192.168.1.20", 24)];
        assert!(allowed("192.168.1.9:7000".parse().unwrap(), &interfaces));
        assert!(allowed("127.0.0.1:7000".parse().unwrap(), &interfaces));
        for other in [
            "192.168.2.9:7000",
            "10.0.0.9:7000",
            "8.8.8.8:7000",
            "224.0.0.1:7000",
        ] {
            assert!(!allowed(other.parse().unwrap(), &interfaces));
        }
    }
    #[test]
    fn virtual_and_overlay_networks_are_not_local() {
        for name in [
            "vEthernet (Default Switch)",
            "vEthernet (WSL (Hyper-V firewall))",
            "ZeroTier One [8056c2e21c000001]",
            "Tailscale",
            "VirtualBox Host-Only Network",
            "VMware Network Adapter VMnet8",
            "zt5u4y6ejv",
            "tailscale0",
            "docker0",
            "br-3f2a9c1d",
            "veth12ab",
            "virbr0",
            "wg0",
            "utun3",
            "bridge100",
        ] {
            assert!(is_virtual(name), "{name}");
        }
        for name in ["Wi-Fi", "Ethernet", "Ethernet 2", "vEthernet (External)", "wlan0",
            "eth0", "enp3s0", "wlp2s0", "br0", "en0", "en1"]
        {
            assert!(!is_virtual(name), "{name}");
        }
    }

    #[test]
    fn zero_prefix_never_opens_the_internet() {
        assert!(!allowed(
            "8.8.8.8:7000".parse().unwrap(),
            &[interface("10.0.0.1", 0)]
        ));
    }

    #[test]
    fn a_very_short_prefix_is_not_a_local_network() {
        assert!(!allowed(
            "8.8.8.8:7000".parse().unwrap(),
            &[interface("10.0.0.1", 1)]
        ));
        assert!(!allowed(
            "11.0.0.9:7000".parse().unwrap(),
            &[interface("10.0.0.1", 7)]
        ));
        assert!(allowed(
            "10.200.0.9:7000".parse().unwrap(),
            &[interface("10.0.0.1", 8)]
        ));
    }

    #[test]
    fn local_scope_excludes_public_and_carrier_addresses() {
        for local in ["192.168.1.2", "10.0.0.1", "169.254.1.1", "fd00::1", "fe80::1"] {
            assert!(is_local_scope(local.parse().unwrap()), "{local}");
        }
        for other in ["81.2.69.160", "100.64.0.1", "2a00:1450::1", "127.0.0.1"] {
            assert!(!is_local_scope(other.parse().unwrap()), "{other}");
        }
    }

    #[test]
    fn link_local_ipv6_requires_the_receiving_interfaces_scope() {
        let interface = Interface {
            name: "test".into(),
            index: Some(7),
            oper_status: if_addrs::IfOperStatus::Up,
            is_p2p: false,
            #[cfg(windows)]
            adapter_name: "test".into(),
            addr: IfAddr::V6(if_addrs::Ifv6Addr {
                ip: "fe80::10".parse().unwrap(),
                prefixlen: 64,
                netmask: "ffff:ffff:ffff:ffff::".parse().unwrap(),
                broadcast: None,
            }),
        };
        assert!(on_interface(
            "[fe80::20%7]:7710".parse().unwrap(),
            &interface
        ));
        assert!(!on_interface(
            "[fe80::20%8]:7710".parse().unwrap(),
            &interface
        ));
        assert!(!on_interface(
            "[fe80::20]:7710".parse().unwrap(),
            &interface
        ));
    }

    #[derive(Debug, Default)]
    struct Recorded(std::sync::atomic::AtomicUsize);
    impl AsyncUdpSocket for Recorded {
        fn create_io_poller(self: Arc<Self>) -> std::pin::Pin<Box<dyn UdpPoller>> {
            Box::pin(Wait)
        }
        fn try_send(&self, _: &Transmit<'_>) -> io::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn poll_recv(
            &self,
            _: &mut Context,
            _: &mut [IoSliceMut<'_>],
            _: &mut [RecvMeta],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn local_addr(&self) -> io::Result<SocketAddr> {
            Ok("127.0.0.1:7000".parse().unwrap())
        }
    }
    #[derive(Debug)]
    struct Wait;
    impl UdpPoller for Wait {
        fn poll_writable(self: std::pin::Pin<&mut Self>, _: &mut Context) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    #[test]
    fn socket_boundary_blocks_packets_even_without_a_connect_call() {
        let raw = Arc::new(Recorded::default());
        let policy = Arc::new(Policy::default());
        policy.enabled.store(true, Ordering::Relaxed); // No networks: loopback only.
        let socket = LocalSocket {
            socket: raw.clone(),
            policy,
        };
        for destination in ["203.0.113.9:7000", "10.0.0.9:7000", "127.0.0.1:7000"] {
            socket
                .try_send(&Transmit {
                    destination: destination.parse().unwrap(),
                    ecn: None,
                    contents: b"packet",
                    segment_size: None,
                    src_ip: None,
                })
                .unwrap();
        }
        assert_eq!(raw.0.load(Ordering::Relaxed), 1);
    }
}
