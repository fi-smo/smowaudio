// Tray flyout: quick channel volumes and mic controls. Talks to the same commands as the main window.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const CHANNELS = [
  { name: "Game", color: "var(--game)" },
  { name: "Chat", color: "var(--chat)" },
  { name: "Media", color: "var(--media)" },
  { name: "Aux", color: "var(--aux)" },
];

let snap = null;
const $ = (s) => document.querySelector(s);
const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
const meterPct = (db) => clamp((db + 60) / 60, 0, 1) * 100;

function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) if (v !== undefined && v !== null) el.setAttribute(k, v);
  for (const c of children) el.append(c instanceof Node ? c : document.createTextNode(c));
  return el;
}

const pending = new Map();
function send(key, fn) {
  const had = pending.has(key);
  pending.set(key, fn);
  if (had) return;
  setTimeout(() => { const f = pending.get(key); pending.delete(key); f().catch(() => {}); }, 40);
}

// Same list as the main window: cables and other apps' virtual devices can't be the output.
const VIRTUAL_HARDWARE = ["VB-Audio", "SteelSeries Sonar", "Elgato Virtual Audio", "Voicemeeter", "VoiceMeeter"];
// "Speakers (5- soundcore Select 4 Go )" -> "soundcore Select 4 Go" (same as the main window).
function shortName(name) {
  const m = /^(.*?)\s*\((.*)\)\s*$/.exec(name);
  return m ? m[2].replace(/^\d+-\s*/, "").trim() || m[1] : name;
}

// An in-page list rather than a <select>: a native dropdown takes focus from the window,
// which would hide the flyout mid-choice.
function renderOutputs() {
  const devices = snap.render_devices.filter((d) => !VIRTUAL_HARDWARE.some((v) => d.hardware.includes(v)));
  const current = snap.config.output_device;
  const selected = devices.find((d) => d.id === current);
  $("#fly-output").textContent = selected ? shortName(selected.name) : "Automatic";
  const option = (id, label, title) => {
    const b = h("button", { type: "button", role: "option", "aria-selected": String(id === current), title }, h("span", {}, label));
    b.addEventListener("click", () => chooseOutput(id));
    return b;
  };
  $("#fly-output-list").replaceChildren(
    option(null, "Automatic", "Your usual default headphones or speakers"),
    ...devices.map((d) => option(d.id, shortName(d.name), d.name)));
}

function toggleOutputs(open) {
  $("#fly-output-list").hidden = !open;
  $("#fly-output-btn").setAttribute("aria-expanded", String(open));
  if (open) $("#fly-output-list [aria-selected='true']")?.focus();
}

async function chooseOutput(id) {
  toggleOutputs(false);
  if (id === snap.config.output_device) return;
  snap.config.output_device = id;
  renderOutputs();
  try { await invoke("set_output_device", { output: id }); } catch { await load(); }
}
$("#fly-output-btn").addEventListener("click", () => toggleOutputs($("#fly-output-list").hidden));
document.addEventListener("pointerdown", (e) => {
  if (!$("#fly-output-list").hidden && !e.target.closest(".fly-dev")) toggleOutputs(false);
});

const SPEAKER_ON = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 6h2.5l3.5-3v10l-3.5-3h-2.5z"/><path d="M11 5.5a3.5 3.5 0 0 1 0 5M12.8 3.5a6.2 6.2 0 0 1 0 9"/></svg>';
const SPEAKER_OFF = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 6h2.5l3.5-3v10l-3.5-3h-2.5z"/><path d="M11 6l3.5 4M14.5 6 11 10"/></svg>';

const MAX_VOLUME = 1.5;
const dbOf = (v) => (v <= 0.0001 ? -Infinity : 20 * Math.log10(v));
const fmtDb = (db) => (db === -Infinity || db < -99 ? "−∞" : (db > 0.05 ? "+" : db < -0.05 ? "−" : "") + Math.abs(db).toFixed(1));

