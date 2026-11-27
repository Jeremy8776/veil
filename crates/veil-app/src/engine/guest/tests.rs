//! Tests for the guest loop's link-up buffering (`guest.rs`).
#![allow(clippy::unwrap_used)]

use std::future::pending;
use std::time::Duration;

use tokio::sync::mpsc::{Sender, UnboundedSender, channel, unbounded_channel};

use super::*;

type Channels = (
    Sender<MemberEvent>,
    Receiver<MemberEvent>,
    UnboundedSender<Command>,
    UnboundedReceiver<Command>,
);

fn channels() -> Channels {
    let (event_tx, events) = channel(veil_call::EVENT_BACKLOG + 1);
    let (command_tx, commands) = unbounded_channel();
    (event_tx, events, command_tx, commands)
}

#[tokio::test]
async fn the_host_ending_the_call_interrupts_a_link_up() {
    let (event_tx, mut events, _command_tx, mut commands) = channels();
    event_tx.try_send(MemberEvent::Closed).unwrap();
    let result = link_up(
        pending(),
        &mut events,
        &mut commands,
        &mut Held::default(),
        None,
    )
    .await;
    assert_eq!(result, Err("The host ended the call.".to_string()));
}

#[tokio::test]
async fn being_removed_interrupts_a_link_up() {
    let (event_tx, mut events, _command_tx, mut commands) = channels();
    event_tx.try_send(MemberEvent::Removed).unwrap();
    let result = link_up(
        pending(),
        &mut events,
        &mut commands,
        &mut Held::default(),
        None,
    )
    .await;
    assert_eq!(
        result,
        Err("The host removed you from the call.".to_string())
    );
}

#[tokio::test]
async fn leaving_interrupts_a_link_up() {
    let (_event_tx, mut events, command_tx, mut commands) = channels();
    command_tx
        .send(Command::Leave)
        .map_err(|_| "closed")
        .unwrap();
    let result = link_up(
        pending(),
        &mut events,
        &mut commands,
        &mut Held::default(),
        None,
    )
    .await;
    assert_eq!(result, Ok(Flow::Leave));
}

#[tokio::test]
async fn other_arrivals_wait_in_order_until_the_link_up_ends() {
    let (event_tx, mut events, command_tx, mut commands) = channels();
    event_tx
        .try_send(MemberEvent::PlanRefused(CallError::Closed))
        .unwrap();
    event_tx
        .try_send(MemberEvent::PlanRefused(CallError::Timeout))
        .unwrap();
    for text in ["first", "second"] {
        let chat = Command::Act(Action::Chat(1, text.into()));
        command_tx.send(chat).map_err(|_| "closed").unwrap();
    }
    let mut held = Held::default();
    let linking = tokio::time::sleep(Duration::from_millis(50));
    let result = link_up(linking, &mut events, &mut commands, &mut held, None).await;
    assert_eq!(result, Ok(Flow::Stay));
    assert!(matches!(
        held.events.make_contiguous(),
        [
            MemberEvent::PlanRefused(CallError::Closed),
            MemberEvent::PlanRefused(CallError::Timeout)
        ]
    ));
    assert_eq!(
        held.chat,
        [
            Action::Chat(1, "first".into()),
            Action::Chat(1, "second".into())
        ]
    );
}

#[tokio::test]
async fn a_continuous_event_producer_cannot_grow_link_up_storage() {
    let (event_tx, mut events, _command_tx, mut commands) = channels();
    let producer = tokio::spawn(async move {
        for _ in 0..10_000 {
            if event_tx
                .send(MemberEvent::PlanRefused(CallError::Timeout))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let mut held = Held::default();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        link_up(pending(), &mut events, &mut commands, &mut held, None),
    )
    .await
    .unwrap();
    assert_eq!(result, Err(LINK_UP_OVERLOAD.into()));
    assert_eq!(held.events.len(), HELD_EVENTS);
    drop(events);
    producer.await.unwrap();
}

#[tokio::test]
async fn excess_chat_cannot_grow_link_up_storage() {
    let (_event_tx, mut events, command_tx, mut commands) = channels();
    for id in 0..=HELD_CHAT {
        command_tx
            .send(Command::Act(Action::Chat(id as u64, "hello".into())))
            .map_err(|_| "closed")
            .unwrap();
    }
    let mut held = Held::default();
    let result = link_up(pending(), &mut events, &mut commands, &mut held, None).await;
    assert_eq!(result, Err(LINK_UP_OVERLOAD.into()));
    assert_eq!(held.chat.len(), HELD_CHAT);
}

#[test]
fn held_chat_checks_payload_limits_and_terminal_events_always_interrupt() {
    let mut held = Held::default();
    let limit = veil_call::app_msg::MAX_CHAT_CHARS;
    assert!(held.said(Action::Chat(1, "🦀".repeat(limit))).is_ok());
    assert!(held.said(Action::Chat(2, "🦀".repeat(limit + 1))).is_err());
    assert!(
        held.said(Action::React(3, "x".repeat(1_000), true))
            .is_err()
    );
    assert_eq!(held.chat.len(), 1);
    for _ in 0..HELD_EVENTS {
        held.event(MemberEvent::Reconnecting).unwrap();
    }
    assert_eq!(
        held.event(MemberEvent::Removed),
        Err("The host removed you from the call.".into())
    );
    assert!(
        held.event(MemberEvent::Overloaded)
            .unwrap_err()
            .contains("too busy")
    );
    assert_eq!(held.events.len(), HELD_EVENTS);
}

#[test]
fn join_errors_read_plainly() {
    assert_eq!(join_error(&CallError::Timeout), "The host did not answer.");
    assert!(join_error(&CallError::Closed).starts_with("Could not reach the call"));
    let unreachable = CallError::Net(veil_net::NetError::Timeout);
    let text = join_error(&unreachable);
    assert!(
        text.contains("press Allow guests"),
        "one space at the line join: {text}"
    );
}

#[test]
fn a_used_invite_is_not_blamed_on_the_host() {
    let used = join_error(&CallError::Refused(RefuseReason::TokenUsed));
    assert!(used.contains("Ask the host for a new one"), "{used}");
    let declined = join_error(&CallError::Refused(RefuseReason::DeclinedByHost));
    assert_eq!(declined, "The host did not let you in.");
}
