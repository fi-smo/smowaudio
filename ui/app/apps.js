// Main window: the Apps view (drag apps onto channels).

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
