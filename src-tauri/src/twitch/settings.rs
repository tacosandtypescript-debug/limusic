//! Where the Twitch integration keeps its state.
//!
//! Two storage shapes, deliberately separate:
//!
//! * **Non-secret configuration** — one `twitch_config` row holding a versioned JSON blob. This is
//!   the established idiom in this codebase for anything structured: `blocked.rs` keeps
//!   "a JSON array in one `settings` row, like `local_folders`", and the queue persists as
//!   `queue_json`. It also means adding a setting later needs no schema migration — the struct
//!   grows and older blobs keep parsing because every field is `#[serde(default)]`.
//!
//! * **Secrets** — their own rows, never inside the config blob and never in
//!   `commands.rs::UI_SETTINGS`. That whitelist is what the renderer may read and write, and the
//!   comment on it is explicit that auth material "never crosses into the webview". A Twitch access
//!   token is exactly that kind of material, so it is reachable only through the `tw_*` commands,
//!   which hand out a redacted snapshot instead.
//!
//! The client ID sits in between: it is not a secret (it is sent in a header on every request and
//! appears in the consent URL), so the UI may read and set it — which is the point, because it
//! lets a user paste their own without rebuilding the app.

use serde::{Deserialize, Serialize};

/// Settings keys. Prefixed so they cannot collide with the app's own keys.
pub const KEY_CONFIG: &str = "twitch_config";
pub const KEY_CLIENT_ID: &str = "twitch_client_id";
/// Secrets. Never leave Rust except inside an Authorization header.
pub const KEY_ACCESS_TOKEN: &str = "twitch_access_token";
pub const KEY_REFRESH_TOKEN: &str = "twitch_refresh_token";
pub const KEY_SCOPES: &str = "twitch_scopes";
pub const KEY_EXPIRES_AT: &str = "twitch_expires_at";
pub const KEY_VALIDATED_AT: &str = "twitch_validated_at";

/// Bumped when a change to the blob cannot be expressed by `#[serde(default)]` alone. Nothing
/// reads it yet; it exists so a future migration has something to branch on instead of guessing
/// from the presence of fields.
pub const CONFIG_VERSION: u32 = 1;

/// A client ID baked in at build time from a gitignored `src-tauri/twitch.keys`, mirroring how
/// `lastfm.rs` gets its API key. Optional: a user who never builds from source pastes theirs into
/// Settings instead, and one who builds a fork can ship a default.
pub fn bundled_client_id() -> &'static str {
    match option_env!("LIMUSIC_TWITCH_CLIENT_ID") {
        Some(v) => v,
        None => "",
    }
}

/// The integration's non-secret configuration.
///
/// Phase 1 needs only the channel. The chat, permissions, rewards and limits sections land in
/// later phases; they go in this same struct, and because every field is `#[serde(default)]` an
/// existing stored blob keeps parsing as they arrive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
// camelCase **both ways**, deliberately, unlike `api::TwitchUser` which is asymmetric.
//
// The difference is who the two formats belong to. `TwitchUser` is read from Twitch (snake_case,
// not our choice) and written to the webview (camelCase), so only one direction is ours to pick.
// This struct round-trips through our own storage: `to_json` writes the blob and `from_json` reads
// it straight back, so a serialize-only rename would write `channelLogin` and then look for
// `channel_login` on the next launch — the config would save and silently never load. A test
// (`json_round_trips`) pins this, because the failure is invisible until a restart.
//
// camelCase on disk matches the app's other JSON blobs: `queue_json` stores `currentIndex`,
// `playedFrom`, `sourceName`.
#[serde(rename_all = "camelCase")]
pub struct TwitchConfig {
    pub version: u32,
    /// The channel the bot listens to, as a login (lowercase, no `@`).
    pub channel_login: Option<String>,
    /// The same channel's numeric id. Every EventSub condition and most Helix parameters take an
    /// id rather than a login, so resolving it once at selection time avoids doing it on every
    /// call — and lets the UI show which channel is actually connected.
    pub channel_id: Option<String>,
    /// Connect on launch when a stored token validates. Off by default: an app that silently
    /// opens a network session and starts answering in someone's chat on startup is a surprise.
    pub auto_connect: bool,

