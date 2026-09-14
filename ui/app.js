const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Same channels as SteelSeries Sonar; Game doubles as the Windows default (system sounds).
const CHANNELS = [
  { name: "Game", color: "var(--game)" },
  { name: "Chat", color: "var(--chat)" },
  { name: "Media", color: "var(--media)" },
  { name: "Aux", color: "var(--aux)" },
];
const MAX_VOLUME = 1.5;
const VIRTUAL_HARDWARE = ["VB-Audio", "SteelSeries Sonar", "Elgato Virtual Audio", "Voicemeeter", "VoiceMeeter"];

let snap = null; // get_state: config, devices, stream status
let apps = [];   // apps playing audio, grouped by exe
let view = "mixer";

// ---------- helpers ----------
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === undefined || v === null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) if (c !== null && c !== undefined && c !== false) el.append(c instanceof Node ? c : document.createTextNode(c));
  return el;
}
const $ = (s, r = document) => r.querySelector(s);
const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
const dbOf = (v) => (v <= 0.0001 ? -Infinity : 20 * Math.log10(v));
const fmtDb = (db) => (db === -Infinity || db < -99 ? "−∞" : (db > 0.05 ? "+" : db < -0.05 ? "−" : "") + Math.abs(db).toFixed(1));
const meterPct = (db) => clamp((db + 60) / 60, 0, 1) * 100;
const isVirtual = (d) => VIRTUAL_HARDWARE.some((v) => d.hardware.includes(v));

/** Coalesces rapid changes into one backend call per key every 40 ms. */
const pending = new Map();
function send(key, fn) {
  const had = pending.has(key);
  pending.set(key, fn);
  if (had) return;
  setTimeout(() => {
    const f = pending.get(key);
    pending.delete(key);
    f().catch(showError);
  }, 40);
}
const sendChannel = (i) => send(`ch${i}`, () => invoke("set_channel", { index: i, settings: snap.config.channels[i].settings }));
const sendMic = () => send("mic", () => invoke("set_mic", { settings: snap.config.mic }));
const sendMaster = () => send("master", () => invoke("set_master", { settings: snap.config.master }));

let toastTimer;
function toast(message, error = false) {
  const t = $("#toast");
  t.textContent = message;
  t.classList.toggle("error", error);
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), error ? 6000 : 2400);
}
const showError = (e) => toast(String(e), true);

const tape = (name, color) => h("span", { class: "tape", style: color ? `--c:${color}` : null }, name);
const pill = (label, value) => h("span", { class: "pill" }, label, " ", value);
function viewHead(title, text, extra) {
  return h("div", { class: "viewhead" }, h("div", {}, h("h2", {}, title), text ? h("p", {}, text) : null), extra || null);
}
function switchEl(label, checked, onChange) {
  const input = h("input", { type: "checkbox" });
  input.checked = checked;
  input.addEventListener("change", () => onChange(input.checked));
  return { el: h("label", { class: "switch" }, input, h("span", { class: "track" }), label), input };
}
function slider({ label, min, max, step = 1, value, format, onInput }) {
  const id = `ctl-${label.replace(/\W+/g, "-").toLowerCase()}-${Math.random().toString(36).slice(2, 7)}`;
  const out = h("output", { for: id }, format(value));
  const input = h("input", { type: "range", id, min, max, step, value });
  input.addEventListener("input", () => { out.textContent = format(input.valueAsNumber); onInput(input.valueAsNumber); });
  return h("div", { class: "ctl" }, h("label", { for: id }, label), out, input);
}
const fmtDbInt = (v) => `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v)} dB`;
const fmtMs = (v) => `${v} ms`;

function deviceName(list, id) {
  return list.find((d) => d.id === id)?.name;
}
function shortName(name) {
  return name ? name.replace(/\s*\(([^)]*)\)\s*$/, (m, hw) => (hw.length < 22 ? ` (${hw})` : "")) : name;
}

// ---------- EQ ----------
const band = (kind, freq, gain_db, q = 0.707) => ({ kind, freq, gain_db, q, enabled: true });
const PRESETS = {
  "Flat": [band("low_shelf", 100, 0), band("peaking", 400, 0, 1), band("peaking", 1500, 0, 1), band("peaking", 4000, 0, 1), band("high_shelf", 10000, 0)],
  "Footsteps": [band("low_shelf", 110, -4), band("peaking", 260, -2, 1), band("peaking", 2200, 4, 1.2), band("peaking", 4800, 5, 1.4), band("high_shelf", 11000, -2)],
  "Bass boost": [band("low_shelf", 90, 6), band("peaking", 420, -1.5, 1), band("peaking", 1500, 0, 1), band("peaking", 4000, 0, 1), band("high_shelf", 10000, 0)],
  "Voice clarity": [band("high_pass", 90, 0), band("peaking", 320, -3, 1), band("peaking", 1500, 0, 1), band("peaking", 3200, 4, 1), band("high_shelf", 9000, 2)],
  "Night": [band("low_shelf", 120, -5), band("peaking", 400, 0, 1), band("peaking", 1500, 0, 1), band("peaking", 4000, 0, 1), band("high_shelf", 8000, -3)],
};
const KIND_LABEL = { low_shelf: "Low shelf", high_shelf: "High shelf", peaking: "Peak", low_pass: "High cut", high_pass: "Low cut" };

