// ─ K2K Packet Desktop — app state (the single structural owner) ─
//
// Seam 2 + 4: introduce `AppCore` as the one facade the command layer talks
// to, and have `AppState` hold a single `Mutex<AppCore>` (one lock) instead of
// a bare `Mutex<Vec<...>>`. Today `AppCore` owns the live KISS/AGW connection
// list; as the rewrite proceeds the same struct grows to own the Radio / Link
// adapters / settings / board (BBS) / output sink, so `AppState` stays a thin
// shell around a single owned structure.

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::kernel::session::TcpClient;

/// The application core: everything the command layer needs hangs off this one
/// owned structure. All accesses go through `AppState::core()` (a single lock),
/// so there is exactly one mutex protecting app state rather than many.
pub struct AppCore {
    pub connections: Vec<Arc<Mutex<TcpClient>>>,
    /// Live PBBS admin-console session. Holds the in-RAM BBS session state
    /// (pending destination, current command step) across `pbbs_admin_*`
    /// calls. Connection-independent: the admin console is a LOCAL, no-RF
    /// console, so it does not require a connected TNC — the board store is
    /// loaded fresh from settings each open/send (it persists to disk).
    pub admin_bbs: Option<crate::bbs::BbsSession>,
}

impl AppCore {
    pub fn new() -> Self {
        Self {
            connections: Vec::new(),
            admin_bbs: None,
        }
    }
    /// True when the named connection is an AGWPE (Direwolf) link.
    pub async fn connection_is_agw(&self, conn_id: &str) -> bool {
        self.connections.iter().any(|arc| {
            if let Ok(c) = arc.try_lock() {
                c.info.id == conn_id && c.protocol == crate::session::Protocol::AGW
            } else {
                false
            }
        })
    }

    /// Lock and return the connection with the given id, if present.
    pub async fn find_client(&self, conn_id: &str) -> Option<Arc<Mutex<TcpClient>>> {
        self.connections
            .iter()
            .find(|arc| match arc.try_lock() {
                Ok(c) => c.info.id == conn_id,
                Err(_) => false, // Locked elsewhere — treat as no match
            })
            .cloned()
    }

    /// Abort all background tasks owned by a client (reader, beacon,
    /// keep-alives, per-session retry timers) and mark it disconnected. Used
    /// before removing a connection from the list.
    fn abort_client_tasks(c: &mut TcpClient) {
        c.info.connected = false;
        if let Some(h) = c.reader_handle.take() {
            h.abort();
        }
        if let Some(h) = c.beacon_handle.take() {
            h.abort();
        }
        for ka in &mut c.ka_handles {
            if let Some(h) = ka.take() {
                h.abort();
            }
        }
        // Cancel every link-layer timer in the bank by value (the idempotency
        // authority). A session's T1/T2/T3 slots are dropped here, so no late
        // deadline can fire after teardown.
        use crate::session::{SessionLoc, MAX_TABS};
        for tab in 0..MAX_TABS {
            c.timers.cancel_all_for(SessionLoc::Tab(tab));
        }
        let nsessions = c.server_sessions.len();
        for i in 0..nsessions {
            c.timers.cancel_all_for(SessionLoc::Server(i));
        }
        // Per-session timers live on the sessions, NOT the client — abort them
        // all and reset. A surviving T1 recovery timer previously held an Arc
        // clone of the TcpClient, keeping the TNC stream open and continuing to
        // send RR enquiries after disconnect until the app exited.
        for s in &mut c.info.sessions {
            s.reset();
            s.state = crate::session::Ax25ConnState::Disconnected;
            s.remote.clear();
        }
        // Drop the TNC socket so the port closes immediately instead of waiting
        // for every task-held Arc clone of the client to release.
        c.stream = None;
    }

    /// Remove every connection to a given host:port, aborting their tasks.
    pub async fn remove_connections_to(&mut self, host: &str, port: u16) {
        self.connections.retain(|client_arc| {
            if let Ok(mut c) = client_arc.try_lock() {
                let same = c.info.host == host && c.info.port == port;
                if same {
                    Self::abort_client_tasks(&mut c);
                }
                !same
            } else {
                true
            }
        });
    }

    /// Remove the connection with the given id, aborting its tasks.
    /// Returns true if a connection was actually removed.
    pub async fn remove_connection_by_id(&mut self, id: &str) -> bool {
        let mut removed = false;
        self.connections.retain(|client_arc| {
            match client_arc.try_lock() {
                Ok(mut c) if c.info.id == id => {
                    Self::abort_client_tasks(&mut c);
                    removed = true;
                    false // Remove from the Vec
                }
                _ => true, // Keep (different id or couldn't lock)
            }
        });
        removed
    }
}

/// Global managed state handed to every `#[tauri::command]` via
/// `tauri::State<'_, AppState>` in the app layer.
pub struct AppState {
    pub core: Mutex<AppCore>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            core: Mutex::new(AppCore::new()),
        }
    }
}