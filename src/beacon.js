// K2K Packet Desktop — Beacon indicator, settings-save validation, and the
// BEACON-indicator disable confirm.
// Migrated verbatim from index.html inline <script> (Phase A). `openBeaconConfirm`
// stays on window (called from the BEACON indicator's inline onclick).

// ── Beacon indicator + validation on save ──
async function updateBeaconIndicator() {
  // The settings-modal checkbox only reflects the value AFTER the modal has
  // been opened once — reading it alone hides the badge until the user
  // opens+closes Settings. Ask Rust for the live settings when the checkbox
  // says off.
  var beaconCheckbox = document.getElementById("set-beacon-enabled");
  var beaconIndicator = document.getElementById("beacon-indicator");
  var on = !!(beaconCheckbox && beaconCheckbox.checked);
  if (!on) {
    try {
      var s = await window.__TAURI_INTERNALS__.invoke("load_settings");
      on = !!(s && s.beaconEnabled);
    } catch (e) {}
  }
  if (beaconIndicator) beaconIndicator.className = on ? "" : "hidden";
}
setInterval(updateBeaconIndicator, 2000);
updateBeaconIndicator();

// Intercept settings save — block if beacon fields missing
document.getElementById("settings-save").addEventListener("click", function (e) {
  var beaconCheckbox = document.getElementById("set-beacon-enabled");
  if (beaconCheckbox && beaconCheckbox.checked) {
    var text = document.getElementById("set-beacon");
    var dest = document.getElementById("set-beacon-dest");
    var issues = [];
    if (!text || !text.value.trim()) issues.push("beacon text");
    if (!dest || !dest.value.trim()) issues.push("a beacon destination");
    if (issues.length) {
      e.stopImmediatePropagation();
      var msg = document.getElementById("beacon-warning-msg");
      var warningText = "";
      if (issues.length === 2) {
        warningText = "Add a beacon destination and the beacon text, or disable the beacon.";
      } else if (issues.indexOf("beacon text") >= 0) {
        warningText = "Add beacon text or disable the beacon.";
      } else {
        warningText = "Add a beacon destination or disable the beacon.";
      }
      if (msg) msg.textContent = warningText;
      document.getElementById("beacon-warning").classList.add("open");
      return;
    }
  }
  // Message board: an enabled board needs a valid callsign — 1-6 letters/
  // numbers with an optional -SSID (0-15). A malformed callsign can never
  // match a decoded AX.25 dest, so the board would silently never answer.
  var mbCheckbox = document.getElementById("set-mb-enabled");
  if (mbCheckbox && mbCheckbox.checked) {
    var mbField = document.getElementById("set-mb-callsign");
    var v = mbField ? mbField.value.trim().toUpperCase() : "";
    var m = v.match(/^([A-Z0-9]{1,6})(?:-(\d{1,2}))?$/);
    var ssidOk = m && (!m[2] || parseInt(m[2], 10) <= 15);
    if (!m || !ssidOk) {
      e.stopImmediatePropagation();
      var mbMsg = document.getElementById("mb-warning-msg");
      var mbText = !v
        ? "Enter a board callsign (e.g. KK6ABC-1) or disable the message board."
        : "\"" + v + "\" is not a valid callsign — use 1-6 letters/numbers with an optional -SSID (0-15), e.g. KK6ABC-1.";
      if (mbMsg) mbMsg.textContent = mbText;
      document.getElementById("mb-warning").classList.add("open");
    }
  }
}, true);

// ── Beacon disable confirm (BEACON indicator click) ──
window.openBeaconConfirm = function () {
  if (!window.__TAURI_INTERNALS__) return;
  document.getElementById("beacon-disable-confirm").classList.add("open");
};
(function () {
  var ok = document.getElementById("beacon-confirm-ok");
  var cancel = document.getElementById("beacon-confirm-cancel");
  var overlay = document.getElementById("beacon-disable-confirm");
  if (ok) ok.onclick = async function () {
    try {
      // Disable the beacon safely: load the FULL settings from disk, flip ONLY
      // beaconEnabled, and save. This preserves every other value (host, port,
      // font sizes, etc.) — it must NOT go through the Settings form's save
      // handler, which rebuilds settings from the form fields and would wipe
      // them if Settings was never opened.
      var s = await window.__TAURI_INTERNALS__.invoke("load_settings");
      if (s && typeof s === "object") {
        s.beaconEnabled = false;
        await window.__TAURI_INTERNALS__.invoke("save_settings", { settings: s });
      }
    } catch (e) {}
    // Uncheck the Settings checkbox now, and hide the indicator.
    var cb = document.getElementById("set-beacon-enabled");
    if (cb) cb.checked = false;
    var bi = document.getElementById("beacon-indicator");
    if (bi) bi.className = "hidden";
    if (overlay) overlay.classList.remove("open");
  };
  if (cancel) cancel.onclick = function () { if (overlay) overlay.classList.remove("open"); };
  if (overlay) overlay.onclick = function (e) { if (e.target === overlay) overlay.classList.remove("open"); };
  // The bundle caches settings in memory (loaded once at startup), so it won't
  // notice our beaconEnabled=false on next Settings open. Fix the beacon
  // checkbox to disk truth after the bundle populates the form (setTimeout
  // defers past the bundle's synchronous open handler).
  var settingsOpenBtn = document.getElementById("settings-btn");
  if (settingsOpenBtn && window.__TAURI_INTERNALS__) {
    settingsOpenBtn.addEventListener("click", function () {
      setTimeout(function () {
        window.__TAURI_INTERNALS__.invoke("load_settings").then(function (s) {
          var cb = document.getElementById("set-beacon-enabled");
          if (cb && s) cb.checked = !!s.beaconEnabled;
        }).catch(function () {});
      }, 0);
    });
  }
})();