function biquadDb(b, f) {
  if (!b.enabled) return 0;
  const fs = 48000, w0 = (2 * Math.PI * Math.min(b.freq, fs * 0.49)) / fs;
  const A = Math.pow(10, b.gain_db / 40), cos = Math.cos(w0), sin = Math.sin(w0);
  const alpha = sin / (2 * Math.max(b.q, 0.05)), sa = 2 * Math.sqrt(A) * alpha;
  let b0, b1, b2, a0, a1, a2;
  switch (b.kind) {
    case "low_shelf":
      [b0, b1, b2] = [A * (A + 1 - (A - 1) * cos + sa), 2 * A * (A - 1 - (A + 1) * cos), A * (A + 1 - (A - 1) * cos - sa)];
      [a0, a1, a2] = [A + 1 + (A - 1) * cos + sa, -2 * (A - 1 + (A + 1) * cos), A + 1 + (A - 1) * cos - sa];
      break;
    case "high_shelf":
      [b0, b1, b2] = [A * (A + 1 + (A - 1) * cos + sa), -2 * A * (A - 1 + (A + 1) * cos), A * (A + 1 + (A - 1) * cos - sa)];
      [a0, a1, a2] = [A + 1 - (A - 1) * cos + sa, 2 * (A - 1 - (A + 1) * cos), A + 1 - (A - 1) * cos - sa];
      break;
    case "low_pass":
      [b0, b1, b2, a0, a1, a2] = [(1 - cos) / 2, 1 - cos, (1 - cos) / 2, 1 + alpha, -2 * cos, 1 - alpha];
      break;
    case "high_pass":
      [b0, b1, b2, a0, a1, a2] = [(1 + cos) / 2, -(1 + cos), (1 + cos) / 2, 1 + alpha, -2 * cos, 1 - alpha];
      break;
    default:
      [b0, b1, b2, a0, a1, a2] = [1 + alpha * A, -2 * cos, 1 - alpha * A, 1 + alpha / A, -2 * cos, 1 - alpha / A];
  }
  const w = (2 * Math.PI * f) / fs, c1 = Math.cos(w), s1 = Math.sin(w), c2 = Math.cos(2 * w), s2 = Math.sin(2 * w);
  const num = (b0 + b1 * c1 + b2 * c2) ** 2 + (b1 * s1 + b2 * s2) ** 2;
  const den = (a0 + a1 * c1 + a2 * c2) ** 2 + (a1 * s1 + a2 * s2) ** 2;
  return 10 * Math.log10(num / den);
}

/** EQ editor: enable switch, presets, draggable response curve, band readouts. */
function eqEditor(eq, onChange) {
  const canvas = h("canvas", { class: "curve", role: "img", "aria-label": "Equalizer curve" });
  const seg = h("div", { class: "seg", role: "group", "aria-label": "Preset" });
  const bands = h("div", { class: "bands" });
  const enabled = switchEl("Equalizer", eq.enabled, (v) => { eq.enabled = v; refresh(); onChange(); });
  const RANGE = 15, hasGain = (b) => b.kind !== "low_pass" && b.kind !== "high_pass";
  const fx = (f, W) => (Math.log10(f / 20) / 3) * W, xf = (x, W) => 20 * Math.pow(10, (x / W) * 3);
  const gy = (g, H) => H / 2 - (g / RANGE) * (H / 2 - 12), yg = (y, H) => ((H / 2 - y) / (H / 2 - 12)) * RANGE;

  function renderSeg() {
    seg.replaceChildren(...[...Object.keys(PRESETS), "Custom"].map((name) => {
      const b = h("button", { type: "button", "aria-pressed": String(eq.preset === name) }, name);
      if (name === "Custom") b.disabled = eq.preset !== "Custom";
      else b.addEventListener("click", () => {
        eq.preset = name; eq.bands = structuredClone(PRESETS[name]); eq.enabled = true; enabled.input.checked = true;
        refresh(); onChange();
      });
      return b;
    }));
  }
  function renderBands() {
    bands.replaceChildren(...eq.bands.map((b) => {
      const f = b.freq >= 1000 ? `${(b.freq / 1000).toFixed(b.freq % 1000 ? 1 : 0)} kHz` : `${Math.round(b.freq)} Hz`;
      const g = hasGain(b) ? `${b.gain_db > 0 ? "+" : b.gain_db < 0 ? "−" : ""}${Math.abs(b.gain_db).toFixed(1)} dB` : "12 dB/oct";
      return h("div", { class: "band" }, h("span", {}, KIND_LABEL[b.kind] || b.kind), h("b", {}, f), h("b", {}, g));
    }));
  }
  function draw() {
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (!W) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = W * dpr; canvas.height = H * dpr;
    const ctx = canvas.getContext("2d");
    ctx.scale(dpr, dpr);
    const css = getComputedStyle(canvas);
    const line = css.getPropertyValue("--line").trim(), faint = css.getPropertyValue("--faint").trim();
    const color = eq.enabled ? css.getPropertyValue("--c").trim() : faint;
    ctx.font = `11px ${css.getPropertyValue("--font-num")}`;
    ctx.lineWidth = 1;
    for (const f of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
      const x = Math.round(fx(f, W)) + 0.5;
      ctx.strokeStyle = line; ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, H); ctx.stroke();
      ctx.fillStyle = faint; ctx.fillText(f >= 1000 ? `${f / 1000}k` : String(f), x + 4, H - 6);
    }
    for (const g of [-12, -6, 0, 6, 12]) {
      const y = Math.round(gy(g, H)) + 0.5;
      ctx.strokeStyle = line; ctx.lineWidth = g === 0 ? 1.5 : 1;
      ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke();
      ctx.fillStyle = faint; ctx.fillText(g === 0 ? "0 dB" : `${g > 0 ? "+" : "−"}${Math.abs(g)}`, 6, y - 4);
    }
    const pts = [];
    for (let x = 0; x <= W; x += 2) {
      const f = xf(x, W);
      const g = eq.bands.reduce((s, b) => s + biquadDb(b, f), 0);
      pts.push([x, clamp(gy(g, H), 2, H - 2)]);
    }
    ctx.beginPath();
    pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
    ctx.lineTo(W, gy(0, H)); ctx.lineTo(0, gy(0, H)); ctx.closePath();
    ctx.globalAlpha = 0.14; ctx.fillStyle = color; ctx.fill(); ctx.globalAlpha = 1;
    ctx.beginPath();
    pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
    ctx.strokeStyle = color; ctx.lineWidth = 2.5; ctx.stroke();
    for (const b of eq.bands) {
      ctx.beginPath(); ctx.fillStyle = b.enabled ? color : faint;
      ctx.arc(fx(b.freq, W), gy(hasGain(b) ? b.gain_db : 0, H), 5.5, 0, Math.PI * 2); ctx.fill();
    }
  }
  function refresh() { renderSeg(); renderBands(); draw(); }

  let dragging = -1;
  const nearest = (e, radius) => {
    const W = canvas.clientWidth, H = canvas.clientHeight;
    let best = -1, bestD = radius;
    eq.bands.forEach((b, i) => {
      const d = Math.hypot(fx(b.freq, W) - e.offsetX, gy(hasGain(b) ? b.gain_db : 0, H) - e.offsetY);
      if (d < bestD) { best = i; bestD = d; }
    });
    return best;
  };
  canvas.addEventListener("pointerdown", (e) => { dragging = nearest(e, 16); if (dragging >= 0) canvas.setPointerCapture(e.pointerId); });
  canvas.addEventListener("pointermove", (e) => {
    if (dragging < 0) return;
    const W = canvas.clientWidth, H = canvas.clientHeight, b = eq.bands[dragging];
    b.freq = Math.round(clamp(xf(e.offsetX, W), 20, 20000));
    if (hasGain(b)) b.gain_db = Math.round(clamp(yg(e.offsetY, H), -RANGE, RANGE) * 2) / 2;
    eq.preset = "Custom"; eq.enabled = true; enabled.input.checked = true;
    draw(); onChange();
  });
  canvas.addEventListener("pointerup", () => { if (dragging >= 0) { dragging = -1; renderSeg(); renderBands(); } });
  canvas.addEventListener("wheel", (e) => {
    const i = nearest(e, 22);
    if (i < 0) return;
    e.preventDefault();
    const b = eq.bands[i];
    b.q = Math.round(clamp(b.q * (e.deltaY < 0 ? 1.1 : 0.9), 0.1, 10) * 100) / 100;
    eq.preset = "Custom"; refresh(); onChange();
  }, { passive: false });
  new ResizeObserver(draw).observe(canvas);
  renderSeg(); renderBands();
  return h("div", { class: "eq" },
    h("div", { class: "drawer-head" }, enabled.el, h("span", { class: "spacer" }), seg),
    canvas, bands,
    h("p", { class: "note" }, "Pick a preset, or drag the points to shape it yourself. Scroll over a point to make it wider or narrower."));
}

