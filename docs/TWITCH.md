# Twitch integration

Let viewers ask for songs and control playback from Twitch chat.

**Status: phase 2 of 6.** The account session works, and chat is being read: connect a Twitch
account, choose a channel, and watch messages arrive in Settings ▸ Twitch. Nothing acts on them
yet — parsing commands and moving the queue is phase 3. See [Phases](#phases) for what comes next
and [Architecture](#architecture) for why it is built this way.

---

## Setting it up

1. Register an application at <https://dev.twitch.tv/console/apps>. Any name and category will do.
   For **OAuth Redirect URLs**, enter `http://localhost` — the device-code flow never redirects, so
   the field only has to be non-empty for Twitch's form.
2. Copy the **Client ID** (30 letters and digits).
3. In Limusic: **Settings ▸ Twitch**, paste it into *Application client ID*, and press Save.
4. Press **Connect with Twitch**. A code appears; a browser tab opens on twitch.tv/activate. Enter
   the code there and approve.
5. Pick a channel. Your own, unless another channel has made you a moderator.
6. The **Chat** section appears and starts filling in. Say something in your channel's chat — it
   shows up within a second or so. If nothing arrives, the dot and the error line under it say why.

### Shipping a client ID with a build

The client ID is not a secret — it travels in a header on every request and appears in the consent
URL — so a fork can bake one in instead of asking every user to register an application. Put it in a
gitignored `src-tauri/twitch.keys`, the same mechanism `lastfm.keys` uses:

```
LIMUSIC_TWITCH_CLIENT_ID=your_thirty_character_id
```

A value pasted into Settings always wins over the bundled one, so a build-time default never traps
someone who needs their own.

---

## Architecture

The integration is a **sidecar**. It owns the Twitch connection and nothing else:

```
src-tauri/src/
  twitch/
    mod.rs          TwitchSession — session state, the hourly validate, the EventSub loop
    auth.rs         Device Code Grant, refresh, validate, revoke     (no Tauri, no SQLite)
    api.rs          the Helix client + error and rate-limit rules    (no Tauri, no SQLite)
    events.rs       the EventSub protocol: envelopes, sessions, chat (no Tauri, no SQLite)
    settings.rs     the versioned config blob                        (no Tauri)
    commands.rs     the tw_* Tauri commands
  lib.rs            three lines of wiring
ui/src/lib/
  api.ts            the tw_* wrappers and the TwitchSnapshot type
  twitch.svelte.ts  reactive mirror of the Rust session
  components/TwitchSettings.svelte
```

`auth.rs`, `api.rs`, `events.rs` and `settings.rs` deliberately import neither `tauri::` nor
`crate::`. They take an injected `reqwest::Client` and return data. That is what makes them
unit-testable outside the app — see [Testing](#testing).

**Why nothing here touches `AppState`.** The queue and the player have exactly one owner, and this
module is not it. When phase 3 gives Twitch something to do, it will do what Listen Together
already does: send a command down an `mpsc` channel that a bridge in `lib.rs` applies by calling
`AppState`'s existing public methods. Copying `listentogether/mod.rs` is the whole design — it
already solved cancellation, reconnection and sharing state with the UI.

The module is registered as its own managed state (`app.manage(twitch.clone())`), so `AppState` is
untouched. `src-tauri/src/commands.rs` is untouched too: the `tw_*` commands live in the module, so
the entire Twitch surface is one directory.

---

## The Twitch rules this code exists to satisfy

Each of these is a documented requirement or a documented failure mode, not a preference. They are
listed here so a later edit does not "simplify" one away.

| Rule | Where | Why it must stay |
|---|---|---|
| **No PKCE.** Use Device Code Grant with a **public** client. | `auth.rs` | Twitch does not implement PKCE — its OIDC discovery advertises only `client_secret_post` — and the Authorization Code Grant requires a `client_secret`, which a desktop binary cannot keep: *"never expose it to users, even in an obscured form"*. |
| **`/validate` on startup and hourly.** | `mod.rs::maintenance_loop` | *"Any third-party app that calls the Twitch APIs and maintains an OAuth session must call the /validate endpoint… when it starts and on an hourly basis thereafter."* Twitch audits this and *"reserves the right to take punitive action, such as revoking the developer's API key or throttling"*. |
| **Persist the rotated refresh token after every refresh.** | `auth.rs::TokenSet::merge_refresh`, `mod.rs::try_refresh` | Refresh tokens are single-use for a public client: using one invalidates it and returns a replacement. A token we fail to write back is a token that no longer works, and the next refresh 401s in a way that looks like a revoked login. |
| **Expect a re-login every ≤30 days.** | `mod.rs::try_refresh` | Refresh tokens issued to a **public** client expire 30 days after they are generated. `RefreshError::Rejected` (vs `Transient`) is what lets the UI say "connect again" instead of silently retrying a dead grant. |
| **Refresh reactively on 401, not on `expires_in`.** | `api.rs::HelixError::Unauthorized` | *"twitch recommends that apps reactively respond to HTTP status code 401."* The expiry is still recorded, for display and for an idle app. |
| **Retry **once** on 5xx.** | `api.rs::Helix::send` | *"If you receive an HTTP status code 503 (Service Unavailable) error, retry once."* One retry, not a backoff loop. |
| **`Ratelimit-Reset` is a Unix epoch, not a delta.** | `api.rs::HelixError::retry_after` | Reading it as a relative number produces a wait of ~1.7 billion seconds. Pinned by `retry_after_treats_reset_as_an_epoch`. |
| **Send both `Authorization` and `Client-Id`.** | `api.rs::Helix::authed` | A missing `Client-Id` is one of the documented causes of a 401, which is a confusing failure to debug. |
| **Do not over-ask for scopes.** | `auth.rs::SCOPES` | *"If you request more scopes than is required to support your app's functionality, Twitch may suspend your application's access to the Twitch API."* The list is the planned feature set across all phases; **trim it if phases are dropped.** |
| **A non-affiliate channel has no Channel Points.** | `api.rs::TwitchUser::can_use_channel_points`, surfaced as `channelPointsAvailable` | The API returns `403 "The broadcaster is not a partner or affiliate."` It is not a bug and not retryable, so the panel says so before rewards get configured that could never be created. |
| **Rewards must be created by *this* app.** | *(phase 4)* | *"The app used to create the reward is the only app that may delete it"*, and the 403 says the `Client-Id` must match the one that created it. We can *see* the streamer's dashboard rewards but never manage them, so phase 4 creates its own. |
| **Chat: 20 messages / 30s, and 1 hour of silence if exceeded.** | *(phase 3, `outbox.rs`)* | *"Twitch ignores the bot messages for 1 hour."* The send queue is not optional. |
| **Subscribe within 10s of `session_welcome`.** | `mod.rs::establish` | An unused session is closed with **4003**. The 10-second budget is Twitch's, and it is why `establish` fails fast rather than waiting on a socket that is already doomed. |
| **Dedupe by `message_id`.** | `events.rs::Dedupe` | Delivery is *at least once*: the same event can arrive twice, and when it does the id is identical. Acting twice on one `!play` queues the song twice. |
| **`session_reconnect` means "connect before you disconnect".** | `mod.rs::events_session` | Twitch gives 30 seconds' warning and says to use the URL verbatim. The replacement is opened and established *before* the current socket is dropped, so no events fall in the gap. |
| **A revoked subscription is not retried.** | `mod.rs::events_loop` | `EventSubError` carries `retryable`, so "no channel chosen", a 403 and a revocation stop the loop, while a dropped socket backs off and retries. Retrying a 4000-times-a-minute refusal is how an app gets throttled. |
| **Send nothing but Pong on the socket.** | `events.rs::open` | *"If you send a message to the server, except for Pong messages, the server closes the connection."* The module has no writer at all, so it cannot break this. |
| **Answer Ping frames.** | *not implemented, on purpose* | `tungstenite` queues the Pong while reading and `tokio-tungstenite` flushes it. Verified live: a session that never subscribes dies of **4003** (unused), not **4002** (ping/pong failure). |

---

## Testing

The three Tauri-free modules have 47 unit tests. They can be run two ways.

**Inside the repo** (needs `libmpv`, exactly like the app):

```bash
RUSTFLAGS="-L native=<dir with mpv.lib>" cargo test -p limusic-app --lib twitch
```

**Standalone**, without libmpv or a V8 link: a throwaway harness that compiles the same files
through `#[path]` and runs their tests plus a live smoke test against Twitch.

```bash
cargo test                              # the unit tests, offline
cargo test -- --ignored --nocapture     # + live checks against the real Twitch API
```

The live checks use a syntactically valid but non-existent client ID. They cannot complete an
authorisation — that needs a human — but they do prove, against the real server:

* the endpoints exist at the URLs we hard-coded, and our request shape is accepted (Twitch answers
  `invalid client`, a complaint about the *credential* rather than the *parameters*);
* `Authorization: OAuth <token>` is the header form `/validate` wants;
* Helix answers `Invalid OAuth token` and it maps to `HelixError::Unauthorized`, the variant the
  refresh path branches on;
* **the EventSub handshake works with no credentials at all**, because only *subscribing* needs a
  token. Connecting yields a real `session_welcome` (session id, `keepalive_timeout_seconds: 10`),
  a real `session_keepalive`, and then the documented **4003** close — *"the connection was never
  used"* — which is the only way to exercise the close-code mapping against a real close frame.

That last one also settles a question the code depends on: whether `tungstenite`'s automatic Pong is
enough. The session died of 4003 (never used), not 4002 (ping/pong failure), so it is.

UI: `cd ui && pnpm check`.

---

## Phases

| | What | State |
|---|---|---|
| 1 | Account session: device flow, refresh, hourly validate, channel selection, Settings panel | **done** |
| 2 | EventSub: one WebSocket, `channel.chat.message`, keepalive/reconnect/revocation, dedupe by `message_id` | **done** |
| 3 | Commands, permissions, cooldowns, chat outbox, the separate chat-request queue | next |
| 4 | Channel Points rewards and Bits | |
| 5 | Subs, gift subs, raids, follows, hype train | |
| 6 | A second adapter: TikTok LIVE, over the same interaction bus | |

Phases 3+ add fields to `settings::TwitchConfig`, which is `#[serde(default)]` and versioned, so an
existing stored blob keeps parsing.

### What phase 3 will need to change

`AppState` still has no idea Twitch exists, and that is the point: the module sends a
`TwitchCommand` down an `mpsc` channel and a bridge in `lib.rs` applies it by calling the public
methods the app already has (`play_next`, `add_to_queue`, `next_in_queue`, …). That bridge does not
exist yet because phase 2 has no command to send. Adding it is the first thing phase 3 does.
