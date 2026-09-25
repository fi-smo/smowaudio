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

// ---------- mixer ----------
const meterRefs = new Map(); // key -> { lits, peaks, hold, holdAt, muted }
const stripRefs = new Map(); // key -> update(volume, muted): shows changes made elsewhere
const SCALE = [0, -12, -24, -36, -48, -60];
let eqOpen = null;

const SPEAKER_ON = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 6h2.5l3.5-3v10l-3.5-3h-2.5z"/><path d="M11 5.5a3.5 3.5 0 0 1 0 5M12.8 3.5a6.2 6.2 0 0 1 0 9"/></svg>';
const SPEAKER_OFF = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 6h2.5l3.5-3v10l-3.5-3h-2.5z"/><path d="M11 6l3.5 4M14.5 6 11 10"/></svg>';

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
  // The cap is 14px tall, so its centre travels 7px in from each end.
  const fromY = (y) => { const r = el.getBoundingClientRect(); set(clamp((r.bottom - 7 - y) / (r.height - 14), 0, 1) * MAX_VOLUME); };
  el.addEventListener("pointerdown", (e) => { el.setPointerCapture(e.pointerId); el.dataset.dragging = ""; fromY(e.clientY); });
  el.addEventListener("pointermove", (e) => { if (el.hasPointerCapture(e.pointerId)) fromY(e.clientY); });
  el.addEventListener("lostpointercapture", () => delete el.dataset.dragging);
  el.addEventListener("dblclick", () => set(1));
  el.addEventListener("wheel", (e) => { e.preventDefault(); set(v + (e.deltaY < 0 ? 0.02 : -0.02)); }, { passive: false });
  el.addEventListener("keydown", (e) => {
    const step = { ArrowUp: 0.02, ArrowDown: -0.02, PageUp: 0.1, PageDown: -0.1 }[e.key];
    if (step !== undefined) { set(v + step); e.preventDefault(); }
    if (e.key === "Home") { set(1); e.preventDefault(); }
  });
  /** Shows a value changed elsewhere (flyout, shortcut) unless the user is dragging this fader. */
  el.setValue = (next) => { if (!("dragging" in el.dataset)) { v = clamp(next, 0, MAX_VOLUME); render(); } };
  render();
  return el;
}

/**
 * One channel strip: tape and cable, app icons, dB scale + meters + fader, one dB readout, and a
 * footer with the mute button and a feature button (EQ, Chain or Limit).
 */
function strip({ key, name, color, sub, icons, volume, onVolume, muted, onMute, extra, kind = "" }) {
  const lits = [h("i", { class: "lit" }), h("i", { class: "lit" })];
  const peaks = [h("i", { class: "peak", hidden: true }), h("i", { class: "peak", hidden: true })];
  meterRefs.set(key, { lits, peaks, hold: [0, 0], holdAt: [0, 0], muted });
  const readout = h("div", { class: "readout num" });
  let current = volume, isMuted = muted;
  const showReadout = () => { readout.textContent = isMuted ? "Muted" : `${fmtDb(dbOf(current))} dB`; };
  const el = h("div", { class: `strip ${kind}`.trim(), style: `--c:${color}`, "data-strip": key });
  const mute = h("button", { type: "button", class: "btn mute" });
  const showMuted = (m) => {
    isMuted = m;
    mute.setAttribute("aria-pressed", String(m));
    mute.title = `${m ? "Unmute" : "Mute"} ${name}`;
    mute.setAttribute("aria-label", mute.title);
    mute.innerHTML = m ? SPEAKER_OFF : SPEAKER_ON;
    el.classList.toggle("muted", m);
    meterRefs.get(key).muted = m;
    showReadout();
  };
  mute.addEventListener("click", () => { showMuted(!isMuted); onMute(isMuted); });
  const fader = makeFader(name, volume, (v) => { current = v; showReadout(); onVolume(v); });
  stripRefs.set(key, (v, m) => {
    fader.setValue(v);
    if (!("dragging" in fader.dataset)) current = v;
    showMuted(m);
  });
  el.append(
    h("div", { class: "strip-top" }, tape(name, color), h("small", { title: sub }, sub)),
    h("div", { class: "appicons" }, icons),
    h("div", { class: "console" },
      h("div", { class: "scale", "aria-hidden": "true" }, SCALE.map((d) => h("span", { style: `bottom:${meterPct(d)}%` }, d === 0 ? "0" : `−${-d}`))),
      h("div", { class: "console-main" },
        h("div", { class: "meter", "aria-hidden": "true" }, h("div", { class: "bar" }, lits[0], peaks[0]), h("div", { class: "bar" }, lits[1], peaks[1])),
        fader)),
    readout,
    h("div", { class: "strip-btns" }, mute, extra));
  showMuted(muted);
  return el;
}

function cableLabel(id) {
  const name = deviceName(snap.capture_devices, id) || deviceName(snap.render_devices, id);
  return name ? name.replace(/\s*\(.*\)$/, "").replace(/ Output$| Input$/, "") : "No cable";
}

/** "EQ · Off", "Chain · 4 of 5 on": a faint label and a value. */
const featureButton = (label, value, attrs = {}) =>
  h("button", { type: "button", class: "btn feature", ...attrs }, h("span", { class: "k" }, label), h("span", { class: "v" }, value));

const MIC_STEPS = ["denoise", "gate", "eq", "compressor", "limiter"];
const micStepsOn = (mic) => MIC_STEPS.filter((k) => mic[k]?.enabled).length;

