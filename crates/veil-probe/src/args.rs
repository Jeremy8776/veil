//! Command line for `veil-probe`. Pure, so every form is tested without a network.

use std::net::SocketAddr;

use veil_net::NodeAddr;

pub const USAGE: &str = "\
usage:
  veil-probe host [--bind IP:PORT] [--advertise IP:PORT]... [--rendezvous KEY@IP:PORT]
                  [--forwarder KEY@IP:PORT]
  veil-probe join LINK [--bind IP:PORT] [--relay-only]

host  starts a call, prints an invite link and admits whoever uses it. --advertise is an
      address joiners can dial directly (your public IP and forwarded port). --rendezvous is a
      veil-node rendezvous role, used for hole punching and as the relay. --forwarder sends all
      media through a veil-node forwarder instead of a mesh.
join  joins the call in LINK. --relay-only never dials directly, so peers never see your address.

Both sides then send a sealed probe every second, as a video frame and as an audio datagram,
and print round-trip times, the path each link took and the reply rate.";

pub const DEFAULT_HOST_BIND: &str = "0.0.0.0:7710";
pub const DEFAULT_JOIN_BIND: &str = "0.0.0.0:0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Host(HostArgs),
    Join(JoinArgs),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostArgs {
    pub bind: SocketAddr,
    pub advertise: Vec<SocketAddr>,
    pub rendezvous: Option<NodeAddr>,
    pub forwarder: Option<NodeAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinArgs {
    pub link: String,
    pub bind: SocketAddr,
    pub relay_only: bool,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None | Some("-h" | "--help" | "help") => Ok(Command::Help),
        Some("host") => parse_host(args).map(Command::Host),
        Some("join") => parse_join(args).map(Command::Join),
        Some(other) => Err(format!("unknown command {other:?}")),
    }
}

fn parse_host(mut args: impl Iterator<Item = String>) -> Result<HostArgs, String> {
    let mut host = HostArgs {
        bind: socket(DEFAULT_HOST_BIND)?,
        advertise: Vec::new(),
        rendezvous: None,
        forwarder: None,
    };
    while let Some(flag) = args.next() {
        let value = value_of(&flag, args.next())?;
        match flag.as_str() {
            "--bind" => host.bind = socket(&value)?,
            "--advertise" => host.advertise.push(socket(&value)?),
            "--rendezvous" => host.rendezvous = Some(node(&value)?),
            "--forwarder" => host.forwarder = Some(node(&value)?),
            _ => return Err(format!("unknown option {flag:?}")),
        }
    }
    Ok(host)
}

fn parse_join(mut args: impl Iterator<Item = String>) -> Result<JoinArgs, String> {
    let link = args.next().ok_or("join needs an invite link")?;
    if link.starts_with("--") {
        return Err("join needs an invite link before any option".into());
    }
    let mut join = JoinArgs {
        link,
        bind: socket(DEFAULT_JOIN_BIND)?,
        relay_only: false,
    };
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--relay-only" => join.relay_only = true,
            "--bind" => join.bind = socket(&value_of(&flag, args.next())?)?,
            _ => return Err(format!("unknown option {flag:?}")),
        }
    }
    Ok(join)
}

fn value_of(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("{flag} needs a value"))
}

fn socket(text: &str) -> Result<SocketAddr, String> {
    text.parse().map_err(|_| format!("{text:?} is not IP:PORT"))
}

fn node(text: &str) -> Result<NodeAddr, String> {
    text.parse().map_err(|e| format!("{text:?}: {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use veil_security::Identity;

    use super::*;

    fn parse_str(line: &str) -> Result<Command, String> {
        parse(line.split_whitespace().map(String::from))
    }

    #[test]
    fn host_takes_every_option_and_repeats_advertise() {
        let node = NodeAddr {
            key: Identity::generate().public(),
            addr: socket("203.0.113.5:7700").unwrap(),
        };
        let line = format!(
            "host --bind 0.0.0.0:9000 --advertise 198.51.100.2:9000 --advertise [2001:db8::1]:9000 \
             --rendezvous {node} --forwarder {node}"
        );
        let Command::Host(host) = parse_str(&line).unwrap() else {
            panic!("wrong command")
        };
        assert_eq!(host.bind, socket("0.0.0.0:9000").unwrap());
        assert_eq!(host.advertise.len(), 2);
        assert_eq!((host.rendezvous, host.forwarder), (Some(node), Some(node)));
    }

    #[test]
    fn defaults_apply_when_options_are_left_out() {
        let Command::Host(host) = parse_str("host").unwrap() else {
            panic!("wrong command")
        };
        assert_eq!(host.bind, socket(DEFAULT_HOST_BIND).unwrap());
        let Command::Join(join) = parse_str("join veil:abc --relay-only").unwrap() else {
            panic!("wrong command")
        };
        assert_eq!((join.link.as_str(), join.relay_only), ("veil:abc", true));
        assert_eq!(join.bind, socket(DEFAULT_JOIN_BIND).unwrap());
    }

    #[test]
    fn bad_input_is_an_error_not_a_guess() {
        assert_eq!(parse_str("").unwrap(), Command::Help);
        assert!(parse_str("call").is_err());
        assert!(parse_str("host --bind").is_err());
        assert!(parse_str("host --bind example.com:80").is_err());
        assert!(
            parse_str("host --rendezvous 1.2.3.4:5").is_err(),
            "a node needs its key"
        );
        assert!(parse_str("join").is_err());
        assert!(parse_str("join --relay-only").is_err());
        assert!(parse_str("join veil:abc --loud").is_err());
    }
}
