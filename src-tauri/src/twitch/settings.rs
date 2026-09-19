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
}

impl Default for TwitchConfig {
    fn default() -> Self {
        TwitchConfig {
            version: CONFIG_VERSION,
            channel_login: None,
            channel_id: None,
            auto_connect: false,
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

#[cfg(test)]
mod tests {
    use super::*;

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
