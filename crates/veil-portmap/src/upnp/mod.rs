//! UPnP IGD: find the router by SSDP, read its description, and map a port with SOAP.
//!
//! Purpose: the oldest and most common of the three ways. Most home routers that offer any
//! automatic port opening offer this one.
//!
//! Invariants:
//! - An SSDP answer is trusted only if it comes from a local address, from the default
//!   gateway when that is known, and names a description URL on the very address it came
//!   from (`text::RouterUrl`). Another device on the network cannot point Veil elsewhere.
//! - HTTP goes to that address only: no redirects, no proxy, at most [`MAX_BODY`] bytes read,
//!   [`HTTP_TIMEOUT`] per request. Every request is blocking and runs off the async threads.
//! - The mapping names this machine's own LAN address as the internal client.

pub mod text;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};

use crate::addr;
use text::RouterUrl;

const SSDP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
/// How long to listen for routers after asking.
const SEARCH_TIME: Duration = Duration::from_millis(1500);
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BODY: u64 = 64 * 1024;
const SEARCH_TARGETS: [&str; 2] = [
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
    "urn:schemas-upnp-org:device:InternetGatewayDevice:2",
];

/// A router's WAN connection service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Igd {
    pub control: RouterUrl,
    pub service: String,
}

/// A failed SOAP call: the UPnP error code when the router gave one.
#[derive(Clone, Debug)]
pub struct SoapError {
    pub code: Option<u16>,
    pub text: String,
}

/// UPnP error codes from the IGD specification, the ones Veil acts on.
pub const CONFLICT: u16 = 718;
pub const ONLY_PERMANENT_LEASES: u16 = 725;