    // ── Phase 3: answering the channel ───────────────────────────────────────────────────────────
    //
    // Every field below carries `#[serde(default)]`, and that is not tidiness. The blob is written
    // by this struct and read straight back, so a config saved before these fields existed has none
    // of them — and without the defaults the parse fails, `from_json` falls back to `default()`, and
    // the channel the streamer chose is silently gone on the next launch. The failure looks like
    // "Twitch forgot my channel", which is a bug report nobody would connect to a new setting.
    //
    // Off by default, like `auto_connect`: an app that starts answering a chat it was only
    // connected to would be a surprise, and a queue filled by strangers is worse than an empty one.
    /// Whether chat commands are answered at all.
    #[serde(default)]
    pub requests_enabled: bool,
    /// The character a command starts with.
    #[serde(default = "default_prefix")]
    pub command_prefix: String,
    /// The command names that ask for a song, without the prefix.
    #[serde(default = "default_aliases")]
    pub request_aliases: Vec<String>,
    /// The lowest role that may request, as `permissions::Role::label` writes it.
    #[serde(default = "default_role")]
    pub min_role: String,
    /// Seconds one viewer must wait between requests. Zero disables the window.
    #[serde(default = "default_user_cooldown")]
    pub user_cooldown_secs: u64,
    /// Seconds between any two requests, whoever makes them. Zero disables the window.
    #[serde(default = "default_global_cooldown")]
    pub global_cooldown_secs: u64,
    /// The Channel Points reward whose redemptions are song requests. Empty answers none.
    #[serde(default)]
    pub reward_id: String,
    /// Whether to say anything back in chat. Off means silent queueing, which some channels prefer.
    #[serde(default = "default_true")]
    pub reply_in_chat: bool,
}

fn default_prefix() -> String {
    "!".into()
}
fn default_aliases() -> Vec<String> {
    vec!["sr".into(), "songrequest".into(), "request".into()]
}
fn default_role() -> String {
    "everyone".into()
}
fn default_user_cooldown() -> u64 {
    30
}
fn default_global_cooldown() -> u64 {
    5
}
fn default_true() -> bool {
    true
}

impl Default for TwitchConfig {
    fn default() -> Self {
        TwitchConfig {
            version: CONFIG_VERSION,
            channel_login: None,
            channel_id: None,
            auto_connect: false,
            requests_enabled: false,
            command_prefix: default_prefix(),
            request_aliases: default_aliases(),
            min_role: default_role(),
            user_cooldown_secs: default_user_cooldown(),
            global_cooldown_secs: default_global_cooldown(),
            reward_id: String::new(),
            reply_in_chat: true,
        }
    }
}

impl TwitchConfig {
    /// Parse a stored blob. A corrupt or truncated row yields defaults rather than failing the
    /// app: the config is a convenience, and losing it must not be worse than losing a preference.
    pub fn from_json(raw: Option<&str>) -> Self {
        raw.filter(|s| !s.trim().is_empty())
            .and_then(|s| serde_json::from_str::<TwitchConfig>(s).ok())
            .map(|mut c| {
                c.version = CONFIG_VERSION;
                // Normalise whatever is on disk: a channel selected before this code existed, or
                // hand-edited, should still match when it reaches Helix.
                c.channel_login = c
                    .channel_login
                    .map(|l| l.trim().trim_start_matches('@').to_ascii_lowercase())
                    .filter(|l| !l.is_empty());
                c
            })
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    /// Whether a channel has been chosen. Chat needs one; connecting to the account does not.
    pub fn has_channel(&self) -> bool {
        self.channel_login.is_some()
    }
}

/// What the Settings panel sends when the phase 3 options are saved.
///
/// A whole struct rather than eight commands: the panel has all of it on screen at once, and saving
/// one field at a time would let a half-applied state exist — the reward set but requests still off,
/// or a cooldown of zero that the user thought they had changed.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSettings {
    pub enabled: bool,
    pub prefix: String,
    pub aliases: Vec<String>,
    pub min_role: String,
    pub user_cooldown_secs: u64,
    pub global_cooldown_secs: u64,
    pub reward_id: String,
    pub reply_in_chat: bool,
}

