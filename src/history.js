// K2K Packet Desktop — History & Suggestion System.
// Migrated verbatim from index.html inline <script> (Phase A). Original code
// ran in classic-script global scope, so every identifier it intentionally
// exposed on window must stay there (showSuggestions/pickSuggestion/
// deleteSuggestion etc. are referenced by HTML attributes and generated
// suggestion-item HTML strings). Each is declared as a module-local const for
// robust internal bare-call resolution under ES modules, then attached to
// window.

(function () {
  // ── Tauri bridge ──
  var _invoke = window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke;
  var usingTauri = !!_invoke;
  var historyCache = {};

  // Single entry point for backend calls — swallows errors so UI code
  // doesn't need a .catch() on every call.
  function invoke(cmd, args) {
    return _invoke(cmd, args).catch(function () {});
  }

  // Read which C tab is active from the DOM (finds .mode-tab.active)
  function activeTabIndex() {
    var tabs = document.querySelectorAll(".mode-tab");
    for (var i = 0; i < tabs.length; i++) if (tabs[i].classList.contains("active")) return i;
    return 0;
  }
  // ── History store (in-memory cache + disk sync) ──

  // Per-tab history keys — evaluated fresh each time so tab switches are respected
  function composeHistoryKey() { return "c" + (activeTabIndex() + 1) + "_msg"; }
  function composeViaHistoryKey() { return "c" + (activeTabIndex() + 1) + "_via"; }

  // Preload all history keys on startup
  if (usingTauri) {
    var KEYS = ["unproto_to", "unproto_via", "unproto_msg",
      "c1_via", "c1_msg", "c2_via", "c2_msg", "c3_via", "c3_msg", "c4_via", "c4_msg"];
    KEYS.forEach(function (k) {
      invoke("load_history", { key: k }).then(function (r) {
        historyCache[k] = r || [];
      }).catch(function () {});
    });
  }

  function getHistoryEntries(key, reversed) {
    var arr = historyCache[key];
    if (!arr) return [];
    var result = reversed ? arr.slice().reverse() : arr;
    return result;
  }

  // Save to cache (sync) + fire-and-forget to disk
  function saveHistoryEntry(key, val, max) {
    if (!usingTauri || !val) return;
    var arr = historyCache[key] = (historyCache[key] || []).slice();
    var pos = arr.indexOf(val);
    if (pos >= 0) arr.splice(pos, 1);
    arr.push(val);                    // newest at the END
    if (arr.length > max) arr.splice(0, arr.length - max);  // drop oldest from front
    historyCache[key] = arr;
    invoke("save_history", { key: key, value: val, max: max });
  }

  // Map input element id → history cache key
  function historyKeyForInput(id) {
    if (id === "via-input") return composeViaHistoryKey();
    if (id === "unproto-to") return "unproto_to";
    if (id === "unproto-via") return "unproto_via";
    return id.replace(/-/g, "_");
  }

  // ── Suggestion Panels ──
  const showSuggestions = function (id) {
    var inp = document.getElementById(id);
    if (!inp) return;
    var panel = document.getElementById("sug-" + id);
    if (!panel) return;
    var vals = getHistoryEntries(historyKeyForInput(id), false);
    // Always show the last 10 history entries — no prefix filtering on the
    // current field content (user request: last 10 regardless of typed text).
    var filtered = vals.slice(-10);
    if (!filtered.length) { panel.classList.remove("open"); panel.innerHTML = ""; return; }
    // Array is oldest-first, newest-last. Display as-is → oldest at top, newest at bottom.
    panel.innerHTML = filtered.map(function (v) {
      var escAtt = String(v).replace(/[&<>"]/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]; });
      var escTxt = String(v).replace(/[&<>]/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]; });
      return "<div class=\"sug-item\" data-val=\"" + escAtt + "\" onmousedown=\"event.preventDefault();pickSuggestion('" + id + "',this)\">" +
        "<span class=\"sug-val\">" + escTxt + "</span>" +
        "<span class=\"sug-del\" title=\"Remove from history\" onmousedown=\"event.preventDefault();event.stopPropagation();deleteSuggestion('" + id + "',this)\"><img class=\"sug-x\" src=\"/redx.png\" width=\"20\" height=\"20\" alt=\"remove\"/></span></div>";
    }).join("");
    panel.classList.add("open");
  };
  // Delete a single entry from history (called from the ✕ in the suggestion panel)
  const deleteSuggestion = function (id, el) {
    var item = el.closest(".sug-item");
    var val = item ? item.getAttribute("data-val") : "";
    if (!val) return;
    var key = historyKeyForInput(id);
    var arr = historyCache[key] || [];
    var pos = arr.indexOf(val);
    if (pos >= 0) arr.splice(pos, 1);
    historyCache[key] = arr;
    invoke("remove_history", { key: key, value: val });
    showSuggestions(id);  // re-render panel without the removed entry
  };
  const hideSuggestions = function (id) {
    var panel = document.getElementById("sug-" + id);
    if (!panel) return;
    setTimeout(function () { panel.classList.remove("open"); }, 300);
  };
  const pickSuggestion = function (id, el) {
    var inp = document.getElementById(id);
    if (inp) { inp.value = el.getAttribute("data-val"); }
    var panel = document.getElementById("sug-" + id);
    if (panel) panel.classList.remove("open");
  };
  document.addEventListener("mousedown", function (e) {
    var wrap = e.target.closest(".sug-wrap");
    if (!wrap) {
      document.querySelectorAll(".sug-panel.open").forEach(function (p) { p.classList.remove("open"); });
    }
  });

  // ── Up-Arrow Command History ──
  window.historyNavState = {};
  const setupCmdHistory = function (textareaId, getKeyFn, max) {
    var ta = document.getElementById(textareaId);
    if (!ta) return;
    var nav = window.historyNavState[textareaId] = { idx: -1, curVal: "" };
    ta.addEventListener("keydown", function (e) {
      if (e.key === "ArrowUp" && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
        var histKey = (typeof getKeyFn === "function") ? getKeyFn() : getKeyFn;
        var arr = getHistoryEntries(histKey, true);  // reversed → newest first
        if (!arr.length) return;
        if (nav.idx === -1) nav.curVal = ta.value;
        if (nav.idx === -1) nav.idx = 0;
        ta.value = arr[nav.idx] || nav.curVal;
        if (nav.idx < arr.length - 1) nav.idx++; else nav.idx = arr.length;
        ta.setSelectionRange(ta.value.length, ta.value.length);
      } else if (e.key === "ArrowDown" && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
        e.preventDefault();
        if (nav.idx < 0) return;
        var histKey = (typeof getKeyFn === "function") ? getKeyFn() : getKeyFn;
        var arr = getHistoryEntries(histKey, true);  // reversed → newest first
        if (nav.idx > 0) { nav.idx--; ta.value = arr[nav.idx]; }
        else { nav.idx = -1; ta.value = nav.curVal; }
        ta.setSelectionRange(ta.value.length, ta.value.length);
      }
    });
    ta.addEventListener("input", function () { nav.idx = -1; nav.curVal = ta.value; });
    window.resetHistoryNav = function (id) {
      if (window.historyNavState[id]) { window.historyNavState[id].idx = -1; window.historyNavState[id].curVal = ""; }
    };
  };
  window.showSuggestions = showSuggestions;
  window.deleteSuggestion = deleteSuggestion;
  window.hideSuggestions = hideSuggestions;
  window.pickSuggestion = pickSuggestion;
  window.setupCmdHistory = setupCmdHistory;

  setupCmdHistory("compose-in", composeHistoryKey, 20);
  setupCmdHistory("unproto-msg", "unproto_msg", 20);

  // ── C Tab Click Handler ──
  var modeTabs = document.getElementById("mode-tabs");
  if (modeTabs) modeTabs.addEventListener("click", function (e) {
    var tab = e.target.closest(".mode-tab");
    if (tab && !tab.classList.contains("active")) {
      modeTabs.querySelectorAll(".mode-tab").forEach(function (t) { t.classList.remove("active"); });
      tab.classList.add("active");
    }
  });

  // ── Save on send ──
  // One shared save path per input set — both the send-button clicks (owned by
  // the JS bundle) and the global Enter handler delegate here, so each send
  // writes history exactly once no matter how it was triggered. The buttons
  // are static HTML, so direct bindings replace the old MutationObservers
  // that waited for them to appear.
  function saveComposeHist() {
    var ta = document.getElementById("compose-in");
    if (ta && ta.value.trim()) { saveHistoryEntry(composeHistoryKey(), ta.value.trim(), 20); window.resetHistoryNav("compose-in"); }
    var vi = document.getElementById("via-input");
    if (vi && vi.value.trim()) saveHistoryEntry(composeViaHistoryKey(), vi.value.trim().toUpperCase(), 10);
  }
  function saveUnprotoHist() {
    var ta = document.getElementById("unproto-msg");
    if (ta && ta.value.trim()) { saveHistoryEntry("unproto_msg", ta.value.trim(), 20); window.resetHistoryNav("unproto-msg"); }
    var to = document.getElementById("unproto-to");
    var vi = document.getElementById("unproto-via");
    if (to && to.value.trim()) saveHistoryEntry("unproto_to", to.value.trim().toUpperCase(), 10);
    if (vi && vi.value.trim()) saveHistoryEntry("unproto_via", vi.value.trim().toUpperCase(), 10);
  }
  var sBtn = document.getElementById("send-btn");
  if (sBtn) sBtn.addEventListener("click", saveComposeHist);
  var uBtn = document.getElementById("unproto-send");
  if (uBtn) uBtn.addEventListener("click", saveUnprotoHist);
  document.addEventListener("keydown", function (e) {
    if (e.key === "Enter" && !e.shiftKey) {
      if (e.target && e.target.id === "compose-in") saveComposeHist();
      else if (e.target && e.target.id === "unproto-msg") saveUnprotoHist();
    }
  });
})();