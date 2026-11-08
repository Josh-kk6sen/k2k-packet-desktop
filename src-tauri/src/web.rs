// ─ K2K Packet Desktop — headless browser front-end (`--web`) ─
//
// `k2k-packet-desktop --web --port 8080` starts the SAME backend as the
// desktop app but builds no window at all: no GTK/WebKit init, no display
// needed. The compiled front-end is embedded in the binary (`dist/`), served
// over HTTP, and talks to the identical command bodies and kernel:
//
//   POST /api/invoke   { "cmd": "...", "args": {...} }  -> { ok, value } | { ok:false, error }
//   GET  /api/state    connection/tab snapshot + version + data dir
//   GET  /api/logs     today's log files (list); /api/logs/<name> downloads one
//   GET  /ws           live events: one {type:"snapshot"} frame of recent
//                      events, then every event emitted from that moment on.
//
// Events are the real ones the kernel emits (23 sites, unchanged); the sink
// fans them out to every connected browser and keeps a ring buffer so a tab
// opened (or reloaded) mid-session immediately gets the monitor tail and the
// current session state instead of an empty window.
//
// This is a LAN tool: no TLS, no passwords. Bind defaults to 0.0.0.0 so the
// machine's IP works from another machine on the network; `--bind 127.0.0.1`
// restricts it to the box itself.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State as AxState};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::app::Out;
use crate::session::AppState;

/// The built front-end, embedded at compile time — the same `dist/` Tauri
/// bundles, so the installer ships everything and there is no asset path to
/// get wrong on a headless box.
static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../dist");

/// How many recent events a newly-connected browser is given. Enough for a
/// useful monitor tail (and to reconstruct connection/session state by
/// replaying them in order) without a multi-megabyte first frame.
const REPLAY_EVENTS: usize = 400;

/// Event fan-out for browser clients.
pub struct Sink {
    tx: broadcast::Sender<String>,
    history: Mutex<VecDeque<Value>>,
}

impl Sink {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(512);
        Arc::new(Self {
            tx,
            history: Mutex::new(VecDeque::with_capacity(REPLAY_EVENTS)),
        })
    }

    /// Record an event and push it to every connected browser.
    pub fn emit(&self, event: &str, payload: Value) {
        let frame = json!({ "event": event, "payload": payload });
        {
            let mut h = lock(&self.history);
            if h.len() == REPLAY_EVENTS {
                h.pop_front();
            }
            h.push_back(frame.clone());
        }
        // No receivers yet is normal (nobody has opened a browser).
        let _ = self.tx.send(frame.to_string());
    }

    fn snapshot(&self) -> Vec<Value> {
        lock(&self.history).iter().cloned().collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

/// A poisoned ring buffer is not worth killing a server over.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
struct Ctx {
    state: Arc<AppState>,
    sink: Arc<Sink>,
    out: Out,
}

// ── Entry point ───────────────────────────────────────────

/// Serve the browser front-end until the process is killed.
pub async fn serve(state: Arc<AppState>, sink: Arc<Sink>, addr: String) -> Result<(), String> {
    let ctx = Ctx {
        state,
        sink: sink.clone(),
        out: Out::web(sink),
    };
    let app = Router::new()
        .route("/api/invoke", post(invoke))
        .route("/api/state", get(state_info))
        .route("/api/logs", get(log_list))
        .route("/api/logs/{name}", get(log_file))
        .route("/ws", get(ws_handler))
        .fallback(get(static_file))
        .with_state(ctx);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("Cannot listen on {addr}: {e}"))?;
    println!("K2K Packet Desktop web client: http://{addr}/");
    println!("  (Ctrl-C stops the server; the desktop app is unaffected.)");
    crate::append_log("raw", &format!("=== web client serving on {addr} ==="));
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Server stopped: {e}"))
}

// ── Static front-end ──────────────────────────────────────

async fn static_file(uri: axum::http::Uri) -> Response {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".to_string();
    }
    let Some(file) = DIST.get_file(&path) else {
        // SPA-ish fallback: unknown paths get the app shell, so a stale
        // bookmark or a relative URL still lands on the client.
        return match DIST.get_file("index.html") {
            Some(f) => serve_bytes("index.html", f.contents()),
            None => (StatusCode::NOT_FOUND, "not found").into_response(),
        };
    };
    serve_bytes(&path, file.contents())
}

fn serve_bytes(path: &str, bytes: &'static [u8]) -> Response {
    let mime = match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            // The client and server are built together; a cached old bundle
            // mixed with a new backend is a confusing bug class.
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response()
}

// ── Events over WebSocket ─────────────────────────────────

async fn ws_handler(ws: WebSocketUpgrade, AxState(ctx): AxState<Ctx>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, ctx))
}

