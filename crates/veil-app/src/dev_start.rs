//! Where a UI development build opens, for `tools/dev-ui.ps1`.
//!
//! Purpose: reopening on the screen being worked on saves clicking back to it after every
//! rebuild. `VEIL_START` names the screen.
//!
//! Invariants:
//! - This module exists only with the `dev-ui` feature, so released builds ignore the variable.
//! - It opens Home, the simulated demo or synthetic report evidence, never a real call.

use crate::app::VeilApp;

#[derive(Debug, PartialEq, Eq)]
enum Start {
    Home,
    PreJoin,
    Call,
    Report,
}

fn parse(value: Option<&str>) -> Start {
    match value {
        Some("prejoin") => Start::PreJoin,
        Some("call") => Start::Call,
        Some("report") => Start::Report,
        _ => Start::Home,
    }
}

/// `app`, moved to the screen `VEIL_START` asks for.
pub fn opened(mut app: VeilApp) -> VeilApp {
    let start = parse(std::env::var("VEIL_START").ok().as_deref());
    if matches!(start, Start::PreJoin | Start::Call) {
        app.demo = true;
        // UI layout checks can run without opening real capture devices.
        if std::env::var("VEIL_DEV_NO_CAPTURE").as_deref() == Ok("1") {
            app.want_camera = false;
            app.want_mic = false;
        }
        app.go_pre_join(true);
    }
    if start == Start::Call {
        app.go_call(true);
    }
    if start == Start::Report {
        use crate::diagnostics::{Problem, Report, Snapshot, State, Step};
        let mut report = Report { started: true, ended: true, ..Default::default() };
        report.set(Step::Socket, State::Passed);
        report.set(Step::Invitation, State::Passed);
        report.set(Step::Helper, State::Failed(Problem::NoUdpReply));
        app.diagnostics.call = Some(Snapshot { report, ..Default::default() });
        app.diagnostics.previewing = true;
        app.diagnostics.open();
    }
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_names_leave_home() {
        assert_eq!(parse(Some("call")), Start::Call);
        assert_eq!(parse(Some("prejoin")), Start::PreJoin);
        assert_eq!(parse(Some("report")), Start::Report);
        assert_eq!(parse(Some("home")), Start::Home);
        assert_eq!(parse(Some("anything")), Start::Home);
        assert_eq!(parse(None), Start::Home);
    }
}
