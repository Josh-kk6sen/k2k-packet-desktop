// K2K Packet Desktop — core app runtime (connect, command line, unproto
// monitor, session tabs, settings, raw-KISS display).
//
// Framework 3 Phase B: a faithful, readable port of the app logic that used to
// live inside the opaque shipped bundle (public/assets/index-DkQAlvay.js), which
// bundled the Tauri v2 JS API plus ~450 lines of app code. Here the Tauri API
// comes from the real @tauri-apps/api package (already a dependency) and the
// app logic is written out in readable, named functions.
//
// IMPORTANT (Phase C contract): this module registers the rendering event
// listeners (unproto-frame, tnc-*, session-*, ui-sent, session-retry,
// session-ack). index.html loads ONLY this module tree — the old shipped
// bundle is no longer loaded, so frames render exactly once. If the shipped
// bundle is ever reintroduced it must NOT co-load with this module, or
// frames will render twice.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

// ── Element handles (same ids as index.html) ──
const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

const hostEl = $("#host");
const portEl = $("#port");
const connectBtn = $("#connect-btn");
const fullscreenBtn = $("#fullscreen-btn");
const connStatus = $("#conn-status");
const modeTabs = $$(".mode-tab");
const leftMsgs = $("#left-msgs");
const viaInput = $("#via-input");
const composeIn = $("#compose-in");
const sendBtn = $("#send-btn");
const unprotoData = $("#unproto-data");
const uCount = $("#u-count");
const uClear = $("#u-clear");
const unprotoTo = $("#unproto-to");
const unprotoVia = $("#unproto-via");
const unprotoMsg = $("#unproto-msg");
const unprotoSend = $("#unproto-send");
const unprotoStatus = $("#unproto-status");
const divider = $("#divider");
const settingsOverlay = $("#settings-overlay");
const settingsBtn = $("#settings-btn");

// HTML-escape a string for use as innerHTML text.
const esc = (t) => {
  const e = document.createElement("div");
  return (e.textContent = t), e.innerHTML;
};

// ── State ──
const TAB_COUNT = 4;
const tabs = Array.from({ length: TAB_COUNT }, (_, i) => ({ id: `tab-${i}`, msgs: [], connId: null, remote: "", digis: [], state: "disconnected", lastSentText: null }));
let activeTab = 0;          // current tab index
let unprotoCount = 0;       // rendered unproto entries
let conn = null;            // current TNC connection {id, host, port, cx}
let settings = { theme: "dark", deduplicatePackets: false, simpleUnproto: false };
let rawKissEntries = [];    // raw-KISS display entries
const unprotoFrames = [];    // all received unproto frames (display model)

const DEFAULT_SETTINGS = {
  callsign: "", host: "", port: 8001, autoConnect: true,
  kissP: 63, kissSlotTime: 10, kissTxdelay: 30, kissT1Ms: 1e4, kissT2Ms: 1e3,
  kissT3Ms: 3e5, kissKaCheckMs: 6e4, kissN2Max: 5,
  messageBoardEnabled: false, messageBoardCallsign: "", messageBoardSsid: 1,
  messageBoardSysops: "", messageBoardPrivacy: false,
  messageBoardPublicAliases: "ALL", messageBoardMaxPacketSize: 250,
  theme: "dark", maxLines: 500,
  beaconEnabled: false, beacon: "", beaconInterval: 240, beaconDest: "BEACON", beaconVia: "",
  appFontSize: 18, connFontSize: -2, unprotoFontSize: -2,
  rawKissDisplay: false, debugTxEnabled: true, deduplicatePackets: false,
  simpleUnproto: false, onlyUnproto: false, hideVia: false,
  unprotoIgnoredDestinations: "NODES BEACON ID", unprotoTo: "", unprotoVia: "",
};

let composeHistory = [];    // :c / connected command history (up-arrow)
let composeHistIdx = -1;
const unprotoCmdHistory = []; // unproto compose history
let unprotoHistIdx = -1;

// phash — simple string hash used for unproto dedup.
function phash(s) {
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = (h << 5) - h + s.charCodeAt(i);
    h = h & h;
  }
  return h;
}

// ── Status dot (conn-status element) ──
function setConnStatus(state, text) {
  connStatus.className = state;
  const cls = state.replace("connected", "green").replace("failed", "red").replace("connecting", "gray").replace("idle", "gray");
  connStatus.innerHTML = `<span class="dot ${cls}"></span> ${text}`;
}

// Current protocol helper: does the connection use AGWPE?
const isAgw = () => settings.protocol === "AGWPE";
const connCmd = (kiss, agw) => (isAgw() ? agw : kiss);

// ── Left panel / session tab rendering ──
function applyRawKissVisibility() {
  const raw = settings.rawKissDisplay;
  modeTabs.forEach((e) => (e.style.display = raw ? "none" : ""));
  $("#via-area").style.display = raw ? "none" : "";
  $("#compose").style.display = raw ? "none" : "";
}

