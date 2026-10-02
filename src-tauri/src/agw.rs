// ── AGWPE (AGW Packet Engine) Protocol Layer ───────────────
//
// This module implements the client side of the AGWPE binary TCP/IP
// protocol for AX.25 packet radio.  AGWPE is a full AX.25 Layer 2
// manager that exposes a TCP/IP API.  Unlike KISS (which passes raw
// HDLC frames), AGWPE handles the entire AX.25 stack — SABM/DISC,
// sequence numbers, retransmissions, T1/T3 timers — and communicates
// with the host using a message-based binary frame protocol.
//
// # Frame format (36-byte header + optional data payload)
//
// | Offset | Size | Field      | Description                                |
// |--------|------|------------|--------------------------------------------|
// |  0–3   |  4   | Port       | `int32` LE (0 = Port1, 1 = Port2, …)      |
// |  4     |  1   | DataKind   | ASCII character (e.g. `'C'`, `'D'`, `'X'`)|
// |  5     |  1   | Reserved   | 0x00                                       |
// |  6     |  1   | PID        | AX.25 PID (0xF0 standard, 0x00 unused)    |
// |  7     |  1   | Reserved   | 0x00                                       |
// |  8–17  | 10   | CallFrom   | Null-terminated ASCII callsign[+SSID]      |
// | 18–27  | 10   | CallTo     | Null-terminated ASCII callsign[+SSID]      |
// | 28–31  |  4   | DataLen    | `uint32` LE — bytes following the header   |
// | 32–35  |  4   | User       | Reserved, set to 0                         |
//
// All multi-byte integers are **little-endian**.
//
// Reference: Official SV2AGW developer kit (develop.zip).

use serde::{Deserialize, Serialize};

// ── Protocol Constants ─────────────────────────────────────

/// Size of a fixed AGWPE frame header in bytes (36).
pub const AGW_HEADER_SIZE: usize = 36;

/// Size of a padded callsign field in the AGWPE header (10 bytes).
pub const CALLSIGN_FIELD_SIZE: usize = 10;

// DataKind values — Application → AGWPE (commands)

/// Register a callsign+SSID with the TNC.
pub const CMD_REGISTER: u8 = b'X';
/// Initiate an AX.25 connected-mode connection.
pub const CMD_CONNECT: u8 = b'C';
/// Send data over an established AX.25 connection (I-frame).
pub const CMD_DATA: u8 = b'D';
/// Terminate an AX.25 connected-mode connection.
pub const CMD_DISCONNECT: u8 = b'd';
/// Connect through one or more digipeaters.
pub const CMD_CONNECT_VIA: u8 = b'v';
/// Send an unnumbered information (UI) frame.
pub const CMD_UNPROTO: u8 = b'M';
/// Send an unnumbered information (UI) frame via digipeaters.
pub const CMD_UNPROTO_VIA: u8 = b'V';
/// Toggle monitoring of channel activity on/off.
pub const CMD_MONITOR_TOGGLE: u8 = b'm';

// DataKind values — AGWPE → Application (responses / events)

/// Incoming connection established notification.
pub const RSP_CONNECTED: u8 = b'C';
/// Incoming connected-mode data (I-frame) received.
pub const RSP_DATA: u8 = b'D';
/// Connection terminated notification.
pub const RSP_DISCONNECTED: u8 = b'd';
/// Registration success / failure result.
pub const RSP_REGISTER_RESULT: u8 = b'X';
/// Unnumbered information (UI) monitoring frame received.
pub const RSP_UNPROTO_MON: u8 = b'U';
/// I-frame (information) monitoring frame received.
pub const RSP_INFO_MON: u8 = b'I';
/// S-frame (supervisory) monitoring frame received.
pub const RSP_SUPER_MON: u8 = b'S';
/// Echo of the station's own transmission.
pub const RSP_OWN_TX: u8 = b'T';
/// Error or retry-out notification.
pub const RSP_ERROR: u8 = b'E';

// ── Data Structures ───────────────────────────────────────

