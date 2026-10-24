// ── Message Board (PBBS) Engine ───────────────────────────
//
// Rust port of Josh's `my_bbs.py` (pre-form-reply version, May 8 2026) —
// byte-for-byte command set, output format, and JSON data schema:
//
//   pbbs_data.json  { messages: [{id, st, to, from, date, subject, body,
//                   read, pinned}], next_id }
//   pbbs_heard.json { CALLSIGN: "MM/DD/YY HH:MM:SS" }
//
// The Python setup wizard is replaced by the app's Settings page (Message
// Board section); config arrives as BbsConfig. Data files live in the app's
// settings dir (same folder as settings.json), with the same .bak-on-save
// self-healing behavior as the Python original.
//
// This module is pure logic — no radio I/O. The session layer (session.rs)
// feeds it one line at a time and sends its output lines as I-frames.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── Data Model (identical JSON schema to the Python PBBS) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BbsMessage {
    pub id: u32,
    pub st: String,
    pub to: String,
    pub from: String,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub read: bool,
    pub pinned: bool,
    /// Digipeater path used when the message was written — shown as
    /// "PATH: ..." on read (JBOX parity). Empty for locally-written
    /// messages (admin console) and messages from before 0.9.53.
    #[serde(default)]
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BbsData {
    pub messages: Vec<BbsMessage>,
    pub next_id: u32,
}

/// Python default when pbbs_data.json is missing: next_id starts at 1.
impl Default for BbsData {
    fn default() -> Self {
        BbsData { messages: Vec::new(), next_id: 1 }
    }
}

/// Configuration that the Python wizard collected at first run. Now
/// populated from the app's Settings → Message Board section.
#[derive(Debug, Clone, Default)]
pub struct BbsConfig {
    /// Station name shown in the banner and prompt. Now derived from the
    /// board callsign + ssid (e.g. "KK6SEN-1") — the Node Name setting was
    /// removed per user request.
    pub node_name: String,
    /// Callsigns with Delete/Pin admin rights (is_sysop).
    pub sysops: Vec<String>,
    /// Privacy: users only see mail to/from them or public aliases.
    pub privacy_enabled: bool,
    /// Group destinations anyone can read ("ALL" included by default).
    pub public_aliases: Vec<String>,
}

impl BbsConfig {
    /// Build from app settings; falls back to the Python defaults.
    pub fn from_app_settings(s: &crate::settings::AppSettings) -> Self {
        let mut aliases: Vec<String> = s
            .message_board_public_aliases
            .split(',')
            .map(|a| a.trim().to_uppercase())
            .filter(|a| !a.is_empty())
            .collect();
        if !aliases.iter().any(|a| a == "ALL") {
            aliases.push("ALL".to_string());
        }
        BbsConfig {
            // Node name is the board callsign + ssid (e.g. "KK6SEN-1");
            // the separate Node Name setting was removed.
            node_name: {
                let call = s.message_board_callsign.trim().to_uppercase();
                if call.is_empty() {
                    "MYNODE".to_string()
                } else if s.message_board_ssid > 0 {
                    format!("{}-{}", call, s.message_board_ssid)
                } else {
                    call
                }
            },
            sysops: {
                let mut sysops: Vec<String> = s
                    .message_board_sysops
                    .split(',')
                    .map(|c| c.trim().to_uppercase())
                    .filter(|c| !c.is_empty())
                    .collect();
                // The station's own callsign is ALWAYS a sysop — the local
                // PBBS admin console runs as this callsign and must have
                // full Delete/Pin rights (and full visibility in privacy
                // mode) regardless of the Sysops setting.
                let own = s.callsign.trim().to_uppercase();
                if !own.is_empty() && !sysops.iter().any(|c| c == &own) {
                    sysops.push(own);
                }
                sysops
            },
            privacy_enabled: s.message_board_privacy,
            public_aliases: aliases,
        }
    }
}

// ── Store (pbbs_data.json / pbbs_heard.json) ──────────────

/// JSON message/heard store with .bak self-healing (Python parity).
pub struct BbsStore {
    data_path: PathBuf,
    heard_path: PathBuf,
    pub data: BbsData,
    pub heard: HashMap<String, String>,
}

impl BbsStore {
    /// Load both files from `dir` (the app's settings dir). Corrupted
    /// files recover from their .bak copy; missing files start empty —
    /// exactly the Python behavior.
    pub fn load(dir: &std::path::Path) -> Self {
        BbsStore {
            data_path: dir.join("pbbs_data.json"),
            heard_path: dir.join("pbbs_heard.json"),
            data: load_json(&dir.join("pbbs_data.json"), BbsData::default()),
            heard: load_json(&dir.join("pbbs_heard.json"), HashMap::new()),
        }
    }

    pub fn save_data(&mut self) -> Result<(), String> {
        save_json(&self.data_path, &self.data)
    }

    pub fn save_heard(&mut self) -> Result<(), String> {
        save_json(&self.heard_path, &self.heard)
    }

    /// Record that `call` connected now (J command source data).
    pub fn record_heard(&mut self, call: &str) {
        self.heard.insert(call.to_string(), now_str());
        if let Err(e) = self.save_heard() {
            crate::commands::log_line(&format!("Message board: heard save failed: {e}"));
        }
    }
}

