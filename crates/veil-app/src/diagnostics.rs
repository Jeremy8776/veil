//! Local diagnostic evidence and an allow-listed export. Never ingest log lines, settings
//! strings, invitations, names, conversation content or cryptographic session material.

use serde::Serialize;
use veil_call::{CallError, node::HelperIssue};
use veil_net::{NetError, NodeAddr};
use veil_security::PublicId;

pub mod review;
#[cfg(test)]
mod tests;

pub const MAX_PEERS: usize = 128;
const MAX_ENDPOINTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    Identity,
    NoUdpReply,
    NoRoute,
    HelperUnavailable,
    HostNotAllowed,
    InviteUnavailable,
    InviteExpired,
    Declined,
    TokenUsed,
    CallFull,
    Removed,
    Recovery,
    PermissionDenied,
    DeviceUnavailable,
    Overloaded,
    Settings,
    NetworkDenied,
    Other,
}

impl Problem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Identity => "Identity could not be verified",
            Self::NoUdpReply => "No verified UDP reply",
            Self::NoRoute => "No permitted route available",
            Self::HelperUnavailable => "Helper not reached",
            Self::HostNotAllowed => "This device is not allowed to host",
            Self::InviteUnavailable => "Invitation could not be retrieved",
            Self::InviteExpired => "Invitation expired or invalid",
            Self::Declined => "The host declined admission",
            Self::TokenUsed => "Invitation already used",
            Self::CallFull => "The call is full",
            Self::Removed => "The host removed this device",
            Self::Recovery => "Host connection could not be restored",
            Self::PermissionDenied => "Capture permission denied",
            Self::DeviceUnavailable => "Capture device unavailable",
            Self::Overloaded => "The call could not keep up",
            Self::Settings => "Connection choices cannot be used together",
            Self::NetworkDenied => "Network access denied by the system",
            Self::Other => "The call stopped before this step completed",
        }
    }
    pub fn action(self) -> &'static str {
        match self {
            Self::Identity => {
                "Compare the configured device key through a trusted channel. Do not disable verification."
            }
            Self::NoUdpReply => {
                "Check the host is running and permits incoming Veil traffic. UDP filtering, NAT and an unavailable endpoint can look alike; this does not prove which one failed. Veil has no TCP fallback yet."
            }
            Self::NoRoute => {
                "Use a reachable full invitation on the local network or a chosen relay that permits this call. Keep your privacy policy enabled."
            }
            Self::HelperUnavailable => {
                "Check the chosen helper and its address. A reachable helper alone does not prove permission to host or working media."
            }
            Self::HostNotAllowed => {
                "Send this device's public key to the chosen helper's owner and ask them to allow hosting."
            }
            Self::InviteUnavailable | Self::InviteExpired | Self::TokenUsed => {
                "Ask the host for a fresh full invitation. Full invitations do not need short-link retrieval."
            }
            Self::Declined => "Contact the host before asking to join again.",
            Self::CallFull => {
                "Retrying will fail until a seat is free. Ask the host to free one, or join another call."
            }
            Self::Removed => {
                "Removal revokes this membership. Rejoining requires a new invitation and admission."
            }
            Self::Recovery => {
                "Check the network, then request a fresh invitation if the host is still calling."
            }
            Self::PermissionDenied => {
                "Allow camera or microphone access in system settings, or continue with capture off."
            }
            Self::DeviceUnavailable => {
                "Check the selected device and system permissions, or choose another device."
            }
            Self::Overloaded => {
                "Leave and start or join a new call. Critical membership events were not silently skipped."
            }
            Self::Settings => {
                "Review Connection settings. Local only and Hide my address must both be honoured."
            }
            Self::NetworkDenied => {
                "Check the system's network permissions and firewall policy for Veil."
            }
            Self::Other => {
                "Review the local connection evidence and try again. Share only the previewed report if you need help."
            }
        }
    }
    pub fn net(error: &NetError) -> Self {
        match error {
            NetError::Identity => Self::Identity,
            NetError::NoAddresses => Self::NoRoute,
            NetError::Timeout | NetError::Connect(_) => Self::NoUdpReply,
            NetError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                Self::NetworkDenied
            }
            _ => Self::Other,
        }
    }
    pub fn call(error: &CallError) -> Self {
        use veil_net::control::RefuseReason as Refused;
        use veil_rendezvous::{Refusal, RendezvousError};
        match error {
            CallError::Net(e) => Self::net(e),
            CallError::Timeout | CallError::Stalled => Self::NoUdpReply,
            CallError::Rendezvous(RendezvousError::Net(e)) => Self::net(e),
            CallError::Rendezvous(RendezvousError::Refused(Refusal::NotAllowed)) => {
                Self::HostNotAllowed
            }
            CallError::Rendezvous(RendezvousError::Refused(Refusal::RelayDisabled)) => {
                Self::NoRoute
            }
            CallError::Rendezvous(RendezvousError::Timeout) => Self::NoUdpReply,
            CallError::Rendezvous(_) => Self::HelperUnavailable,
            CallError::Refused(Refused::DeclinedByHost) => Self::Declined,
            CallError::Refused(Refused::TokenUsed) => Self::TokenUsed,
            CallError::Refused(Refused::UnknownRoom) => Self::InviteUnavailable,
            CallError::Refused(Refused::RoomFull) => Self::CallFull,
            CallError::Plan(_) => Self::NoRoute,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Unchecked,
    Waiting,
    Passed,
    Skipped,
    Failed(Problem),
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unchecked => "Not observed",
            Self::Waiting => "Waiting",
            Self::Passed => "Confirmed",
            Self::Skipped => "Not needed",
            Self::Failed(p) => p.label(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Socket,
    Invitation,
    Helper,
    Forwarder,
    Hosting,
    HostConnection,
    Admission,
    Membership,
}
impl Step {
    pub const ALL: [Self; 8] = [
        Self::Socket,
        Self::Invitation,
        Self::Helper,
        Self::Forwarder,
        Self::Hosting,
        Self::HostConnection,
        Self::Admission,
        Self::Membership,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Socket => "Local call socket",
            Self::Invitation => "Invitation",
            Self::Helper => "Helper connection",
            Self::Forwarder => "Forwarder check",
            Self::Hosting => "Permission to host",
            Self::HostConnection => "Host connection",
            Self::Admission => "Host admission",
            Self::Membership => "Call membership",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capture {
    #[default]
    Off,
    Starting,
    Active,
    PermissionDenied,
    Unavailable,
}
impl Capture {
    pub fn with_permission(self, wanted: bool, granted: bool) -> Self {
        if wanted && !granted {
            Self::PermissionDenied
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Scope {
    pub host: bool,
    pub local_only: bool,
    pub relay_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum End {
    Left,
    HostEnded,
    Removed,
    Failed,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub scope: Scope,
    pub started: bool,
    pub ended: bool,
    pub end: Option<End>,
    pub reconnecting: bool,
    pub steps: [State; 8],
    pub problem: Option<Problem>,
    pub endpoints: Vec<(bool, NodeAddr)>,
}
impl Report {
    pub fn set(&mut self, step: Step, state: State) {
        self.steps[step as usize] = state;
        self.problem = self.steps.iter().rev().find_map(|state| {
            if let State::Failed(problem) = state {
                Some(*problem)
            } else {
                None
            }
        });
    }
    pub fn endpoint(&mut self, helper: bool, addr: NodeAddr) {
        if self.endpoints.len() < MAX_ENDPOINTS
            && !self.endpoints.iter().any(|(_, old)| *old == addr)
        {
            self.endpoints.push((helper, addr));
        }
    }
    pub fn helper(&mut self, connected: bool, issue: Option<HelperIssue>, needed: bool) {
        let state = if connected {
            State::Passed
        } else if !needed {
            State::Skipped
        } else {
            match issue {
                Some(HelperIssue::Identity) => State::Failed(Problem::Identity),
                Some(HelperIssue::Refused(_)) => State::Passed,
                Some(HelperIssue::NoReply) => State::Failed(Problem::NoUdpReply),
                Some(HelperIssue::NoRoute) => State::Failed(Problem::NoRoute),
                _ => State::Failed(Problem::HelperUnavailable),
            }
        };
        self.set(Step::Helper, state);
    }
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub id: u32,
    pub device: PublicId,
    pub route: crate::model::LinkPath,
    pub audio_recent: bool,
    pub video_recent: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub report: Report,
    pub own_device: Option<PublicId>,
    pub peers: Vec<Peer>,
    pub camera: Capture,
    pub microphone: Capture,
    pub sampled: bool,
    pub rtt_ms: Option<u64>,
    pub down_kbps: Option<u32>,
    pub up_kbps: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Include {
    pub addresses: bool,
    pub device_keys: bool,
}

impl Snapshot {
    pub fn problem(&self) -> Option<Problem> {
        self.report.problem.or_else(|| {
            if [self.camera, self.microphone].contains(&Capture::PermissionDenied) {
                Some(Problem::PermissionDenied)
            } else if [self.camera, self.microphone].contains(&Capture::Unavailable) {
                Some(Problem::DeviceUnavailable)
            } else {
                None
            }
        })
    }
    pub fn media_label(&self) -> &'static str {
        if self.peers.is_empty() {
            "No other admitted participants"
        } else if self.peers.iter().any(|p| p.audio_recent || p.video_recent) {
            "Recent decoded media received"
        } else {
            "No recent decoded media observed"
        }
    }
    pub fn media_action(&self) -> &'static str {
        "Ask the other participants whether they are sending audio or video. A media link or shared keys alone do not prove delivery; silent or disabled capture may be intentional. This report cannot prove what another device receives."
    }
    /// Deliberately construct each field. Free-form app/model/log strings cannot enter here.
    pub fn export(&self, include: Include) -> String {
        use serde_json::json;
        let steps: Vec<_> = Step::ALL
            .iter()
            .map(|step| {
                let state = self.report.steps[*step as usize];
                json!({"step":step.label(), "evidence":state.label(), "action":match state {
                State::Failed(problem) => Some(problem.action()), _ => None }})
            })
            .collect();
        let peers: Vec<_> = self.peers.iter().take(MAX_PEERS).map(|peer| json!({
            "participant":peer.id, "route":peer.route.label(), "recent_decoded_audio":peer.audio_recent,
            "recent_decoded_video":peer.video_recent,
            "device_public_key":include.device_keys.then(|| veil_net::node_addr::key_text(&peer.device))
        })).collect();
        let endpoints: Vec<_> = self.report.endpoints.iter().take(MAX_ENDPOINTS).map(|(helper, addr)| json!({
            "role":if *helper { "helper" } else { "host" },
            "address":include.addresses.then(|| addr.addr.to_string()),
            "device_public_key":include.device_keys.then(|| veil_net::node_addr::key_text(&addr.key))
        })).collect();
        let value = json!({"format":"veil-diagnostics-v1", "version":env!("CARGO_PKG_VERSION"),
            "platform":std::env::consts::OS, "architecture":std::env::consts::ARCH,
            "source":if self.report.started {"call"} else {"connection_check"},
            "scope":self.report.scope, "ended":self.report.ended, "end":self.report.end,"reconnecting":self.report.reconnecting,
            "privacy":{"addresses_included":include.addresses,"device_public_keys_included":include.device_keys,
                "raw_logs_included":false,"conversation_content_included":false},
            "local_device_public_key":include.device_keys.then(|| self.own_device.map(|key| veil_net::node_addr::key_text(&key))).flatten(),
            "steps":steps, "problem":self.problem(), "media_evidence":self.media_label(),
            "media_guidance":self.media_action(), "camera":self.camera,"microphone":self.microphone,
            "stats":{"sampled":self.sampled,"max_rtt_ms":self.rtt_ms,"down_kbps":self.down_kbps,"up_kbps":self.up_kbps},
            "participants":peers,"endpoints":endpoints});
        // This contains only enums, fixed labels, integers and bounded typed identifiers.
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| {
            "{\"format\":\"veil-diagnostics-v1\",\"error\":\"report_unavailable\"}".into()
        })
    }
}
