//! What happened to a request, and what to say about it.
//!
//! Every path through phase 3 ends here, and every one of them owes the viewer an answer. A request
//! that is silently dropped is worse than one that is refused: the viewer has spent points or typed
//! a command and has no way to tell whether it worked, so they do it again.
//!
//! ## Why the reply text lives in the module and not at the call site
//!
//! Because it is the only part of this the audience ever sees, and it was the part most likely to be
//! written inline, six times, in six slightly different voices. Here it can be read in one screen and
//! checked for the two things that actually break a Twitch message: a newline, which Twitch rejects
//! outright, and the length cap, which it also rejects — and a song title can carry both.

use super::cooldown::Refusal;
use super::permissions::Role;
use serde::Serialize;

/// Twitch's limit is 500 characters. Replying at 400 leaves room for the `@name ` prefix and for the
/// emote or command suffix some channels append, without ever being the message that got dropped.
pub const MAX_REPLY_CHARS: usize = 400;

/// The song a request resolved to, already looked up.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub title: String,
    pub artist: String,
}

/// How a request ended.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Outcome {
    /// It is in the queue.
    Queued { song: Found, position: usize },
    /// The search came back empty.
    NoMatch { query: String },
    /// The command arrived without anything to search for.
    NothingAsked,
    /// A cooldown is still running.
    TooSoon(Refusal),
    /// The viewer's role is below the configured minimum.
    NotAllowed(Role),
    /// The lookup itself failed. Not the viewer's fault, and worth saying so.
    LookupFailed,
}

impl Outcome {
    /// The line to send to chat.
    ///
    /// `user` is the display name, without the `@`.
    pub fn reply(&self, user: &str) -> String {
        let body = match self {
            Outcome::Queued { song, position } => {
                let artist = song.artist.trim();
                if artist.is_empty() {
                    format!("queued at #{position}: {}", song.title)
                } else {
                    format!("queued at #{position}: {artist} — {}", song.title)
                }
            }
            Outcome::NoMatch { query } => format!("nothing found for \"{query}\""),
            Outcome::NothingAsked => {
                "type a song after the command, e.g. !sr never gonna give you up".into()
            }
            Outcome::TooSoon(r) if r.is_global() => {
                format!("the queue is cooling down — try again in {}s", r.remaining_seconds())
            }
            Outcome::TooSoon(r) => format!("you can request again in {}s", r.remaining_seconds()),
            Outcome::NotAllowed(role) => match role {
                Role::Everyone => "song requests are closed right now".into(),
                other => format!("song requests are for {}", other.spoken()),
            },
            Outcome::LookupFailed => {
                "could not reach the music search — try again in a moment".into()
            }
        };
        sanitize(&format!("@{user} {body}"))
    }

    /// Only a queued request is worth counting as a successful one.
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Queued { .. })
    }
}

/// Make a string safe to send to Twitch.
///
/// Two things, both of which get a message rejected rather than truncated:
///
/// * **newlines.** A chat message is one line; Twitch refuses the send outright. A song title from
///   YouTube Music can contain one — they appear in the wild — and so can a reward's text box, which
///   a viewer types by hand into a field that accepts them.
/// * **length.** Over 500 characters and the send fails. Titles are not usually long, but a reward
///   prompt copied into a query can be.
///
/// Carriage returns are folded to spaces rather than dropped, so `"a\r\nb"` reads as `"a b"` instead
/// of `"ab"`.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_space = false;
    for ch in raw.chars() {
        let ch = if ch == '\n' || ch == '\r' || ch == '\t' {
            ' '
        } else if ch.is_control() {
            // Anything else non-printing: emotes and accents are fine, control codes are not.
            continue;
        } else {
            ch
        };
        if ch == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(ch);
    }
    let trimmed = out.trim();
    if trimmed.chars().count() <= MAX_REPLY_CHARS {
        return trimmed.to_string();
    }
    // Cut on a character boundary, then drop a trailing partial word.
    let cut: String = trimmed.chars().take(MAX_REPLY_CHARS - 1).collect();
    match cut.rfind(' ') {
        Some(i) if i > MAX_REPLY_CHARS / 2 => format!("{}…", cut[..i].trim_end()),
        _ => format!("{}…", cut.trim_end()),
    }
}

