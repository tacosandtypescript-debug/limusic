/* The overlay runtime. Loaded at the end of the body, as before. */
"use strict";
"use strict";
const q = new URLSearchParams(location.search);
const body = document.body;
const root = document.documentElement;

/* ── The overlay registry ───────────────────────────────────────────────────────────────────────
   One entry per overlay, and the ONLY place a base size is written. The CSS reads `--base-w` and
   `--base-h`, which `apply()` below sets from here, so a size cannot drift between the two. */
const OVERLAYS = {
  "sleeve-wide":  { name: "Sleeve",  variant: "horizontal", w: 620, h: 200 },
  "sleeve-tall":  { name: "Sleeve",  variant: "vertical",   w: 380, h: 430 },
  "playout-wide": { name: "Playout", variant: "horizontal", w: 760, h: 222 },
  "playout-tall": { name: "Playout", variant: "vertical",   w: 380, h: 430 },
  "vinyl-wide":   { name: "Vinyl",   variant: "horizontal", w: 680, h: 230 },
  "vinyl-tall":   { name: "Vinyl",   variant: "vertical",   w: 400, h: 470 }
};
/* The first version shipped three ids and adapted them with a media query. Those links still work,
   and they land on the horizontal variant, which is what they used to render on a wide canvas. */
const ALIASES = { sleeve: "sleeve-wide", playout: "playout-wide", vinyl: "vinyl-wide" };

const wanted = (q.get("design") || "sleeve-wide").toLowerCase();
const id = OVERLAYS[wanted] ? wanted : (ALIASES[wanted] || "sleeve-wide");
const spec = OVERLAYS[id];

body.dataset.design = id;
body.dataset.pos = (q.get("pos") || "bl").toLowerCase();
if (q.get("idle") === "show") body.dataset.idle = "show";
if (q.get("backdrop")) body.dataset.backdrop = q.get("backdrop").toLowerCase();
if (q.get("scale")) root.style.setProperty("--scale", q.get("scale"));

root.style.setProperty("--base-w", spec.w + "px");
root.style.setProperty("--base-h", spec.h + "px");

/* The scale origin has to follow the anchor, or a bottom-right overlay grows off-canvas. */
const pos = body.dataset.pos;
root.style.setProperty("--origin",
  (pos[0] === "t" ? "top" : pos[0] === "m" ? "center" : "bottom") + " " +
  (pos[1] === "l" ? "left" : pos[1] === "c" ? "center" : "right"));

/* Nine of these open at once are otherwise indistinguishable in the title bar and the taskbar. */
document.title = spec.name + " · " + spec.variant + " · " + spec.w + "×" + spec.h + " · LiMusic";

/* ── The one thing that reacts to the container ────────────────────────────────────────────────
   `min(w/bw, h/bh)` scales the fixed box so it fits, uniformly. Nothing inside is touched: the
   composition, the proportions and the aspect ratio are all consequences of that single number.
   Setting the browser source to the base size evaluates to exactly 1. */
function fit() {
  const s = Math.min(window.innerWidth / spec.w, window.innerHeight / spec.h);
  root.style.setProperty("--fit", String(s));
}
fit();
window.addEventListener("resize", fit);

/* ═══════════════════════════════════════════════════════════════════════════════════════════════
   THE LOOK — card mode, preset, and the knobs on top
   ═══════════════════════════════════════════════════════════════════════════════════════════════ */

/** Named bundles, so nobody has to set eight options by hand to get a coherent overlay.
 *  A preset only fills in what was not asked for explicitly — `?preset=glass&cardop=0.9` means
 *  what it says. */
const PRESETS = {
  minimal: { card: "transparent", glow: 0,    shadow: 0,   border: 0 },
  card:    { card: "solid",       glow: 0.25, shadow: 1,   border: 1, radius: 20 },
  glass:   { card: "glass",       glow: 0.35, shadow: 1,   border: 1, radius: 20 },
  dynamic: { card: "translucent", glow: 0.7,  shadow: 1,   border: 1, color: "dynamic" },
  vinyl:   { card: "auto",        glow: 0.6,  shadow: 1,   border: 1 }
};
const preset = PRESETS[(q.get("preset") || "").toLowerCase()] || null;
/** Explicit parameter, else the preset, else the design's own default. */
const pick = (key, fallback) => (q.has(key) ? q.get(key) : (preset && key in preset ? preset[key] : fallback));
/** The same, for options with two accepted names.
 *
 *  The header documents `cardglow`, `cardcolor`, `cardborder` and `cardshadow`; the code read
 *  `glow`, `color`, `border` and `shadow`. So all four documented parameters did nothing at all —
 *  silently, which is the worst way for a documented interface to be wrong. Both spellings work
 *  now, the short one first because that is what the presets use internally. */
const pickAny = (names, fallback) => {
  for (const n of names) if (q.has(n)) return q.get(n);
  for (const n of names) if (preset && n in preset) return preset[n];
  return fallback;
};
const num = (v, d) => { const n = parseFloat(v); return Number.isFinite(n) ? n : d; };

