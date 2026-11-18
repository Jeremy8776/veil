//! The host's IPv6 pinhole for one call: asked for, renewed, removed at the end.
//!
//! Purpose: wraps `veil_portmap::Pinhole` for `host.rs`, beside `router.rs` (the IPv4 port).
//! The call's IPv6 address is in the invite from the start; the pinhole only lets guests'
//! first packets through the router's firewall. [`V6Port::next`] says once whether it opened,
//! which is when the host checks its IPv6 address from outside.
//!
//! Invariants:
//! - Asked for only when the person turned port opening on, as for the IPv4 port.
//! - [`V6Port::next`] is cancel-safe for `tokio::select!`: a renewal interrupted half way is
//!   simply tried again.
//! - A pinhole that fails or stops renewing is logged only. People keeps showing the IPv4
//!   port; the reachability check shows whether IPv6 guests can dial in.

use std::net::Ipv6Addr;

use tokio::task::JoinHandle;
use veil_portmap::{CLOSE_LIMIT, Pinhole};

pub struct V6Port {
    opening: Option<JoinHandle<Result<Pinhole, String>>>,
    pinhole: Option<Pinhole>,
}

impl V6Port {
    /// Starts asking for a pinhole to `ip` at `port` if `enabled`.
    pub fn start(enabled: bool, ip: Option<Ipv6Addr>, port: u16) -> Option<Self> {
        let ip = ip.filter(|_| enabled)?;
        Some(Self {
            opening: Some(tokio::spawn(Pinhole::open(ip, port))),
            pinhole: None,
        })
    }

    /// Once: whether the pinhole opened. Then pending for ever, renewing it while it lasts.
    pub async fn next(&mut self) -> bool {
        if let Some(opening) = &mut self.opening {
            let result = opening.await;
            self.opening = None;
            return match result {
                Ok(Ok(pinhole)) => {
                    crate::note!("router: PCP opened IPv6 {}", pinhole.external());
                    self.pinhole = Some(pinhole);
                    true
                }
                Ok(Err(e)) => {
                    crate::note!("router: no IPv6 pinhole ({e})");
                    false
                }
                Err(e) => {
                    crate::note!("router: asking for an IPv6 pinhole stopped ({e})");
                    false
                }
            };
        }
        loop {
            let Some(at) = self.pinhole.as_ref().map(Pinhole::renew_at) else {
                return std::future::pending().await;
            };
            tokio::time::sleep_until(at).await;
            let Some(pinhole) = &mut self.pinhole else {
                continue;
            };
            if let Err(e) = pinhole.renew().await {
                crate::note!("router: IPv6 pinhole renewal failed ({e})");
                self.pinhole = None;
            }
        }
    }

    /// Removes the pinhole, if there is one, within `CLOSE_LIMIT`.
    pub async fn close(self) {
        if let Some(opening) = self.opening {
            opening.abort();
        }
        if let Some(pinhole) = self.pinhole {
            let _ = tokio::time::timeout(CLOSE_LIMIT, pinhole.close()).await;
            crate::note!("router: IPv6 pinhole closed");
        }
    }
}
