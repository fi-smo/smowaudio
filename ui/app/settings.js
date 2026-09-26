// Main window: Settings (Devices, General, Updates and the changelog).

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

/** Every stream with its state. A microphone that isn't plugged in or switched on is "off", not
 *  failing: that's normal for a wireless mic, and it starts by itself once the mic is back. */
function streams() {
  const rank = (name) => { const i = STREAM_ORDER.indexOf(name); return i < 0 ? STREAM_ORDER.length : i; };
  return Object.entries(snap?.status ?? {})
    .map(([name, state]) => {
      const ok = state === "running";
      const off = !ok && name === "Microphone" && deviceMissing(state);
      return { name, ok, off, reason: ok ? null : state };
    })
    .sort((a, b) => rank(a.name) - rank(b.name) || a.name.localeCompare(b.name));
}
const failingStreams = () => streams().filter((s) => !s.ok && !s.off);
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
  const bad = list.filter((s) => !s.ok && !s.off);
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
    const off = list.filter((s) => s.off);
    const running = list.length - off.length;
    const text = !list.length ? " — starting"
      : off.length ? ` — ${running} running · mic not connected`
      : ` — all ${running} running`;
    summary = h("span", { class: "health-text" }, h("b", {}, "Audio streams"), h("span", { class: "muted" }, text));
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
    h("span", { class: s.ok ? "ok" : s.off ? "off" : "bad", title: s.ok ? "Running" : s.off ? "Not connected: starts when the mic is back" : s.reason },
      h("i"), s.name)))] : []));
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
          ? "Not connected right now. It starts by itself when the mic is switched on."
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