/// The longest a command prefix may be.
///
/// Not a technical limit — `chat.rs` handles multi-character prefixes — but a practical one: at some
/// length it stops being a prefix and starts being a word, and every message beginning with that
/// word becomes a request.
const MAX_PREFIX_CHARS: usize = 3;
/// Plenty for `sr`, `songrequest`, `request` and a translation or two.
const MAX_ALIASES: usize = 8;
/// An hour. A cooldown longer than that is a closed queue with extra steps, and `enabled: false`
/// says it better.
const MAX_COOLDOWN_SECS: u64 = 3600;

impl TwitchConfig {
    /// Apply the phase 3 options, refusing anything that would store a rule nobody meant.
    ///
    /// This is where a typo is caught. `min_role` is a string in the file and a `Role` in the code,
    /// and a misspelling that was stored would read back as `None` — which the session resolves to
    /// `Everyone`, turning a strict channel permissive. Refusing at the door is the only place that
    /// can be prevented, because by the time it is read back the intent is gone.
    ///
    /// Pure, so all of it is tested without a session.
    pub fn apply_requests(&mut self, patch: RequestSettings) -> Result<(), String> {
        let prefix = patch.prefix.trim().to_string();
        if patch.enabled && prefix.is_empty() {
            return Err(
                "A command needs a prefix: with none, every message in chat would be one.".into()
            );
        }
        if prefix.chars().count() > MAX_PREFIX_CHARS {
            return Err(format!("The prefix can be at most {MAX_PREFIX_CHARS} characters."));
        }
        if prefix.chars().any(char::is_whitespace) {
            return Err("A prefix cannot contain a space.".into());
        }

        let mut aliases: Vec<String> = Vec::new();
        for raw in &patch.aliases {
            let name = raw.trim().trim_start_matches(&prefix).trim().to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            if name.chars().any(char::is_whitespace) {
                return Err(format!("`{raw}` is not a command name: it contains a space."));
            }
            if !aliases.contains(&name) {
                aliases.push(name);
            }
        }
        if patch.enabled && aliases.is_empty() {
            return Err("Name at least one command, e.g. `sr`.".into());
        }
        if aliases.len() > MAX_ALIASES {
            return Err(format!("At most {MAX_ALIASES} command names."));
        }

        let role = super::permissions::Role::parse(&patch.min_role).ok_or_else(|| {
            format!(
                "`{}` is not a role. Use one of: everyone, subscriber, vip, moderator, broadcaster.",
                patch.min_role.trim()
            )
        })?;

        if patch.user_cooldown_secs > MAX_COOLDOWN_SECS
            || patch.global_cooldown_secs > MAX_COOLDOWN_SECS
        {
            return Err(format!("A cooldown can be at most {MAX_COOLDOWN_SECS} seconds."));
        }

        self.requests_enabled = patch.enabled;
        self.command_prefix = prefix;
        self.request_aliases = aliases;
        // Stored as `parse` accepts it, so the file and the code agree on the spelling.
        self.min_role = role.label().to_string();
        self.user_cooldown_secs = patch.user_cooldown_secs;
        self.global_cooldown_secs = patch.global_cooldown_secs;
        self.reward_id = patch.reward_id.trim().to_string();
        self.reply_in_chat = patch.reply_in_chat;
        Ok(())
    }
}

