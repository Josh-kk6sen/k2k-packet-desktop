// ─ K2K Packet Desktop — kernel output seam (the only way the kernel talks outward)
//
// Phase 2.1: extract the ONE trait the kernel needs to reach the UI/side-app,
// so `kernel/` stops importing `tauri`. The kernel builds payloads (pure JSON,
// pure string labels) and hands them to a `KernelOutput`; the tauri
// implementation (`app::TauriOutput`) is the only place that touches an
// `AppHandle`. A test/fake impl drives a unit test of the mapper.
//
// Rule (DESIGN/rewrite.md §2): kernel may define the trait (a contract), it
// may NOT name `tauri`. Implementation lives in `app/`.

use crate::kernel::bbs_link::BbsGate;
use crate::settings::AppSettings;
use std::path::PathBuf;

/// Everything the kernel needs from the outside world. Implementations must be
/// `Send + Sync` so they can be cloned into spawned task runtimes.
pub trait KernelOutput: Send + Sync {
    /// Emit a strongly-typed UI event to the frontend.
    fn emit(&self, event: &str, payload: serde_json::Value);

    /// Load the app settings (from disk/cache). `Ok(None)` = no settings file.
    fn load_settings(&self) -> Result<Option<AppSettings>, String>;

    /// The single base directory for all app data (settings/history/logs/bbs).
    fn data_dir(&self) -> PathBuf;

    /// Write a line to the daily log.
    fn log_line(&self, msg: &str);

    /// Snapshot/report a connection-level operation to the log.
    fn log_packet(&self, data: &[u8], kind: &str);

    /// Re-read the message-board settings into effect (mid-session change).
    fn refresh_bbs_setup(&self);
}

/// Live-read the message-board gate from settings, cached 1s to avoid
/// per-frame disk reads. Pure wrapper over `load_settings`.
pub fn bbs_gate(out: &dyn KernelOutput) -> BbsGate {
    match out.load_settings() {
        Ok(Some(s)) => crate::kernel::bbs_link::compute_bbs_gate(&s),
        Ok(None) => BbsGate::NotConfigured,
        Err(e) => {
            out.log_line(&format!(
                "Message board: settings unreadable ({e}) — board off"
            ));
            BbsGate::NotConfigured
        }
    }
}

/// A no-op output for contexts that have no UI (headless tests, invisible
/// sessions). Test-only.
#[cfg(test)]
pub struct NullOutput;
#[cfg(test)]
impl KernelOutput for NullOutput {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
    fn load_settings(&self) -> Result<Option<AppSettings>, String> {
        Ok(Some(crate::settings::AppSettings::default()))
    }
    fn data_dir(&self) -> PathBuf {
        PathBuf::new()
    }
    fn log_line(&self, _msg: &str) {}
    fn log_packet(&self, _data: &[u8], _kind: &str) {}
    fn refresh_bbs_setup(&self) {}
}

/// Convenience: build + deliver an unproto-panel frame through the sink.
pub fn emit_unproto_frame(
    out: &dyn KernelOutput,
    source: &str,
    dest: &str,
    info: &str,
    formatted: &str,
    frame_type: &str,
    digis: &[String],
    radio_header: &str,
    raw_kiss: &str,
    sent: bool,
) {
    out.emit(
        "unproto-frame",
        serde_json::json!({
            "source": source, "dest": dest, "info": info, "formatted": formatted,
            "frame_type": frame_type, "digis": digis, "radio_header": radio_header,
            "raw_kiss_data": raw_kiss, "sent": sent,
        }),
    );
}

/// Emit any locally-transmitted AX.25 frame to the unproto monitor.
pub fn emit_tx_frame(out: &dyn KernelOutput, raw_frame: &[u8]) {
    if let Some(f) = crate::kernel::ax25::parse_ax25_frame(raw_frame) {
        use crate::kernel::ax25::{
            build_radio_header, classify_frame_type, fmt_call, fmt_digi_labels, fmt_raw_kiss,
            info_to_string,
        };
        let frame_type = classify_frame_type(f.control);
        let src_label = fmt_call(&f.src, f.src_ssid);
        let dest_label = fmt_call(&f.dest, f.dest_ssid);
        let digi_labels: Vec<String> = fmt_digi_labels(&f.digis);
        let radio_header = build_radio_header(
            &src_label,
            &dest_label,
            &f.digis,
            &frame_type,
            f.control,
            f.pid,
            f.info.len(),
        );
        let raw_kiss_hex = fmt_raw_kiss(raw_frame);
        let info_text = info_to_string(&f.info);
        let formatted = if info_text.is_empty() {
            format!("{}>{}: [{}]", src_label, dest_label, frame_type)
        } else {
            format!("{}>{}: {}", src_label, dest_label, info_text)
        };
        emit_unproto_frame(
            out,
            &src_label,
            &dest_label,
            &info_text,
            &formatted,
            &frame_type,
            &digi_labels,
            &radio_header,
            &raw_kiss_hex,
            true,
        );
    }
}
