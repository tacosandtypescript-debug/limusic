//! Who is allowed to ask for a song.
//!
//! Twitch hands every chat message a list of badges, and that list is the whole of what the app
//! knows about a viewer's standing. There is no relationship to query and no permission to check:
//! the badge is either there or it is not, so this is a mapping and nothing more. Keeping it in its
//! own file means the rule can be read in one screen and tested without a socket.
//!
//! ## The ladder
//!
//! Broadcaster, then moderator, then VIP, then subscriber (and founder, which is a subscriber with
//! tenure), then everyone else. Higher roles satisfy lower requirements — a moderator may do
//! anything a subscriber may — which is what makes a single `minimum role` setting enough instead of
//! a matrix of checkboxes nobody wants to fill in.
//!
//! ## What this deliberately does not do
//!
//! It does not look at the message, the channel or the connection. It takes badge set ids and a
//! required role, and answers. Everything that decides *which* badges a viewer has already happened
//! in `events.rs`, and everything that decides what to do about it happens in the caller.

use serde::Serialize;

/// The rungs of the ladder, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Everyone,
    Subscriber,
    Vip,
    Moderator,
    Broadcaster,
}

impl Role {
    /// Parse a stored setting. Case-insensitive and trimmed, because this value round-trips through
    /// a settings file a person may have edited by hand.
    pub fn parse(raw: &str) -> Option<Role> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "everyone" | "all" | "" => Some(Role::Everyone),
            "subscriber" | "sub" | "subs" => Some(Role::Subscriber),
            "vip" | "vips" => Some(Role::Vip),
            "moderator" | "mod" | "mods" => Some(Role::Moderator),
            "broadcaster" | "streamer" | "owner" => Some(Role::Broadcaster),
            _ => None,
        }
    }

    /// The stored form. `Role::parse(self.label())` is always `self`.
    pub fn label(self) -> &'static str {
        match self {
            Role::Everyone => "everyone",
            Role::Subscriber => "subscriber",
            Role::Vip => "vip",
            Role::Moderator => "moderator",
            Role::Broadcaster => "broadcaster",
        }
    }

    /// What to call it in a chat reply, where "everyone" reads oddly as a requirement.
    pub fn spoken(self) -> &'static str {
        match self {
            Role::Everyone => "everyone",
            Role::Subscriber => "subscribers",
            Role::Vip => "VIPs",
            Role::Moderator => "moderators",
            Role::Broadcaster => "the broadcaster",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Role::Everyone => 0,
            Role::Subscriber => 1,
            Role::Vip => 2,
            Role::Moderator => 3,
            Role::Broadcaster => 4,
        }
    }
}

/// The highest rung a set of badge set ids reaches.
///
/// `founder` counts as a subscriber: it is a subscriber badge with tenure on it, and Twitch sends it
/// *instead of* `subscriber`, never alongside. A viewer who is both a moderator and a subscriber
/// gets moderator, because the highest rung wins and nothing here depends on the order.
pub fn rank_of(badge_sets: &[&str]) -> u8 {
    let mut rank = Role::Everyone.rank();
    for set in badge_sets {
        let r = match *set {
            "broadcaster" => Role::Broadcaster,
            "moderator" => Role::Moderator,
            "vip" => Role::Vip,
            "subscriber" | "founder" => Role::Subscriber,
            _ => continue,
        };
        rank = rank.max(r.rank());
    }
    rank
}

/// May someone with these badges do a thing that requires `required`?
pub fn allows(required: Role, badge_sets: &[&str]) -> bool {
    rank_of(badge_sets) >= required.rank()
}

