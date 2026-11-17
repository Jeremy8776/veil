//! The host's reachability check: can someone outside dial the call's public address?
//!
//! Purpose: once the router opens the call's port, or when the person set a public address
//! by hand, the host asks its public node (Settings > Connection > Rendezvous and relay) to
//! dial that address back (`veil_rendezvous::reach`), and People shows the answer. Without a
//! public node nothing is checked: the host's own rendezvous sees its own network only.
//!
//! Invariants:
//! - One check at a time. A newer address replaces a check still running.
//! - [`Checker::next`] is cancel-safe for `tokio::select!`: the check runs on a task of its
//!   own, and awaiting its handle can be repeated.
//! - A check that could not run (an old or unreachable node) shows nothing, not "unreachable".

use std::future::Future;
use std::net::SocketAddr;

use tokio::task::JoinHandle;
use veil_call::CallError;
use veil_rendezvous::Reach;

use crate::model::Reachable;

type Check = JoinHandle<Result<Reach, CallError>>;

#[derive(Default)]
pub struct Checker {
    running: Option<(SocketAddr, Check)>,
}

impl Checker {
    /// Starts checking `expected` with `check`, replacing any check still running.
    pub fn start(
        &mut self,
        expected: SocketAddr,
        check: impl Future<Output = Result<Reach, CallError>> + Send + 'static,
    ) {
        if let Some((_, old)) = self.running.take() {
            old.abort();
        }
        self.running = Some((expected, tokio::spawn(check)));
    }

    /// The next answer. Pending for ever while no check runs.
    pub async fn next(&mut self) -> Reachable {
        let Some((expected, task)) = &mut self.running else {
            return std::future::pending().await;
        };
        let expected = *expected;
        let result = task.await;
        self.running = None;
        let result = match result {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = &result {
            crate::note!("reach: could not check {expected} ({e})");
        }
        verdict(expected, result)
    }
}

/// A check still running when the call ends is stopped with it.
impl Drop for Checker {
    fn drop(&mut self) {
        if let Some((_, task)) = self.running.take() {
            task.abort();
        }
    }
}

/// What People says about `expected`, given the public node's answer.
pub fn verdict(expected: SocketAddr, result: Result<Reach, String>) -> Reachable {
    let Ok(reach) = result else {
        return Reachable::Unknown;
    };
    if reach.addr.ip() != expected.ip() {
        return Reachable::No(format!(
            "Your public node sees this computer at {}, not {}. Your internet provider probably \
             shares one public address between customers, so guests on the internet cannot \
             dial you directly. They still join through your public node's relay.",
            reach.addr.ip(),
            expected.ip()
        ));
    }
    if reach.open {
        Reachable::Yes(format!(
            "Your public node reached {} from outside. Guests on the internet can dial you \
             directly.",
            reach.addr
        ))
    } else {
        Reachable::No(format!(
            "Your public node could not reach {}. A firewall on this computer or the router may \
             be blocking UDP port {}. Guests still join through your public node's relay, \
             which is slower.",
            reach.addr,
            reach.addr.port()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::from((ip, port))
    }

    #[test]
    fn open_at_the_expected_address_is_yes() {
        let at = addr([203, 0, 113, 9], 7710);
        let v = verdict(
            at,
            Ok(Reach {
                addr: at,
                open: true,
            }),
        );
        assert!(
            matches!(&v, Reachable::Yes(t) if t.contains("203.0.113.9:7710")),
            "{v:?}"
        );
    }

    #[test]
    fn closed_names_the_port() {
        let at = addr([203, 0, 113, 9], 7710);
        let v = verdict(
            at,
            Ok(Reach {
                addr: at,
                open: false,
            }),
        );
        assert!(
            matches!(&v, Reachable::No(t) if t.contains("UDP port 7710")),
            "{v:?}"
        );
    }

    #[test]
    fn another_public_ip_means_a_shared_address() {
        let router = addr([100, 64, 3, 4], 7710);
        let seen = addr([203, 0, 113, 9], 7710);
        // Even if the dial got through (a NAT that lets anything in), the router's port is not
        // what guests would dial.
        let v = verdict(
            router,
            Ok(Reach {
                addr: seen,
                open: true,
            }),
        );
        assert!(
            matches!(&v, Reachable::No(t) if t.contains("100.64.3.4") && t.contains("relay")),
            "{v:?}"
        );
    }

    #[test]
    fn a_check_that_could_not_run_shows_nothing() {
        let at = addr([203, 0, 113, 9], 7710);
        assert_eq!(verdict(at, Err("closed".into())), Reachable::Unknown);
    }
}
