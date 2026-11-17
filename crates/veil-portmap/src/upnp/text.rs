//! The text side of UPnP: SSDP answers, device descriptions, SOAP bodies, and router URLs.
//!
//! Purpose: everything here reads text a device on the local network sent, so it is pure,
//! tested, and strict. Only the few elements Veil needs are read; this is not a general XML
//! parser, and it does not need to be: unknown elements, comments and namespaces are skipped.
//!
//! Invariants:
//! - A router URL is `http://IPV4[:PORT]/PATH` with a printable ASCII path ([`RouterUrl`]).
//!   No names (nothing to resolve or spoof), no `https`, no user info.
//! - A control URL must be on the same address and port as the description that named it.
//! - A service type is accepted only if it is exactly `urn:schemas-upnp-org:service:` then
//!   `WANIPConnection` or `WANPPPConnection`, `:` and a version digit. That makes it safe in
//!   the SOAPAction header and in the body's namespace attribute without escaping.

use std::fmt;
use std::net::Ipv4Addr;

/// A router's URL, checked (see the module's invariants).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouterUrl {
    pub host: Ipv4Addr,
    pub port: u16,
    /// Starts with `/`.
    pub path: String,
}

impl RouterUrl {
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.trim().strip_prefix("http://")?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, 80),
        };
        let url = Self {
            host: host.parse().ok()?,
            port,
            path: path.to_string(),
        };
        (port != 0 && printable(&url.path)).then_some(url)
    }

    /// Resolves a control URL found in this description: absolute (same address and port
    /// only), absolute path, or relative to the root.
    pub fn join(&self, control: &str) -> Option<Self> {
        let control = control.trim();
        if control.starts_with("http://") {
            let url = Self::parse(control)?;
            return (url.host == self.host && url.port == self.port).then_some(url);
        }
        if control.contains("://") || control.is_empty() {
            return None;
        }
        let path = if control.starts_with('/') {
            control.to_string()
        } else {
            format!("/{control}")
        };
        printable(&path).then_some(Self {
            host: self.host,
            port: self.port,
            path,
        })
    }
}

impl fmt::Display for RouterUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{}:{}{}", self.host, self.port, self.path)
    }
}

fn printable(path: &str) -> bool {
    path.starts_with('/') && path.len() <= 512 && path.bytes().all(|b| b.is_ascii_graphic())
}

/// The `LOCATION` header of an SSDP answer (an HTTP-style header block).
pub fn ssdp_location(answer: &str) -> Option<&str> {
    let mut lines = answer.lines();
    if !lines.next()?.starts_with("HTTP/1.1 200") {
        return None;
    }
    lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim())
    })
}

/// The first WAN connection service in a device description: its type and control URL.
/// `WANIPConnection` is preferred over `WANPPPConnection`.
pub fn wan_service(description: &str) -> Option<(String, String)> {
    let mut found: Vec<(String, String)> = blocks(description, "service")
        .filter_map(|block| {
            let service = element(block, "serviceType")?;
            let control = element(block, "controlURL")?;
            valid_service(&service).then_some((service, control))
        })
        .collect();
    found.sort_by_key(|(service, _)| !service.contains("WANIPConnection"));
    found.into_iter().next()
}

fn valid_service(service: &str) -> bool {
    let Some(rest) = service.strip_prefix("urn:schemas-upnp-org:service:") else {
        return false;
    };
    let Some((name, version)) = rest.split_once(':') else {
        return false;
    };
    matches!(name, "WANIPConnection" | "WANPPPConnection")
        && version.len() == 1
        && version.bytes().all(|b| b.is_ascii_digit())
}

/// A SOAP request for `action` on `service` (checked by [`wan_service`]) with `args`, which
/// are Veil's own numbers, addresses and words, never text from the network.
pub fn soap(service: &str, action: &str, args: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (name, value) in args {
        body.push_str(&format!("<{name}>{value}</{name}>"));
    }
    format!(
        "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{service}\">{body}</u:{action}></s:Body></s:Envelope>\r\n"
    )
}

/// The UPnP error code in a SOAP fault, such as 718 for a port already mapped.
pub fn error_code(body: &str) -> Option<u16> {
    element(body, "errorCode")?.parse().ok()
}

