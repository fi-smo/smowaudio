// Main window: navigation, polling and startup. Loaded last.

// ---------- status ----------
function renderStatus() {
  const bad = failingStreams().length;
  $("#pill-status").classList.toggle("warn", bad > 0);
  const micOff = streams().some((s) => s.off);
  $("#pill-status-text").textContent = bad ? `${bad} ${bad === 1 ? "stream needs" : "streams need"} attention`
    : micOff ? "Running · mic off" : "All streams running";
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
