//! Verification harness for the Tauri-free part of the LiMusic Twitch integration.
//!
//! It pulls in the three modules that carry the actual logic — `auth.rs` (Device Code Grant,
//! refresh, validate, revoke), `api.rs` (the Helix client and its error/retry rules) and
//! `settings.rs` (the versioned config blob) — through `#[path]`, so they are compiled from their
//! real location in the fork rather than from a copy that could drift.
//!
//! See `Cargo.toml` for why this exists instead of `cargo test -p limusic-app`.
//!
//! ```text
//! cargo test                       # the unit tests, offline
//! cargo test -- --ignored --nocapture   # + the live smoke test against Twitch
//! ```

#[path = "../../../src-tauri/src/twitch/auth.rs"]
pub mod auth;

#[path = "../../../src-tauri/src/twitch/api.rs"]
pub mod api;

#[path = "../../../src-tauri/src/twitch/settings.rs"]
pub mod settings;

#[path = "../../../src-tauri/src/twitch/events.rs"]
pub mod events;

/// The pure half of the overlay server: routing, the token gate, and the artwork allowlist.
///
/// Those tests could not run where they were written. The module they lived in pulls in `AppState`
/// and hyper, so its test binary needs a Tauri app and a `libmpv` runtime that the app's own test
/// binary cannot load here (`STATUS_ENDPOINT_NOT_FOUND`, 0xc0000139) — which left the two things
/// standing between a local process and an open proxy compiled but never executed. They execute
/// here.
///
/// `include_str!` inside it resolves relative to *that file*, so it still finds `page.html` and the
/// stylesheets next to it in the fork.
#[path = "../../../src-tauri/src/overlay/routing.rs"]
pub mod overlay_routing;