// ---------- apps ----------
function groupApps(list) {
  const byExe = new Map();
  for (const a of list) {
    const g = byExe.get(a.exe) ?? { exe: a.exe, name: a.name, path: a.path, pids: [], active: false, peak: 0, assigned_device: null };
    g.pids.push(a.pid);
    g.active ||= a.active;
    g.peak = Math.max(g.peak, a.peak);
    g.assigned_device ||= a.assigned_device;
    byExe.set(a.exe, g);
  }
  return [...byExe.values()].sort((x, y) => x.name.localeCompare(y.name));
}
// Exe path -> PNG data URL, null when the exe has no icon, undefined while it's being extracted.
const iconCache = new Map();

/** An app's own icon, or its first letter until (or unless) Windows gives us one. */
function appIcon(app, className) {
  const el = h("span", { class: className, "data-icon": app.path });
  fillIcon(el, app);
  if (app.path && !iconCache.has(app.path)) {
    iconCache.set(app.path, undefined);
    invoke("app_icon", { path: app.path })
      .then((url) => {
        iconCache.set(app.path, url || null);
        document.querySelectorAll(`[data-icon="${CSS.escape(app.path)}"]`).forEach((e) => fillIcon(e, app));
      })
      .catch(() => iconCache.set(app.path, null));
  }
  return el;
}
function fillIcon(el, app) {
  const url = iconCache.get(app.path);
  el.classList.toggle("has-img", Boolean(url));
  el.replaceChildren(url ? h("img", { src: url, alt: "" }) : app.name[0]?.toUpperCase() || "?");
}

/** Which channel an app plays on, and whether that was chosen or is just the Windows default. */
function channelOf(app) {
  const rule = snap.config.app_rules[app.exe];
  if (rule !== undefined && rule < CHANNELS.length) return { index: rule, chosen: true };
  const i = snap.config.channels.findIndex((c) => c.sink && c.sink === app.assigned_device);
  if (i >= 0) return { index: i, chosen: true };
  return { index: 0, chosen: false };
}
async function refreshApps() {
  try { apps = groupApps(await invoke("list_apps")); } catch (e) { return; }
  updateStripIcons();
  if (view === "apps") updateLanes();
}

// ---------- mixer ----------
const meterRefs = new Map(); // key -> { lits, peaks, hold, holdAt }
const SCALE = [0, -6, -12, -24, -36, -48, -60];
let eqOpen = null;

function makeFader(label, value, onInput) {
  const el = h("div", { class: "fader", role: "slider", tabindex: "0", "aria-label": `${label} volume`, "aria-valuemin": "0", "aria-valuemax": "150" },
    h("div", { class: "unity" }), h("div", { class: "fill" }), h("div", { class: "cap" }));
  let v = value;
  const render = () => {
    el.style.setProperty("--p", (v / MAX_VOLUME).toFixed(4));
    el.setAttribute("aria-valuenow", String(Math.round(v * 100)));
    el.setAttribute("aria-valuetext", `${fmtDb(dbOf(v))} dB`);
  };
  const set = (next) => {
    v = clamp(next, 0, MAX_VOLUME);
    if (Math.abs(v - 1) < 0.03) v = 1; // snap to unity
    render(); onInput(v);
  };
  const fromY = (y) => { const r = el.getBoundingClientRect(); set(clamp((r.bottom - 9 - y) / (r.height - 18), 0, 1) * MAX_VOLUME); };
  el.addEventListener("pointerdown", (e) => { el.setPointerCapture(e.pointerId); fromY(e.clientY); });
  el.addEventListener("pointermove", (e) => { if (el.hasPointerCapture(e.pointerId)) fromY(e.clientY); });
  el.addEventListener("dblclick", () => set(1));
  el.addEventListener("keydown", (e) => {
    const step = { ArrowUp: 0.02, ArrowDown: -0.02, PageUp: 0.1, PageDown: -0.1 }[e.key];
    if (step !== undefined) { set(v + step); e.preventDefault(); }
    if (e.key === "Home") { set(1); e.preventDefault(); }
  });
  render();
  return el;
}

