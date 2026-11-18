//! This machine's default IPv6 router: where a PCP pinhole request goes.
//!
//! Purpose: the IPv6 twin of `gateway.rs`. The default IPv6 router is usually named by its
//! link-local address (`fe80::...`), which can be dialled only with the interface it is on,
//! so each answer carries that interface's index as the scope. Read from each OS's own tool:
//! - Linux: `/proc/net/ipv6_route`, and `/sys/class/net/<name>/ifindex` for the index.
//! - Windows: `route print -6 ::/0`, whose first column is the interface index.
//!
//! Invariants:
//! - The parsers are pure and tested on sample output; only `find` touches the system.
//! - Only a unicast router address is returned: never loopback, multicast, unspecified or
//!   IPv4-mapped.
//! - When several default routes exist, the lowest metric wins.
//!
//! shortcut: macOS and other BSDs are not read (`route -n get -inet6 default` names the
//! interface, not its index), so no pinhole is asked for there. Add it with `ifconfig`'s
//! `scopeid` when those builds are tested on real machines.

use std::net::{Ipv6Addr, SocketAddrV6};

/// The default IPv6 router at `port`, if the system reports one.
pub async fn find(port: u16) -> Option<SocketAddrV6> {
    let (ip, scope) = tokio::task::spawn_blocking(sys).await.ok()??;
    usable(ip).then(|| SocketAddrV6::new(ip, port, 0, scope))
}

fn usable(ip: Ipv6Addr) -> bool {
    !(ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() || ip.to_ipv4_mapped().is_some())
}

#[cfg(target_os = "linux")]
fn sys() -> Option<(Ipv6Addr, u32)> {
    let text = std::fs::read_to_string("/proc/net/ipv6_route").ok()?;
    let (ip, device) = parse_proc_ipv6_route(&text)?;
    let index = std::fs::read_to_string(format!("/sys/class/net/{device}/ifindex")).ok()?;
    Some((ip, index.trim().parse().ok()?))
}

#[cfg(windows)]
fn sys() -> Option<(Ipv6Addr, u32)> {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up while the call starts.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("route")
        .args(["print", "-6", "::/0"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_route_print6(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn sys() -> Option<(Ipv6Addr, u32)> {
    None
}

/// Linux `/proc/net/ipv6_route`: rows of destination, its prefix length, source, its prefix
/// length, next hop, metric, use count, reference count, flags and device, all hexadecimal
/// but the device. The default route has an all-zero destination of length 0, a next hop,
/// and the gateway flag (`0x2`). Returns the next hop and the device name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "Linux only"))]
fn parse_proc_ipv6_route(text: &str) -> Option<(Ipv6Addr, String)> {
    const RTF_GATEWAY: u32 = 0x2;
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [dest, len, _, _, hop, metric, _, _, flags, device] = f.as_slice() else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            if u128::from_str_radix(dest, 16).ok()? != 0
                || *len != "00"
                || flags & RTF_GATEWAY == 0
            {
                return None;
            }
            let hop = Ipv6Addr::from(u128::from_str_radix(hop, 16).ok()?);
            let metric = u32::from_str_radix(metric, 16).ok()?;
            (!hop.is_unspecified()).then(|| (metric, hop, (*device).to_string()))
        })
        .min_by_key(|(metric, ..)| *metric)
        .map(|(_, hop, device)| (hop, device))
}

/// Windows `route print -6 ::/0`: under "Active Routes", rows `IF METRIC ::/0 GATEWAY`. A long
/// gateway may wrap onto the next line on its own. "On-link" rows have no router.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows only"))]
fn parse_route_print6(text: &str) -> Option<(Ipv6Addr, u32)> {
    let mut best: Option<(u32, Ipv6Addr, u32)> = None;
    let mut active = false;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("Active Routes") {
            active = true;
            continue;
        }
        if line.starts_with("Persistent Routes") {
            break;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if !active || f.get(2) != Some(&"::/0") {
            continue;
        }
        let gateway = match f.get(3) {
            Some(g) => (*g).to_string(),
            None => match lines.peek() {
                Some(next) if next.split_whitespace().count() == 1 => {
                    lines.next().map(|n| n.trim().to_string()).unwrap_or_default()
                }
                _ => continue,
            },
        };
        let (Ok(index), Ok(metric), Ok(ip)) = (
            f[0].parse::<u32>(),
            f[1].parse::<u32>(),
            gateway.parse::<Ipv6Addr>(),
        ) else {
            continue;
        };
        if best.is_none_or(|(m, ..)| metric < m) {
            best = Some((metric, ip, index));
        }
    }
    best.map(|(_, ip, index)| (ip, index))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn linux_default_route_with_lowest_metric() {
        let text = "\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000dad8e5fffe3c2cbc 00000400 00000001 00000000 00000003 wlan0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 eth0
2a0023c5123456000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo
";
        assert_eq!(
            parse_proc_ipv6_route(text),
            Some(("fe80::1".parse().unwrap(), "eth0".to_string()))
        );
        assert_eq!(parse_proc_ipv6_route(""), None);
    }

    #[test]
    fn windows_default_route_with_lowest_metric() {
        let text = "\
===========================================================================
IPv6 Route Table
===========================================================================
Active Routes:
 If Metric Network Destination      Gateway
 34    500 ::/0                     On-link
 20    286 ::/0                     fe80::dad8:e5ff:fe3c:2cbc
 12     25 ::/0
                                    fe80::1
  1    331 ::1/128                  On-link
===========================================================================
Persistent Routes:
 20    100 ::/0                     fe80::99
";
        assert_eq!(parse_route_print6(text), Some(("fe80::1".parse().unwrap(), 12)));
        let one = "Active Routes:\n 20    286 ::/0                     fe80::dad8:e5ff:fe3c:2cbc\n";
        assert_eq!(
            parse_route_print6(one),
            Some(("fe80::dad8:e5ff:fe3c:2cbc".parse().unwrap(), 20))
        );
        assert_eq!(parse_route_print6("Active Routes:\n 34 500 ::/0 On-link\n"), None);
    }

    #[test]
    fn only_router_addresses_are_used() {
        assert!(usable("fe80::1".parse().unwrap()));
        assert!(usable("fd00::1".parse().unwrap()));
        assert!(usable("2a00:23c5::1".parse().unwrap()));
        assert!(!usable("::1".parse().unwrap()));
        assert!(!usable("ff02::1".parse().unwrap()));
        assert!(!usable("::ffff:192.168.1.1".parse().unwrap()));
    }
}