/// A parsed AGWPE frame, consisting of a 36-byte header and an
/// optional data payload.
///
/// Fields correspond one-to-one with the AGWPE binary header layout
/// (see [the module-level documentation](index.html) for the wire format).
#[derive(Debug, Clone)]
pub struct AgwFrame {
    /// DataKind byte identifying the frame type.
    pub kind: u8,
    /// AX.25 protocol ID (e.g. `0xF0` for standard layer-3).
    pub pid: u8,
    /// Source callsign (with optional `-SSID` suffix).
    pub call_from: String,
    /// Destination callsign (with optional `-SSID` suffix).
    pub call_to: String,
    /// Number of payload bytes declared in the header.
    pub data_len: u32,
    /// Raw payload bytes following the header.
    pub data: Vec<u8>,
}

/// Tab-mapping entry that associates an AGWPE connection
/// (identified by `call_from` + `call_to`) with a UI tab index.
///
/// Used by the Tauri frontend to route incoming frames to the
/// correct conversation tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabMapping {
    /// Index of the UI tab this connection is bound to.
    pub tab: usize,
    /// Local callsign (with optional `-SSID`).
    pub call_from: String,
    /// Remote callsign (with optional `-SSID`).
    pub call_to: String,
    /// Remote base callsign (without SSID).
    pub remote: String,
    /// Remote SSID (0–15).
    pub remote_ssid: u8,
}

// ── Frame Building ────────────────────────────────────────

/// Format a callsign for an AGWPE header field: `"CALL"` or `"CALL-SSID"`.
/// `build_header` pads the result to 10 bytes with nulls.
fn fmt_header_callsign(call: &str, ssid: u8) -> String {
    if ssid == 0 {
        call.to_string()
    } else {
        format!("{}-{}", call, ssid)
    }
}

/// Build an AGWPE digipeater VIA payload: count byte + 10-byte padded callsigns.
fn build_via_payload(digis: &[String]) -> Vec<u8> {
    let mut via_data = vec![digis.len() as u8];
    for digi in digis {
        let digi_bytes = digi.as_bytes();
        for i in 0..CALLSIGN_FIELD_SIZE {
            via_data.push(if i < digi_bytes.len() { digi_bytes[i] } else { 0x00 });
        }
    }
    via_data
}

/// Extract a callsign from a 10-byte AGWPE header field.
///
/// Reads bytes up to (but not including) the first null byte or
/// space character, then converts the result to a `String` via
/// [`String::from_utf8_lossy`].
fn parse_callsign_field(field: &[u8]) -> String {
    let trimmed = field
        .iter()
        .take_while(|&&b| b != 0 && b != b' ')
        .copied()
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&trimmed).to_string()
}

/// Build a 36-byte AGWPE frame header.
///
/// Writes every header field in little-endian byte order:
///
/// * `port` — TNC port (written as a 4-byte LE `int32`)
/// * `kind` — DataKind byte placed at offset 4
/// * `pid`  — AX.25 PID byte at offset 6
/// * `call_from`, `call_to` — padded to 10 bytes each at offsets 8 and 18
/// * `data_len` — payload length written as a 4-byte LE `uint32` at offset 28
///
/// The reserved bytes at offsets 5, 7, and 32–35 are set to zero.
pub fn build_header(
    port: u8,
    kind: u8,
    pid: u8,
    call_from: &str,
    call_to: &str,
    data_len: u32,
) -> Vec<u8> {
    let mut header = Vec::with_capacity(AGW_HEADER_SIZE);

    // Port (int32 LE)
    header.push(port);
    header.push(0x00);
    header.push(0x00);
    header.push(0x00);

    // DataKind
    header.push(kind);
    // Reserved
    header.push(0x00);
    // PID
    header.push(pid);
    // Reserved
    header.push(0x00);

    // CallFrom
    let call_from_bytes = call_from.as_bytes();
    for i in 0..CALLSIGN_FIELD_SIZE {
        if i < call_from_bytes.len() {
            header.push(call_from_bytes[i]);
        } else {
            header.push(0x00);
        }
    }

    // CallTo
    let call_to_bytes = call_to.as_bytes();
    for i in 0..CALLSIGN_FIELD_SIZE {
        if i < call_to_bytes.len() {
            header.push(call_to_bytes[i]);
        } else {
            header.push(0x00);
        }
    }

    // DataLen (uint32 LE)
    header.push((data_len & 0xFF) as u8);
    header.push(((data_len >> 8) & 0xFF) as u8);
    header.push(((data_len >> 16) & 0xFF) as u8);
    header.push(((data_len >> 24) & 0xFF) as u8);

    // User (reserved)
    header.push(0x00);
    header.push(0x00);
    header.push(0x00);
    header.push(0x00);

    header
}