fn load_json<T: serde::de::DeserializeOwned + Default + Serialize>(path: &std::path::Path, default: T) -> T {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("*** Warning: {} is corrupted.", path.file_name().unwrap_or_default().to_string_lossy());
                let bak = PathBuf::from(format!("{}.bak", path.display()));
                match std::fs::read_to_string(&bak) {
                    Ok(btext) => match serde_json::from_str(&btext) {
                        Ok(v) => {
                            eprintln!("*** Restoring from backup: {}", bak.file_name().unwrap_or_default().to_string_lossy());
                            // Write the recovered data straight to the main file —
                            // NOT via save_json: its .bak-copy-first step would copy
                            // the still-corrupted main file over the only good
                            // backup, so a second corruption would lose everything.
                            let _ = write_json_atomic(path, &v);
                            return v;
                        }
                        Err(_) => eprintln!("*** Critical: Backup also corrupted."),
                    },
                    Err(_) => eprintln!("*** Critical: Backup also corrupted."),
                }
                default
            }
        },
        Err(_) => default,
    }
}

/// Atomic write (tmp + rename; Windows fallback) that never touches the
/// .bak file — used for both normal saves (after the .bak copy) and
/// self-heal restores (which must NOT clobber the good backup).
fn write_json_atomic<T: Serialize>(path: &std::path::Path, data: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(data).map_err(|e| format!("serialize: {e}"))?;
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, &text).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    if let Err(_e) = std::fs::rename(&tmp, path) {
        // Windows: rename over an existing file can fail — remove+rename.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path).map_err(|e| format!("rename to {}: {}", path.display(), e))?;
    }
    Ok(())
}

/// .bak copy first (Python parity), then an atomic write so a crash
/// mid-write can't leave a truncated main file. Returns the failure for
/// callers to surface — the old code swallowed every error and replied
/// "MESSAGE SAVED" even when the disk write failed.
fn save_json<T: Serialize>(path: &std::path::Path, data: &T) -> Result<(), String> {
    if path.exists() {
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        std::fs::copy(path, &bak).map_err(|e| format!("copy to {}: {}", bak.display(), e))?;
    }
    write_json_atomic(path, data)
}

// ── Date / Disk helpers (Python datetime.now / shutil.disk_usage) ──

/// "%m/%d/%y %H:%M:%S" in local time — matches Python datetime.now().
pub fn now_str() -> String {
    chrono::Local::now().format("%m/%d/%y %H:%M:%S").to_string()
}

// ── Session (one connected BBS user) ──────────────────────

/// Where the session is in a multi-step command flow. The Python version
/// interleaves input() calls mid-command; we model those as explicit modes.
#[derive(Debug, Clone, PartialEq)]
pub enum BbsInputMode {
    Command,
    /// After S/SB/SP — waiting for the SUBJECT line.
    Subject { kind: char },
    /// After subject — collecting body lines until "/EX".
    Body { kind: char, dest: String, subj: String, mid: u32 },
    /// After "K n" — waiting for y/N confirmation.
    DeleteConfirm { id: u32 },
    /// After "KM" — waiting for y/N confirmation.
    KillMineConfirm,
}

/// Output of one handled input line.
#[derive(Debug, Default, PartialEq)]
pub struct BbsReply {
    pub lines: Vec<String>,
    /// True when the user typed B(ye) — the link should send DISC.
    pub hangup: bool,
}

/// Save the store and translate a failure into a user-visible reply line.
/// The Python original printed "MESSAGE SAVED" even when the disk write
/// failed; a silent data loss is worse — log to the in-app Log viewer and
/// tell the user.
fn save_reply(store: &mut BbsStore, ok: &str) -> BbsReply {
    match store.save_data() {
        Ok(()) => BbsReply { lines: vec![ok.to_string()], hangup: false },
        Err(e) => {
            crate::commands::log_line(&format!("Message board: data save failed: {e}"));
            BbsReply { lines: vec!["*** SAVE FAILED (see Log viewer)".to_string()], hangup: false }
        }
    }
}

pub struct BbsSession {
    pub user: String,
    pub mode: BbsInputMode,
    /// Body lines collected while in Body mode (Python body.append).
    pending_body: String,
    /// Destination captured at S/SB/SP, consumed at the SUBJECT line.
    pending_dest: String,
    /// Digipeater path of the connected peer, captured at session creation
    /// (the "PATH:" recorded on messages written over this link).
    pub path: Vec<String>,
}

impl BbsSession {
    pub fn new(user: &str) -> Self {
        BbsSession {
            user: user.to_uppercase(),
            mode: BbsInputMode::Command,
            pending_body: String::new(),
            pending_dest: String::new(),
            path: Vec::new(),
        }
    }