const CARD = String(pick("card", "auto")).toLowerCase();
body.dataset.card = ["auto", "transparent", "solid", "translucent", "glass"].includes(CARD)
  ? CARD : "auto";
/* What each surface suggests for the ambient glow. It is a suggestion and not a rule, because the
   ambient light is painted *over* the card's own background — a `z-index: -1` pseudo-element still
   paints above its element — so at full strength it tints whatever surface is chosen. On `auto`
   that tint is the design and has to stay at 1; on a surface someone picked *because* they wanted
   a solid dark background, a full-strength wash of the artwork's colour is the opposite of the
   request. An explicit `cardglow` (or a preset) still wins. */
const MODE_GLOW = { auto: 1, transparent: 1, solid: 0.15, translucent: 0.7, glass: 0.8 };
const GLOW = Math.min(1, Math.max(0, num(pickAny(["cardglow", "glow"], MODE_GLOW[body.dataset.card] ?? 1),
                                          MODE_GLOW[body.dataset.card] ?? 1)));
const CARD_OP = Math.min(1, Math.max(0, num(pick("cardop", 0.88), 0.88)));
const CARD_COLOR = String(pickAny(["cardcolor", "color"], "neutral")).toLowerCase();

/* Only the values that differ from the stylesheet are written, so `auto` stays exactly the design
   that was measured and the CSS remains the single place the defaults live. */
root.style.setProperty("--glow", String(GLOW));
if (q.has("cardr")) root.style.setProperty("--card-radius", num(q.get("cardr"), 20) + "px");
if (q.has("cardpad")) root.style.setProperty("--pad", num(q.get("cardpad"), 22) + "px");
if (q.has("cardblur")) root.style.setProperty("--card-blur", Math.min(40, num(q.get("cardblur"), 20)) + "px");
if (String(pickAny(["cardborder", "border"], "1")) === "0") root.style.setProperty("--card-edge", "1px solid transparent");
if (String(pickAny(["cardshadow", "shadow"], "1")) === "0") root.style.setProperty("--card-shadow", "none");
/* `cardop` only means something for a surface that has an opacity to set — not for `auto`, whose
   value belongs to the design. */
if (q.has("cardop") || (preset && "opacity" in preset)) {
  root.style.setProperty("--card-bg", "oklch(0.145 0.008 285 / " + (CARD_OP * 100).toFixed(0) + "%)");
}

const $ = (elId) => document.getElementById(elId);
const el = { art: $("art"), cover: $("cover"), ring: $("ring"), title: $("title"),
             titleText: $("titleText"), artist: $("artist"), artistText: $("artistText"),
             album: $("album"), byline: $("byline"),
             fill: $("fill"), elapsed: $("elapsed"), remaining: $("remaining") };

/**
 * The queue entry's `from` label, as something worth reading.
 *
 * `twitch:<login>` is what the Twitch sidecar writes when a viewer spends points or types the
 * command. Anything else is the user's own — a playlist name, a folder — and is shown as it is
 * rather than guessed at.
 */
function byline(raw) {
  if (!raw) return "";
  const s = String(raw);
  const twitch = /^twitch:(.+)$/i.exec(s);
  return twitch ? "@" + twitch[1] : s;
}

/* Both endpoints live under the same token as the page, so they are derived from its own path
   rather than written as absolute "/state" — which would 404 and leave the overlay blank. */
