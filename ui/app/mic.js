// Main window: the Microphone view, its level meters and the mic test.

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
      "From ", from ? h("b", {}, from) : h("b", { class: "muted" }, "your mic, not connected right now"),
      snap.config.mic_device || !from ? "" : " (automatic)", " → To ",
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
  if (caption) caption.lastChild.textContent = missing ? "Mic not connected: nothing to measure"
    : !mic.gate.enabled ? `Level ${fmtDb(m.input_db)} dB · the gate is off`
    : m.gate_open ? `Level ${fmtDb(m.input_db)} dB, so the gate is open`
    : `Room noise ${fmtDb(m.input_db)} dB, so the gate is closed`;
  const limiterNow = $("#limiter-now");
  if (limiterNow) limiterNow.textContent = `${fmtDb(m.limiter_db)} dB`;

  // Chain tabs: a status line and a dot per step. "warn" marks a step that isn't ready.
  const states = {
    input: missing ? (deviceMissing(snap.status.Microphone) ? ["Not connected", "off"] : ["Stopped", "warn"]) : [`${fmtDb(m.input_db)} dBFS`, "ok"],
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