function renderTab(tabIndex) {
  applyRawKissVisibility();
  if (settings.rawKissDisplay) {
    leftMsgs.innerHTML = "";
    if (rawKissEntries.length === 0) {
      leftMsgs.innerHTML = '<div class="left-msg sys">Raw KISS mode enabled. Displaying full raw frames.</div>';
    } else {
      rawKissEntries.forEach((n) => {
        const el = document.createElement("div");
        el.className = "left-msg raw" + (n.isSent ? " sent" : "");
        el.innerHTML = `<div class="t">${esc(n.ts)}</div><div class="decoded">${esc(n.header)}</div><div class="hex">${esc(n.hex)}</div>`;
        leftMsgs.appendChild(el);
      });
    }
    leftMsgs.scrollTop = leftMsgs.scrollHeight;
    return;
  }
  leftMsgs.innerHTML = "";
  const t = tabs[tabIndex];
  if (!t || !t.msgs.length) {
    const msg = t.state === "connected" ? `Connected to ${t.remote}. Type a message.` : t.state === "connecting" ? "Connecting..." : ":c CALLSIGN to connect";
    leftMsgs.innerHTML = `<div class="left-msg sys">${msg}</div>`;
    return;
  }
  t.msgs.forEach((n) => {
    const el = document.createElement("div");
    el.className = "left-msg " + n.dir;
    el.style.whiteSpace = "pre-wrap";
    el.textContent = n.text;
    leftMsgs.appendChild(el);
  });
  leftMsgs.scrollTop = leftMsgs.scrollHeight;
}

// push a message into a tab's transcript
function tabMsg(tabIdx, dir, text) {
  const t = tabs[tabIdx];
  if (!t) return;
  t.msgs.push({
    dir: dir,
    text: text,
    ts: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
  });
  if (tabIdx === activeTab) renderTab(tabIdx);
}

// refresh the tab-bar state dots + labels
function refreshTabBar() {
  modeTabs.forEach((t, e) => {
    const st = tabs[e].state;
    const dot = t.querySelector(".conn-dot");
    const label = t.querySelector(".remote-label");
    dot.className = "conn-dot " + (st === "connected" ? "green" : st === "connecting" ? "yellow" : "gray");
    label.textContent = st === "connected" ? " - " + (tabs[e].digis && tabs[e].digis.length ? tabs[e].digis[0] : tabs[e].remote) : "";
  });
}

// ── Connect / disconnect ──
async function disconnectTnc() {
  if (!conn) return;
  try {
    await invoke(connCmd("kiss_disconnect", "agw_disconnect_server"), { id: conn.id });
  } catch (e) {}
  conn.cx = false;
  conn = null;
  setConnStatus("idle", "Disconnected");
  sendBtn.disabled = true;
  unprotoSend.disabled = true;
  connectBtn.textContent = "Connect";
  connectBtn.className = "";
  hostEl.disabled = false;
  portEl.disabled = false;
  for (let i = 0; i < TAB_COUNT; i++) { tabs[i].state = "disconnected"; tabs[i].remote = ""; }
  refreshTabBar();
}

async function connectTnc() {
  const hostv = hostEl.value.trim() || "";
  const portv = parseInt(portEl.value) || 8001;
  if (!hostv) {
    hostEl.focus();
    hostEl.style.borderColor = "var(--red)";
    setTimeout(() => (hostEl.style.borderColor = ""), 2000);
    return;
  }
  let callsign = settings.callsign || "";
  if (!callsign) {
    settingsBtn.click();
    setTimeout(() => document.getElementById("set-call").focus(), 100);
    return;
  }
  const parts = callsign.split("-");
  const base = parts[0];
  const ssid = parseInt(parts[1]) || 0;
  setConnStatus("connecting", "Connecting...");
  try {
    const id = `c-${Date.now()}`;
    conn = { id, host: hostv, port: portv, cx: false };
    await invoke(connCmd("kiss_connect", "agw_connect"), { id, label: base, host: hostv, port: portv, callsign: base, ssid });
    conn.cx = true;
    setConnStatus("connected", "Connected");
    sendBtn.disabled = false;
    unprotoSend.disabled = false;
    connectBtn.textContent = "Disconnect";
    connectBtn.className = "disconnect";
    hostEl.disabled = true;
    portEl.disabled = true;
    composeIn.focus();
    tabMsg(activeTab, "sys", "TNC connected to " + hostv + ":" + portv);
    settings.host = hostv;
    settings.port = portv;
    settings.callsign = callsign;
    try {
      // Lossless merge-save: load the full settings from disk and flip ONLY
      // host/port/callsign (never re-save the stale module snapshot wholesale).
      const savedSettings = await invoke("load_settings");
      if (savedSettings) {
        savedSettings.host = hostv;
        savedSettings.port = portv;
        savedSettings.callsign = callsign;
        await invoke("save_settings", { settings: savedSettings });
      }
    } catch (e) {}
  } catch (err) {
    setConnStatus("idle", " Disconnected");
    tabMsg(activeTab, "sys", "Failed: " + err);
  }
}

// In-flight connect guard: prevents a double-click (or re-click during the
// async connect) from spawning a second backend session / orphaning the first.
let connecting = false;

connectBtn.onclick = async () => {
  if (conn && conn.cx) { await disconnectTnc(); return; }
  if (connecting) return;
  connecting = true;
  try { await connectTnc(); }
  finally { connecting = false; }
};

// ── Command line (compose-in) ──
composeIn.onkeydown = (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    handleCommand();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (composeHistory.length && composeHistIdx > -1) {
      if (composeHistIdx > 0) composeHistIdx--;
      composeIn.value = composeHistory[composeHistIdx];
    }
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    if (composeHistory.length && composeHistIdx < composeHistory.length - 1) {
      composeHistIdx++;
      composeIn.value = composeHistory[composeHistIdx];
    } else if (composeHistIdx >= composeHistory.length - 1) {
      composeHistIdx = composeHistory.length;
      composeIn.value = "";
    }
  }
};

