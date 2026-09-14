const { invoke } = window.__TAURI__.core;

// Same channels as SteelSeries Sonar; Game doubles as the Windows default (system sounds).
const CHANNELS = ["Game", "Chat", "Media", "Aux"];
const BAND_COLORS = ["#f472b6", "#fbbf24", "#34d399", "#22d3ee", "#818cf8", "#c084fc", "#fb7185", "#a3e635", "#38bdf8", "#f97316"];
let snapshot = null;
const meterEls = {}; // key -> {fill, text}

// ---------- helpers ----------
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v !== undefined && v !== null && v !== false) el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c !== null && c !== undefined && c !== false) el.append(c instanceof Node ? c : document.createTextNode(c));
  }
  return el;
}

/** Coalesces rapid slider changes into one backend call per key every 40 ms. */
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

function showError(e) {
  const banner = document.getElementById("banner");
  banner.textContent = String(e);
  banner.hidden = false;
  setTimeout(() => (banner.hidden = !needsSetup()), 6000);
}

function slider({ label, min, max, step = 1, value, unit = "", format, onInput }) {
  const fmt = format || ((v) => `${Number(v).toFixed(step < 1 ? 1 : 0)}${unit}`);
  const out = h("output", {}, fmt(value));
  const input = h("input", { type: "range", min, max, step, value });
  input.addEventListener("input", () => {
    out.textContent = fmt(input.valueAsNumber);
    onInput(input.valueAsNumber);
  });
  return h("div", { class: "slider" }, h("label", {}, label), out, input);
}

function toggle(checked, onChange) {
  const input = h("input", { type: "checkbox" });
  input.checked = checked;
  input.addEventListener("change", () => onChange(input.checked));
  return h("label", { class: "switch" }, input, h("span"));
}

function meter(key, { marker } = {}) {
  const fill = h("div", { class: "fill" });
  const el = h("div", { class: "meter" + (key.endsWith("gr") ? " gr" : "") }, fill);
  if (marker !== undefined) {
    const m = h("div", { class: "marker" });
    m.style.left = `${dbToPct(marker)}%`;
    el.append(m);
    el.marker = m;
  }
  meterEls[key] = { fill, el };
  return el;
}

const dbToPct = (db) => Math.max(0, Math.min(100, ((db + 60) / 60) * 100));

function card(title, { enabled, onToggle, wide, extra } = {}, ...body) {
  const bodyEl = h("div", { class: "body" }, ...body);
  bodyEl.style.display = "contents";
  const el = h("div", { class: "card" + (wide ? " wide" : "") + (enabled === false ? " disabled" : "") },
    h("h3", {}, title, h("span", { class: "spacer" }), extra || null,
      onToggle ? toggle(enabled, (v) => { el.classList.toggle("disabled", !v); onToggle(v); }) : null),
    bodyEl);
  return el;
}

