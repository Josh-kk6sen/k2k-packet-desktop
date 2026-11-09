// ─ K2K — type-only stand-ins for `tauri` in a headless build ─
//
// `cargo build --no-default-features` compiles this crate without the `tauri`
// dependency, so the binary links no GTK / WebKitGTK. That is the point of the
// headless package: it installs on a lite Raspberry Pi OS image in seconds and
// serves the identical client over HTTP (`--web`).
//
// The `#[tauri::command]` wrappers in `commands.rs` / `settings.rs` /
// `history.rs` are desktop-window entry points; headless mode dispatches to
// the same `*_impl` bodies over `/api/invoke` (see `web.rs`). They still have
// to *compile* without Tauri, so this module supplies the only two types they
// name. Nothing here is constructible: `AppHandle` has no public constructor,
// so a headless process can never build a window sink.

use std::ops::Deref;

/// Stand-in for `tauri::State<'_, T>`; the wrappers only ever deref it to
/// `&T` when they delegate to the `*_impl` body.
pub struct State<'a, T>(&'a T);

impl<T> Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

/// Stand-in for `tauri::AppHandle`. Uninhabited on purpose — there is no way
/// to obtain one, which is what makes the desktop-only sinks unreachable.
pub struct AppHandle(std::convert::Infallible);

impl Clone for AppHandle {
    fn clone(&self) -> Self {
        match self.0 {}
    }
}
