// K2K Packet Desktop — Framework 3 frontend entry point.
//
// Move-in progress, surface by surface, behavior-preserving. The six inline
// <script> blocks that used to live in index.html are now ES modules under
// src/, imported here in the same relative order. Each module was migrated
// verbatim and keeps any identifier the HTML or generated strings reference
// on window (classic-script globals would otherwise vanish under modules).
//
// This module tree is the single frontend entry point: index.html loads only
// this file (see the <script type="module" src="/src/main.js"> tag). The old
// opaque shipped bundle (public/assets/index-DkQAlvay.js) is no longer
// loaded — it exists only in git history from before the Framework 3 port.
//
// CSS NOTE: the LIVE stylesheet is the inline <style> block inside index.html
// (~35KB). src/styles/main.css is a synced reference copy of the same CSS —
// it is NOT imported here, so keep the two in sync when editing styles.

import "./logging";
import "./pbbs";
import "./protocol";
import "./history";
import "./beacon";
import "./font-colors";
import "./app";

console.debug("[k2k] Framework 3 entry loaded");