function renderMixer() {
  const root = $("#view-mixer");
  meterRefs.clear();
  stripRefs.clear();
  const playback = h("div", { class: "strips playback" });
  CHANNELS.forEach((c, i) => {
    const cfg = snap.config.channels[i];
    const s = cfg.settings;
    const eqButton = featureButton("EQ", s.eq.enabled ? s.eq.preset : "Off", { "aria-expanded": String(eqOpen === i), "data-eq": i, title: `${c.name} equalizer` });
    eqButton.classList.add("eq");
    eqButton.addEventListener("click", () => toggleDrawer(i));
    playback.append(strip({
      key: `ch${i}`, name: c.name, color: c.color, sub: cfg.source ? cableLabel(cfg.source) : "No cable",
      icons: cfg.source ? [] : h("span", { class: "appnote" }, "Pick a cable in Settings"),
      volume: s.volume, onVolume: (v) => { s.volume = v; sendChannel(i); },
      muted: s.muted, onMute: (m) => { s.muted = m; sendChannel(i); },
      extra: eqButton,
    }));
  });
  const master = snap.config.master;
  playback.append(
    h("div", { class: "sum-arrow", "aria-hidden": "true" }, h("span", { class: "chev right" })),
    strip({
      key: "master", name: "Master", color: "var(--master)", sub: "Output", kind: "master",
      icons: h("span", { class: "appnote", title: snap.active_output || outputName() }, shortName(snap.active_output || outputName())),
      volume: master.volume, onVolume: (v) => { master.volume = v; sendMaster(); },
      muted: master.muted, onMute: (m) => { master.muted = m; sendMaster(); },
      extra: featureButton("Limit", h("span", { id: "limit-readout" }, "0.0 dB"), { class: "btn feature static", disabled: true, title: "Peaks above −1 dBFS are turned down so the mix never clips" }),
    }));

  const mic = snap.config.mic;
  const micDevice = snap.active_mic || deviceName(snap.capture_devices, snap.config.mic_device);
  const chain = featureButton("Chain", `${micStepsOn(mic)} of ${MIC_STEPS.length} on`, { id: "mic-chain-btn", title: "Open the mic chain" });
  chain.addEventListener("click", () => setView("mic"));
  const voice = h("div", { class: "strips voice" }, strip({
    key: "mic", name: "Mic", color: "var(--mic)", sub: virtualMicName() || "Virtual mic", kind: "mic",
    icons: h("span", { class: "appnote", title: micDevice || "" }, shortName(micDevice) || "No microphone"),
    volume: micVolume(mic),
    onVolume: (v) => { mic.gain_db = v <= 0.001 ? -60 : Math.round(dbOf(v) * 10) / 10; sendMic(); },
    muted: mic.muted, onMute: (m) => { mic.muted = m; sendMic(); renderMicControls(); },
    extra: chain,
  }));

  root.replaceChildren(
    viewHead("Mixer", "System sounds and apps you haven't placed play in Game.",
      h("div", { class: "chips" }, bufferPill())),
    h("div", { class: "mixer-groups" },
      h("section", { class: "mixer-group", "aria-label": "Playback" },
        h("div", { class: "group-label" }, h("span", {}, "Playback"), h("small", {}, "Game, Chat, Media and Aux mix into Master, then your headphones")),
        playback),
      h("section", { class: "mixer-group voice-group", "aria-label": "Your voice" },
        h("div", { class: "group-label" }, h("span", {}, "Your voice"), h("small", {}, "Sent to apps as the virtual mic")),
        voice)),
    h("section", { class: "drawer", id: "eq-drawer", hidden: true }));
  renderDrawer();
  updateStripIcons();
  // Re-fit whenever the EQ panel changes height (opening, a preset adding band cards).
  new ResizeObserver(() => fitMixer()).observe($("#eq-drawer"));
  fitMixer();
}

// The Mixer, with the EQ open, should fit the window without scrolling: shorten the faders (down
// to 140px) before the page has to scroll.
const FADER_MAX = 190, FADER_MIN = 140;
function fitMixer() {
  const root = $("#view-mixer"), content = $(".content");
  if (!root || root.hidden) return;
  root.style.setProperty("--fader-h", `${FADER_MAX}px`);
  const overflow = content.scrollHeight - content.clientHeight;
  if (overflow > 0) root.style.setProperty("--fader-h", `${Math.max(FADER_MIN, FADER_MAX - overflow)}px`);
}
window.addEventListener("resize", () => fitMixer());

/** Smowaudio's own buffer; clicking it opens the full delay measurement in Settings. */
function bufferPill() {
  const b = h("button", { type: "button", class: "pill", title: "Smowaudio's own buffer. Click to measure the full delay." },
    "Buffer ", h("b", { class: "num", id: "chip-buffer" }, "–"));
  b.addEventListener("click", () => {
    setSettingsTab("devices");
    setView("settings");
    $("#delay-card")?.scrollIntoView({ block: "start" });
  });
  return b;
}

const micVolume = (mic) => clamp(Math.pow(10, mic.gain_db / 20), 0, MAX_VOLUME);

/** Brings the mixer strips in line with snap.config without rebuilding them. */
function updateStrips() {
  snap.config.channels.forEach((c, i) => {
    stripRefs.get(`ch${i}`)?.(c.settings.volume, c.settings.muted);
    const b = $(`[data-eq="${i}"] .v`);
    if (b) b.textContent = c.settings.eq.enabled ? c.settings.eq.preset : "Off";
  });
  stripRefs.get("mic")?.(micVolume(snap.config.mic), snap.config.mic.muted);
  stripRefs.get("master")?.(snap.config.master.volume, snap.config.master.muted);
  const chain = $("#mic-chain-btn .v");
  if (chain) chain.textContent = `${micStepsOn(snap.config.mic)} of ${MIC_STEPS.length} on`;
  const outNote = $('[data-strip="master"] .appicons .appnote');
  if (outNote) { outNote.textContent = shortName(snap.active_output || outputName()); outNote.title = snap.active_output || outputName(); }
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
  fitMixer();
}
function renderDrawer() {
  const drawer = $("#eq-drawer");
  if (!drawer) return;
  if (eqOpen === null) { drawer.hidden = true; drawer.replaceChildren(); return; }
  const i = eqOpen, c = CHANNELS[i], eq = snap.config.channels[i].settings.eq;
  drawer.hidden = false;
  drawer.style.setProperty("--c", c.color);
  drawer.replaceChildren(eqEditor(eq, () => {
    sendChannel(i);
    const v = $(`[data-eq="${i}"] .v`);
    if (v) v.textContent = eq.enabled ? eq.preset : "Off";
  }, { lead: [tape(c.name), h("h3", {}, "Equalizer")], onClose: () => toggleDrawer(i), graphHeight: 118 }));
}

function setMeterPair(key, pair) {
  const m = meterRefs.get(key);
  if (!m || !m.lits[0].isConnected) return;
  const now = performance.now();
  pair.forEach((db, i) => {
    // A muted strip's meters stay empty.
    const pct = m.muted ? 0 : meterPct(db);
    m.lits[i].style.setProperty("--lvl", `${pct}%`);
    if (pct >= m.hold[i]) { m.hold[i] = pct; m.holdAt[i] = now; }
    else if (now - m.holdAt[i] > 900) m.hold[i] = Math.max(pct, m.hold[i] - 2.5);
    m.peaks[i].style.setProperty("--pk", `${m.hold[i]}%`);
    m.peaks[i].hidden = m.hold[i] <= 0;
  });
}

// ---------- apps view ----------
let dragExe = null;
let laneSignature = "";
let moveMenu = null; // the open Move menu, if any

