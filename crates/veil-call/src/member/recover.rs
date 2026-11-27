//! Reaching the host again: after a transport loss (the same host) or a handover (the new
//! host). Both resume the existing membership with `Resume`; neither uses an invite.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use veil_group::{Applied, Group};
use veil_net::control::Control;
use veil_net::{FramedRecv, FramedSend, Node};

use super::handover::MemberCommand;
use super::{STEP, recv, send_control};
use crate::CallError;

/// Waits for the app to move the node to the new host's room. `None` after the recovery
/// window, or when the session's handle is gone.
pub(super) async fn wait_to_follow(
    commands: &mut mpsc::Receiver<MemberCommand>,
) -> Option<(Arc<Node>, Option<crate::presence::Presence>, bool)> {
    let deadline = tokio::time::Instant::now() + crate::host::CONTROL_RECOVERY;
    loop {
        match tokio::time::timeout_at(deadline, commands.recv()).await {
            Ok(Some(MemberCommand::Follow {
                node,
                presence,
                relay_only,
            })) => return Some((node, presence, relay_only)),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

/// Recovery authenticates the same device and pinned host. It never reuses a lobby token or
/// creates a new membership. Only contiguous host-authenticated commits advance the group.
pub(super) async fn recover_control(
    node: &Node,
    presence: Option<&crate::presence::Presence>,
    relay_only: bool,
    host: veil_security::PublicId,
    addrs: &[SocketAddr],
    room_id: [u8; 16],
    group: &mut Group,
) -> Result<(crate::dial::Dialled, FramedSend, FramedRecv), CallError> {
    let deadline = tokio::time::Instant::now() + crate::host::CONTROL_RECOVERY;
    let mut gap = Duration::from_millis(200);
    loop {
        let attempt = async {
            let dialled = crate::dial::dial(node, presence, host, addrs, relay_only).await?;
            let (mut out, mut input) = dialled.link.open_control().await?.split();
            send_control(
                &mut out,
                &Control::Resume {
                    room_id,
                    epoch: group.epoch(),
                },
            )
            .await?;
            let current = match recv(&mut input, STEP).await? {
                Control::Resumed { epoch }
                    if epoch >= group.epoch() && epoch - group.epoch() <= 64 =>
                {
                    epoch
                }
                Control::Refused(reason) => return Err(CallError::Refused(reason)),
                _ => return Err(CallError::Protocol("resume acknowledgment")),
            };
            while group.epoch() < current {
                let Control::Commit(commit) = recv(&mut input, STEP).await? else {
                    return Err(CallError::Protocol("recovery commit"));
                };
                match group.apply(&commit)? {
                    Applied::NewEpoch(_) => {}
                    Applied::Removed => {
                        return Err(CallError::Refused(
                            veil_net::control::RefuseReason::DeclinedByHost,
                        ));
                    }
                }
            }
            Ok((dialled, out, input))
        };
        match tokio::time::timeout_at(deadline, attempt).await {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err(e @ (CallError::Refused(_) | CallError::Group(_) | CallError::Protocol(_)))) => {
                return Err(e);
            }
            Err(_) => return Err(CallError::Timeout),
            Ok(Err(_)) => {}
        }
        tokio::time::timeout_at(deadline, tokio::time::sleep(gap))
            .await
            .map_err(|_| CallError::Timeout)?;
        gap = (gap * 2).min(Duration::from_secs(3));
    }
}