async function handleCommand() {
  const text = composeIn.value.trim();
  const via = viaInput.value.trim();
  if (!text) return;

  // :c CALLSIGN [via ...] — open a connected-mode session
  if (text.match(/^:c /i)) {
    const target = text.slice(3).trim().split(/\s+/)[0];
    if (!target || !conn || !conn.cx) {
      tabMsg(activeTab, "sys", "Not connected to TNC");
      composeIn.value = "";
      return;
    }
    const parts = target.split("-");
    const remoteBase = parts[0].toUpperCase();
    const ssid = (parts.length > 1 && parseInt(parts[1])) || 0;
    tabMsg(activeTab, "sys", `Connecting to ${target}${via ? " via " + via : ""}...`);
    tabs[activeTab].state = "connecting";
    tabs[activeTab].remote = target;
    tabs[activeTab].remoteSsid = ssid;
    tabs[activeTab].digis = via ? via.split(/\s+/).map((k) => k.trim().toUpperCase()).filter(Boolean) : [];
    refreshTabBar();
    try {
      await invoke(connCmd("ax25_connect", "agw_ax25_connect"), { connId: conn.id, tab: activeTab, remote: remoteBase, remoteSsid: ssid, via });
    } catch (err) {
      tabMsg(activeTab, "sys", "Connect failed: " + err);
      tabs[activeTab].state = "disconnected";
      refreshTabBar();
    }
    composeIn.value = "";
    composeHistory.unshift(text);
    composeHistIdx = composeHistory.length;
    return;
  }

  // :d — disconnect current session
  if (/^:d$/i.test(text)) {
    if (tabs[activeTab].state === "connected" || tabs[activeTab].state === "connecting") {
      tabMsg(activeTab, "sys", "Disconnecting...");
      try {
        await invoke(connCmd("ax25_disconnect", "agw_disconnect"), { connId: conn.id, tab: activeTab });
      } catch (err) {
        tabMsg(activeTab, "sys", "Disconnect failed: " + err);
      }
      tabs[activeTab].state = "disconnected";
      tabs[activeTab].remote = "";
      refreshTabBar();
    }
    composeIn.value = "";
    return;
  }

  if (!conn || !conn.cx) {
    tabMsg(activeTab, "sys", "Not connected");
    return;
  }
  if (tabs[activeTab].state !== "connected") {
    tabMsg(activeTab, "sys", "Not connected to any station. Use :c CALLSIGN");
    composeIn.value = "";
    return;
  }

  tabs[activeTab].lastSentText = text;
  try {
    await invoke(connCmd("ax25_send_iframe", "agw_send_data"), { connId: conn.id, tab: activeTab, text });
    composeHistory.unshift(text);
    composeHistIdx = composeHistory.length;
    composeIn.value = "";
  } catch (err) {
    tabMsg(activeTab, "sys", "Send failed: " + err);
  }
}

sendBtn.onclick = () => handleCommand();

// ── Unproto monitor ──
function timeStr() {
  return new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
}

function ingestUnproto(src, dst, payload, ts, frameType, radioHeader, rawKiss, sent) {
  if (frameType === "DBG" && settings.debugTxEnabled === false) return;
  const t = ts || timeStr();
  unprotoFrames.push({ src, dst, payload, ts: t, frameType, radioHeader, rawKiss, sent });
  const max = settings.maxLines || 500;
  while (unprotoFrames.length > max) unprotoFrames.shift();
  if (settings.rawKissDisplay && rawKiss) {
    const header = radioHeader ? radioHeader.replace(/^radio:\s*(?:fm\s+)?/, t + " ") : `${t} ${src}${dst ? " to " + dst : ""} ${frameType || ""}`;
    rawKissEntries.push({ ts: t, header, hex: rawKiss, isSent: !!sent });
    while (rawKissEntries.length > max) rawKissEntries.shift();
    renderTab(activeTab);
  }
  renderUnproto();
}

function renderUnproto() {
  unprotoData.innerHTML = "";
  unprotoCount = 0;
  const seen = new Set();
  for (let idx = 0; idx < unprotoFrames.length; idx++) {
    const entry = unprotoFrames[idx];
    if (settings.simpleUnproto && settings.unprotoIgnoredDestinations) {
      const ignored = settings.unprotoIgnoredDestinations.toUpperCase().split(/[, ]+/).filter(Boolean);
      if (ignored.includes(entry.dst.toUpperCase())) continue;
    }
    if (settings.deduplicatePackets) {
      const key = entry.src + "|" + entry.dst + "|" + phash(entry.payload);
      if (seen.has(key)) continue;
      seen.add(key);
    }
    const ee = document.createElement("div");
    ee.className = "u-entry";
    let out = "";
    const hdr = entry.radioHeader;
    const ft = entry.frameType || "UI";
    if (hdr) {
      if (settings.simpleUnproto) {
        let uv = "";
        if (!settings.hideVia && hdr) {
          const m = hdr.match(/via\s+(\S+)/i);
          if (m && m[1]) uv = " via " + esc(m[1]);
        }
        out += `<div class="u-header"><span class="h-time">${esc(entry.ts)}</span> <span class="h-src">${esc(entry.src)}</span> to <span class="h-dst">${esc(entry.dst || "")}</span><span class="h-via">${uv}</span></div>`;
      } else {
        const q = hdr.replace(/^radio:\s*(?:fm\s+)?/, entry.ts + " ").replace(/^(\d{2}:\d{2}:\d{2})\s+(\S+)\s+to\s+(\S+?)(?:\s+(via\s+\S+(?:\s+\S+)*?(?=\s+ctl\s|\s+len\s|$)))?\s+(.*)$/, (m, t, s, d, v, rest) => `<span class="h-time">${esc(t)}</span> <span class="h-src">${esc(s)}</span> <span class="h-to">to</span> <span class="h-dst">${esc(d)}</span> <span class="h-via">${esc(v || "")}</span> <span class="h-meta">${esc(rest)}</span>`);
        out += `<div class="u-header">${q}</div>`;
      }
    } else {
      out += `<div class="u-header"><span class="h-time">${esc(entry.ts)}</span> <span class="h-src">${esc(entry.src)}</span> <span class="h-meta">${esc(entry.dst || "")} ${esc(ft)}</span></div>`;
    }
    const ctrlFrames = ["SABM+", "SABM-", "UA+", "UA-", "DISC+", "DISC-", "DM+", "DM-", "FRMR+", "FRMR-", "SABME+", "SABME-", "RR", "RNR", "REJ", "SREJ", "XID+", "XID-"];
    const lines = entry.payload.split(/[\r\n]+/).filter((l) => l.length > 0);
    if (lines.length > 0 && !ctrlFrames.includes(ft)) {
      for (let li = 0; li < lines.length; li++) {
        out += `<div class="${entry.sent ? "u-payload sent" : "u-payload"}">${esc(lines[li])}</div>`;
      }
    }
    ee.innerHTML = out;
    unprotoData.appendChild(ee);
    unprotoCount++;
  }
  uCount.textContent = `(${unprotoCount})`;
  unprotoData.scrollTop = unprotoData.scrollHeight;
  const mxd = settings.maxLines || 500;
  while (unprotoData.children.length > mxd) {
    unprotoData.removeChild(unprotoData.firstChild);
    unprotoCount--;
    uCount.textContent = `(${unprotoCount})`;
  }
}