const BASE = location.pathname.replace(/\/*$/, "/");
const STATE_URL = BASE + "state";
const CONTROL_URL = BASE + "control";
const COVER_URL = BASE + "cover?";

/** Motion timings, read from the stylesheet rather than repeated here: the tokens are the source of
 *  truth, and a JS constant alongside them is a second copy waiting to drift. That is not
 *  hypothetical — the sequencer used to carry its own `SWAP_OUT` table whose values disagreed with
 *  the CSS, and cut every outgoing animation short.
 *
 *  Read from `body`, not from `:root`: the halves are derived from `--exit-share`, which each design
 *  overrides on the body, and a custom property substitutes where it is *declared*. */
const T = (() => {
  const cs = getComputedStyle(body);
  const ms = (name, d) => { const v = parseFloat(cs.getPropertyValue(name)); return Number.isFinite(v) ? v : d; };
  return { text: ms("--dur-text", 300), art: ms("--dur-art", 440),
           exit: ms("--dur-exit", 218), enter: ms("--dur-enter", 462),
           swap: ms("--dur-swap", 680), color: ms("--dur-color", 680) };
})();
const REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const wait = (ms) => new Promise((r) => setTimeout(r, REDUCED ? 0 : ms));

/* ── Artwork ───────────────────────────────────────────────────────────────────────────────────
   When LiMusic serves the page, remote artwork goes through it rather than straight to Google, for
   three reasons that all showed up as "the cover does not load": YouTube still returns
   protocol-relative URLs (`//lh3.googleusercontent.com/…`) which resolve to `file://…` and fail
   silently off a local page; an OBS source would otherwise depend on Google's hotlinking
   behaviour; and it keeps the request on the machine that already holds the session.

   Opened from disk, the proxy must NOT be used: the relative URL would resolve to a file next to
   this one, which is a 404 the page cannot explain. */
const PROXIED = location.protocol !== "file:";

function coverSrc(raw) {
  const t = (raw || "").trim();
  if (!t) return "";
  if (t.startsWith("//")) return PROXIED ? COVER_URL + encodeURIComponent("https:" + t) : "https:" + t;
  if (/^https?:/i.test(t)) return PROXIED ? COVER_URL + encodeURIComponent(t) : t;
  return t;   // data: URLs (the generated demo art) are used as-is
}

/* ═══════════════════════════════════════════════════════════════════════════════════════════════
   COLOUR FROM THE ARTWORK

   The design keeps its identity and its background. What the record lends it is an *accent*: the
   progress fill, the vinyl ring, the eyebrow, the hairline around the cover, the glow. Everything
   else stays exactly as designed, so legibility cannot depend on what someone is listening to.

   Reading the pixels needs a canvas, and a canvas needs the image to be same-origin. In OBS it is:
   the page and the artwork both come from LiMusic's loopback server. A cover fetched straight from
   Google would taint the canvas and `getImageData` would throw — caught, not ignored: the
   stylesheet's own crimson stays and nothing else moves.
   ═══════════════════════════════════════════════════════════════════════════════════════════════ */
const DYNAMIC = q.get("dynamic") !== "0" && !q.get("accent");

/** `[h, s, l]`, h in degrees, s and l in 0..1. */
function rgbToHsl(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
  if (d === 0) return [0, 0, l];
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h;
  if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
  else if (max === g) h = (b - r) / d + 2;
  else h = (r - g) / d + 4;
  return [h * 60, s, l];
}

/** The stylesheet's own crimson, as HSL, so the tween has somewhere to return to. */
const DEFAULT_ACCENT = [352, 0.62, 0.40];
let accentNow = null;
let accentRaf = 0;

/**
 * The most usable colour in the artwork, or `null` when there is not one.
 *
 * Throws if the canvas is tainted; the caller turns that into the fallback. Sampling at 32×32
 * rather than at full size is deliberate — it averages out grain and JPEG noise for free, and it is
 * a thousand times fewer pixels to walk. Runs once per cover, never per frame.
 */
function extractAccent(img) {
  const size = 32;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = size;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(img, 0, 0, size, size);
  const { data } = ctx.getImageData(0, 0, size, size);

  // Bucket by hue and weight each pixel by how usable it would be as an accent.
  const bins = new Map();
  for (let i = 0; i < data.length; i += 4) {
    if (data[i + 3] < 200) continue;
    const [h, s, l] = rgbToHsl(data[i], data[i + 1], data[i + 2]);
    // Near-black, near-white and near-grey: the first two have no room to be lifted into a usable
    // accent, and greys carry a hue value that means nothing. Averaging them in is how this ends up
    // returning mud.
    if (l < 0.14 || l > 0.9 || s < 0.2) continue;
    // Saturation is squared, and that exponent is the whole fix.
    //
    // Weighting by `s` alone means a bin's score is roughly its *area*, so the winner is the
    // dominant colour — and on a photograph the dominant colour is skin or wood, whatever the cover
    // is actually about. Measured on six real ones: four of them came out between 25 and 54 degrees,
    // a range of warm browns no viewer could tell apart, and the complaint was that the colours
    // repeat. They did.
    //
    // An accent is not the commonest colour, it is the most *usable* one, so vividness has to beat
    // area. Squared is enough to lift a saturated detail over a large dull field without letting a
    // five-pixel logo win.
    let w = s * s * (1 - Math.abs(l - 0.5) * 1.4);
    if (w <= 0) continue;

    // Skin, down-weighted.
    //
    // Nine album covers in ten have a face on them, it occupies the middle of the frame, and its hue
    // sits between roughly 15 and 45 degrees — which is exactly where orange and amber live. So the
    // heaviest bin is skin almost every time, and the accent comes out the same warm brown for cover
    // after cover while the covers themselves are nothing alike. Measured on six real ones: four of
    // them landed between 25 and 54 degrees, a range no viewer could tell apart, and the feature
    // reads as broken rather than as faithful.
    //
    // Down-weighted and not excluded, because that range is also where a genuinely orange cover
    // lives: at 0.35 a real amber still wins when nothing else is competing, and a face loses to any
    // other colour that is actually there.
    const skin = h >= 12 && h <= 48 && s > 0.15 && s < 0.68 && l > 0.25 && l < 0.82;
    if (skin) w *= 0.35;
    // 24 bins of 15 degrees rather than 12 of 30. The buckets only decide *which* colour wins; the
    // value returned is the weighted average inside the winner, so a finer grid means two covers
    // 20 degrees apart are compared fairly instead of one of them being rounded into the other's
    // territory and dragging the average towards it.
    const key = Math.round(h / 15) % 24;
    const bin = bins.get(key) ?? { w: 0, h: 0, s: 0, l: 0 };
    bin.w += w; bin.h += h * w; bin.s += s * w; bin.l += l * w;
    bins.set(key, bin);
  }
  if (!bins.size) return null;

  let best = null;
  for (const bin of bins.values()) if (!best || bin.w > best.w) best = bin;

  // Corrected before use, and the band is the whole point: under 0.60 lightness the accent stops
  // reading as a colour on a near-black plate, over 0.75 it stops reading as an *accent* and starts
  // competing with the title. Saturation is floored too — a washed-out cover should still give the
  // progress bar something to be.
  // The band is still a band — under 0.58 of lightness the accent stops reading as a colour on a
  // near-black plate, over 0.78 it competes with the title — but it is wider than it was, so a muted
  // cover gives a muted accent instead of every cover giving the same one. That sameness was half
  // the complaint: four different album covers, four different hues, identical saturation and
  // identical lightness, so they read as one colour four times.
  // No 1.15 boost on the saturation, and that multiplier was doing harm by the end. It was there so a
  // washed-out cover would still give the bar something to be — but the band's own floor does that
  // now, and multiplying a source that is *already* saturated just pins every cover to the ceiling.
  // Measured: with the boost, four of six covers sat at 82-90% saturation, which is the "they all look
  // the same" complaint arriving by the other road. The colour the cover has is the colour it gets,
  // held only inside a band wide enough to stay legible on a near-black plate.
  return [
    best.h / best.w,
    Math.min(0.95, Math.max(0.30, best.s / best.w)),
    Math.min(0.84, Math.max(0.52, best.l / best.w))
  ];
}

const accentCss = (a) => `hsl(${a[0].toFixed(1)} ${(a[1] * 100).toFixed(1)}% ${(a[2] * 100).toFixed(1)}%)`;

/** Shortest way round the wheel, so a red-to-blue change passes through magenta rather than
 *  sweeping back through green. This is what makes the palette change read as a blend. */
function lerpHue(a, b, t) {
  const d = ((b - a + 540) % 360) - 180;
  return (a + d * t + 360) % 360;
}

/**
 * Move the accent to a new colour over `--dur-color`, rather than switching it.
 *
 * A jump is the one thing that makes dynamic colour look cheap: the bar, the ring and every glow
 * change in the same frame as the artwork. Tweening also means the palette change is *inside* the
 * track-change sequence rather than an event that happens during it.
 *
 * The tween runs only while a change is in flight — never on a timer, never per frame while idle.
 */
function accentTo(target) {
  const to = target || DEFAULT_ACCENT;
  const from = accentNow || DEFAULT_ACCENT;
  accentNow = to;
  cancelAnimationFrame(accentRaf);
  if (REDUCED) { root.style.setProperty("--accent", accentCss(to)); return; }
  const t0 = performance.now();
  const step = (now) => {
    const k = Math.min(1, (now - t0) / T.color);
    const e = 1 - Math.pow(1 - k, 3);
    root.style.setProperty("--accent", accentCss([
      lerpHue(from[0], to[0], e),
      from[1] + (to[1] - from[1]) * e,
      from[2] + (to[2] - from[2]) * e
    ]));
    if (k < 1) accentRaf = requestAnimationFrame(step);
  };
  accentRaf = requestAnimationFrame(step);
}

/** `null` restores the stylesheet's accent; `--accent-ink` and `--accent-soft` follow it, and so
 *  does everything they feed. */
function applyAccent(hsl) {
  if (!hsl) { accentTo(null); return; }
  accentTo(hsl);
  console.debug("overlay: accent from artwork", accentCss(hsl));
  // A surface derived from the record is only offered where it belongs: a solid card in `dynamic`
  // mode. It is pushed almost to black on purpose — vivid colour belongs in the accents.
  if (CARD_COLOR === "dynamic" && body.dataset.card === "solid") {
    root.style.setProperty("--card-bg", `hsl(${hsl[0].toFixed(0)} 30% 9%)`);
  }
}

/* Sample data, overridable from the URL so a real track can be dropped in without rebuilding the
   app that embeds this file. The artwork is generated when `dc` is absent, so the plain demo needs
   no network at all.

   `q.has`, not `||`: a track with no album sends an empty `dal`, and `||` reads that as "not given"
   and falls back to the sample album — so a song without one displayed someone else's. */
const DEMO = {
  videoId: q.get("dv") || "demo",
  title: q.has("dt") ? q.get("dt") : "Higher Ground",
  artists: q.has("da") ? q.get("da") : "Nova Sky",
  album: q.has("dal") ? q.get("dal") : "Night Signals",
  thumbnail: q.get("dc") || null,
  duration: Number(q.get("ddur")) || 214,
  position: Number(q.get("dpos")) || 78,
  // Preview-only, like everything else on this object: the paused look is a state someone has to be
  // able to *see* to review, and there is no way to reach it from the URL otherwise.
  paused: q.get("dpaused") === "1",
    // Same reasoning. A requested song is a state, and without this the byline could only be
    // looked at by connecting a channel and spending points on it.
    queuedFrom: q.get("dfrom") || null
};
const DEMO_ART = "data:image/svg+xml;utf8," + encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" width="300" height="300">' +
  '<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">' +
  '<stop offset="0" stop-color="#2a1f3d"/><stop offset="0.5" stop-color="#b8395a"/>' +
  '<stop offset="1" stop-color="#f0a03c"/></linearGradient></defs>' +
  '<rect width="300" height="300" fill="url(#g)"/>' +
  '<circle cx="150" cy="150" r="70" fill="none" stroke="rgba(255,255,255,.35)" stroke-width="2"/>' +
  '</svg>'
);

