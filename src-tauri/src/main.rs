// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Disable WebKit overlay scrollbars so CSS ::-webkit-scrollbar{width} takes effect
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_OVERLAY_SCROLLBARS", "1");
    k2k_packet_desktop_lib::run()
}
