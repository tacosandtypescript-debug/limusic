//! Contract tests for the overlay page.
//!
//! These read `overlay/page.html` from disk rather than the compiled-in copy, so they can run here
//! without linking the app (which needs libmpv and, on this machine, a DLL the running app holds
//! open). What they guard is the rule the overlays were rebuilt around:
//!
//!   **one overlay = one fixed composition; resizing scales it, it never rearranges it.**
//!
//! That rule is easy to undo by accident. A single `@media (max-width: …)`, one `clamp(…, 4vw, …)`
//! for a title, and the same URL renders a different design depending on the browser source's size
//! — which is exactly the bug these overlays exist to remove, and exactly the kind of change that
//! looks harmless in a diff. So it is pinned here instead of trusted.
//!
//! The base sizes are also duplicated by necessity: the page cannot be read by the Svelte panel at
//! build time, so `OverlaySettings.svelte` carries its own copy to show the streamer what to type
//! into OBS. Two copies of a number drift; [`base_sizes_match_the_panel`] is what stops them.

/// The page is a shell plus these, in the order the shell links them.
///
/// Every assertion below is made against the whole set. The rules being checked are spread across
/// the files on purpose — that split is the point of it — so a test that read only `page.html`
/// would now be reading a hundred lines of markup and nothing else.
const SHELL: &str = include_str!("../../../src-tauri/src/overlay/page.html");
const PARTS: &[&str] = &[
    include_str!("../../../src-tauri/src/overlay/base.css"),
    include_str!("../../../src-tauri/src/overlay/designs/sleeve-wide.css"),
    include_str!("../../../src-tauri/src/overlay/designs/sleeve-tall.css"),
    include_str!("../../../src-tauri/src/overlay/designs/playout-wide.css"),
    include_str!("../../../src-tauri/src/overlay/designs/playout-tall.css"),
    include_str!("../../../src-tauri/src/overlay/designs/vinyl-wide.css"),
    include_str!("../../../src-tauri/src/overlay/designs/vinyl-tall.css"),
    include_str!("../../../src-tauri/src/overlay/overlay.js"),
];

/// How many *distinct* rules mention `needle`.
///
/// One file per overlay means a rule shared by the two orientations of a design now exists in both
/// files. A raw `matches()` count therefore says "two" for something that is one rule — which is
/// what these assertions actually mean. The copies are held identical by
/// `shared_rules_are_identical_across_an_overlays_pair`, so collapsing them loses nothing.
fn rule_count(src: &str, needle: &str) -> usize {
    let mut seen = std::collections::HashSet::new();
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.contains(needle))
        .filter(|l| seen.insert(*l))
        .count()
}

/// The two vinyl overlays, on their own.
///
/// One file per overlay means an assertion that means "each of these has X" has to look at each of
/// them. Counting occurrences across the whole page says three for a rule that is really two,
/// because the scrim both vinyl overlays share now exists in both files.
const VINYL_WIDE: &str =
    include_str!("../../../src-tauri/src/overlay/designs/vinyl-wide.css");
const VINYL_TALL: &str =
    include_str!("../../../src-tauri/src/overlay/designs/vinyl-tall.css");

/// Everything the browser receives, as one string.
///
/// Built once: it is around 95 KB, every test wants the same copy, and rebuilding it per assertion
/// would be a lot of copying for nothing.
fn page() -> &'static str {
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| {
        let mut s = String::from(SHELL);
        for part in PARTS {
            s.push('\n');
            s.push_str(part);
        }
        s
    })
}

/// One overlay, as declared in the page's `OVERLAYS` table.
#[derive(Debug, PartialEq)]
struct Decl {
    id: String,
    w: u32,
    h: u32,
}

/// Parse the `OVERLAYS` table out of the page's script.
///
/// Deliberately a hand-rolled scan rather than a JS engine: the shape is fixed and a real parser
/// would be a dependency for one regex-shaped job.
fn declared() -> Vec<Decl> {
    let start = page().find("const OVERLAYS = {").expect("the page must declare an OVERLAYS table");
    let body = &page()[start..];
    let end = body.find("};").expect("the OVERLAYS table must be closed");
    let mut out = Vec::new();
    for line in body[..end].lines() {
        // `  "sleeve-wide": { name: "Sleeve", variant: "horizontal", w: 620, h: 200 },`
        // Splitting on `":` leaves the opening quote on the key, hence the trim.
        let Some((key, rest)) = line.split_once("\":") else { continue };
        if !rest.contains("w:") || !rest.contains("h:") {
            continue;
        }
        let id = key.trim().trim_matches('"').trim().to_string();
        let num = |tag: &str| -> u32 {
            rest.split_once(tag)
                .and_then(|(_, after)| {
                    let digits: String =
                        after.trim_start().chars().take_while(char::is_ascii_digit).collect();
                    digits.parse().ok()
                })
                .unwrap_or(0)
        };
        out.push(Decl { id, w: num("w:"), h: num("h:") });
    }
    out
}

