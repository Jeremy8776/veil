//! Member side: join a forwarder's room, then use the link like a direct one.

use std::net::SocketAddr;
use std::time::Duration;

use veil_net::{FramedRecv, FramedSend, Link, Node};
use veil_security::PublicId;

use crate::ForwardError;
use crate::protocol::{ClientMsg, ServerMsg};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// A joined forwarder link. Send sealed frames with `link().send_frame` and audio with
/// `link().send_datagram`; every other member of the room receives them.
pub struct ForwarderClient {
    link: Link,
    members: u16,
    // Held so the control stream stays open: the forwarder treats its end as leaving.
    _control: (FramedSend, FramedRecv),
}

impl ForwarderClient {
    /// Connects to a forwarder, pinned to its key, and joins the room named by `room_tag`.
    pub async fn join(
        node: &Node,
        addrs: &[SocketAddr],
        forwarder: PublicId,
        room_tag: [u8; 32],
    ) -> Result<Self, ForwardError> {
        let link = node.connect(addrs, forwarder).await?;
        let (mut out, mut input) = link.open_control().await?.split();
        out.send(&ClientMsg::Join { room_tag }).await?;
        let answer = tokio::time::timeout(ANSWER_TIMEOUT, input.recv::<ServerMsg>())
            .await
            .map_err(|_| ForwardError::Timeout)??;
        match answer {
            ServerMsg::Joined { members } => Ok(Self {
                link,
                members,
                _control: (out, input),
            }),
            ServerMsg::Refused(refusal) => Err(ForwardError::Refused(refusal)),
        }
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Members in the room when this member joined, including itself.
    pub fn members_at_join(&self) -> u16 {
        self.members
    }
}