/// Build a complete AGWPE frame (36-byte header + data payload).
///
/// Convenience wrapper around [`build_header`] that appends the
/// raw payload bytes and returns the full frame.
pub fn build_frame(
    port: u8,
    kind: u8,
    pid: u8,
    call_from: &str,
    call_to: &str,
    data: &[u8],
) -> Vec<u8> {
    let mut frame = build_header(port, kind, pid, call_from, call_to, data.len() as u32);
    frame.extend_from_slice(data);
    frame
}

/// Build a Register Callsign (`'X'`) frame — header only, no payload.
///
/// Registers a local callsign and optional SSID with the AGWPE
/// engine so it can be used for outgoing connections.
pub fn build_register(callsign: &str, ssid: u8) -> Vec<u8> {
    build_header(0, CMD_REGISTER, 0x00, &fmt_header_callsign(callsign, ssid), "", 0)
}

/// Build a Connect (`'C'`) frame to initiate an AX.25 connection.
pub fn build_connect(port: u8, my_call: &str, my_ssid: u8, dest: &str, dest_ssid: u8) -> Vec<u8> {
    build_header(port, CMD_CONNECT, 0xF0, &fmt_header_callsign(my_call, my_ssid), &fmt_header_callsign(dest, dest_ssid), 0)
}

/// Build a Connect-Via (`'v'`) frame — connect through digipeaters.
///
/// The digipeater path is encoded in the payload as a count byte
/// followed by a sequence of 10-byte, null-terminated callsign fields.
pub fn build_connect_via(
    port: u8,
    my_call: &str,
    my_ssid: u8,
    dest: &str,
    dest_ssid: u8,
    digis: &[String],
) -> Vec<u8> {
    build_frame(
        port, CMD_CONNECT_VIA, 0xF0,
        &fmt_header_callsign(my_call, my_ssid), &fmt_header_callsign(dest, dest_ssid),
        &build_via_payload(digis),
    )
}

/// Build a Send Unproto Via (`'V'`) frame — send a UI frame through
/// digipeaters.
///
/// The payload contains the digipeater path (count + 10-byte callsign
/// fields) followed by the raw UI-frame data.
pub fn build_unproto_via(
    port: u8,
    my_call: &str,
    my_ssid: u8,
    dest: &str,
    dest_ssid: u8,
    digis: &[String],
    data: &[u8],
) -> Vec<u8> {
    let mut via_data = build_via_payload(digis);
    // Append the payload data after the via routing
    via_data.extend_from_slice(data);

    build_frame(
        port, CMD_UNPROTO_VIA, 0xF0,
        &fmt_header_callsign(my_call, my_ssid), &fmt_header_callsign(dest, dest_ssid),
        &via_data,
    )
}

/// Build a Send Connected Data (`'D'`) frame to send an I-frame over
/// an established AX.25 connection.
pub fn build_data(
    port: u8,
    my_call: &str,
    my_ssid: u8,
    dest: &str,
    dest_ssid: u8,
    data: &[u8],
) -> Vec<u8> {
    build_frame(port, CMD_DATA, 0xF0, &fmt_header_callsign(my_call, my_ssid), &fmt_header_callsign(dest, dest_ssid), data)
}

/// Build a Disconnect (`'d'`) frame to terminate an AX.25 connection.
pub fn build_disconnect(
    port: u8,
    my_call: &str,
    my_ssid: u8,
    dest: &str,
    dest_ssid: u8,
) -> Vec<u8> {
    build_header(port, CMD_DISCONNECT, 0xF0, &fmt_header_callsign(my_call, my_ssid), &fmt_header_callsign(dest, dest_ssid), 0)
}

