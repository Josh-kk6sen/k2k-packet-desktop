// Headless verification for src/app.js (Phase B port of the shipped bundle's
// app logic). Verifies the module loads under a jsdom DOM + Tauri stub with no
// runtime errors, and that the critical DOM wiring is present.
//
// NOTE: this one-off harness is NOT the permanent CI test (that's
// tests/frontend/smoke_test.mjs). app.js is intentionally excluded from the
// build/index until Phase C swaps out the shipped bundle, and needs the real
// @tauri-apps/api which the permanent harness stubs out. This exists to prove
// the port runs before it's wired in.
import { createRequire } from "module";
const require = createRequire(import.meta.url);
const { JSDOM } = require("jsdom");
import fs from "fs";
import { pathToFileURL } from "url";
import path from "path";

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

// Record mock invoke calls so tests can assert the right commands are issued.
const invokeCalls = [];
w.__TAURI_INTERNALS__ = {
  transformCallback: (cb) => cb,
  metadata: { currentWindow: { label: "main" } },
  invoke: (cmd, args) => {
    invokeCalls.push({ cmd, args });
    switch (cmd) {
      case "load_settings": return Promise.resolve({ protocol: "AGWPE", callsign: "KK6SEN", host: "192.168.1.100", port: 8000, beaconEnabled: false, beaconInterval: 240, unprotoColors: null, theme: "dark", messageBoardEnabled: false, autoConnect: false, appFontSize: 15, connFontSize: 14, unprotoFontSize: 14, unprotoTo: "BEACON", unprotoVia: "" });
      case "save_settings": return Promise.resolve(null);
      case "reset_settings": return Promise.resolve(null);
      case "load_history": return Promise.resolve([]);
      case "save_history": return Promise.resolve(null);
      case "remove_history": return Promise.resolve(null);
      case "append_log": return Promise.resolve(null);
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
let intervals = [];
global.window = w;
global.document = w.document;
global.getComputedStyle = w.getComputedStyle.bind(w);
global.MutationObserver = w.MutationObserver;
global.requestAnimationFrame = (cb) => setTimeout(cb, 16);
global.Event = w.Event;
global.setInterval = (cb, ms) => { intervals.push(cb); return intervals.length; };
global.setTimeout = (cb, ms) => nodeSetTimeout(cb, ms || 0);
global.clearInterval = () => {};
global.clearTimeout = (id) => nodeClearTimeout(id);

try {
  await import(pathToFileURL(path.join(ROOT, "src", "app.js")));
  console.log("app.js loaded (no throw)");
} catch (e) {
  errors.push("app.js threw: " + (e && (e.stack || e.message) || e));
}

await new Promise((r) => setTimeout(r, 300));

// Assert DOM wiring happened
const checks = {
  "connect-btn wired": typeof w.document.getElementById("connect-btn").onclick === "function",
  "unproto-data present": !!w.document.getElementById("unproto-data"),
  "settings-btn wired": typeof w.document.getElementById("settings-btn").onclick === "function",
  "u-count present": !!w.document.getElementById("u-count"),
};

console.log("=== DOM wiring checks ===");
for (const [k, v] of Object.entries(checks)) console.log(`  ${k}: ${v ? "OK" : "FAIL"}`);
console.log("=== runtime errors ===");
console.log(errors.length ? errors : "(none)");
console.log("=== invoke saw load_settings + save_window_state path ===");
const cmds = [...new Set(invokeCalls.map((c) => c.cmd))];
console.log("captured invoke commands:", cmds.join(", "));

const failing = Object.values(checks).some((v) => !v) || errors.length;
setTimeout(() => process.exit(failing ? 1 : 0), 200);