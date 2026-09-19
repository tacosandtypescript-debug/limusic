import io, json, os, sqlite3

tools = r"C:\Users\KTZ\Documents\.tools"
db = os.path.join(os.environ["APPDATA"], "com.limusic.desktop", "limusic.sqlite")
con = sqlite3.connect(db)
seen, songs = set(), []
for vid, js in con.execute("select video_id, song_json from plays order by played_at desc"):
    if vid in seen:
        continue
    seen.add(vid)
    try:
        d = json.loads(js)
    except Exception:
        continue
    dur = d.get("duration")
    if isinstance(dur, str) and ":" in dur:
        parts = [int(x) for x in dur.split(":") if x.isdigit()]
        dur = (parts[0] * 60 + (parts[1] if len(parts) > 1 else 0)) if parts else 225
    elif not isinstance(dur, (int, float)) or dur <= 0:
        dur = 225
    songs.append({
        "videoId": vid,
        "title": d.get("title") or "?",
        "artists": d.get("artists") or "",
        "album": d.get("album") or "",
        "duration": int(dur),
        "cover": "https://i.ytimg.com/vi/%s/maxresdefault.jpg" % vid,
    })
for i, s in enumerate(songs):
    s["position"] = [38, 96, 12, 150, 64, 25][i % 6]