function strip({ key, name, color, sub, icons, volume, onVolume, muted, onMute, extra }) {
  const lits = [h("div", { class: "lit" }), h("div", { class: "lit" })];
  const peaks = [h("div", { class: "peak" }), h("div", { class: "peak" })];
  meterRefs.set(key, { lits, peaks, hold: [0, 0], holdAt: [0, 0] });
  const db = h("span", { class: "db" }), pct = h("span", { class: "pct" });
  const showVolume = (v) => { db.textContent = `${fmtDb(dbOf(v))} dB`; pct.textContent = `${Math.round(v * 100)}%`; };
  showVolume(volume);
  const el = h("div", { class: "strip" + (muted ? " muted" : ""), style: `--c:${color}`, "data-strip": key });
  const mute = h("button", { type: "button", class: "btn mute", "aria-pressed": String(muted), title: `Mute ${name}` }, "M");
  mute.addEventListener("click", () => {
    const m = mute.getAttribute("aria-pressed") !== "true";
    mute.setAttribute("aria-pressed", String(m));
    el.classList.toggle("muted", m);
    onMute(m);
  });
  el.append(
    h("div", { class: "strip-top" }, tape(name), h("small", {}, sub)),
    h("div", { class: "appicons" }, icons),
    h("div", { class: "console" },
      h("div", { class: "scale", "aria-hidden": "true" }, SCALE.map((d) => h("span", { style: `bottom:${meterPct(d)}%` }, String(d)))),
      h("div", { class: "meter", "aria-hidden": "true" },
        h("div", { class: "bar" }, lits[0], peaks[0]), h("div", { class: "bar" }, lits[1], peaks[1])),
      makeFader(name, volume, (v) => { showVolume(v); onVolume(v); })),
    h("div", { class: "readout" }, db, pct),
    h("div", { class: "strip-btns" }, mute, extra));
  return el;
}

function cableLabel(id) {
  const name = deviceName(snap.capture_devices, id) || deviceName(snap.render_devices, id);
  return name ? name.replace(/\s*\(.*\)$/, "").replace(/ Output$| Input$/, "") : "No cable";
}

function renderMixer() {
  const root = $("#view-mixer");
  meterRefs.clear();
  const strips = h("div", { class: "strips" });
  CHANNELS.forEach((c, i) => {
    const cfg = snap.config.channels[i];
    const s = cfg.settings;
    const eqButton = h("button", { type: "button", class: "btn eq", "aria-expanded": String(eqOpen === i), "data-eq": i },
      h("span", { class: "k" }, "EQ"), s.eq.enabled ? s.eq.preset : "Off");
    eqButton.addEventListener("click", () => toggleDrawer(i));
    strips.append(strip({
      key: `ch${i}`, name: c.name, color: c.color, sub: cfg.source ? cableLabel(cfg.source) : "No cable",
      icons: cfg.source ? [] : h("span", { class: "appnote" }, "Pick a cable in Settings"),
      volume: s.volume, onVolume: (v) => { s.volume = v; sendChannel(i); },
      muted: s.muted, onMute: (m) => { s.muted = m; sendChannel(i); },
      extra: eqButton,
    }));
  });
  const mic = snap.config.mic;
  const micDevice = deviceName(snap.capture_devices, snap.config.mic_device);
  strips.append(h("div", { class: "divider", "aria-hidden": "true" }), strip({
    key: "mic", name: "Mic", color: "var(--mic)", sub: "Virtual Mic",
    icons: h("span", { class: "appnote" }, shortName(micDevice) || "Automatic"),
    volume: clamp(Math.pow(10, mic.gain_db / 20), 0, MAX_VOLUME),
    onVolume: (v) => { mic.gain_db = v <= 0.001 ? -60 : Math.round(dbOf(v) * 10) / 10; sendMic(); },
    muted: mic.muted, onMute: (m) => { mic.muted = m; sendMic(); renderMicControls(); },
    extra: h("button", { type: "button", class: "btn", onclick: () => setView("mic") }, h("span", { class: "k" }, "Chain"), "Open"),
  }));
  const master = snap.config.master;
  strips.append(strip({
    key: "master", name: "Master", color: "var(--master)", sub: "Output",
    icons: h("span", { class: "appnote" }, shortName(outputName())),
    volume: master.volume, onVolume: (v) => { master.volume = v; sendMaster(); },
    muted: master.muted, onMute: (m) => { master.muted = m; sendMaster(); },
    extra: h("span", { class: "btn static", title: "Peaks above −1 dBFS are turned down so the mix never clips" }, h("span", { class: "k" }, "Limit"), h("span", { id: "limit-readout", class: "num" }, "0.0 dB")),
  }));

  root.replaceChildren(
    viewHead("Mixer", "Game is your Windows default output, so system sounds and apps you haven't assigned land there.",
      h("div", { class: "chips" }, pill("Buffer", h("b", { class: "num", id: "chip-buffer" }, "–")))),
    strips,
    h("section", { class: "drawer", id: "eq-drawer", hidden: true }));
  renderDrawer();
  updateStripIcons();
}

function updateStripIcons() {
  if (!snap) return;
  CHANNELS.forEach((c, i) => {
    const holder = $(`[data-strip="ch${i}"] .appicons`);
    if (!holder || !snap.config.channels[i].source) return;
    const list = apps.filter((a) => channelOf(a).index === i);
    const shown = list.slice(0, 5).map((a) => {
      const icon = appIcon(a, "appicon" + (a.active ? " live" : ""));
      icon.title = `${a.name}${a.active ? " (playing)" : ""}`;
      return icon;
    });
    if (list.length > 5) shown.push(h("span", { class: "appnote" }, `+${list.length - 5}`));
    holder.replaceChildren(...(shown.length ? shown : [h("span", { class: "appnote" }, "No apps yet")]));
  });
}

