// Main window, part 1 of 7: shared state, helpers, EQ editor, app helpers.
// The main window's scripts are plain scripts sharing one global scope, loaded in order by
// index.html (core, mixer, apps, mic, settings, shortcuts, main); main.js starts everything.

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

const tape = (name, color) => h("span", { class: "tape", style: color ? `--c:${color}${color === "var(--master)" ? ";--ink:var(--master-ink)" : ""}` : null }, name);
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

/**
 * EQ editor: header (lead items, On switch, presets, close), draggable response curve, band cards.
 * `showSwitch: false` leaves the on/off switch to the surrounding card (the Mic tab).
 */
function eqEditor(eq, onChange, { lead = [], onClose = null, showSwitch = true, graphHeight = 118 } = {}) {
  const canvas = h("canvas", { class: "curve", role: "img", "aria-label": "Equalizer curve", style: `height:${graphHeight}px` });
  const seg = h("div", { class: "seg", role: "group", "aria-label": "Preset" });
  const bands = h("div", { class: "bands" });
  const enabled = switchEl(eq.enabled ? "On" : "Off", eq.enabled, (v) => { eq.enabled = v; showState(); refresh(); onChange(); });
  const showState = () => { enabled.el.lastChild.textContent = eq.enabled ? "On" : "Off"; };
  let selected = -1; // the band last clicked or dragged
  const RANGE = 15, hasGain = (b) => b.kind !== "low_pass" && b.kind !== "high_pass";
  const fx = (f, W) => (Math.log10(f / 20) / 3) * W, xf = (x, W) => 20 * Math.pow(10, (x / W) * 3);
  const gy = (g, H) => H / 2 - (g / RANGE) * (H / 2 - 12), yg = (y, H) => ((H / 2 - y) / (H / 2 - 12)) * RANGE;

  function renderSeg() {
    seg.replaceChildren(...[...Object.keys(PRESETS), "Custom"].map((name) => {
      const b = h("button", { type: "button", "aria-pressed": String(eq.preset === name) }, name);
      if (name === "Custom") b.disabled = eq.preset !== "Custom";
      else b.addEventListener("click", () => {
        eq.preset = name; eq.bands = structuredClone(PRESETS[name]); eq.enabled = true; enabled.input.checked = true; showState();
        refresh(); onChange();
      });
      return b;
    }));
  }
  function renderBands() {
    bands.replaceChildren(...eq.bands.map((b, i) => {
      const f = b.freq >= 1000 ? `${(b.freq / 1000).toFixed(b.freq % 1000 ? 1 : 0)} kHz` : `${Math.round(b.freq)} Hz`;
      const g = hasGain(b) ? `${b.gain_db > 0 ? "+" : b.gain_db < 0 ? "−" : ""}${Math.abs(b.gain_db).toFixed(1)} dB` : "12 dB/oct";
      const card = h("button", { type: "button", class: "band" + (i === selected ? " sel" : ""), "aria-pressed": String(i === selected) },
        h("span", {}, KIND_LABEL[b.kind] || b.kind), h("b", {}, `${f} · ${g}`));
      card.addEventListener("click", () => { selected = i; renderBands(); draw(); });
      return card;
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
    eq.bands.forEach((b, i) => {
      const x = fx(b.freq, W), y = gy(hasGain(b) ? b.gain_db : 0, H);
      if (i === selected) {
        // The selected point is larger, with a 4px ring.
        ctx.beginPath(); ctx.globalAlpha = 0.3; ctx.strokeStyle = color; ctx.lineWidth = 4;
        ctx.arc(x, y, 9.5, 0, Math.PI * 2); ctx.stroke(); ctx.globalAlpha = 1;
      }
      ctx.beginPath(); ctx.fillStyle = b.enabled ? color : faint;
      ctx.arc(x, y, i === selected ? 7 : 5.5, 0, Math.PI * 2); ctx.fill();
    });
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
  canvas.addEventListener("pointerdown", (e) => {
    dragging = nearest(e, 16);
    if (dragging >= 0) { canvas.setPointerCapture(e.pointerId); selected = dragging; renderBands(); draw(); }
  });
  canvas.addEventListener("pointermove", (e) => {
    if (dragging < 0) return;
    const W = canvas.clientWidth, H = canvas.clientHeight, b = eq.bands[dragging];
    b.freq = Math.round(clamp(xf(e.offsetX, W), 20, 20000));
    if (hasGain(b)) b.gain_db = Math.round(clamp(yg(e.offsetY, H), -RANGE, RANGE) * 2) / 2;
    eq.preset = "Custom"; eq.enabled = true; enabled.input.checked = true; showState();
    draw(); renderBands(); onChange();
  });
  canvas.addEventListener("pointerup", () => { if (dragging >= 0) { dragging = -1; renderSeg(); renderBands(); } });
  canvas.addEventListener("wheel", (e) => {
    const i = nearest(e, 22);
    if (i < 0) return;
    e.preventDefault();
    const b = eq.bands[i];
    selected = i;
    b.q = Math.round(clamp(b.q * (e.deltaY < 0 ? 1.1 : 0.9), 0.1, 10) * 100) / 100;
    eq.preset = "Custom"; refresh(); onChange();
  }, { passive: false });
  new ResizeObserver(draw).observe(canvas);
  renderSeg(); renderBands();
  const close = onClose ? h("button", { type: "button", class: "iconbtn eq-close", title: "Close", "aria-label": "Close equalizer" }, "×") : null;
  close?.addEventListener("click", onClose);
  return h("div", { class: "eq" },
    h("div", { class: "drawer-head" }, ...lead, showSwitch ? enabled.el : null, h("span", { class: "spacer" }), seg, close),
    canvas, bands,
    h("p", { class: "note" }, "Drag a point to shape the sound. Scroll over a point to make it wider or narrower."));
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