/** Horizontal version of the mixer's fader: drag, click, arrow keys; double-click resets to 100 %. */
function makeFader(label, onInput) {
  const el = h("div", { class: "hfader", role: "slider", tabindex: "0", "aria-label": `${label} volume`, "aria-valuemin": "0", "aria-valuemax": "150" },
    h("div", { class: "unity" }), h("div", { class: "fill" }), h("div", { class: "cap" }));
  let v = 1;
  const render = () => {
    el.style.setProperty("--p", (v / MAX_VOLUME).toFixed(4));
    el.setAttribute("aria-valuenow", String(Math.round(v * 100)));
    el.setAttribute("aria-valuetext", `${Math.round(v * 100)} %`);
    el.title = `${fmtDb(dbOf(v))} dB · double-click for 100 %`;
  };
  const set = (next) => {
    v = clamp(next, 0, MAX_VOLUME);
    if (Math.abs(v - 1) < 0.03) v = 1; // snap to unity, like the mixer
    render(); onInput(v);
  };
  const fromX = (x) => { const r = el.getBoundingClientRect(); set(clamp((x - r.left - 9) / (r.width - 18), 0, 1) * MAX_VOLUME); };
  let lastTap = 0;
  el.addEventListener("pointerdown", (e) => {
    // Touch has no dblclick, so a quick second tap resets too.
    if (e.pointerType !== "mouse" && e.timeStamp - lastTap < 350) { set(1); lastTap = 0; return; }
    lastTap = e.timeStamp;
    el.setPointerCapture(e.pointerId); el.dataset.dragging = ""; fromX(e.clientX);
  });
  el.addEventListener("pointermove", (e) => { if (el.hasPointerCapture(e.pointerId)) fromX(e.clientX); });
  el.addEventListener("lostpointercapture", () => delete el.dataset.dragging);
  el.addEventListener("dblclick", () => set(1));
  el.addEventListener("keydown", (e) => {
    const step = { ArrowRight: 0.02, ArrowUp: 0.02, ArrowLeft: -0.02, ArrowDown: -0.02, PageUp: 0.1, PageDown: -0.1 }[e.key];
    if (step !== undefined) { set(v + step); e.preventDefault(); }
    if (e.key === "Home") { set(1); e.preventDefault(); }
  });
  /** Shows a value changed elsewhere, unless the user is dragging this fader. */
  el.setValue = (next) => { if (!("dragging" in el.dataset)) { v = clamp(next, 0, MAX_VOLUME); render(); } };
  render();
  return el;
}

// Meter segments are real elements rather than a repeating gradient: the browser snaps elements to
// whole pixels, so they stay even at 125 % or 150 % display scaling. The lit copy sits on top of the
// dark one and is cut off at the level.
const SEGMENTS = 30;
function segments(lit) {
  return h("span", { class: lit ? "segs lit" : "segs" }, ...Array.from({ length: SEGMENTS }, (_, k) =>
    h("i", lit && k >= SEGMENTS * 0.85 ? { class: "hot" } : lit && k >= SEGMENTS * 0.7 ? { class: "warn" } : {})));
}
/** Rounds a meter percentage to whole segments, like LEDs. */
const toSegment = (pct) => Math.round((pct / 100) * SEGMENTS) * (100 / SEGMENTS);

// Built once and then updated in place, so a slider is never replaced under the mouse.
const rows = [];
const meters = []; // per channel: { lits, peaks, hold, holdAt }

function buildRows() {
  $("#fly-rows").replaceChildren(...CHANNELS.map((c, i) => {
    const settings = () => snap.config.channels[i].settings;
    const out = h("output", {});
    const fader = makeFader(c.name, (v) => {
      settings().volume = v;
      out.textContent = `${Math.round(v * 100)}%`;
      send(`ch${i}`, () => invoke("set_channel", { index: i, settings: settings() }));
    });
    const mute = h("button", { type: "button", class: "btn mute fly-mute" });
    const lits = [segments(true), segments(true)];
    const peaks = [h("i", { class: "peak" }), h("i", { class: "peak" })];
    meters[i] = { lits, peaks, hold: [0, 0], holdAt: [0, 0] };
    const row = h("div", { class: "fly-row", style: `--c:${c.color}` },
      h("span", { class: "tape" }, c.name), fader, out, mute,
      h("div", { class: "fmeter", "aria-hidden": "true" },
        h("span", { class: "hrow" }, segments(false), lits[0], peaks[0]),
        h("span", { class: "hrow" }, segments(false), lits[1], peaks[1])));
    const showMute = (muted) => {
      mute.setAttribute("aria-pressed", String(muted));
      mute.title = muted ? `Unmute ${c.name}` : `Mute ${c.name}`;
      mute.setAttribute("aria-label", mute.title);
      mute.innerHTML = muted ? SPEAKER_OFF : SPEAKER_ON;
      row.classList.toggle("muted", muted);
    };
    mute.addEventListener("click", () => {
      settings().muted = !settings().muted;
      showMute(settings().muted);
      send(`ch${i}`, () => invoke("set_channel", { index: i, settings: settings() }));
    });
    rows[i] = (s) => {
      fader.setValue(s.volume);
      if (!("dragging" in fader.dataset)) out.textContent = `${Math.round(s.volume * 100)}%`;
      showMute(s.muted);
    };
    return row;
  }));
}

