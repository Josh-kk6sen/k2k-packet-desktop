// K2K Packet Desktop — Protocol dropdown + port/KISS-settings defaults.
// Migrated verbatim from index.html inline <script> blocks (Phase A).
// `applyProtocolDefaults` stays on window (called from inline onchange /
// init in index.html).

// Custom protocol dropdown (the styled popup, not a native <select>).
(function (d) {
  var sel = d.getElementById("cs-protocol-selected");
  var opts = d.getElementById("cs-protocol-options");
  var hidden = d.getElementById("set-protocol");
  if (!sel || !opts || !hidden) return;
  sel.addEventListener("click", function (e) {
    opts.classList.toggle("open");
    sel.classList.toggle("open");
    e.stopPropagation();
  });
  opts.querySelectorAll(".cs-opt").forEach(function (opt) {
    opt.addEventListener("click", function () {
      hidden.value = this.getAttribute("data-value");
      sel.querySelector(".cs-text").innerHTML = this.innerHTML;
      opts.classList.remove("open");
      sel.classList.remove("open");
      hidden.dispatchEvent(new Event("change", { bubbles: true }));
    });
  });
  d.addEventListener("click", function () {
    opts.classList.remove("open");
    sel.classList.remove("open");
  });
})(document);

// Apply the port + KISS-settings visibility defaults for the chosen protocol
// (AGWPE → 8000/hidden, KISS → 8001/visible).
window.applyProtocolDefaults = function () {
  var sel = document.getElementById("set-protocol");
  var port = document.getElementById("set-port");
  var kg = document.getElementById("kiss-settings-group");
  if (!sel || !port) return;
  if (sel.value === "AGWPE") { port.value = "8000"; port.placeholder = "8000"; if (kg) kg.style.display = "none"; }
  else { port.value = "8001"; port.placeholder = "8001"; if (kg) kg.style.display = ""; }
};