function toggleDrawer(i) {
  eqOpen = eqOpen === i ? null : i;
  document.querySelectorAll("[data-eq]").forEach((b) => b.setAttribute("aria-expanded", String(+b.dataset.eq === eqOpen)));
  renderDrawer();
}
function renderDrawer() {
  const drawer = $("#eq-drawer");
  if (!drawer) return;
  if (eqOpen === null) { drawer.hidden = true; drawer.replaceChildren(); return; }
  const i = eqOpen, c = CHANNELS[i], eq = snap.config.channels[i].settings.eq;
  drawer.hidden = false;
  drawer.style.setProperty("--c", c.color);
  drawer.replaceChildren(
    h("div", { class: "drawer-head" }, tape(c.name), h("h3", {}, "Equalizer"), h("span", { class: "spacer" }),
      h("button", { type: "button", class: "iconbtn", onclick: () => toggleDrawer(i) }, "Close")),
    eqEditor(eq, () => {
      sendChannel(i);
      const b = $(`[data-eq="${i}"]`);
      if (b) b.replaceChildren(h("span", { class: "k" }, "EQ"), eq.enabled ? eq.preset : "Off");
    }));
}

function setMeterPair(key, pair) {
  const m = meterRefs.get(key);
  if (!m || !m.lits[0].isConnected) return;
  const now = performance.now();
  pair.forEach((db, i) => {
    const pct = meterPct(db);
    m.lits[i].style.setProperty("--lvl", `${pct}%`);
    if (pct >= m.hold[i]) { m.hold[i] = pct; m.holdAt[i] = now; }
    else if (now - m.holdAt[i] > 900) m.hold[i] = Math.max(pct, m.hold[i] - 2.5);
    m.peaks[i].style.setProperty("--pk", `${m.hold[i]}%`);
  });
}

// ---------- apps view ----------
let dragExe = null;
let laneSignature = "";

function renderApps() {
  const root = $("#view-apps");
  const lanes = h("div", { class: "lanes" });
  CHANNELS.forEach((c, i) => {
    const list = apps.filter((a) => channelOf(a).index === i);
    const lane = h("section", { class: "lane", style: `--c:${c.color}`, "aria-label": `${c.name} channel` },
      h("div", { class: "lane-head" }, tape(c.name), h("small", {}, `${list.length} app${list.length === 1 ? "" : "s"}`)));
    for (const app of list) lane.append(appCard(app));
    if (i === 0) {
      lane.append(h("div", { class: "appcard locked", title: "Windows always plays system sounds on the default output device" },
        h("span", { class: "icon" }, "S"), h("span", { class: "title" }, "System sounds"),
        h("span", { class: "sub" }, h("em", {}, "Windows default"), h("span", { class: "badge" }, "Fixed"))));
    }
    if (!list.length && i !== 0) lane.append(h("p", { class: "empty" }, "Drag an app here."));
    lane.addEventListener("dragover", (e) => { e.preventDefault(); lane.classList.add("over"); });
    lane.addEventListener("dragleave", (e) => { if (!lane.contains(e.relatedTarget)) lane.classList.remove("over"); });
    lane.addEventListener("drop", (e) => { e.preventDefault(); lane.classList.remove("over"); if (dragExe) moveApp(dragExe, i); });
    lanes.append(lane);
  });
  const playing = apps.filter((a) => a.active).length;
  root.replaceChildren(
    viewHead("Apps", "Drag an app onto a channel. Windows remembers the choice, and apps you haven't placed follow the default: Game.",
      h("span", { class: "pill" }, h("span", { class: "dot" }), h("b", {}, `${playing} playing`))),
    apps.length ? lanes : h("p", { class: "hint" }, "No apps are playing audio right now. Start something and it appears here."));
  laneSignature = signature();
}

function appCard(app) {
  const where = channelOf(app);
  const select = h("select", { "aria-label": `Channel for ${app.name}` },
    h("option", { value: "" }, "Follow Windows default"),
    ...CHANNELS.map((c, i) => { const o = h("option", { value: String(i) }, c.name); o.selected = where.chosen && where.index === i; return o; }));
  if (!where.chosen) select.value = "";
  select.addEventListener("change", () => moveApp(app.exe, select.value === "" ? null : +select.value));
  const card = h("div", { class: "appcard", draggable: "true", "data-exe": app.exe },
    appIcon(app, "icon"),
    h("span", { class: "title", title: app.name }, app.name),
    h("span", { class: "sub" }, h("em", {}, app.exe), where.chosen ? null : h("span", { class: "badge" }, "Default"),
      h("span", { class: "activity", title: app.active ? "Playing" : "Silent" }, h("i", { "data-activity": app.exe }))),
    select);
  card.addEventListener("dragstart", (e) => { dragExe = app.exe; e.dataTransfer.setData("text/plain", app.exe); card.classList.add("dragging"); });
  card.addEventListener("dragend", () => { dragExe = null; card.classList.remove("dragging"); });
  return card;
}

const signature = () => apps.map((a) => `${a.exe}:${channelOf(a).index}:${channelOf(a).chosen}`).join("|");
function updateLanes() {
  if (dragExe || document.activeElement?.tagName === "SELECT") return;
  if (signature() !== laneSignature) { renderApps(); }
  for (const a of apps) {
    const bar = document.querySelector(`[data-activity="${CSS.escape(a.exe)}"]`);
    if (bar) bar.style.setProperty("--a", `${meterPct(dbOf(a.peak))}%`);
  }
}

async function moveApp(exe, channel) {
  const app = apps.find((a) => a.exe === exe);
  if (!app) return;
  try {
    await invoke("assign_app", { pid: app.pids[0], exe, channel });
    if (channel === null) delete snap.config.app_rules[exe];
    else snap.config.app_rules[exe] = channel;
    renderApps();
    updateStripIcons();
    toast(channel === null ? `${app.name} follows the Windows default again` : `${app.name} now plays on ${CHANNELS[channel].name}`);
  } catch (e) { showError(e); }
}

