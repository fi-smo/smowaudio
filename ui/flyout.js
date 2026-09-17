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
const shortName = (name) => name.replace(/\s*\(.*\)$/, "");

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

function render() {
  renderOutputs();
  const bad = Object.values(snap.status).filter((v) => v !== "running").length;
  $("#fly-status").classList.toggle("warn", bad > 0);
  $("#fly-status").title = bad ? `${bad} stream${bad > 1 ? "s" : ""} need attention` : "All streams running";

  $("#fly-rows").replaceChildren(...CHANNELS.map((c, i) => {
    const s = snap.config.channels[i].settings;
    const id = `fly-vol-${i}`;
    const out = h("output", { for: id }, `${Math.round(s.volume * 100)}%`);
    const input = h("input", { type: "range", id, min: "0", max: "150", step: "1", value: String(Math.round(s.volume * 100)), "aria-label": `${c.name} volume` });
    input.addEventListener("input", () => {
      s.volume = input.valueAsNumber / 100;
      out.textContent = `${input.value}%`;
      send(`ch${i}`, () => invoke("set_channel", { index: i, settings: s }));
    });
    return h("div", { class: "fly-row", style: `--c:${c.color}` },
      h("span", { class: "tape" }, c.name), input, out,
      h("span", { class: "thin", "aria-hidden": "true" }, h("i", { "data-level": String(i) })));
  }));

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
      m.channels.forEach((pair, i) => {
        const bar = document.querySelector(`[data-level="${i}"]`);
        if (bar) bar.style.setProperty("--a", `${meterPct(Math.max(pair[0], pair[1]))}%`);
      });
    } catch { /* engine restarting */ }
  }
  setTimeout(pollMeters, 60);
}

// Keep the window exactly as tall as its content (+2 for the body's border).
let fittedHeight = 0;
new ResizeObserver(() => {
  const height = Math.ceil($(".flyout").getBoundingClientRect().height) + 2;
  if (height === fittedHeight) return;
  fittedHeight = height;
  invoke("fit_flyout", { height }).catch(() => {});
}).observe($(".flyout"));

// Volumes may have changed in the main window since the flyout was last open.
listen("flyout-shown", () => { toggleOutputs(false); load().catch(() => {}); });
// A shortcut or the main window changed something.
listen("config-changed", () => load().catch(() => {}));
window.addEventListener("focus", () => load().catch(() => {}));
load().catch(() => {});
pollMeters();