let state = null;                                 // last snapshot from Rust
let shownId = null;                               // which track the DOM currently shows
let anchor = { pos: 0, at: performance.now() };   // for interpolating between polls
let booted = false;                               // has the entrance played?
let previewTrack = null;                          // a track pushed in from a preview page

/* ── Preview channel ──────────────────────────────────────────────────────────────────────────
   Reloading the page to change the song plays the *entrance*, which is not the animation anyone is
   reviewing — the track change is, and it only happens when the content changes inside a live page.
   So a preview page can post a track in and the ordinary sequence runs.

   Gated on `demo=1`, which is a preview-only flag: an OBS browser source never sets it, so a live
   overlay cannot be driven from outside its own page. */
if (q.get("demo") === "1") {
  window.addEventListener("message", (ev) => {
    const t = ev.data && ev.data.limusicPreview;
    if (!t || typeof t !== "object" || typeof t.title !== "string") return;
    const first = shownId === null;
    previewTrack = t;
    if (first) { shownId = t.videoId; paint(t); }
    else if (t.videoId !== shownId) { shownId = t.videoId; swapTo(t); }
  });
}

/* Preview-only: change the track after `dswap` milliseconds.
 *
 * A track change is the one animation that cannot be seen without provoking one, and provoking it by
 * hand means clicking a window that has to be open, focused, positioned and scaled correctly — which
 * failed five times in a row while this was being written. A delay from the URL makes the whole
 * sequence reproducible, and it drives the real path: the same `swapTo` the preview channel and the
 * transport use, not a simplified copy of it. */
