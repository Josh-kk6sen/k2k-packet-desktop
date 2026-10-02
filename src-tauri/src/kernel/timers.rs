// ── K2K Packet Desktop — kernel/timers.rs ───────────────────────────────
//
// Phase 2 of the rewrite milestone: make the AX.25 link-layer timers *data*
// instead of opaque `tokio::JoinHandle` fields buried in a session struct.
//
// Why this matters (from DESIGN/rewrite.md §3.3): the explicit audit finding
// was a `keepalive_handle` that was declared and `.take()`n but never signed,
// and a re-SABM that spawned a second keepalive without aborting the first →
// duplicate T3 pollers. That whole bug class is "a handle got lost in a
// struct." Timing-as-data eliminates it: a timer is an entry in a map,
// scheduled and cancelled **by value**, never a task you can lose.
//
// The kernel emits `Effect::Schedule(TimerKind, loc, deadline)` and
// `Effect::Cancel(TimerKind, loc)`; one owned `TimerBank` holds every
// deadline; a single driver (the session engine / commands layer) waits for
// the nearest deadline and wakes the kernel with `Effect::TimerNow(timer)`.
//
// No `tauri` here — this module must stay importable by the pure kernel,
// proving the seam (§6 gate: kernel compiles & tests with no tauri).

use crate::kernel::session::SessionLoc;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The AX.25 link-layer timers the engine drives, plus the app beacon.
/// Matches the kernel `T1/T2/T3` + keepalive timers and the periodic beacon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimerKind {
    /// T1 — retransmission timeout (connect / I-frame / recovery).
    T1,
    /// T2 — delayed-ACK for a received I-frame burst (fire when peer goes
    /// quiet or polls P=1).
    T2,
    /// T3 — keep-alive / idle timeout (server + connected sessions).
    T3,
    /// Periodic app beacon.
    Beacon,
}

/// One scheduled timer: a deadline owned by a session (or the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerID {
    pub kind: TimerKind,
    pub loc: SessionLoc,
}

impl TimerID {
    pub fn new(kind: TimerKind, loc: SessionLoc) -> Self {
        Self { kind, loc }
    }
}

/// The single owned deadline store. Every timer the engine needs lives here,
/// keyed by (kind, session). Scheduling is idempotent: re-scheduling an
/// already-live timer just resets its deadline (never spawns a duplicate) —
/// which is exactly the "second keepalive without aborting the first" bug it
/// replaces.
#[derive(Debug, Default)]
pub struct TimerBank {
    deadlines: HashMap<TimerID, Instant>,
}

impl TimerBank {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many timers are currently scheduled (for diagnostics/tests).
    pub fn len(&self) -> usize {
        self.deadlines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.deadlines.is_empty()
    }

    /// Schedule (or re-schedule) a timer to fire at `at`. If the timer was
    /// already scheduled, its deadline is **reset** to the new `at` — there is
    /// no second task, no duplicate poller. Returns `true` if this was a
    /// re-schedule (the timer already existed), `false` if brand new.
    pub fn schedule(&mut self, id: TimerID, at: Instant) -> bool {
        self.deadlines.insert(id, at).is_some()
    }

    /// Convenience: schedule `after` from now.
    pub fn schedule_in(&mut self, id: TimerID, after: Duration) -> bool {
        self.schedule(id, Instant::now() + after)
    }

    /// Cancel a timer by value. Returns `true` if it was actually scheduled.
    pub fn cancel(&mut self, id: TimerID) -> bool {
        self.deadlines.remove(&id).is_some()
    }

