#![allow(clippy::expect_used)]

use super::*;
use crate::model::LinkPath;
use veil_security::Identity;

pub(super) fn example() -> Snapshot {
    let key = Identity::generate().public();
    let mut report = Report {
        started: true,
        ..Default::default()
    };
    report.endpoint(
        true,
        NodeAddr {
            key,
            addr: "192.0.2.7:4444".parse().expect("address"),
        },
    );
    report.set(Step::Membership, State::Passed);
    Snapshot {
        report,
        own_device: Some(key),
        peers: vec![Peer {
            id: 1,
            device: key,
            route: LinkPath::Direct,
            audio_recent: false,
            video_recent: false,
        }],
        ..Default::default()
    }
}

#[test]
fn export_metadata_requires_independent_explicit_choices() {
    let snapshot = example();
    let key = veil_net::node_addr::key_text(&snapshot.own_device.expect("device"));
    for include in [
        Include::default(),
        Include {
            addresses: true,
            device_keys: false,
        },
        Include {
            addresses: false,
            device_keys: true,
        },
        Include {
            addresses: true,
            device_keys: true,
        },
    ] {
        let text = snapshot.export(include);
        assert_eq!(text.contains("192.0.2.7:4444"), include.addresses);
        assert_eq!(text.contains(&key), include.device_keys);
        let value: serde_json::Value = serde_json::from_str(&text).expect("JSON export");
        assert_eq!(value["privacy"]["raw_logs_included"], false);
        assert_eq!(value["privacy"]["conversation_content_included"], false);
        assert_eq!(value["participants"][0]["route"], "Direct");
    }
}

#[test]
fn arbitrary_error_text_cannot_enter_an_export_even_with_all_details() {
    let mut snapshot = example();
    // Real error variants may contain driver messages, an invite, or peer-supplied text.
    for error in [
        NetError::Connect("INVITE_SECRET MLS_SECRET CHAT_TEXT PRIVATE_KEY".into()),
        NetError::Closed("INVITE_SECRET MLS_SECRET CHAT_TEXT PRIVATE_KEY".into()),
        NetError::Config("INVITE_SECRET MLS_SECRET CHAT_TEXT PRIVATE_KEY".into()),
    ] {
        snapshot
            .report
            .set(Step::HostConnection, State::Failed(Problem::net(&error)));
        let text = snapshot.export(Include {
            addresses: true,
            device_keys: true,
        });
        for secret in ["INVITE_SECRET", "MLS_SECRET", "CHAT_TEXT", "PRIVATE_KEY"] {
            assert!(!text.contains(secret));
        }
    }
}

#[test]
fn export_is_bounded_even_if_a_caller_supplies_excess_evidence() {
    let mut snapshot = example();
    snapshot.peers = vec![snapshot.peers[0].clone(); 10_000];
    snapshot.report.endpoints = vec![snapshot.report.endpoints[0]; 10_000];
    let text = snapshot.export(Include {
        addresses: true,
        device_keys: true,
    });
    let value: serde_json::Value = serde_json::from_str(&text).expect("JSON export");
    assert_eq!(
        value["participants"]
            .as_array()
            .expect("participants")
            .len(),
        MAX_PEERS
    );
    assert_eq!(
        value["endpoints"].as_array().expect("endpoints").len(),
        MAX_ENDPOINTS
    );
    assert!(text.len() < 64 * 1024);
}

#[test]
fn a_reachable_helper_does_not_prove_hosting_admission_or_media() {
    let mut snapshot = Snapshot::default();
    snapshot.report.helper(
        false,
        Some(HelperIssue::Refused(veil_rendezvous::Refusal::NotAllowed)),
        true,
    );
    snapshot
        .report
        .set(Step::Hosting, State::Failed(Problem::HostNotAllowed));
    assert_eq!(snapshot.report.steps[Step::Helper as usize], State::Passed);
    assert_eq!(
        snapshot.report.steps[Step::Admission as usize],
        State::Unchecked
    );
    assert_eq!(
        snapshot.report.steps[Step::Membership as usize],
        State::Unchecked
    );
    assert_eq!(snapshot.problem(), Some(Problem::HostNotAllowed));
    snapshot.peers = example().peers;
    assert_eq!(snapshot.media_label(), "No recent decoded media observed");
    snapshot.peers[0].audio_recent = true;
    assert_eq!(snapshot.media_label(), "Recent decoded media received");
}

#[test]
fn failure_evidence_does_not_guess_filtering_or_weaken_verification() {
    let mut report = Report::default();
    report.helper(false, Some(HelperIssue::Unavailable), true);
    assert_eq!(report.problem, Some(Problem::HelperUnavailable));
    report.helper(false, Some(HelperIssue::NoRoute), true);
    assert_eq!(report.problem, Some(Problem::NoRoute));
    report.helper(false, Some(HelperIssue::NoReply), true);
    assert_eq!(report.problem, Some(Problem::NoUdpReply));
    assert_eq!(Problem::net(&NetError::Identity), Problem::Identity);
    assert_eq!(Problem::net(&NetError::NoAddresses), Problem::NoRoute);
    assert_eq!(Problem::net(&NetError::Timeout), Problem::NoUdpReply);
    assert!(Problem::NoUdpReply.action().contains("does not prove"));
    assert!(
        Problem::Identity
            .action()
            .contains("Do not disable verification")
    );
    let mut snapshot = example();
    snapshot.camera = Capture::Off.with_permission(true, false);
    assert_eq!(snapshot.problem(), Some(Problem::PermissionDenied));
    assert_eq!(Capture::Off.with_permission(false, false), Capture::Off);
    snapshot.camera = Capture::Unavailable;
    assert_eq!(snapshot.problem(), Some(Problem::DeviceUnavailable));
    snapshot
        .report
        .set(Step::Admission, State::Failed(Problem::Declined));
    assert_eq!(snapshot.problem(), Some(Problem::Declined));
}

#[test]
fn a_full_call_is_not_reported_as_a_generic_retryable_failure() {
    use veil_net::control::RefuseReason;
    let problem = Problem::call(&veil_call::CallError::Refused(RefuseReason::RoomFull));
    assert_eq!(problem, Problem::CallFull);
    assert_eq!(problem.label(), "The call is full");
    assert!(problem.action().contains("until a seat is free"));
}