// An empty image, so the browser's own drag image doesn't cover the columns; the drop zone shows a
// preview of the card instead.
const NO_DRAG_IMAGE = (() => { const i = new Image(); i.src = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw=="; return i; })();

function renderApps() {
  closeMoveMenu();
  const root = $("#view-apps");
  const lanes = h("div", { class: "lanes" });
  CHANNELS.forEach((c, i) => {
    const list = apps.filter((a) => channelOf(a).index === i);
    const zone = h("div", { class: "dropzone", hidden: list.length > 0 }, h("span", { class: "dz-text" }, "Drag an app here"));
    const lane = h("section", { class: "lane", style: `--c:${c.color}`, "aria-label": `${c.name} channel`, "data-lane": i },
      h("div", { class: "lane-head" }, tape(c.name), h("small", {}, `${list.length} app${list.length === 1 ? "" : "s"}`)),
      ...list.map(appCard),
      zone,
      i === 0 ? h("p", { class: "lane-note" }, h("span", { class: "info", "aria-hidden": "true" }, "i"), "Default channel. System sounds and new apps play here.") : null);
    lane.addEventListener("dragover", (e) => {
      const app = apps.find((a) => a.exe === dragExe);
      if (!app || channelOf(app).index === i) return;
      e.preventDefault();
      if (lane.classList.contains("over")) return;
      document.querySelectorAll(".lane.over").forEach((l) => leaveLane(l));
      lane.classList.add("over");
      // The zone grows, says where the app goes, and holds a preview of the card.
      zone.hidden = false;
      zone.classList.add("active");
      zone.replaceChildren(appCard(app, true), h("span", { class: "dz-text" }, `Drop to move to ${c.name}`));
    });
    lane.addEventListener("dragleave", (e) => { if (!lane.contains(e.relatedTarget)) leaveLane(lane); });
    lane.addEventListener("drop", (e) => {
      e.preventDefault();
      const exe = dragExe;
      leaveLane(lane);
      if (exe) moveApp(exe, i);
    });
    lanes.append(lane);
  });
  const playing = apps.filter((a) => a.active).length;
  root.replaceChildren(
    viewHead("Apps", "Drag an app onto a channel. Windows remembers where you put it.",
      h("span", { class: "pill" }, h("span", { class: "dot" }), h("b", {}, `${playing} playing`))),
    apps.length ? lanes : h("p", { class: "hint" }, "No apps are playing audio right now. Start something and it appears here."));
  laneSignature = signature();
}

function leaveLane(lane) {
  lane.classList.remove("over");
  const zone = lane.querySelector(".dropzone");
  if (!zone) return;
  zone.classList.remove("active");
  zone.replaceChildren(h("span", { class: "dz-text" }, "Drag an app here"));
  zone.hidden = lane.querySelectorAll(".appcard:not(.ghost)").length > 0;
}

const MOVE_ICON = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 5h9M9.5 2.5 12 5 9.5 7.5M13 11H4M6.5 8.5 4 11l2.5 2.5"/></svg>';

/** An app card; `ghost` is the tilted preview shown in a drop zone while dragging. */
function appCard(app, ghost = false) {
  const where = channelOf(app);
  const card = h("div", { class: "appcard" + (ghost ? " ghost" : ""), draggable: ghost ? null : "true", "data-exe": ghost ? null : app.exe },
    appIcon(app, "icon"),
    h("div", { class: "app-text" },
      h("div", { class: "app-line" }, h("span", { class: "title", title: app.name }, app.name), h("em", { class: "exe" }, app.exe)),
      h("div", { class: "app-line sub" },
        where.chosen ? null : h("span", { class: "badge", title: "Not placed yet: follows the Windows default, Game" }, "Default"),
        h("span", { class: "level num silent", "data-level-db": ghost ? null : app.exe }, "−∞"))));
  if (ghost) return card;
  const move = h("button", { type: "button", class: "iconbtn move", title: `Move ${app.name} to another channel`, "aria-label": `Move ${app.name}`, "aria-haspopup": "menu" });
  move.innerHTML = MOVE_ICON;
  move.addEventListener("click", (e) => { e.stopPropagation(); openMoveMenu(app, move); });
  card.append(move, h("span", { class: "activity", "data-activity": app.exe, "aria-hidden": "true" }, appSegments(false), appSegments(true)));
  card.addEventListener("dragstart", (e) => {
    closeMoveMenu();
    dragExe = app.exe;
    e.dataTransfer.setData("text/plain", app.exe);
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setDragImage(NO_DRAG_IMAGE, 0, 0);
    // Styled as a placeholder only after the browser has taken its (empty) snapshot.
    requestAnimationFrame(() => card.classList.add("dragging"));
  });
  card.addEventListener("dragend", () => {
    dragExe = null;
    card.classList.remove("dragging");
    document.querySelectorAll(".lane.over").forEach((l) => leaveLane(l));
  });
  return card;
}

/** Move menu: the other channels, and "Follow Windows default" for apps that were placed. */
function openMoveMenu(app, anchor) {
  const wasOpen = moveMenu?.anchor === anchor;
  closeMoveMenu();
  if (wasOpen) return;
  const where = channelOf(app);
  const item = (label, channel, color) => {
    const b = h("button", { type: "button", role: "menuitem" }, color ? h("span", { class: "swatch", style: `--c:${color}` }) : null, label);
    b.addEventListener("click", () => { closeMoveMenu(); moveApp(app.exe, channel); });
    return b;
  };
  const menu = h("div", { class: "move-menu", role: "menu", "aria-label": `Move ${app.name}` },
    h("div", { class: "menu-label" }, "Move to"),
    ...CHANNELS.map((c, i) => (i === where.index ? null : item(c.name, i, c.color))),
    where.chosen ? h("hr") : null,
    where.chosen ? item("Follow Windows default", null, null) : null);
  document.body.append(menu);
  const r = anchor.getBoundingClientRect();
  const left = Math.min(r.right - menu.offsetWidth, window.innerWidth - menu.offsetWidth - 8);
  const below = r.bottom + 4 + menu.offsetHeight < window.innerHeight;
  menu.style.left = `${Math.max(8, left)}px`;
  menu.style.top = `${below ? r.bottom + 4 : r.top - 4 - menu.offsetHeight}px`;
  anchor.setAttribute("aria-expanded", "true");
  moveMenu = { menu, anchor };
  menu.querySelector("button")?.focus();
}

function closeMoveMenu() {
  if (!moveMenu) return;
  moveMenu.anchor.removeAttribute("aria-expanded");
  moveMenu.menu.remove();
  moveMenu = null;
}
document.addEventListener("pointerdown", (e) => { if (moveMenu && !moveMenu.menu.contains(e.target) && !moveMenu.anchor.contains(e.target)) closeMoveMenu(); }, true);
document.addEventListener("keydown", (e) => { if (moveMenu && e.key === "Escape") { const a = moveMenu.anchor; closeMoveMenu(); a.focus(); } });
$(".content").addEventListener("scroll", () => closeMoveMenu(), { passive: true });

const signature = () => apps.map((a) => `${a.exe}:${channelOf(a).index}:${channelOf(a).chosen}`).join("|");
function updateLanes() {
  if (dragExe || moveMenu) return;
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
// The chain, left to right. `toggle` is the setting that switches a step on and off; Mic input and
// Virtual mic are the ends of the chain, not steps you can switch off.
const NODES = [
  { id: "input", name: "Mic input", help: "The physical microphone everything below starts from." },
  { id: "denoise", name: "Noise removal", toggle: "denoise", help: "DeepFilterNet3 removes keyboards, fans and background voices while you talk." },
  { id: "gate", name: "Noise gate", toggle: "gate", help: "Cuts room noise between words. Stay quiet for a moment, then set the threshold just above the grey noise bar." },
  { id: "eq", name: "Equalizer", toggle: "eq", help: "Shapes your voice: cut rumble, add clarity." },
  { id: "comp", name: "Compressor", toggle: "compressor", help: "Evens out loud and quiet speech so you stay at one level in Discord." },
  { id: "limiter", name: "Limiter", toggle: "limiter", help: "Catches sudden peaks, like a laugh or a desk bump, at −1 dBFS so the virtual mic never clips. Normal speech passes untouched and it adds no delay." },
  { id: "output", name: "Virtual mic", help: "What Discord, OBS or TeamSpeak hear." },
];
const nodeOn = (n) => !n.toggle || snap.config.mic[n.toggle]?.enabled !== false;
const GATE_MIN = -80, GATE_MAX = 0; // the threshold slider and the input meter share this scale
const gatePct = (db) => clamp((db - GATE_MIN) / (GATE_MAX - GATE_MIN), 0, 1) * 100;

const HEADPHONES_ICON = '<svg class="btn-icon" viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 10V8a5.5 5.5 0 0 1 11 0v2"/><rect x="2" y="9.5" width="3" height="4.5" rx="1"/><rect x="11" y="9.5" width="3" height="4.5" rx="1"/></svg>';
const MIC_ICON = '<svg class="btn-icon" viewBox="0 0 16 16" aria-hidden="true"><rect x="5.5" y="1.5" width="5" height="8" rx="2.5"/><path d="M3 7.5a5 5 0 0 0 10 0M8 12.5V15"/></svg>';

function virtualMicName() {
  const sink = snap.render_devices.find((d) => d.id === snap.config.mic_sink);
  if (!sink) return null;
  return snap.capture_devices.find((d) => d.hardware === sink.hardware && !d.name.includes("16ch"))?.name.replace(/\s*\(.*\)$/, "");
}

function renderMic() {
  const root = $("#view-mic");
  const mic = snap.config.mic;
  const listen = h("button", { type: "button", class: "btn listen", id: "mic-listen" });
  listen.addEventListener("click", () => { mic.monitor = !mic.monitor; sendMic(); renderMicControls(); });
  const mute = h("button", { type: "button", class: "btn mute", id: "mic-mute" });
  mute.addEventListener("click", () => { mic.muted = !mic.muted; sendMic(); renderMicControls(); updateStrips(); });

  const change = h("button", { type: "button", class: "linkish link" }, "Change");
  change.addEventListener("click", () => { setSettingsTab("devices"); setView("settings"); });
  const from = snap.active_mic ? shortName(snap.active_mic) : null;
  const to = virtualMicName();
  root.replaceChildren(
    viewHead("Microphone", "Your voice goes through each step, left to right, before Discord, OBS or TeamSpeak hear it.",
      h("div", { class: "head-actions" }, listen, mute)),
    h("div", { class: "chain-tabs", id: "chain", role: "tablist", "aria-label": "Mic chain" }),
    h("p", { class: "route" },
      "From ", from ? h("b", {}, from) : h("b", { class: "bad" }, "no microphone found"),
      snap.config.mic_device ? "" : " (automatic)", " → To ",
      to ? h("b", {}, to) : h("b", { class: "bad" }, "no Virtual Mic cable"), to ? ", which apps pick as their mic" : "", " ", change),
    h("div", { class: "detail" },
      h("section", { class: "card mic-card", id: "node-detail" }),
      h("div", { class: "mic-side" }, levelsCard(), micTestCard())));
  renderMicControls();
  renderChain();
  renderNodeDetail();
}

function renderMicControls() {
  const mic = snap.config.mic;
  const mute = $("#mic-mute");
  if (mute) {
    mute.setAttribute("aria-pressed", String(mic.muted));
    mute.innerHTML = `${MIC_ICON}<span>${mic.muted ? "Mic muted" : "Mute mic"}</span>`;
  }
  const listen = $("#mic-listen");
  if (listen) {
    listen.setAttribute("aria-pressed", String(mic.monitor));
    listen.innerHTML = `${HEADPHONES_ICON}<span>Listen to my mic</span>`;
    listen.title = mic.monitor ? "Stop hearing yourself" : "Hear your processed mic in your headphones";
  }
}

function renderChain() {
  const chain = $("#chain");
  if (!chain) return;
  chain.replaceChildren(...NODES.map((n) => {
    const b = h("button", { type: "button", role: "tab", class: "chain-tab" + (nodeOn(n) ? "" : " off"), "aria-selected": String(n.id === selectedNode), "data-node": n.id },
      h("span", { class: "cdot", "data-led": n.id }),
      h("span", { class: "ctext" }, h("span", { class: "cname" }, n.name), h("span", { class: "cstate", "data-state": n.id }, "…")));
    b.addEventListener("click", () => { selectedNode = n.id; renderChain(); renderNodeDetail(); });
    return b;
  }));
  if (lastMicMeters) updateMicMeters(lastMicMeters);
}

/** A section of the step card, separated from the one above by a line. */
const micSection = (...children) => h("div", { class: "mic-sec" }, ...children);

/**
 * Compact value field: drag up or down to change it, or type a new value. Used for timings.
 */
function valueField({ label, value, unit, min, max, step, onChange }) {
  const input = h("input", { class: "vf-input num", inputmode: "decimal", "aria-label": `${label} in ${unit}` });
  const round = (v) => Math.round(clamp(v, min, max) / step) * step;
  let v = value;
  const show = () => { input.value = String(+v.toFixed(step < 1 ? 1 : 0)); };
  const commit = (next) => { v = round(next); show(); onChange(v); };
  input.addEventListener("change", () => { const n = parseFloat(input.value.replace(",", ".")); if (Number.isFinite(n)) commit(n); else show(); });
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowUp" || e.key === "ArrowDown") { e.preventDefault(); commit(v + (e.key === "ArrowUp" ? step : -step)); }
    if (e.key === "Enter") input.blur();
  });
  const field = h("label", { class: "vfield" }, h("span", { class: "vf-label" }, label), h("span", { class: "vf-value" }, input, h("span", { class: "vf-unit" }, unit)));
  // Dragging: one step per 3 px, starting from where the pointer went down; a click still types.
  field.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const y0 = e.clientY, v0 = v;
    let moved = false;
    const move = (ev) => {
      const d = Math.round((y0 - ev.clientY) / 3);
      if (!moved && Math.abs(d) < 1) return;
      moved = true;
      ev.preventDefault();
      commit(v0 + d * step);
    };
    const up = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); if (moved) input.blur(); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  });
  show();
  field.set = (next) => { v = next; show(); };
  return field;
}

