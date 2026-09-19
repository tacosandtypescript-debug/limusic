//! How often the same viewer may ask, and how often anyone may.
//!
//! Two windows, because they stop two different things. The per-user one stops a single viewer
//! filling the queue from their keyboard; the global one stops a raid from doing it collectively,
//! which no per-user limit can see. A channel of five regulars wants the first one short or off; a
//! channel that just got raided wants the second one.
//!
//! ## Why `Instant` is a parameter
//!
//! Every method takes `now`. Not because the clock is interesting, but because a cooldown that reads
//! the clock itself can only be tested by sleeping — and a test suite that sleeps is a test suite
//! that gets deleted. Passing it in means the whole thing is arithmetic.
//!
//! ## Why it prunes
//!
//! A stream runs for hours and a busy channel sees thousands of distinct viewers. Keeping an entry
//! per viewer forever is a leak with a friendly name: it grows exactly in proportion to how well the
//! stream is going. Entries older than the window they belong to cannot affect any future answer, so
//! they are dropped.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

/// Why a request was refused, and how long until it would not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// This viewer asked too recently.
    User { remaining: Duration },
    /// Someone asked too recently, whoever it was.
    Global { remaining: Duration },
}

/// Serialised as whole seconds under a named kind, not as the `{ secs, nanos }` a bare `Duration`
/// would produce.
///
/// `Refusal` reaches the UI folded inside a request outcome, and what the UI shows is a countdown.
/// Handing it `secs` and `nanos` would push the rounding into the interface, where it would be
/// written once per place that displays it and get it wrong in at least one.
impl Serialize for Refusal {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Only the kind is destructured: the seconds come from `remaining_seconds`, so pulling the
        // `Duration` out here would be a second, unused copy of the same value.
        let kind = match self {
            Refusal::User { .. } => "user",
            Refusal::Global { .. } => "global",
        };
        let mut st = s.serialize_struct("Refusal", 2)?;
        st.serialize_field("kind", kind)?;
        st.serialize_field("remainingSeconds", &self.remaining_seconds())?;
        st.end()
    }
}

impl Refusal {
    /// Whole seconds, rounded up: "wait 0s" is not an instruction anyone can follow, and a cooldown
    /// that has 200ms left should read as one second rather than as nothing.
    pub fn remaining_seconds(self) -> u64 {
        let d = match self {
            Refusal::User { remaining } | Refusal::Global { remaining } => remaining,
        };
        d.as_secs() + u64::from(d.subsec_millis() > 0)
    }

    pub fn is_global(self) -> bool {
        matches!(self, Refusal::Global { .. })
    }
}

pub struct Cooldowns {
    per_user: Duration,
    global: Duration,
    last_by_user: HashMap<String, Instant>,
    last_any: Option<Instant>,
}

impl Cooldowns {
    pub fn new(per_user: Duration, global: Duration) -> Self {
        Self { per_user, global, last_by_user: HashMap::new(), last_any: None }
    }

    pub fn per_user(&self) -> Duration {
        self.per_user
    }

    pub fn global(&self) -> Duration {
        self.global
    }

    /// May `user` ask at `now`?
    ///
    /// The global window is checked first on purpose: it is the one the viewer can do nothing about,
    /// so telling them "someone else just asked" is more honest than telling them to wait when
    /// waiting will not help.
    pub fn check(&self, user: &str, now: Instant) -> Result<(), Refusal> {
        let waited = |since: Instant| now.saturating_duration_since(since);

        if !self.global.is_zero() {
            if let Some(last) = self.last_any {
                let elapsed = waited(last);
                if elapsed < self.global {
                    return Err(Refusal::Global { remaining: self.global - elapsed });
                }
            }
        }

        if !self.per_user.is_zero() {
            if let Some(last) = self.last_by_user.get(&user.to_ascii_lowercase()) {
                let elapsed = waited(*last);
                if elapsed < self.per_user {
                    return Err(Refusal::User { remaining: self.per_user - elapsed });
                }
            }
        }

        Ok(())
    }

    /// Record that `user` asked. Called only when the request was actually accepted — a refused
    /// request must not extend the window it was refused by, or a viewer hammering the command
    /// would lock themselves out permanently.
    pub fn record(&mut self, user: &str, now: Instant) {
        self.last_by_user.insert(user.to_ascii_lowercase(), now);
        self.last_any = Some(now);
    }

    /// Drop entries that can no longer affect an answer.
    ///
    /// An entry is dead once the per-user window has passed, because `check` compares against `now`
    /// and would accept it. The global stamp is not stored per user, so it never accumulates.
    pub fn prune(&mut self, now: Instant) {
        if self.per_user.is_zero() {
            self.last_by_user.clear();
            return;
        }
        let window = self.per_user;
        self.last_by_user.retain(|_, last| now.saturating_duration_since(*last) < window);
    }