// ---------- microphone ----------
let selectedNode = "denoise";
const NODES = [
  { id: "input", name: "Mic input" },
  { id: "denoise", name: "Noise removal", toggle: "denoise" },
  { id: "gate", name: "Noise gate", toggle: "gate" },
  { id: "eq", name: "Equalizer", toggle: "eq" },
  { id: "comp", name: "Compressor", toggle: "compressor" },
  { id: "limiter", name: "Limiter" },
  { id: "output", name: "Virtual Mic" },
];
const nodeOn = (n) => !n.toggle || snap.config.mic[n.toggle].enabled;

function virtualMicName() {
  const sink = snap.render_devices.find((d) => d.id === snap.config.mic_sink);
  if (!sink) return null;
  return snap.capture_devices.find((d) => d.hardware === sink.hardware && !d.name.includes("16ch"))?.name.replace(/\s*\(.*\)$/, "");
}

function renderMic() {
  const root = $("#view-mic");
  const mic = snap.config.mic;
  const listen = switchEl("Listen to my mic", mic.monitor, (v) => { mic.monitor = v; sendMic(); });
  listen.el.style.setProperty("--c", "var(--mic)");
  const mute = h("button", { type: "button", class: "btn mute", id: "mic-mute", "aria-pressed": String(mic.muted) }, mic.muted ? "Mic muted" : "Mute mic");
  mute.addEventListener("click", () => { mic.muted = !mic.muted; sendMic(); renderMicControls(); renderMixer(); });

  const chain = h("div", { class: "chain", id: "chain", role: "group", "aria-label": "Signal chain" });
  const levels = h("section", { class: "pane" },
    h("h3", {}, "Levels"),
    hmeter("Mic input", "mic-in", true),
    hmeter("Virtual Mic output", "mic-out"),
    h("div", { class: "hscale", "aria-hidden": "true" }, ["−60", "−48", "−36", "−24", "−12", "0 dBFS"].map((t) => h("span", {}, t))),
    hmeter("Compressor gain reduction", "mic-gr", false, true),
    h("p", { class: "hint" }, "The white mark on the input meter is the gate threshold: speech above it passes, room noise below it is cut."));

  root.replaceChildren(
    viewHead("Microphone", "Your voice runs through each step from left to right before Discord, OBS or TeamSpeak hear it. Select a step to adjust it."),
    h("div", { class: "route" },
      pill("From", h("b", {}, shortName(deviceName(snap.capture_devices, snap.config.mic_device)) || "Automatic")),
      h("span", { class: "arrow", "aria-hidden": "true" }, "→"),
      pill("To", h("b", {}, virtualMicName() ? `Virtual Mic · ${virtualMicName()}` : "No Virtual Mic cable")),
      h("span", { class: "spacer" }), listen.el, mute),
    chain,
    h("div", { class: "detail" }, h("section", { class: "pane", id: "node-detail" }), levels));
  renderChain();
  renderNodeDetail();
}

function hmeter(label, id, marker = false, gr = false) {
  return h("div", { class: "hmeter" },
    h("div", { class: "lbl" }, h("span", {}, label), h("span", { class: "num", id: `${id}-db` }, "–")),
    h("div", { class: "hbar" + (gr ? " gr" : ""), id }, h("div", { class: "lit" }), marker ? h("div", { class: "marker", id: "gate-marker", title: "Gate threshold" }) : null));
}

function renderMicControls() {
  const mute = $("#mic-mute");
  if (mute) { mute.setAttribute("aria-pressed", String(snap.config.mic.muted)); mute.textContent = snap.config.mic.muted ? "Mic muted" : "Mute mic"; }
}

function renderChain() {
  const chain = $("#chain");
  if (!chain) return;
  const chev = () => { const s = document.createElementNS("http://www.w3.org/2000/svg", "svg"); s.setAttribute("class", "chev"); s.setAttribute("viewBox", "0 0 14 14"); s.setAttribute("aria-hidden", "true"); s.innerHTML = '<path d="M5 3l4 4-4 4" stroke="currentColor" fill="none" stroke-width="1.6"/>'; return s; };
  chain.replaceChildren(...NODES.flatMap((n, i) => {
    const b = h("button", { type: "button", class: "node" + (nodeOn(n) ? "" : " off"), "aria-pressed": String(n.id === selectedNode) },
      h("span", { class: "nhead" }, h("span", { class: "led" + (nodeOn(n) ? " on" : ""), "data-led": n.id }), n.name),
      h("span", { class: "state", "data-state": n.id }, "…"));
    b.addEventListener("click", () => { selectedNode = n.id; renderChain(); renderNodeDetail(); });
    return i ? [chev(), b] : [b];
  }));
}