    /// The nearest deadline, if any. The driver waits on this.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.values().copied().min()
    }

    /// Drop every timer for a given session (e.g. on disconnect/reset).
    /// Returns how many were dropped.
    pub fn cancel_all_for(&mut self, loc: SessionLoc) -> usize {
        let keep: Vec<(TimerID, Instant)> = self
            .deadlines
            .iter()
            .filter(|(id, _)| id.loc != loc)
            .map(|(id, at)| (*id, *at))
            .collect();
        let dropped = self.deadlines.len() - keep.len();
        self.deadlines = keep.into_iter().collect();
        dropped
    }

    /// Take the timers due at `now`. Each due timer is removed and returned;
    /// the driver maps it to `Effect::TimerNow`. `now` is injectable so the
    /// scheduler is deterministic in tests (no real clock needed).
    pub fn take_due(&mut self, now: Instant) -> Vec<TimerID> {
        let mut due = Vec::new();
        let mut remaining = HashMap::new();
        for (id, at) in self.deadlines.drain() {
            if at <= now {
                due.push(id);
            } else {
                remaining.insert(id, at);
            }
        }
        self.deadlines = remaining;
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(kind: TimerKind) -> TimerID {
        TimerID::new(kind, SessionLoc::Tab(0))
    }

    #[test]
    fn schedule_then_due_returns_it() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        bank.schedule(id(TimerKind::T2), now + Duration::from_millis(10));
        assert_eq!(bank.len(), 1);
        let due = bank.take_due(now + Duration::from_millis(11));
        assert_eq!(due, vec![id(TimerKind::T2)]);
        assert!(bank.is_empty());
    }

    #[test]
    fn not_due_is_kept() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        bank.schedule(id(TimerKind::T1), now + Duration::from_millis(100));
        assert!(bank.take_due(now + Duration::from_millis(50)).is_empty());
        assert_eq!(bank.len(), 1);
    }

    #[test]
    fn reschedule_is_idempotent_and_returns_true() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        assert!(!bank.schedule(id(TimerKind::T3), now + Duration::from_secs(1)));
        assert_eq!(bank.len(), 1);
        // Re-scheduling the SAME timer just moves its deadline — no duplicate.
        assert!(bank.schedule(id(TimerKind::T3), now + Duration::from_secs(2)));
        assert_eq!(bank.len(), 1, "reschedule must not add a second timer");
    }

    #[test]
    fn cancel_removes_and_returns_true_then_noop() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        let t = id(TimerKind::Beacon);
        bank.schedule(t, now + Duration::from_secs(1));
        assert!(bank.cancel(t));
        assert!(bank.is_empty());
        assert!(!bank.cancel(t), "second cancel is a no-op");
    }

    #[test]
    fn cancel_all_for_session_clears_only_that_session() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        let a = TimerID::new(TimerKind::T2, SessionLoc::Tab(0));
        let b = TimerID::new(TimerKind::T3, SessionLoc::Tab(1));
        let c = TimerID::new(TimerKind::T1, SessionLoc::Server(0));
        bank.schedule(a, now + Duration::from_secs(1));
        bank.schedule(b, now + Duration::from_secs(1));
        bank.schedule(c, now + Duration::from_secs(1));
        assert_eq!(bank.cancel_all_for(SessionLoc::Tab(0)), 1);
        assert_eq!(bank.len(), 2, "other sessions' timers survive");
        assert!(bank.next_deadline().is_some());
    }

    #[test]
    fn next_deadline_is_earliest() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        bank.schedule(id(TimerKind::T3), now + Duration::from_secs(5));
        bank.schedule(id(TimerKind::T2), now + Duration::from_millis(50));
        bank.schedule(id(TimerKind::T1), now + Duration::from_secs(2));
        assert_eq!(bank.next_deadline(), Some(now + Duration::from_millis(50)));
    }

    #[test]
    fn due_timers_removed_in_batch() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        bank.schedule(id(TimerKind::T2), now); // due exactly at now
        bank.schedule(TimerID::new(TimerKind::T2, SessionLoc::Tab(1)), now + Duration::from_secs(60));
        bank.schedule(TimerID::new(TimerKind::Beacon, SessionLoc::Tab(0)), now + Duration::from_millis(1));
        let mut due = bank.take_due(now + Duration::from_millis(1));
        due.sort_by_key(|d| d.loc.index());
        assert_eq!(due.len(), 2);
        assert_eq!(bank.len(), 1);
    }

    #[test]
    fn cross_session_timers_are_distinct() {
        let mut bank = TimerBank::new();
        let now = Instant::now();
        let a = TimerID::new(TimerKind::T2, SessionLoc::Tab(0));
        let b = TimerID::new(TimerKind::T2, SessionLoc::Tab(2));
        bank.schedule(a, now + Duration::from_secs(1));
        bank.schedule(b, now + Duration::from_secs(1));
        assert_eq!(bank.len(), 2, "same kind on different sessions are distinct");
    }

    #[test]
    fn recovery_slot_reserves_before_spawn() {
        // Mirrors ensure_recovery_timer: asking for the same T1 on the same
        // session twice must NOT produce a second deadline — the bank is the
        // idempotency authority that prevents duplicate T1 pollers.
        let mut bank = TimerBank::new();
        let now = Instant::now();
        let id = TimerID::new(TimerKind::T1, SessionLoc::Tab(0));
        let first = bank.schedule(id, now + Duration::from_millis(1000));
        assert!(!first, "first reserve is brand-new");
        // A second request re-schedules (returns true) — the caller must treat
        // "already scheduled" as "no duplicate spawn needed".
        let second = bank.schedule(id, now + Duration::from_millis(1000));
        assert!(second, "duplicate reserve detected");
        assert_eq!(bank.len(), 1, "never two T1 slots for the same session");
        // After the deadline fires, the slot is free for a fresh recover.
        assert_eq!(bank.take_due(now + Duration::from_millis(1001)), vec![id]);
        assert!(bank.is_empty());
    }

    #[test]
    fn teardown_cancels_all_sessions() {
        // Mirrors abort_client_tasks: on disconnect we cancel every scheduled
        // timer for every tab/server slot so no late deadline fires.
        let mut bank = TimerBank::new();
        let now = Instant::now();
        let ids = [
            TimerID::new(TimerKind::T1, SessionLoc::Tab(0)),
            TimerID::new(TimerKind::T2, SessionLoc::Tab(1)),
            TimerID::new(TimerKind::T3, SessionLoc::Tab(2)),
            TimerID::new(TimerKind::T1, SessionLoc::Server(0)),
            TimerID::new(TimerKind::T3, SessionLoc::Server(3)),
        ];
        for id in &ids {
            bank.schedule(*id, now + Duration::from_secs(5));
        }
        assert_eq!(bank.len(), 5);
        for i in 0..4 {
            bank.cancel_all_for(SessionLoc::Tab(i));
        }
        for i in 0..4 {
            bank.cancel_all_for(SessionLoc::Server(i));
        }
        // Every tab (0..4) and server-slot (0..4) timer is cancelled by value;
        // the Server(3) entry falls within the loop range, so all 5 are dropped.
        assert!(
            bank.is_empty(),
            "teardown should cancel every timer by value: left {}",
            bank.len()
        );
    }
}