function renderNodeDetail() {
  const pane = $("#node-detail");
  if (!pane) return;
  const mic = snap.config.mic;
  const n = NODES.find((x) => x.id === selectedNode);
  // One switch per step, in the card header, labelled with its state.
  let toggle = null;
  if (n.toggle) {
    const on = mic[n.toggle].enabled;
    toggle = switchEl(on ? "On" : "Off", on, (v) => {
      mic[n.toggle].enabled = v;
      toggle.el.lastChild.textContent = v ? "On" : "Off";
      pane.classList.toggle("off", !v);
      sendMic(); renderChain(); updateStrips();
    });
    toggle.input.setAttribute("aria-label", `${n.name} on or off`);
  }
  const body = [];
  if (n.id === "input") {
    const settings = h("button", { type: "button", class: "btn" }, "Change in Settings");
    settings.addEventListener("click", () => { setSettingsTab("devices"); setView("settings"); });
    body.push(micSection(h("p", { class: "hint" },
      snap.active_mic ? `Recording from ${snap.active_mic}.` : "No microphone found. Connect one, or pick a specific device in Settings.",
      " A mic you plug in later is picked up automatically."), settings));
  }
  if (n.id === "denoise") {
    const d = mic.denoise;
    const ll = switchEl("Low-latency model", d.low_latency, (v) => { d.low_latency = v; sendMic(); renderNodeDetail(); });
    body.push(
      micSection(slider({ label: "Strength", min: 6, max: 100, value: d.strength_db, format: (v) => (v >= 100 ? "Max" : `${v} dB`), onInput: (v) => { d.strength_db = v; sendMic(); } })),
      micSection(slider({ label: "Residual cleanup", min: 0, max: 0.05, step: 0.005, value: d.post_filter, format: (v) => (v === 0 ? "Off" : v.toFixed(3)), onInput: (v) => { d.post_filter = v; sendMic(); } })),
      micSection(ll.el, h("p", { class: "hint" }, d.low_latency ? "Model delay 10 ms. Slightly less clean and uses more CPU." : "Model delay 30 ms. The low-latency model saves 20 ms.")));
  }
  if (n.id === "gate") {
    const g = mic.gate;
    const out = h("output", { class: "num" }, fmtDbInt(g.threshold_db));
    const range = h("input", { type: "range", min: String(GATE_MIN), max: String(GATE_MAX), step: "1", value: String(g.threshold_db), "aria-label": "Gate threshold" });
    const track = h("div", { class: "gate-track", id: "gate-track" },
      h("i", { class: "gate-open" }), h("i", { class: "gate-level" }), h("i", { class: "gate-mark" }), range);
    const placeThreshold = () => track.style.setProperty("--th", `${gatePct(g.threshold_db)}%`);
    range.addEventListener("input", () => { g.threshold_db = range.valueAsNumber; out.textContent = fmtDbInt(g.threshold_db); placeThreshold(); placeGate(); sendMic(); });
    placeThreshold();
    const defaults = { attack_ms: 2, hold_ms: 150, release_ms: 120 };
    const fields = {
      attack_ms: valueField({ label: "Attack", value: g.attack_ms, unit: "ms", min: 0.5, max: 50, step: 0.5, onChange: (v) => { g.attack_ms = v; sendMic(); } }),
      hold_ms: valueField({ label: "Hold", value: g.hold_ms, unit: "ms", min: 0, max: 1000, step: 10, onChange: (v) => { g.hold_ms = v; sendMic(); } }),
      release_ms: valueField({ label: "Release", value: g.release_ms, unit: "ms", min: 10, max: 1000, step: 10, onChange: (v) => { g.release_ms = v; sendMic(); } }),
    };
    const reset = h("button", { type: "button", class: "linkish" }, "Reset to defaults");
    reset.addEventListener("click", () => { Object.assign(g, defaults); for (const [k, f] of Object.entries(fields)) f.set(g[k]); sendMic(); });
    body.push(
      micSection(h("div", { class: "ctl" }, h("label", {}, "Threshold"), out), track,
        h("div", { class: "gate-caption" }, h("span", { id: "gate-caption" }, h("i", { class: "swatch" }), "Room noise –"), h("span", { class: "num" }, "−80 … 0"))),
      micSection(slider({ label: "How much to cut when closed", min: -80, max: -3, value: g.range_db, format: fmtDbInt, onInput: (v) => { g.range_db = v; sendMic(); } })),
      micSection(h("div", { class: "sec-head" }, h("span", {}, "Timing"), reset),
        h("div", { class: "vfields" }, fields.attack_ms, fields.hold_ms, fields.release_ms),
        h("p", { class: "hint small" }, "Drag a value up or down, or type a new one.")));
  }
  if (n.id === "eq") {
    body.push(micSection(eqEditor(mic.eq, () => { sendMic(); renderChain(); }, { showSwitch: false })));
  }
  if (n.id === "comp") {
    const c = mic.compressor;
    body.push(
      micSection(slider({ label: "Threshold", min: -60, max: 0, value: c.threshold_db, format: fmtDbInt, onInput: (v) => { c.threshold_db = v; sendMic(); } })),
      micSection(slider({ label: "Ratio", min: 1, max: 20, step: 0.1, value: c.ratio, format: (v) => `${v.toFixed(1)}:1`, onInput: (v) => { c.ratio = v; sendMic(); } })),
      micSection(slider({ label: "Knee", min: 0, max: 24, value: c.knee_db, format: fmtDbInt, onInput: (v) => { c.knee_db = v; sendMic(); } })),
      micSection(slider({ label: "Makeup gain", min: 0, max: 24, step: 0.5, value: c.makeup_db, format: fmtDbInt, onInput: (v) => { c.makeup_db = v; sendMic(); } })),
      micSection(h("div", { class: "sec-head" }, h("span", {}, "Timing")),
        h("div", { class: "vfields two" },
          valueField({ label: "Attack", value: c.attack_ms, unit: "ms", min: 0.1, max: 100, step: 0.1, onChange: (v) => { c.attack_ms = v; sendMic(); } }),
          valueField({ label: "Release", value: c.release_ms, unit: "ms", min: 10, max: 1000, step: 10, onChange: (v) => { c.release_ms = v; sendMic(); } })),
        h("p", { class: "hint small" }, "Drag a value up or down, or type a new one.")));
  }
  if (n.id === "limiter") {
    body.push(micSection(h("div", { class: "ctl" }, h("label", {}, "Turning peaks down by"), h("output", { class: "num", id: "limiter-now" }, "0.0 dB")),
      mic.limiter?.enabled === false ? h("p", { class: "hint warn" }, "Off: loud peaks can clip and distort what others hear.") : null));
  }
  if (n.id === "output") {
    body.push(
      micSection(h("p", { class: "hint" }, `In Discord, OBS or TeamSpeak choose ${virtualMicName() || "the Virtual Mic cable's Output"} as the microphone, and turn off their own noise suppression.`)),
      micSection(slider({ label: "Output gain", min: -20, max: 20, step: 0.5, value: mic.gain_db, format: fmtDbInt, onInput: (v) => { mic.gain_db = v; sendMic(); updateStrips(); } })));
  }
  pane.classList.toggle("off", Boolean(n.toggle) && !mic[n.toggle].enabled);
  pane.replaceChildren(
    h("div", { class: "card-head row" }, h("div", {}, h("h3", {}, n.name), h("p", {}, n.help)), toggle ? toggle.el : null),
    ...body);
  if (lastMicMeters) updateMicMeters(lastMicMeters);
}