function renderNodeDetail() {
  const pane = $("#node-detail");
  if (!pane) return;
  const mic = snap.config.mic;
  const n = NODES.find((x) => x.id === selectedNode);
  const onToggle = n.toggle ? switchEl("On", mic[n.toggle].enabled, (v) => { mic[n.toggle].enabled = v; sendMic(); renderChain(); }) : null;
  const body = [];
  if (n.id === "input") {
    body.push(h("p", { class: "hint" }, `Recording from ${deviceName(snap.capture_devices, snap.config.mic_device) || "your default microphone"}. Change the microphone in Settings; a mic you plug in later is picked up automatically.`),
      h("button", { type: "button", class: "btn", onclick: () => setView("settings") }, "Open Settings"));
  }
  if (n.id === "denoise") {
    const d = mic.denoise;
    const ll = switchEl("Low-latency model", d.low_latency, (v) => { d.low_latency = v; sendMic(); renderNodeDetail(); });
    body.push(h("p", { class: "hint" }, "DeepFilterNet3 removes keyboards, fans and background voices while you talk."),
      slider({ label: "Strength", min: 6, max: 100, value: d.strength_db, format: (v) => (v >= 100 ? "Max" : `${v} dB`), onInput: (v) => { d.strength_db = v; sendMic(); } }),
      slider({ label: "Residual cleanup", min: 0, max: 0.05, step: 0.005, value: d.post_filter, format: (v) => (v === 0 ? "Off" : v.toFixed(3)), onInput: (v) => { d.post_filter = v; sendMic(); } }),
      ll.el,
      h("p", { class: "hint" }, d.low_latency ? "Model delay 10 ms. Slightly less clean and uses more CPU." : "Model delay 30 ms. The low-latency model saves 20 ms."));
  }
  if (n.id === "gate") {
    const g = mic.gate;
    body.push(h("p", { class: "hint" }, "Cuts room noise between words. Stay quiet for a moment and set the threshold just above the input meter."),
      slider({ label: "Threshold", min: -80, max: -10, value: g.threshold_db, format: fmtDbInt, onInput: (v) => { g.threshold_db = v; placeGate(); sendMic(); } }),
      slider({ label: "Closed attenuation", min: -80, max: -3, value: g.range_db, format: fmtDbInt, onInput: (v) => { g.range_db = v; sendMic(); } }),
      slider({ label: "Attack", min: 0.5, max: 50, step: 0.5, value: g.attack_ms, format: fmtMs, onInput: (v) => { g.attack_ms = v; sendMic(); } }),
      slider({ label: "Hold", min: 0, max: 1000, step: 10, value: g.hold_ms, format: fmtMs, onInput: (v) => { g.hold_ms = v; sendMic(); } }),
      slider({ label: "Release", min: 10, max: 1000, step: 10, value: g.release_ms, format: fmtMs, onInput: (v) => { g.release_ms = v; sendMic(); } }));
  }
  if (n.id === "eq") {
    body.push(eqEditor(mic.eq, () => { sendMic(); renderChain(); }));
  }
  if (n.id === "comp") {
    const c = mic.compressor;
    body.push(h("p", { class: "hint" }, "Evens out loud and quiet speech so you stay at one level in Discord."),
      slider({ label: "Threshold", min: -60, max: 0, value: c.threshold_db, format: fmtDbInt, onInput: (v) => { c.threshold_db = v; sendMic(); } }),
      slider({ label: "Ratio", min: 1, max: 20, step: 0.1, value: c.ratio, format: (v) => `${v.toFixed(1)}:1`, onInput: (v) => { c.ratio = v; sendMic(); } }),
      slider({ label: "Knee", min: 0, max: 24, value: c.knee_db, format: fmtDbInt, onInput: (v) => { c.knee_db = v; sendMic(); } }),
      slider({ label: "Attack", min: 0.1, max: 100, step: 0.1, value: c.attack_ms, format: fmtMs, onInput: (v) => { c.attack_ms = v; sendMic(); } }),
      slider({ label: "Release", min: 10, max: 1000, step: 10, value: c.release_ms, format: fmtMs, onInput: (v) => { c.release_ms = v; sendMic(); } }),
      slider({ label: "Makeup gain", min: 0, max: 24, step: 0.5, value: c.makeup_db, format: fmtDbInt, onInput: (v) => { c.makeup_db = v; sendMic(); } }));
  }
  if (n.id === "limiter") {
    body.push(h("p", { class: "hint" }, "Catches sudden peaks, like a laugh or a desk bump, at −1 dBFS so the Virtual Mic never clips. Normal speech passes untouched and it adds no delay."));
  }
  if (n.id === "output") {
    body.push(h("p", { class: "hint" }, `In Discord, OBS or TeamSpeak choose ${virtualMicName() || "the Virtual Mic cable's Output"} as the microphone, and turn off their own noise suppression.`),
      slider({ label: "Output gain", min: -20, max: 20, step: 0.5, value: mic.gain_db, format: fmtDbInt, onInput: (v) => { mic.gain_db = v; sendMic(); } }));
  }
  pane.replaceChildren(h("h3", {}, n.name, h("span", { class: "spacer" }), onToggle ? onToggle.el : null), ...body);
}

function placeGate() {
  const marker = $("#gate-marker");
  if (marker) marker.style.setProperty("--m", `${meterPct(snap.config.mic.gate.threshold_db)}%`);
}

function updateMicMeters(m) {
  const mic = snap.config.mic;
  const set = (id, pct, text) => {
    const bar = $(`#${id} .lit`);
    if (bar) bar.style.setProperty("--lvl", `${pct}%`);
    const label = $(`#${id}-db`);
    if (label) label.textContent = text;
  };
  set("mic-in", meterPct(m.input_db), `${fmtDb(m.input_db)} dB`);
  set("mic-out", meterPct(m.output_db), `${fmtDb(m.output_db)} dB`);
  set("mic-gr", clamp(-m.gain_reduction_db / 20, 0, 1) * 100, `${fmtDb(m.gain_reduction_db)} dB`);
  placeGate();
  const states = {
    input: `${fmtDb(m.input_db)} dBFS`,
    denoise: !mic.denoise.enabled ? "off" : m.denoise_ready ? `−${Math.max(0, m.noise_reduction_db).toFixed(0)} dB noise` : "loading model",
    gate: !mic.gate.enabled ? "off" : m.gate_open ? "open" : "closed",
    eq: mic.eq.enabled ? mic.eq.preset : "off",
    comp: mic.compressor.enabled ? `${fmtDb(m.gain_reduction_db)} dB` : "off",
    limiter: m.limiter_db < -0.05 ? `${fmtDb(m.limiter_db)} dB` : "idle",
    output: mic.muted ? "muted" : `${fmtDb(m.output_db)} dBFS`,
  };
  for (const [id, text] of Object.entries(states)) {
    const el = document.querySelector(`[data-state="${id}"]`);
    if (el) el.textContent = text;
  }
  const gateLed = document.querySelector('[data-led="gate"]');
  if (gateLed && mic.gate.enabled) gateLed.className = "led " + (m.gate_open ? "on" : "act");
}

// ---------- settings ----------
function outputName() {
  return deviceName(snap.render_devices, snap.config.output_device) || "Automatic";
}