// Unproto toolbar toggles
$("#u-dedup-btn").onclick = async () => {
  settings.deduplicatePackets = !settings.deduplicatePackets;
  $("#u-dedup-btn").classList.toggle("active", settings.deduplicatePackets);
  const dd = document.querySelector("#set-dedup");
  if (dd) dd.checked = settings.deduplicatePackets;
  renderUnproto();
  savePathwayField("deduplicatePackets", settings.deduplicatePackets);
};
$("#u-simple-btn").onclick = async () => {
  settings.simpleUnproto = !settings.simpleUnproto;
  $("#u-simple-btn").classList.toggle("active", settings.simpleUnproto);
  const ds = document.querySelector("#set-simple-unproto");
  if (ds) ds.checked = settings.simpleUnproto;
  renderUnproto();
  const lp = document.getElementById("left-panel");
  const dv = document.getElementById("divider");
  if (lp) lp.style.display = settings.onlyUnproto ? "none" : "";
  if (dv) dv.style.display = settings.onlyUnproto ? "none" : "";
  savePathwayField("simpleUnproto", settings.simpleUnproto);
};
$("#u-only-btn").onclick = async () => {
  settings.onlyUnproto = !settings.onlyUnproto;
  $("#u-only-btn").classList.toggle("active", settings.onlyUnproto);
  const ds2 = document.querySelector("#set-only-unproto");
  if (ds2) ds2.checked = settings.onlyUnproto;
  const lp = document.getElementById("left-panel");
  const dv = document.getElementById("divider");
  if (lp) lp.style.display = settings.onlyUnproto ? "none" : "";
  if (dv) dv.style.display = settings.onlyUnproto ? "none" : "";
  savePathwayField("onlyUnproto", settings.onlyUnproto);
};
uClear.onclick = () => {
  unprotoData.innerHTML = "";
  unprotoCount = 0;
  uCount.textContent = "(0)";
  unprotoFrames.length = 0;
};
$("#left-clear").onclick = () => {
  if (settings.rawKissDisplay) { rawKissEntries = []; renderTab(activeTab); }
  else { tabs[activeTab].msgs = []; renderTab(activeTab); }
};
unprotoSend.onclick = () => sendUnproto();

function saveUnprotoTo() {
  settings.unprotoTo = unprotoTo.value.trim().toUpperCase();
  savePathwayField("unprotoTo", settings.unprotoTo);
}
function savePathwayField(field, val) {
  // Merge-save a single settings field: load the full settings from disk,
  // overlay ONLY this field, then save. Re-saving the module snapshot (as the
  // old code did) can clobber newer disk changes made by other components
  // (e.g. beacon.js disabling the beacon) and even re-enable things the user
  // turned off.
  (async () => {
    let base;
    try { base = await invoke("load_settings"); } catch (e) { base = null; }
    const merged = (base && typeof base === "object" && Object.keys(base).length) ? base : { ...DEFAULT_SETTINGS };
    merged[field] = val;
    try { await invoke("save_settings", { settings: merged }); }
    catch (e) { console.error("save_settings failed for " + field, e); }
  })();
}
function saveUnprotoVia() {
  settings.unprotoVia = unprotoVia.value.trim().toUpperCase();
  savePathwayField("unprotoVia", settings.unprotoVia);
}
let unprotoToSaveTimer = null;
let unprotoViaSaveTimer = null;
function scheduleUnprotoToSave() { clearTimeout(unprotoToSaveTimer); unprotoToSaveTimer = setTimeout(saveUnprotoTo, 300); }
function scheduleUnprotoViaSave() { clearTimeout(unprotoViaSaveTimer); unprotoViaSaveTimer = setTimeout(saveUnprotoVia, 300); }
unprotoTo.addEventListener("input", scheduleUnprotoToSave);
unprotoTo.onchange = saveUnprotoTo;
unprotoVia.addEventListener("input", scheduleUnprotoViaSave);
unprotoVia.onchange = saveUnprotoVia;

unprotoMsg.onkeydown = (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    sendUnproto();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (unprotoCmdHistory.length && unprotoHistIdx > -1) {
      if (unprotoHistIdx > 0) unprotoHistIdx--;
      unprotoMsg.value = unprotoCmdHistory[unprotoHistIdx];
    }
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    if (unprotoCmdHistory.length && unprotoHistIdx < unprotoCmdHistory.length - 1) {
      unprotoHistIdx++;
      unprotoMsg.value = unprotoCmdHistory[unprotoHistIdx];
    } else if (unprotoHistIdx >= unprotoCmdHistory.length - 1) {
      unprotoHistIdx = unprotoCmdHistory.length;
      unprotoMsg.value = "";
    }
  }
};