/// The role a set of badges actually is, for saying so in a reply.
pub fn role_of(badge_sets: &[&str]) -> Role {
    match rank_of(badge_sets) {
        4 => Role::Broadcaster,
        3 => Role::Moderator,
        2 => Role::Vip,
        1 => Role::Subscriber,
        _ => Role::Everyone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder is a ladder: every role satisfies every requirement at or below it, and none
    /// above. Stated as a loop because the interesting failures are the adjacent pairs — VIP
    /// against subscriber, moderator against VIP — and those are exactly what a hand-written list
    /// of examples tends to leave out.
    #[test]
    fn a_role_satisfies_everything_at_or_below_it() {
        let ladder =
            [Role::Everyone, Role::Subscriber, Role::Vip, Role::Moderator, Role::Broadcaster];
        for (i, holder) in ladder.iter().enumerate() {
            for (j, required) in ladder.iter().enumerate() {
                let badges: &[&str] = match holder {
                    Role::Everyone => &[],
                    Role::Subscriber => &["subscriber"],
                    Role::Vip => &["vip"],
                    Role::Moderator => &["moderator"],
                    Role::Broadcaster => &["broadcaster"],
                };
                assert_eq!(allows(*required, badges), i >= j, "{holder:?} against {required:?}");
            }
        }
    }

    /// A founder is a subscriber. Twitch sends `founder` instead of `subscriber`, never with it, so
    /// a check that only looked for `subscriber` would lock out exactly the longest-standing
    /// viewers in the channel.
    #[test]
    fn a_founder_counts_as_a_subscriber() {
        assert!(allows(Role::Subscriber, &["founder"]));
        assert_eq!(role_of(&["founder"]), Role::Subscriber);
        assert!(!allows(Role::Vip, &["founder"]), "but no higher");
    }

    /// A viewer can carry several badges, and the highest one is what counts. A moderator who also
    /// subscribes is a moderator; the order the badges arrive in must not matter.
    #[test]
    fn the_highest_badge_wins_in_any_order() {
        for badges in [
            vec!["subscriber", "moderator"],
            vec!["moderator", "subscriber"],
            vec!["subscriber", "vip", "moderator"],
        ] {
            assert_eq!(role_of(&badges), Role::Moderator, "{badges:?}");
            assert!(allows(Role::Moderator, &badges));
        }
    }

    /// Badges this app has no opinion about must not grant anything. Twitch sends plenty — `premium`,
    /// `turbo`, `glhf-pledge`, channel-specific ones — and a lookup that fell through to a default
    /// would hand a stranger whatever the default was.
    #[test]
    fn unknown_badges_grant_nothing() {
        assert_eq!(role_of(&["premium", "turbo", "glhf-pledge", "no_audio"]), Role::Everyone);
        assert!(!allows(Role::Subscriber, &["premium"]));
        // And the same for empty, which is what a viewer with no badges has.
        assert_eq!(role_of(&[]), Role::Everyone);
        assert!(allows(Role::Everyone, &[]));
    }

    /// A stored setting round-trips. It is written to a file, possibly edited by hand, and read back
    /// on the next launch — so `parse(label())` has to be the identity, and the tolerant spellings
    /// have to land on the same rung rather than near it.
    #[test]
    fn the_labels_round_trip() {
        for role in
            [Role::Everyone, Role::Subscriber, Role::Vip, Role::Moderator, Role::Broadcaster]
        {
            assert_eq!(Role::parse(role.label()), Some(role), "{}", role.label());
        }
        for (raw, want) in [
            ("  MOD  ", Role::Moderator),
            ("Sub", Role::Subscriber),
            ("subs", Role::Subscriber),
            ("", Role::Everyone),
            ("streamer", Role::Broadcaster),
            ("owner", Role::Broadcaster),
        ] {
            assert_eq!(Role::parse(raw), Some(want), "{raw:?}");
        }
        // Nonsense must be rejected rather than quietly becoming Everyone: a typo in a settings file
        // should not silently open the queue to the channel.
        assert_eq!(Role::parse("moderater"), None);
        assert_eq!(Role::parse("admin"), None);
        assert_eq!(Role::parse("123"), None);
    }

    /// The default has to be the safe end. Anyone reading a settings file that predates this option
    /// gets `everyone`, which is the permissive one — so the *caller* is where a stricter default
    /// belongs, and this test exists to make that explicit rather than assumed.
    #[test]
    fn an_absent_setting_is_everyone() {
        assert_eq!(Role::parse(""), Some(Role::Everyone));
        assert_eq!(Role::Everyone.label(), "everyone");
    }
}
