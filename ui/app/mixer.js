// Main window: the Mixer view.

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
