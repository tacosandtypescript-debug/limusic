//! Channel Points: the redemption event, and deciding whether it is one of ours.
//!
//! ## The trap this file exists to avoid
//!
//! A redemption does not arrive once. Twitch sends `…redemption.add` when a viewer spends the points
//! — with `status: "unfulfilled"` — and then `…redemption.update` every time the redemption changes
//! state, including when the streamer fulfils or cancels it. Both carry the same `id` and the same
//! `user_input`, and both have a subscription type that ends in a word containing "redemption".
//!
//! A handler that acts on the event without reading `status` therefore queues the song a second time
//! the moment the streamer marks it done. And a handler that subscribes to the wrong type queues it
//! on cancellation. So: the type is pinned as a constant, and the status is checked before the
//! reward is even looked at.
//!
//! ## Why the reward is matched by id
//!
//! The title is what a person recognises and the id is what Twitch guarantees. A streamer renaming
//! "Song Request" to "🎵 Song Request" — which is a normal thing to do on a stream — would silently
//! stop every redemption from matching if the title were the key.

use serde::Serialize;
use serde_json::Value;

/// The only subscription type that means "a viewer just spent points".
pub const REDEMPTION_ADD: &str = "channel.channel_points_custom_reward_redemption.add";

/// The status of a redemption that has not been dealt with yet.
pub const UNFULFILLED: &str = "unfulfilled";

/// A `channel.channel_points_custom_reward_redemption.add` event, reduced to what this app uses.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Redemption {
    /// Twitch's id for the redemption. The dedupe key, and what fulfil/cancel takes.
    pub id: String,
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
    pub reward_id: String,
    pub reward_title: String,
    /// What the viewer typed into the reward's text box. Empty when the reward does not ask for one.
    pub user_input: String,
    pub status: String,
    /// Points spent. Carried for the reply, not for any decision.
    pub cost: i64,
}

/// The raw shape, separated for the same reason `events.rs` separates its own: Twitch nests the
/// reward object, and flattening that in serde would need a custom deserializer for one field.
#[derive(Debug, serde::Deserialize)]
struct RawRedemption {
    id: String,
    user_id: String,
    user_login: String,
    user_name: String,
    #[serde(default)]
    user_input: String,
    status: String,
    reward: RawReward,
}

#[derive(Debug, serde::Deserialize)]
struct RawReward {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    cost: i64,
}

impl Redemption {
    /// Parse the `event` object of a redemption notification.
    pub fn from_event(event: &Value) -> Result<Self, String> {
        let raw: RawRedemption = serde_json::from_value(event.clone())
            .map_err(|e| format!("unexpected redemption payload: {e}"))?;
        Ok(Self {
            id: raw.id,
            user_id: raw.user_id,
            user_login: raw.user_login,
            user_name: raw.user_name,
            reward_id: raw.reward.id,
            reward_title: raw.reward.title,
            user_input: raw.user_input,
            status: raw.status,
            cost: raw.reward.cost,
        })
    }

    /// Has this redemption already been dealt with?
    ///
    /// The mirror of `is_actionable`, and separate from it because the two answer different
    /// questions: this one is about Twitch's state machine, the other about configuration.
    pub fn is_unfulfilled(&self) -> bool {
        self.status.eq_ignore_ascii_case(UNFULFILLED)
    }

    /// Should this redemption queue a song?
    ///
    /// Three things have to hold, and each of them has cost something in the past:
    ///
    /// * the status is `unfulfilled` — or the fulfilment that follows queues a second song;
    /// * a reward is configured — because `configured` being empty must match **nothing**, not
    ///   everything. This is the single most dangerous default in the file: an unconfigured channel
    ///   would otherwise treat every redemption of every reward as a song request;
    /// * the ids are equal, case-insensitively, because the value round-trips through a settings
    ///   file a person may have pasted into.
    pub fn is_actionable(&self, configured_reward: &str) -> bool {
        self.is_unfulfilled() && matches_reward(&self.reward_id, configured_reward)
    }

    /// The search terms, cleaned, or `None` when the viewer left the box empty.
    pub fn query(&self) -> Option<String> {
        super::chat::clean_query(&self.user_input)
    }

    /// How to label the queue entry.
    pub fn source(&self) -> String {
        format!("twitch:{}", self.user_login)
    }
}