HTML = """<!doctype html>
<html lang="es" translate="no"><head><meta charset="utf-8">
<meta name="google" content="notranslate">
<title>overlay 1 - selector</title>
<style>
  :root { color-scheme: dark; }
  body { margin:0; padding:18px; font:14px "Segoe UI",system-ui,sans-serif; background:#131217; color:#e9e7ee; }
  h1 { font-size:15px; margin:0 0 4px; font-weight:600; }
  p.note { margin:0 0 14px; font-size:12px; color:#9b97a8; max-width:800px; line-height:1.5; }
  code { background:#241f2b; padding:1px 5px; border-radius:4px; }
  h2 { font-size:11px; margin:0 0 6px; font-weight:600; letter-spacing:.08em; text-transform:uppercase; color:#8d889c; }
  .songs { display:flex; flex-wrap:wrap; gap:8px; margin-bottom:18px; max-width:940px; }
  button { display:flex; align-items:center; gap:9px; padding:6px 12px 6px 6px; cursor:pointer;
           background:#1d1b23; color:#e9e7ee; border:1px solid #33303c; border-radius:9px;
           font:inherit; font-size:12px; text-align:left; transition:background .15s,border-color .15s; }
  button:hover { background:#26232e; }
  button[aria-pressed="true"] { border-color:#c8495f; background:#2a1a20; }
  button img { width:30px; height:30px; border-radius:5px; display:block; object-fit:cover; background:#2a2733; }
  button .t { font-weight:600; }
  button .a { color:#9b97a8; font-size:11px; display:block; }
  .row { display:flex; gap:10px; align-items:flex-start; margin-bottom:10px; flex-wrap:wrap; }
  .row .lbl { font-size:12px; color:#9b97a8; min-width:132px; padding-top:6px; }
  /* The chips need a flex row of their own: a `button` styled `display:flex` is block-level, so
     inside a bare `span` they stack one per line instead of flowing. */
  .row .chips { display:flex; gap:8px; flex-wrap:wrap; }
  .row button { padding:5px 11px; border-radius:20px; }
  .stage { border-radius:10px; display:inline-block; overflow:hidden; margin-top:6px; }
  iframe { display:block; border:0; background:transparent; }
</style></head>
<body>
<h1>Overlay 1 &middot; Sleeve horizontal &middot; 620&times;200</h1>
<p class="note">
  Pulsa una cancion para lanzar el <strong>cambio</strong> sin recargar el iframe: se ve la secuencia
  real (salida, cambio, entrada). Cambiar el <strong>look</strong> si recarga, porque son parametros
  de URL. Si edito <code>page.html</code> esta pagina se recarga sola.
</p>

<h2>Canciones</h2>
<div class="songs" id="songs"></div>

<h2>Look del overlay</h2>
<p class="note" style="margin:-4px 0 10px">
  El <strong>fondo</strong> es una sola cosa: la superficie detras del contenido. El
  <strong>preset</strong> es un conjunto que fija varias a la vez &mdash; y el fondo es una de
  ellas. Por eso al elegir un preset la fila de fondo se mueve sola: te esta ensenando cual eligio.
  Si despues pulsas un fondo a mano, el tuyo manda sobre el del preset.
</p>
<div class="row"><span class="lbl">Fondo</span><span class="chips" id="cards"></span></div>
<div class="row"><span class="lbl">Preset</span><span class="chips" id="presets"></span></div>

<h2>Solo para revisar</h2>
<p class="note" style="margin:-4px 0 10px">
  El fondo del stage <strong>no forma parte del overlay</strong>: es el fondo de esta pagina, puesto
  ahi para poder juzgar el contraste y los bordes. Un browser source de OBS nunca lo lleva, y por eso
  esta en su propio bloque y no junto a las opciones de arriba.
</p>
<div class="row"><span class="lbl">Fondo del stage</span><span class="chips" id="bgs"></span></div>

<div class="stage" id="stage"><iframe id="ov" width="620" height="200" src="about:blank"></iframe></div>
<script>
const SONGS = __SONGS__;
const CARDS = ["auto", "transparent", "solid", "translucent", "glass"];
const PRESETS = ["default", "minimal", "card", "glass", "dynamic", "vinyl"];
const LABELS = { auto: "diseno", default: "ninguno" };
/* Which surface each preset chooses. Mirrors PRESETS in page.html, and it is here so the Fondo row
   can follow the preset instead of sitting there showing a stale answer — the two controls looked
   like duplicates precisely because nothing connected them. */
const PRESET_CARD = {
  default: null, minimal: "transparent", card: "solid",
  glass: "glass", dynamic: "translucent", vinyl: "auto"
};

const ov = document.getElementById("ov");
const stage = document.getElementById("stage");
let card = "auto", preset = "default", current = null;

function look() {
  return (card === "auto" ? "" : "&card=" + card) +
         (preset === "default" ? "" : "&preset=" + preset);
}
function srcFor(s) {
  const p = new URLSearchParams({
    design: "sleeve-wide", pos: "mc", demo: "1",
    dt: s.title, da: s.artists, dal: s.album,
    ddur: String(s.duration), dpos: String(s.position), dc: s.cover
  });
  return "/?" + p.toString() + look();
}

/* Songs change by message, so the change animation plays; look changes reload, because they are
   URL parameters read once at load. Different kinds of change, different mechanisms. */
function select(i, first) {
  const s = SONGS[i];
  for (const b of document.querySelectorAll("#songs button")) {
    b.setAttribute("aria-pressed", String(+b.dataset.i === i));
  }
  if (first || current === null) {
    ov.src = srcFor(s);
  } else if (s.videoId !== current.videoId) {
    ov.contentWindow.postMessage({ limusicPreview: {
      videoId: s.videoId, title: s.title, artists: s.artists, album: s.album,
      thumbnail: s.cover, duration: s.duration, position: s.position, paused: false
    } }, "*");
  }
  current = s;
}

/* Both rows repaint together: choosing a preset can change what the Fondo row ought to be showing,
   and leaving it stale is what made the two look like duplicates. */
function paintChips() {
  for (const o of document.querySelectorAll("#cards button")) {
    o.setAttribute("aria-pressed", String(o.dataset.v === card));
  }
  for (const o of document.querySelectorAll("#presets button")) {
    o.setAttribute("aria-pressed", String(o.dataset.v === preset));
  }
}

function buildChips(host, values, onPick) {
  const box = document.getElementById(host);
  values.forEach((v) => {
    const b = document.createElement("button");
    b.textContent = LABELS[v] || v;
    b.dataset.v = v;
    b.onclick = () => { onPick(v); paintChips(); if (current) ov.src = srcFor(current); };
    box.appendChild(b);
  });
}

const wrap = document.getElementById("songs");
SONGS.forEach((s, i) => {
  const b = document.createElement("button");
  b.dataset.i = i;
  b.innerHTML = '<img src="' + s.cover + '" alt="">' +
    '<span><span class="t">' + s.title + '</span><span class="a">' + (s.artists || "-") + '</span></span>';
  b.onclick = () => select(i, false);
  wrap.appendChild(b);
});

buildChips("cards", CARDS, (v) => { card = v; });
buildChips("presets", PRESETS, (v) => {
  preset = v;
  const chosen = PRESET_CARD[v];
  if (chosen) card = chosen;
});
paintChips();

const BGS = {
  dark: "#17161b",
  checker: "repeating-conic-gradient(#2a2a30 0% 25%, #4a4a52 0% 50%) 50% / 36px 36px",
  scene: "radial-gradient(120% 90% at 74% 22%, #55617a, #202836 55%, #0b0d14)",
  light: "linear-gradient(135deg,#fbfbf8,#e9e7e1 55%,#d2cfc7)",
  none: "transparent"
};
const bgBox = document.getElementById("bgs");
Object.keys(BGS).forEach((k) => {
  const b = document.createElement("button");
  b.textContent = k;
  b.dataset.k = k;
  b.onclick = () => {
    stage.style.background = BGS[k];
    for (const o of bgBox.querySelectorAll("button")) o.setAttribute("aria-pressed", String(o.dataset.k === k));
  };
  bgBox.appendChild(b);
});
bgBox.querySelector('button[data-k="dark"]').setAttribute("aria-pressed", "true");
stage.style.background = BGS.dark;
select(0, true);
</script>
</body></html>
"""

out = os.path.join(tools, "overlay-switch.html")
io.open(out, "w", encoding="utf-8").write(HTML.replace("__SONGS__", json.dumps(songs, ensure_ascii=False)))
print("  switcher regenerado: %s (%.1f KB)" % (out, os.path.getsize(out) / 1024))
