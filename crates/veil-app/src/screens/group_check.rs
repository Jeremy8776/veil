//! "Check everyone at once" in the Security panel: one group code per person, posted in a
//! group chat, instead of a code swap with each person.
//!
//! Purpose: in a call of five, pair codes mean ten swaps. Here each person copies one message,
//! posts it in a group chat, and pastes what everyone posted. Veil verifies each person whose
//! code matches (`veil_security::group_code`).
//!
//! Invariants:
//! - A matching code verifies its sender only, found by their tag. Nobody else.
//! - Shown only with two or more other people who have a safety number. With one, the pair
//!   code is the same work.
//! - The pasted text is kept per group number, so it clears when someone joins or leaves.
//! - In a demo call the codes are made up, like the pair codes.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, TextEdit, Ui};
use veil_security::group_code::{GroupCheck, check_group, group_code};
use veil_security::safety::{PASTE_LIMIT, tags};

use crate::icons::Icon;
use crate::model::Participant;
use crate::theme::{self, MUTED};
use crate::widgets::{Tone, pill};

/// One other person, as the group check sees them.
struct Member {
    id: u32,
    name: String,
    tag: String,
    verified: bool,
}

/// What the pasted text means: whom to verify and what to say.
#[derive(Debug, Default, PartialEq)]
struct Outcome {
    verify: Vec<u32>,
    lines: Vec<(String, Color32)>,
}

/// What the panel needs from the call.
pub struct Group<'a> {
    pub number: &'a str,
    pub epoch: u64,
    pub demo: bool,
    pub your_name: &'a str,
}

/// Your tag and everyone else's, from the pair safety numbers. `None` until two or more
/// people have one.
fn members<'a>(others: impl Iterator<Item = &'a Participant>) -> Option<(String, Vec<Member>)> {
    let mut yours = None;
    let mut list = Vec::new();
    for p in others {
        let Some((mine, theirs)) = tags(&p.safety, p.side) else {
            continue;
        };
        yours.get_or_insert(mine);
        list.push(Member {
            id: p.id,
            name: p.name.clone(),
            tag: theirs,
            verified: p.verified,
        });
    }
    (list.len() >= 2).then_some(())?;
    Some((yours?, list))
}

fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

