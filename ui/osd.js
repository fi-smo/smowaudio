// Overlay confirming what a keyboard shortcut changed. Rust shows and hides the window.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s) => document.querySelector(s);
// Rust hides the window after 1.6 s; fade out just before that.
const FADE_AFTER_MS = 1300;
let fadeTimer;

function show(osd) {
  if (!osd) return;
  const card = $("#osd");
  card.style.setProperty("--c", `var(--${osd.group === "system" || osd.group === "output" ? "focus" : osd.group})`);
  card.classList.toggle("dim", osd.dim);
  $("#osd-tape").textContent = osd.tape;
  $("#osd-label").textContent = osd.label;
  $("#osd-value").textContent = osd.value;
  $("#osd-value").title = osd.value;
  const hasBar = osd.level !== null && osd.level !== undefined;
  $("#osd-bar").hidden = !hasBar;
  if (hasBar) {
    $("#osd-fill").style.width = `${Math.max(0, Math.min(1, osd.level)) * 100}%`;
    $("#osd-unity").hidden = osd.unity === null || osd.unity === undefined;
    $("#osd-unity").style.left = `${(osd.unity ?? 0) * 100}%`;
  }
  card.classList.add("shown");
  clearTimeout(fadeTimer);
  fadeTimer = setTimeout(() => card.classList.remove("shown"), FADE_AFTER_MS);
}

listen("osd", (e) => show(e.payload));
invoke("take_osd").then(show).catch(() => {});