async function sendUnproto() {
  const to = unprotoTo.value.trim().toUpperCase();
  const msg = unprotoMsg.value.trim();
  const via = unprotoVia.value.trim();
  settings.unprotoTo = to;
  settings.unprotoVia = via.trim().toUpperCase();
  savePathwayField("unprotoTo", settings.unprotoTo);
  savePathwayField("unprotoVia", settings.unprotoVia);
  if (!to || !msg || !conn || !conn.cx) return;
  const parts = to.split("-");
  const base = parts[0];
  const ssid = (parts.length > 1 && parseInt(parts[1])) || 0;
  try {
    await invoke(connCmd("ax25_send_ui", "agw_send_ui"), { connId: conn.id, dest: base, destSsid: ssid, text: msg, via });
    unprotoCmdHistory.unshift(msg);
    unprotoHistIdx = unprotoCmdHistory.length;
    unprotoMsg.value = "";
    unprotoStatus.textContent = "";
  } catch (err) {
    unprotoStatus.textContent = "\u2715 " + err;
    unprotoStatus.style.color = "var(--red)";
  }
}

// ── Settings load / save ──
// Mirror of the Rust-side migration in settings.rs: stored font fields can be
// legacy absolute px (≤0.11.17) or the experiment-round offsets. Normalize so
// the browser preview (whose shim bypasses Rust) behaves exactly like the real
// app: app = absolute px 10-25 (old offset ≤5 → default 18); conn/unproto =
// offsets -5..+5 (legacy px p → offset p - base, so it renders the same).
function normalizeFontSettings(s) {
  const a = s.appFontSize ?? 18;
  s.appFontSize = a <= 5 ? 18 : Math.min(25, Math.max(10, a));
  const base = s.appFontSize;
  const norm = (v, fallback) => {
    const n = v ?? fallback;
    return n > 5 ? n - base : Math.min(5, Math.max(-5, n));
  };
  s.connFontSize = norm(s.connFontSize, -2);
  s.unprotoFontSize = norm(s.unprotoFontSize, -2);
  return s;
}

function applyFontsAndTheme() {
  // App Font Size is an absolute px base (10-25). The whole chrome (topbar,
  // buttons, badges, tabs, dialogs — everything not pane-specific) derives
  // from --app-font-size via em units. Connected-mode and Unproto panes are
  // OFFSETS (-5..+5) from that base: base 18 + -2 → 16px, and when the base
  // moves they ride along (base 20 + -2 → 18px). So one "bigness" knob
  // scales everything, and the two pane knobs fine-tune each pane relative
  // to it. Rendered sizes are clamped 8-30 so extreme combos stay legible.
  const BASE_DEFAULT = 18; // settings.rs default base px
  const clampPx = (v) => Math.min(30, Math.max(8, v));
  const base = Math.min(25, Math.max(10, settings.appFontSize || BASE_DEFAULT));
  const conn = clampPx(base + (settings.connFontSize ?? -2));
  const unproto = clampPx(base + (settings.unprotoFontSize ?? -2));
  const px = (v) => Math.round(v) + "px";
  document.documentElement.style.setProperty("--app-font-size", px(base));
  document.documentElement.style.setProperty("--conn-font-size", px(conn));
  document.documentElement.style.setProperty("--unproto-font-size", px(unproto));
  document.documentElement.setAttribute("data-theme", settings.theme === "dark" ? "" : settings.theme || "");
  // Live hints in the settings modal: offset → rendered px
  const hint = (id, offset, rendered) => {
    const el = document.getElementById(id);
    if (el) el.textContent = "(" + (offset > 0 ? "+" : "") + offset + " → " + rendered + "px)";
  };
  hint("conn-fontsize-hint", settings.connFontSize ?? -2, conn);
  hint("unproto-fontsize-hint", settings.unprotoFontSize ?? -2, unproto);
}

function applyPanelVisibility() {
  const lp = document.getElementById("left-panel");
  const dv = document.getElementById("divider");
  if (lp) lp.style.display = settings.onlyUnproto ? "none" : "";
  if (dv) dv.style.display = settings.onlyUnproto ? "none" : "";
}

async function loadSettings() {
  try {
    const t = await invoke("load_settings");
    settings = normalizeFontSettings(t);
    settings.unprotoColors = void 0;
    if (t.callsign) hostEl.placeholder = t.callsign;
    if (t.host) hostEl.value = t.host;
    if (t.port) portEl.value = t.port;
    if (t.unprotoTo) unprotoTo.value = t.unprotoTo;
    if (t.unprotoVia) unprotoVia.value = t.unprotoVia;
    applyFontsAndTheme();
    if (t.autoConnect && t.callsign && t.host) setTimeout(() => connectBtn.click(), 500);
    $("#u-dedup-btn").classList.toggle("active", settings.deduplicatePackets);
    $("#u-simple-btn").classList.toggle("active", settings.simpleUnproto);
    $("#u-only-btn").classList.toggle("active", settings.onlyUnproto);
    applyPanelVisibility();
  } catch (e) {
    settings = { ...DEFAULT_SETTINGS };
    $("#u-dedup-btn").classList.toggle("active", settings.deduplicatePackets);
    $("#u-simple-btn").classList.toggle("active", settings.simpleUnproto);
  }
}

loadSettings();

// Settings modal: populate from settings when opened