function render() {
  renderOutputs();
  const bad = Object.values(snap.status).filter((v) => v !== "running").length;
  $("#fly-status").classList.toggle("warn", bad > 0);
  $("#fly-status").title = bad ? `${bad} stream${bad > 1 ? "s" : ""} need attention` : "All streams running";

  if (!rows.length) buildRows();
  snap.config.channels.forEach((c, i) => rows[i]?.(c.settings));

  const mic = snap.config.mic;
  const mute = $("#fly-mute");
  mute.setAttribute("aria-pressed", String(mic.muted));
  mute.textContent = mic.muted ? "Muted" : "Mute";
  $("#fly-listen").checked = mic.monitor;
}

$("#fly-mute").addEventListener("click", () => {
  if (!snap) return;
  snap.config.mic.muted = !snap.config.mic.muted;
  render();
  send("mic", () => invoke("set_mic", { settings: snap.config.mic }));
});
$("#fly-listen").addEventListener("change", (e) => {
  if (!snap) return;
  snap.config.mic.monitor = e.target.checked;
  send("mic", () => invoke("set_mic", { settings: snap.config.mic }));
});
document.querySelectorAll("[data-open]").forEach((b) =>
  b.addEventListener("click", () => invoke("open_main_window", { view: b.dataset.open })));
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if (!$("#fly-output-list").hidden) {
    toggleOutputs(false);
    $("#fly-output-btn").focus();
  } else {
    window.__TAURI__.window.getCurrentWindow().hide();
  }
});

async function load() {
  snap = await invoke("get_state");
  render();
}

async function pollMeters() {
  if (!document.hidden && snap) {
    try {
      const m = await invoke("get_meters");
      const now = performance.now();
      m.channels.forEach((pair, i) => {
        const meter = meters[i];
        if (!meter) return;
        pair.forEach((db, side) => {
          const pct = meterPct(db);
          meter.lits[side].style.setProperty("--lvl", `${toSegment(pct)}%`);
          // Same peak hold as the mixer: hold for 0.9 s, then fall.
          if (pct >= meter.hold[side]) { meter.hold[side] = pct; meter.holdAt[side] = now; }
          else if (now - meter.holdAt[side] > 900) meter.hold[side] = Math.max(pct, meter.hold[side] - 2.5);
          meter.peaks[side].style.setProperty("--pk", `${Math.min(toSegment(meter.hold[side]), 100 - 100 / SEGMENTS)}%`);
        });
      });
    } catch { /* engine restarting */ }
  }
  setTimeout(pollMeters, 50);
}

// Keep the window exactly as tall as the card (its border included). Sent on every change and
// every time the flyout opens: a resize requested while the window was hidden can be lost.
const fit = () => invoke("fit_flyout", { height: Math.ceil($(".flyout").getBoundingClientRect().height) }).catch(() => {});
new ResizeObserver(fit).observe($(".flyout"));

// Volumes may have changed in the main window since the flyout was last open.
listen("flyout-shown", () => { toggleOutputs(false); fit(); load().then(fit).catch(() => {}); });
// The main window or a shortcut changed something. Changes made here are skipped (so a slider
// being dragged isn't redrawn), and bursts reload at most about 8 times a second.
let reloadTimer = null;
listen("config-changed", (e) => {
  if (e.payload?.source === "flyout" || reloadTimer) return;
  reloadTimer = setTimeout(() => {
    reloadTimer = null;
    if (!document.hidden) load().catch(() => {});
  }, 120);
});
window.addEventListener("focus", () => load().catch(() => {}));
load().catch(() => {});
pollMeters();
