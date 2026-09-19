//! Reading a request out of a chat message.
//!
//! ## What makes this worth its own file
//!
//! Almost nothing about it is hard, and every part of it is easy to get subtly wrong in a way that
//! only shows up on stream. The prefix has to be at the *start* of the message, or somebody saying
//! "I love !sr" queues a song. The command name has to match a whole word, or `!srsly` does too. The
//! query can be any script Twitch allows, so every slice has to land on a character boundary or the
//! app panics on the first Japanese title anyone requests.
//!
//! None of that needs a connection to test, so none of it lives where a connection is needed.

/// What a message turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// A request command with something to search for, already trimmed.
    Request(&'a str),
    /// The command on its own. Distinct from `NotACommand`: this one deserves a reply telling the
    /// viewer how to use it, and the other one does not.
    Bare,
    /// A command, but not one this app answers.
    Other(&'a str),
    /// An ordinary message.
    NotACommand,
}

/// The commands this app answers, and the prefix they are typed with.
#[derive(Debug, Clone)]
pub struct Commands {
    prefix: String,
    request: Vec<String>,
}

impl Default for Commands {
    fn default() -> Self {
        Self::new("!", &["sr", "songrequest", "request"])
    }
}

impl Commands {
    /// Aliases are stored lower-case, because the name is compared case-insensitively and doing it
    /// once here beats doing it on every message.
    pub fn new(prefix: &str, request_aliases: &[&str]) -> Self {
        Self {
            prefix: prefix.to_string(),
            request: request_aliases.iter().map(|a| a.to_ascii_lowercase()).collect(),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The request aliases, for showing in a chat reply or a settings hint.
    pub fn request_aliases(&self) -> Vec<String> {
        self.request.iter().map(|a| format!("{}{a}", self.prefix)).collect()
    }

    /// Classify a message.
    ///
    /// An empty prefix answers `NotACommand` for everything rather than for nothing: a settings file
    /// that lost its `!` would otherwise turn every message in the channel into a song request, and
    /// that is a far worse failure than a command that does not work.
    pub fn parse<'a>(&self, text: &'a str) -> Parsed<'a> {
        if self.prefix.is_empty() {
            return Parsed::NotACommand;
        }
        // Leading spaces only. A message that arrives with them is still a command; one that has
        // words before it is not.
        let Some(rest) = text.trim_start().strip_prefix(&self.prefix) else {
            return Parsed::NotACommand;
        };

        // The name ends at the first whitespace, which is also where the query begins. `find`
        // returns a byte index on a character boundary, so both slices are safe whatever script the
        // query is in.
        let (name, args) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, ""),
        };

        // The whole name, not a prefix of it: `!srsly` is not `!sr`.
        let name_lower = name.to_ascii_lowercase();
        if !self.request.iter().any(|a| *a == name_lower) {
            return Parsed::Other(name);
        }

        if args.is_empty() {
            Parsed::Bare
        } else {
            Parsed::Request(args)
        }
    }
}