// ---------- levels ----------
function levelBar(label, id, { marker = false, reverse = false } = {}) {
  return h("div", { class: "lvl" },
    h("div", { class: "lvl-head" }, h("span", {}, label), h("span", { class: "num", id: `${id}-db` }, "–")),
    h("div", { class: "lvl-bar" + (reverse ? " gr" : ""), id }, h("i", { class: "lit" }), marker ? h("i", { class: "gmark", id: "gate-marker" }) : null));
}

function levelsCard() {
  return h("section", { class: "card mic-levels" },
    h("div", { class: "card-head" }, h("h3", {}, "Levels")),
    h("div", { class: "lvl-list" },
      levelBar("Mic input", "mic-in", { marker: true }),
      levelBar("Virtual mic output", "mic-out"),
      levelBar("Compressor gain reduction", "mic-gr", { reverse: true }),
      h("div", { class: "hscale", "aria-hidden": "true" }, ["−60", "−48", "−36", "−24", "−12", "0 dB"].map((t) => h("span", {}, t))),
      h("p", { class: "legend" }, h("i", { class: "gmark-key" }), "Gate threshold")));
}

function placeGate() {
  const marker = $("#gate-marker");
  if (marker) marker.style.setProperty("--m", `${meterPct(snap.config.mic.gate.threshold_db)}%`);
  marker?.toggleAttribute("hidden", !snap.config.mic.gate.enabled);
}

