// K2K Packet Desktop — PBBS admin console (local, no RF).
// Migrated verbatim from index.html inline <script> (Phase A). The green "PBBS
// enabled" badge opens a local console talking straight to the board engine
// (bbs::BbsSession) — nothing transmits. `openPbbsAdmin` stays on window (an
// inline onclick in index.html calls it). Uses window.logTo/logTs (defined by
// src/logging.js, which must import first).
const _pbbsOverlay = document.getElementById("pbbs-overlay");
const _pbbsConsole = document.getElementById("pbbs-console");
const _pbbsInput = document.getElementById("pbbs-input");
function _pbbsAppend(text) {
  if (!_pbbsConsole) return;
  _pbbsConsole.textContent += text + "\n";
  _pbbsConsole.scrollTop = _pbbsConsole.scrollHeight;
}
function _pbbsRender(view) {
  if (view && view.lines) view.lines.forEach(function (l) { _pbbsAppend(l); window.logTo("admin", "[" + window.logTs() + "] " + l); });
  if (view && view.hangup && _pbbsOverlay) _pbbsOverlay.classList.remove("open");
}
function _pbbsError(msg) { _pbbsAppend("*** " + String(msg) + " ***"); }
function openPbbsAdmin() {
  if (!_pbbsOverlay) return;
  _pbbsOverlay.classList.add("open");
  // Always start fresh — clear the screen, the open command returns a
  // brand-new session (banner + message list + prompt), no saved state.
  if (_pbbsConsole) _pbbsConsole.textContent = "";
  window.__TAURI_INTERNALS__.invoke("pbbs_admin_open").then(_pbbsRender).catch(function (e) { _pbbsError(e); });
  if (_pbbsInput) _pbbsInput.focus();
}
function _pbbsSend() {
  if (!_pbbsInput) return;
  var line = _pbbsInput.value.trim();
  // Blank lines are allowed — the BBS stores them as blank body lines while
  // composing (JBOX/KPC3P parity); at the command prompt they're a silent no-op.
  _pbbsAppend("> " + line);
  window.logTo("admin", "[" + window.logTs() + "] > " + line);
  _pbbsInput.value = "";
  window.__TAURI_INTERNALS__.invoke("pbbs_admin_send", { line: line }).then(_pbbsRender).catch(function (e) { _pbbsError(e); });
}
function _pbbsClear() {
  if (_pbbsConsole) _pbbsConsole.textContent = "";
  window.__TAURI_INTERNALS__.invoke("pbbs_admin_reset").then(_pbbsRender).catch(function (e) { _pbbsError(e); });
}
window.openPbbsAdmin = openPbbsAdmin;
(function () {
  var closeBtn = document.getElementById("close-pbbs");
  var sendBtn = document.getElementById("pbbs-send");
  var clearBtn = document.getElementById("pbbs-clear");
  if (closeBtn) closeBtn.onclick = function () { _pbbsOverlay.classList.remove("open"); };
  if (_pbbsOverlay) _pbbsOverlay.onclick = function (e) { if (e.target === _pbbsOverlay) _pbbsOverlay.classList.remove("open"); };
  if (_pbbsInput) _pbbsInput.addEventListener("keydown", function (e) { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); _pbbsSend(); } });
  if (sendBtn) sendBtn.onclick = _pbbsSend;
  if (clearBtn) clearBtn.onclick = _pbbsClear;
  // Badge visibility — mirrors the beacon indicator. The settings-modal
  // checkbox only populates once the modal has been opened — ask Rust for the
  // live settings when the checkbox says off.
  var pbbsIndicator = document.getElementById("pbbs-indicator");
  async function updatePbbsIndicator() {
    var mbCheckbox = document.getElementById("set-mb-enabled");
    var on = !!(mbCheckbox && mbCheckbox.checked);
    if (!on) {
      try {
        var s = await window.__TAURI_INTERNALS__.invoke("load_settings");
        on = !!(s && s.messageBoardEnabled);
      } catch (e) {}
    }
    if (pbbsIndicator) {
      pbbsIndicator.className = on ? "" : "hidden";
      if (on) {
        // Show a live connected-user count: "PBBS 0/4", "1/4", … "4/4".
        try {
          var u = await window.__TAURI_INTERNALS__.invoke("pbbs_users");
          var n = (u && typeof u.users === "number") ? u.users : 0;
          var m = (u && typeof u.max === "number") ? u.max : 4;
          pbbsIndicator.textContent = "PBBS " + n + "/" + m;
        } catch (e) {
          pbbsIndicator.textContent = "PBBS";
        }
      }
    }
  }
  setInterval(updatePbbsIndicator, 2000);
  updatePbbsIndicator();
})();