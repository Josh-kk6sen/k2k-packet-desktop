// K2K Packet Desktop — Daily log capture.
// Migrated verbatim from index.html inline <script> (Phase A). Preserves the
// exact behavior: writes one file per day per kind into the app data dir via
// the Rust `append_log` command; `openLogDir` + `logTs` + `logTo` are exposed
// on window (openLogDir is called from an inline onclick in index.html).
function logTo(kind, line) {
  try {
    window.__TAURI_INTERNALS__.invoke("append_log", { kind: kind, text: line }).catch(function () {});
  } catch (e) {}
}
window.logTo = logTo;
const logTs = function () { return new Date().toLocaleTimeString(); };
window.logTs = logTs;
window.onerror = function (msg, url, line, col, err) {
  var detail = (err && err.stack) ? ("\n" + err.stack) : "";
  logTo("raw", "JS ERROR: " + msg + " at " + line + ":" + col + detail);
};
window.addEventListener("unhandledrejection", function (e) {
  logTo("raw", "JS UNHANDLED PROMISE: " + (e.reason || e));
});
window.openLogDir = function () {
  try { window.__TAURI_INTERNALS__.invoke("open_log_dir").catch(function () {}) } catch (e) {}
};

(function () {
  // Settings modal: click outside closes it.
  var settingsOverlay = document.getElementById("settings-overlay");
  if (settingsOverlay) settingsOverlay.onclick = function (e) { if (e.target === settingsOverlay) settingsOverlay.classList.remove("open"); };

  // NOTE: window.__TAURI__ is NOT exposed in this build (withGlobalTauri off),
  // so use the plugin:event|listen invoke path. __TAURI_INTERNALS__ is always
  // present inside Tauri.
  function on(evt, fn) {
    try {
      var handler = window.__TAURI_INTERNALS__.transformCallback(function (ev) {
        fn((ev && ev.payload !== undefined) ? ev.payload : ev);
      });
      window.__TAURI_INTERNALS__.invoke("plugin:event|listen", { event: evt, target: { kind: "Any" }, handler: handler })
        .then(function () { logTo("raw", "log capture: listening " + evt); })
        .catch(function (e) { logTo("raw", "log capture: FAILED " + evt + ": " + e); });
    } catch (e) { logTo("raw", "log capture: error " + evt + ": " + e); }
  }
  // Unproto monitor data
  on("unproto-frame", function (p) {
    var hdr = (p.sent ? "TX " : "") + (p.radio_header ? p.radio_header : "");
    logTo("unproto", "[" + logTs() + "] " + (hdr ? hdr + " " : "") + p.source + (p.dest ? " to " + p.dest : "") + " " + (p.frame_type || "UI") + (p.info ? (" " + p.info) : ""));
  });
  // Connected-mode tab conversation
  on("session-iframe", function (p) { logTo("connected", "[" + logTs() + "] RX: " + p.text); });
  on("session-sent", function (p) { logTo("connected", "[" + logTs() + "] TX: " + p.text); });
  on("session-connecting", function (p) { logTo("connected", "[" + logTs() + "] connecting to " + (p.remote || "")); });
  on("session-connected", function (p) { logTo("connected", "[" + logTs() + "] Connected to " + (p.remote || "")); });
  on("session-disconnected", function (p) { logTo("connected", "[" + logTs() + "] disconnected: " + (p.reason || "")); });
})();