const swapAfter = Number(q.get("dswap")) || 0;
if (swapAfter > 0 && q.get("demo") === "1") {
  setTimeout(() => {
    demoAt = (demoAt + 1) % DEMO_SET.length;
    const next = DEMO_SET[demoAt];
    previewTrack = next;
    shownId = next.videoId;
    swapTo(next);
  }, swapAfter);
}

function fmt(s) {
  s = Math.max(0, Math.floor(s || 0));
  return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
}

/**
 * A duration, in seconds, from either shape LiMusic publishes it in.
 *
 * `/state` carries it twice: `now.duration` is the *formatted* string the app shows ("4:03") and
 * `state.duration` is the number of seconds (242.401). The progress code used to read
 * `track.duration || state.duration`, and because a non-empty string is truthy the formatted one
 * won — after which `"4:03" > 0` coerces to NaN, the comparison is false, and the bar sat at zero
 * with `-0:00` beside it for the whole song.
 *
 * It never showed up in the demo because the sample data's duration is a plain number. It took
 * running against the real server to see it, which is the whole argument for running against the
 * real server.
 */
function seconds(v) {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v.includes(":")) {
    const parts = v.split(":").map(Number);
    if (parts.every((n) => Number.isFinite(n))) return parts.reduce((acc, n) => acc * 60 + n, 0);
  }
  return 0;
}

/**
 * Hand the new artwork to the ambient light, cross-faded.
 *
 * `background-image` is not an animatable property, so a single layer cannot do this: it would jump
 * from one cover to the next in a frame. The incoming artwork goes on whichever of the two layers
 * is currently hidden, and then which one is showing swaps over `--dur-color` — the same window the
 * accent tween runs in, so the palette moves as one thing instead of part of it jumping.
 */
let glowFlip = 0;
function setGlow(url) {
  const next = glowFlip ^ 1;
  root.style.setProperty(next ? "--cover-b" : "--cover-a", 'url("' + url + '")');
  root.style.setProperty("--glow-a", next ? "0" : "1");
  root.style.setProperty("--glow-b", next ? "1" : "0");
  glowFlip = next;
}

/** Cross-fade the cover: a src swap repaints a blank frame otherwise, which reads as a flash. */
function setCover(raw) {
  const url = coverSrc(raw);
  if (url === el.cover.dataset.src) return;
  el.cover.dataset.src = url;
  if (!url) { el.art.classList.remove("has-art"); body.classList.remove("has-art"); return; }
  // Only when nothing else is already animating this cover. During a track change the `.art`
  // container runs its own settle, and this fade multiplies with it: the overshoot flattens
  // (1.045 x 0.96 is barely a scale at all) and the fade turns quadratic. So `.swapping` is for a
  // cover that changes on its own — a late thumbnail, a failure — not for one the sequence owns.
  if (!swapping) el.cover.classList.add("swapping");
  const next = new Image();
  next.onload = next.onerror = () => {
    // `onerror` lands here too, which is the point: a cover that cannot be fetched leaves the
    // music note rather than an empty square — and the src is dropped, or the browser paints its
    // own broken-image glyph on top of that note.
    const ok = next.naturalWidth > 0;
    if (ok) {
      el.cover.src = url;
      setGlow(url);
    } else {
      el.cover.removeAttribute("src");
      console.warn("overlay: cover failed to load", url);
    }
    el.cover.classList.remove("swapping");
    el.art.classList.toggle("has-art", ok);
    body.classList.toggle("has-art", ok);
    // The accent is refreshed on every cover, including on failure — where it has to go *back* to
    // the default rather than keep the colour of the track before.
    if (DYNAMIC) {
      if (ok) {
        try { applyAccent(extractAccent(next)); } catch (e) { applyAccent(null); }
      } else {
        applyAccent(null);
      }
    }
  };
  next.src = url;
}

