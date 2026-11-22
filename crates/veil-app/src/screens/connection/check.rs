//! On-demand connection checks. No call, registration, router mapping or saved identity.
//! Saved Local only remains a restriction until the person saves a different policy.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::sync::oneshot;
use veil_net::{Node, NodeName};
use veil_security::Identity;

use crate::connection::{Field, Settings};
use crate::engine::plan::NetConfig;

const SERVER_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Note,
}

#[derive(Debug)]
pub struct Row {
    pub label: &'static str,
    pub detail: String,
    pub outcome: Outcome,
}

#[derive(Default)]
pub struct Checker {
    receiver: Option<mpsc::Receiver<Vec<Row>>>,
    cancel: Option<oneshot::Sender<()>>,
    input: Option<Settings>,
    saved_local: bool,
    pub result: Option<Vec<Row>>,
    config: Option<NetConfig>,
    finished_at: Option<Instant>,
}

impl Drop for Checker {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Checker {
    pub fn running(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn clear(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        self.receiver = None;
        self.input = None;
        self.result = None;
        self.config = None;
        self.finished_at = None;
    }

    /// A changed draft invalidates results and cancels further network activity.
    pub fn refresh(&mut self, draft: &Settings, saved_local: bool) {
        if self
            .input
            .as_ref()
            .is_some_and(|input| input != draft || self.saved_local != saved_local)
        {
            self.clear();
        }
        let answer = self.receiver.as_ref().map(mpsc::Receiver::try_recv);
        match answer {
            Some(Ok(rows)) => {
                self.finished_at = Some(Instant::now());
                self.result = Some(rows);
                self.receiver = None;
                self.cancel = None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.result = Some(vec![row(
                    "Connection check",
                    Outcome::Failed,
                    "The check stopped. Try again.",
                )]);
                self.receiver = None;
                self.cancel = None;
            }
            _ => {}
        }
    }

    pub fn start(
        &mut self,
        draft: &Settings,
        saved_local: bool,
        ctx: egui::Context,
    ) -> Result<(), Vec<(Field, String)>> {
        let config = config_for(draft, saved_local, |name| std::env::var(name).ok())?;
        self.clear();
        self.config = Some(config.clone());
        self.input = Some(draft.clone());
        self.saved_local = saved_local;
        let (send, receive) = mpsc::channel();
        let (cancel, cancelled) = oneshot::channel();
        self.receiver = Some(receive);
        self.cancel = Some(cancel);
        let started = std::thread::Builder::new()
            .name("connection-check".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancelled => None,
                            rows = run(config) => Some(rows),
                        }
                    }),
                    Err(why) => Some(vec![row(
                        "Connection check",
                        Outcome::Failed,
                        why.to_string(),
                    )]),
                };
                if let Some(rows) = result {
                    let _ = send.send(rows);
                }
                ctx.request_repaint();
            });
        if let Err(why) = started {
            self.clear();
            self.result = Some(vec![row(
                "Connection check",
                Outcome::Failed,
                why.to_string(),
            )]);
        }
        Ok(())
    }

    pub fn report(&self) -> Option<crate::connection::Checked> {
        let rows = self.result.as_ref()?;
        let servers: Vec<_> = rows
            .iter()
            .filter(|row| matches!(row.label, "Rendezvous" | "Forwarder"))
            .collect();
        Some(crate::connection::Checked {
            config: self.config.clone()?,
            at: self.finished_at?,
            servers_reachable: (!servers.is_empty())
                .then(|| servers.iter().all(|row| row.outcome == Outcome::Passed)),
        })
    }
}

fn row(label: &'static str, outcome: Outcome, detail: impl Into<String>) -> Row {
    Row {
        label,
        detail: detail.into(),
        outcome,
    }
}

fn config_for(
    draft: &Settings,
    saved_local: bool,
    get: impl Fn(&str) -> Option<String>,
) -> Result<NetConfig, Vec<(Field, String)>> {
    let mut settings = draft.clone();
    settings.local_only |= saved_local;
    settings.check().map(|config| config.with_env(get))
}