#[cfg(test)]
mod request_settings_tests {
    use super::*;

    fn patch() -> RequestSettings {
        RequestSettings {
            enabled: true,
            prefix: "!".into(),
            aliases: vec!["sr".into(), "songrequest".into()],
            min_role: "everyone".into(),
            user_cooldown_secs: 30,
            global_cooldown_secs: 5,
            reward_id: "abc-123".into(),
            reply_in_chat: true,
        }
    }

    #[test]
    fn a_good_patch_is_stored() {
        let mut c = TwitchConfig::default();
        c.apply_requests(patch()).unwrap();
        assert!(c.requests_enabled);
        assert_eq!(c.command_prefix, "!");
        assert_eq!(c.request_aliases, vec!["sr", "songrequest"]);
        assert_eq!(c.min_role, "everyone");
        assert_eq!(c.user_cooldown_secs, 30);
        assert_eq!(c.reward_id, "abc-123");
        assert!(c.reply_in_chat);
    }

    /// The one that matters most. A misspelt role stored as-is reads back as `None`, and the session
    /// resolves `None` to `Everyone` — so a channel that asked for subscribers would quietly open
    /// its queue to the channel. Refusing here is the only place it can be caught.
    #[test]
    fn a_misspelt_role_is_refused() {
        let mut c = TwitchConfig::default();
        let err = c.apply_requests(RequestSettings { min_role: "moderater".into(), ..patch() });
        assert!(err.is_err(), "a typo must not be stored");
        assert!(err.unwrap_err().contains("moderater"), "and the message names it");
        // Nothing was applied.
        assert!(!c.requests_enabled);
    }

    /// Roles are stored in the spelling `parse` accepts, so the file and the code agree. The panel
    /// may send `MOD` or `subs`; what lands on disk is the canonical word.
    #[test]
    fn a_role_is_stored_canonically() {
        for (sent, want) in [
            ("MOD", "moderator"),
            ("subs", "subscriber"),
            ("  vip  ", "vip"),
            ("streamer", "broadcaster"),
        ] {
            let mut c = TwitchConfig::default();
            c.apply_requests(RequestSettings { min_role: sent.into(), ..patch() }).unwrap();
            assert_eq!(c.min_role, want, "sent {sent:?}");
        }
    }

    /// An empty prefix with requests on is refused, because `chat.rs` resolves an empty prefix to
    /// "no commands at all" — so the panel would show requests enabled and nothing would ever fire.
    #[test]
    fn an_empty_prefix_is_refused_while_enabled() {
        let mut c = TwitchConfig::default();
        assert!(c.apply_requests(RequestSettings { prefix: "   ".into(), ..patch() }).is_err());
        // But switching requests off with no prefix is a coherent state.
        assert!(c
            .apply_requests(RequestSettings { enabled: false, prefix: String::new(), ..patch() })
            .is_ok());
    }

    /// Aliases arrive as the panel typed them: with the prefix on, in mixed case, duplicated, or
    /// with blanks. All four are normalised rather than rejected — they are the same command.
    #[test]
    fn aliases_are_normalised_and_deduplicated() {
        let mut c = TwitchConfig::default();
        c.apply_requests(RequestSettings {
            prefix: "!".into(),
            aliases: vec![
                "!SR".into(),
                " sr ".into(),
                "SongRequest".into(),
                "".into(),
                "   ".into(),
                "songrequest".into(),
            ],
            ..patch()
        })
        .unwrap();
        assert_eq!(c.request_aliases, vec!["sr", "songrequest"]);
    }

    /// Asking for requests with nothing to type is refused: the feature would be on and unreachable.
    #[test]
    fn no_command_names_is_refused_while_enabled() {
        let mut c = TwitchConfig::default();
        assert!(c
            .apply_requests(RequestSettings { aliases: vec!["".into(), "  ".into()], ..patch() })
            .is_err());
        assert!(c
            .apply_requests(RequestSettings { enabled: false, aliases: vec![], ..patch() })
            .is_ok());
    }