/// Asks the network for a gateway device and returns the first one that checks out.
pub async fn discover(local: Ipv4Addr, gateway: Option<Ipv4Addr>) -> Result<Igd, String> {
    let socket = UdpSocket::bind(SocketAddr::new(local.into(), 0))
        .await
        .map_err(|e| format!("cannot open a socket: {e}"))?;
    for target in SEARCH_TARGETS {
        let search = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP}\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: \
             {target}\r\n\r\n"
        );
        socket
            .send_to(search.as_bytes(), SocketAddr::V4(SSDP))
            .await
            .map_err(|e| format!("cannot search the network: {e}"))?;
    }
    let deadline = Instant::now() + SEARCH_TIME;
    let mut buf = [0u8; 2048];
    let mut tried: Vec<RouterUrl> = Vec::new();
    let mut last = String::from("no router answered");
    while let Ok(received) = timeout_at(deadline, socket.recv_from(&mut buf)).await {
        let Ok((n, SocketAddr::V4(from))) = received else {
            continue;
        };
        let Some(url) = trusted_location(&buf[..n.min(buf.len())], *from.ip(), gateway) else {
            continue;
        };
        if tried.contains(&url) {
            continue;
        }
        tried.push(url.clone());
        match describe(url).await {
            Ok(igd) => return Ok(igd),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The description URL in an SSDP answer from `from`, if Veil may trust it.
fn trusted_location(data: &[u8], from: Ipv4Addr, gateway: Option<Ipv4Addr>) -> Option<RouterUrl> {
    if !addr::is_local(from) || gateway.is_some_and(|g| g != from) {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let url = RouterUrl::parse(text::ssdp_location(text)?)?;
    (url.host == from).then_some(url)
}

/// Reads the device description at `url` and finds its WAN connection service.
async fn describe(url: RouterUrl) -> Result<Igd, String> {
    let shown = url.to_string();
    let body = blocking(move || {
        let mut response = agent()
            .get(&url.to_string())
            .call()
            .map_err(|e| format!("cannot read {url}: {e}"))?;
        if response.status() != 200 {
            return Err(format!("{url} answered {}", response.status()));
        }
        let body = read(&mut response)?;
        let (service, control) =
            text::wan_service(&body).ok_or("the router offers no port mapping service")?;
        let control = url
            .join(&control)
            .ok_or("the router's control address is not on the router")?;
        Ok(Igd { control, service })
    })
    .await;
    body.map_err(|e| format!("{e} ({shown})"))
}

/// Maps UDP `external_port` to `client:internal_port` for `lease` (zero is permanent).
pub async fn add(
    igd: &Igd,
    external_port: u16,
    client: Ipv4Addr,
    internal_port: u16,
    lease: Duration,
) -> Result<(), SoapError> {
    let (ext, int) = (external_port.to_string(), internal_port.to_string());
    let (client, lease) = (client.to_string(), lease.as_secs().to_string());
    let args = [
        ("NewRemoteHost", ""),
        ("NewExternalPort", ext.as_str()),
        ("NewProtocol", "UDP"),
        ("NewInternalPort", int.as_str()),
        ("NewInternalClient", client.as_str()),
        ("NewEnabled", "1"),
        ("NewPortMappingDescription", "Veil"),
        ("NewLeaseDuration", lease.as_str()),
    ];
    call(igd, "AddPortMapping", &args).await.map(drop)
}

/// The router's own address on the internet side.
pub async fn external_ip(igd: &Igd) -> Result<Ipv4Addr, SoapError> {
    let body = call(igd, "GetExternalIPAddress", &[]).await?;
    text::element(&body, "NewExternalIPAddress")
        .and_then(|ip| ip.parse().ok())
        .ok_or_else(|| SoapError {
            code: None,
            text: String::from("the router gave no external address"),
        })
}

/// Removes the mapping of UDP `external_port`.
pub async fn delete(igd: &Igd, external_port: u16) -> Result<(), SoapError> {
    let ext = external_port.to_string();
    let args = [
        ("NewRemoteHost", ""),
        ("NewExternalPort", ext.as_str()),
        ("NewProtocol", "UDP"),
    ];
    call(igd, "DeletePortMapping", &args).await.map(drop)
}

/// What a UPnP error code means, in words.
pub fn error_name(code: u16) -> &'static str {
    match code {
        401 | 602 => "the router does not support this",
        402 => "the router refused the request",
        501 => "the router could not do it",
        606 => "not authorised (turned off on the router)",
        714 => "no such mapping",
        CONFLICT => "the port is already mapped to another device",
        724 => "the router needs the same port inside and out",
        ONLY_PERMANENT_LEASES => "the router allows permanent mappings only",
        726 | 727 => "the router refused the request",
        _ => "refused",
    }
}

async fn call(igd: &Igd, action: &'static str, args: &[(&str, &str)]) -> Result<String, SoapError> {
    let body = text::soap(&igd.service, action, args);
    let (url, soap_action) = (
        igd.control.to_string(),
        format!("\"{}#{action}\"", igd.service),
    );
    let failed = |text: String| SoapError { code: None, text };
    let result = blocking(move || {
        let mut response = agent()
            .post(&url)
            .header("SOAPAction", &soap_action)
            .content_type("text/xml; charset=\"utf-8\"")
            .send(&body)
            .map_err(|e| format!("cannot reach the router: {e}"))?;
        let status = response.status().as_u16();
        Ok((status, read(&mut response)?))
    })
    .await;
    match result {
        Err(e) => Err(failed(e)),
        Ok((200, body)) => Ok(body),
        Ok((status, body)) => {
            let code = text::error_code(&body);
            let text = code.map_or_else(
                || format!("the router answered {status}"),
                |c| format!("{} ({c})", error_name(c)),
            );
            Err(SoapError { code, text })
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build()
        .into()
}

fn read(response: &mut ureq::http::Response<ureq::Body>) -> Result<String, String> {
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .map_err(|e| format!("the router's answer could not be read: {e}"))
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| format!("the request stopped: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 1);
    const ANSWER: &[u8] =
        b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";

    #[test]
    fn only_the_router_itself_is_trusted() {
        assert!(trusted_location(ANSWER, GATEWAY, Some(GATEWAY)).is_some());
        assert!(
            trusted_location(ANSWER, GATEWAY, None).is_some(),
            "gateway unknown"
        );
        let other = Ipv4Addr::new(192, 168, 1, 66);
        assert!(
            trusted_location(ANSWER, other, Some(GATEWAY)).is_none(),
            "not the gateway"
        );
        assert!(
            trusted_location(ANSWER, other, None).is_none(),
            "names an address it did not answer from"
        );
        let public = Ipv4Addr::new(81, 2, 69, 160);
        let far = b"HTTP/1.1 200 OK\r\nLOCATION: http://81.2.69.160:5000/d.xml\r\n\r\n";
        assert!(trusted_location(far, public, None).is_none(), "not local");
    }
}