async fn ws_loop(mut socket: WebSocket, ctx: Ctx) {
    // Subscribe BEFORE reading the snapshot so nothing emitted between the
    // two is lost; the snapshot is ordered ahead of the live stream, and a
    // duplicate of an event that lands in both is harmless (the client
    // renders frames, it does not count them).
    let mut rx = ctx.sink.subscribe();
    let snapshot = json!({
        "type": "snapshot",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "web",
        "dataDir": crate::storage::data_base_static().display().to_string(),
        "events": ctx.sink.snapshot(),
        "connections": connections_json(&ctx.state).await,
    });
    if socket.send(Message::Text(snapshot.to_string().into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // A slow tab lost frames; tell it so the operator knows
                    // the monitor has a gap instead of silently lying.
                    let note = json!({
                        "event": "system-message",
                        "payload": { "text": format!("Browser fell behind — {n} monitor frames were skipped.") }
                    });
                    if socket.send(Message::Text(note.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            inbound = socket.recv() => match inbound {
                // Client pings/close.
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ── State / logs endpoints ────────────────────────────────

async fn state_info(AxState(ctx): AxState<Ctx>) -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "web",
        "dataDir": crate::storage::data_base_static().display().to_string(),
        "connections": connections_json(&ctx.state).await,
        "events": ctx.sink.snapshot(),
    }))
}

async fn connections_json(state: &Arc<AppState>) -> Value {
    let core = state.core.lock().await;
    let mut list = Vec::new();
    for arc in &core.connections {
        // Lock (don't try_lock): a connection that happens to be busy is still
        // part of the state, and a snapshot that silently omits it looks like
        // "not connected" in a newly opened tab.
        {
            let c = arc.lock().await;
            let tabs: Vec<Value> = c
                .info
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| s.state != crate::session::Ax25ConnState::Disconnected)
                .map(|(i, s)| {
                    json!({
                        "tab": i,
                        "state": format!("{:?}", s.state),
                        "remote": s.remote,
                        "remoteSsid": s.remote_ssid,
                    })
                })
                .collect();
            list.push(json!({
                "id": c.info.id,
                "label": c.info.label,
                "host": c.info.host,
                "port": c.info.port,
                "connected": c.info.connected,
                "protocol": match c.protocol {
                    crate::session::Protocol::KISS => "KISS",
                    crate::session::Protocol::AGW => "AGW",
                },
                "tabs": tabs,
            }));
        }
    }
    json!(list)
}

async fn log_list() -> Response {
    let Some(dir) = crate::LOG_DIR.get() else {
        return (StatusCode::NOT_FOUND, "Log folder not initialized").into_response();
    };
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .filter(|n| n.ends_with(".log"))
            .collect(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    names.sort();
    names.reverse(); // today's files (YYMMDD_*) first
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>K2K logs</title>\
         <style>body{background:#111;color:#ddd;font:14px ui-monospace,monospace;padding:24px}\
         a{color:#6cf;display:block;padding:3px 0}code{color:#888}</style>\
         <h3>K2K Packet Desktop — log files</h3>",
    );
    html.push_str(&format!("<p><code>{}</code></p>", dir.display()));
    if names.is_empty() {
        html.push_str("<p>No log files yet.</p>");
    }
    for n in names {
        html.push_str(&format!("<a href=\"/api/logs/{n}\">{n}</a>"));
    }
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn log_file(Path(name): Path<String>) -> Response {
    // Only ever read a plain file name out of the log dir.
    if name.contains('/') || name.contains('\\') || name.contains("..") || !name.ends_with(".log") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Some(dir) = crate::LOG_DIR.get() else {
        return (StatusCode::NOT_FOUND, "Log folder not initialized").into_response();
    };
    match std::fs::read_to_string(dir.join(&name)) {
        Ok(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CONTENT_DISPOSITION, "inline"),
            ],
            body,
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

// ── Command dispatch ──────────────────────────────────────

fn snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Tauri converts camelCase JS keys to snake_case Rust args; do the same here
/// so the front-end sends identical payloads in both modes.
fn normalize_args(args: &Value) -> Value {
    let Some(obj) = args.as_object() else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (k, v) in obj {
        out.insert(snake(k), v.clone());
    }
    Value::Object(out)
}

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let v = args
        .get(key)
        .ok_or_else(|| format!("Missing argument: {key}"))?;
    serde_json::from_value(v.clone()).map_err(|e| format!("Bad argument {key}: {e}"))
}

fn val(v: impl serde::Serialize) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn invoke(AxState(ctx): AxState<Ctx>, body: String) -> Response {
    let req: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return Json(json!({ "ok": false, "error": format!("Bad request: {e}") })).into_response(),
    };
    let cmd = req.get("cmd").and_then(|c| c.as_str()).unwrap_or_default().to_string();
    let args = normalize_args(req.get("args").unwrap_or(&Value::Null));

    let res = dispatch(&ctx, &cmd, &args).await;
    match res {
        Ok(v) => Json(json!({ "ok": true, "value": v })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}

async fn dispatch(ctx: &Ctx, cmd: &str, a: &Value) -> Result<Value, String> {
    let st = ctx.state.as_ref();
    let out = &ctx.out;
    let r: Result<Value, String> = match cmd {
        // ── connection control ──
        "kiss_connect" => crate::commands::kiss_connect_impl(
            st, out.clone(), arg(a, "id")?, arg(a, "label")?, arg(a, "host")?, arg(a, "port")?,
            arg(a, "callsign")?, arg(a, "ssid")?,
        ).await.map(|_| Value::Null),
        "kiss_disconnect" => crate::commands::kiss_disconnect_impl(st, arg(a, "id")?).await.map(|_| Value::Null),
        "agw_connect" => crate::commands::agw_connect_impl(
            st, out.clone(), arg(a, "id")?, arg(a, "label")?, arg(a, "host")?, arg(a, "port")?,
            arg(a, "callsign")?, arg(a, "ssid")?,
        ).await.map(|_| Value::Null),
        "agw_disconnect_server" => {
            crate::commands::agw_disconnect_server_impl(st, arg(a, "id")?).await.map(|_| Value::Null)
        }
        "connect_to_server" => crate::commands::connect_to_server_impl(
            st, out.clone(), arg(a, "id")?, arg(a, "label")?, arg(a, "host")?, arg(a, "port")?,
            arg(a, "callsign")?, arg(a, "ssid")?, arg(a, "protocol")?,
        ).await.map(|_| Value::Null),

        // ── connected-mode sessions ──
        "ax25_connect" => crate::commands::ax25_connect_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?, arg(a, "remote")?,
            arg(a, "remote_ssid")?, arg(a, "via")?,
        ).await.map(|_| Value::Null),
        "agw_ax25_connect" => crate::commands::agw_ax25_connect_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?, arg(a, "remote")?,
            arg(a, "remote_ssid")?, arg(a, "via")?,
        ).await.map(|_| Value::Null),
        "connect_session" => crate::commands::connect_session_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?, arg(a, "remote")?,
            arg(a, "remote_ssid")?, arg(a, "via")?,
        ).await.map(|_| Value::Null),
        "ax25_send_iframe" => crate::commands::ax25_send_iframe_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?, arg(a, "text")?,
        ).await.map(|_| Value::Null),
        "agw_send_data" => crate::commands::agw_send_data_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?, arg(a, "text")?,
        ).await.map(|_| Value::Null),
        "send_connected_data" => crate::commands::send_connected_data_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?, arg(a, "text")?,
        ).await.map(|_| Value::Null),
        "ax25_disconnect" => crate::commands::ax25_disconnect_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?,
        ).await.map(|_| Value::Null),
        "agw_disconnect" => crate::commands::agw_disconnect_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "tab")?,
        ).await.map(|_| Value::Null),

        // ── unproto (UI) frames ──
        "ax25_send_ui" => crate::commands::ax25_send_ui_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "dest")?, arg(a, "dest_ssid")?,
            arg(a, "text")?, arg(a, "via")?,
        ).await.map(|_| Value::Null),
        "agw_send_ui" => crate::commands::agw_send_ui_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "dest")?, arg(a, "dest_ssid")?,
            arg(a, "text")?, arg(a, "via")?,
        ).await.map(|_| Value::Null),
        "send_ui_frame" => crate::commands::send_ui_frame_impl(
            st, out.clone(), arg(a, "conn_id")?, arg(a, "dest")?, arg(a, "dest_ssid")?,
            arg(a, "text")?, arg(a, "via")?,
        ).await.map(|_| Value::Null),

        // ── message board admin console ──
        "pbbs_admin_open" => crate::commands::pbbs_admin_open_impl(st, out.clone()).await.map(val),
        "pbbs_admin_reset" => crate::commands::pbbs_admin_reset_impl(st, out.clone()).await.map(val),
        "pbbs_admin_send" => crate::commands::pbbs_admin_send_impl(st, out.clone(), arg(a, "line")?).await.map(val),
        "pbbs_users" => crate::commands::pbbs_users_impl(st).await.map(val),

        // ── settings / history / logs ──
        "save_settings" => crate::settings::save_settings(arg(a, "settings")?).await.map(|_| Value::Null),
        "load_settings" => crate::settings::load_settings().await.map(val),
        "reset_settings" => crate::settings::reset_settings().await.map(|_| Value::Null),
        // No window in this mode: report success without letting a browser
        // write window geometry that belongs to the desktop session.
        "save_window_state" => Ok(Value::Null),
        "save_history" => crate::history::save_history(
            arg(a, "key")?, arg(a, "value")?, arg(a, "max")?,
        ).map(|_| Value::Null),
        "load_history" => crate::history::load_history(arg(a, "key")?).map(val),
        "remove_history" => crate::history::remove_history(arg(a, "key")?, arg(a, "value")?).map(|_| Value::Null),
        "clear_history" => crate::history::clear_history(arg(a, "key")?).map(|_| Value::Null),
        "append_log" => crate::commands::append_log(arg(a, "kind")?, arg(a, "text")?).map(|_| Value::Null),
        // The browser cannot open a folder on the server; the client opens
        // /api/logs instead (see src/transport.js).
        "open_log_dir" => Err(format!(
            "Logs live on the server: {}",
            crate::LOG_DIR.get().map(|d| d.display().to_string()).unwrap_or_default()
        )),

        // ── info ──
        "web_info" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "mode": "web",
            "dataDir": crate::storage::data_base_static().display().to_string(),
        })),

        other => Err(format!("Unknown command: {other}")),
    };
    r
}
