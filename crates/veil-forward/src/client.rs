//! Member side: join a forwarder's room, then use the link like a direct one.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::Mutex;
use veil_net::{FramedRecv, FramedSend, Link, Node};
use veil_security::PublicId;

use crate::ForwardError;
use crate::subscribe::Wanted;
use crate::protocol::{ClientMsg, ServerMsg};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Connects to a forwarder, pinned to its key, and joins the room a sealed
    /// [`crate::RosterTicket`] names.
    pub async fn join(
        node: &Node,
        addrs: &[SocketAddr],
        forwarder: PublicId,
        ticket: &[u8],
    ) -> Result<Self, ForwardError> {
        let link = node.connect(addrs, forwarder).await?;
        let (mut out, mut input) = link.open_control().await?.split();
        out.send(&ClientMsg::Join { ticket: ticket.to_vec() }).await?;
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
            ServerMsg::KeyframeRequest => Err(ForwardError::Closed),
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