    /// Everything is bounded, and the bounds are what stop a prefix that is really a word and a
    /// cooldown that is really a closed queue.
    #[test]
    fn the_bounds_hold() {
        let mut c = TwitchConfig::default();
        assert!(c.apply_requests(RequestSettings { prefix: "!!!!!".into(), ..patch() }).is_err());
        assert!(
            c.apply_requests(RequestSettings { prefix: "a b".into(), ..patch() }).is_err(),
            "a prefix with a space is not a prefix"
        );
        assert!(c
            .apply_requests(RequestSettings {
                aliases: (0..20).map(|i| format!("c{i}")).collect(),
                ..patch()
            })
            .is_err());
        assert!(c.apply_requests(RequestSettings { user_cooldown_secs: 9999, ..patch() }).is_err());
        // And at the edges it is accepted, so the bound is not off by one.
        assert!(c
            .apply_requests(RequestSettings {
                prefix: "!!!".into(),
                aliases: vec!["sr".into()],
                user_cooldown_secs: 3600,
                global_cooldown_secs: 3600,
                ..patch()
            })
            .is_ok());
    }

    /// A rejected patch leaves the config exactly as it was, so a bad save cannot half-apply.
    #[test]
    fn a_refused_patch_changes_nothing() {
        let mut c = TwitchConfig::default();
        c.apply_requests(patch()).unwrap();
        let before = c.clone();
        assert!(c
            .apply_requests(RequestSettings {
                aliases: vec!["sr".into()],
                min_role: "nonsense".into(),
                user_cooldown_secs: 99,
                ..patch()
            })
            .is_err());
        assert_eq!(c, before, "the cooldown must not have moved either");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blob written before phase 3 existed has none of its fields, and it still has to load.
    ///
    /// This is the failure that would be reported as "Twitch forgot my channel": the parse fails,
    /// `from_json` falls back to `default()`, and the chosen channel is gone — with nothing on
    /// screen connecting it to a setting added months later. `#[serde(default)]` on every new field
    /// is what prevents it, and this is the test that says so.
    #[test]
    fn a_config_from_before_phase_3_still_loads() {
        let old =
            r#"{"version":1,"channelLogin":"shroud","channelId":"37402112","autoConnect":true}"#;
        let c = TwitchConfig::from_json(Some(old));

        assert_eq!(c.channel_login.as_deref(), Some("shroud"), "the channel was lost");
        assert_eq!(c.channel_id.as_deref(), Some("37402112"));
        assert!(c.auto_connect);
        // And the new settings arrive inert rather than arbitrary.
        assert!(!c.requests_enabled, "answering chat must not switch itself on");
        assert_eq!(c.command_prefix, "!");
        assert_eq!(c.min_role, "everyone");
        assert!(c.reward_id.is_empty(), "an unset reward must match no redemption");
        assert!(c.user_cooldown_secs > 0 && c.global_cooldown_secs > 0);
        assert_eq!(c.request_aliases, vec!["sr", "songrequest", "request"]);
    }

    /// The phase 3 settings survive a write and a read, including the list.
    #[test]
    fn the_phase_3_settings_round_trip() {
        let c = TwitchConfig {
            channel_login: Some("someone".into()),
            requests_enabled: true,
            command_prefix: "$".into(),
            request_aliases: vec!["pedir".into(), "sr".into()],
            min_role: "subscriber".into(),
            user_cooldown_secs: 90,
            global_cooldown_secs: 15,
            reward_id: "abc-123".into(),
            reply_in_chat: false,
            ..Default::default()
        };
        let json = c.to_json();
        // camelCase on disk, like the fields that were already there.
        for key in [
            "requestsEnabled",
            "commandPrefix",
            "requestAliases",
            "minRole",
            "userCooldownSecs",
            "globalCooldownSecs",
            "rewardId",
            "replyInChat",
        ] {
            assert!(json.contains(&format!("\"{key}\"")), "missing {key} in {json}");
        }
        assert_eq!(TwitchConfig::from_json(Some(&json)), c);
    }

    #[test]
    fn defaults_are_inert() {
        let c = TwitchConfig::default();
        assert_eq!(c.version, CONFIG_VERSION);
        assert!(!c.has_channel());
        // Not auto-connecting by default is a product decision, not an oversight.
        assert!(!c.auto_connect);
    }

    /// The blob is written and read by this same struct, so the round trip is the contract: a
    /// serialize-only rename here would store a key that `from_json` then fails to find, and the
    /// config would appear to save and silently reset on the next launch.
    #[test]
    fn json_round_trips() {
        let c = TwitchConfig {
            version: CONFIG_VERSION,
            channel_login: Some("shroud".into()),
            channel_id: Some("37402112".into()),
            auto_connect: true,
            // Phase 3's fields are set below by their own test; this one is about the round trip,
            // and `..Default::default()` keeps it from having to be edited every time a setting is
            // added. That is not hypothetical — it failed to compile the moment these eight arrived.
            ..Default::default()
        };
        let json = c.to_json();
        // Pin the on-disk keys, which is what a mismatched rename would break.
        assert!(json.contains("\"channelLogin\""), "stored blob keys: {json}");
        assert!(json.contains("\"autoConnect\""), "stored blob keys: {json}");
        assert_eq!(TwitchConfig::from_json(Some(&json)), c);
    }

    /// The forward-compatibility promise: a blob written by an older build (missing keys) and one
    /// written by a newer build (extra keys) must both parse. This is what makes later phases
    /// additive instead of a migration.
    #[test]
    fn older_and_newer_blobs_both_parse() {
        let older = TwitchConfig::from_json(Some(r#"{"channelLogin":"shroud"}"#));
        assert_eq!(older.channel_login.as_deref(), Some("shroud"));
        assert_eq!(older.version, CONFIG_VERSION, "the version is stamped on load");
        assert!(!older.auto_connect);

        let newer = TwitchConfig::from_json(Some(
            r#"{"channelLogin":"x","some_future_field":{"a":1},"another":[1,2]}"#,
        ));
        assert_eq!(newer.channel_login.as_deref(), Some("x"));
    }

    /// A missing, empty or corrupt row degrades to defaults. A settings row is not worth an error
    /// path that can strand the app.
    #[test]
    fn unreadable_rows_degrade_to_defaults() {
        for raw in [None, Some(""), Some("   "), Some("not json"), Some("{truncated")] {
            assert_eq!(TwitchConfig::from_json(raw), TwitchConfig::default(), "{raw:?}");
        }
    }

    /// Logins are normalised on the way in so a pasted `@Name` still resolves.
    #[test]
    fn stored_logins_are_normalised_on_load() {
        let c = TwitchConfig::from_json(Some(r#"{"channelLogin":"  @Shroud "}"#));
        assert_eq!(c.channel_login.as_deref(), Some("shroud"));
        // An empty login is the same as no channel at all, not a channel named "".
        let empty = TwitchConfig::from_json(Some(r#"{"channelLogin":"@"}"#));
        assert!(!empty.has_channel());
    }

    /// The key names are a contract with whatever is already on disk; pin them so a rename is a
    /// deliberate act rather than a silent data loss.
    #[test]
    fn settings_keys_are_stable() {
        assert_eq!(KEY_CONFIG, "twitch_config");
        assert_eq!(KEY_CLIENT_ID, "twitch_client_id");
        assert_eq!(KEY_ACCESS_TOKEN, "twitch_access_token");
        assert_eq!(KEY_REFRESH_TOKEN, "twitch_refresh_token");
    }
}