let lastMicMeters = null;
function updateMicMeters(m) {
  lastMicMeters = m;
  const mic = snap.config.mic;
  const set = (id, pct, text) => {
    const bar = $(`#${id} .lit`);
    if (bar) bar.style.setProperty("--lvl", `${pct}%`);
    const label = $(`#${id}-db`);
    if (label) label.textContent = text;
  };
  const missing = snap.status.Microphone !== undefined && snap.status.Microphone !== "running";
  set("mic-in", missing ? 0 : meterPct(m.input_db), missing ? "–" : `${fmtDb(m.input_db)} dB`);
  set("mic-out", missing ? 0 : meterPct(m.output_db), missing ? "–" : `${fmtDb(m.output_db)} dB`);
  set("mic-gr", clamp(-m.gain_reduction_db / 20, 0, 1) * 100, `${fmtDb(m.gain_reduction_db)} dB`);
  placeGate();
  // Noise gate card: the threshold track doubles as the live input meter.
  const track = $("#gate-track");
  if (track) track.style.setProperty("--lvl", `${missing ? 0 : gatePct(m.input_db)}%`);
  const caption = $("#gate-caption");
  if (caption) caption.lastChild.textContent = missing ? "No microphone: nothing to measure"
    : !mic.gate.enabled ? `Level ${fmtDb(m.input_db)} dB · the gate is off`
    : m.gate_open ? `Level ${fmtDb(m.input_db)} dB, so the gate is open`
    : `Room noise ${fmtDb(m.input_db)} dB, so the gate is closed`;
  const limiterNow = $("#limiter-now");
  if (limiterNow) limiterNow.textContent = `${fmtDb(m.limiter_db)} dB`;

  // Chain tabs: a status line and a dot per step. "warn" marks a step that isn't ready.
  const states = {
    input: missing ? ["No microphone", "warn"] : [`${fmtDb(m.input_db)} dBFS`, "ok"],
    denoise: !mic.denoise.enabled ? ["Off", "off"] : m.denoise_ready ? [`−${Math.max(0, m.noise_reduction_db).toFixed(0)} dB noise`, "ok"] : ["Loading model…", "warn"],
    gate: !mic.gate.enabled ? ["Off", "off"] : [m.gate_open ? "Open" : "Closed", "ok"],
    eq: mic.eq.enabled ? [mic.eq.preset, "ok"] : ["Off", "off"],
    comp: mic.compressor.enabled ? [`${fmtDb(m.gain_reduction_db)} dB`, "ok"] : ["Off", "off"],
    limiter: mic.limiter?.enabled === false ? ["Off", "off"] : [m.limiter_db < -0.05 ? `${fmtDb(m.limiter_db)} dB` : "Idle", "ok"],
    output: mic.muted ? ["Muted", "warn"] : missing ? ["Silent", "warn"] : [`${fmtDb(m.output_db)} dBFS`, "ok"],
  };
  for (const [id, [text, kind]] of Object.entries(states)) {
    const el = document.querySelector(`[data-state="${id}"]`);
    if (el) { el.textContent = text; el.className = `cstate ${kind}`; }
    const dot = document.querySelector(`[data-led="${id}"]`);
    if (dot) dot.className = `cdot ${kind}`;
  }
}