/**
 * Long titles.
 *
 * A clipped title with an ellipsis is honest but loses the end of the name, and a song title is
 * exactly the thing a viewer is trying to read. The marquee only starts when the text genuinely
 * overflows, and the duration is derived from the distance so a slightly-too-long title and a very
 * long one scroll at the same speed instead of the long one racing.
 */
function fitMarquee(box, inner) {
  box.classList.remove("marquee");
  const over = inner.scrollWidth - box.clientWidth;
  if (over <= 2) return;
  box.style.setProperty("--shift", -over + "px");
  // 30px/s, plus the rests at each end, gives a duration that reads as a drift rather than a scroll.
  box.style.setProperty("--marquee-dur", (over / 30 + 5).toFixed(1) + "s");
  box.classList.add("marquee");
}

/**
 * Write the track into the DOM.
 *
 * Split out from the sequencing on purpose: this is the instant where the content changes, and the
 * whole point of the sequence is that it never happens on a frame the viewer can see. The old
 * content has already left by the time this runs, and the new content is still invisible.
 */
function paint(track) {
  el.titleText.textContent = track.title || "—";
  el.artistText.textContent = track.artists || "";
  el.album.textContent = track.album || "";
  // Who asked for it, when somebody did — the payoff of the request path, which until now wrote a
  // label into the queue that nothing on screen ever read.
  //
  // Two parts so the name can carry the accent while the words around it stay furniture, and
  // `hidden` rather than empty text: an empty span still takes the eyebrow's flex gap, so the
  // equaliser bars would sit eight pixels further from the words on every song nobody requested.
  const asked = byline(track.queuedFrom);
  el.byline.hidden = !asked;
  el.byline.textContent = "";
  if (asked) {
    el.byline.append("pedido por ");
    const who = document.createElement("b");
    who.textContent = asked;
    el.byline.append(who);
  }
  setCover(track.thumbnail || (q.get("demo") === "1" ? DEMO_ART : ""));
  // Measured after the text is in place and before the fade-in finishes, so the marquee is already
  // set up when the title becomes visible.
  fitMarquee(el.title, el.titleText);
  fitMarquee(el.artist, el.artistText);
  // The next tick writes the new track's values regardless of what the old ones were: without this
  // a song that starts at 0:00 after one that ended near 0:00 would keep the previous timestamps.
  lastSec = -1; lastPct = -1; lastRing = -1;
  // And the clock has to be re-anchored, or the bar describes the age of the tab.
  //
  // `poll` returns before the server path that anchors it when `demo=1`, so in the preview nothing
  // ever did: the position ran from the moment the page loaded rather than from the song's own
  // position. Eight seconds in it read 0:08 and looked plausible; six minutes in it read 6:43 against
  // a 4:05 track and sat pinned at 100%. Every look at the bar in the preview so far has been a look
  // at the wrong number.
  //
  // Demo only. A real overlay takes its position from the server, and a track's own `position` is a
  // snapshot from when the queue entry was built, not where playback is now.
  if (q.get("demo") === "1") {
    anchor = { pos: Number(track.position) || 0, at: performance.now() };
  }
  // And the bar must not *slide* to the new position. A track change jumps, the same as a seek
  // does, so without this the seek transition fires and the new song's bar animates up from the
  // previous song's position — which reads as the old track still finishing.
  justPainted = true;
}

let swapping = false;

/**
 * The sequence: out, change, in. Never the two at once, and never all of it in one frame.
 *
 * Both halves come from the stylesheet, so the waiting and the animating cannot disagree. The DOM
 * is only touched between them, while the outgoing content is at opacity 0 — so there is no frame
 * in which the old cover has gone and the new one has not arrived, which is the cut this replaced.
 */
async function swapTo(track) {
  if (swapping) { paint(track); return; }   // two changes inside one sequence: update, don't queue
  swapping = true;
  body.classList.add("swap-out");
  await wait(T.exit);
  paint(track);
  body.classList.remove("swap-out");
  void body.offsetWidth;                    // force a reflow so `swap-in` starts a fresh animation
  body.classList.add("swap-in");
  await wait(T.enter);
  body.classList.remove("swap-in");
  swapping = false;
}

function current() {
  // A preview track, when one has been pushed in, is the current one — it has to win over the demo
  // object or the next poll would swap straight back and the change could never be watched.
  if (previewTrack) return previewTrack;
  return state && state.now ? state.now : (q.get("demo") === "1" ? DEMO : null);
}