/// Does a redemption's reward match the configured one?
///
/// Empty matches nothing. See `is_actionable`.
pub fn matches_reward(reward_id: &str, configured: &str) -> bool {
    let configured = configured.trim();
    !configured.is_empty() && reward_id.eq_ignore_ascii_case(configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REWARD: &str = "d8b8f2a1-0000-4000-8000-000000000001";

    fn event(status: &str, reward_id: &str, input: &str) -> Value {
        json!({
            "id": "redemption-1",
            "broadcaster_user_id": "111",
            "broadcaster_user_login": "streamer",
            "user_id": "222",
            "user_login": "viewer",
            "user_name": "Viewer",
            "user_input": input,
            "status": status,
            "reward": { "id": reward_id, "title": "Song Request", "cost": 250, "prompt": "Which song?" },
            "redeemed_at": "2024-01-01T00:00:00Z"
        })
    }

    #[test]
    fn a_redemption_is_parsed() {
        let r = Redemption::from_event(&event("unfulfilled", REWARD, "darude sandstorm")).unwrap();
        assert_eq!(r.id, "redemption-1");
        assert_eq!(r.user_login, "viewer");
        assert_eq!(r.reward_id, REWARD);
        assert_eq!(r.reward_title, "Song Request");
        assert_eq!(r.user_input, "darude sandstorm");
        assert_eq!(r.cost, 250);
        assert_eq!(r.source(), "twitch:viewer");
        assert_eq!(r.query().as_deref(), Some("darude sandstorm"));
    }

    /// The two notifications Twitch sends for one redemption differ only in `status`, so the status
    /// is the whole of what separates "queue this" from "this is already queued".
    #[test]
    fn the_follow_up_event_is_not_a_second_request() {
        let add = Redemption::from_event(&event("unfulfilled", REWARD, "a song")).unwrap();
        let done = Redemption::from_event(&event("fulfilled", REWARD, "a song")).unwrap();
        let cancelled = Redemption::from_event(&event("canceled", REWARD, "a song")).unwrap();

        assert!(add.is_actionable(REWARD));
        assert!(!done.is_actionable(REWARD), "fulfilling it would queue the song again");
        assert!(!cancelled.is_actionable(REWARD), "cancelling it would queue it at all");
        // Same redemption id, which is exactly why the id cannot be the only guard.
        assert_eq!(add.id, done.id);
    }

    /// An unconfigured channel must match nothing. If empty matched everything, connecting the app
    /// to a channel that uses Channel Points for anything at all would start queueing songs from
    /// every reward on the list.
    #[test]
    fn an_unconfigured_reward_matches_nothing() {
        let r = Redemption::from_event(&event("unfulfilled", REWARD, "a song")).unwrap();
        for empty in ["", "   ", "\t"] {
            assert!(!r.is_actionable(empty), "{empty:?} must not match");
        }
        assert!(!matches_reward(REWARD, ""));
        assert!(!matches_reward(REWARD, "   "));
    }

    /// A redemption of some other reward is not a song request, however the app is configured.
    #[test]
    fn another_reward_is_ignored() {
        let other =
            Redemption::from_event(&event("unfulfilled", "some-other-reward", "hi")).unwrap();
        assert!(!other.is_actionable(REWARD));
        assert!(other.is_actionable("some-other-reward"));
    }

    /// The configured id round-trips through a settings file, so it can arrive with whitespace or
    /// in a different case. Refusing it then would look like the feature is broken.
    #[test]
    fn the_configured_id_is_compared_sensibly() {
        let r = Redemption::from_event(&event("unfulfilled", "ABC-123", "x")).unwrap();
        for configured in ["ABC-123", "abc-123", "  ABC-123  ", "abc-123\n"] {
            assert!(r.is_actionable(configured), "{configured:?}");
        }
        assert!(!r.is_actionable("ABC-124"));
        assert!(!r.is_actionable("ABC-12"), "not a prefix match");
    }

    /// A reward that does not ask for text arrives with an empty box. That is not an error: it earns
    /// a reply telling the viewer to type something, which the caller can only do if it can tell the
    /// difference between "no query" and "no redemption".
    #[test]
    fn an_empty_input_is_reported_rather_than_guessed() {
        let r = Redemption::from_event(&event("unfulfilled", REWARD, "")).unwrap();
        assert!(r.is_actionable(REWARD), "the redemption is still ours to answer");
        assert_eq!(r.query(), None);

        let spaces = Redemption::from_event(&event("unfulfilled", REWARD, "   ")).unwrap();
        assert_eq!(spaces.query(), None);

        // And a real one is cleaned the same way a chat query is.
        let messy = Redemption::from_event(&event("unfulfilled", REWARD, "  a   b  ")).unwrap();
        assert_eq!(messy.query().as_deref(), Some("a b"));
    }

    /// Every field the app relies on has to be required, so a payload that is missing one is a
    /// parse error rather than a redemption with an empty id that cannot be fulfilled or deduped.
    #[test]
    fn an_incomplete_payload_is_an_error() {
        assert!(Redemption::from_event(&json!({})).is_err());
        for missing in ["id", "user_login", "status", "reward", "user_id", "user_name"] {
            let mut e = event("unfulfilled", REWARD, "x");
            e.as_object_mut().unwrap().remove(missing);
            assert!(Redemption::from_event(&e).is_err(), "missing {missing} should fail");
        }
        // `user_input` is the exception: plenty of rewards do not ask for one.
        let mut e = event("unfulfilled", REWARD, "x");
        e.as_object_mut().unwrap().remove("user_input");
        assert_eq!(Redemption::from_event(&e).unwrap().user_input, "");
    }

    /// The status is compared case-insensitively, because it is Twitch's string and not a value this
    /// app controls. Being strict here would mean silently ignoring every redemption the day Twitch
    /// changes one letter's case.
    #[test]
    fn the_status_comparison_is_lenient() {
        for status in ["unfulfilled", "UNFULFILLED", "Unfulfilled"] {
            let r = Redemption::from_event(&event(status, REWARD, "x")).unwrap();
            assert!(r.is_unfulfilled(), "{status}");
            assert!(r.is_actionable(REWARD), "{status}");
        }
        for status in ["fulfilled", "canceled", "", "unknown"] {
            let r = Redemption::from_event(&event(status, REWARD, "x")).unwrap();
            assert!(!r.is_actionable(REWARD), "{status:?} must not act");
        }
    }
}