/// Collapse a query for searching: runs of whitespace become one space, and the ends are trimmed.
///
/// Done before the search rather than after, because YouTube Music treats a double space as a
/// different query and a request pasted from a phone tends to carry both.
pub fn normalize_query(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Trim a query to something worth searching, or `None` if there is nothing left.
///
/// The length cap exists because the search is a URL and a chat message can be 500 characters of
/// anything. Truncation rather than refusal: a viewer who pasted a paragraph meant to request the
/// first line of it.
pub const MAX_QUERY_CHARS: usize = 120;

pub fn clean_query(raw: &str) -> Option<String> {
    let collapsed = normalize_query(raw);
    if collapsed.is_empty() {
        return None;
    }
    let mut out: String = collapsed.chars().take(MAX_QUERY_CHARS).collect();
    // Truncating mid-word leaves a trailing space more often than not.
    while out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds() -> Commands {
        Commands::default()
    }

    #[test]
    fn a_request_is_read() {
        let c = cmds();
        assert_eq!(
            c.parse("!sr Never Gonna Give You Up"),
            Parsed::Request("Never Gonna Give You Up")
        );
        // Aliases, and the name is not case-sensitive.
        assert_eq!(c.parse("!songrequest darude"), Parsed::Request("darude"));
        assert_eq!(c.parse("!SR darude"), Parsed::Request("darude"));
        assert_eq!(c.parse("!Sr Darude"), Parsed::Request("Darude"));
        // Surrounding spaces go; the query keeps the ones inside it.
        assert_eq!(c.parse("!sr    a  b   "), Parsed::Request("a  b"));
        assert_eq!(c.parse("   !sr leading spaces"), Parsed::Request("leading spaces"));
    }

    /// The prefix has to start the message. Somebody saying they love the command must not queue
    /// anything — and this is the mistake that only shows up once a channel is busy enough that
    /// nobody reads every message.
    #[test]
    fn the_prefix_must_come_first() {
        let c = cmds();
        for text in ["I love !sr", "what does !sr do", "hey !songrequest this", "-!sr", "a!sr b"] {
            assert_eq!(c.parse(text), Parsed::NotACommand, "{text:?}");
        }
    }

    /// A whole word, not a prefix of one. `!srsly` is not a request, and neither is `!sriracha`.
    #[test]
    fn the_command_name_is_a_whole_word() {
        let c = cmds();
        assert_eq!(c.parse("!srsly"), Parsed::Other("srsly"));
        assert_eq!(c.parse("!sriracha please"), Parsed::Other("sriracha"));
        assert_eq!(c.parse("!srx"), Parsed::Other("srx"));
        // Tabs and newlines end the name too, not just spaces.
        assert_eq!(c.parse("!sr\tsong"), Parsed::Request("song"));
        assert_eq!(c.parse("!sr\nsong"), Parsed::Request("song"));
    }

    /// The command on its own is its own case, because it earns a reply and a stray message does
    /// not. So does the bare prefix.
    #[test]
    fn a_bare_command_is_recognised_as_such() {
        let c = cmds();
        assert_eq!(c.parse("!sr"), Parsed::Bare);
        assert_eq!(c.parse("!sr   "), Parsed::Bare);
        assert_eq!(c.parse("!songrequest"), Parsed::Bare);
        // The prefix alone is not a request command, so it is an unknown one.
        assert_eq!(c.parse("!"), Parsed::Other(""));
    }

    /// A command this app does not answer is reported as a name rather than swallowed, so a future
    /// `!queue` can be added without changing the shape of this.
    #[test]
    fn unknown_commands_keep_their_name() {
        let c = cmds();
        assert_eq!(c.parse("!queue"), Parsed::Other("queue"));
        assert_eq!(c.parse("!skip now"), Parsed::Other("skip"));
        assert_eq!(c.parse("!SRX foo"), Parsed::Other("SRX"));
    }

    /// Queries are any script Twitch allows. Slicing at a byte offset that landed mid-character
    /// would panic the app on the first Japanese or Cyrillic request — which is a crash on stream,
    /// caused by somebody being polite in their own language.
    #[test]
    fn a_query_in_any_script_survives() {
        let c = cmds();
        for (text, want) in [
            ("!sr 夜に駆ける", "夜に駆ける"),
            ("!sr Привет, мир", "Привет, мир"),
            ("!sr 你好世界", "你好世界"),
            ("!sr 🎵 emoji song", "🎵 emoji song"),
            ("!sr café", "café"),
        ] {
            assert_eq!(c.parse(text), Parsed::Request(want), "{text:?}");
            // And the query survives being cleaned.
            assert_eq!(clean_query(want).as_deref(), Some(want));
        }
    }

    /// The truncation cap has to land on a character boundary as well. 120 characters of a script
    /// with three-byte characters is 360 bytes, and `truncate` would panic there.
    #[test]
    fn truncation_lands_on_a_character_boundary() {
        let long = "夜".repeat(400);
        let cleaned = clean_query(&long).expect("something should survive");
        assert_eq!(cleaned.chars().count(), MAX_QUERY_CHARS);
        assert!(cleaned.chars().all(|c| c == '夜'));
    }

    /// Cleaning normalises what a phone keyboard produces: runs of spaces, and ends that were
    /// trimmed by whichever client sent it.
    #[test]
    fn queries_are_cleaned() {
        assert_eq!(clean_query("  a   b  ").as_deref(), Some("a b"));
        assert_eq!(clean_query("a\t\tb").as_deref(), Some("a b"));
        assert_eq!(clean_query("a\nb").as_deref(), Some("a b"));
        // Nothing worth searching.
        assert_eq!(clean_query(""), None);
        assert_eq!(clean_query("     "), None);
        assert_eq!(clean_query("\t\n "), None);
        // The cap trims a trailing space rather than leaving one.
        let padded = format!("{} x", "a".repeat(MAX_QUERY_CHARS - 1));
        let cleaned = clean_query(&padded).unwrap();
        assert!(!cleaned.ends_with(' '), "{cleaned:?}");
        assert_eq!(cleaned.chars().count(), MAX_QUERY_CHARS - 1);
    }

    /// An empty prefix must disable commands rather than enable everything. A settings file that
    /// lost its `!` would otherwise make every message in the channel a song request.
    #[test]
    fn an_empty_prefix_disables_rather_than_opens() {
        let c = Commands::new("", &["sr"]);
        for text in ["!sr song", "sr song", "hello", "!"] {
            assert_eq!(c.parse(text), Parsed::NotACommand, "{text:?}");
        }
    }

    /// A prefix is configurable, and a multi-character one is not a special case.
    #[test]
    fn the_prefix_is_configurable() {
        let c = Commands::new("!!", &["sr"]);
        assert_eq!(c.parse("!!sr a song"), Parsed::Request("a song"));
        assert_eq!(c.parse("!sr a song"), Parsed::NotACommand, "a single ! is not the prefix");
        // And a multi-byte prefix does not panic.
        let c = Commands::new("♪", &["sr"]);
        assert_eq!(c.parse("♪sr a song"), Parsed::Request("a song"));
    }

    /// The aliases are shown to the viewer somewhere, so they have to come back with the prefix on
    /// them rather than as bare words.
    #[test]
    fn aliases_are_advertised_with_their_prefix() {
        let names = cmds().request_aliases();
        assert!(names.contains(&"!sr".to_string()));
        assert!(names.contains(&"!songrequest".to_string()));
        assert!(names.iter().all(|n| n.starts_with('!')), "{names:?}");
        assert_eq!(cmds().prefix(), "!");
    }
}