// ---------- EQ editor ----------
function biquadDb(band, f) {
  if (!band.enabled) return 0;
  const fs = 48000, w0 = (2 * Math.PI * Math.min(band.freq, fs * 0.49)) / fs;
  const A = Math.pow(10, band.gain_db / 40), cos = Math.cos(w0), sin = Math.sin(w0);
  const alpha = sin / (2 * Math.max(band.q, 0.05)), sa = 2 * Math.sqrt(A) * alpha;
  let b0, b1, b2, a0, a1, a2;
  switch (band.kind) {
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

function eqEditor(eq, onChange) {
  const canvas = h("canvas");
  const RANGE = 18;
  const fx = (f, W) => (Math.log10(f / 20) / 3) * W;
  const xf = (x, W) => 20 * Math.pow(10, (x / W) * 3);
  const gy = (g, H) => H / 2 - (g / RANGE) * (H / 2 - 10);
  const yg = (y, H) => ((H / 2 - y) / (H / 2 - 10)) * RANGE;
  const hasGain = (b) => b.kind !== "low_pass" && b.kind !== "high_pass";

  function draw() {
    const dpr = window.devicePixelRatio || 1;
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (!W) return;
    canvas.width = W * dpr; canvas.height = H * dpr;
    const ctx = canvas.getContext("2d");
    ctx.scale(dpr, dpr);
    ctx.strokeStyle = "#262c38"; ctx.fillStyle = "#8b93a3"; ctx.font = "10px Segoe UI"; ctx.lineWidth = 1;
    for (const f of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
      const x = fx(f, W); ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, H); ctx.stroke();
      ctx.fillText(f >= 1000 ? `${f / 1000}k` : f, x + 3, H - 4);
    }
    for (const g of [-12, -6, 0, 6, 12]) {
      const y = gy(g, H); ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke();
      ctx.fillText(`${g > 0 ? "+" : ""}${g}`, 3, y - 3);
    }
    ctx.beginPath(); ctx.lineWidth = 2; ctx.strokeStyle = eq.enabled ? "#22d3ee" : "#8b93a3";
    for (let x = 0; x <= W; x += 2) {
      const f = xf(x, W);
      const g = eq.bands.reduce((s, b) => s + biquadDb(b, f), 0);
      x === 0 ? ctx.moveTo(x, gy(g, H)) : ctx.lineTo(x, gy(Math.max(-RANGE - 2, Math.min(RANGE + 2, g)), H));
    }
    ctx.stroke();
    eq.bands.forEach((b, i) => {
      ctx.beginPath(); ctx.fillStyle = b.enabled ? BAND_COLORS[i] : "#4b5263";
      ctx.arc(fx(b.freq, W), gy(hasGain(b) ? b.gain_db : 0, H), 6, 0, Math.PI * 2); ctx.fill();
    });
  }

  let dragging = -1;
  canvas.addEventListener("pointerdown", (e) => {
    const W = canvas.clientWidth, H = canvas.clientHeight;
    let best = -1, bestD = 16;
    eq.bands.forEach((b, i) => {
      const d = Math.hypot(fx(b.freq, W) - e.offsetX, gy(hasGain(b) ? b.gain_db : 0, H) - e.offsetY);
      if (d < bestD) { best = i; bestD = d; }
    });
    dragging = best;
    if (best >= 0) canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (dragging < 0) return;
    const W = canvas.clientWidth, H = canvas.clientHeight, b = eq.bands[dragging];
    b.freq = Math.round(Math.max(20, Math.min(20000, xf(e.offsetX, W))));
    if (hasGain(b)) b.gain_db = Math.round(Math.max(-RANGE, Math.min(RANGE, yg(e.offsetY, H))) * 2) / 2;
    changed();
  });
  canvas.addEventListener("pointerup", () => { dragging = -1; renderRows(); });
  canvas.addEventListener("wheel", (e) => {
    const W = canvas.clientWidth, H = canvas.clientHeight;
    const i = eq.bands.findIndex((b) => Math.hypot(fx(b.freq, W) - e.offsetX, gy(hasGain(b) ? b.gain_db : 0, H) - e.offsetY) < 20);
    if (i < 0) return;
    e.preventDefault();
    const b = eq.bands[i];
    b.q = Math.round(Math.max(0.1, Math.min(10, b.q * (e.deltaY < 0 ? 1.1 : 0.9))) * 100) / 100;
    changed(); renderRows();
  }, { passive: false });

  const rows = h("div", { class: "eq-bands" });
  function num(value, min, max, step, onInput) {
    const input = h("input", { type: "number", min, max, step, value });
    input.addEventListener("input", () => { if (!isNaN(input.valueAsNumber)) { onInput(input.valueAsNumber); changed(); } });
    return input;
  }
  function renderRows() {
    rows.replaceChildren(...eq.bands.map((b, i) => {
      const kind = h("select", {}, ...[["low_shelf", "Low shelf"], ["peaking", "Peak"], ["high_shelf", "High shelf"], ["low_pass", "Low pass"], ["high_pass", "High pass"]]
        .map(([v, l]) => { const o = h("option", { value: v }, l); o.selected = b.kind === v; return o; }));
      kind.addEventListener("change", () => { b.kind = kind.value; changed(); });
      const sw = h("span", { class: "swatch" }); sw.style.background = BAND_COLORS[i];
      return h("div", { class: "eq-band" },
        h("div", { class: "row" }, toggle(b.enabled, (v) => { b.enabled = v; changed(); }), sw),
        kind,
        num(b.freq, 20, 20000, 1, (v) => (b.freq = v)),
        num(b.gain_db, -18, 18, 0.5, (v) => (b.gain_db = v)),
        num(b.q, 0.1, 10, 0.05, (v) => (b.q = v)));
    }));
  }

  function changed() { draw(); onChange(); }

  renderRows();
  new ResizeObserver(draw).observe(canvas);
  return h("div", { class: "eq" }, canvas,
    h("div", { class: "eq-band hint" }, h("span"), h("span", {}, "Type"), h("span", {}, "Freq (Hz)"), h("span", {}, "Gain (dB)"), h("span", {}, "Q")),
    rows,
    h("p", { class: "hint" }, "Drag points to set frequency and gain; scroll over a point to change Q."));
}

// ---------- Mixer ----------
function renderMixer() {
  const root = document.getElementById("tab-mixer");
  const cfg = snapshot.config;
  const cards = CHANNELS.map((name, i) => {
    const ch = cfg.channels[i];
    const s = ch.settings;
    const push = () => send(`ch${i}`, () => invoke("set_channel", { index: i, settings: s }));
    if (!ch.source) {
      return card(name, {}, h("p", { class: "hint" }, "No virtual cable assigned. Pick one in Settings."));
    }
    const volText = h("span", { class: "big-volume" }, `${Math.round(s.volume * 100)}%`);
    const mute = h("button", { class: "btn" + (s.muted ? " on" : ""), onclick: () => {
      s.muted = !s.muted; mute.classList.toggle("on", s.muted); mute.textContent = s.muted ? "Muted" : "Mute"; push();
    } }, s.muted ? "Muted" : "Mute");
    const eqWrap = h("div");
    eqWrap.hidden = true;
    const eqBtn = h("button", { class: "btn", onclick: () => {
      eqWrap.hidden = !eqWrap.hidden;
      if (!eqWrap.firstChild) eqWrap.append(eqEditor(s.eq, push));
    } }, "EQ");
    return card(name, { extra: h("span", { class: "hint" }, s.eq.enabled ? "EQ on" : "") },
      h("div", { class: "row" }, volText, h("span", { class: "grow" }), eqBtn, mute),
      meter(`ch${i}`),
      slider({ label: "Volume", min: 0, max: 150, value: Math.round(s.volume * 100), unit: "%",
        onInput: (v) => { s.volume = v / 100; volText.textContent = `${v}%`; push(); } }),
      h("div", { class: "row" }, h("span", { class: "hint grow" }, "Equalizer"),
        toggle(s.eq.enabled, (v) => { s.eq.enabled = v; push(); })),
      eqWrap);
  });
  cards.push(card("Master", {}, meter("master"), h("p", { class: "hint" }, "Mixed output to your headphones.")));
  root.replaceChildren(h("div", { class: "grid" }, cards));
}

// ---------- Microphone ----------
function renderMic() {
  const root = document.getElementById("tab-mic");
  const m = snapshot.config.mic;
  const push = () => send("mic", () => invoke("set_mic", { settings: m }));
  const gateMeter = meter("gate_in", { marker: m.gate.threshold_db });

  const levels = card("Levels", { wide: true },
    h("div", { class: "meter-label" }, h("span", {}, "Input"), h("span", { id: "mic-in-db" }, "")), meter("mic_in"),
    h("div", { class: "row" }, h("span", { class: "grow" }, "Listen to my mic"),
      toggle(m.monitor, (v) => { m.monitor = v; push(); })),
    h("p", { class: "hint" }, "Plays your processed mic in your headphones so you can check how you sound. It adds a little delay, so it can feel slightly echoey."),
    h("div", { class: "meter-label" }, h("span", {}, "Virtual Mic output"), h("span", { id: "mic-out-db" }, "")), meter("mic_out"),
    h("div", { class: "row" },
      h("div", { class: "grow" }, slider({ label: "Output gain", min: -20, max: 20, step: 0.5, value: m.gain_db, unit: " dB",
        onInput: (v) => { m.gain_db = v; push(); } })),
      (() => {
        const b = h("button", { class: "btn" + (m.muted ? " on" : ""), onclick: () => {
          m.muted = !m.muted; b.classList.toggle("on", m.muted); b.textContent = m.muted ? "Muted" : "Mute"; push();
        } }, m.muted ? "Muted" : "Mute");
        return b;
      })()));

  const denoise = card("Noise Removal", { enabled: m.denoise.enabled, onToggle: (v) => { m.denoise.enabled = v; push(); },
    extra: h("span", { class: "hint", id: "denoise-state" }, "") },
    h("p", { class: "hint" }, "DeepFilterNet3 AI noise suppression. Removes keyboards, fans, and background voices."),
    slider({ label: "Strength", min: 6, max: 100, value: m.denoise.strength_db,
      format: (v) => (v >= 100 ? "Max" : `${v} dB`), onInput: (v) => { m.denoise.strength_db = v; push(); } }),
    slider({ label: "Residual cleanup", min: 0, max: 0.05, step: 0.005, value: m.denoise.post_filter,
      format: (v) => (v === 0 ? "Off" : v.toFixed(3)), onInput: (v) => { m.denoise.post_filter = v; push(); } }),
    h("div", { class: "row" }, h("span", { class: "grow" }, "Low-latency model"),
      toggle(m.denoise.low_latency, (v) => { m.denoise.low_latency = v; push(); })),
    h("p", { class: "hint" }, "About 20 ms less mic delay, slightly less clean, uses more CPU. Your mic pauses for a moment while the model switches."));

  const gate = card("Noise Gate", { enabled: m.gate.enabled, onToggle: (v) => { m.gate.enabled = v; push(); },
    extra: h("span", { class: "hint", id: "gate-state" }, "") },
    gateMeter,
    slider({ label: "Threshold", min: -80, max: -10, value: m.gate.threshold_db, unit: " dB",
      onInput: (v) => { m.gate.threshold_db = v; gateMeter.marker.style.left = `${dbToPct(v)}%`; push(); } }),
    slider({ label: "Closed attenuation", min: -80, max: -3, value: m.gate.range_db, unit: " dB", onInput: (v) => { m.gate.range_db = v; push(); } }),
    slider({ label: "Attack", min: 0.5, max: 50, step: 0.5, value: m.gate.attack_ms, unit: " ms", onInput: (v) => { m.gate.attack_ms = v; push(); } }),
    slider({ label: "Hold", min: 0, max: 1000, step: 10, value: m.gate.hold_ms, unit: " ms", onInput: (v) => { m.gate.hold_ms = v; push(); } }),
    slider({ label: "Release", min: 10, max: 1000, step: 10, value: m.gate.release_ms, unit: " ms", onInput: (v) => { m.gate.release_ms = v; push(); } }));

  const comp = card("Compressor", { enabled: m.compressor.enabled, onToggle: (v) => { m.compressor.enabled = v; push(); } },
    h("div", { class: "meter-label" }, h("span", {}, "Gain reduction"), h("span", { id: "gr-db" }, "")), meter("mic_gr"),
    slider({ label: "Threshold", min: -60, max: 0, value: m.compressor.threshold_db, unit: " dB", onInput: (v) => { m.compressor.threshold_db = v; push(); } }),
    slider({ label: "Ratio", min: 1, max: 20, step: 0.1, value: m.compressor.ratio, format: (v) => `${v.toFixed(1)}:1`, onInput: (v) => { m.compressor.ratio = v; push(); } }),
    slider({ label: "Knee", min: 0, max: 24, value: m.compressor.knee_db, unit: " dB", onInput: (v) => { m.compressor.knee_db = v; push(); } }),
    slider({ label: "Attack", min: 0.1, max: 100, step: 0.1, value: m.compressor.attack_ms, unit: " ms", onInput: (v) => { m.compressor.attack_ms = v; push(); } }),
    slider({ label: "Release", min: 10, max: 1000, step: 10, value: m.compressor.release_ms, unit: " ms", onInput: (v) => { m.compressor.release_ms = v; push(); } }),
    slider({ label: "Makeup gain", min: 0, max: 24, step: 0.5, value: m.compressor.makeup_db, unit: " dB", onInput: (v) => { m.compressor.makeup_db = v; push(); } }));

  const eq = card("Equalizer", { wide: true, enabled: m.eq.enabled, onToggle: (v) => { m.eq.enabled = v; push(); } },
    eqEditor(m.eq, push));

  const chain = h("p", { class: "hint card wide" }, "Signal chain: Microphone → Noise Removal → Noise Gate → Equalizer → Compressor → Virtual Mic");
  root.replaceChildren(h("div", { class: "grid" }, levels, denoise, gate, comp, eq, chain));
}

// ---------- Apps ----------
async function renderApps() {
  const root = document.getElementById("tab-apps");
  const cfg = snapshot.config;
  let apps = [];
  try { apps = await invoke("list_apps"); } catch (e) { showError(e); }
  const sinks = cfg.channels.map((c) => c.sink);
  const rows = apps.map((app) => {
    let current = cfg.app_rules[app.exe];
    if (current === undefined) {
      const idx = sinks.findIndex((s) => s && s === app.assigned_device);
      current = idx >= 0 ? idx : null;
    }
    const seg = h("div", { class: "seg" }, ...[null, ...CHANNELS.keys()].map((ch) =>
      h("button", { class: current === ch ? "active" : "", onclick: async () => {
        try {
          await invoke("assign_app", { pid: app.pid, exe: app.exe, channel: ch });
          if (ch === null) delete cfg.app_rules[app.exe]; else cfg.app_rules[app.exe] = ch;
          renderApps();
        } catch (e) { showError(e); }
      } }, ch === null ? "Default" : CHANNELS[ch])));
    return h("div", { class: "app-row" }, h("div", { class: "name" }, app.exe, h("small", {}, app.path)), seg);
  });
  root.replaceChildren(card("Apps playing audio", { wide: true,
    extra: h("button", { class: "btn", onclick: renderApps }, "Refresh") },
    h("p", { class: "hint" }, "Choose a channel per app. Windows remembers the choice, and AudioManager re-applies it if it gets reset. Some apps need a restart to switch."),
    rows.length ? rows : h("p", { class: "hint" }, "No apps are playing audio right now.")));
}

// ---------- Settings ----------
function renderSettings() {
  const root = document.getElementById("tab-settings");
  const cfg = snapshot.config;
  const VIRTUAL = ["VB-Audio", "SteelSeries Sonar", "Elgato Virtual Audio", "Voicemeeter", "VoiceMeeter"];
  const physical = (list) => list.filter((d) => !VIRTUAL.some((v) => d.hardware.includes(v)));
  const cables = (list) => list.filter((d) => d.hardware.includes("VB-Audio"));

  function select(list, value, emptyLabel) {
    const sel = h("select", {}, h("option", { value: "" }, emptyLabel),
      ...list.map((d) => { const o = h("option", { value: d.id }, d.name + (d.is_default ? " (default)" : "")); o.selected = d.id === value; return o; }));
    return sel;
  }
  const output = select(physical(snapshot.render_devices), cfg.output_device, "Windows default");
  const mic = select(physical(snapshot.capture_devices), cfg.mic_device, "Windows default");
  const micSink = select(cables(snapshot.render_devices), cfg.mic_sink, "None");
  const sources = CHANNELS.map((_, i) => select(cables(snapshot.capture_devices), cfg.channels[i].source, "None"));

  const field = (label, el, hint) => h("div", {}, h("div", { class: "meter-label" }, h("span", {}, label)), el, hint ? h("p", { class: "hint" }, hint) : null);

  const devices = card("Devices", {},
    field("Headphones / speakers", output),
    field("Microphone", mic),
    field("Virtual Mic cable", micSink, "Select the cable's Input side here; pick its Output side as the microphone in Discord/OBS."),
    ...CHANNELS.map((name, i) => field(`${name} channel cable`, sources[i], i === 0 ? "Select the cable's Output side; apps get routed to its Input side." : null)),
    h("button", { class: "btn primary", onclick: async () => {
      const v = (s) => s.value || null;
      try {
        await invoke("set_devices", { output: v(output), mic: v(mic), micSink: v(micSink), sources: sources.map(v) });
        await refresh();
      } catch (e) { showError(e); }
    } }, "Apply & restart audio"));

  const general = card("General", {},
    h("div", { class: "row" }, h("span", { class: "grow" }, "Launch at Windows sign-in (starts in tray)"),
      toggle(cfg.launch_at_login, (v) => invoke("set_launch_at_login", { enabled: v }).catch(showError))),
    h("div", { class: "row" }, h("span", { class: "grow" }, "Set Windows default devices (like Sonar)"),
      toggle(cfg.set_windows_defaults, (v) => invoke("set_windows_defaults", { enabled: v }).catch(showError))),
    h("p", { class: "hint" }, "Game becomes the default playback device, Chat the default communications device, and the Virtual Mic the default recording device. Turning this off restores the defaults you had before."));

  const status = card("Engine status", {},
    h("div", { class: "status-list" }, ...Object.entries(snapshot.status).sort().flatMap(([k, v]) =>
      [h("span", {}, k), h("span", { class: v === "running" ? "ok" : "bad" }, v)])));

  root.replaceChildren(h("div", { class: "grid" }, devices, general, status));
}

// ---------- state & meters ----------
function needsSetup() {
  return snapshot && !snapshot.capture_devices.some((d) => d.hardware.includes("VB-Audio"));
}

function renderStatus() {
  const el = document.getElementById("status");
  const entries = Object.entries(snapshot.status);
  const bad = entries.filter(([, v]) => v !== "running");
  el.classList.toggle("ok", entries.length > 0 && bad.length === 0);
  el.classList.toggle("bad", bad.length > 0);
  document.getElementById("status-text").textContent = bad.length ? `${bad.length} stream issue${bad.length > 1 ? "s" : ""}` : "Running";
  const banner = document.getElementById("banner");
  if (needsSetup()) {
    banner.replaceChildren("No VB-Audio virtual cables found. Install VB-CABLE plus the A+B and C+D packs from vb-audio.com, reboot, then restart AudioManager.");
    banner.hidden = false;
  }
}

async function refresh() {
  snapshot = await invoke("get_state");
  renderStatus();
  renderMixer();
  renderMic();
  renderSettings();
  if (document.getElementById("tab-apps").classList.contains("active")) renderApps();
}

function setMeter(key, db) {
  const m = meterEls[key];
  if (m && m.el.isConnected) m.fill.style.width = `${dbToPct(db)}%`;
}

async function pollMeters() {
  if (!document.hidden && snapshot) {
    try {
      const m = await invoke("get_meters");
      m.channels.forEach((db, i) => setMeter(`ch${i}`, db));
      setMeter("master", m.master);
      setMeter("mic_in", m.mic.input_db);
      setMeter("gate_in", m.mic.input_db);
      setMeter("mic_out", m.mic.output_db);
      const gr = meterEls.mic_gr;
      if (gr && gr.el.isConnected) gr.fill.style.width = `${Math.min(100, (-m.mic.gain_reduction_db / 20) * 100)}%`;
      const txt = (id, t) => { const e = document.getElementById(id); if (e) e.textContent = t; };
      txt("mic-in-db", `${m.mic.input_db.toFixed(0)} dB`);
      txt("mic-out-db", `${m.mic.output_db.toFixed(0)} dB`);
      txt("gr-db", `${m.mic.gain_reduction_db.toFixed(1)} dB`);
      txt("gate-state", m.mic.gate_open ? "open" : "closed");
      txt("denoise-state", m.mic.denoise_ready ? `+${m.mic.denoise_latency_ms.toFixed(0)} ms` : "loading model…");
    } catch { /* engine restarting */ }
  }
  setTimeout(pollMeters, 50);
}

document.querySelectorAll(".tab").forEach((tab) => tab.addEventListener("click", () => {
  document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
  document.querySelectorAll(".panel").forEach((p) => p.classList.toggle("active", p.id === `tab-${tab.dataset.tab}`));
  if (tab.dataset.tab === "apps") renderApps();
}));

setInterval(async () => {
  if (document.hidden || !snapshot) return;
  try {
    const next = await invoke("get_state");
    snapshot.status = next.status;
    renderStatus();
  } catch { /* ignore */ }
}, 3000);

refresh().catch(showError);
pollMeters();