// ---------- mic test ----------
function micTestCard() {
  const button = (id, label, action, cls = "btn") => {
    const b = h("button", { type: "button", class: cls, id }, label);
    b.addEventListener("click", () => invoke("mic_test", { action: b.dataset.action || action }).catch(showError));
    return b;
  };
  return h("section", { class: "card mictest", id: "mic-test" },
    h("div", { class: "card-head" }, h("h3", {}, "Test your mic"),
      h("p", { id: "mic-test-hint" }, "Record 5 seconds, then compare the filtered and original versions in your headphones.")),
    h("div", { class: "mictest-body" },
      h("div", { class: "mictest-row" },
        button("mic-test-record", "● Record 5 s", "record", "btn record"),
        button("mic-test-play", "▶ Filtered", "play"),
        button("mic-test-original", "▶ Original", "play_original")),
      h("div", { class: "mictest-bar", "aria-hidden": "true" }, h("i", { id: "mic-test-progress" })),
      h("p", { class: "hint small", id: "mic-test-note" }, "Play buttons unlock after recording.")));
}

function updateMicTest(t) {
  const panel = $("#mic-test");
  if (!panel || !t) return;
  const muted = snap.config.mic.muted;
  const busy = t.phase !== "idle";
  const record = $("#mic-test-record");
  // While recording or playing, the record button stops it.
  record.dataset.action = busy ? "stop" : "record";
  record.textContent = t.phase === "recording" ? `■ Stop · ${Math.ceil(5 * (1 - t.progress))} s` : t.phase === "playing" ? "■ Stop" : "● Record 5 s";
  record.disabled = !busy && muted;
  $("#mic-test-play").disabled = busy || !t.recorded;
  $("#mic-test-original").disabled = busy || !t.recorded;
  $("#mic-test-progress").style.width = `${busy ? t.progress * 100 : 0}%`;
  panel.dataset.phase = t.phase;
  $("#mic-test-note").textContent = muted && !busy ? "Your mic is muted. Unmute it to record a test."
    : t.phase === "recording" ? "Speak normally, the way you would in Discord."
    : t.phase === "playing" ? (t.original ? "Playing your mic without any filters." : "Playing with every filter applied, as others hear you.")
    : t.recorded ? "Compare the two, or record again." : "Play buttons unlock after recording.";
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
  if (settingsTab === "devices") renderDelay();
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
/** A switch for an on/off setting the backend stores as is ("master_per_output", "auto_update"). */
function preferenceSwitch(name, label, after) {
  const sw = switchEl("", !!snap.config[name], async (on) => {
    try {
      await invoke("set_preference", { name, enabled: on });
      snap.config[name] = on;
      after?.();
    } catch (e) { showError(e); sw.input.checked = !on; }
  });
  sw.input.setAttribute("aria-label", label);
  return sw.el;
}

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
      settingRow({ label: "Separate master for each device", fit: true,
        help: "Each pair of headphones or speakers keeps its own master volume and EQ, and gets them back when it plays again.",
        control: preferenceSwitch("master_per_output", "Separate master for each device") }).row,
      deviceRow({ key: "mic", label: "Microphone", list: physical(snap.capture_devices), empty: "Automatic (your usual default)",
        help: () => deviceMissing(snap.status.Microphone)
          ? { text: "No microphone found. Connect one, or pick a specific device.", bad: true }
          : "Filtered by the chain in the Mic tab, then sent to the Virtual Mic." })),
    card({ title: "Virtual cables", sub: "Each channel runs through one VB-Audio cable. Pick the side Smowaudio listens on — the other side is set up for you. Changes apply immediately and briefly restart audio." },
      deviceRow({ key: "mic_sink", label: cableLabelEl("Virtual mic", "var(--mic)"), list: cables(snap.render_devices), empty: "None", compact: true,
        help: () => { const side = pairedSide(snap.render_devices, snap.config.mic_sink); return side ? ["Apps pick ", code(side), " as their mic"] : "No cable: apps have no Virtual Mic"; } }),
      ...CHANNELS.map((c, i) => deviceRow({ key: `source${i}`, label: cableLabelEl(c.name, c.color), list: cables(snap.capture_devices), empty: "None", compact: true, help: channelHelp(i) }))),
    delayCard(),
  ];
}

// ---------- settings: delay ----------
// Results survive re-renders and tab switches until the app window closes.
let delayResults = null; // [{ channel, cable_ms, engine_ms, error }]
let delayMeasuredAt = null;
let delayRunning = null; // channel index being measured, or -1 while starting

function delayCard() {
  const button = h("button", { type: "button", class: "btn primary", id: "delay-measure" }, "Measure");
  button.addEventListener("click", measureDelay);
  return h("section", { class: "card delay", id: "delay-card" },
    h("div", { class: "card-head row" },
      h("div", {}, h("h3", {}, "Delay"),
        h("p", {}, "How long sound takes from an app to your output device, through VB-Cable and Smowaudio. Measuring plays three short, quiet beeps on each channel.")),
      button),
    h("div", { id: "delay-body" }),
    h("p", { class: "delay-note" }, "Your headphones or speakers add their own delay after this, which Windows can't measure: Bluetooth usually 100–250 ms, wired headphones and USB about 1–10 ms."));
}

function renderDelay() {
  const body = $("#delay-body");
  const button = $("#delay-measure");
  if (!body || !button) return;
  const running = delayRunning !== null;
  button.disabled = running;
  button.textContent = running ? (delayRunning >= 0 ? `Measuring ${CHANNELS[delayRunning].name}…` : "Starting…") : delayResults ? "Measure again" : "Measure";
  if (!delayResults) {
    body.replaceChildren(h("div", { class: "delay-empty" }, running ? "Listening for the beeps…" : "Not measured yet."));
    return;
  }
  const ok = delayResults.filter((r) => r.error === null);
  const longest = Math.max(1, ...ok.map((r) => r.cable_ms + r.engine_ms));
  const ms = (v) => `${Math.round(v)} ms`;
  body.replaceChildren(
    h("div", { class: "delay-legend" },
      h("span", {}, h("i", { class: "k cable" }), "VB-Cable"),
      h("span", {}, h("i", { class: "k engine" }), "Smowaudio + Windows"),
      h("span", { class: "spacer" }),
      delayMeasuredAt ? h("span", { class: "when" }, `measured ${ago(delayMeasuredAt)}`) : null),
    ...delayResults.map((r) => {
      const c = CHANNELS[r.channel];
      if (r.error !== null) {
        return h("div", { class: "delay-row" }, cableLabelEl(c.name, c.color), h("span", { class: "delay-error" }, r.error));
      }
      const total = r.cable_ms + r.engine_ms;
      return h("div", { class: "delay-row", title: `VB-Cable ${ms(r.cable_ms)} + Smowaudio and Windows ${ms(r.engine_ms)}` },
        cableLabelEl(c.name, c.color),
        h("span", { class: "delay-bar", "aria-hidden": "true" },
          h("i", { class: "cable", style: `width:${(r.cable_ms / longest) * 100}%` }),
          h("i", { class: "engine", style: `width:${(r.engine_ms / longest) * 100}%` })),
        h("span", { class: "delay-parts num" }, `${ms(r.cable_ms)} + ${ms(r.engine_ms)}`),
        h("b", { class: "delay-total num" }, ms(total)));
    }));
}