/// Which search result a request takes.
///
/// The first one, deliberately. YouTube Music orders by relevance and its first result is what a
/// person would have clicked; offering a viewer a menu in chat means a second round trip, a second
/// cooldown, and a bot that has to remember what it asked.
///
/// A function rather than an index at the call site so that the policy is in one place if it ever
/// needs to change — and so that "we take the first result" is a sentence in the code rather than
/// something a reader has to infer from `results.first()`.
pub fn pick<'a>(results: &'a [Found]) -> Option<&'a Found> {
    results.first()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn song() -> Found {
        Found { title: "Sandstorm".into(), artist: "Darude".into() }
    }

    #[test]
    fn a_queued_request_says_what_and_where() {
        let r = Outcome::Queued { song: song(), position: 3 };
        let text = r.reply("viewer");
        assert!(text.starts_with("@viewer "), "{text}");
        assert!(text.contains("Darude"), "{text}");
        assert!(text.contains("Sandstorm"), "{text}");
        assert!(text.contains('3'), "{text}");
        assert!(r.is_success());
    }

    /// A track with no artist must not produce a dangling separator. YouTube Music returns plenty of
    /// these — uploads, mixes, anything a label did not fill in — and `" — Title"` reads as a bug.
    #[test]
    fn a_song_without_an_artist_reads_cleanly() {
        let r = Outcome::Queued {
            song: Found { title: "Some Upload".into(), artist: "  ".into() },
            position: 1,
        };
        let text = r.reply("viewer");
        assert!(!text.contains('—'), "{text}");
        assert!(!text.contains("  "), "{text}");
        assert!(text.contains("Some Upload"), "{text}");
    }

    #[test]
    fn every_outcome_answers_and_none_is_a_success_but_queued() {
        let outcomes = [
            Outcome::NoMatch { query: "asdkjhasd".into() },
            Outcome::NothingAsked,
            Outcome::TooSoon(Refusal::User { remaining: Duration::from_secs(12) }),
            Outcome::TooSoon(Refusal::Global { remaining: Duration::from_secs(8) }),
            Outcome::NotAllowed(Role::Subscriber),
            Outcome::LookupFailed,
        ];
        for o in &outcomes {
            let text = o.reply("viewer");
            assert!(text.starts_with("@viewer "), "{o:?} -> {text}");
            assert!(text.len() > "@viewer ".len() + 4, "{o:?} says nothing: {text}");
            assert!(!o.is_success(), "{o:?} must not count as queued");
        }
    }

    /// The two cooldown refusals have to read differently. Telling a viewer to wait when waiting will
    /// not help is how a bot teaches people to stop using it.
    #[test]
    fn the_two_cooldowns_say_different_things() {
        let user =
            Outcome::TooSoon(Refusal::User { remaining: Duration::from_secs(12) }).reply("viewer");
        let global = Outcome::TooSoon(Refusal::Global { remaining: Duration::from_secs(12) })
            .reply("viewer");
        assert_ne!(user, global);
        assert!(user.contains("12s"), "{user}");
        assert!(global.contains("12s"), "{global}");
        assert!(user.contains("you"), "{user}");
        assert!(!global.contains("you can"), "the global one is not about them: {global}");
    }

    /// The reply names who may ask, using the role the viewer failed to reach.
    #[test]
    fn a_refusal_by_role_names_the_role() {
        assert!(Outcome::NotAllowed(Role::Subscriber).reply("v").contains("subscribers"));
        assert!(Outcome::NotAllowed(Role::Vip).reply("v").contains("VIPs"));
        assert!(Outcome::NotAllowed(Role::Moderator).reply("v").contains("moderators"));
        // `everyone` as a requirement means requests are switched off, which reads oddly if said
        // literally.
        let closed = Outcome::NotAllowed(Role::Everyone).reply("v");
        assert!(closed.contains("closed"), "{closed}");
    }

    /// Twitch rejects a message containing a newline outright, so a title that carries one must not
    /// take the whole reply with it. Titles with newlines exist; reward text boxes accept them, and a
    /// viewer typing a tracklist pastes one.
    #[test]
    fn a_newline_in_a_title_does_not_kill_the_reply() {
        let r = Outcome::Queued {
            song: Found { title: "Track One\nTrack Two".into(), artist: "Someone".into() },
            position: 1,
        };
        let text = r.reply("viewer");
        assert!(!text.contains('\n'), "{text:?}");
        assert!(!text.contains('\r'), "{text:?}");
        assert!(text.contains("Track One"), "{text}");
        assert!(text.contains("Track Two"), "the rest is not thrown away: {text}");

        // Folding rather than deleting, so words do not get glued together.
        assert_eq!(sanitize("a\r\nb"), "a b");
        assert_eq!(sanitize("a\n\nb"), "a b");
        assert_eq!(sanitize("a\tb"), "a b");
        // And control characters that are not whitespace are dropped.
        assert_eq!(sanitize("a\u{7}b"), "ab");
    }

    /// The length cap, which Twitch also enforces by rejecting the message.
    #[test]
    fn a_long_reply_is_cut_before_twitch_cuts_it() {
        let long = Outcome::Queued {
            song: Found { title: "x".repeat(900), artist: "y".repeat(200) },
            position: 1,
        };
        let text = long.reply("viewer");
        assert!(text.chars().count() <= MAX_REPLY_CHARS, "{} chars", text.chars().count());
        assert!(text.ends_with('…'), "a cut should look cut: {text}");

        // Cutting lands on a character boundary even in a script with wide characters.
        let wide = sanitize(&"夜".repeat(900));
        assert!(wide.chars().count() <= MAX_REPLY_CHARS);
        assert!(wide.chars().all(|c| c == '夜' || c == '…'));
    }

    /// The policy for choosing between results, in one place.
    #[test]
    fn the_first_result_is_taken() {
        let results = vec![
            Found { title: "Right One".into(), artist: "A".into() },
            Found { title: "Also This".into(), artist: "B".into() },
        ];
        assert_eq!(pick(&results).unwrap().title, "Right One");
        assert!(pick(&[]).is_none());
    }
}
