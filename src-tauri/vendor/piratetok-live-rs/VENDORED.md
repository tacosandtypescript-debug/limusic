# Why `piratetok-live-rs` is vendored, and what was changed

This is `piratetok-live-rs` 0.2.0, copied from crates.io and modified in one place. The licence is
**0BSD**, which grants the right to use, copy, modify and distribute with no conditions — so this is
permitted, and there is no obligation attached to it.

## The problem: the published crate does not build

Its `build.rs` is a self-imposed source lint — file length limits, no `.unwrap()` in library code, no
glob imports — and it **fails on the crate's own source**. Every published version, 0.1.0 through
0.2.1, fails. Two separate causes:

1. **The exemption list uses forward slashes.** `LOC_EXEMPT` contains `"src/structs/proto/messages.rs"`
   and friends, while the scan builds paths with `Path`, which yields backslashes on Windows. So
   nothing is ever exempt, and `messages.rs` — 811 lines against a limit of 800 when exempt, 900 when
   not — trips the rule it was explicitly excused from.
2. **It scans `src/bin/`.** The rule against `.unwrap()` is meant for library code, but
   `scan_directory(Path::new("src"))` walks the CLI binaries too, and those are full of `expect` at
   the top level. They are behind the `cli` feature and a library consumer never compiles them.

The second cause fails on every platform. The first is Windows-only.

With `build.rs` reduced to `fn main() {}` — the only change made here — **the library itself compiles
cleanly** into LiMusic, which is how the decision to keep it was made rather than assumed.

## What was changed

- `build.rs` — reduced to an empty `main()`. The original is not preserved here; it is on crates.io
  under version 0.2.0 if anyone needs to read it.
- `src/bin/` and `src/main.rs` — removed. They are the crate's own debug tools (`record_capture`,
  `replay_capture`, `generate_manifest`), they are what tripped the second rule, and they are dead
  weight in a vendored dependency.
- `Cargo.toml` — the `[[bin]]`, `[[example]]` and `cli` feature entries removed with them.

**Nothing in `src/` was touched.** The protocol decoder, the WebSocket connection and the struct
definitions are exactly as published.

## Why not just depend on it from crates.io

Because it does not compile, so there is nothing to depend on. And because vendoring removes the
supply-chain risk as a side effect: the crate has five stars and one maintainer, and if it is
abandoned the code is already here and already under a licence that lets it stay.

## What to do when a new upstream version appears

Read its `build.rs` first. If the exemption list has been fixed to use `Path::join` — or the scan
skips `src/bin/` — the change to make here is to delete this file and depend on crates.io again.
That is the outcome to hope for; this directory exists because it was not available.
