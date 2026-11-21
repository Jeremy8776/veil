//! `veil-probe join LINK`: joins the call, follows the host's plan and probes.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{MissedTickBehavior, interval};
use veil_call::{CallNode, MemberEvent, join};
use veil_security::Identity;
use veil_security::invite::Invite;
use veil_security::safety::safety_number;

use crate::args::JoinArgs;
use crate::session::{Session, short};

pub async fn run(args: JoinArgs) -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let invite = Invite::from_link(&args.link, now).map_err(|e| format!("invite: {e}"))?;
    let identity = Identity::generate();
    let node = crate::bind(&identity, args.bind)?;
    let mut call = CallNode::for_invite(&identity, node, &invite).await;
    call.set_relay_only(args.relay_only);
    println!(
        "joining as {} from {}; safety number with the host {}",
        short(&identity.public()),
        call.local_addr().map_err(|e| e.to_string())?,
        safety_number(&identity.public(), &invite.host),
    );
    match (&invite.relay, call.has_rendezvous()) {
        (Some(_), true) => println!("registered with the invite's rendezvous"),
        (Some(r), false) => println!("WARNING: rendezvous {r} did not answer; direct links only"),
        (None, _) => {}
    }
    let mut member = join(&call, &invite)
        .await
        .map_err(|e| format!("join: {e}"))?;
    println!("admitted; link to the host is {:?}", member.path());
    let mut session = Session::new(call.media().map_err(|e| e.to_string())?, identity.public());

    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut report = interval(Duration::from_secs(10));
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            event = member.events.recv() => match event {
                Some(MemberEvent::Keys(keys)) => session.on_keys(keys),
                Some(MemberEvent::Routes(routes)) => session.on_routes(routes).await,
                Some(MemberEvent::PlanRefused(e)) => println!("refused the host's plan: {e}"),
                Some(MemberEvent::Reconnecting) => println!("reconnecting to the host"),
                Some(MemberEvent::Reconnected) => println!("host connection restored"),
                Some(MemberEvent::ConnectionLost) => return Err("the connection to the host could not be restored; use a new invite".into()),
                Some(MemberEvent::Removed) => return Err("the host removed this member".into()),
                Some(MemberEvent::Failed(e)) => return Err(format!("group failed: {e}")),
                Some(MemberEvent::Closed) | None => return Err("the call ended".into()),
            },
            Some(incoming) = session.media.recv() => session.on_incoming(incoming).await,
            _ = tick.tick() => session.ping().await,
            _ = report.tick() => session.report().iter().for_each(|line| println!("{line}")),
            _ = &mut stop => return Ok(()),
        }
    }
}