/// Build a Send Unproto (`'M'`) frame to send a UI frame (beacon /
/// broadcast / unconnected message).
pub fn build_unproto(
    port: u8,
    my_call: &str,
    my_ssid: u8,
    dest: &str,
    dest_ssid: u8,
    data: &[u8],
) -> Vec<u8> {
    build_frame(port, CMD_UNPROTO, 0xF0, &fmt_header_callsign(my_call, my_ssid), &fmt_header_callsign(dest, dest_ssid), data)
}

/// Build a Toggle Monitoring (`'m'`) frame — header only, no payload.
///
/// Sending this frame to the AGWPE engine toggles channel-monitor
/// mode on or off.
pub fn build_monitor_toggle() -> Vec<u8> {
    build_header(0, CMD_MONITOR_TOGGLE, 0x00, "", "", 0)
}

// ── Frame Parsing ──────────────────────────────────────────

/// Parse an AGWPE frame from raw bytes.
///
/// Reads the 36-byte header and up to `DataLen` payload bytes.
/// Returns `None` if fewer than [`AGW_HEADER_SIZE`] bytes are
/// available.
pub fn parse_frame(data: &[u8]) -> Option<AgwFrame> {
    if data.len() < AGW_HEADER_SIZE {
        return None;
    }

    let kind = data[4];
    let pid = data[6];

    // Extract CallFrom (10 bytes starting at offset 8)
    let call_from = parse_callsign_field(&data[8..18]);

    // Extract CallTo (10 bytes starting at offset 18)
    let call_to = parse_callsign_field(&data[18..28]);

    // DataLen (uint32 LE at offset 28)
    let data_len = u32::from_le_bytes([
        data[28],
        data[29],
        data[30],
        data[31],
    ]);

    // Extract data payload
    let total_needed = AGW_HEADER_SIZE + data_len as usize;
    let payload = if data.len() >= total_needed && data_len > 0 {
        data[AGW_HEADER_SIZE..total_needed].to_vec()
    } else {
        Vec::new()
    };

    Some(AgwFrame {
        kind,
        pid,
        call_from,
        call_to,
        data_len,
        data: payload,
    })
}

/// Format a callsign and SSID for human-readable display.
///
/// The callsign is uppercased; the SSID suffix is appended when
/// `ssid` is non-zero (e.g. `"N7XYZ-4"`).
pub fn fmt_display_call(call: &str, ssid: u8) -> String {
    crate::ax25::fmt_call(call, ssid)
}

/// Extract the base callsign and SSID from a string like `"N7XYZ-4"`.
///
/// Returns `(base, ssid)` where `ssid` is clamped to 0–15.
/// When no dash is present the SSID is assumed to be 0.
pub fn parse_callsign_ssid(call: &str) -> (String, u8) {
    crate::ax25::parse_callsign_ssid(call)
}

