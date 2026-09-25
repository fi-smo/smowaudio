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
// "Speakers (5- soundcore Select 4 Go )" -> "soundcore Select 4 Go": Windows puts the endpoint type
// first and the actual device in parentheses, with a counter when names repeat.
function shortName(name) {
  const m = name && /^(.*?)\s*\((.*)\)\s*$/.exec(name);
  return m ? m[2].replace(/^\d+-\s*/, "").trim() || m[1] : name;
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
const stripRefs = new Map(); // key -> update(volume, muted): shows changes made elsewhere
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
  el.addEventListener("pointerdown", (e) => { el.setPointerCapture(e.pointerId); el.dataset.dragging = ""; fromY(e.clientY); });
  el.addEventListener("pointermove", (e) => { if (el.hasPointerCapture(e.pointerId)) fromY(e.clientY); });
  el.addEventListener("lostpointercapture", () => delete el.dataset.dragging);
  el.addEventListener("dblclick", () => set(1));
  /** Shows a value changed elsewhere (flyout, shortcut) unless the user is dragging this fader. */
  el.setValue = (next) => { if (!("dragging" in el.dataset)) { v = clamp(next, 0, MAX_VOLUME); render(); } };
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
  const showMuted = (m) => { mute.setAttribute("aria-pressed", String(m)); el.classList.toggle("muted", m); };
  mute.addEventListener("click", () => {
    const m = mute.getAttribute("aria-pressed") !== "true";
    showMuted(m);
    onMute(m);
  });
  const fader = makeFader(name, volume, (v) => { showVolume(v); onVolume(v); });
  stripRefs.set(key, (v, m) => { fader.setValue(v); if (!("dragging" in fader.dataset)) showVolume(v); showMuted(m); });
  el.append(
    h("div", { class: "strip-top" }, tape(name), h("small", {}, sub)),
    h("div", { class: "appicons" }, icons),
    h("div", { class: "console" },
      h("div", { class: "scale", "aria-hidden": "true" }, SCALE.map((d) => h("span", { style: `bottom:${meterPct(d)}%` }, String(d)))),
      h("div", { class: "meter", "aria-hidden": "true" },
        h("div", { class: "bar" }, lits[0], peaks[0]), h("div", { class: "bar" }, lits[1], peaks[1])),
      fader),
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
  stripRefs.clear();
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
    volume: micVolume(mic),
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

const micVolume = (mic) => clamp(Math.pow(10, mic.gain_db / 20), 0, MAX_VOLUME);

/** Brings the mixer strips in line with snap.config without rebuilding them. */
function updateStrips() {
  snap.config.channels.forEach((c, i) => {
    stripRefs.get(`ch${i}`)?.(c.settings.volume, c.settings.muted);
    const b = $(`[data-eq="${i}"]`);
    if (b) b.replaceChildren(h("span", { class: "k" }, "EQ"), c.settings.eq.enabled ? c.settings.eq.preset : "Off");
  });
  stripRefs.get("mic")?.(micVolume(snap.config.mic), snap.config.mic.muted);
  stripRefs.get("master")?.(snap.config.master.volume, snap.config.master.muted);
  const outNote = $('[data-strip="master"] .appicons .appnote');
  if (outNote) outNote.textContent = shortName(outputName());
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
    h("span", { class: "activity", "data-activity": app.exe, "aria-hidden": "true" }, appSegments(false), appSegments(true)),
    h("span", { class: "level num silent", "data-level-db": app.exe }, "−∞"),
    h("span", { class: "sub" }, h("em", {}, app.exe), where.chosen ? null : h("span", { class: "badge" }, "Default")),
    select);
  card.addEventListener("dragstart", (e) => { dragExe = app.exe; e.dataTransfer.setData("text/plain", app.exe); card.classList.add("dragging"); });
  card.addEventListener("dragend", () => { dragExe = null; card.classList.remove("dragging"); });
  return card;
}

const signature = () => apps.map((a) => `${a.exe}:${channelOf(a).index}:${channelOf(a).chosen}`).join("|");
function updateLanes() {
  if (dragExe || document.activeElement?.tagName === "SELECT") return;
  if (signature() !== laneSignature) { renderApps(); }
}

// Real segment elements (not a repeating gradient) so they stay even at any display scaling. The lit
// copy is cut off at the level; the peak lights one segment of the dark copy underneath.
const APP_SEGMENTS = 12;
function appSegments(lit) {
  return h("span", { class: lit ? "vsegs lit" : "vsegs" }, ...Array.from({ length: APP_SEGMENTS }, (_, k) =>
    h("i", lit && k >= APP_SEGMENTS * 0.85 ? { class: "hot" } : lit && k >= APP_SEGMENTS * 0.7 ? { class: "warn" } : {})));
}

// Apps view meters: polled as often as the channel meters, with the same ballistics (instant rise,
// smooth fall, a peak mark that holds), plus a dB readout.
const appMeter = new Map(); // exe -> { db, hold, holdAt }
async function pollAppLevels() {
  if (view === "apps" && !document.hidden && apps.length) {
    try {
      const levels = new Map(await invoke("app_levels"));
      const now = performance.now();
      for (const a of apps) {
        const peak = Math.max(0, ...a.pids.map((pid) => levels.get(pid) ?? 0));
        const target = Math.max(-90, dbOf(peak));
        const m = appMeter.get(a.exe) ?? { db: -90, hold: -90, holdAt: 0, at: now };
        // Fall at about 26 dB/s, like a PPM; rise instantly.
        m.db = Math.max(target, m.db - 0.026 * (now - m.at));
        m.at = now;
        if (target >= m.hold) { m.hold = target; m.holdAt = now; } else if (now - m.holdAt > 900) m.hold = Math.max(m.db, m.hold - 0.03 * 50);
        appMeter.set(a.exe, m);
        const bar = document.querySelector(`[data-activity="${CSS.escape(a.exe)}"]`);
        if (!bar) continue;
        const lit = Math.round((meterPct(m.db) / 100) * APP_SEGMENTS);
        bar.style.setProperty("--lvl", `${(lit / APP_SEGMENTS) * 100}%`);
        // Peak mark: the highest segment reached recently, only while there's sound.
        const peakSeg = Math.round((meterPct(m.hold) / 100) * APP_SEGMENTS) - 1;
        bar.firstChild.querySelectorAll("i").forEach((seg, k) => seg.classList.toggle("pk", k === peakSeg));
        const silent = m.hold <= -60;
        const readout = document.querySelector(`[data-level-db="${CSS.escape(a.exe)}"]`);
        if (readout) { readout.textContent = silent ? "−∞" : `${fmtDb(m.hold)} dB`; readout.classList.toggle("silent", silent); }
      }
    } catch { /* devices changing */ }
  }
  setTimeout(pollAppLevels, 50);
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
  mute.addEventListener("click", () => { mic.muted = !mic.muted; sendMic(); renderMicControls(); updateStrips(); });

  const chain = h("div", { class: "chain", id: "chain", role: "group", "aria-label": "Signal chain" });
  const levels = h("section", { class: "pane" },
    h("h3", {}, "Levels"),
    hmeter("Mic input", "mic-in", true),
    hmeter("Virtual Mic output", "mic-out"),
    h("div", { class: "hscale", "aria-hidden": "true" }, ["−60", "−48", "−36", "−24", "−12", "0 dBFS"].map((t) => h("span", {}, t))),
    hmeter("Compressor gain reduction", "mic-gr", false, true),
    h("p", { class: "hint" }, "The white mark on the input meter is the gate threshold: speech above it passes, room noise below it is cut."),
    micTestPanel());

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

// ---------- mic test ----------
function micTestPanel() {
  const button = (id, label, action) => {
    const b = h("button", { type: "button", class: "btn", id }, label);
    b.addEventListener("click", () => invoke("mic_test", { action }).catch(showError));
    return b;
  };
  return h("div", { class: "mictest", id: "mic-test" },
    h("h4", {}, "Test your mic"),
    h("p", { class: "hint", id: "mic-test-hint" }, "Records 5 seconds, then plays it back on your headphones with every filter applied. Play the original to hear the difference."),
    h("div", { class: "mictest-row" },
      button("mic-test-record", "Record 5 s", "record"),
      button("mic-test-play", "Play filtered", "play"),
      button("mic-test-original", "Play original", "play_original"),
      button("mic-test-stop", "Stop", "stop")),
    h("div", { class: "mictest-bar", "aria-hidden": "true" }, h("i", { id: "mic-test-progress" })));
}

function updateMicTest(t) {
  const panel = $("#mic-test");
  if (!panel || !t) return;
  const muted = snap.config.mic.muted;
  const busy = t.phase !== "idle";
  $("#mic-test-record").disabled = busy || muted;
  $("#mic-test-record").textContent = t.phase === "recording" ? `Recording… ${Math.ceil(5 * (1 - t.progress))} s` : "Record 5 s";
  $("#mic-test-play").disabled = busy || !t.recorded;
  $("#mic-test-original").disabled = busy || !t.recorded;
  $("#mic-test-stop").disabled = !busy;
  $("#mic-test-progress").style.width = `${busy ? t.progress * 100 : 0}%`;
  panel.dataset.phase = t.phase;
  const hint = $("#mic-test-hint");
  hint.textContent = muted ? "Your mic is muted. Unmute it to record a test."
    : t.phase === "recording" ? "Speak normally, the way you would in Discord."
    : t.phase === "playing" ? (t.original ? "Playing your mic without any filters." : "Playing with every filter applied, as others hear you.")
    : "Records 5 seconds, then plays it back on your headphones with every filter applied. Play the original to hear the difference.";
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

// Device choices picked in Settings but not applied yet; changes from elsewhere mustn't wipe them.
let settingsDirty = false;

// Remembered between sessions; localStorage can be unavailable, which just means no memory.
const store = {
  get(key, fallback) { try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; } },
  set(key, value) { try { localStorage.setItem(key, value); } catch { /* not remembered */ } },
};

const SETTINGS_TABS = [["devices", "Devices"], ["general", "General"], ["shortcuts", "Shortcuts"], ["updates", "Updates"]];
let settingsTab = store.get("settingsTab", "devices");
if (!SETTINGS_TABS.some(([id]) => id === settingsTab)) settingsTab = "devices";

function setSettingsTab(id) {
  settingsTab = id;
  store.set("settingsTab", id);
  if (recording) stopRecording();
  renderSettings();
}

function renderSettings() {
  settingsDirty = false;
  const root = $("#view-settings");
  const bad = failingStreams().length;
  const tabs = h("div", { class: "tabs", role: "tablist", "aria-label": "Settings sections" }, ...SETTINGS_TABS.map(([id, label]) => {
    const b = h("button", { type: "button", role: "tab", id: `tab-${id}`, "aria-selected": String(id === settingsTab), "aria-controls": "settings-panel" }, label,
      id === "devices" ? h("span", { class: "tabdot", id: "tabdot-devices", hidden: !bad, title: "A stream has stopped" }) : null);
    b.addEventListener("click", () => setSettingsTab(id));
    return b;
  }));
  const content = { devices: devicesTab, general: generalTab, shortcuts: shortcutsTab, updates: updatesTab }[settingsTab]();
  root.replaceChildren(
    h("div", { class: "viewhead" }, h("h2", {}, "Settings")),
    tabs,
    h("div", { class: "settings-body", id: "settings-panel", role: "tabpanel", "aria-labelledby": `tab-${settingsTab}` }, ...content));
  if (settingsTab === "shortcuts") renderShortcuts();
  renderStatus();
  refreshUpdates();
}

// ---------- settings: building blocks ----------
/** A card: optional header (title + subtitle), then setting rows. */
function card(head, ...children) {
  return h("section", { class: "card" },
    head ? h("div", { class: "card-head" }, h("h3", {}, head.title), head.sub ? h("p", {}, head.sub) : null) : null,
    ...children);
}

/** The one row layout every setting uses: label and help on the left, the control on the right. */
function settingRow({ label, help, control, compact = false, fit = false, key }) {
  const helpEl = h("p", { class: "srow-help" }, help ?? "");
  helpEl.hidden = !help;
  const row = h("div", { class: "srow" + (compact ? " compact" : "") + (fit ? " fit" : ""), "data-row": key },
    h("div", { class: "srow-text" }, h("div", { class: "srow-label" }, label), helpEl),
    h("div", { class: "srow-control" }, control));
  return { row, helpEl };
}

const chevron = (up) => h("span", { class: "chev" + (up ? " up" : ""), "aria-hidden": "true" });

// ---------- settings: stream health ----------
// The order streams are listed in; unknown ones go last.
const STREAM_ORDER = ["App routing", "Output", "Aux input", "Game input", "Chat input", "Media input", "Microphone", "Virtual mic", "Virtual mic listeners"];
let healthExpanded = null; // null: open exactly when something is failing

function streams() {
  const rank = (name) => { const i = STREAM_ORDER.indexOf(name); return i < 0 ? STREAM_ORDER.length : i; };
  return Object.entries(snap?.status ?? {})
    .map(([name, state]) => ({ name, ok: state === "running", reason: state === "running" ? null : state }))
    .sort((a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name));
}
const failingStreams = () => streams().filter((s) => !s.ok);
const deviceMissing = (reason) => /no physical audio device/i.test(reason ?? "");

function reasonText(stream) {
  const reason = stream.reason.replace(/[.\s]+$/, "");
  const sentence = reason.charAt(0).toLowerCase() + reason.slice(1);
  return deviceMissing(reason) ? `${sentence}. Plug one in or pick another below.` : `${sentence}.`;
}

function healthCard() {
  const el = h("section", { class: "card health", "data-health": "" });
  fillHealth(el);
  return el;
}

function fillHealth(el) {
  const list = streams();
  const bad = list.filter((s) => !s.ok);
  const expanded = healthExpanded ?? bad.length > 0;
  el.classList.toggle("problem", bad.length > 0);
  const toggle = h("button", { type: "button", class: "linkish", "aria-expanded": String(expanded) },
    bad.length ? (expanded ? "Hide streams" : "Show streams") : (expanded ? "Hide details" : "Show details"), chevron(expanded));
  toggle.addEventListener("click", () => {
    healthExpanded = !expanded;
    document.querySelectorAll("[data-health]").forEach(fillHealth);
  });
  let summary;
  if (!bad.length) {
    summary = h("span", { class: "health-text" }, h("b", {}, "Audio streams"),
      h("span", { class: "muted" }, list.length ? ` — all ${list.length} running` : " — starting"));
  } else {
    const title = bad.length === 1 ? `${bad[0].name} stream stopped` : `${bad.length} streams stopped`;
    summary = h("span", { class: "health-text" }, h("b", {}, title), h("span", { class: "muted" }, ` — ${reasonText(bad[0])}`));
  }
  const retry = bad.length ? h("button", { type: "button", class: "btn" }, "Retry") : null;
  retry?.addEventListener("click", async () => {
    retry.disabled = true;
    retry.textContent = "Retrying…";
    try {
      await invoke("restart_audio");
      // Streams report back within a moment of starting.
      setTimeout(() => refreshStatus().catch(() => {}), 1500);
    } catch (e) { showError(e); retry.disabled = false; retry.textContent = "Retry"; }
  });
  const head = h("div", { class: "health-head" }, h("span", { class: "dot" + (bad.length ? " warn" : "") }), summary, retry, toggle);
  el.replaceChildren(head, ...(expanded ? [h("div", { class: "health-grid" }, ...list.map((s) =>
    h("span", { class: s.ok ? "ok" : "bad", title: s.ok ? "Running" : s.reason }, h("i"), s.name)))] : []));
}

// ---------- settings: devices ----------
// Every device and cable select applies as soon as it changes; this tracks each row's progress.
const rowStatus = {}; // row key -> "applying" | "applied" | { error }
const deviceRows = {}; // row key -> { row, select, helpEl, help() }
const DEVICE_FIELDS = {
  output: { get: (c) => c.output_device, set: (c, v) => { c.output_device = v; } },
  mic: { get: (c) => c.mic_device, set: (c, v) => { c.mic_device = v; } },
  mic_sink: { get: (c) => c.mic_sink, set: (c, v) => { c.mic_sink = v; } },
  ...Object.fromEntries(CHANNELS.map((_, i) => [`source${i}`, {
    get: (c) => c.channels[i].source, set: (c, v) => { c.channels[i].source = v; },
  }])),
};

/** "CABLE-C Input (VB-Audio Cable C)" -> "CABLE-C Output": the side apps use. */
function pairedSide(list, id) {
  const name = deviceName(list, id);
  if (!name) return null;
  const base = name.replace(/\s*\(.*\)\s*$/, "");
  return /Input$/.test(base) ? base.replace(/Input$/, "Output") : base.replace(/Output$/, "Input");
}

function deviceRow({ key, label, list, empty, help, compact }) {
  const value = DEVICE_FIELDS[key].get(snap.config) ?? "";
  const select = h("select", { class: "input", "aria-label": typeof label === "string" ? label : key },
    h("option", { value: "" }, empty),
    ...list.map((d) => { const o = h("option", { value: d.id }, d.name); o.selected = d.id === value; return o; }));
  // A device that's configured but unplugged still needs to show as selected.
  if (value && !list.some((d) => d.id === value)) select.prepend(h("option", { value, selected: true }, "Unavailable device"));
  select.value = value;
  const { row, helpEl } = settingRow({ label, control: select, key, compact });
  deviceRows[key] = { row, select, helpEl, help };
  select.addEventListener("change", () => pickDevice(key, select.value || null));
  showDeviceRow(key);
  return row;
}

function showDeviceRow(key) {
  const r = deviceRows[key];
  if (!r) return;
  const st = rowStatus[key];
  const selected = r.select.selectedOptions[0];
  r.select.title = selected ? selected.textContent : "";
  r.select.disabled = st === "applying";
  r.row.classList.toggle("applied", st === "applied");
  const base = r.help();
  r.select.classList.toggle("invalid", Boolean(base?.bad));
  let content = base?.text ?? base, cls = base?.bad ? "bad" : "";
  if (st === "applying") { content = "Applying…"; cls = "muted"; }
  else if (st === "applied") { content = "✓ Applied · audio restarted"; cls = "ok"; }
  else if (st?.error) { content = st.error; cls = "bad"; }
  r.helpEl.className = "srow-help" + (cls ? ` ${cls}` : "");
  r.helpEl.replaceChildren(...(content === undefined || content === null ? [] : [].concat(content)));
  r.helpEl.hidden = content === undefined || content === null || content === "";
}

let applyTimer = null;
let applyQueue = Promise.resolve();
const pendingKeys = new Set();
let pendingPrevious = {};

function pickDevice(key, value) {
  if (!(key in pendingPrevious)) pendingPrevious[key] = DEVICE_FIELDS[key].get(snap.config) ?? null;
  DEVICE_FIELDS[key].set(snap.config, value);
  pendingKeys.add(key);
  rowStatus[key] = "applying";
  showDeviceRow(key);
  // Several changes in quick succession restart audio once.
  clearTimeout(applyTimer);
  applyTimer = setTimeout(() => { applyQueue = applyQueue.then(applyDevices); }, 400);
}

async function applyDevices() {
  const keys = [...pendingKeys];
  const previous = pendingPrevious;
  pendingKeys.clear();
  pendingPrevious = {};
  if (!keys.length) return;
  const c = snap.config;
  try {
    await invoke("set_devices", { output: c.output_device, mic: c.mic_device, micSink: c.mic_sink, sources: c.channels.map((x) => x.source) });
    keys.forEach((k) => { rowStatus[k] = "applied"; });
    await loadState();
    setTimeout(() => keys.forEach((k) => { if (rowStatus[k] === "applied") { delete rowStatus[k]; showDeviceRow(k); } }), 2500);
  } catch (e) {
    for (const k of keys) {
      DEVICE_FIELDS[k].set(snap.config, previous[k]);
      rowStatus[k] = { error: `Couldn't apply: ${String(e)}` };
    }
    if (view === "settings" && settingsTab === "devices") renderSettings();
  }
}

function devicesTab() {
  const physical = (list) => list.filter((d) => !isVirtual(d));
  const cables = (list) => list.filter((d) => d.hardware.includes("VB-Audio"));
  const code = (text) => h("code", {}, text);
  const channelHelp = (i) => () => {
    const side = pairedSide(snap.capture_devices, snap.config.channels[i].source);
    return side ? ["Apps play into ", code(side)] : "No cable: this channel is off";
  };
  return [
    failingStreams().length ? healthCard() : null,
    card({ title: "Playback and recording" },
      deviceRow({ key: "output", label: "Headphones / speakers", list: physical(snap.render_devices), empty: "Automatic (your usual default)",
        help: () => "Where every channel is mixed down to." }),
      deviceRow({ key: "mic", label: "Microphone", list: physical(snap.capture_devices), empty: "Automatic (your usual default)",
        help: () => deviceMissing(snap.status.Microphone)
          ? { text: "No microphone found. Connect one, or pick a specific device.", bad: true }
          : "Filtered by the chain in the Mic tab, then sent to the Virtual Mic." })),
    card({ title: "Virtual cables", sub: "Each channel runs through one VB-Audio cable. Pick the side Smowaudio listens on — the other side is set up for you. Changes apply immediately and briefly restart audio." },
      deviceRow({ key: "mic_sink", label: cableLabelEl("Virtual mic", "var(--mic)"), list: cables(snap.render_devices), empty: "None", compact: true,
        help: () => { const side = pairedSide(snap.render_devices, snap.config.mic_sink); return side ? ["Apps pick ", code(side), " as their mic"] : "No cable: apps have no Virtual Mic"; } }),
      ...CHANNELS.map((c, i) => deviceRow({ key: `source${i}`, label: cableLabelEl(c.name, c.color), list: cables(snap.capture_devices), empty: "None", compact: true, help: channelHelp(i) }))),
  ];
}

const cableLabelEl = (name, color) => h("span", { class: "tape small", style: `--c:${color}` }, name);


// ---------- settings: general ----------
function generalTab() {
  const cfg = snap.config;
  const launch = switchEl("", cfg.launch_at_login, (on) => invoke("set_launch_at_login", { enabled: on }).catch((e) => { showError(e); launch.input.checked = !on; }));
  launch.input.setAttribute("aria-label", "Launch at Windows sign-in");
  const chips = h("div", { class: "chips-row" + (cfg.set_windows_defaults ? "" : " off") },
    defaultChip("Playback", "Game", "var(--game)"), defaultChip("Communications", "Chat", "var(--chat)"), defaultChip("Recording", "Virtual mic", "var(--mic)"));
  const defaults = switchEl("", cfg.set_windows_defaults, async (on) => {
    chips.classList.toggle("off", !on);
    try {
      await invoke("set_windows_defaults", { enabled: on });
      cfg.set_windows_defaults = on;
      toast(on ? "Game, Chat and Virtual Mic are now the Windows defaults" : "Your previous Windows defaults are restored");
    } catch (e) { showError(e); defaults.input.checked = !on; chips.classList.toggle("off", on); }
  });
  defaults.input.setAttribute("aria-label", "Set Windows default devices");
  const defaultsRow = settingRow({ label: "Set Windows default devices", fit: true, control: defaults.el,
    help: "While Smowaudio runs, Windows uses these defaults. Turning this off restores the ones you had before." });
  defaultsRow.row.querySelector(".srow-text").append(chips);

  const openFolder = h("button", { type: "button", class: "btn" }, "Open folder");
  openFolder.addEventListener("click", () => invoke("open_log_folder").catch(showError));
  const logPath = h("span", { class: "path" }, "%APPDATA%\\Smowaudio\\smowaudio.log");
  return [
    card(null,
      settingRow({ label: "Launch at Windows sign-in", help: "Starts Smowaudio in the tray when you sign in.", control: launch.el, fit: true }).row,
      defaultsRow.row),
    h("section", { class: "card" }, healthBlock(), settingRow({ label: "Log file", help: logPath, control: openFolder, fit: true }).row),
  ];
}

const defaultChip = (label, value, color) => h("span", { class: "chip" }, h("span", {}, label), h("b", { style: `color:${color}` }, value));

/** The stream health summary without a card of its own, for placing inside another card. */
function healthBlock() {
  const el = h("div", { class: "health-block", "data-health": "" });
  fillHealth(el);
  return el;
}

// ---------- settings: updates ----------
let updateStatus = null;
let installing = false;

function updatesTab() {
  const check = h("button", { type: "button", class: "btn", id: "update-check" }, "Check for updates");
  check.addEventListener("click", () => checkUpdates(true));
  const install = h("button", { type: "button", class: "btn primary", id: "update-install", hidden: true }, "Install update");
  install.addEventListener("click", installUpdate);
  const version = h("section", { class: "card" },
    h("div", { class: "srow fit" },
      h("div", { class: "srow-text" }, h("div", { class: "version num", id: "update-version" }), h("p", { class: "srow-help", id: "update-state" })),
      h("div", { class: "srow-control" }, install, check)),
    h("div", { class: "update-extra", id: "update-extra", hidden: true },
      h("div", { class: "update-progress", id: "update-progress", hidden: true }, h("i")),
      h("p", { class: "update-notes", id: "update-notes", hidden: true })));
  return [version];
}

async function refreshUpdates() {
  try { updateStatus = await invoke("update_status"); } catch { return; }
  renderUpdates();
}

function ago(ms) {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const hrs = Math.round(m / 60);
  return hrs < 24 ? `${hrs} h ago` : `${Math.round(hrs / 24)} d ago`;
}

function renderUpdates() {
  const st = updateStatus;
  if (!st) return;
  $("#pill-update").hidden = !st.available;
  $("#pill-update-version").textContent = st.available ?? "";
  const state = $("#update-state");
  if (!state) return;
  $("#update-version").textContent = `Version ${st.current}`;
  const [text, cls] = st.available ? [`Version ${st.available} is available`, "ok"]
    : st.error ? [st.error, "bad"]
    : st.checked ? [`You're up to date${st.checked_at ? ` · checked ${ago(st.checked_at)}` : ""}`, "ok"]
    : ["Not checked yet", ""];
  state.textContent = text;
  state.className = "srow-help" + (cls ? ` ${cls}` : "");
  $("#update-install").hidden = !st.available;
  $("#update-install").disabled = installing;
  $("#update-check").disabled = installing;
  const notes = $("#update-notes");
  notes.hidden = !(st.available && st.notes);
  notes.textContent = st.notes ?? "";
  $("#update-extra").hidden = notes.hidden && $("#update-progress").hidden;
}

async function checkUpdates(manual = false) {
  const button = $("#update-check");
  if (button) { button.disabled = true; button.textContent = "Checking…"; }
  try {
    const version = await invoke("check_for_update");
    if (manual) toast(version ? `Version ${version} is available` : "You're up to date");
  } catch (e) { if (manual) showError(e); }
  if (button) button.textContent = "Check for updates";
  await refreshUpdates();
}

async function installUpdate() {
  installing = true;
  renderUpdates();
  $("#update-progress").hidden = false;
  $("#update-extra").hidden = false;
  $("#update-install").textContent = "Downloading…";
  try {
    // On success the installer closes this app and starts the new version.
    await invoke("install_update");
  } catch (e) {
    showError(e);
    installing = false;
    $("#update-install").textContent = "Install update";
    $("#update-progress").hidden = true;
    await refreshUpdates();
  }
}

listen("update-status", (e) => { updateStatus = e.payload; renderUpdates(); });
listen("update-progress", (e) => {
  const { downloaded, total } = e.payload;
  const bar = $("#update-progress i");
  if (bar && total) bar.style.width = `${(downloaded / total) * 100}%`;
  const button = $("#update-install");
  if (button && total) button.textContent = downloaded >= total ? "Installing… Smowaudio restarts in a moment" : `Downloading… ${Math.round((downloaded / total) * 100)} %`;
});
$("#pill-update").addEventListener("click", () => { setSettingsTab("updates"); setView("settings"); });

// ---------- keyboard shortcuts ----------
// Action ids must match hotkeys.rs.
const channelShortcuts = (id, name, color) => ({
  id, title: name, color, actions: [
    [`channel.${id}.volume_up`, "Volume up"],
    [`channel.${id}.volume_down`, "Volume down"],
    [`channel.${id}.mute`, "Mute on/off"],
    [`channel.${id}.eq`, "EQ on/off"],
  ],
});
const SHORTCUT_GROUPS = [
  { id: "general", title: "General", actions: [
    ["app.mixer", "Open the mixer"],
    ["app.flyout", "Show or hide the tray flyout"],
    ["output.next", "Next output device"],
    ["output.previous", "Previous output device"],
    ["windows_defaults", "Windows default devices on/off"],
  ] },
  channelShortcuts("master", "Master", "var(--master)"),
  ...CHANNELS.map((c) => channelShortcuts(c.name.toLowerCase(), c.name, c.color)),
  { id: "mic", title: "Mic", color: "var(--mic)", actions: [
    ["mic.mute", "Mute on/off"],
    ["mic.push_to_talk", "Push to talk (hold)"],
    ["mic.push_to_mute", "Push to mute (hold)"],
    ["mic.gain_up", "Gain up 1 dB"],
    ["mic.gain_down", "Gain down 1 dB"],
    ["mic.monitor", "Listen to yourself on/off"],
    ["mic.denoise", "Noise removal on/off"],
    ["mic.low_latency", "Low-latency noise model on/off"],
    ["mic.gate", "Noise gate on/off"],
    ["mic.eq", "EQ on/off"],
    ["mic.compressor", "Compressor on/off"],
  ] },
];
const groupOf = (action) => SHORTCUT_GROUPS.find((g) => g.actions.some(([id]) => id === action));
const actionLabel = (action) => groupOf(action)?.actions.find(([id]) => id === action)?.[1] ?? action;
/** "Game → Mute on/off", or just the label for General actions. */
const shortcutLabel = (action) => {
  const g = groupOf(action);
  return !g ? action : g.id === "general" ? actionLabel(action) : `${g.title} → ${actionLabel(action)}`;
};

// KeyboardEvent.code -> what's printed on the key. Codes not listed show as-is (F13, Pause…).
const KEY_NAMES = {
  Backquote: "`", Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Semicolon: ";", Quote: "'",
  Comma: ",", Period: ".", Slash: "/", ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→", PageUp: "Page Up",
  PageDown: "Page Down", PrintScreen: "Print Screen", ScrollLock: "Scroll Lock", NumLock: "Num Lock", CapsLock: "Caps Lock",
  NumpadAdd: "Num +", NumpadSubtract: "Num −", NumpadMultiply: "Num *", NumpadDivide: "Num /", NumpadDecimal: "Num .",
  NumpadEnter: "Num Enter", NumpadEqual: "Num =", AudioVolumeUp: "Volume Up", AudioVolumeDown: "Volume Down",
  AudioVolumeMute: "Volume Mute", MediaPlayPause: "Play/Pause", MediaStop: "Media Stop", MediaTrackNext: "Next Track",
  MediaTrackPrevious: "Previous Track", Super: "Win",
};
const keyName = (part) => KEY_NAMES[part] ?? part.replace(/^Key|^Digit/, "").replace(/^Numpad(\d)$/, "Num $1");
const comboText = (keys) => keys.split("+").map(keyName).join(" + ");
// Keys the shortcut library can register (global-hotkey's parser).
const SUPPORTED_KEY = /^(Key[A-Z]|Digit\d|F([1-9]|1\d|2[0-4])|Numpad(\d|Add|Subtract|Multiply|Divide|Decimal|Enter|Equal)|Arrow(Up|Down|Left|Right)|Backquote|Minus|Equal|BracketLeft|BracketRight|Backslash|Semicolon|Quote|Comma|Period|Slash|Space|Tab|Enter|Backspace|Delete|Insert|Home|End|PageUp|PageDown|PrintScreen|ScrollLock|Pause|NumLock|CapsLock|AudioVolume(Up|Down|Mute)|Media(PlayPause|Stop|TrackNext|TrackPrevious))$/;
// Keys that are fine on their own; anything else needs a modifier so typing keeps working.
const BARE_OK = /^(F([1-9]|1\d|2[0-4])|Pause|ScrollLock|PrintScreen|AudioVolume\w+|Media\w+)$/;
const MODIFIER_CODES = new Set(["ControlLeft", "ControlRight", "AltLeft", "AltRight", "ShiftLeft", "ShiftRight", "MetaLeft", "MetaRight", "OSLeft", "OSRight"]);

let shortcutChannel = store.get("shortcutChannel", "general");
if (!SHORTCUT_GROUPS.some((g) => g.id === shortcutChannel)) shortcutChannel = "general";
let recording = null; // { action }: the one row waiting for keys
let conflict = null; // { action, combo, other }: a recorded combo another action already uses
let recordNote = null; // { action, text }: why the last key pressed can't be used

const keycaps = (keys) => keys.split("+").map((k) => h("kbd", {}, keyName(k)));
const totalShortcuts = () => Object.keys(snap.config.hotkeys).length;

function shortcutsTab() {
  const cfg = snap.config;
  const steps = [0.01, 0.02, 0.05, 0.1];
  const step = h("div", { class: "seg", role: "group", "aria-label": "Volume step" }, ...steps.map((s) => {
    const b = h("button", { type: "button", "aria-pressed": String(Math.abs(cfg.volume_step - s) < 0.001) }, `${Math.round(s * 100)}%`);
    b.addEventListener("click", () => {
      cfg.volume_step = s;
      step.querySelectorAll("button").forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
      invoke("set_volume_step", { step: s }).catch(showError);
    });
    return b;
  }));
  const reset = h("button", { type: "button", class: "btn", id: "sc-reset" }, "Reset all…");
  reset.addEventListener("click", confirmResetShortcuts);
  return [
    h("div", { class: "sc-toolbar" },
      h("p", {}, "Shortcuts work anywhere in Windows, even while a game has focus."),
      h("label", { class: "sc-step" }, h("span", {}, "Volume step"), step),
      reset),
    h("section", { class: "card sc-card" }, h("nav", { class: "sc-list", id: "sc-list", "aria-label": "Shortcut groups" }), h("div", { class: "sc-panel", id: "sc-panel" })),
  ];
}

function renderShortcuts() {
  const list = $("#sc-list"), panel = $("#sc-panel");
  if (!list || !panel) return;
  const hotkeys = snap.config.hotkeys;
  $("#sc-reset").disabled = totalShortcuts() === 0;

  list.replaceChildren(...SHORTCUT_GROUPS.map((g) => {
    const assigned = g.actions.filter(([id]) => hotkeys[id]).length;
    const flagged = conflict && groupOf(conflict.other) === g;
    const b = h("button", { type: "button", class: "sc-item", "aria-current": g.id === shortcutChannel ? "true" : null },
      g.color ? h("span", { class: "tape small", style: `--c:${g.color}` }, g.title) : h("span", { class: "sc-plain" }, g.title),
      h("span", { class: "sc-count" }, flagged ? h("span", { class: "tabdot", title: "One of these clashes with the shortcut you just pressed" }) : null, `${assigned}/${g.actions.length}`));
    b.addEventListener("click", () => {
      shortcutChannel = g.id;
      store.set("shortcutChannel", g.id);
      if (recording) cancelRecording();
      conflict = null;
      renderShortcuts();
    });
    return b;
  }));

  const group = SHORTCUT_GROUPS.find((g) => g.id === shortcutChannel);
  panel.replaceChildren(
    h("div", { class: "sc-head" }, h("h3", {}, group.title), h("span", {}, "Click a shortcut, then press the keys")),
    ...group.actions.map(([action, label]) => shortcutRow(action, label)));
}

function shortcutRow(action, label) {
  const keys = snap.config.hotkeys[action];
  const isRecording = recording?.action === action;
  const clash = conflict?.action === action ? conflict : null;
  const shown = clash ? clash.combo : keys;
  const button = h("button", { type: "button", class: "keybind" + (isRecording ? " recording" : !shown ? " empty" : "") + (clash ? " clash" : ""),
    "aria-label": `${label}: ${shown ? comboText(shown) : "not set"}. Change shortcut` },
    ...(isRecording ? [h("span", { class: "recdot" }), "Press keys…"] : shown ? keycaps(shown) : ["Set"]));
  button.addEventListener("click", () => startRecording(action));
  const clear = keys && !isRecording ? h("button", { type: "button", class: "iconbtn keyclear", title: "Remove shortcut", "aria-label": `Remove shortcut for ${label}` }, "×") : h("span");
  if (keys && !isRecording) clear.addEventListener("click", () => saveShortcut(action, null));

  let note = null;
  if (isRecording) {
    note = h("div", { class: "sc-note" }, recordNote?.action === action ? recordNote.text : [h("kbd", {}, "Esc"), " cancels · ", h("kbd", {}, "Backspace"), " clears"]);
  } else if (clash) {
    const use = h("button", { type: "button", class: "btn" }, `Use here, clear ${groupOf(clash.other).title}`);
    use.addEventListener("click", () => { conflict = null; saveShortcut(action, clash.combo); });
    const again = h("button", { type: "button", class: "btn" }, "Pick another");
    again.addEventListener("click", () => startRecording(action));
    note = h("div", { class: "sc-note warn" },
      h("span", {}, `${comboText(clash.combo)} is already `, h("b", {}, shortcutLabel(clash.other)), ". Only one can use it."),
      h("span", { class: "sc-actions" }, use, again));
  } else if (snap.hotkey_errors[action]) {
    note = h("div", { class: "sc-note bad" }, snap.hotkey_errors[action]);
  }
  return h("div", { class: "sc-row" + (clash ? " clash" : ""), "data-action": action },
    h("span", { class: "sc-label" }, label), button, clear, note);
}

function startRecording(action) {
  // Bound shortcuts would otherwise fire instead of reaching this page.
  if (!recording) invoke("pause_hotkeys", { paused: true }).catch(() => {});
  recording = { action };
  recordNote = null;
  // A clash on another row stays until it's resolved there.
  if (conflict?.action === action) conflict = null;
  renderShortcuts();
}

/** Stops listening for keys and turns the shortcuts back on. */
function cancelRecording() {
  if (!recording) return Promise.resolve();
  recording = null;
  recordNote = null;
  return invoke("pause_hotkeys", { paused: false }).then((errors) => { snap.hotkey_errors = errors; }).catch(() => {});
}

async function stopRecording() {
  await cancelRecording();
  renderShortcuts();
}

async function saveShortcut(action, keys) {
  recording = null;
  recordNote = null;
  try {
    snap.hotkey_errors = await invoke("set_hotkey", { action, keys });
    if (keys) {
      for (const [a, k] of Object.entries(snap.config.hotkeys)) if (k === keys) delete snap.config.hotkeys[a];
      snap.config.hotkeys[action] = keys;
    } else {
      delete snap.config.hotkeys[action];
    }
  } catch (e) { showError(e); }
  renderShortcuts();
}

function confirmResetShortcuts() {
  const count = totalShortcuts();
  if (!count) return;
  const cancel = h("button", { type: "button", class: "btn" }, "Cancel");
  const clear = h("button", { type: "button", class: "btn hot" }, "Clear all");
  const dialog = h("dialog", { class: "confirm", "aria-labelledby": "confirm-title" },
    h("p", { id: "confirm-title" }, `Clear all ${count} shortcut${count === 1 ? "" : "s"}? This can't be undone.`),
    h("div", { class: "confirm-actions" }, cancel, clear));
  cancel.addEventListener("click", () => dialog.close());
  clear.addEventListener("click", async () => {
    dialog.close();
    await cancelRecording();
    conflict = null;
    try {
      snap.hotkey_errors = await invoke("clear_hotkeys");
      snap.config.hotkeys = {};
      toast("All shortcuts cleared");
    } catch (e) { showError(e); }
    renderShortcuts();
  });
  dialog.addEventListener("close", () => dialog.remove());
  document.body.append(dialog);
  dialog.showModal();
  cancel.focus();
}

document.addEventListener("keydown", (e) => {
  if (!recording) return;
  e.preventDefault();
  e.stopPropagation();
  const { action } = recording;
  const button = $(`[data-action="${CSS.escape(action)}"] .keybind`);
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
  if (MODIFIER_CODES.has(e.code)) {
    button?.replaceChildren(...keycaps(mods.join("+")), "…");
    return;
  }
  if (!mods.length && e.code === "Escape") return stopRecording();
  if (!mods.length && (e.code === "Backspace" || e.code === "Delete")) return saveShortcut(action, null);
  const note = (text) => { recordNote = { action, text }; renderShortcuts(); };
  if (!SUPPORTED_KEY.test(e.code)) return note("That key can't be used for a shortcut. Try another.");
  if (!mods.length && !BARE_OK.test(e.code)) return note("Add Ctrl, Alt, Shift or Win to this key.");
  const combo = [...mods, e.code].join("+");
  if (snap.config.hotkeys[action] === combo) return stopRecording();
  // Checked before saving: a combo that's taken asks which action should keep it.
  const other = Object.entries(snap.config.hotkeys).find(([a, k]) => k === combo && a !== action)?.[0];
  if (other) {
    cancelRecording().then(() => { conflict = { action, combo, other }; renderShortcuts(); });
    return;
  }
  saveShortcut(action, combo);
}, true);
// Clicking elsewhere or leaving the window cancels recording.
window.addEventListener("blur", () => { if (recording) stopRecording(); });
document.addEventListener("pointerdown", (e) => { if (recording && !e.target.closest(".keybind")) stopRecording(); }, true);

function renderStatus() {
  const bad = failingStreams().length;
  $("#pill-status").classList.toggle("warn", bad > 0);
  $("#pill-status-text").textContent = bad ? `${bad} ${bad === 1 ? "stream needs" : "streams need"} attention` : "All streams running";
  $("#pill-output").textContent = shortName(outputName());
  const dot = $("#tabdot-devices");
  if (dot) dot.hidden = !bad;
  document.querySelectorAll("[data-health]").forEach(fillHealth);
  // Devices shows the health card only while something is wrong, so add or drop it.
  const onDevices = view === "settings" && settingsTab === "devices";
  if (onDevices && Boolean(bad) !== Boolean($("#settings-panel [data-health]"))) renderSettings();
  for (const key of Object.keys(deviceRows)) showDeviceRow(key);
}

async function refreshStatus() {
  const next = await invoke("get_state");
  snap.status = next.status;
  renderStatus();
}
$("#pill-status").addEventListener("click", () => { setSettingsTab("devices"); setView("settings"); });

// ---------- navigation & polling ----------
function setView(next) {
  // "settings:devices" opens Settings on that tab (the flyout's status pill asks for this).
  const [name, tab] = next.split(":");
  next = name;
  if (tab && SETTINGS_TABS.some(([id]) => id === tab) && tab !== settingsTab) {
    settingsTab = tab;
    store.set("settingsTab", tab);
    if (snap) renderSettings();
  }
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

const isObj = (x) => x !== null && typeof x === "object" && !Array.isArray(x);
/** Copies `src` into `target` in place, so everything holding a piece of snap.config stays valid. */
function mergeInto(target, src) {
  for (const k of Object.keys(target)) if (!(k in src)) delete target[k];
  for (const [k, v] of Object.entries(src)) {
    const t = target[k];
    if (Array.isArray(v) && Array.isArray(t) && t.length === v.length) {
      v.forEach((x, i) => { if (isObj(x) && isObj(t[i])) mergeInto(t[i], x); else t[i] = x; });
    } else if (Array.isArray(v) && Array.isArray(t)) {
      t.length = 0; t.push(...v);
    } else if (isObj(v) && isObj(t)) {
      mergeInto(t, v);
    } else {
      target[k] = v;
    }
  }
}

const settingsKey = (c) => JSON.stringify([c.output_device, c.mic_device, c.mic_sink, c.channels.map((x) => x.source),
  c.launch_at_login, c.set_windows_defaults, c.hotkeys, c.volume_step]);
const micKey = (m) => JSON.stringify({ ...m, gain_db: 0, muted: false });

/** Picks up settings changed by the flyout or a shortcut, updating only what changed. */
async function syncConfig() {
  const next = await invoke("get_config");
  const before = { settings: settingsKey(snap.config), mic: micKey(snap.config.mic), eq: snap.config.channels.map((c) => JSON.stringify(c.settings.eq)) };
  mergeInto(snap.config, next);
  updateStrips();
  renderMicControls();
  if (eqOpen !== null && JSON.stringify(snap.config.channels[eqOpen].settings.eq) !== before.eq[eqOpen]) renderDrawer();
  if (micKey(snap.config.mic) !== before.mic) renderMic();
  if (settingsKey(snap.config) !== before.settings && !settingsDirty && !recording) renderSettings();
  updateStripIcons();
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
      if (view === "mic") { updateMicMeters(m.mic); updateMicTest(m.mic_test); }
    } catch { /* engine restarting */ }
  }
  setTimeout(pollMeters, 50);
}

async function pollApps() {
  if (!document.hidden && snap) await refreshApps();
  setTimeout(pollApps, view === "apps" ? 900 : 3000);
}

// Stream status, and devices plugged in or removed.
const deviceKey = (x) => JSON.stringify([x.render_devices, x.capture_devices]);
setInterval(async () => {
  if (document.hidden || !snap) return;
  try {
    const next = await invoke("get_state");
    snap.status = next.status;
    if (deviceKey(next) !== deviceKey(snap)) {
      snap.render_devices = next.render_devices;
      snap.capture_devices = next.capture_devices;
      if (!settingsDirty && !recording) renderSettings();
      updateStrips();
    }
    renderStatus();
  } catch { /* ignore */ }
}, 3000);

// Anything changed while this window was in the background.
window.addEventListener("focus", () => { if (snap) syncConfig().catch(showError); });
listen("show-view", (e) => setView(e.payload));
// The tray flyout or a shortcut changed settings: show it right away. Changes made here are
// skipped (so a fader being dragged isn't redrawn), and bursts, like a held volume shortcut,
// reload at most about 8 times a second.
let reloadTimer = null;
listen("config-changed", (e) => {
  if (e.payload?.source === "main" || reloadTimer) return;
  reloadTimer = setTimeout(() => {
    reloadTimer = null;
    if (snap) syncConfig().catch(showError);
  }, 120);
});

(async () => {
  try {
    await loadState();
    const wanted = await invoke("take_pending_view");
    if (wanted) setView(wanted);
    await refreshApps();
  } catch (e) { showError(e); }
  pollMeters();
  pollApps();
  pollAppLevels();
})();