function render() {
  const track = current();
  body.classList.toggle("idle", !track);
  if (!track) { shownId = null; return; }
  if (track.videoId !== shownId) {
    const first = shownId === null;
    shownId = track.videoId;
    if (first) {
      paint(track);
      if (!booted) {
        booted = true;
        body.classList.add("boot");
        setTimeout(() => body.classList.remove("boot"), T.swap);
      }
    } else {
      swapTo(track);
    }
  }
}

function tick(now) {
  // Frame delta, for the one thing here that cannot be eased by CSS.
  const dt = lastFrame ? Math.min(0.05, (now - lastFrame) / 1000) : 0;
  lastFrame = now;
  const track = current();
  const paused = state ? !!state.paused : (track ? !!track.paused : false);
  // Every write below is guarded, and this loop runs for the entire stream — sixty times a second,
  // for hours. An unguarded `textContent =` with the same string still invalidates layout, so the
  // timestamps row was being re-laid-out sixty times a second to show a value that changes once.
  if (paused !== lastPaused) { body.classList.toggle("paused", paused); lastPaused = paused; }
  if (track) {
    // Seconds from the state first — it is the number. The per-track string is only a fallback.
    const dur = seconds(state && state.duration) || seconds(track.duration);
    // Interpolate locally: Rust pushes a position about four times a second, and a bar that only
    // moves on the poll looks like it is stuttering.
    // Clamped at the end in demo mode, and here rather than at the bar because the timestamps read
    // from the same number: `live` counts up from the track's position for as long as a preview page
    // is open, so four minutes into a 4:05 track the bar pinned itself at 100% and the remainder
    // read `-0:00`. On stream the position comes from the server and never passes the end; in a
    // preview it does, and it reads as a bug — which is how it was reported.
    const raw = paused ? anchor.pos : anchor.pos + (performance.now() - anchor.at) / 1000;
    const live = q.get("demo") === "1" && dur > 0 ? Math.min(raw, dur) : raw;
    const p = dur > 0 ? Math.min(1, live / dur) : 0;
    // A seek is the one time the width jumps, and animating that single jump is what keeps the bar
    // from teleporting. A track change jumps too, but it must not be animated — see `justPainted`.
    if (!justPainted && Math.abs(live - lastLive) > 1.5) {
      el.fill.style.transition = "width 260ms cubic-bezier(0.16, 1, 0.3, 1)";
      setTimeout(() => { el.fill.style.transition = "none"; }, 300);
      lastPct = -1;                       // the bar is about to jump: let the new width through
    }
    const jumped = justPainted;
    justPainted = false;
    lastLive = live;
    // 0.1% steps — finer than the eye can see, coarse enough that most frames write nothing.
    const pct = Math.round(p * 1000) / 10;
    if (pct !== lastPct) {
      el.fill.style.width = pct + "%";
      lastPct = pct;
      // The position dot needs a fill to sit on. Below this it hangs off the pill's rounded end.
      const dot = pct > 1.2;
      if (dot !== lastDot) { body.classList.toggle("no-dot", !dot); lastDot = dot; }
      // The last stretch of a track, as a class rather than a second width write — only one design
      // uses it today, and the tick is the only place that knows the position.
      const ending = p > 0.92;
      if (ending !== lastEnding) { body.classList.toggle("ending", ending); lastEnding = ending; }
    }
    // The ring's stop is a custom property inside a conic gradient, which CSS cannot interpolate —
    // so the target is approached here instead, and a seek glides the ring exactly as it glides the
    // bar. Before this the bar slid over 260ms while the ring teleported: one gesture, two
    // behaviours. `dt * 12` settles in about 260ms too, which is the point of matching them.
    if (jumped) ringShown = p;
    else ringShown += (p - ringShown) * Math.min(1, dt * 12);
    const ring = Math.round(ringShown * 500) / 500;
    if (ring !== lastRing) { el.ring.style.setProperty("--p", String(ring)); lastRing = ring; }
    const sec = Math.floor(live);
    if (sec !== lastSec) {
      el.elapsed.textContent = fmt(live);
      el.remaining.textContent = dur > 0 ? "-" + fmt(dur - live) : "-0:00";
      lastSec = sec;
    }
  }
  requestAnimationFrame(tick);
}
let lastLive = 0, lastPct = -1, lastRing = -1, lastSec = -1, lastPaused = null, lastDot = null, lastEnding = null;
let justPainted = false;   // the track changed on this frame: the bar's jump is not a seek
let ringShown = 0;         // the ring's eased position; it cannot be CSS-transitioned
let lastFrame = 0;

/* ── The record ───────────────────────────────────────────────────────────────────────────────
   A turntable winds down and winds back up; `animation-play-state: paused` stopped the disc dead,
   and a record that freezes mid-turn reads as a broken image rather than a paused one. The speed is
   eased toward the play state, so pause takes about a second to come to rest and play about a
   second to get going.

   The same mechanism gives the track change its physicality: while a swap is in flight the target
   speed drops, so the record is visibly slowing as it leaves and still gathering speed as the new
   one settles. That is why this is driven from JS rather than by a CSS animation.
   */