async fn run(config: NetConfig) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(problem) = config.conflict() {
        rows.push(row("Settings", Outcome::Failed, problem));
        return rows;
    }
    rows.push(row("Settings", Outcome::Passed, "Valid"));
    let identity = Identity::generate();
    let bound = if config.local_only {
        Node::bind_local(&identity, config.port)
    } else {
        Node::bind_any(&identity, config.port)
    };
    let node = match bound {
        Ok(node) => node,
        Err(why) => {
            rows.push(row("Call port", Outcome::Failed, why.to_string()));
            return rows;
        }
    };
    rows.push(row(
        "Call port",
        Outcome::Passed,
        "UDP socket opened successfully",
    ));
    let local = veil_net::local::local_candidates(config.port);
    rows.push(if local.is_empty() {
        row("Local network", Outcome::Note, "No local address found")
    } else {
        row("Local network", Outcome::Passed, "Local address available")
    });
    if config.local_only {
        rows.push(row(
            "Internet checks",
            Outcome::Note,
            "Skipped in Local only",
        ));
    } else {
        let servers = [
            ("Rendezvous", config.rendezvous),
            ("Forwarder", config.forwarder),
        ];
        if servers.iter().all(|(_, server)| server.is_none()) {
            rows.push(row(
                "Servers",
                Outcome::Note,
                "No external servers configured",
            ));
        }
        for (label, server) in servers {
            if let Some(server) = server {
                rows.push(check_server(&node, label, server).await);
            }
        }
    }
    node.close();
    rows
}

async fn check_server(node: &Node, label: &'static str, server: NodeName) -> Row {
    let started = Instant::now();
    let answer = tokio::time::timeout(SERVER_TIMEOUT, async {
        let server = server.resolve().await.map_err(|why| why.to_string())?;
        let link = node
            .connect(&[server.addr], server.key)
            .await
            .map_err(|why| why.to_string())?;
        link.close("connection check complete");
        Ok::<_, String>(())
    })
    .await;
    match answer {
        Ok(Ok(())) => row(
            label,
            Outcome::Passed,
            format!(
                "Reached with verified key in {} ms",
                started.elapsed().as_millis()
            ),
        ),
        Ok(Err(why)) => row(label, Outcome::Failed, why),
        Err(_) => row(
            label,
            Outcome::Failed,
            "No verified connection within 6 seconds",
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn saved_local_only_prevents_unsaved_internet_tests_and_overrides() {
        let draft = Settings {
            rendezvous: "not a server".into(),
            ..Default::default()
        };
        let config = config_for(&draft, true, |_| Some("bad override".into()));
        assert!(config.is_ok());
        let config = config.unwrap_or_default();
        assert!(config.local_only);
        assert!(config.rendezvous.is_none());
        assert!(config.forwarder.is_none());
        assert!(!config.open_port);
    }

    #[tokio::test]
    async fn occupied_call_port_reports_failure() {
        let busy = Node::bind_any(&Identity::generate(), 0).expect("test UDP socket");
        let port = busy.local_addr().expect("bound address").port();
        // A fallback to the other address family is valid; occupy it too when
        // Windows allows a separate IPv4 bind alongside the IPv6 endpoint.
        let _other_family = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).ok();
        let rows = run(NetConfig {
            port,
            ..Default::default()
        })
        .await;
        assert!(
            rows.iter()
                .any(|row| row.label == "Call port" && row.outcome == Outcome::Failed),
            "{rows:?}",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_check_uses_a_real_key_pinned_connection() {
        let server = Node::bind(
            &Identity::generate(),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .expect("test server");
        let address = server.local_addr().expect("address");
        let text = format!("{}@{address}", veil_net::node_addr::key_text(&server.id()));
        let config = Settings {
            rendezvous: text,
            ..Default::default()
        }
        .check()
        .expect("test settings");
        let rows = run(config).await;
        assert!(
            rows.iter()
                .any(|row| row.label == "Rendezvous" && row.outcome == Outcome::Passed),
            "{rows:?}",
        );
        server.close();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn server_check_rejects_an_incorrect_key() {
        let server = Node::bind(
            &Identity::generate(),
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .expect("test server");
        let node = Node::bind_any(&Identity::generate(), 0).expect("test client");
        let text = format!(
            "{}@{}",
            veil_net::node_addr::key_text(&Identity::generate().public()),
            server.local_addr().expect("address")
        );
        let answer =
            check_server(&node, "Rendezvous", text.parse().expect("test server name")).await;
        assert_eq!(answer.outcome, Outcome::Failed);
        server.close();
        node.close();
    }

    #[tokio::test]
    async fn conflicting_privacy_settings_never_open_a_socket() {
        let rows = run(NetConfig {
            local_only: true,
            relay_only: true,
            ..Default::default()
        })
        .await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Settings");
        assert_eq!(rows[0].outcome, Outcome::Failed);
    }
}