function renderSettings() {
  const root = $("#view-settings");
  const cfg = snap.config;
  const physical = (list) => list.filter((d) => !isVirtual(d));
  const cables = (list) => list.filter((d) => d.hardware.includes("VB-Audio"));
  const select = (label, list, value, emptyLabel, hint) => {
    const s = h("select", { class: "input", "aria-label": label },
      h("option", { value: "" }, emptyLabel),
      ...list.map((d) => { const o = h("option", { value: d.id }, d.name); o.selected = d.id === value; return o; }));
    return { el: h("label", { class: "field" }, h("span", {}, label), s, hint ? h("p", { class: "hint" }, hint) : null), s };
  };
  const output = select("Headphones / speakers", physical(snap.render_devices), cfg.output_device, "Automatic (your usual default)");
  const mic = select("Microphone", physical(snap.capture_devices), cfg.mic_device, "Automatic (your usual default)");
  const micSink = select("Virtual Mic cable", cables(snap.render_devices), cfg.mic_sink, "None", "Pick the cable's Input side. Apps then use its Output side as the microphone.");
  const sources = CHANNELS.map((c, i) => select(`${c.name} cable`, cables(snap.capture_devices), cfg.channels[i].source, "None", i === 0 ? "Pick the cable's Output side; apps are routed to its Input side." : null));

  const apply = h("button", { type: "button", class: "btn primary" }, "Apply and restart audio");
  apply.addEventListener("click", async () => {
    const v = (x) => x.s.value || null;
    try {
      await invoke("set_devices", { output: v(output), mic: v(mic), micSink: v(micSink), sources: sources.map(v) });
      await loadState();
      toast("Audio restarted with the new devices");
    } catch (e) { showError(e); }
  });

  const launch = switchEl("Launch at Windows sign-in", cfg.launch_at_login, (on) => invoke("set_launch_at_login", { enabled: on }).catch(showError));
  const defaults = switchEl("Set Windows default devices", cfg.set_windows_defaults, (on) =>
    invoke("set_windows_defaults", { enabled: on }).then(() => toast(on ? "Game, Chat and Virtual Mic are now the Windows defaults" : "Your previous Windows defaults are restored")).catch(showError));

  const statusList = h("div", { class: "statuslist", id: "status-list" });
  root.replaceChildren(
    viewHead("Settings"),
    h("div", { class: "settings" },
      h("section", { class: "pane" }, h("h3", {}, "Devices"), output.el, mic.el, micSink.el, ...sources.map((s) => s.el), apply),
      h("section", { class: "pane" }, h("h3", {}, "General"),
        launch.el, h("p", { class: "hint" }, "Starts AudioManager in the tray when you sign in."),
        defaults.el, h("p", { class: "hint" }, "Like Sonar: Game becomes the default playback device, Chat the communications device and the Virtual Mic the recording device. Turning this off restores the defaults you had before.")),
      h("section", { class: "pane" }, h("h3", {}, "Audio streams"), statusList,
        h("p", { class: "hint" }, "Problems are also written to %APPDATA%\\AudioManager\\audiomanager.log."))));
  renderStatus();
}

function renderStatus() {
  const entries = Object.entries(snap.status).sort(([a], [b]) => a.localeCompare(b));
  const bad = entries.filter(([, v]) => v !== "running");
  $("#pill-status").classList.toggle("warn", bad.length > 0);
  $("#pill-status-text").textContent = bad.length ? `${bad.length} stream${bad.length > 1 ? "s" : ""} need attention` : "All streams running";
  $("#pill-output").textContent = shortName(outputName());
  const list = $("#status-list");
  if (list) list.replaceChildren(...entries.flatMap(([k, v]) => [h("span", {}, k), h("span", { class: v === "running" ? "ok" : "bad" }, v === "running" ? "Running" : v)]));
}

// ---------- navigation & polling ----------
function setView(next) {
  view = next;
  for (const v of ["mixer", "apps", "mic", "settings"]) $(`#view-${v}`).hidden = v !== view;
  document.querySelectorAll(".rail button").forEach((b) => (b.dataset.view === view ? b.setAttribute("aria-current", "page") : b.removeAttribute("aria-current")));
  if (view === "apps") renderApps();
  $(".content").scrollTop = 0;
}
document.querySelectorAll(".rail button").forEach((b) => b.addEventListener("click", () => setView(b.dataset.view)));

async function loadState() {
  snap = await invoke("get_state");
  renderMixer();
  renderMic();
  renderSettings();
  if (view === "apps") renderApps();
  renderStatus();
}

async function pollMeters() {
  if (!document.hidden && snap) {
    try {
      const m = await invoke("get_meters");
      m.channels.forEach((pair, i) => setMeterPair(`ch${i}`, pair));
      setMeterPair("mic", [m.mic.output_db, m.mic.output_db]);
      setMeterPair("master", m.master);
      const buffer = $("#chip-buffer");
      if (buffer) buffer.textContent = m.buffer_ms ? `${Math.round(m.buffer_ms)} ms` : "–";
      const limit = $("#limit-readout");
      if (limit) limit.textContent = `${fmtDb(m.master_reduction_db)} dB`;
      if (view === "mic") updateMicMeters(m.mic);
    } catch { /* engine restarting */ }
  }
  setTimeout(pollMeters, 50);
}

async function pollApps() {
  if (!document.hidden && snap) await refreshApps();
  setTimeout(pollApps, view === "apps" ? 900 : 3000);
}

setInterval(async () => {
  if (document.hidden || !snap) return;
  try {
    const next = await invoke("get_state");
    snap.status = next.status;
    renderStatus();
  } catch { /* ignore */ }
}, 3000);

// Settings changed in the tray flyout while this window was in the background.
window.addEventListener("focus", () => { if (snap && !document.querySelector(".fader:active")) loadState().catch(showError); });
listen("show-view", (e) => setView(e.payload));

(async () => {
  try {
    await loadState();
    const wanted = await invoke("take_pending_view");
    if (wanted) setView(wanted);
    await refreshApps();
  } catch (e) { showError(e); }
  pollMeters();
  pollApps();
})();