    /// The welcome banner + a message-count summary sent on connect.
    /// The full list is NOT sent — it can get long, so it loads on demand
    /// with L(Browse) instead. The range is the lowest..highest message id
    /// currently on the board (ids are never reused, so with deletions the
    /// range can be wider than the count).
    pub fn connect_output(&self, store: &BbsStore, cfg: &BbsConfig) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "*** Welcome {} to the {} K2K Packet Message Board ***",
            self.user, cfg.node_name
        ));
        if store.data.messages.is_empty() {
            out.push("THERE ARE 0 MESSAGES".to_string());
        } else {
            let lo = store.data.messages.iter().map(|m| m.id).min().unwrap_or(0);
            let hi = store.data.messages.iter().map(|m| m.id).max().unwrap_or(0);
            out.push(format!(
                "THERE ARE {} MESSAGES NUMBERED {}-{}",
                store.data.messages.len(),
                lo,
                hi
            ));
        }
        out
    }

    /// The command prompt, printed after the banner and after every reply
    /// (Python main() prints it before each input read). Sysops get the
    /// extra P command in the prompt.
    pub fn prompt(&self, cfg: &BbsConfig) -> String {
        if is_sysop(&self.user, cfg) {
            "ENTER COMMAND:  B,J,K,L,R,S,P, or Help >".to_string()
        } else {
            "ENTER COMMAND:  B,J,K,L,R,S, or Help >".to_string()
        }
    }

    /// True when the session is at the command menu. The link layer sends
    /// the prompt only then — while the peer is writing a message (Subject
    /// or Body mode) or confirming a delete, a command prompt is noise
    /// ("no command line while writing a message").
    pub fn at_command(&self) -> bool {
        matches!(self.mode, BbsInputMode::Command)
    }

    /// Handle one line of input; returns output lines (Python's print()s).
    pub fn handle_line(&mut self, line: &str, store: &mut BbsStore, cfg: &BbsConfig) -> BbsReply {
        match &self.mode {
            BbsInputMode::Subject { kind } => {
                let kind = *kind;
                let subj = line.trim();
                let subj = if subj.is_empty() {
                    "(No Subject)".to_string()
                } else {
                    subj.chars().take(50).collect()
                };
                let dest = self.take_pending_dest();
                let mid = store.data.next_id;
                store.data.next_id += 1;
                self.mode = BbsInputMode::Body { kind, dest, subj, mid };
                BbsReply {
                    lines: vec![format!("\nENTER MESSAGE {}--END WITH /EX", mid)],
                    hangup: false,
                }
            }
            BbsInputMode::Body { kind, dest, subj, mid } => {
                if line.trim().eq_ignore_ascii_case("/EX") {
                    // Python appends whatever body was collected (possibly empty)
                    let (kind, dest, subj, mid) = (kind.clone(), dest.clone(), subj.clone(), *mid);
                    // Body text was captured into the pending buffer — see below.
                    let body = std::mem::take(&mut self.pending_body);
                    store.data.messages.push(BbsMessage {
                        id: mid,
                        st: if kind == 'B' { "B".to_string() } else { "P".to_string() },
                        to: dest,
                        from: self.user.clone(),
                        date: now_str(),
                        subject: subj,
                        body,
                        read: false,
                        pinned: false,
                        path: self.path.clone(),
                    });
                    self.mode = BbsInputMode::Command;
                    save_reply(store, &format!("MESSAGE SAVED to # {}", mid))
                } else {
                    // Accumulate body line (Python: body.append(line))
                    if !self.pending_body.is_empty() {
                        self.pending_body.push('\n');
                    }
                    self.pending_body.push_str(line);
                    BbsReply::default()
                }
            }
            BbsInputMode::DeleteConfirm { id } => {
                let id = *id;
                self.mode = BbsInputMode::Command;
                if line.trim().eq_ignore_ascii_case("Y") {
                    store.data.messages.retain(|m| m.id != id);
                    save_reply(store, "MESSAGE DELETED")
                } else {
                    BbsReply::default()
                }
            }
            BbsInputMode::KillMineConfirm => {
                self.mode = BbsInputMode::Command;
                if line.trim().eq_ignore_ascii_case("Y") {
                    store.data.messages.retain(|m| !(m.to == self.user && m.read));
                    save_reply(store, "Read messages killed.")
                } else {
                    BbsReply::default()
                }
            }
            BbsInputMode::Command => self.handle_command(line, store, cfg),
        }
    }

    fn handle_command(&mut self, line: &str, store: &mut BbsStore, cfg: &BbsConfig) -> BbsReply {
        let mut out = Vec::new();
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        if parts.is_empty() {
            return BbsReply::default();
        }
        let cmd = parts[0].to_uppercase();
        let arg = parts.get(1).map(|s| s.to_uppercase());
        let user = self.user.clone();

        match cmd.as_str() {
            "B" => BbsReply { lines: out, hangup: true },
            "J" => {
                let mut heard: Vec<(&String, &String)> = store.heard.iter().collect();
                heard.sort_by(|a, b| b.1.cmp(a.1));
                for (call, t) in heard {
                    out.push(format!("{:<10} {}", call, t));
                }
                BbsReply { lines: out, hangup: false }
            }
            "L" => {
                if arg.is_none() {
                    list_messages_lines(&mut out, &store.data, cfg, &user, None, false, None, None);
                    BbsReply { lines: out, hangup: false }
                } else if (arg.as_deref() == Some("<") || arg.as_deref() == Some(">"))
                    && parts.len() > 2
                {
                    let dir = if arg.as_deref() == Some("<") { Some("FROM") } else { Some("TO") };
                    list_messages_lines(
                        &mut out, &store.data, cfg, &user, None, false,
                        Some(&parts[2].to_uppercase()), dir,
                    );
                    BbsReply { lines: out, hangup: false }
                } else {
                    // Python falls through to "Not a valid command" — a bare
                    // "L call" (no direction) or "L <" without a callsign is
                    // a silent no-op otherwise, which looks dead over RF.
                    BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false }
                }
            }
            "LL" => {
                // Python: LL requires a numeric arg — anything else is
                // "Not a valid command", not silence.
                match arg.and_then(|a| a.parse::<usize>().ok()) {
                    Some(a) => {
                        list_messages_lines(&mut out, &store.data, cfg, &user, Some(a), false, None, None);
                        BbsReply { lines: out, hangup: false }
                    }
                    None => BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false },
                }
            }
            "LM" => {
                list_messages_lines(&mut out, &store.data, cfg, &user, None, true, None, None);
                BbsReply { lines: out, hangup: false }
            }
            "R" => {
                // Python: R requires an arg — bare "R" is "Not a valid command".
                match arg {
                    Some(a) => {
                        let target = store.data.messages.iter().find(|m| m.id.to_string() == a);
                        if let Some(m) = target {
                            if can_view(m, &user, cfg) {
                                let path_line = if m.path.is_empty() {
                                    String::new()
                                } else {
                                    format!("PATH: {}\n\n", m.path.join(" "))
                                };
                                out.push(format!(
                                    "MSG#{} {} FROM {} TO {}\nSUBJECT: {}\n{}{}",
                                    m.id, m.date, m.from, m.to, m.subject, path_line, m.body
                                ));
                                if m.to == user {
                                    // find_mut pattern: re-lookup to mutate
                                    if let Some(mm) = store.data.messages.iter_mut().find(|m| m.id.to_string() == a) {
                                        mm.read = true;
                                    }
                                    if let Err(e) = store.save_data() {
                                        crate::commands::log_line(&format!(
                                            "Message board: read-mark save failed: {e}"
                                        ));
                                    }
                                }
                            }
                        }
                        BbsReply { lines: out, hangup: false }
                    }
                    None => BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false },
                }
            }
            "RM" => {
                let mut any = false;
                for m in store.data.messages.iter_mut() {
                    if m.to == user && !m.read {
                        let path_line = if m.path.is_empty() {
                            String::new()
                        } else {
                            format!("PATH: {}\n\n", m.path.join(" "))
                        };
                        out.push(format!(
                            "MSG#{} {} FROM {} TO {}\nSUBJECT: {}\n{}{}",
                            m.id, m.date, m.from, m.to, m.subject, path_line, m.body
                        ));
                        m.read = true;
                        any = true;
                    }
                }
                if any {
                    if let Err(e) = store.save_data() {
                        crate::commands::log_line(&format!(
                            "Message board: read-all save failed: {e}"
                        ));
                    }
                }
                BbsReply { lines: out, hangup: false }
            }
            "S" | "SB" | "SP" => {
                // Python: dest = arg or "ALL"; then subject line is requested.
                // The dest is consumed at the SUBJECT stage: we stash it and
                // ask for the subject line.
                let kind = if cmd == "SB" { 'B' } else { 'P' };
                let dest = arg.unwrap_or_else(|| "ALL".to_string());
                self.pending_dest = dest;
                self.mode = BbsInputMode::Subject { kind };
                BbsReply { lines: vec!["SUBJECT: ".to_string()], hangup: false }
            }
            "K" => {
                // Python: K requires an arg — bare "K" is "Not a valid command".
                match arg {
                    Some(a) => {
                        if let Some(m) = store.data.messages.iter().find(|m| m.id.to_string() == a) {
                            if m.from == user || m.to == user || is_sysop(&user, cfg) {
                                if let Some(id) = a.parse::<u32>().ok() {
                                    self.mode = BbsInputMode::DeleteConfirm { id };
                                    out.push(format!("Delete message {}? (y/N): ", id));
                                }
                            }
                        }
                        BbsReply { lines: out, hangup: false }
                    }
                    None => BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false },
                }
            }
            "KM" => {
                self.mode = BbsInputMode::KillMineConfirm;
                BbsReply { lines: vec!["Delete ALL your read messages? (y/N): ".to_string()], hangup: false }
            }
            "P" => {
                // Python: P requires sysop AND an arg — anything else is
                // "Not a valid command".
                if !is_sysop(&user, cfg) {
                    return BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false };
                }
                match arg {
                    Some(a) => {
                        if let Some(m) = store.data.messages.iter_mut().find(|m| m.id.to_string() == a) {
                            m.pinned = !m.pinned;
                            if let Err(e) = store.save_data() {
                                crate::commands::log_line(&format!(
                                    "Message board: pin save failed: {e}"
                                ));
                            }
                            out.push("Toggled Pin.".to_string());
                        }
                        BbsReply { lines: out, hangup: false }
                    }
                    None => BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false },
                }
            }
            "H" | "HELP" => {
                out.push(format!("--- {} K2K Packet Message Board Help ---", cfg.node_name));
                out.push("B(ye)         K2K PACKET MESSAGE BOARD WILL DISCONNECT".to_string());
                out.push("J(heard)      CALLSIGNS WITH DAYSTAMP".to_string());
                out.push("K(ill) n      DELETE MESSAGE NUMBER n".to_string());
                out.push("KM(ine)       DELETE ALL READ MESSAGES ADDRESSED TO YOU".to_string());
                out.push("L             LIST ALL ACCESSIBLE MESSAGES".to_string());
                out.push("L < call      LIST MESSAGES FROM CALL".to_string());
                out.push("L > call      LIST MESSAGES TO CALL".to_string());
                out.push("LL n          LIST LAST n MESSAGES".to_string());
                out.push("LM(ine)       LIST UNREAD MESSAGES ADDRESSED TO YOU".to_string());
                out.push("R(ead) n      DISPLAY MESSAGE NUMBER n".to_string());
                out.push("RM(ine)       READ ALL MESSAGES ADDRESSED TO YOU".to_string());
                out.push("S(end) call   SEND PRIVATE MESSAGE (LEAVE BLANK FOR ALL)".to_string());
                out.push("SB call       SEND BULLETIN".to_string());
                if is_sysop(&user, cfg) {
                    out.push(String::new());
                    out.push("SYSOP COMMANDS:".to_string());
                    out.push("P n           PIN MESSAGE n (Forces to top of list with '!')".to_string());
                }
                BbsReply { lines: out, hangup: false }
            }
            _ => BbsReply { lines: vec!["Not a valid command".to_string()], hangup: false },
        }
    }
}

