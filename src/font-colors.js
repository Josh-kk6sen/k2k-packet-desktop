// K2K Packet Desktop — Unproto Font Colors (per-theme customization).
// Migrated verbatim from index.html inline <script> (Phase A). The fc*
// functions are referenced from inline onclick/oninput/onblur attributes AND
// from generated HTML strings inside this module, so they must all stay on
// window (the original block assigned them explicitly — preserved here).

(function () {
  if (!window.__TAURI_INTERNALS__) return;
  var FC_ROLES = ["time", "call", "via", "meta", "payload", "connrx"];
  var FC_LABELS = { time: "Time", call: "Source &amp; Destination", via: "Via", meta: "Packet Info", payload: "Payload", connrx: "C1 - C4 Payload" };
  var FC_VARS = { time: "--u-time", call: "--u-call", via: "--u-via", meta: "--u-meta", payload: "--u-payload", connrx: "--msg-rx" };
  var FC_THEMES = { dark: "Dark", light: "Light", blue: "Blue", sepia: "Sepia", synthwave: "Synthwave", vintage: "Vintage" };
  var fcCached = null;
  var fcState = null;
  function fcInvoke(cmd, args) { return window.__TAURI_INTERNALS__.invoke(cmd, args || {}); }
  function fc$(id) { return document.getElementById(id); }
  function fcHexOk(v) { return typeof v === "string" && /^#[0-9a-f]{6}$/i.test(v); }

  function fcActiveTheme() {
    var sel = fc$("set-theme");
    return sel && FC_THEMES[sel.value] ? sel.value : "dark";
  }
  // The theme actually applied to the UI (data-theme; "" = dark). Overrides
  // must key off this, NOT the settings dropdown — the dropdown can be
  // changed-but-unsaved, and colors would otherwise tint the wrong theme.
  function fcAppliedTheme() {
    var t = document.documentElement.getAttribute("data-theme") || "";
    return FC_THEMES[t] ? t : "dark";
  }
  function fcThemeName(t) { return FC_THEMES[t] || t; }

  // Read a theme's built-in default colors straight from the stylesheet.
  function fcThemeDefaults(theme) {
    var out = {}, bg = null, target = theme === "dark" ? "root" : "[data-theme=\"" + theme + "\"]";
    try {
      for (var i = 0; i < document.styleSheets.length; i++) {
        var rules;
        try { rules = document.styleSheets[i].cssRules; } catch (e) { continue; }
        if (!rules) continue;
        for (var j = 0; j < rules.length; j++) {
          var r = rules[j], sel = r.selectorText || "";
          if (sel.indexOf(target) < 0) continue;
          var st = r.style;
          for (var k = 0; k < FC_ROLES.length; k++) {
            var role = FC_ROLES[k];
            if (!out[role]) {
              var v = st.getPropertyValue(FC_VARS[role]);
              if (v) out[role] = v.trim().toLowerCase();
            }
          }
          if (!bg) {
            var bv = st.getPropertyValue("--monitor-bg");
            if (bv) bg = bv.trim().toLowerCase();
          }
        }
      }
    } catch (e) {}
    if (!out.time) out.time = "#e6d080";
    if (!out.call) out.call = "#33ff33";
    if (!out.via) out.via = "#008888";
    if (!out.meta) out.meta = "#008888";
    if (!out.payload) out.payload = "#00cccc";
    if (!out.connrx) out.connrx = "#00cccc";
    if (!bg) {
      // Last resort: the applied theme's computed monitor background
      try { bg = getComputedStyle(document.documentElement).getPropertyValue("--monitor-bg").trim().toLowerCase(); } catch (e) {}
    }
    if (!bg) bg = "#000000";
    return { colors: out, bg: bg };
  }

  // Apply saved overrides for the ACTIVE theme as inline CSS vars.
  function fcApplyOverrides(settings) {
    var root = document.documentElement, theme = fcAppliedTheme();
    var map = (settings && settings.unprotoColors) || {};
    var over = map[theme] || {};
    for (var i = 0; i < FC_ROLES.length; i++) {
      var role = FC_ROLES[i], v = over[role];
      if (fcHexOk(v)) root.style.setProperty(FC_VARS[role], v);
      else root.style.removeProperty(FC_VARS[role]);
    }
  }
  new MutationObserver(function (muts) {
    for (var i = 0; i < muts.length; i++) {
      if (muts[i].attributeName === "data-theme") { fcApplyOverrides(fcCached); break; }
    }
  }).observe(document.documentElement, { attributes: true });
  fcInvoke("load_settings").then(function (s) { fcCached = s; fcApplyOverrides(s); }).catch(function () {});

  // ── Dialog rendering ──
  function fcRowHtml(role, val) {
    var label = FC_LABELS[role] || role;
    return "<div class=\"fc-row\" data-role=\"" + role + "\">" +
      "<label>" + label + "</label>" +
      "<input class=\"fc-hex\" type=\"text\" spellcheck=\"false\" maxlength=\"7\" value=\"" + val.toUpperCase() + "\" oninput=\"fcHexText('" + role + "',this.value)\" onblur=\"fcHexBlur('" + role + "',this)\">" +
      "<span class=\"fc-swatch\" style=\"background:" + val + "\"></span>" +
      "<input type=\"color\" value=\"" + val + "\" oninput=\"fcPick('" + role + "',this.value)\">" +
      "<button class=\"fc-reset\" title=\"Reset to theme default\" onclick=\"fcResetRow('" + role + "')\">&#8634;</button>" +
      "</div>";
  }
  function fcSpan(id, color, text) {
    var el = fc$(id); if (!el) return;
    el.style.color = color; el.textContent = text;
  }
  function fcUpdateSample() {
    var s = fcState, c = s.colors;
    fcSpan("fc-s-time", c.time, "12:00:00");
    fcSpan("fc-s-src", c.call, "KK6SEN");
    fcSpan("fc-s-to", c.call, "to");
    fcSpan("fc-s-dst", c.call, "BEACON");
    fcSpan("fc-s-via", c.via, "via DIGI1 DIGI2");
    fcSpan("fc-s-meta", c.meta, "ctl UI pid=F0(Text) len 21");
    fcSpan("fc-s-payload", c.payload, "This is only a test. I repeat, this is only a test.");
    fcSpan("fc-c-msg", c.connrx, "Hello from the connected session - payload text color.");
  }
  function fcRender() {
    var s = fcState, html = "";
    for (var i = 0; i < FC_ROLES.length; i++) html += fcRowHtml(FC_ROLES[i], s.colors[FC_ROLES[i]]);
    fc$("fc-rows").innerHTML = html;
    fc$("fc-theme-name2").textContent = fcThemeName(s.theme);
    var sample = document.querySelector("#fontcolors-overlay .fc-sample");
    if (sample) sample.style.background = s.bg;
    fcUpdateSample();
  }
  function fcPick(role, val) {
    if (!fcState) return;
    fcState.colors[role] = val.toLowerCase();
    var row = document.querySelector("#fc-rows .fc-row[data-role=\"" + role + "\"]");
    if (row) {
      row.querySelector(".fc-hex").value = val.toUpperCase();
      row.querySelector(".fc-swatch").style.background = val;
    }
    fcUpdateSample();
  }
  // Editable hex text: live-apply when valid, flag invalid input, revert on blur.
  function fcHexText(role, val) {
    if (!fcState) return;
    var v = val.trim();
    var row = document.querySelector("#fc-rows .fc-row[data-role=\"" + role + "\"]");
    if (fcHexOk(v)) {
      fcState.colors[role] = v.toLowerCase();
      if (row) {
        row.querySelector(".fc-swatch").style.background = v.toLowerCase();
        row.querySelector("input[type=color]").value = v.toLowerCase();
        row.querySelector(".fc-hex").classList.remove("bad");
      }
      fcUpdateSample();
    } else if (row) {
      row.querySelector(".fc-hex").classList.add("bad");
    }
  }
  function fcHexBlur(role, el) {
    if (!fcState) return;
    el.value = fcState.colors[role].toUpperCase();
    el.classList.remove("bad");
  }
  function fcResetRow(role) {
    if (!fcState) return;
    var def = fcState.defaults[role];
    fcState.colors[role] = def;
    var row = document.querySelector("#fc-rows .fc-row[data-role=\"" + role + "\"]");
    if (row) {
      row.querySelector("input[type=color]").value = def;
      row.querySelector(".fc-hex").value = def.toUpperCase();
      row.querySelector(".fc-swatch").style.background = def;
    }
    fcUpdateSample();
  }
  function fcDefaults() {
    if (!fcState) return;
    fcState.colors = {};
    for (var i = 0; i < FC_ROLES.length; i++) fcState.colors[FC_ROLES[i]] = fcState.defaults[FC_ROLES[i]];
    fcRender();
  }
  function openFontColors() {
    var overlay = fc$("fontcolors-overlay");
    if (!overlay) return;
    fcInvoke("load_settings").then(function (cur) {
      fcCached = cur;
      var theme = fcActiveTheme();
      var def = fcThemeDefaults(theme);
      var map = (cur && cur.unprotoColors) || {};
      var saved = map[theme] || {};
      var colors = {};
      for (var i = 0; i < FC_ROLES.length; i++) {
        var role = FC_ROLES[i], v = saved[role];
        colors[role] = fcHexOk(v) ? v.toLowerCase() : def.colors[role];
      }
      fcState = { theme: theme, colors: colors, defaults: def.colors, bg: def.bg };
      fcRender();
      overlay.classList.add("open");
    }).catch(function () {});
  }
  function fcCancel() {
    fcState = null;
    var overlay = fc$("fontcolors-overlay");
    if (overlay) overlay.classList.remove("open");
  }
  function fcSave() {
    if (!fcState) return;
    var s = fcState;
    fcInvoke("load_settings").then(function (cur) {
      var map = (cur && cur.unprotoColors) ? JSON.parse(JSON.stringify(cur.unprotoColors)) : {};
      var entry = {};
      for (var i = 0; i < FC_ROLES.length; i++) {
        var role = FC_ROLES[i];
        if (s.colors[role].toLowerCase() !== s.defaults[role]) entry[role] = s.colors[role];
      }
      if (Object.keys(entry).length > 0) map[s.theme] = entry; else delete map[s.theme];
      cur.unprotoColors = map;
      fcCached = cur;
      return fcInvoke("save_settings", { settings: cur }).then(function () {
        fcApplyOverrides(cur);
        fcCancel();
      });
    }).catch(function () {});
  }
  // Backdrop click closes the dialog
  var fcOverlay = fc$("fontcolors-overlay");
  if (fcOverlay) fcOverlay.onclick = function (e) { if (e.target === fcOverlay) fcCancel(); };

  window.openFontColors = openFontColors;
  window.fcCancel = fcCancel;
  window.fcSave = fcSave;
  window.fcDefaults = fcDefaults;
  window.fcResetRow = fcResetRow;
  window.fcPick = fcPick;
  window.fcHexText = fcHexText;
  window.fcHexBlur = fcHexBlur;
})();