// Main window: Settings → Shortcuts (recording and binding key combinations).

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
