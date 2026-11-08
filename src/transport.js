// K2K Packet Desktop — front-end transport.
//
// The UI is identical in both front-ends; only the pipe changes.
//
//   Desktop (Tauri):  invoke()/listen() are the real Tauri IPC calls.
//   Browser (--web):   invoke() is POST /api/invoke, listen() is the event
//                      stream from /ws (with a replay snapshot on connect so
//                      a tab opened mid-session is not blank).
//
// Legacy modules in this app call `window.__TAURI_INTERNALS__.invoke(...)`
// and the `plugin:event|listen` path directly (that is how the pre-Framework-3
// inline scripts did it). In browser mode this module installs a small
// __TAURI_INTERNALS__ shim with the same shape, so those modules work
// unchanged instead of being rewritten twice.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

/** True when this bundle is served to a real browser rather than a WebView. */
export const isWeb = !(typeof window !== "undefined" && !!window.__TAURI_INTERNALS__);

// ── Browser event bus ────────────────────────────────────
const listeners = new Map(); // event name -> Set<fn({event, payload})>
let ws = null;
let wsOpen = false;
let firstSnapshot = true;
let reconnectDelay = 500;
let snapshotMeta = null;

function addListener(event, fn) {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event).add(fn);
  return () => {
    const set = listeners.get(event);
    if (set) set.delete(fn);
  };
}

function dispatch(event, payload) {
  const set = listeners.get(event);
  if (!set) return;
  for (const fn of [...set]) {
    try {
      fn({ event, payload });
    } catch (e) {
      console.error("listener failed for " + event, e);
    }
  }
}

function connect() {
  if (!isWeb) return;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const url = proto + "//" + location.host + "/ws";
  try {
    ws = new WebSocket(url);
  } catch (e) {
    scheduleReconnect();
    return;
  }
  ws.onopen = () => {
    wsOpen = true;
    reconnectDelay = 500;
  };
  ws.onmessage = (msg) => {
    let data;
    try {
      data = JSON.parse(msg.data);
    } catch (e) {
      return;
    }
    if (data && data.type === "snapshot") {
      snapshotMeta = data;
      if (firstSnapshot) {
        // Replay in order: this reconstructs connection + session state and
        // the monitor tail for a tab that was opened after the action.
        firstSnapshot = false;
        for (const ev of data.events || []) dispatch(ev.event, ev.payload);
      }
      dispatch("web-snapshot", data);
      return;
    }
    if (data && data.event) dispatch(data.event, data.payload);
  };
  ws.onclose = () => {
    wsOpen = false;
    scheduleReconnect();
  };
  ws.onerror = () => {
    try {
      ws.close();
    } catch (e) {}
  };
}

function scheduleReconnect() {
  setTimeout(connect, reconnectDelay);
  reconnectDelay = Math.min(reconnectDelay * 2, 5000);
}

if (isWeb) connect();

/** Last snapshot's metadata (version, data dir, connections). */
export function webSnapshot() {
  return snapshotMeta;
}

// ── invoke ───────────────────────────────────────────────
async function httpInvoke(cmd, args) {
  // A browser cannot open a folder on the server; show the log files instead.
  if (cmd === "open_log_dir") {
    window.open("/api/logs", "_blank");
    return null;
  }
  const res = await fetch("/api/invoke", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cmd: cmd, args: args || {} }),
  });
  if (!res.ok) throw "Server error " + res.status;
  const data = await res.json();
  if (!data.ok) throw data.error; // Tauri rejects with a plain string; match it
  return data.value === undefined ? null : data.value;
}

export function invoke(cmd, args) {
  return isWeb ? httpInvoke(cmd, args) : tauriInvoke(cmd, args);
}

export function listen(event, handler) {
  if (!isWeb) return tauriListen(event, handler);
  const off = addListener(event, (ev) => handler(ev));
  return Promise.resolve(off);
}

// ── __TAURI_INTERNALS__ shim (browser mode only) ─────────
// Same shape the legacy modules expect: invoke(), transformCallback(), and
// the plugin:event|listen / plugin:event|unlisten commands.
if (isWeb) {
  let cbId = 0;
  const callbacks = new Map();
  const handlerEvents = new Map();

  window.__TAURI_INTERNALS__ = {
    invoke(cmd, args) {
      if (cmd === "plugin:event|listen") {
        const event = args && args.event;
        const handler = args && args.handler;
        const off = addListener(event, (ev) => {
          const cb = callbacks.get(handler);
          if (cb) cb(ev);
        });
        handlerEvents.set(handler, off);
        return Promise.resolve(handler);
      }
      if (cmd === "plugin:event|unlisten") {
        const handler = args && args.handler;
        const off = handlerEvents.get(handler);
        if (off) off();
        handlerEvents.delete(handler);
        return Promise.resolve(null);
      }
      return httpInvoke(cmd, args);
    },
    transformCallback(fn) {
      const id = ++cbId;
      callbacks.set(id, fn);
      return id;
    },
    // Not used by this app, present so version checks can't throw.
    metadata: { currentWindow: { label: "main" }, currentWebview: { windowLabel: "main", label: "main" } },
  };
}