fn names(list: &[&str]) -> String {
    match list {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Turns the checks into who to verify and the lines to show.
fn outcome(checks: &[GroupCheck], members: &[Member]) -> Outcome {
    let by_tag = |tag: &str| {
        let mut matches = members.iter().filter(|m| m.tag == tag);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    };
    let mut out = Outcome::default();
    let mut matched = Vec::new();
    let mut different = Vec::new();
    let (mut own, mut old) = (false, false);
    for check in checks {
        match check {
            GroupCheck::Match(tag) => {
                if let Some(m) = by_tag(tag) {
                    if !m.verified && !out.verify.contains(&m.id) {
                        out.verify.push(m.id);
                    }
                    matched.push(first_name(&m.name));
                } else {
                    out.lines.push((
                        String::from("This code does not name one person in this call. Check their pair codes instead."),
                        theme::WARN,
                    ));
                }
            }
            GroupCheck::Different(tag) => {
                different.push(by_tag(tag).map_or("Someone", |m| first_name(&m.name)));
            }
            GroupCheck::OwnCode => own = true,
            GroupCheck::Old => old = true,
        }
    }
    matched.dedup();
    if !matched.is_empty() {
        out.lines.push((format!("Matches {}.", names(&matched)), theme::SPEAKING));
    }
    if !different.is_empty() {
        out.lines.push((
            format!(
                "{} does not match. Check it came from them. If it did, someone may be \
                 listening: leave the call.",
                names(&different)
            ),
            theme::DANGER_HOVER,
        ));
    }
    if old {
        out.lines.push((
            String::from("A code is from before someone joined or left. Ask for a new one."),
            theme::WARN,
        ));
    }
    if own && matched.is_empty() && different.is_empty() && !old {
        out.lines.push((
            String::from("That is your own code. Paste the codes the others posted."),
            theme::WARN,
        ));
    }
    if checks.is_empty() {
        out.lines.push((String::from("No Veil group code found."), MUTED));
    }
    out
}

/// Draws the section and returns the people to mark verified.
pub fn show<'a>(
    ui: &mut Ui,
    group: &Group,
    others: impl Iterator<Item = &'a Participant>,
) -> Vec<u32> {
    let Some((your_tag, members)) = members(others) else {
        return Vec::new();
    };
    if group.number.is_empty() || members.iter().all(|m| m.verified) {
        return Vec::new();
    }
    let code = group_code(group.number, &your_tag, group.epoch);
    let store = ui.make_persistent_id(("group-paste", group.number));
    let mut pasted: String = ui.data(|d| d.get_temp(store)).unwrap_or_default();
    let mut verify = Vec::new();

    egui::Frame::new()
        .fill(theme::RAISED)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new("Check everyone at once").strong());
            ui.label(
                RichText::new("Post your code in a group chat you all trust, then paste theirs.")
                    .small()
                    .color(MUTED),
            )
            .on_hover_text(
                "Not this call's chat: someone between you could change it.\n\
                 Each matching code verifies the person who posted it.",
            );
            let width = ui.available_width();
            if pill(ui, Some(Icon::Copy), "Copy my group code", Tone::Quiet, width).clicked() {
                ui.ctx().copy_text(format!(
                    "Veil group code from {}. Paste everyone's codes into Veil to check.\n{code}",
                    first_name(group.your_name)
                ));
            }
            ui.add(
                TextEdit::singleline(&mut pasted)
                    .hint_text("Paste the codes from the group chat")
                    .char_limit(PASTE_LIMIT)
                    .desired_width(width),
            );
            if !pasted.trim().is_empty() {
                let checks = check_group(group.number, &your_tag, group.epoch, &pasted);
                let result = outcome(&checks, &members);
                for (text, colour) in &result.lines {
                    ui.label(RichText::new(text).small().color(*colour));
                }
                verify = result.verify;
            }
            if group.demo
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new("Fill demo codes").small().color(theme::WARN),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Pretends everyone posted their group code.")
                    .clicked()
            {
                pasted = members
                    .iter()
                    .map(|m| group_code(group.number, &m.tag, group.epoch))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        });
    ui.data_mut(|d| d.insert_temp(store, pasted));
    ui.add_space(10.0);
    verify
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u32, name: &str, tag: &str, verified: bool) -> Member {
        Member {
            id,
            name: name.into(),
            tag: tag.into(),
            verified,
        }
    }

    fn group() -> Vec<Member> {
        vec![
            member(1, "Jimmy Reload", "11111", false),
            member(2, "Martin", "22222", true),
            member(3, "Sam", "33333", false),
        ]
    }

    #[test]
    fn a_match_verifies_only_its_sender() {
        let out = outcome(&[GroupCheck::Match("11111".into())], &group());
        assert_eq!(out.verify, vec![1]);
        assert_eq!(out.lines[0].0, "Matches Jimmy.");
    }

    #[test]
    fn someone_already_verified_is_not_verified_again() {
        let checks = [GroupCheck::Match("22222".into()), GroupCheck::Match("33333".into())];
        let out = outcome(&checks, &group());
        assert_eq!(out.verify, vec![3]);
        assert_eq!(out.lines[0].0, "Matches Martin and Sam.");
    }

    #[test]
    fn a_mismatch_names_the_sender_and_warns() {
        let out = outcome(&[GroupCheck::Different("33333".into())], &group());
        assert!(out.verify.is_empty());
        assert!(out.lines[0].0.starts_with("Sam does not match."));
        assert_eq!(out.lines[0].1, theme::DANGER_HOVER);
    }

    #[test]
    fn own_and_old_codes_verify_nobody() {
        for check in [GroupCheck::OwnCode, GroupCheck::Old] {
            let out = outcome(&[check], &group());
            assert!(out.verify.is_empty());
            assert_eq!(out.lines.len(), 1);
        }
        let out = outcome(&[], &group());
        assert_eq!(out.lines[0].0, "No Veil group code found.");
    }

    #[test]
    fn an_unknown_tag_verifies_nobody() {
        let out = outcome(&[GroupCheck::Match("99999".into())], &group());
        assert!(out.verify.is_empty());
    }

    #[test]
    fn a_shared_short_tag_never_verifies_the_first_person_with_that_tag() {
        let members = [
            member(1, "Jo", "11111", false),
            member(2, "Sam", "11111", false),
            member(3, "Ana", "33333", false),
        ];
        let checks = [GroupCheck::Match("11111".into()), GroupCheck::Match("33333".into())];
        let out = outcome(&checks, &members);
        assert_eq!(out.verify, [3]);
        assert!(out.lines.iter().any(|(line, _)| line.contains("pair codes instead")));
    }

    #[test]
    fn names_read_naturally() {
        assert_eq!(names(&["Jo"]), "Jo");
        assert_eq!(names(&["Jo", "Sam"]), "Jo and Sam");
        assert_eq!(names(&["Jo", "Sam", "Al"]), "Jo, Sam and Al");
    }
}