// ── Settings modal: single declarative field spec ─────────
// Each entry maps one form input <-> one settings key, with explicit
// pull (input -> settings value on save) and push (settings -> input on
// populate). Driving both directions from ONE table removes the hand-rolled
// duplication that let the two halves drift apart. The save button uses the
// documented merge-save strategy (load full settings from disk, overlay only
// the fields the form owns), matching the connect path.
const SETTING_FIELDS = [
  { id: "set-call", key: "callsign",
    pull: (el) => el.value.toUpperCase(),
    push: (el, v) => { el.value = v || ""; } },
  // host/port default to the live connection during populate (see fieldsPopulate).
  { id: "set-host", key: "host",
    pull: (el) => el.value,
    push: (el, v) => { el.value = v || ""; } },
  { id: "set-port", key: "port",
    pull: (el) => parseInt(el.value) || 8000,
    push: (el, v) => { el.value = v ?? 8000; } },
  { id: "set-autoconnect", key: "autoConnect",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-kiss-p", key: "kissP",
    pull: (el) => parseInt(el.value) || 63,
    push: (el, v) => { el.value = v ?? 63; } },
  { id: "set-kiss-slot", key: "kissSlotTime",
    pull: (el) => parseInt(el.value) || 10,
    push: (el, v) => { el.value = v ?? 10; } },
  { id: "set-kiss-txdelay", key: "kissTxdelay",
    pull: (el) => parseInt(el.value) || 30,
    push: (el, v) => { el.value = v ?? 30; } },
  // AX.25 timers are stored in ms but shown/edited in deciseconds.
  { id: "set-kiss-t1", key: "kissT1Ms",
    pull: (el) => (parseInt(el.value) || 1000) * 10,
    push: (el, v) => { el.value = (v ?? 10000) / 10; } },
  { id: "set-kiss-t2", key: "kissT2Ms",
    pull: (el) => (parseInt(el.value) || 100) * 10,
    push: (el, v) => { el.value = (v ?? 1000) / 10; } },
  { id: "set-kiss-t3", key: "kissT3Ms",
    pull: (el) => (parseInt(el.value) || 30000) * 10,
    push: (el, v) => { el.value = (v ?? 300000) / 10; } },
  { id: "set-kiss-kacheck", key: "kissKaCheckMs",
    pull: (el) => (parseInt(el.value) || 6000) * 10,
    push: (el, v) => { el.value = (v ?? 60000) / 10; } },
  { id: "set-kiss-n2", key: "kissN2Max",
    pull: (el) => parseInt(el.value) || 5,
    push: (el, v) => { el.value = v ?? 5; } },
  { id: "set-mb-enabled", key: "messageBoardEnabled",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  // MB callsign is one input holding "CALL-SSID"; split on pull, compose on push.
  { id: "set-mb-callsign", key: "messageBoardCallsign",
    pull: (el) => { const m = el.value.toUpperCase().match(/^(.*)-(\d+)$/); return m ? m[1] : el.value.toUpperCase(); },
    push: () => {} },  // composed specially below from both halves
  { id: "set-mb-sysops", key: "messageBoardSysops",
    pull: (el) => el.value.toUpperCase(),
    push: (el, v) => { el.value = v || ""; } },
  { id: "set-mb-privacy", key: "messageBoardPrivacy",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-mb-aliases", key: "messageBoardPublicAliases",
    pull: (el) => el.value.toUpperCase() || "ALL",
    push: (el, v) => { el.value = v || "ALL"; } },
  { id: "set-mb-maxpacket", key: "messageBoardMaxPacketSize",
    pull: (el) => Math.min(256, Math.max(32, parseInt(el.value) || 250)),
    push: (el, v) => { el.value = v || 250; } },
  { id: "set-beacon-enabled", key: "beaconEnabled",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-beacon", key: "beacon",
    pull: (el) => el.value,
    push: (el, v) => { el.value = v || ""; } },
  { id: "set-beacon-int", key: "beaconInterval",
    pull: (el) => parseInt(el.value) || 1,
    push: (el, v) => { el.value = v || 1; } },
  { id: "set-beacon-dest", key: "beaconDest",
    pull: (el) => el.value.toUpperCase() || "BEACON",
    push: (el, v) => { el.value = v || "BEACON"; } },
  { id: "set-beacon-via", key: "beaconVia",
    pull: (el) => el.value.toUpperCase() || "",
    push: (el, v) => { el.value = v || ""; } },
  { id: "set-app-fontsize", key: "appFontSize",
    pull: (el) => Math.min(25, Math.max(10, parseInt(el.value, 10) || 18)),
    push: (el, v) => { el.value = v ?? 18; } },
  { id: "set-conn-fontsize", key: "connFontSize",
    pull: (el) => { const v = parseInt(el.value, 10); return Number.isNaN(v) ? -2 : Math.min(5, Math.max(-5, v)); },
    push: (el, v) => { el.value = v ?? -2; } },
  { id: "set-unproto-fontsize", key: "unprotoFontSize",
    pull: (el) => { const v = parseInt(el.value, 10); return Number.isNaN(v) ? -2 : Math.min(5, Math.max(-5, v)); },
    push: (el, v) => { el.value = v ?? -2; } },
  { id: "set-rawkiss", key: "rawKissDisplay",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-debugtx", key: "debugTxEnabled",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = v !== false; } },
  { id: "set-dedup", key: "deduplicatePackets",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-theme", key: "theme",
    pull: (el) => el.value,
    push: (el, v) => { el.value = v || "dark"; } },
  { id: "set-simple-unproto", key: "simpleUnproto",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-only-unproto", key: "onlyUnproto",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-hide-via", key: "hideVia",
    pull: (el) => el.checked,
    push: (el, v) => { el.checked = !!v; } },
  { id: "set-ignored-dest", key: "unprotoIgnoredDestinations",
    pull: (el) => el.value.toUpperCase(),
    push: (el, v) => { el.value = v || ""; } },
  { id: "set-protocol", key: "protocol",
    pull: (el) => el.value,
    push: (el, v) => { el.value = v || "KISS"; } },
];

function fieldsPopulate(prefs) {
  SETTING_FIELDS.forEach((f) => {
    if (f.id === "set-mb-callsign") return;
    f.push($("#" + f.id), prefs[f.key]);
  });
  // host/port default to the live connection when opened, else saved values.
  $("#set-host").value = hostEl.value || prefs.host || "";
  $("#set-port").value = portEl.value || (prefs.port ?? 8000);
  // MB callsign input is composed from the callsign + ssid halves.
  const mbv = prefs.messageBoardCallsign
    ? prefs.messageBoardCallsign + "-" + (typeof prefs.messageBoardSsid === "number" ? prefs.messageBoardSsid : 1)
    : (prefs.callsign ? prefs.callsign.toUpperCase() + "-1" : "");
  $("#set-mb-callsign").value = mbv;
  // Conditional visibility driven by current values (not part of the value spec).
  $("#kiss-settings-group").style.display = prefs.protocol === "KISS" ? "" : "none";
  $("#mb-aliases-row").style.display = prefs.messageBoardPrivacy ? "" : "none";
  // Sync the custom protocol select's visible label.
  const _sl = document.getElementById("cs-protocol-selected");
  const _tx = _sl && _sl.querySelector(".cs-text");
  const _op = document.querySelector('.cs-opt[data-value="' + prefs.protocol + '"]');
  if (_tx && _op) _tx.innerHTML = _op.innerHTML;
}

function fieldsCollect(base) {
  // Merge the form's fields onto an existing settings object (merge-save).
  SETTING_FIELDS.forEach((f) => {
    if (f.id === "set-mb-callsign") {
      const v = $("#" + f.id).value.toUpperCase();
      const m = v.match(/^(.*)-(\d+)$/);
      base.messageBoardCallsign = m ? m[1] : v;
      base.messageBoardSsid = m ? parseInt(m[2]) : 1;
      return;
    }
    base[f.key] = f.pull($("#" + f.id));
  });
  return base;
}

// Settings modal: populate from settings when opened
settingsBtn.onclick = () => {
  const prefs = Object.keys(settings).length ? settings : { ...DEFAULT_SETTINGS };
  fieldsPopulate(prefs);
  settingsOverlay.classList.add("open");
  $("#u-dedup-btn").classList.toggle("active", settings.deduplicatePackets);
  $("#u-simple-btn").classList.toggle("active", settings.simpleUnproto);
};

$("#close-settings").onclick = () => settingsOverlay.classList.remove("open");
$("#settings-cancel").onclick = () => settingsOverlay.classList.remove("open");
$("#settings-reset").onclick = async () => {
  try {
    await invoke("reset_settings");
    settingsOverlay.classList.remove("open");
    document.getElementById("reset-overlay").classList.add("open");
  } catch (e) {
    console.error("reset failed", e);
  }
};
$("#close-reset-overlay").onclick = () => document.getElementById("reset-overlay").classList.remove("open");

$("#settings-save").onclick = async () => {
  // Merge-save: start from the full settings on disk and overlay ONLY the
  // fields the modal owns (via the declarative spec), so no unrelated field
  // is ever wiped — the same strategy the connect path uses.
  let saved;
  try { saved = await invoke("load_settings"); } catch (e) { saved = null; }
  const base = (saved && typeof saved === "object" && Object.keys(saved).length) ? saved : { ...DEFAULT_SETTINGS };
  fieldsCollect(base);
  // The two unproto compose inputs are outside the modal but were part of
  // the old save; keep writing them so their content persists.
  base.unprotoTo = unprotoTo.value.trim();
  base.unprotoVia = unprotoVia.value.trim();
  settings = base;
  applyFontsAndTheme();
  hostEl.value = settings.host;
  portEl.value = settings.port;
  hostEl.placeholder = "IP or hostname";
  try {
    await invoke("save_settings", { settings });
  } catch (e) {
    console.error("save failed", e);
  }
  $("#u-simple-btn").classList.toggle("active", settings.simpleUnproto);
  $("#u-dedup-btn").classList.toggle("active", settings.deduplicatePackets);
  settingsOverlay.classList.remove("open");
  renderUnproto();
  renderTab(activeTab);
};

// ── Events from backend (Tauri emit) ──
listen("session-connected", (ev) => {
  const { tab, remote } = ev.payload;
  if (tab < TAB_COUNT) {
    tabs[tab].state = "connected";
    tabs[tab].remote = remote;
    tabs[tab].ns = 0;
    tabs[tab].nr = 0;
    refreshTabBar();
    tabMsg(tab, "sys", `Connected to ${remote}`);
  }
});

listen("session-disconnected", (ev) => {
  const { tab, reason = "Disconnected" } = ev.payload;
  if (tab < TAB_COUNT) {
    tabs[tab].state = "disconnected";
    tabs[tab].remote = "";
    refreshTabBar();
    tabMsg(tab, "sys", reason);
    if (retryTimer) { clearInterval(retryTimer); retryTimer = null; }
    retryIndicator.className = "retry-indicator";
  }
});

listen("session-iframe", (ev) => {
  const { tab, text } = ev.payload;
  if (tab >= TAB_COUNT) return;
  const norm = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  if (norm.trimEnd() === tabs[tab].lastSentText) {
    tabs[tab].lastSentText = null;
    return;
  }
  tabs[tab].lastSentText = null;
  const t = tabs[tab];
  if (t) {
    const last = t.msgs[t.msgs.length - 1];
    if (last && last.dir === "rx") {
      last.text += norm;
      if (tab === activeTab) renderTab(tab);
    } else {
      tabMsg(tab, "rx", norm);
    }
  }
});

listen("session-sent", (ev) => {
  const { tab, text } = ev.payload;
  if (tab < TAB_COUNT) {
    tabs[tab].lastSentText = text;
    tabMsg(tab, "tx", text);
  }
});

listen("ui-sent", (ev) => {
  const { dest, text, radio_header, raw_kiss } = ev.payload;
  const callsign = settings.callsign || "N0CALL";
  ingestUnproto(callsign, dest, text, timeStr(), "UI+", radio_header || "", raw_kiss || "", true);
});

listen("session-connecting", (ev) => {
  const { tab, remote } = ev.payload;
  if (tab < TAB_COUNT) {
    tabs[tab].state = "connecting";
    tabs[tab].remote = remote;
    refreshTabBar();
  }
});

listen("unproto-frame", (ev) => {
  const { source, dest, info, frame_type, radio_header, raw_kiss_data, sent } = ev.payload;
  ingestUnproto(source, dest, info, timeStr(), frame_type || "UI-", radio_header || "", raw_kiss_data || "", !!sent);
});

listen("tnc-connected", (ev) => {
  if (conn && conn.id === ev.payload.id) {
    conn.cx = true;
    setConnStatus("connected", "Connected");
    connectBtn.textContent = "Disconnect";
    connectBtn.className = "disconnect";
    hostEl.disabled = true;
    portEl.disabled = true;
    sendBtn.disabled = false;
    unprotoSend.disabled = false;
  }
});

listen("tnc-disconnected", (ev) => {
  if (conn && conn.id === ev.payload.id) disconnectTnc();
});

// ── Retry / ACK indicator ──
const retryIndicator = $("#retry-indicator");
let retryTimer = null;
listen("session-retry", (ev) => {
  const { tab, retry, max, text, t1_ms } = ev.payload;
  if (tab !== activeTab) return;
  retryIndicator.textContent = "\u23F3 Retry " + retry + "/" + max + ": " + esc(text || "").slice(0, 40) + ((text || "").length > 40 ? "..." : "") + " (" + (t1_ms / 1000).toFixed(1) + "s)";
  retryIndicator.className = "retry-indicator show";
  retryIndicator.style.color = retry >= 3 ? "#ff6600" : retry >= 2 ? "#cccc00" : "#00cc00";
  if (retryTimer) clearInterval(retryTimer);
  let count = t1_ms / 1000;
  retryTimer = setInterval(() => {
    count -= 0.1;
    if (count <= 0) {
      clearInterval(retryTimer);
      retryTimer = null;
      retryIndicator.textContent = retryIndicator.textContent.replace(/\(\d+\.\d+s\)/, "(0.0s)");
      return;
    }
    retryIndicator.textContent = retryIndicator.textContent.replace(/\(\d+\.\d+s\)/, `(${Math.max(0, count).toFixed(1)}s)`);
  }, 100);
});
listen("session-ack", (ev) => {
  const { tab } = ev.payload;
  if (tab === activeTab) {
    retryIndicator.textContent = "\u2713 ACK";
    retryIndicator.className = "retry-indicator show";
    retryIndicator.style.color = "#006600";
    setTimeout(() => { retryIndicator.className = "retry-indicator"; }, 2000);
  }
  if (retryTimer) { clearInterval(retryTimer); retryTimer = null; }
});

// ── Window state persistence ──
let winStateTimer = null;
async function saveWindowStateDebounced() {
  if (winStateTimer) clearTimeout(winStateTimer);
  winStateTimer = setTimeout(async () => {
    try {
      const win = getCurrentWindow();
      if (await win.isMaximized()) {
        await invoke("save_window_state", { width: 0, height: 0, x: 0, y: 0, maximized: true });
      } else {
        const size = await win.outerSize();
        const pos = await win.outerPosition();
        await invoke("save_window_state", { width: size.width, height: size.height, x: pos.x, y: pos.y, maximized: false });
      }
    } catch (e) {}
  }, 1000);
}
const currentWindow = getCurrentWindow();
currentWindow.onResized(() => saveWindowStateDebounced());
currentWindow.onMoved(() => saveWindowStateDebounced());

// ── Misc UI wiring ──
fullscreenBtn.onclick = () => document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen();
$("#help-btn").onclick = () => $("#help-overlay").classList.add("open");
$("#close-help").onclick = () => $("#help-overlay").classList.remove("open");
$("#help-overlay").onclick = (e) => { if (e.target === $("#help-overlay")) $("#help-overlay").classList.remove("open"); };

// Tab switching
function selectTab(idx) {
  activeTab = idx;
  modeTabs.forEach((e, n) => e.classList.toggle("active", n === idx));
  renderTab(idx);
}
modeTabs.forEach((t, e) => (t.onclick = () => selectTab(e)));

// Left panel drag-resize
let dragging = false;
divider.onmousedown = (e) => {
  dragging = true;
  divider.classList.add("active");
  document.body.style.cursor = "col-resize";
};
document.onmousemove = (e) => {
  if (!dragging) return;
  const lp = document.getElementById("left-panel");
  const x = e.clientX;
  lp.style.width = Math.max(250, Math.min(x - 2, window.innerWidth - 350)) + "px";
  lp.style.flex = "none";
};
document.onmouseup = () => {
  if (dragging) {
    dragging = false;
    divider.classList.remove("active");
    document.body.style.cursor = "";
  }
};

// ── Init ──
setConnStatus("idle", "Disconnected");
sendBtn.disabled = true;
unprotoSend.disabled = true;
selectTab(0);
renderTab(activeTab);
hostEl.focus();