/// [`page()`] with its comments removed.
///
/// The rule is about code, and this file's own header explains — in prose — the `@media
/// (orientation: portrait)` and the `vw`/`vmin` sizes that were removed. Scanning the raw text made
/// the guard fail on its own documentation, which is the wrong kind of strict: a test nobody can
/// keep passing gets deleted instead of fixed.
fn code() -> String {
    // Walked as `char`, not as bytes: the page is full of `—`, `·`, `×` and box-drawing characters,
    // and slicing a `&str` at a byte offset that lands mid-character panics.
    let chars: Vec<char> = page().chars().collect();
    let at = |i: usize, c: char| chars.get(i) == Some(&c);
    let mut out = String::with_capacity(page().len());
    let mut i = 0;
    while i < chars.len() {
        if at(i, '<') && at(i + 1, '!') && at(i + 2, '-') && at(i + 3, '-') {
            i += 4;
            while i < chars.len() && !(at(i, '-') && at(i + 1, '-') && at(i + 2, '>')) {
                i += 1;
            }
            i += 3;
            continue;
        }
        if at(i, '/') && at(i + 1, '*') {
            i += 2;
            while i < chars.len() && !(at(i, '*') && at(i + 1, '/')) {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Does `haystack` contain a CSS length relative to the viewport, e.g. `4vw`, `100vh`, `2vmin`?
/// Found by scanning for a digit followed by the unit, so the word "view" cannot trip it.
fn has_viewport_unit(haystack: &str) -> Option<String> {
    for unit in ["vmin", "vmax", "vw", "vh"] {
        let bytes = haystack.as_bytes();
        let mut from = 0usize;
        while let Some(at) = haystack[from..].find(unit) {
            let idx = from + at;
            if idx > 0 && bytes[idx - 1].is_ascii_digit() {
                return Some(haystack[idx.saturating_sub(6)..(idx + unit.len()).min(haystack.len())]
                    .to_string());
            }
            from = idx + unit.len();
        }
    }
    None
}

/// The six overlays, with the base sizes the panel tells the streamer to use.
#[test]
fn six_overlays_are_declared_with_their_base_sizes() {
    let d = declared();
    let got: Vec<(&str, u32, u32)> =
        d.iter().map(|o| (o.id.as_str(), o.w, o.h)).collect();
    assert_eq!(
        got,
        vec![
            ("sleeve-wide", 620, 200),
            ("sleeve-tall", 380, 430),
            ("playout-wide", 760, 222),
            ("playout-tall", 380, 430),
            ("vinyl-wide", 680, 230),
            ("vinyl-tall", 400, 470),
        ],
        "an overlay was added, removed or resized — update the panel's copy of this table too"
    );
}

/// Every declared overlay must have its own `body[data-design="…"]` block, or it renders as the
/// default with no styling at all.
#[test]
fn every_declared_overlay_has_a_style_block() {
    for o in declared() {
        let needle = format!("body[data-design=\"{}\"]", o.id);
        assert!(page().contains(&needle), "{} is declared but has no CSS block", o.id);
    }
}

/// THE rule. A layout media query reintroduces "same URL, different design".
#[test]
fn the_page_has_no_layout_media_queries() {
    let src = code();
    let mut seen = 0;
    for (n, line) in src.lines().enumerate() {
        if !line.contains("@media") {
            continue;
        }
        seen += 1;
        assert!(
            line.contains("prefers-reduced-motion"),
            "line {}: only the reduced-motion query is allowed, found: {}",
            n + 1,
            line.trim()
        );
    }
    assert_eq!(seen, 1, "expected exactly one @media block, the reduced-motion one");
    // `orientation` is how the old build switched layout; it must stay gone.
    assert!(
        !src.contains("orientation"),
        "an orientation query is back — that is a second design wearing one URL"
    );
}

/// No size may depend on the viewport. Every dimension is a fixed pixel value, and the only thing
/// that changes with the container is the single uniform scale.
#[test]
fn no_size_is_relative_to_the_viewport() {
    let src = code();
    assert!(!src.contains("clamp("), "a clamp() reintroduces a size that follows the container");
    if let Some(found) = has_viewport_unit(&src) {
        panic!("viewport-relative unit in the overlay: …{found}…");
    }
}

/// The whole scaling model rests on one number. If the fit ever stops being a single `min()` of the
/// two axes, the composition starts to stretch instead of scale.
#[test]
fn the_scale_is_one_uniform_number() {
    assert!(
        page().contains("Math.min(window.innerWidth / spec.w, window.innerHeight / spec.h)"),
        "the fit must stay a single uniform scale over both axes"
    );
    // And it must be applied as a transform-like scale, never by resizing the box.
    assert!(page().contains("--fit"), "the fit has to reach the box through --fit");
    assert!(
        page().contains("scale: calc(var(--fit, 1) * var(--scale, 1))"),
        "the box is scaled by --fit times the user's multiplier"
    );
}

/// The base size has to come from the table, or the CSS and the JS can disagree about the box.
#[test]
fn the_base_size_is_set_from_the_table() {
    assert!(page().contains(r#"setProperty("--base-w", spec.w + "px")"#));
    assert!(page().contains(r#"setProperty("--base-h", spec.h + "px")"#));
    // And the box must actually be that size rather than a percentage of the canvas.
    assert!(page().contains("width: var(--base-w)"));
    assert!(page().contains("height: var(--base-h)"));
}

/// The five ways the overlay is allowed to talk to LiMusic. Anything else would 404 in OBS, on a
/// stream, at the worst possible moment.
#[test]
fn the_page_only_calls_endpoints_that_exist() {
    for endpoint in [
        r#"BASE + "state""#,
        r#"BASE + "control""#,
        r#"BASE + "cover?""#,
    ] {
        assert!(page().contains(endpoint), "page does not call {endpoint}");
    }
    assert!(
        page().contains(r#"location.pathname.replace"#),
        "the endpoints must be derived from the page's own path, or the token is skipped"
    );
    assert!(!page().contains(r#"fetch("/state""#), "a bare /state fetch misses the token");
}

/// The transport is the one element a viewer compares across overlays, so it is defined once. Any
/// per-design rule for the buttons or their icons is how the six drift apart — which is what had
/// happened: `playout` had shrunk them to 30×28 with a 3px radius.
#[test]
fn the_transport_is_defined_once_for_every_overlay() {
    let src = code();
    // Anchored to the start of a line, so a bare `.controls button {` is the sizing rule and the
    // `:hover` / `:active` / `body.idle …` variants — states of the same box — are not counted.
    let button_rules = src.matches("\n.controls button {").count();
    let icon_rules = src.matches("\n.controls svg {").count()
        + src.matches("\n.controls .play svg {").count();
    assert_eq!(button_rules, 1, "the transport button must have exactly one sizing rule");
    assert_eq!(icon_rules, 2, "one icon size and one for the play glyph");

    for (n, line) in src.lines().enumerate() {
        if line.contains("data-design") && (line.contains(".controls button") || line.contains(".controls svg")) {
            panic!("line {}: a per-overlay transport override: {}", n + 1, line.trim());
        }
    }
    // All three glyphs, so the transport cannot be half-designed.
    for glyph in ["data-action=\"prev\"", "data-action=\"toggle\"", "data-action=\"next\""] {
        assert!(page().contains(glyph), "the transport is missing {glyph}");
    }
}

/// The accent has to survive not being available. The canvas read is the only step that can fail on
/// its own — a cross-origin cover taints it — and when it does, the design keeps its own colour
/// rather than going colourless or keeping the previous track's.
#[test]
fn the_dynamic_accent_is_corrected_and_has_a_fallback() {
    let src = code();
    assert!(src.contains("getImageData"), "the accent comes from the artwork's pixels");
    assert!(
        src.contains("applyAccent(null)"),
        "there must be a path back to the stylesheet's own accent"
    );
    // Wrapped, so a SecurityError from a tainted canvas degrades instead of breaking the overlay.
    assert!(
        src.contains("try { applyAccent(extractAccent(next)); } catch (e) { applyAccent(null); }"),
        "extraction must be wrapped, or a tainted canvas stops the whole render"
    );
    // The correction band: the numbers that keep an accent legible on a near-black plate and stop it
    // competing with the title.
    //
    // Parsed rather than pinned. This comment used to say "loose bounds — the point is that a band
    // exists at all" while the assertions named four exact literals, so widening the band — a design
    // decision about how much of the artwork's own character the accent keeps, not a bug — broke a
    // test that was never trying to check those numbers. What it does check is that a band exists and
    // that its ends leave room: a ceiling at the title's brightness would put the accent in
    // competition with the words, and a floor above the plate's would make every cover the same.
    // Scoped to the extraction function: the tick loop clamps too, and its `Math.max(0, …)` would
    // otherwise be read as a floor of zero. The calls are nested — `Math.min(0.95, Math.max(0.30,
    // …))` — so the number is taken from the front of the argument list rather than up to the first
    // closing bracket, which is the inner call's.
    let scope = src
        .split_once("function extractAccent")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("\n}").map(|(body, _)| body))
        .expect("extractAccent must exist");
    let clamp = |call: &str| -> Vec<f64> {
        scope
            .match_indices(call)
            .filter_map(|(i, _)| {
                let rest = &scope[i + call.len()..];
                let end = rest
                    .find(|c: char| !c.is_ascii_digit() && c != '.')
                    .unwrap_or(rest.len());
                rest[..end].parse::<f64>().ok()
            })
            .collect()
    };
    let caps = clamp("Math.min(");
    let floors = clamp("Math.max(");
    assert!(caps.len() >= 2, "the accent needs a ceiling on both saturation and lightness");
    assert!(floors.len() >= 2, "and a floor on both");
    for v in &caps {
        assert!((0.5..=1.0).contains(v), "a ceiling of {v} is not one");
    }
    for v in &floors {
        assert!((0.1..=0.7).contains(v), "a floor of {v} leaves no room to vary");
    }
    // And an escape hatch, for a streamer who wants the artwork to stay out of it.
    assert!(src.contains(r#"q.get("dynamic") !== "0""#), "there must be a way to turn it off");
}

/// Both endless animations must be stoppable, and the record is driven from JS rather than by a CSS
/// animation precisely so it can wind down instead of stopping dead.
#[test]
fn the_ambient_motion_can_be_stopped() {
    let src = code();
    assert!(src.contains("prefers-reduced-motion"), "the media query must exist");
    assert!(src.contains("spinTick"), "the record must be driven by the eased loop");
    assert!(
        src.contains(r#"if (!REDUCED && id.startsWith("vinyl")) requestAnimationFrame(spinTick);"#),
        "the spin loop must not start at all under reduced motion"
    );
    assert!(
        !src.contains("animation: spin "),
        "the disc must not be spun by a CSS animation, which cannot wind down"
    );
}

/// Every overlay has to carry its own contrast. Nothing here can rely on the scene behind it being
/// cooperative: an overlay is dropped on gameplay, on a bright title card, on footage that happens
/// to sit in the same colours as the overlay does.
///
/// Three numbers do that work, and all three are decisions rather than accidents — which is why
/// they are pinned instead of trusted:
///
/// * the plate is **88%** opaque, because a translucent plate is only as dark as what is behind it
///   and 80% let a white scene lift the secondary text under 4.5:1;
/// * secondary ink is **0.68**, the lightest value that still reads on that worst case;
/// * the two `vinyl` overlays, which have no plate by design, each carry a shaped scrim instead.
#[test]
fn every_overlay_carries_its_own_contrast() {
    let src = code();
    assert!(
        src.contains("oklch(0.145 0.008 285 / 88%)"),
        "the plate opacity is the contrast floor for sleeve and playout"
    );
    assert!(src.contains("--ink-3:      oklch(0.68"), "secondary ink must stay above the floor");

    // vinyl has no plate, so it must have a support layer of its own. One selector covers both
    // variants — they share the scrim's geometry and differ only in where the core sits — so the
    // check accepts either the shared family selector or a per-variant one.
    for id in ["vinyl-wide", "vinyl-tall"] {
        let shared = src.contains(r#"body[data-design^="vinyl"] #now::before"#);
        let exact = src.contains(&format!(r#"body[data-design="{id}"] #now::before"#));
        assert!(shared || exact, "{id} has no support layer behind its text");
    }
    // And the support layers must be *soft*: a gradient that ends in `transparent` fades into the
    // scene, where a flat fill would read as a panel dropped on top of it.
    // Per overlay, because the two vinyl overlays share the scrim's geometry and one file per
    // overlay duplicated it: a count over the whole page says three for what is two scrims.
    for (id, sheet) in [("vinyl-wide", VINYL_WIDE), ("vinyl-tall", VINYL_TALL)] {
        assert!(
            sheet.contains("oklch(0 0 0 / 30%) 82%, transparent 100%)"),
            "{id}'s support layer must fade out rather than end at an edge"
        );
    }
    assert!(
        !src.contains("backdrop-filter: blur(10px)"),
        "the masked blur was replaced by a gradient: without a mask it ends in a visible rectangle"
    );

    // The plate designs keep their blur, which is what lets the scene's colour through the 88%.
    let blurs = src.matches("backdrop-filter: blur(").count();
    assert!(blurs >= 4, "sleeve and playout should still soften what is behind the plate");
}

/// The timestamps have to breathe under the bar.
///
/// On the two playout overlays the ticker was pinned with `position: absolute; bottom: 0`, on the
/// theory that it would land on the plate's edge. It did not: it resolved against an ancestor whose
/// bottom sat on the timestamps row, so the line rendered **one pixel** below the numbers. Measured
/// on a capture before the fix — times at rows 133-140, line at 141. That is what "glued to the
/// timeline" looks like as numbers, and nothing in the stylesheet said so out loud.
///
/// Hence the invariant: no bar leaves the flow, and the gap is a value someone can point at.
#[test]
fn the_timestamps_are_not_glued_to_the_bar() {
    let src = code();
    for (n, line) in src.lines().enumerate() {
        if line.contains(".bar") && line.contains("position: absolute") {
            panic!(
                "line {}: the bar must stay in the flow — out of it, where it lands depends on which \
                 ancestor establishes a containing block: {}",
                n + 1,
                line.trim()
            );
        }
    }
    assert!(src.contains("margin-top: 8px;"), "the base gap between bar and times");
    // Both playout overlays get more, because a hard 3px rule reads tighter against text than the
    // soft 4px pill the other designs use.
    assert_eq!(
        rule_count(&src, " .times { margin-top: 12px; }"),
        2,
        "both playout overlays need the wider gap"
    );
}

/// Timing lives in tokens, not in the rules that use them.
///
/// The brief pinned the bands — hover 120-180, buttons 150-250, text 250-350, artwork 350-500, a
/// whole change 500-700, a palette change 500-800 — and a scale is only a scale if nothing
/// bypasses it. A stray `animation: … 900ms` in one design block is invisible in review and is
/// exactly how six overlays end up feeling like six different products.
#[test]
fn motion_is_defined_once_in_tokens() {
    let src = code();
    for token in ["--dur-hover", "--dur-btn", "--dur-text", "--dur-art", "--dur-swap", "--dur-color"] {
        assert!(src.contains(&format!("{token}:")), "{token} is missing");
    }
    // Every animation and transition in the stylesheet must be expressed through a token.
    for (n, line) in src.lines().enumerate() {
        let trimmed = line.trim();
        if !(trimmed.starts_with("animation:") || trimmed.starts_with("transition:")) {
            continue;
        }
        for word in trimmed.split([' ', ',', ':']) {
            if word.ends_with("ms") && word[..word.len() - 2].chars().all(|c| c.is_ascii_digit()) {
                panic!("line {}: a literal duration outside the tokens: {}", n + 1, trimmed);
            }
        }
    }
    // And the tokens have to be inside the bands the brief set, or the scale drifts.
    let ms = |name: &str| -> u32 {
        src.split_once(&format!("{name}:"))
            .and_then(|(_, rest)| rest.split_once("ms"))
            .and_then(|(v, _)| v.trim().parse().ok())
            .unwrap_or(0)
    };
    // `--dur-text` and `--dur-art` are not in this list: they are derived per design from the
    // incoming half, and `a_track_change_adds_up` is what checks those against their bands.
    for (name, lo, hi) in [
        ("--dur-hover", 120, 180), ("--dur-btn", 150, 250),
        ("--dur-swap", 500, 700), ("--dur-color", 500, 800),
    ] {
        let v = ms(name);
        assert!((lo..=hi).contains(&v), "{name} is {v}ms, outside {lo}-{hi}ms");
    }
}

/// The card is one option that works on all six overlays, which is only true if every design builds
/// its surface from the same tokens. `auto` must stay an *absence* of override: it is the
/// appearance that was measured against hostile backgrounds, and a mode block that quietly changed
/// it would undo that work without touching a single verified value.
#[test]
fn the_card_is_one_system_for_every_overlay() {
    let src = code();
    for mode in ["transparent", "solid", "translucent", "glass"] {
        assert!(
            src.contains(&format!(r#"body[data-card="{mode}"]"#)),
            "the {mode} card mode has no block"
        );
    }
    assert!(
        !src.contains(r#"body[data-card="auto"] {"#),
        "`auto` must not override anything — it is the design's own surface"
    );
    // Every design has to actually consume the tokens, or choosing a mode changes nothing.
    let uses_bg = src.matches("background: var(--card-bg)").count();
    assert!(uses_bg >= 4, "only {uses_bg} designs take their surface from the card tokens");
    for required in ["--card-blur", "--card-radius", "--card-edge", "--card-shadow", "--card-sheen"] {
        assert!(src.contains(&format!("{required}:")), "{required} is never defined");
    }
    // Presets, so nobody has to set eight options by hand to get a coherent overlay.
    for preset in ["minimal", "card:", "glass:", "dynamic:", "vinyl:"] {
        assert!(src.contains(preset), "the {preset} preset is missing");
    }
}

/// A palette change is a blend, not a switch. The accent is tweened through the hue wheel, and the
/// tween only runs while a change is in flight — never on a timer, and never per frame while idle,
/// which is what keeps a six-hour stream from recalculating colours 60 times a second.
#[test]
fn the_palette_blends_rather_than_jumping() {
    let src = code();
    assert!(src.contains("function accentTo("), "there must be a tween");
    assert!(
        src.contains("const d = ((b - a + 540) % 360) - 180;"),
        "hue must interpolate the short way round, or red-to-blue sweeps back through green"
    );
    assert!(src.contains("accentRaf = requestAnimationFrame(step);"), "the tween is rAF-driven");
    // It ends. A tween with no terminal condition is a permanent per-frame cost.
    assert!(src.contains("if (k < 1) accentRaf = requestAnimationFrame(step);"));
    assert!(
        src.contains("cancelAnimationFrame(accentRaf)"),
        "a second change must cancel the first tween, not race it"
    );
}

/// One sequence, and the DOM is only touched between its two halves.
///
/// The failure this prevents is the one the brief named: old cover disappears, new cover appears,
/// both on the same frame. `swap-out` and `swap-in` are separate classes precisely because the
/// content changes between them — and `paint` has to sit between the two, not before or after.
#[test]
fn a_track_change_is_sequenced() {
    let src = code();
    let out = src.find(r#"body.classList.add("swap-out")"#).expect("no outgoing half");
    let paint = src.find("  paint(track);\n  body.classList.remove(\"swap-out\");")
        .expect("the content must change while the old one is invisible");
    let inn = src.find(r#"body.classList.add("swap-in")"#).expect("no incoming half");
    assert!(out < paint && paint < inn, "the sequence is out of order");
    // A reflow between the halves, or the incoming animation is dropped as a no-op.
    assert!(src.contains("void body.offsetWidth;"));
    // The stagger: the words must not all arrive on the same frame, and every delay is the
    // artwork's lead plus a step — see `a_track_change_adds_up`.
    assert!(src.contains("var(--art-lead)"), "the words must wait for the artwork's head start");
    for frac in ["0.05", "0.10", "0.15", "0.20"] {
        assert!(
            src.contains(&format!("calc(var(--art-lead) + var(--dur-enter) * {frac})")),
            "the text stagger is missing the {frac} step"
        );
    }
    // And the exit staggers too, or the content switches off instead of being replaced.
    //
    // The steps are read rather than pinned, and their *shape* is checked: five of them, in
    // increasing order, evenly spaced, and the last one small enough that the exit animation still
    // fits inside the half it belongs to. The exact fractions are a tuning decision and have already
    // moved once, from 0.06..0.22 to 0.03..0.15, to stop the card emptying out before the swap —
    // pinning them made that deliberate change look like a regression.
    let mut steps: Vec<f64> = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if !line.starts_with("body.swap-out") || !line.contains("text-out") {
            continue;
        }
        if let Some((_, rest)) = line.split_once("--dur-exit) * ") {
            if let Some((v, _)) = rest.split_once(')') {
                if let Ok(d) = v.trim().parse::<f64>() {
                    steps.push(d);
                }
            }
        }
    }
    steps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    steps.dedup();
    assert!(
        steps.len() >= 4,
        "the exit stagger has {} distinct steps, which is not a stagger",
        steps.len()
    );
    assert!(
        steps.windows(2).all(|w| w[1] > w[0]),
        "the exit steps are not in order: {steps:?}"
    );
    let gaps: Vec<f64> = steps.windows(2).map(|w| w[1] - w[0]).collect();
    let even = gaps.iter().cloned().fold(f64::INFINITY, f64::min) > 0.0
        && (gaps.iter().cloned().fold(0.0_f64, f64::max)
            - gaps.iter().cloned().fold(f64::INFINITY, f64::min))
            < 0.005;
    assert!(even, "the exit steps are not evenly spaced: {steps:?}");
    assert!(
        *steps.last().unwrap() < 0.30,
        "the last exit step starts at {:.2} of the half, leaving no room to animate",
        steps.last().unwrap()
    );
    // The entrance plays once, on the first paint, and never again.
    assert!(src.contains("if (!booted)"), "the entrance must be one-shot");
}

/// The two transport arrows have to point opposite ways.
///
/// They did not, for several rounds. `next` was `M5.5 12l9 7V5z`: move to (5.5,12), line to
/// (14.5,19), up to (14.5,5), close — a triangle whose *apex* sits at x=5.5 and whose base is at
/// x=14.5, so it pointed left, exactly like `prev`. Both arrows pointed backwards and it rendered
/// as a perfectly ordinary triangle, which is why reading the path, reading the markup and reading
/// the diff all passed it. It took a 5× zoom of the running overlay to see `◀ || ◀|`.
///
/// So the geometry is written out here, because the shape is the specification.
#[test]
fn the_transport_arrows_point_opposite_ways() {
    let src = code();
    // prev: bar at x 6-8, then a triangle with its apex on the LEFT at (9.5,12), base at x=18.5.
    assert!(
        src.contains(r#"d="M6 5h2v14H6zm3.5 7 9-7v14z""#),
        "prev must read |◀ — bar first, apex pointing left"
    );
    // next: apex on the RIGHT at (14.5,12) with the base at x=5.5, then the bar at x 16-18.
    assert!(
        src.contains(r#"d="M16 5h2v14h-2zM5.5 5l9 7-9 7z""#),
        "next must read ▶| — apex pointing right, mirror of prev rather than a repeat of it"
    );
    // Play is apex-right too, which is correct for a play glyph and is why it is spelled out: it
    // must not be "fixed" into pointing left by someone aligning it with `prev`.
    assert!(src.contains(r#"d="M7 4l13 8-13 8z""#), "play must point right");
}

/// LiMusic publishes the duration in two shapes, and the overlay must not confuse them.
///
/// `/state` carries `now.duration` as the *formatted* string ("4:03") and `state.duration` as the
/// number of seconds (242.401). The progress code read `track.duration || state.duration`, and
/// because a non-empty string is truthy the formatted one won — after which `"4:03" > 0` coerces to
/// NaN, the comparison is false, and the bar stayed at zero with `-0:00` beside it for the whole
/// song, against the real server, while the demo data (a plain number) looked perfect.
#[test]
fn the_duration_is_read_as_seconds() {
    let src = code();
    assert!(src.contains("function seconds(v)"), "there must be one place that reads a duration");
    assert!(
        src.contains("const dur = seconds(state && state.duration) || seconds(track.duration);"),
        "the numeric state duration is the one to read first"
    );
    assert!(
        !src.contains("const dur = track.duration ||"),
        "the formatted string must not shadow the number — a truthy string is how this broke"
    );
    // And the parser has to handle both a number and either string shape.
    assert!(src.contains(r#"if (typeof v === "string" && v.includes(":"))"#));
    assert!(src.contains("parts.reduce((acc, n) => acc * 60 + n, 0)"), "mm:ss and h:mm:ss");
}

/// A track change is one budget split into two halves, and nothing inside either half may be longer
/// than the half it sits in.
///
/// This started as a flat contradiction: the sequencer carried its own `SWAP_OUT` table — 170 ms for
/// sleeve, 330 for vinyl — while the animations it was cutting were written for `--dur-art`, 440 ms.
/// Every exit was cut at **39 % of its travel**, and because the incoming wait was the whole
/// `--dur-swap` on top, a change took 790–950 ms against the 500–700 the brief asked for.
///
/// Fixing that exposed the same shape one level down: the exit is now a per-design *share*, so a
/// longer exit means a shorter entrance, and anything fixed inside the entrance is cut again. Hence
/// `--dur-art` and `--dur-text` are `min()` against the half, and the stagger is a fraction of it.
/// This test walks the numbers for every design and fails if any of them overflows its half.
#[test]
fn a_track_change_adds_up() {
    let src = code();
    // The shape of the thing, which is what makes the arithmetic below possible.
    assert!(src.contains("--dur-exit:  calc(var(--dur-swap) * var(--exit-share))"));
    assert!(src.contains("--dur-enter: calc(var(--dur-swap) * (1 - var(--exit-share)))"));
    // Read out of the stylesheet rather than pinned as text. This test is named for the arithmetic
    // and it was checking four literals, so when the exits were lengthened on purpose — to stop the
    // card emptying out between the two halves — it failed on the change and not on the sum. The
    // numbers below are the sums, and they are what the design actually promises.
    let factor = |token: &str| -> f64 {
        src.split_once(token)
            .and_then(|(_, rest)| rest.split_once('*'))
            .map(|(_, v)| v.trim())
            .and_then(|v| v.split(')').next())
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.0)
    };
    let exit_art_share = factor("--dur-exit-art:");
    let exit_text_share = factor("--dur-exit-text:");
    assert!(
        (0.5..=1.0).contains(&exit_art_share),
        "the artwork leaves over {exit_art_share:.2} of the outgoing half"
    );
    assert!(
        (0.5..=1.0).contains(&exit_text_share),
        "the words leave over {exit_text_share:.2} of it"
    );
    assert!(src.contains("--dur-art:   min(440ms, calc(var(--dur-enter) * 0.95))"));
    assert!(src.contains("--dur-text:  min(320ms, calc(var(--dur-enter) * 0.66))"));
    assert!(src.contains("--art-lead:  calc(var(--dur-enter) * 0.12)"));
    assert!(src.contains("await wait(T.exit);") && src.contains("await wait(T.enter);"));
    assert!(!src.contains("SWAP_OUT"), "the timing table is back — it is the thing that drifted");
    // `body`, not `:root`: a custom property substitutes where it is declared, so a share overridden
    // on the body would never reach a `--dur-exit` declared on the root.
    assert!(src.contains("  const cs = getComputedStyle(body);"), "the tokens must be read from body");

    let number = |key: &str| -> f64 {
        src.split_once(key)
            .map(|(_, rest)| rest.trim_start())
            .and_then(|rest| rest.split_once(|c: char| !c.is_ascii_digit() && c != '.'))
            .and_then(|(v, _)| v.parse().ok())
            .unwrap_or(0.0)
    };
    let swap = number("--dur-swap:");
    assert!((500.0..=700.0).contains(&swap), "a whole change is {swap}ms, outside the 500-700 band");

    let mut shares: Vec<(String, f64)> = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if !line.starts_with("body[data-design") || !line.contains("--exit-share:") {
            continue;
        }
        let family = line.split('"').nth(1).unwrap_or("").trim_end_matches('^').to_string();
        shares.push((family, number("--exit-share:")));
    }
    for family in ["vinyl", "playout", "sleeve"] {
        assert!(
            shares.iter().any(|(f, _)| f.trim_end_matches('=') == family),
            "{family} has no exit share"
        );
    }

    for (family, share) in &shares {
        let exit = swap * share;
        let enter = swap * (1.0 - share);
        // Outgoing. The point of the outgoing half is that nothing has finished leaving when the
        // content changes: whatever is still moving when the swap happens is what keeps the card
        // from reading as blank in the middle of a change. That is the assertion, and it is the one
        // the old numbers failed — the artwork went at 0.85 and the words tailed out at 0.90, so the
        // card was empty for the whole join. It looked like a coarse transition because it was one.
        let exit_art = exit * exit_art_share;
        assert!(
            exit_art <= exit,
            "{family}: the artwork's exit ({exit_art:.0}ms) overruns --dur-exit"
        );
        assert!(
            exit - exit_art < 0.06 * exit,
            "{family}: the artwork has finished leaving {:.0}ms before the content changes — the card \
             empties out on the way in",
            exit - exit_art
        );

        // The stagger, read from the stylesheet: the first delay and the last, and the animation
        // they share. The last word must still be leaving at the moment of the swap.
        let mut delays: Vec<f64> = Vec::new();
        for line in src.lines() {
            let line = line.trim();
            if !line.starts_with("body.swap-out") || !line.contains("text-out") {
                continue;
            }
            if let Some((_, rest)) = line.split_once("--dur-exit) * ") {
                if let Some((v, _)) = rest.split_once(')') {
                    if let Ok(d) = v.trim().parse::<f64>() {
                        delays.push(d);
                    }
                }
            }
        }
        assert!(delays.len() >= 4, "{family}: the exit stagger has {} steps", delays.len());
        let first = delays.iter().cloned().fold(f64::INFINITY, f64::min);
        let last = delays.iter().cloned().fold(0.0_f64, f64::max);
        assert!(
            first < last,
            "{family}: the exit stagger ({first:.2}..{last:.2}) has no order to it"
        );
        let tail = exit * last + exit * exit_text_share;
        assert!(tail <= exit + 0.5, "{family}: the exit tail ({tail:.0}ms) overruns --dur-exit");
        assert!(
            exit - tail < 0.06 * exit,
            "{family}: the words finish leaving {:.0}ms before the content changes",
            exit - tail
        );

        // Incoming: the artwork leads, every word is delayed by the lead plus its own step.
        let art = 440.0_f64.min(enter * 0.95);
        let text = 320.0_f64.min(enter * 0.66);
        let tail = enter * 0.12 + enter * 0.20 + text;
        assert!(art <= enter, "{family}: the artwork ({art:.0}ms) does not fit in --dur-enter ({enter:.0}ms)");
        assert!(tail <= enter, "{family}: the text tail ({tail:.0}ms) overruns --dur-enter ({enter:.0}ms)");
        assert!(
            enter * 0.12 > 0.0 && enter * 0.12 < art,
            "{family}: the artwork must still be arriving when the first word starts"
        );
        // And the derived durations stay inside the bands the brief set.
        assert!((350.0..=500.0).contains(&art), "{family}: artwork {art:.0}ms outside 350-500");
        assert!((250.0..=350.0).contains(&text), "{family}: text {text:.0}ms outside 250-350");
    }

    // No exit animation may run longer than the half it has to fit in.
    for (n, line) in src.lines().enumerate() {
        if line.contains(".swap-out") && line.contains("animation:") && line.contains("var(--dur")
            && !line.contains("--dur-exit")
        {
            panic!("line {}: an exit animation not on --dur-exit: {}", n + 1, line.trim());
        }
    }
}

/// The rAF loops run for the whole stream, so they must not write to the DOM when nothing changed.
///
/// `elapsed` and `remaining` were rewritten with the *same string* sixty times a second to show a
/// value that changes once, which still invalidates layout on the row each time. And `--spin` was
/// written to `document.documentElement`, invalidating the style of the entire tree at 60 fps for a
/// rotation that only the cover reads.
#[test]
fn the_frame_loops_do_not_write_when_nothing_changed() {
    let src = code();
    assert!(src.contains("if (sec !== lastSec)"), "the timestamps must be written once a second");
    assert!(src.contains("if (pct !== lastPct)"), "the bar must be written only when it moves");
    assert!(src.contains("if (ring !== lastRing)"), "so must the ring");
    assert!(src.contains("if (paused !== lastPaused)"), "and the paused class is a state, not a tick");
    assert!(
        src.contains(r#"el.cover.style.setProperty("--spin""#),
        "the spin belongs on the disc, not on the document root"
    );
    assert!(
        !src.contains(r#"root.style.setProperty("--spin""#),
        "--spin on the root invalidates the whole tree every frame"
    );
}

/// The preview channel must be unreachable from a live overlay.
///
/// It exists because reloading the page to change the song plays the *entrance*, and the thing being
/// reviewed is the track change — which only happens when content changes inside a live page. So a
/// preview page posts a track in and the ordinary sequence runs. That is a listener accepting
/// messages from anywhere, which is fine for a preview and not fine for a browser source on stream:
/// hence the gate, and hence this test.
#[test]
fn the_preview_channel_is_demo_only() {
    let src = code();
    let gate = src
        .find(r#"if (q.get("demo") === "1") {"#)
        .expect("the preview channel must be gated on demo");
    assert_eq!(
        src.matches(r#"addEventListener("message""#).count(),
        1,
        "exactly one message listener"
    );
    let listener = src.find(r#"addEventListener("message""#).unwrap();
    assert!(listener > gate, "the message listener must sit inside the demo gate");
    // What it accepts is a track, not an instruction: the payload is shape-checked and nothing in it
    // is ever evaluated.
    assert!(src.contains("ev.data && ev.data.limusicPreview"), "the payload must be namespaced");
    assert!(
        src.contains(r#"typeof t.title !== "string""#),
        "the payload must be shape-checked before use"
    );
}

/// The ambient light is the largest area of colour in the overlay, so it must not change in a frame.
///
/// It did. The glow was one layer with `background-image: var(--cover)` and a
/// `transition: opacity var(--dur-color)` that never fired, because on a track change the opacity
/// did not change — the image did, and `background-image` is not an animatable property. So the
/// accent blended carefully over 680 ms while the thing that dominates the card snapped from one
/// colour to the next in a single frame. Exactly what the brief said must not happen.
#[test]
fn the_ambient_light_cross_fades() {
    let src = code();
    assert!(src.contains("function setGlow(url)"), "the glow needs a cross-fade");
    for layer in ["--cover-a", "--cover-b"] {
        assert!(
            src.contains(&format!("background-image: var({layer})")),
            "{layer} is declared but never used as a layer"
        );
    }
    assert!(!src.contains("background-image: var(--cover)"), "one layer cannot cross-fade");
    assert!(
        src.contains("transition: opacity var(--dur-color)"),
        "the cross-fade has to run in the same window as the accent"
    );
    // A wash, not a repaint: the brief asks for the background to stay stable and the accents to
    // change. At 0.55 and 1.8x saturation the card took the cover's colour outright.
    assert!(src.contains("--glow-wash:  0.30"), "the ambient wash must tint, not repaint");
    assert!(src.contains("saturate(var(--glow-sat))"));
}

/// Nothing may appear or disappear in a single frame.
///
/// `visibility` is not an interpolated property, so `body.idle .progress { visibility: hidden }` made
/// the timestamps, the album and the transport vanish instantly while the card around them was still
/// fading over `--dur-art`. The delay is what turns that pop back into a fade, and it has to live on
/// the idle rule so that coming *back* is immediate.
#[test]
fn hiding_waits_for_the_fade() {
    let src = code();
    assert!(
        src.contains("body.idle .progress, body.idle .album, body.idle .controls, body.idle .eq {"),
        "the idle rule must cover the four things it hides"
    );
    assert!(
        src.contains("transition: visibility 0s linear var(--dur-art);"),
        "hiding must wait for the fade, or the contents pop"
    );
}

/// The cover must not be animated twice at once.
///
/// `.art img` carries its own opacity/scale transition, driven by `.swapping`, and the `.art`
/// container runs a settle keyframe. On a track change both were live, so they multiplied: the
/// overshoot flattened (1.045 x 0.96 is barely a scale at all) and the fade turned quadratic.
#[test]
fn the_cover_is_animated_once() {
    let src = code();
    assert!(src.contains(".art img.swapping"), "the standalone cross-fade must exist");
    assert!(
        src.contains("if (!swapping) el.cover.classList.add(\"swapping\");"),
        "`.swapping` must stand down while the sequence is animating the same cover"
    );
}

/// A pause has to be visible without reading the controls, and the ambient motion has to exist.
///
/// Two gaps, both of them the same shape: something the brief asked for that was only applied
/// narrowly. A paused overlay was indistinguishable from a playing one on four of the six designs —
/// the glyph is a small detail and the record only turns on two — and the slow zoom the brief asked
/// for was on one design out of six, so the rest sat perfectly still once their entrance finished.
#[test]
fn a_pause_is_visible_and_the_artwork_breathes() {
    let src = code();
    // Pause: the artwork loses most of its colour, and the filter is slower than the button.
    assert!(
        src.contains("body.paused .art img { filter: saturate("),
        "a paused overlay must look paused"
    );
    assert!(
        src.contains("filter var(--dur-swap) var(--ease-out);"),
        "the desaturation must move slower than the control that caused it"
    );
    // The ambient zoom, on the designs whose artwork is otherwise still. Each design declares it in
    // its own file now, so this is checked per design rather than over one selector group — which is
    // what the split bought: sleeve.css owns sleeve's ambient motion and nothing else's.
    assert!(page().contains("@keyframes breathe"), "the ambient zoom must exist");
    for family in ["sleeve", "playout"] {
        assert!(
            page().contains(&format!(
                r#"body[data-design^="{family}"] .art img {{ animation: breathe var(--cycle-breathe)"#
            )),
            "{family}'s artwork never moves while playing"
        );
    }
    // Vinyl is excluded on purpose: its disc already turns, and two motions on the same square fight.
    assert!(
        !page().contains(r#"body[data-design^="vinyl"] .art img { animation: breathe"#),
        "vinyl must not get a second motion on the same square"
    );
    // And it stops with the music, on every design that has it.
    assert!(page().contains("body.paused .art img { animation-play-state: paused; }"));
}

/// Every documented parameter has to actually do something.
///
/// Four of them did not. The header documents `cardglow`, `cardcolor`, `cardborder` and
/// `cardshadow`; the code read `glow`, `color`, `border` and `shadow`. All four were silently
/// ignored — the worst way for a documented interface to be wrong, because nothing tells you, and
/// the person following the documentation concludes the feature is broken rather than misspelled.
#[test]
fn the_documented_parameters_are_the_ones_that_are_read() {
    let src = code();
    for (long, short) in [
        ("cardglow", "glow"),
        ("cardcolor", "color"),
        ("cardborder", "border"),
        ("cardshadow", "shadow"),
    ] {
        assert!(src.contains(long), "`{long}` is documented but never read");
        assert!(
            src.contains(&format!(r#"pickAny(["{long}", "{short}"]"#)),
            "`{long}` and `{short}` must both be accepted"
        );
    }
}

/// The ring and the bar are the same gesture, so a seek has to move them the same way.
///
/// The bar slides over 260ms. The ring teleported, because its stop is a custom property inside a
/// conic gradient and CSS cannot interpolate that — so it is eased per frame instead.
#[test]
fn the_ring_glides_like_the_bar() {
    let src = code();
    assert!(
        src.contains("ringShown += (p - ringShown) * Math.min(1, dt * 12)"),
        "the ring must be eased rather than snapped"
    );
    assert!(
        src.contains("if (jumped) ringShown = p;"),
        "a track change still snaps, exactly as the bar does"
    );
}

/// A transparent page is what makes it an overlay; a backdrop is opt-in for preview only.
#[test]
fn the_default_page_is_transparent() {
    assert!(page().contains("background: transparent"));
    for key in ["dark", "light", "checker"] {
        assert!(page().contains(&format!(r#"body[data-backdrop="{key}"]"#)), "no {key} backdrop");
    }
}