    /// How many viewers are being tracked. `prune` is what keeps this number honest, so a test can
    /// watch it rather than trusting that a map does not grow.
    pub fn tracked(&self) -> usize {
        self.last_by_user.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn a_first_request_is_always_allowed() {
        let c = Cooldowns::new(Duration::from_secs(30), Duration::from_secs(5));
        let t = Instant::now();
        assert_eq!(c.check("someone", t), Ok(()));
        // And a different viewer is not blocked by the first having asked.
        assert_eq!(c.check("someone-else", t), Ok(()));
    }

    /// The per-user window, at the boundaries. `at the edge` is the interesting one: a cooldown
    /// that only expired strictly *after* its length would make every configured value a second
    /// longer than it says.
    #[test]
    fn the_per_user_window_expires_exactly() {
        let mut c = Cooldowns::new(Duration::from_secs(30), Duration::ZERO);
        let t = Instant::now();
        c.record("viewer", t);

        match c.check("viewer", at(t, 29)) {
            Err(Refusal::User { remaining }) => {
                assert_eq!(remaining, Duration::from_secs(1));
                assert_eq!(Refusal::User { remaining }.remaining_seconds(), 1);
            }
            other => panic!("expected a refusal with 1s left, got {other:?}"),
        }
        assert_eq!(c.check("viewer", at(t, 30)), Ok(()), "the edge is inclusive");
        // Another viewer is unaffected throughout.
        assert_eq!(c.check("other", at(t, 1)), Ok(()));
    }

    /// The global window applies to everyone, including someone who has never asked.
    #[test]
    fn the_global_window_stops_a_raid() {
        let mut c = Cooldowns::new(Duration::ZERO, Duration::from_secs(10));
        let t = Instant::now();
        c.record("first", t);

        for who in ["first", "second", "third"] {
            match c.check(who, at(t, 4)) {
                Err(r) => {
                    assert!(r.is_global(), "{who} should hit the global window");
                    assert_eq!(r.remaining_seconds(), 6);
                }
                Ok(()) => panic!("{who} slipped through the global window"),
            }
        }
        assert_eq!(c.check("fourth", at(t, 10)), Ok(()));
    }

    /// Global is reported before per-user, because waiting does not help with it. A viewer told to
    /// wait 6 seconds who then waits 6 seconds and is refused again learns nothing.
    #[test]
    fn global_is_reported_before_user() {
        let mut c = Cooldowns::new(Duration::from_secs(60), Duration::from_secs(10));
        let t = Instant::now();
        c.record("viewer", t);
        // At t+5 both windows are still closed; the answer must be the global one.
        assert!(c.check("viewer", at(t, 5)).unwrap_err().is_global());
    }

    /// A window of zero is off, not instantaneous. Somebody will want the queue open.
    #[test]
    fn a_zero_window_is_disabled() {
        let mut c = Cooldowns::new(Duration::ZERO, Duration::ZERO);
        let t = Instant::now();
        for i in 0..5 {
            c.record("spammer", at(t, 0));
            assert_eq!(c.check("spammer", at(t, i)), Ok(()));
        }
    }

    /// The map has to stay bounded. Without pruning it holds one entry per viewer for as long as the
    /// app runs, which for a long stream in a busy channel is the whole audience.
    #[test]
    fn pruning_bounds_the_map() {
        let mut c = Cooldowns::new(Duration::from_secs(30), Duration::ZERO);
        let t = Instant::now();

        for i in 0..500 {
            c.record(&format!("viewer-{i}"), at(t, i));
        }
        assert_eq!(c.tracked(), 500, "all still inside their window");

        // Two windows later, everything from the first stretch is dead.
        c.prune(at(t, 500));
        assert!(c.tracked() < 40, "expected the old entries gone, {} left", c.tracked());

        // And pruning must not have changed any answer, in either direction. The newest viewer is
        // still inside their window — pruning must not have quietly freed them — and one from a
        // while back is not, because they served their time and the entry saying so is gone.
        assert!(
            c.check("viewer-499", at(t, 500)).is_err(),
            "pruning freed a viewer who was still serving their cooldown"
        );
        assert_eq!(
            c.check("viewer-400", at(t, 500)),
            Ok(()),
            "a viewer whose window has passed must not be held by a stale entry"
        );
    }

    #[test]
    fn pruning_a_disabled_window_clears_it() {
        let mut c = Cooldowns::new(Duration::ZERO, Duration::ZERO);
        let t = Instant::now();
        c.record("someone", t);
        assert_eq!(c.tracked(), 1, "`record` still notes who asked");
        c.prune(t);
        assert_eq!(c.tracked(), 0, "but nothing is worth keeping");
    }

    /// A refused request must not extend the window. If `record` ran before the check, a viewer
    /// hammering the command would never be allowed — the queue would close for them permanently
    /// while telling them to wait a moment.
    #[test]
    fn a_refusal_does_not_extend_the_window() {
        let mut c = Cooldowns::new(Duration::from_secs(10), Duration::ZERO);
        let t = Instant::now();
        c.record("viewer", t);

        // Hammering for the whole window.
        for i in 0..10 {
            assert!(c.check("viewer", at(t, i)).is_err());
        }
        // At the edge it opens, because the failed attempts recorded nothing.
        assert_eq!(c.check("viewer", at(t, 10)), Ok(()));
    }

    /// Casing is not a distinction: Twitch logins are lower-case, but they arrive from a settings
    /// file and a chat event, and `Viewer` and `viewer` are one person.
    #[test]
    fn logins_are_matched_case_insensitively() {
        let mut c = Cooldowns::new(Duration::from_secs(30), Duration::ZERO);
        let t = Instant::now();
        c.record("Viewer", t);
        assert!(c.check("viewer", at(t, 1)).is_err());
        assert!(c.check("VIEWER", at(t, 1)).is_err());
        assert_eq!(c.tracked(), 1, "and only one entry was made");
    }

    /// Seconds are rounded up, so a sub-second remainder is never reported as zero.
    #[test]
    fn a_partial_second_reads_as_one() {
        let mut c = Cooldowns::new(Duration::from_secs(5), Duration::ZERO);
        let t = Instant::now();
        c.record("viewer", t);
        let r = c.check("viewer", t + Duration::from_millis(4800)).unwrap_err();
        assert_eq!(r.remaining_seconds(), 1, "200ms left is still a second to wait");
    }
}
