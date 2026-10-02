// K2K Packet Desktop frontend CI smoke test (Framework 3, post-Phase-C).
//
// Loads the REAL production entry tree — src/main.js and everything it imports
// (logging, pbbs, protocol, history, beacon, font-colors, app) — under a jsdom
// DOM + Tauri-internals stub. Asserts:
//   1. every migrated/app module loads with zero runtime errors,
//   2. all window globals referenced by inline HTML handlers are present,
//   3. the app's critical DOM wiring (connect, settings, unproto) is in place.
//
// This exercises the exact module set the shipped build compiles, so it is the
// gate that replaces "node --check / esbuild VALID" for the frontend.
// Run: npm run test:frontend   (exit 0 = pass)
import { createRequire } from "module";
const require = createRequire(import.meta.url);
const { JSDOM } = require("jsdom");
import fs from "fs";
import { pathToFileURL } from "url";
import path from "path";

// Resolve the repo root from this file's own location so the test is portable
// (works in CI and on any checkout, not just a specific dev machine).
import { fileURLToPath } from "url";
const __dir = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dir, "../..");
const html = fs.readFileSync(path.join(ROOT, "index.html"), "utf8");
const errors = [];

const dom = new JSDOM(html, {
  url: "http://localhost/",
  runScripts: "dangerously",
  pretendToBeVisual: true,
});
const w = dom.window;

w.__TAURI_INTERNALS__ = {
  transformCallback: (cb) => cb,
  metadata: { currentWindow: { label: "main" } },
  invoke: (cmd, args) => {
    switch (cmd) {
      case "load_settings": return Promise.resolve({ protocol: "AGWPE", callsign: "KK6SEN", host: "192.168.1.100", port: 8000, beaconEnabled: false, beaconInterval: 240, unprotoColors: null, theme: "dark", messageBoardEnabled: false, autoConnect: false, appFontSize: 15, connFontSize: 14, unprotoFontSize: 14, unprotoTo: "BEACON", unprotoVia: "", deduplicatePackets: false, simpleUnproto: false, onlyUnproto: false });
      case "save_settings": return Promise.resolve(null);
      case "reset_settings": return Promise.resolve(null);
      case "load_history": return Promise.resolve([]);
      case "save_history": return Promise.resolve(null);
      case "remove_history": return Promise.resolve(null);
      case "append_log": return Promise.resolve(null);
      case "open_log_dir": return Promise.resolve(null);
      case "pbbs_admin_open": return Promise.resolve({ lines: [], hangup: false });
      case "plugin:resources|close": return Promise.resolve(null);
      case "plugin:event|listen": return Promise.resolve(null);
      case "plugin:window|outerSize": return Promise.resolve({ width: 1024, height: 768 });
      case "plugin:window|outerPosition": return Promise.resolve({ x: 0, y: 0 });
      case "plugin:window|isMaximized": return Promise.resolve(false);
      case "save_window_state": return Promise.resolve(null);
      default: return Promise.resolve(null);
    }
  },
};
w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
w.addEventListener("error", (e) => errors.push("window.onerror: " + (e.message || String(e.error))));

const nodeSetTimeout = global.setTimeout;
const nodeClearTimeout = global.clearTimeout;
global.window = w;
global.document = w.document;
global.getComputedStyle = w.getComputedStyle.bind(w);
global.MutationObserver = w.MutationObserver;
global.requestAnimationFrame = (cb) => setTimeout(cb, 16);
global.Event = w.Event;
global.setInterval = (cb) => { /* captured; not run */ return 1; };
global.setTimeout = (cb, ms) => nodeSetTimeout(cb, ms || 0);
global.clearInterval = () => {};
global.clearTimeout = (id) => nodeClearTimeout(id);

// Load the exact production entry graph main.js imports, in order.
const SRC = path.join(ROOT, "src");
const entryFiles = ["logging.js", "pbbs.js", "protocol.js", "history.js", "beacon.js", "font-colors.js", "app.js"];
for (const f of entryFiles) {
  try {
    await import(pathToFileURL(path.join(SRC, f)));
    console.log("loaded:", f);
  } catch (e) {
    errors.push(f + " threw: " + (e && (e.stack || e.message) || e));
  }
}

await new Promise((r) => setTimeout(r, 300));

// All window globals referenced by inline HTML handlers + app controllers.
const requiredGlobals = [
  "logTo", "logTs", "openLogDir", "openPbbsAdmin", "applyProtocolDefaults",
  "openBeaconConfirm", "openFontColors", "fcCancel", "fcSave", "fcDefaults",
  "fcResetRow", "fcPick", "fcHexText", "fcHexBlur", "fcHexBlur",
  "showSuggestions", "hideSuggestions", "pickSuggestion", "deleteSuggestion",
  "setupCmdHistory",
];
const missing = requiredGlobals.filter((g) => typeof w[g] !== "function");

// DOM wiring from app.js
const domChecks = {
  "connect-btn wired": typeof w.document.getElementById("connect-btn").onclick === "function",
  "settings-btn wired": typeof w.document.getElementById("settings-btn").onclick === "function",
  "send-btn wired": typeof w.document.getElementById("send-btn").onclick === "function",
  "unproto-data present": !!w.document.getElementById("unproto-data"),
  "u-dedup-btn wired": typeof w.document.querySelector("#u-dedup-btn").onclick === "function",
};

// The shipped bundle must NOT still be referenced (we replaced it).
const bundleGone = !html.includes("/assets/index-DkQAlvay.js");

// Functional check: clicking the settings button must actually open the modal.
// (Regression guard for the SETTING_FIELDS id bug that threw in fieldsPopulate
// and aborted before the .open class was added.)
let settingsOpens = false;
let settingsOpenErr = null;
try {
  w.document.getElementById("settings-btn").click();
  settingsOpens = w.document.getElementById("settings-overlay").classList.contains("open");
} catch (e) {
  settingsOpenErr = e && (e.message || String(e));
}
const domChecksFn = {
  "settings click opens modal": settingsOpens && !settingsOpenErr,
};

console.log("\n=== window globals ===");
console.log("missing:", missing.length ? missing : "(none — all present)");
console.log("=== DOM wiring ===");
for (const [k, v] of Object.entries(domChecks)) console.log(`  ${k}: ${v ? "OK" : "FAIL"}`);
console.log("=== functional ===");
for (const [k, v] of Object.entries(domChecksFn)) console.log(`  ${k}: ${v ? "OK" : "FAIL"}` + (settingsOpenErr ? ` (${settingsOpenErr})` : ""));
console.log("=== shipped bundle removed from index.html ===");
console.log(bundleGone ? "yes" : "NO — still referenced!");
console.log("=== runtime errors ===");
console.log(errors.length ? errors : "(none)");

const failed = errors.length || missing.length || Object.values(domChecks).some((v) => !v) || Object.values(domChecksFn).some((v) => !v) || !bundleGone;
setTimeout(() => process.exit(failed ? 1 : 0), 200);