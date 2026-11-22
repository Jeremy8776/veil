//! Member side: join a forwarder's room, then use the link like a direct one.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::Mutex;
use veil_net::{FramedRecv, FramedSend, Link, Node};
use veil_security::PublicId;

use crate::ForwardError;
use crate::protocol::{ClientMsg, ServerMsg};
use crate::subscribe::Wanted;

const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// A joined forwarder link. Send sealed frames with `link().send_frame` and audio with
/// `link().send_datagram`; every other member of the room receives them.
pub struct ForwarderClient {
    link: Link,
    members: u16,
    // Held so the control stream stays open: the forwarder treats its end as leaving.
    control: Mutex<FramedSend>,
    input: Mutex<FramedRecv>,
}

impl ForwarderClient {
    /// Verifies reachability, role and admission before a call retires working routes.
    /// This check has one deadline over dialing, stream opening, writing and reading. It
    /// never joins a room or displaces this device's existing forwarding link. Availability
    /// is only a snapshot, not a reservation; a subsequent join can still fail.
    pub async fn probe(
        node: &Node,
        addrs: &[SocketAddr],
        forwarder: PublicId,
        ticket: &[u8],
    ) -> Result<(), ForwardError> {
        let check = async {
            let link = node.connect(addrs, forwarder).await?;
            let result = async {
                let (mut out, mut input) = link.open_control().await?.split();
                out.send(&ClientMsg::Probe {
                    ticket: ticket.to_vec(),
                })
                .await?;
                match input.recv::<ServerMsg>().await? {
                    ServerMsg::Available => Ok(()),
                    ServerMsg::Refused(why) => Err(ForwardError::Refused(why)),
                    _ => Err(ForwardError::Closed),
                }
            }
            .await;
            link.close("forwarder check complete");
            result
        };
        tokio::time::timeout(PROBE_TIMEOUT, check)
            .await
            .map_err(|_| ForwardError::Timeout)?
    }

    /// Connects to a forwarder, pinned to its key, and joins the room a sealed
    /// [`crate::RosterTicket`] names.
    pub async fn join(
        node: &Node,
        addrs: &[SocketAddr],
        forwarder: PublicId,
        ticket: &[u8],
    ) -> Result<Self, ForwardError> {
        // The peer controls stream credit as well as the reply. Cover every stage, not
        // only reading Joined, so a forwarder that passed a probe cannot later park a
        // topology transition indefinitely by withholding its control stream.
        tokio::time::timeout(
            veil_net::endpoint::CONNECT_TIMEOUT + ANSWER_TIMEOUT,
            Self::join_inner(node, addrs, forwarder, ticket),
        )
        .await
        .map_err(|_| ForwardError::Timeout)?
    }

    async fn join_inner(
        node: &Node,
        addrs: &[SocketAddr],
        forwarder: PublicId,
        ticket: &[u8],
    ) -> Result<Self, ForwardError> {
        let link = node.connect(addrs, forwarder).await?;
        let (mut out, mut input) = link.open_control().await?.split();
        out.send(&ClientMsg::Join {
            ticket: ticket.to_vec(),
        })
        .await?;
        let answer = tokio::time::timeout(ANSWER_TIMEOUT, input.recv::<ServerMsg>())
            .await
            .map_err(|_| ForwardError::Timeout)??;
        match answer {
            ServerMsg::Joined { members } => Ok(Self {
                link,
                members,
                control: Mutex::new(out),
                input: Mutex::new(input),
            }),
            ServerMsg::Refused(refusal) => Err(ForwardError::Refused(refusal)),
            ServerMsg::KeyframeRequest | ServerMsg::App(_) | ServerMsg::Available => {
                Err(ForwardError::Closed)
            }
        }
    }

    /// Hands the forwarder the host's ticket for a later epoch. It drops members the new
    /// ticket no longer lists.
    pub async fn update_ticket(&self, ticket: &[u8]) -> Result<(), ForwardError> {
        let message = ClientMsg::Ticket(ticket.to_vec());
        Ok(self.control.lock().await.send(&message).await?)
    }

    /// Asks for at most these layers from each named sender. Raising takes effect at that
    /// sender's next keyframe; lowering at once.
    pub async fn subscribe(&self, request: &[(PublicId, Wanted)]) -> Result<(), ForwardError> {
        let message = ClientMsg::Subscribe(request.to_vec());
        Ok(self.control.lock().await.send(&message).await?)
    }

    /// Tells the forwarder this member needs a keyframe from `sender`.
    pub async fn request_keyframe(&self, sender: PublicId) -> Result<(), ForwardError> {
        let message = ClientMsg::KeyframeRequest(sender);
        Ok(self.control.lock().await.send(&message).await?)
    }

    /// Sends a sealed app message to every other member of the room.
    pub async fn send_app(&self, sealed: &[u8]) -> Result<(), ForwardError> {
        let message = ClientMsg::App(sealed.to_vec());
        Ok(self.control.lock().await.send(&message).await?)
    }

    /// The forwarder's next message after joining, such as [`ServerMsg::KeyframeRequest`].
    /// Read it in one task; an error means the link has closed.
    pub async fn recv(&self) -> Result<ServerMsg, ForwardError> {
        Ok(self.input.lock().await.recv::<ServerMsg>().await?)
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Members in the room when this member joined, including itself.
    pub fn members_at_join(&self) -> u16 {
        self.members
    }
}