// Per-session fields that don't serialize — pending multi-step input.
impl BbsSession {
    /// Destination captured at the S/SB/SP line, consumed at SUBJECT time.
    pub fn take_pending_dest(&mut self) -> String {
        std::mem::take(&mut self.pending_dest)
    }
}

// ── Shared helpers (Python module-level functions) ────────

fn is_sysop(call: &str, cfg: &BbsConfig) -> bool {
    cfg.sysops.iter().any(|s| s == call)
}

fn can_view(msg: &BbsMessage, user: &str, cfg: &BbsConfig) -> bool {
    if is_sysop(user, cfg) || !cfg.privacy_enabled {
        return true;
    }
    if cfg.public_aliases.iter().any(|a| a == &msg.to) {
        return true;
    }
    if user == "GUEST" {
        return false;
    }
    msg.to == user || msg.from == user
}

/// Python list_messages — appends the header, rows, and terminator to `out`.
#[allow(clippy::too_many_arguments)]
fn list_messages_lines(
    out: &mut Vec<String>,
    data: &BbsData,
    cfg: &BbsConfig,
    user: &str,
    limit: Option<usize>,
    mine_only: bool,
    call_filter: Option<&str>,
    dir_filter: Option<&str>,
) {
    let mut msgs: Vec<&BbsMessage> = data
        .messages
        .iter()
        .filter(|m| can_view(m, user, cfg) && !m.date.is_empty())
        .collect();
    if mine_only {
        msgs.retain(|m| m.to == user && !m.read);
    } else if let Some(filter) = call_filter {
        msgs.retain(|m| {
            if dir_filter == Some("FROM") {
                m.from == filter
            } else {
                m.to == filter
            }
        });
    }
    // Sort FIRST (newest on top), then limit — the old order sliced by
    // insertion order, so "LL n" was wrong when dates weren't in id order
    // and a recently-pinned message inserted early got dropped.
    msgs.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
    msgs.sort_by_key(|m| !m.pinned); // stable: pinned (false) rises to the top
    if let Some(limit) = limit {
        if limit < msgs.len() {
            msgs.truncate(limit); // newest n by date
        }
    }

    if msgs.is_empty() {
        out.push("*** No messages ***".to_string());
        out.push("end of messages".to_string());
        return;
    }
    out.push(format!(
        "{:<5} {:<2} {:<6} {:<8} {:<8} {:<18} {}",
        "MSG#", "ST", "SIZE", "TO", "FROM", "DATE", "SUBJECT"
    ));
    for m in msgs {
        let mut st = if m.pinned { "!".to_string() } else { m.st.clone() };
        if m.to == user && !m.read {
            st.push('N');
        }
        let to8: String = m.to.chars().take(8).collect();
        let from8: String = m.from.chars().take(8).collect();
        let date18: String = m.date.chars().take(18).collect();
        let subj26: String = m.subject.chars().take(26).collect();
        out.push(format!(
            "{:<5} {:<2} {:<6} {:<8} {:<8} {:<18} {}",
            m.id, st, m.body.len(), to8, from8, date18, subj26
        ));
    }
    // No trailing blank line — pack_bbs_lines keeps empty elements, so an
    // empty line here would show as a gap in the connected tab (and on RF).
    out.push("end of messages".to_string());
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BbsConfig {
        BbsConfig {
            node_name: "KK6ABC-1".to_string(),
            sysops: vec!["KK6SEN".to_string()],
            privacy_enabled: true,
            public_aliases: vec!["ALL".to_string(), "TECH".to_string()],
        }
    }

    fn tmp_store(name: &str) -> BbsStore {
        let dir = std::env::temp_dir().join(format!("k2k_bbs_test_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        BbsStore::load(&dir)
    }

    fn seed(store: &mut BbsStore) {
        store.data.messages.push(BbsMessage {
            id: 1,
            st: "P".into(),
            to: "KK6SEN".into(),
            from: "W6ABC".into(),
            date: "08/01/26 12:00:00".into(),
            subject: "Hello".into(),
            body: "Line one\nLine two".into(),
            read: false,
            pinned: false,
            path: vec![],
        });
        store.data.messages.push(BbsMessage {
            id: 2,
            st: "B".into(),
            to: "ALL".into(),
            from: "N7XYZ".into(),
            date: "08/02/26 09:30:00".into(),
            subject: "Net tonight".into(),
            body: "Join us".into(),
            read: false,
            pinned: false,
            path: vec![],
        });
        store.data.next_id = 3;
    }

    #[test]
    fn connect_banner_shows_summary_not_list() {
        let mut store = tmp_store("banner");
        seed(&mut store);
        let sess = BbsSession::new("KK6SEN");
        let out = sess.connect_output(&store, &test_config());
        assert_eq!(
            out[0],
            "*** Welcome KK6SEN to the KK6ABC-1 K2K Packet Message Board ***"
        );
        assert!(
            !out.iter().any(|l| l.contains("AVAILABLE")),
            "disk-usage line must be gone"
        );
        // Summary line with dynamic count + lowest..highest id — the full
        // list is NOT sent on connect (it loads on demand with L).
        assert_eq!(out[1], "THERE ARE 2 MESSAGES NUMBERED 1-2");
        assert!(
            !out.iter().any(|l| l.starts_with("MSG#")),
            "no message list on connect: {out:?}"
        );
        assert!(
            !out.iter().any(|l| l == "end of messages"),
            "no end-of-messages on connect: {out:?}"
        );
        assert_eq!(out.len(), 2, "banner + summary only");
    }

    #[test]
    fn empty_board_says_zero_messages() {
        let store = tmp_store("empty");
        let sess = BbsSession::new("W6ABC");
        let out = sess.connect_output(&store, &test_config());
        assert_eq!(out[1], "THERE ARE 0 MESSAGES");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn send_message_flow() {
        let mut store = tmp_store("send");
        let mut sess = BbsSession::new("KK6SEN");
        // S → SUBJECT prompt
        let r = sess.handle_line("S W6ABC", &mut store, &test_config());
        assert_eq!(r.lines, vec!["SUBJECT: ".to_string()]);
        assert!(!r.hangup);
        // subject → ENTER MESSAGE 1
        let r = sess.handle_line("Test Subject", &mut store, &test_config());
        assert_eq!(r.lines, vec!["\nENTER MESSAGE 1--END WITH /EX".to_string()]);
        // body lines accumulate silently
        let r = sess.handle_line("Body line one", &mut store, &test_config());
        assert!(r.lines.is_empty());
        let r = sess.handle_line("Body line two", &mut store, &test_config());
        assert!(r.lines.is_empty());
        // /EX saves
        let r = sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(r.lines, vec!["MESSAGE SAVED to # 1".to_string()]);
        assert_eq!(store.data.messages.len(), 1);
        let m = &store.data.messages[0];
        assert_eq!(m.id, 1);
        assert_eq!(m.to, "W6ABC");
        assert_eq!(m.from, "KK6SEN");
        assert_eq!(m.subject, "Test Subject");
        assert_eq!(m.body, "Body line one\nBody line two");
        assert_eq!(store.data.next_id, 2);
        // data file exists on disk
        assert!(store.data_path.exists());
    }

    /// Admin-console contract (the all-messages-#1 bug): the store is held
    /// across pbbs_admin_send calls, so a SECOND message composed in the
    /// SAME session/store must get id 2 — never reuse id 1 — and next_id
    /// must advance to 3. Regression: the admin console reloaded the store
    /// from disk between calls, discarding the subject-line next_id
    /// increment so every message landed as id 1.
    #[test]
    fn sequential_messages_increment_ids_never_reuse() {
        let mut store = tmp_store("idseq");
        let mut sess = BbsSession::new("KK6SEN");

        // Message 1
        sess.handle_line("S K6ABC", &mut store, &test_config());
        sess.handle_line("First", &mut store, &test_config());
        let r1 = sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(r1.lines, vec!["MESSAGE SAVED to # 1".to_string()]);

        // Message 2 — same session, same held store (as the admin console
        // does after the fix)
        let r2 = sess.handle_line("S W7XYZ", &mut store, &test_config());
        assert_eq!(r2.lines, vec!["SUBJECT: ".to_string()]);
        sess.handle_line("Second", &mut store, &test_config());
        let r3 = sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(r3.lines, vec!["MESSAGE SAVED to # 2".to_string()]);

        // And a third — ids must be unique and strictly increasing.
        sess.handle_line("S N0CALL", &mut store, &test_config());
        sess.handle_line("Third", &mut store, &test_config());
        let r4 = sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(r4.lines, vec!["MESSAGE SAVED to # 3".to_string()]);

        let ids: Vec<u32> = store.data.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![1, 2, 3], "message ids must increment, never reuse");
        assert_eq!(store.data.next_id, 4);
    }

    #[test]
    fn send_with_no_subject_and_no_dest() {
        let mut store = tmp_store("sendnaked");
        let mut sess = BbsSession::new("W6ABC");
        sess.handle_line("S", &mut store, &test_config());
        let r = sess.handle_line("", &mut store, &test_config());
        assert_eq!(r.lines, vec!["\nENTER MESSAGE 1--END WITH /EX".to_string()]);
        sess.handle_line("hi", &mut store, &test_config());
        let r = sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(r.lines, vec!["MESSAGE SAVED to # 1".to_string()]);
        let m = &store.data.messages[0];
        assert_eq!(m.to, "ALL");
        assert_eq!(m.subject, "(No Subject)");
        assert_eq!(m.st, "P");
    }

    #[test]
    fn message_records_writer_path_and_shows_on_read() {
        let mut store = tmp_store("path");
        let mut sess = BbsSession::new("KK6SEN");
        sess.path = vec!["KBANN".to_string(), "KJOHN".to_string()];
        sess.handle_line("S W6ABC", &mut store, &test_config());
        sess.handle_line("Test Subject", &mut store, &test_config());
        sess.handle_line("Body line", &mut store, &test_config());
        let r = sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(r.lines, vec!["MESSAGE SAVED to # 1".to_string()]);
        assert_eq!(store.data.messages[0].path, vec!["KBANN", "KJOHN"]);
        let r = sess.handle_line("R 1", &mut store, &test_config());
        assert!(r
            .lines
            .join("\n")
            .contains("SUBJECT: Test Subject\nPATH: KBANN KJOHN\n\nBody line"));
        // No path → no PATH line.
        let mut sess2 = BbsSession::new("W6ABC");
        sess2.handle_line("S", &mut store, &test_config());
        sess2.handle_line("NoPath", &mut store, &test_config());
        sess2.handle_line("hi", &mut store, &test_config());
        sess2.handle_line("/EX", &mut store, &test_config());
        let r2 = sess2.handle_line("R 2", &mut store, &test_config());
        assert!(!r2.lines.join("\n").contains("PATH:"));
    }

    #[test]
    fn bulletin_sets_st_b() {
        let mut store = tmp_store("sb");
        let mut sess = BbsSession::new("W6ABC");
        sess.handle_line("SB", &mut store, &test_config());
        sess.handle_line("Subj", &mut store, &test_config());
        sess.handle_line("/EX", &mut store, &test_config());
        assert_eq!(store.data.messages[0].st, "B");
    }

    #[test]
    fn read_marks_read_for_addressee_only() {
        let mut store = tmp_store("read");
        seed(&mut store);
        // W6ABC reading their own SENT message (to != user) must not mark read
        let mut sess2 = BbsSession::new("W6ABC");
        let r = sess2.handle_line("R 1", &mut store, &test_config());
        assert!(r.lines[0].starts_with("MSG#1 08/01/26 12:00:00 FROM W6ABC TO KK6SEN"));
        assert!(!store.data.messages[0].read, "reading own sent message must not mark read");
        // The addressee reading marks it read
        let mut sess = BbsSession::new("KK6SEN");
        sess.handle_line("R 1", &mut store, &test_config());
        assert!(store.data.messages[0].read);
    }

    #[test]
    fn privacy_blocks_other_users_mail() {
        let mut store = tmp_store("privacy");
        seed(&mut store);
        // K2ABC (not sysop, not sender/addressee) cannot see KK6SEN's private mail
        let mut sess = BbsSession::new("K2ABC");
        let r = sess.handle_line("L", &mut store, &test_config());
        let joined = r.lines.join("\n");
        assert!(!joined.contains("Hello"), "private mail leaked: {joined}");
        assert!(joined.contains("Net tonight"), "public bulletin missing: {joined}");
        // sysop sees everything
        let mut sysop = BbsSession::new("KK6SEN");
        let r = sysop.handle_line("L", &mut store, &test_config());
        assert!(r.lines.join("\n").contains("Hello"));
        // GUEST gets nothing private
        let mut guest = BbsSession::new("GUEST");
        let r = guest.handle_line("L", &mut store, &test_config());
        assert!(!r.lines.join("\n").contains("Hello"));
    }

    #[test]
    fn delete_requires_confirmation_and_permission() {
        let mut store = tmp_store("del");
        seed(&mut store);
        // Third party (neither from/to, not sysop) gets NO response — Python prints nothing
        let mut sess = BbsSession::new("K2ABC");
        let r = sess.handle_line("K 1", &mut store, &test_config());
        assert!(r.lines.is_empty());
        assert_eq!(store.data.messages.len(), 2);
        // Owner of the message can delete it with Y
        let mut sess = BbsSession::new("KK6SEN");
        let r = sess.handle_line("K 1", &mut store, &test_config());
        assert_eq!(r.lines, vec!["Delete message 1? (y/N): ".to_string()]);
        let r = sess.handle_line("Y", &mut store, &test_config());
        assert_eq!(r.lines, vec!["MESSAGE DELETED".to_string()]);
        assert_eq!(store.data.messages.len(), 1);
        // N keeps it
        let r = sess.handle_line("K 2", &mut store, &test_config());
        assert_eq!(r.lines, vec!["Delete message 2? (y/N): ".to_string()]);
        sess.handle_line("n", &mut store, &test_config());
        assert_eq!(store.data.messages.len(), 1);
    }

    #[test]
    fn pin_toggle_sysop_only() {
        let mut store = tmp_store("pin");
        seed(&mut store);
        let mut non = BbsSession::new("W6ABC");
        non.handle_line("P 1", &mut store, &test_config());
        assert!(!store.data.messages[0].pinned);
        let mut sys = BbsSession::new("KK6SEN");
        let r = sys.handle_line("P 1", &mut store, &test_config());
        assert_eq!(r.lines, vec!["Toggled Pin.".to_string()]);
        assert!(store.data.messages[0].pinned);
    }

    #[test]
    fn list_filters_and_limit() {
        let mut store = tmp_store("list");
        seed(&mut store);
        let mut sess = BbsSession::new("KK6SEN");
        // L > call (to filter)
        let r = sess.handle_line("L > KK6SEN", &mut store, &test_config());
        assert!(r.lines.join("\n").contains("Hello"));
        assert!(!r.lines.join("\n").contains("Net tonight"));
        // L < call (from filter)
        let r = sess.handle_line("L < N7XYZ", &mut store, &test_config());
        assert!(r.lines.join("\n").contains("Net tonight"));
        // LL 1 → only newest
        let r = sess.handle_line("LL 1", &mut store, &test_config());
        let joined = r.lines.join("\n");
        assert!(joined.contains("Net tonight"));
        assert!(!joined.contains("Hello"));
        // LM → unread mine
        let r = sess.handle_line("LM", &mut store, &test_config());
        assert!(r.lines.join("\n").contains("Hello"));
    }

    #[test]
    fn bye_hangs_up() {
        let mut store = tmp_store("bye");
        let mut sess = BbsSession::new("W6ABC");
        let r = sess.handle_line("B", &mut store, &test_config());
        assert!(r.hangup);
        assert!(r.lines.is_empty());
    }

    #[test]
    fn invalid_command_message() {
        let mut store = tmp_store("badcmd");
        let mut sess = BbsSession::new("W6ABC");
        let r = sess.handle_line("XYZ", &mut store, &test_config());
        assert_eq!(r.lines, vec!["Not a valid command".to_string()]);
        // empty line in command mode → silent
        let r = sess.handle_line("", &mut store, &test_config());
        assert!(r.lines.is_empty());
    }

    #[test]
    fn help_shows_sysop_commands_for_sysop() {
        let mut store = tmp_store("help");
        let mut sess = BbsSession::new("KK6SEN");
        let r = sess.handle_line("H", &mut store, &test_config());
        let joined = r.lines.join("\n");
        assert!(joined.contains("--- KK6ABC-1 K2K Packet Message Board Help ---"));
        assert!(joined.contains("SYSOP COMMANDS:"));
        assert!(joined.contains("P n"));
        let mut non = BbsSession::new("W6ABC");
        let r = non.handle_line("HELP", &mut store, &test_config());
        assert!(!r.lines.join("\n").contains("SYSOP COMMANDS:"));
    }

    #[test]
    fn heard_recorded_on_connect() {
        let mut store = tmp_store("heard");
        store.record_heard("KK6SEN");
        assert!(store.heard.contains_key("KK6SEN"));
        assert!(store.heard_path.exists());
        let mut sess = BbsSession::new("KK6SEN");
        let r = sess.handle_line("J", &mut store, &test_config());
        assert!(r.lines[0].starts_with("KK6SEN    "));
    }

    #[test]
    fn prompt_shown_for_user_and_sysop() {
        let cfg = test_config(); // sysops: [KK6SEN]
        let sess = BbsSession::new("KK6SEN");
        assert_eq!(sess.prompt(&cfg), "ENTER COMMAND:  B,J,K,L,R,S,P, or Help >");
        let guest = BbsSession::new("W6ABC");
        assert_eq!(guest.prompt(&cfg), "ENTER COMMAND:  B,J,K,L,R,S, or Help >");
    }

    #[test]
    fn config_node_name_derived_from_board_callsign() {
        // Node name = board callsign + ssid (the Node Name setting is gone).
        let mut s = crate::settings::AppSettings::default();
        s.message_board_callsign = "KK6SEN".to_string();
        s.message_board_ssid = 1;
        let cfg = BbsConfig::from_app_settings(&s);
        assert_eq!(cfg.node_name, "KK6SEN-1");
        // SSID 0 renders without the suffix (AX.25 display convention)
        s.message_board_ssid = 0;
        let cfg = BbsConfig::from_app_settings(&s);
        assert_eq!(cfg.node_name, "KK6SEN");
    }

    #[test]
    fn prompt_only_at_command_menu() {
        let mut store = tmp_store("prompt");
        let cfg = test_config();
        let mut sess = BbsSession::new("KK6SEN");
        // At connect: at the menu (banner + prompt are sent together).
        assert!(sess.at_command());
        // S → Subject mode: NOT at the menu — no command line while writing.
        sess.handle_line("S", &mut store, &cfg);
        assert!(!sess.at_command());
        // subject → Body mode: still not at the menu.
        sess.handle_line("Hello net", &mut store, &cfg);
        assert!(!sess.at_command());
        // body line → still Body (empty reply, and no prompt).
        sess.handle_line("Net tonight at 8", &mut store, &cfg);
        assert!(!sess.at_command());
        // /EX → saved → back at the menu (prompt OK after "message saved").
        let r = sess.handle_line("/EX", &mut store, &cfg);
        assert!(r.lines.iter().any(|l| l.contains("MESSAGE SAVED")));
        assert!(sess.at_command());
        // K n → DeleteConfirm: not at the menu (no prompt while confirming).
        sess.handle_line("K 1", &mut store, &cfg);
        assert!(!sess.at_command());
        // Y → deleted → back at the menu.
        sess.handle_line("Y", &mut store, &cfg);
        assert!(sess.at_command());
        // Plain commands stay at the menu.
        sess.handle_line("L", &mut store, &cfg);
        assert!(sess.at_command());
    }

    #[test]
    fn store_save_load_roundtrip_and_bak_recovery() {
        let dir = std::env::temp_dir().join(format!("k2k_bbs_test_roundtrip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Save twice — the .bak is written on the second save (Python parity)
        {
            let mut store = BbsStore::load(&dir);
            seed(&mut store);
            store.save_data().unwrap();
            store.save_heard().unwrap();
            store.save_data().unwrap(); // second save creates pbbs_data.json.bak
        }
        // Load back
        let store = BbsStore::load(&dir);
        assert_eq!(store.data.messages.len(), 2);
        assert_eq!(store.data.next_id, 3);
        // Corrupt the main file → .bak recovery restores it
        let data_path = dir.join("pbbs_data.json");
        std::fs::write(&data_path, "{corrupted!!!").unwrap();
        let store = BbsStore::load(&dir);
        assert_eq!(store.data.messages.len(), 2, ".bak recovery failed");
        // The restore must NOT have clobbered the .bak with the corrupted
        // file (save_json's bak-copy-first used to overwrite the good copy).
        // Corrupt again and recover again — the old code lost everything here.
        std::fs::write(&data_path, "also bad!!!").unwrap();
        let store = BbsStore::load(&dir);
        assert_eq!(
            store.data.messages.len(),
            2,
            ".bak was destroyed by the first recovery"
        );
    }
}
