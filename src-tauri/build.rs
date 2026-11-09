fn main() {
    // The desktop build embeds tauri.conf.json, generates the capability
    // schemas and sets the env the window needs. The headless build
    // (`--no-default-features`, i.e. no `tauri` crate) has none of that, and
    // running tauri-build there would only re-introduce a build-time GTK
    // dependency — so it is skipped.
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        tauri_build::build();
    }
}
