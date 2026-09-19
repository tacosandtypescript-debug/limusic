# tools

The overlay tooling. It lives inside the repo on purpose: the tests are the safety net for the OBS
overlay work, and a net that does not come with the code is not a net. Everything here resolves its
paths relative to itself, so a fresh clone on another machine runs without editing a thing.

None of it ships. Nothing in `src-tauri` or `ui` imports any of it.

## `verify/` — the tests

A standalone crate that pulls the testable parts of the app in through `#[path]`, so they are
compiled from their real location rather than from a copy that could drift.

```sh
cd tools/verify
cargo test                              # everything, offline
cargo test page_contract                # the overlay page only
cargo test overlay_routing              # routing, the token gate, the artwork allowlist
cargo test -- --ignored --nocapture     # + the live smoke test, which talks to Twitch
```

**Why a separate crate instead of `cargo test -p limusic-app`.** The app's test binary needs a Tauri
app and, on Windows, a `libmpv` runtime it cannot load — it dies with `STATUS_ENTRYPOINT_NOT_FOUND`
(`0xc0000139`) before running a single test. That is not something to work around with a flag: it
means every test in the app was compiled and never executed. The parts that carry real logic were
moved somewhere they can run.

Three suites:

| file | what it holds |
|---|---|
| `src/lib.rs` + the `twitch/` modules | the Device Code Grant, the Helix client, the versioned config blob |
| `src/overlay/routing.rs` | the token gate, the artwork allowlist, the asset table |
| `tests/page_contract.rs` | the overlay page: timing arithmetic, the swap sequence, contrast, the design split |

The overlay tests read the page **from disk** and assert against the whole set — the shell plus every
stylesheet and the script — because that is what the browser receives. A rule being in the wrong
file is a failure, not a detail: that is how a global `prefers-reduced-motion` block was found
living inside one design's stylesheet, doing work for all six.

## `overlay-dev-server.py` — the overlay, off disk

```sh
python tools/overlay-dev-server.py     # http://127.0.0.1:8788
```

Serves `src-tauri/src/overlay/` straight off disk with a live-reload poll, which is the only way to
work on the overlay without rebuilding the app: the real server embeds every file with
`include_str!`, so **nothing reaches OBS until the Rust is rebuilt**.

| route | |
|---|---|
| `/` | the overlay, with the same query parameters the real server takes |
| `/switch` | the switcher: real songs from the database, plus the look controls |
| `/version` | a timestamp, which is what the reload poll reads |

Only the shared stylesheet, the six design sheets and the script are served, by name from an
allowlist — the same reasoning as the token on the real server, one order of magnitude simpler.

## `make-switcher.py` — regenerate the switcher

```sh
python tools/make-switcher.py
```

Reads the real library out of LiMusic's SQLite database and writes `overlay-switch.html`.

It exists because reloading the page to change the song plays the *entrance*, and what you are
looking at when you are working on motion is the track **change**. A preview page posts a track in
and the ordinary sequence runs. That channel is gated behind `?demo=1` and exists only for this.

## What is not here

The one-shot scripts that performed the splits — `split-overlay.py`, `split-designs.py`,
`consolidate-playout-typography.py`, `reflow-designs.py`. They were scaffolding for a single
restructure, they read line numbers that have since moved, and re-running one now would do damage
rather than work. The structure they produced is what is in the repo.
