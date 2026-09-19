fn main() {
    // Last.fm API credentials live in a gitignored `lastfm.keys` next to this file (the repo is
    // public — they must never be tracked). Format: `LIMUSIC_LASTFM_API_KEY=…` and
    // `LIMUSIC_LASTFM_API_SECRET=…`, one per line. Exported as compile-time env for `option_env!`
    // in lastfm.rs; missing file just means the scrobbler reports "not configured".
    println!("cargo:rerun-if-changed=lastfm.keys");
    if let Ok(keys) = std::fs::read_to_string("lastfm.keys") {
        for line in keys.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if k == "LIMUSIC_LASTFM_API_KEY" || k == "LIMUSIC_LASTFM_API_SECRET" {
                    println!("cargo:rustc-env={k}={v}");
                }
            }
        }
    }
    // The Twitch client ID, same mechanism and same gitignored file convention (`twitch.keys`).
    // It is NOT a secret — it travels in a header on every request and appears in the consent URL
    // — so this only exists so a fork can ship a working default instead of asking every user to
    // register their own application. A value pasted into Settings overrides it (twitch/mod.rs).
    println!("cargo:rerun-if-changed=twitch.keys");
    if let Ok(keys) = std::fs::read_to_string("twitch.keys") {
        for line in keys.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if k == "LIMUSIC_TWITCH_CLIENT_ID" {
                    println!("cargo:rustc-env={k}={v}");
                }
            }
        }
    }
    // tauri-build watches tauri.conf.json but not the icons it embeds, so editing a PNG here
    // leaves `generate_context!` emitting the old `default_window_icon` (window, tray, taskbar).
    println!("cargo:rerun-if-changed=icons");

    tauri_build::build()
}
