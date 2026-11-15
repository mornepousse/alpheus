//! Do-not-disturb as a model (small features 1, decisions 6-8): on or off, with an end (Unix
//! seconds) when timed, and a flag saying a screen capture turned it on. Pure: the daemon's
//! `notify_app.rs` applies it (file, server, bell, timer).

use alpheus_commun::launcher_ipc::DndVerb;

/// What the state file holds: empty, `until <unix>` or `sharing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    On,
    Until(u64),
    Sharing,
}

pub fn saved_text(s: Saved) -> String {
    match s {
        Saved::On => String::new(),
        Saved::Until(t) => format!("until {t}\n"),
        Saved::Sharing => "sharing\n".into(),
    }
}

/// Reads a file's text back; anything unreadable as the other two forms (empty included) is `On`:
/// a file that exists was always "on".
pub fn parse_saved(text: &str) -> Saved {
    let text = text.trim();
    if text == "sharing" {
        return Saved::Sharing;
    }
    match text.strip_prefix("until ").map(str::trim) {
        Some(n) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => n.parse().map_or(Saved::On, Saved::Until),
        _ => Saved::On,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dnd {
    pub on: bool,
    pub until: Option<u64>,
    pub by_sharing: bool,
}

impl Dnd {
    /// The state read at startup: an end already passed and a sharing state read as off (the
    /// capture is evaluated again), a file with no end as untimed.
    pub fn loaded(saved: Option<Saved>, now: u64) -> Dnd {
        match saved {
            Some(Saved::On) => Dnd::user(true),
            Some(Saved::Until(t)) if t > now => Dnd { on: true, until: Some(t), by_sharing: false },
            Some(Saved::Until(_) | Saved::Sharing) | None => Dnd::default(),
        }
    }

    /// What the file holds for this state (None: no file).
    pub fn saved(&self) -> Option<Saved> {
        match (self.on, self.by_sharing, self.until) {
            (false, ..) => None,
            (true, true, _) => Some(Saved::Sharing),
            (true, false, Some(t)) => Some(Saved::Until(t)),
            (true, false, None) => Some(Saved::On),
        }
    }

    /// Every user path: untimed, never by sharing, so the user's choice wins.
    pub fn user(on: bool) -> Dnd {
        Dnd { on, until: None, by_sharing: false }
    }

    /// `alpheus-launch --dnd`: `on` takes the end given (None: untimed), `toggle` flips and is never timed.
    pub fn after_verb(self, verb: DndVerb, until: Option<u64>) -> Dnd {
        match verb {
            DndVerb::On => Dnd { on: true, until, by_sharing: false },
            DndVerb::Off => Dnd::user(false),
            DndVerb::Toggle => Dnd::user(!self.on),
        }
    }

    /// A capture started (true) or ended (false): on only from off, off only for its own turning on.
    pub fn after_sharing(self, sharing: bool) -> Dnd {
        match (sharing, self.on, self.by_sharing) {
            (true, false, _) => Dnd { on: true, until: None, by_sharing: true },
            (false, true, true) => Dnd::default(),
            _ => self,
        }
    }

    /// Off once `now` reaches the end (the wall clock: the monotonic timer drifts across a suspend);
    /// with a screen shared and `dnd_while_sharing` on (`sharing`), it hands over to sharing instead,
    /// so the capture's end turns it off as usual.
    pub fn expired(self, now: u64, sharing: bool) -> Dnd {
        match self.until {
            Some(t) if self.on && now >= t => {
                if sharing {
                    Dnd { on: true, until: None, by_sharing: true }
                } else {
                    Dnd::default()
                }
            }
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: Dnd = Dnd { on: false, until: None, by_sharing: false };
    const UNTIMED: Dnd = Dnd { on: true, until: None, by_sharing: false };
    const SHARING: Dnd = Dnd { on: true, until: None, by_sharing: true };
    fn timed(t: u64) -> Dnd {
        Dnd { on: true, until: Some(t), by_sharing: false }
    }

    #[test]
    fn saved_forms_round_trip() {
        assert_eq!(saved_text(Saved::On), "");
        assert_eq!(saved_text(Saved::Until(1800)), "until 1800\n");
        assert_eq!(saved_text(Saved::Sharing), "sharing\n");
        for s in [Saved::On, Saved::Until(1800), Saved::Sharing, Saved::Until(u64::MAX)] {
            assert_eq!(parse_saved(&saved_text(s)), s);
        }
        assert_eq!(parse_saved("garbage"), Saved::On);
        assert_eq!(parse_saved("until x"), Saved::On);
        assert_eq!(parse_saved("until "), Saved::On);
        assert_eq!(parse_saved("until 99999999999999999999999"), Saved::On, "past u64: unreadable, so on");
        assert_eq!(parse_saved("  sharing \n"), Saved::Sharing, "trimmed");
    }

    #[test]
    fn loaded_drops_a_past_end_and_a_sharing_state() {
        assert_eq!(Dnd::loaded(Some(Saved::Until(100)), 100), OFF);
        assert_eq!(Dnd::loaded(Some(Saved::Until(101)), 100), timed(101));
        assert_eq!(Dnd::loaded(Some(Saved::Sharing), 100), OFF);
        assert_eq!(Dnd::loaded(None, 100), OFF);
        assert_eq!(Dnd::loaded(Some(Saved::On), 100), UNTIMED);
    }

    #[test]
    fn verbs() {
        assert_eq!(OFF.after_verb(DndVerb::On, Some(5)), timed(5));
        assert_eq!(OFF.after_verb(DndVerb::On, None), UNTIMED);
        assert_eq!(timed(5).after_verb(DndVerb::On, None), UNTIMED);
        assert_eq!(UNTIMED.after_verb(DndVerb::On, Some(9)), timed(9));
        assert_eq!(timed(5).after_verb(DndVerb::Off, None), OFF);
        assert_eq!(timed(5).after_verb(DndVerb::Toggle, None), OFF);
        assert_eq!(OFF.after_verb(DndVerb::Toggle, None), UNTIMED);
        assert_eq!(SHARING.after_verb(DndVerb::On, None), UNTIMED, "the user's word clears by-sharing");
    }

    #[test]
    fn the_user_clears_the_end_and_sharing() {
        assert_eq!(Dnd::user(true), Dnd { on: true, until: None, by_sharing: false });
        assert_eq!(Dnd::user(false), OFF);
    }

    #[test]
    fn sharing_turns_on_only_from_off_and_turns_off_only_its_own() {
        assert_eq!(OFF.after_sharing(true), SHARING);
        assert_eq!(SHARING.after_sharing(false), OFF);
        assert_eq!(UNTIMED.after_sharing(true).after_sharing(false), UNTIMED);
        assert_eq!(SHARING.after_sharing(true), SHARING, "twice is once");
        assert_eq!(OFF.after_sharing(false), OFF);
        assert_eq!(SHARING.after_verb(DndVerb::Off, None).after_sharing(false), OFF);
        assert_eq!(Dnd::user(false).after_sharing(false), OFF);
        assert_eq!(Dnd::user(true).after_sharing(false), UNTIMED, "the user's on survives the capture's end");
        assert_eq!(timed(7).after_sharing(true).after_sharing(false), timed(7));
    }

    #[test]
    fn expiry() {
        assert_eq!(timed(100).expired(99, false), timed(100));
        assert_eq!(timed(100).expired(100, false), OFF);
        assert_eq!(timed(100).expired(u64::MAX, false), OFF);
        assert_eq!(UNTIMED.expired(u64::MAX, false), UNTIMED, "no end, no expiry");
        assert_eq!(SHARING.expired(u64::MAX, false), SHARING);
        assert_eq!(OFF.expired(5, false), OFF);
    }

    #[test]
    fn an_end_during_a_share_hands_over_to_sharing() {
        assert_eq!(timed(100).expired(100, true), SHARING);
        assert_eq!(timed(100).expired(99, true), timed(100), "not yet: unchanged");
        assert_eq!(UNTIMED.expired(u64::MAX, true), UNTIMED);
        assert_eq!(OFF.expired(5, true), OFF);
        assert_eq!(timed(100).expired(100, true).after_sharing(false), OFF, "the cast's end turns it off");
    }

    #[test]
    fn saved_of_each_state() {
        assert_eq!(OFF.saved(), None);
        assert_eq!(UNTIMED.saved(), Some(Saved::On));
        assert_eq!(timed(9).saved(), Some(Saved::Until(9)));
        assert_eq!(SHARING.saved(), Some(Saved::Sharing));
    }
}