async function measureDelay() {
  delayRunning = -1;
  renderDelay();
  try {
    delayResults = await invoke("measure_delay");
    delayMeasuredAt = Date.now();
    if (!delayResults.length) toast("No channel has a cable to measure. Pick cables above.", true);
  } catch (e) {
    showError(e);
  }
  delayRunning = null;
  renderDelay();
}
listen("delay-progress", (e) => { if (delayRunning !== null) { delayRunning = e.payload; renderDelay(); } });

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
    settingRow({ label: "Install updates automatically", fit: true,
      help: "Only while the window is closed and nothing is playing: right after Smowaudio starts, or once it's been quiet for 2 minutes. It comes back in the tray.",
      control: preferenceSwitch("auto_update", "Install updates automatically", renderUpdates) }).row,
    h("div", { class: "update-extra", id: "update-extra", hidden: true },
      h("div", { class: "update-progress", id: "update-progress", hidden: true }, h("i"))));
  const whatsNew = h("section", { class: "card", id: "whats-new", hidden: true },
    h("div", { class: "card-head" }, h("h3", { id: "whats-new-title" }, "What's new")),
    h("div", { class: "changes", id: "whats-new-body" }));
  const history = h("section", { class: "card", id: "changelog-card" },
    h("div", { class: "card-head" }, h("h3", {}, "Changelog"), h("p", {}, "What changed in each version.")),
    h("div", { class: "changes", id: "changelog-body" }));
  if (!changelog) {
    invoke("changelog").then((md) => { changelog = parseChangelog(md); renderChangelog(); }).catch(() => {});
  }
  return [version, whatsNew, history];
}

// ---------- settings: changelog ----------
// CHANGELOG.md is built into the app for the history; an update's manifest carries the sections
// of the versions it brings, in the same format.
let changelog = null; // [{ version, date, items }]
let changelogAll = false;
const CHANGELOG_SHOWN = 5;

function parseChangelog(md) {
  const entries = [];
  for (const line of md.split(/\r?\n/)) {
    const head = line.match(/^##\s+v?(\d+\.\d+\.\d+)\s*(?:[—–-]\s*(\d{4}-\d{2}-\d{2}))?/);
    if (head) entries.push({ version: head[1], date: head[2] ?? null, items: [] });
    else if (entries.length && /^\s*[-*]\s+/.test(line)) entries.at(-1).items.push(line.replace(/^\s*[-*]\s+/, "").trim());
  }
  return entries;
}

/** Compares "1.2.3" versions: negative when a is older. */
function compareVersions(a, b) {
  const pa = a.split(".").map(Number), pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) if (pa[i] !== pb[i]) return (pa[i] ?? 0) - (pb[i] ?? 0);
  return 0;
}

function formatDate(iso) {
  if (!iso) return null;
  const [y, m, d] = iso.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

function changeEntry(e, badge) {
  return h("article", { class: "change" },
    h("header", {},
      h("b", { class: "num" }, e.version),
      badge ? h("span", { class: "badge" }, badge) : null,
      e.date ? h("time", { datetime: e.date }, formatDate(e.date)) : null),
    h("ul", {}, ...e.items.map((item) => h("li", {}, item))));
}

function renderChangelog() {
  const body = $("#changelog-body");
  if (!body || !changelog) return;
  const current = updateStatus?.current;
  const shown = changelogAll ? changelog : changelog.slice(0, CHANGELOG_SHOWN);
  const more = changelog.length - shown.length;
  const toggle = more > 0 || changelogAll
    ? h("button", { type: "button", class: "btn changes-more", id: "changelog-more" }, changelogAll ? "Show fewer" : `Show ${more} older version${more === 1 ? "" : "s"}`)
    : null;
  toggle?.addEventListener("click", () => { changelogAll = !changelogAll; renderChangelog(); });
  body.replaceChildren(...[...shown.map((e) => changeEntry(e, e.version === current ? "Installed" : null)), toggle].filter(Boolean));
}

function renderWhatsNew() {
  const card = $("#whats-new");
  if (!card) return;
  const st = updateStatus;
  if (!st?.available) { card.hidden = true; return; }
  const notes = st.notes ?? "";
  const entries = parseChangelog(notes).filter((e) => compareVersions(e.version, st.current) > 0);
  // Notes from before the changelog existed are plain text; skip GitHub's "Full Changelog" link.
  const plain = notes.split(/\r?\n/).filter((l) => l.trim() && !/Full Changelog/i.test(l)).join("\n");
  card.hidden = !entries.length && !plain;
  $("#whats-new-title").textContent = entries.length > 1 ? `What's new since ${st.current}` : `What's new in ${st.available}`;
  $("#whats-new-body").replaceChildren(...(entries.length
    ? entries.map((e) => changeEntry(e, e.version === st.available ? "New" : null))
    : [h("p", { class: "change-plain" }, plain)]));
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
  const auto = snap?.config.auto_update ? " · installs itself once this window is closed and nothing is playing" : "";
  const [text, cls] = st.available ? [`Version ${st.available} is available${auto}`, "ok"]
    : st.error ? [st.error, "bad"]
    : st.checked ? [`You're up to date${st.checked_at ? ` · checked ${ago(st.checked_at)}` : ""}`, "ok"]
    : ["Not checked yet", ""];
  state.textContent = text;
  state.className = "srow-help" + (cls ? ` ${cls}` : "");
  $("#update-install").hidden = !st.available;
  $("#update-install").disabled = installing;
  $("#update-check").disabled = installing;
  $("#update-extra").hidden = $("#update-progress").hidden;
  renderWhatsNew();
  renderChangelog();
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
    ["mic.limiter", "Limiter on/off"],
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
