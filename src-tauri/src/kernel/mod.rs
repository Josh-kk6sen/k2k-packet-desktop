// ── K2K Packet Desktop — Kernel (pure Rust, no Tauri) ────────────────
//
// Phase 1 of the rewrite milestone (branch k2k-rewrite-010, target v0.10.0).
//
// This directory holds the modules that MUST stay free of any `tauri::`
// dependency, so the AX.25 / PBBS / NET-ROM logic is unit-testable with no
// GUI and no radio. During Phase 1 these are a *pure move*: the files and
// their behaviour are byte-for-byte identical to where they lived at src/
// root; the crate still resolves them through `pub use` re-exports in lib.rs,
// so no call site changed.
//
// Seam rule (from DESIGN/rewrite.md):
//   kernel/*  must never import tauri. In later phases the effect seam
//   (timers.rs, the Effect rather than emit() calls) lands here.
//   One exception to the purity note: session.rs re-exports `app::state::AppState`
//   (which itself is tauri-free) so call sites keep a stable `crate::session::AppState`
//   path — the kernel stays free of `tauri::`, just not of the `app/` module tree.
//
// Modules handed over so far (moved unchanged from crate root):
//   - session.rs   AX.25 connected-mode state machine (Client/Server roles)
//   - ax25.rs      AX.25 frame parse / build / classify
//   - bbs.rs       Message Board (PBBS) engine — pure
//   - bbs_link.rs  Board gate + setup snapshot (kernel-merge B1 trim)
//   - netrom.rs    NET/ROM protocol parsing
//   - output.rs    `KernelOutput` trait + emitters (the effect seam's types)
//   - timers.rs    T1/T2/T3 + keepalive timer bank (as data)
//
// TODO(next): moving the transport faces (kiss.rs / agw.rs) under link/, then
// a thin app/ + thin commands.rs. (timers.rs already landed — see above.)

pub mod ax25;
pub mod bbs;
pub mod bbs_link;
pub mod netrom;
pub mod output;
pub mod session;
pub mod timers;