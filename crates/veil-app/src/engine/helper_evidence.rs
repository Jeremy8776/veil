//! Keeps the report's helper row in step with the presence's live state.
//!
//! The presence retries a helper in the background, so evidence taken once at start goes
//! stale. Callers poll [`HelperWatch::refresh`] on a tick; it rewrites the row only when the
//! connection or the failure kind changes, and never replaces a typed issue with a generic one.

use veil_call::CallNode;
use veil_call::node::HelperIssue;

use super::view::Shared;
use crate::diagnostics::{Report, State, Step};

/// What the report last showed for the helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HelperWatch {
    connected: bool,
    issue: Option<HelperIssue>,
    needed: bool,
    /// Hosts also show permission to host, which follows registration.
    hosting: bool,
}

impl HelperWatch {
    /// The evidence at start. Write it with [`Self::write`].
    pub fn start(call: &CallNode, needed: bool, hosting: bool) -> Self {
        Self {
            connected: call.has_rendezvous(),
            issue: call.helper_issue(),
            needed,
            hosting,
        }
    }

    /// Rewrites the row when the live state differs. Returns the new connection state.
    pub fn refresh(&mut self, shared: &Shared, call: &CallNode) -> bool {
        if self.observe(call.has_rendezvous(), call.helper_issue()) {
            let watch = *self;
            shared.diagnose(|d| watch.write(d));
        }
        self.connected
    }

    /// Whether `connected` and `issue` change what the report shows.
    fn observe(&mut self, connected: bool, issue: Option<HelperIssue>) -> bool {
        // A lost connection with no recorded kind yet keeps the last typed one.
        let issue = if connected {
            None
        } else {
            issue.or(self.issue)
        };
        let changed = connected != self.connected || issue != self.issue;
        self.connected = connected;
        self.issue = issue;
        changed
    }

    pub fn write(self, report: &mut Report) {
        report.helper(self.connected, self.issue, self.needed);
        if self.hosting {
            let refused = matches!(self.issue, Some(HelperIssue::Refused(_)));
            // A refusal is written by the host loop with its own typed problem.
            if !refused {
                let state = if self.connected {
                    State::Passed
                } else {
                    State::Unchecked
                };
                report.set(Step::Hosting, state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Problem;

    fn watch() -> HelperWatch {
        HelperWatch {
            connected: false,
            issue: Some(HelperIssue::Identity),
            needed: true,
            hosting: true,
        }
    }

    #[test]
    fn a_later_registration_replaces_stale_failure_evidence() {
        let mut watch = watch();
        assert!(watch.observe(true, None));
        let mut report = Report::default();
        watch.write(&mut report);
        assert_eq!(report.steps[Step::Helper as usize], State::Passed);
        assert_eq!(report.steps[Step::Hosting as usize], State::Passed);
        assert!(
            !watch.observe(true, None),
            "no rewrite while nothing changes"
        );
    }

    #[test]
    fn losing_the_helper_keeps_the_typed_issue_instead_of_a_generic_one() {
        let mut watch = watch();
        assert!(
            !watch.observe(false, None),
            "a missing kind does not erase the typed one"
        );
        let mut report = Report::default();
        watch.write(&mut report);
        assert_eq!(report.problem, Some(Problem::Identity));
        assert_eq!(report.steps[Step::Hosting as usize], State::Unchecked);
        assert!(
            watch.observe(false, Some(HelperIssue::NoReply)),
            "a newer kind is shown"
        );
        watch.write(&mut report);
        assert_eq!(report.problem, Some(Problem::NoUdpReply));
    }

    #[test]
    fn a_guest_row_does_not_touch_hosting() {
        let mut watch = HelperWatch {
            hosting: false,
            ..watch()
        };
        let mut report = Report::default();
        watch.observe(true, None);
        watch.write(&mut report);
        assert_eq!(report.steps[Step::Hosting as usize], State::Unchecked);
    }
}
