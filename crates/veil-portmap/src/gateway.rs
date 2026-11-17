//! This machine's default IPv4 gateway: the router PCP and NAT-PMP talk to.
//!
//! Purpose: PCP and NAT-PMP requests go to the default gateway on UDP 5351, and UPnP answers
//! are trusted only from it when it is known. The standard library cannot read the routing
//! table, and native calls are kept out of this crate (`unsafe` is forbidden outside
//! `veil-media`), so each OS's own tool is read instead:
//! - Linux: `/proc/net/route`, a file, no process.
//! - Windows: `route print -4 0.0.0.0`, no console window.
//! - macOS and other BSDs: `route -n get default`.
//!
//! Invariants:
//! - The parsers are pure and tested on sample output; only `find` touches the system.
//! - A gateway is returned only if it is a local address (`addr::is_local`).
//! - When several default routes exist, the one on `local` (the interface Veil's calls use)
//!   wins, then the lowest metric.

use std::net::Ipv4Addr;

use crate::addr;

/// The default gateway for traffic from `local`, if the system reports one.
pub async fn find(local: Ipv4Addr) -> Option<Ipv4Addr> {
    let found = tokio::task::spawn_blocking(move || sys(local))
        .await
        .ok()??;
    addr::is_local(found).then_some(found)
}

#[cfg(target_os = "linux")]
fn sys(_local: Ipv4Addr) -> Option<Ipv4Addr> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    parse_proc_route(&text)
}

#[cfg(windows)]
fn sys(local: Ipv4Addr) -> Option<Ipv4Addr> {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up while the call starts.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("route")
        .args(["print", "-4", "0.0.0.0"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_route_print(&String::from_utf8_lossy(&output.stdout), local)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn sys(_local: Ipv4Addr) -> Option<Ipv4Addr> {
    let output = std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    parse_route_get(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(unix, windows)))]
fn sys(_local: Ipv4Addr) -> Option<Ipv4Addr> {
    None
}

/// Linux `/proc/net/route`: the default route (destination and mask 0) with the gateway flag,
/// lowest metric first. Addresses are hexadecimal in the kernel's (little-endian) order.
#[cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "Linux only"))]
fn parse_proc_route(text: &str) -> Option<Ipv4Addr> {
    const RTF_GATEWAY: u32 = 0x2;
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (dest, gateway, flags) =
                (hex(cols.get(1)?)?, hex(cols.get(2)?)?, hex(cols.get(3)?)?);
            let (metric, mask) = (cols.get(6)?.parse::<u32>().ok()?, hex(cols.get(7)?)?);
            (dest == 0 && mask == 0 && flags & RTF_GATEWAY != 0 && gateway != 0)
                .then(|| (metric, Ipv4Addr::from(gateway.to_le_bytes())))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, ip)| ip)
}

/// Windows `route print -4 0.0.0.0`: rows `0.0.0.0 0.0.0.0 GATEWAY INTERFACE METRIC`. Headers
/// are translated on other languages, so only the rows' shape is read. "On-link" rows have
/// no gateway and are skipped.
#[cfg_attr(not(windows), allow(dead_code, reason = "Windows only"))]
fn parse_route_print(text: &str, local: Ipv4Addr) -> Option<Ipv4Addr> {
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let [dest, mask, gateway, interface, metric] = cols.as_slice() else {
                return None;
            };
            if *dest != "0.0.0.0" || *mask != "0.0.0.0" {
                return None;
            }
            let gateway: Ipv4Addr = gateway.parse().ok()?;
            let interface: Ipv4Addr = interface.parse().ok()?;
            let metric: u32 = metric.parse().ok()?;
            Some((interface != local, metric, gateway))
        })
        .min_by_key(|(other, metric, _)| (*other, *metric))
        .map(|(_, _, ip)| ip)
}

/// macOS `route -n get default`: the `gateway:` line.
#[cfg_attr(
    not(all(unix, not(target_os = "linux"))),
    allow(dead_code, reason = "macOS and BSD only")
)]
fn parse_route_get(text: &str) -> Option<Ipv4Addr> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("gateway:"))
        .find_map(|rest| rest.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

    #[test]
    fn linux_proc_route() {
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                    wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                    eth0\t00000000\t0100000A\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                    wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(parse_proc_route(text), Some(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(parse_proc_route("Iface\n"), None);
    }

    #[test]
    fn windows_route_print() {
        let text = "===========================================================================\n\
                    Active Routes:\n\
                    Network Destination        Netmask          Gateway       Interface  Metric\n\
                    \x20         0.0.0.0          0.0.0.0     10.8.0.1      10.8.0.5      5\n\
                    \x20         0.0.0.0          0.0.0.0      192.168.1.1   192.168.1.20     35\n\
                    \x20         0.0.0.0          0.0.0.0         On-link     172.20.0.1    500\n\
                    ===========================================================================\n\
                    Persistent Routes:\n  None\n";
        assert_eq!(
            parse_route_print(text, LOCAL),
            Some(Ipv4Addr::new(192, 168, 1, 1)),
            "the call interface's route wins over a lower metric"
        );
        assert_eq!(
            parse_route_print(text, Ipv4Addr::new(172, 16, 0, 9)),
            Some(Ipv4Addr::new(10, 8, 0, 1)),
            "otherwise the lowest metric"
        );
        assert_eq!(parse_route_print("Active Routes:\n  None\n", LOCAL), None);
    }

    #[test]
    fn macos_route_get() {
        let text = "   route to: default\ndestination: default\n       mask: default\n    \
                    gateway: 192.168.0.1\n  interface: en0\n";
        assert_eq!(parse_route_get(text), Some(Ipv4Addr::new(192, 168, 0, 1)));
        assert_eq!(
            parse_route_get("route: writing to routing socket: not in table\n"),
            None
        );
    }
}