// ── Helper for KISS ↔ AGW call mapping ─────────────────────

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_header_size() {
        let header = build_header(0, CMD_REGISTER, 0x00, "KK6SEN", "", 0);
        assert_eq!(header.len(), AGW_HEADER_SIZE);
        assert_eq!(header[0], 0); // Port
        assert_eq!(header[4], CMD_REGISTER); // DataKind
    }

    #[test]
    fn test_header_port_encoding() {
        let header = build_header(1, CMD_CONNECT, 0xF0, "KK6SEN", "KK6SEN", 0);
        assert_eq!(header[0], 1); // Port 1
        assert_eq!(header[1], 0);
        assert_eq!(header[2], 0);
        assert_eq!(header[3], 0);
    }

    #[test]
    fn test_header_callsign_padding() {
        let header = build_header(0, CMD_CONNECT, 0xF0, "KK6SEN-4", "N7XYZ", 0);
        // CallFrom at offset 8-17
        let call_from = &header[8..18];
        // Should be "KK6SEN-4\0" padded
        assert!(call_from.starts_with(b"KK6SEN-4"));
        assert_eq!(call_from[8], 0); // null terminator at position 8
        // CallTo at offset 18-27
        let call_to = &header[18..28];
        assert!(call_to.starts_with(b"N7XYZ"));
        assert_eq!(call_to[5], 0); // null terminator at position 5
    }

    #[test]
    fn test_header_data_len() {
        let data = b"Hello, Packet!";
        let frame = build_frame(0, CMD_DATA, 0xF0, "KK6SEN", "KK6SEN", data);
        assert_eq!(frame.len(), AGW_HEADER_SIZE + data.len());

        let data_len_bytes = &frame[28..32];
        let parsed_len = u32::from_le_bytes([data_len_bytes[0], data_len_bytes[1], data_len_bytes[2], data_len_bytes[3]]);
        assert_eq!(parsed_len, data.len() as u32);
    }

    #[test]
    fn test_parse_register_frame() {
        let frame = build_register("KK6SEN", 0);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.kind, CMD_REGISTER);
        assert_eq!(parsed.call_from.trim(), "KK6SEN");
        assert_eq!(parsed.data_len, 0);
    }

    #[test]
    fn test_parse_data_frame() {
        let payload = b"Test message";
        let frame = build_data(0, "KK6SEN", 1, "N7XYZ", 0, payload);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.kind, CMD_DATA);
        assert_eq!(parsed.call_from.trim(), "KK6SEN-1");
        assert_eq!(parsed.call_to.trim(), "N7XYZ");
        assert_eq!(parsed.data_len, payload.len() as u32);
        assert_eq!(&parsed.data, payload);
    }

    #[test]
    fn test_parse_disconnect_frame() {
        let frame = build_disconnect(0, "KK6SEN", 0, "N7XYZ", 4);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.kind, CMD_DISCONNECT);
        assert_eq!(parsed.call_from.trim(), "KK6SEN");
        assert_eq!(parsed.call_to.trim(), "N7XYZ-4");
        assert_eq!(parsed.data_len, 0);
    }

    #[test]
    fn test_build_connect_via() {
        let digis = vec!["WIDE1".to_string(), "WIDE2".to_string()];
        let frame = build_connect_via(0, "KK6SEN", 0, "N7XYZ", 0, &digis);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.kind, CMD_CONNECT_VIA);

        // VIA data: count(1) + 2 digis (10 bytes each) = 21 bytes
        assert_eq!(parsed.data_len, 21);
        assert_eq!(parsed.data[0], 2); // Two digis
    }

    #[test]
    fn test_parse_callsign_field() {
        let field = b"KK6SEN-4\0\0";
        let parsed = parse_callsign_field(field);
        assert_eq!(parsed, "KK6SEN-4");
    }

    #[test]
    fn test_parse_callsign_ssid() {
        let (call, ssid) = parse_callsign_ssid("KK6SEN-4");
        assert_eq!(call, "KK6SEN");
        assert_eq!(ssid, 4);

        let (call, ssid) = parse_callsign_ssid("KK6SEN");
        assert_eq!(call, "KK6SEN");
        assert_eq!(ssid, 0);
    }

    #[test]
    fn test_fmt_call() {
        assert_eq!(fmt_display_call("kk6sen", 0), "KK6SEN");
        assert_eq!(fmt_display_call("kk6sen", 4), "KK6SEN-4");
    }

    #[test]
    fn test_build_monitor_toggle() {
        let frame = build_monitor_toggle();
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.kind, CMD_MONITOR_TOGGLE);
        assert_eq!(parsed.data_len, 0);
    }

    #[test]
    fn test_build_unproto_frame() {
        let payload = b"Hello world";
        let frame = build_unproto(0, "KK6SEN", 0, "CQ", 0, payload);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.kind, CMD_UNPROTO);
        assert_eq!(parsed.call_from.trim(), "KK6SEN");
        assert_eq!(parsed.call_to.trim(), "CQ");
        assert_eq!(&parsed.data, payload);
    }

    #[test]
    fn test_frame_roundtrip_data() {
        let original = b"This is a test message for AGWPE!";
        let frame = build_data(0, "KK6SEN", 0, "TEST", 0, original);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(&parsed.data, original);
        assert_eq!(parsed.call_from.trim(), "KK6SEN");
        assert_eq!(parsed.call_to.trim(), "TEST");
    }

    #[test]
    fn test_parse_frame_too_short() {
        let short = vec![0u8; 10];
        assert!(parse_frame(&short).is_none());
    }
}