/// The text of the first element called `name` (any namespace prefix), entities decoded.
pub fn element(text: &str, name: &str) -> Option<String> {
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest.find('>')?;
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        let tag_name = tag.split_whitespace().next().unwrap_or("");
        let local = tag_name.rsplit(':').next().unwrap_or(tag_name);
        if local == name && !tag.ends_with('/') {
            let content = &rest[..rest.find('<').unwrap_or(rest.len())];
            return Some(decode(content.trim()));
        }
    }
    None
}

/// Each `<name>...</name>` block, by local name, outermost only.
fn blocks<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut rest = text;
    std::iter::from_fn(move || {
        let start = rest.find(&open)? + open.len();
        let len = rest[start..].find(&close)?;
        let block = &rest[start..start + len];
        rest = &rest[start + len + close.len()..];
        Some(block)
    })
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
 <device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
  <deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
   <deviceList><device><serviceList>
    <service>
     <serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType>
     <controlURL>/upnp/control/WANPPPConn1</controlURL>
    </service>
    <service>
     <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
     <serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
     <controlURL>upnp/control/WANIPConn1?a=1&amp;b=2</controlURL>
    </service>
   </serviceList></device></deviceList>
  </device></deviceList>
 </device>
</root>"#;

    #[test]
    fn finds_the_wan_ip_service() {
        let (service, control) = wan_service(DESCRIPTION).unwrap_or_default();
        assert_eq!(service, "urn:schemas-upnp-org:service:WANIPConnection:1");
        assert_eq!(control, "upnp/control/WANIPConn1?a=1&b=2");
        assert_eq!(wan_service("<root></root>"), None);
        let evil = DESCRIPTION.replace(
            "WANIPConnection:1</serviceType>",
            "WANIPConnection:1\"><x>\r\n</serviceType>",
        );
        let (service, _) = wan_service(&evil).unwrap_or_default();
        assert!(
            service.contains("WANPPPConnection"),
            "the odd one is skipped"
        );
    }

    #[test]
    fn router_urls() {
        let base = RouterUrl::parse("http://192.168.1.1:5000/rootDesc.xml").unwrap();
        assert_eq!(base.to_string(), "http://192.168.1.1:5000/rootDesc.xml");
        assert_eq!(
            base.join("ctl/IPConn").map(|u| u.to_string()),
            Some("http://192.168.1.1:5000/ctl/IPConn".into())
        );
        assert_eq!(
            base.join("/ctl").map(|u| u.to_string()),
            Some("http://192.168.1.1:5000/ctl".into())
        );
        assert!(base.join("http://192.168.1.1:5000/x").is_some());
        assert!(
            base.join("http://192.168.1.66:5000/x").is_none(),
            "another host"
        );
        assert!(
            base.join("http://192.168.1.1:80/x").is_none(),
            "another port"
        );
        assert!(base.join("https://192.168.1.1:5000/x").is_none());
        assert!(base.join("/a b").is_none());
        assert_eq!(
            RouterUrl::parse("http://192.168.1.1/").map(|u| u.port),
            Some(80)
        );
        for bad in [
            "https://192.168.1.1/",
            "http://router.lan/",
            "http://user@192.168.1.1/",
            "http://192.168.1.1:0/",
            "ftp://192.168.1.1/",
        ] {
            assert!(RouterUrl::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn ssdp_answers() {
        let answer = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nST: \
                      urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nLocation: \
                      http://192.168.1.1:5000/rootDesc.xml\r\n\r\n";
        assert_eq!(
            ssdp_location(answer),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        assert_eq!(ssdp_location("NOTIFY * HTTP/1.1\r\nLOCATION: x\r\n"), None);
    }

    #[test]
    fn soap_bodies_and_answers() {
        let body = soap(
            "urn:schemas-upnp-org:service:WANIPConnection:1",
            "GetExternalIPAddress",
            &[],
        );
        assert!(body.contains(
            "<u:GetExternalIPAddress xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">"
        ));
        let answer = "<s:Envelope><s:Body><u:GetExternalIPAddressResponse>\
                      <NewExternalIPAddress>81.2.69.160</NewExternalIPAddress>\
                      </u:GetExternalIPAddressResponse></s:Body></s:Envelope>";
        assert_eq!(
            element(answer, "NewExternalIPAddress").as_deref(),
            Some("81.2.69.160")
        );
        let fault = "<s:Fault><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
                     <errorCode>718</errorCode><errorDescription>ConflictInMappingEntry\
                     </errorDescription></UPnPError></detail></s:Fault>";
        assert_eq!(error_code(fault), Some(718));
        assert_eq!(error_code("<a/>"), None);
    }
}
