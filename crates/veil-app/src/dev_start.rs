//! Where a UI development build opens, for `tools/dev-ui.ps1`.
//!
//! Purpose: reopening on the screen being worked on saves clicking back to it after every
//! rebuild. `VEIL_START` names the screen.
//!
//! Invariants:
//! - This module exists only with the `dev-ui` feature, so released builds ignore the variable.
//! - It opens only Home or the simulated demo, never a real call.

use crate::app::VeilApp;

#[derive(Debug, PartialEq, Eq)]
enum Start {
    Home,
    PreJoin,
    Call,
}

fn parse(value: Option<&str>) -> Start {
    match value {
        Some("prejoin") => Start::PreJoin,
        Some("call") => Start::Call,
        _ => Start::Home,
    }
}

/// `app`, moved to the screen `VEIL_START` asks for.
pub fn opened(mut app: VeilApp) -> VeilApp {
    let start = parse(std::env::var("VEIL_START").ok().as_deref());
    if start != Start::Home {
        app.demo = true;
        app.go_pre_join(true);
    }
    if start == Start::Call {
        app.go_call(true);
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
        assert_eq!(parse(Some("home")), Start::Home);
        assert_eq!(parse(Some("anything")), Start::Home);
        assert_eq!(parse(None), Start::Home);
    }
}
