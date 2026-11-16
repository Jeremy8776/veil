//! Display names in a real call: who is called what, and when to send your own name.
//!
//! Purpose: each member sends its name as a sealed `AppMessage::Name` (see
//! `veil_call::app_msg`). [`Names`] keeps what arrived, by device, and names people who have
//! not sent one yet ("Host", or "Guest" and a key prefix). [`Announcer`] decides when to
//! send yours: for a few seconds after each key epoch, new routes or a new link, once a
//! second. A member that moves to the new epoch a moment later than you cannot open a name
//! sealed for it before then, and a link that comes up late missed the first send, so one
//! send is not enough.
//!
//! Invariants:
//! - A name is self-chosen and unverified. Who someone is rests on the safety number; the
//!   host badge comes from the invite's host key, never from a name.
//! - Names are kept by device key, so they survive epoch changes.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use veil_call::AppMessage;
use veil_security::PublicId;

use super::short;

/// How long your name is sent again, once a second, after something changes.
pub const ANNOUNCE_FOR: Duration = Duration::from_secs(5);

/// The names other members sent.
#[derive(Default)]
pub struct Names {
    known: HashMap<PublicId, String>,
}

impl Names {
    /// The name `device` sent, or how to call them until they do.
    pub fn of(&self, device: &PublicId, host: &PublicId) -> String {
        if let Some(name) = self.known.get(device) {
            return name.clone();
        }
        if device == host {
            String::from("Host")
        } else {
            format!("Guest {}", short(device))
        }
    }

    /// Stores a name. Returns `true` if it changed.
    pub fn set(&mut self, device: PublicId, name: String) -> bool {
        self.known.insert(device, name.clone()) != Some(name)
    }
}

/// What to show for each person: their name, plus their key prefix when someone else in the
/// call (you included) shows the same name, so no two people ever look alike (audit L4).
/// Names match ignoring case, spacing and a trailing key prefix, so "priya (ab12cd34)" cannot
/// pose as a tagged "Priya".
///
/// shortcut: letters that only look alike (Latin "a", Cyrillic "а") still count as different;
/// add a confusables table if that is ever abused. The safety number stays the real check.
pub fn labels<'a>(
    yours: Option<&str>,
    people: impl IntoIterator<Item = (&'a PublicId, &'a str)>,
) -> HashMap<PublicId, String> {
    let people: Vec<(&PublicId, &str)> = people.into_iter().collect();
    let mut count: HashMap<String, usize> = HashMap::new();
    for name in yours.into_iter().chain(people.iter().map(|(_, n)| *n)) {
        *count.entry(base(name)).or_default() += 1;
    }
    people
        .into_iter()
        .map(|(device, name)| {
            let name = name.trim();
            let label = if count.get(&base(name)).copied().unwrap_or(0) > 1 {
                format!("{name} ({})", short(device))
            } else {
                name.to_string()
            };
            (*device, label)
        })
        .collect()
}

/// A name as compared: lower case, single spaces, without a trailing "(8 hex digits)".
fn base(name: &str) -> String {
    let words: Vec<String> = name.split_whitespace().map(str::to_lowercase).collect();
    let mut joined = words.join(" ");
    if let Some(open) = joined.rfind(" (") {
        let tail = &joined[open + 2..];
        if tail.len() == 9
            && tail.ends_with(')')
            && tail[..8].bytes().all(|b| b.is_ascii_hexdigit())
        {
            joined.truncate(open);
        }
    }
    joined
}

/// When to send your own name.
pub struct Announcer {
    mine: Option<AppMessage>,
    until: Option<Instant>,
    linked: HashSet<PublicId>,
}

impl Announcer {
    /// `None` inside if your name is not a valid name (then nothing is sent).
    pub fn new(your_name: &str) -> Self {
        Self {
            mine: AppMessage::name(your_name).ok(),
            until: None,
            linked: HashSet::new(),
        }
    }

    /// Something changed (keys, routes): send your name again for a while.
    pub fn restart(&mut self, now: Instant) {
        self.until = Some(now + ANNOUNCE_FOR);
    }

    /// The peers linked now. A new one restarts the sends.
    pub fn links(&mut self, linked: HashSet<PublicId>, now: Instant) {
        if !linked.is_subset(&self.linked) {
            self.restart(now);
        }
        self.linked = linked;
    }

    /// Your name message, if it should go out now.
    pub fn due(&self, now: Instant) -> Option<&AppMessage> {
        self.mine
            .as_ref()
            .filter(|_| self.until.is_some_and(|until| now < until))
    }
}

#[cfg(test)]
mod tests {
    use veil_security::Identity;

    use super::*;

    #[test]
    fn people_are_named_by_what_they_sent_or_by_role() {
        let (host, guest) = (Identity::generate().public(), Identity::generate().public());
        let mut names = Names::default();
        assert_eq!(names.of(&host, &host), "Host");
        assert_eq!(names.of(&guest, &host), format!("Guest {}", short(&guest)));
        assert!(names.set(guest, "Priya".into()));
        assert!(
            !names.set(guest, "Priya".into()),
            "the same name again is no change"
        );
        assert_eq!(names.of(&guest, &host), "Priya");
    }

    #[test]
    fn a_shared_name_shows_the_key_prefix() {
        let (a, b, c) = (
            Identity::generate().public(),
            Identity::generate().public(),
            Identity::generate().public(),
        );
        let shown = labels(Some("Jo"), [(&a, "Priya"), (&b, " priya "), (&c, "Sam")]);
        assert_eq!(shown[&a], format!("Priya ({})", short(&a)));
        assert_eq!(shown[&b], format!("priya ({})", short(&b)));
        assert_eq!(shown[&c], "Sam", "a name nobody else has stays as it is");

        let shown = labels(Some("Jo"), [(&a, "JO")]);
        assert_eq!(shown[&a], format!("JO ({})", short(&a)), "nobody looks like you");

        let fake = format!("Priya ({})", short(&a));
        let shown = labels(None, [(&a, "Priya"), (&b, fake.as_str())]);
        assert!(shown[&a].ends_with(&format!("({})", short(&a))));
        assert!(
            shown[&b].ends_with(&format!("({})", short(&b))),
            "a copied prefix does not hide the real one"
        );
    }

    #[test]
    fn your_name_goes_out_for_a_while_after_each_change() {
        let now = Instant::now();
        let mut announcer = Announcer::new(" Jo ");
        assert!(
            announcer.due(now).is_none(),
            "nothing before the first keys"
        );
        announcer.restart(now);
        assert_eq!(announcer.due(now), Some(&AppMessage::Name("Jo".into())));
        assert!(announcer.due(now + ANNOUNCE_FOR).is_none());

        let later = now + ANNOUNCE_FOR * 2;
        let peer = Identity::generate().public();
        announcer.links(HashSet::from([peer]), later);
        assert!(announcer.due(later).is_some(), "a new link restarts");
        let quiet = later + ANNOUNCE_FOR;
        announcer.links(HashSet::from([peer]), quiet);
        assert!(announcer.due(quiet).is_none(), "the same links do not");
        announcer.links(HashSet::new(), quiet);
        assert!(announcer.due(quiet).is_none(), "nor does a link going away");
    }

    #[test]
    fn an_invalid_name_is_never_sent() {
        let now = Instant::now();
        let mut announcer = Announcer::new("   ");
        announcer.restart(now);
        assert!(announcer.due(now).is_none());
    }
}