let spinDeg = 0, spinSpeed = 0, spinPrev = performance.now();
function spinTick(now) {
  const dt = Math.min(0.05, (now - spinPrev) / 1000);
  spinPrev = now;
  const target = body.classList.contains("paused") || swapping ? 0 : 1;
  spinSpeed += (target - spinSpeed) * Math.min(1, dt * (swapping ? 3.2 : 2.2));
  // Stop writing once it has effectively stopped, rather than setting a property 60 times a second
  // on a paused overlay for the rest of the stream.
  if (spinSpeed > 0.002 || target === 1) {
    spinDeg = (spinDeg + spinSpeed * (360 / 22) * dt) % 360;
    // Written to the disc, not to the document root. A custom property on the root invalidates the
    // style of the whole tree, and this runs at sixty frames a second for as long as the record
    // turns; only the cover ever reads it.
    el.cover.style.setProperty("--spin", spinDeg.toFixed(2) + "deg");
  }
  requestAnimationFrame(spinTick);
}

async function poll() {
  if (q.get("demo") === "1") { render(); return; }
  try {
    const r = await fetch(STATE_URL, { cache: "no-store" });
    if (!r.ok) throw new Error(String(r.status));
    state = await r.json();
    anchor = { pos: state.position || 0, at: performance.now() };
    render();
  } catch (e) {
    // LiMusic closed, or the overlay outlived it. Blank rather than stale: a frozen song title on
    // a live stream is worse than nothing.
    body.classList.add("idle");
  }
}

/* Real controls, not decoration. They work from OBS's "Interact" window; on stream nobody can
   click them, which is exactly why they must not pretend.

   In `demo` mode there is no LiMusic to post to, so the transport acts on the short list below
   instead. Without that the prev and next buttons did nothing at all in the preview — which meant
   the one animation this whole file is built around, the track change, could only be triggered from
   the switcher page and never from the thing being reviewed. Reviewing motion by reloading a page is
   reviewing the *entrance*, which is a different animation. */
const DEMO_SET = [
  { videoId: "demo-1", title: "Eye Of The Tiger", artists: "Survivor", album: "Eye Of The Tiger",
    duration: 245, position: 38, thumbnail: "https://i.ytimg.com/vi/btPJPFnesV4/maxresdefault.jpg" },
  { videoId: "demo-2", title: "Girls Just Want To Have Fun", artists: "Cyndi Lauper", album: "She's So Unusual",
    duration: 238, position: 96, thumbnail: "https://i.ytimg.com/vi/PIb6AZdTr-A/maxresdefault.jpg" },
  { videoId: "demo-3", title: "Jump", artists: "Van Halen", album: "1984",
    duration: 242, position: 12, thumbnail: "https://i.ytimg.com/vi/SwYN7mTi6HM/maxresdefault.jpg" },
  { videoId: "demo-4", title: "Take On Me", artists: "a-ha", album: "Hunting High and Low",
    duration: 225, position: 150, thumbnail: "https://i.ytimg.com/vi/djV11Xbc914/maxresdefault.jpg" },
  // A deliberately long one, because the marquee, the ellipsis and the title's fit only ever show
  // themselves on a title that does not fit. Every demo song being short is how those bugs survive.
  { videoId: "demo-5", title: "Everybody Wants To Rule The World", artists: "Tears for Fears",
    album: "Songs from the Big Chair", duration: 251, position: 200,
    thumbnail: "https://i.ytimg.com/vi/aGCdLKXNF3w/maxresdefault.jpg" }
];

/** Which of the demo tracks is showing, by index.
 *
 * Starts at 0 rather than -1, because the first entry is the track the URL describes. Beginning at
 * -1 made the first press of "next" move to index 0 — the same song — so the button looked broken
 * on the very first click, which is the click that decides whether someone trusts it.
 */
let demoAt = 0;

$("controls").addEventListener("click", async (ev) => {
  const btn = ev.target.closest("button[data-action]");
  if (!btn) return;
  const action = btn.dataset.action;
  if (q.get("demo") === "1") {
    if (action === "toggle") { body.classList.toggle("paused"); return; }
    // Wrapping, so prev from the first lands on the last rather than doing nothing — a button that
    // silently stops at the end of a list reads as broken.
    const step = action === "next" ? 1 : -1;
    demoAt = ((demoAt < 0 ? 0 : demoAt) + step + DEMO_SET.length) % DEMO_SET.length;
    const next = DEMO_SET[demoAt];
    const first = shownId === null;
    previewTrack = next;
    shownId = next.videoId;
    // The same two calls the preview channel makes, so the demo runs the real sequence rather than
    // a simplified one that could hide what is being reviewed.
    if (first) paint(next); else swapTo(next);
    return;
  }
  try {
    const r = await fetch(CONTROL_URL, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action })
    });
    if (!r.ok) throw new Error(String(r.status));
    if (action !== "toggle") setTimeout(poll, 120);
  } catch (e) { /* LiMusic is gone; the next poll fades the overlay out. */ }
});

poll();
setInterval(poll, 1000);
requestAnimationFrame(tick);
// Only the two vinyl overlays have a record, and a reduced-motion request means no ambient spin at
// all — so on those, the loop is never started rather than started and then fought with.
if (!REDUCED && id.startsWith("vinyl")) requestAnimationFrame(spinTick);
