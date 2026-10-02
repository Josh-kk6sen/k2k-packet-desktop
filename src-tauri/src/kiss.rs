// ── KISS Protocol Layer ──────────────────────────────────────────────────────
//
// This module implements the KISS (Keep It Simple, Stupid) TNC framing
// protocol for AX.25 packet radio communication.
//
// KISS is a minimal serial-line protocol originally defined by Mike Chepponis
// (K3MC) and Phil Karn (KA9Q) in 1987 (ARRL 6th Computer Networking
// Conference). It wraps raw AX.25 HDLC frames in a simple SLIP-like framing
// structure using two special bytes:
//
// - `FEND` (0xC0) — Frame End delimiter (marks frame boundaries)
// - `FESC` (0xDB) — Frame Escape (byte-stuffing escape)
//
// Unlike AGWPE, KISS has **no AX.25 state machine** — it's a dumb pipe that
// passes raw bytes. The host application is responsible for implementing the
// entire AX.25 Layer 2 protocol (SABM/UA/DISC, sequence numbers, T1/T3 timers,
// window management, etc.).
//
// # Frame Format
//
// ```text
// [FEND] [Command/Port] [Escaped Data...] [FEND]
//   1B         1B            N bytes        1B
// ```
//
// The command/port byte encodes both the KISS command (lower nibble) and the
// TNC port number (upper nibble). For data transmission, this byte is always
// `0x00` (port 0, CMD_DATA).
//
// # References
//
// - "KISS: A Simple Protocol for TNC Communication", ARRL 6th Computer
//   Networking Conference, 1987
// - AX.25 Amateur Packet-Radio Link-Layer Protocol, Version 2.2
//

// ── KISS Protocol Constants ────────────────────────────────────────────────

use tokio::io::AsyncWriteExt;

/// Frame End (0xC0) — Marks the beginning and end of a KISS frame.
///
/// This is the frame delimiter byte. Every complete KISS frame starts and
/// ends with this byte. When `FEND` appears literally inside the payload
/// data, it must be escaped using the byte-stuffing mechanism (see
/// [`kiss_escape`]).
pub const FEND: u8 = 0xC0;

/// Frame Escape (0xDB) — Introduces a two-byte escape sequence.
///
/// When the payload contains a literal `FEND` or `FESC` byte, that byte is
/// replaced with a two-byte escape sequence starting with `FESC`:
/// - `FESC TFEND` (0xDB 0xDC) → literal `FEND` (0xC0)
/// - `FESC TFESC` (0xDB 0xDD) → literal `FESC` (0xDB)
pub const FESC: u8 = 0xDB;

/// Transposed FEND (0xDC) — Second byte of the FEND escape sequence.
///
/// When `FESC` is followed by `TFEND`, the pair represents a literal `FEND`
/// byte that was present in the original data.
pub const TFEND: u8 = 0xDC;

/// Transposed FESC (0xDD) — Second byte of the FESC escape sequence.
///
/// When `FESC` is followed by `TFESC`, the pair represents a literal `FESC`
/// byte that was present in the original data.
pub const TFESC: u8 = 0xDD;

/// KISS command byte for data frames (lower nibble = 0x00).
///
/// The KISS command/port byte is the first byte after the opening `FEND`.
/// The upper nibble encodes the TNC port number; the lower nibble encodes the
/// command. `0x00` means port 0, CMD_DATA — the standard command for sending
/// an AX.25 frame. The receiver peels this byte off before processing the
/// payload.
///
/// When building a KISS frame for transmission, use:
/// ```text
/// let cmd_byte = (port << 4) | CMD_DATA;   // CMD_DATA = 0x00
/// ```
pub const CMD_DATA: u8 = 0x00;

/// KISS command: Transmitter keyup delay in 10 ms units. Default: 50 (500ms).
/// For Direwolf + UV-Pro on localhost: 30 (300ms) is recommended.
pub const CMD_TXDELAY: u8 = 0x01;

/// KISS command: P-persistence parameter. P = p × 256 − 1 (p in 0..1).
/// Default: 63 (p≈0.25). Set to 255 for immediate transmit on clear channel.
pub const CMD_P: u8 = 0x02;

/// KISS command: Slot interval in 10 ms units. Default: 10 (100ms).
/// UV-Pro default is 50 (500ms). Lower = more responsive.
pub const CMD_SLOT_TIME: u8 = 0x03;

// ── Byte Stuffing / Escape Functions ──────────────────────────────────────

/// Escape special bytes for transmission over a KISS link.
///
/// This is the **byte stuffing** (SLIP-style) encoding that ensures the frame
/// delimiter bytes (`FEND` and `FESC`) never appear literally inside the
/// payload, allowing the receiver to unambiguously detect frame boundaries.
///
/// # Substitutions
///
/// | Original | Replaced with  |
/// |----------|----------------|
/// | `FEND` (0xC0) | `FESC TFEND` (0xDB 0xDC) |
/// | `FESC` (0xDB) | `FESC TFESC` (0xDB 0xDD) |
///
/// All other bytes pass through unchanged.
pub fn kiss_escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &b in data {
        match b {
            FEND => {
                out.push(FESC);
                out.push(TFEND);
            }
            FESC => {
                out.push(FESC);
                out.push(TFESC);
            }
            _ => out.push(b),
        }
    }
    out
}

/// Unescape received KISS data — reverses `kiss_escape()`.
///
/// Processes a byte buffer that was previously escaped for KISS framing,
/// replacing two-byte escape sequences back to their original single-byte
/// values. Also strips any stray `FEND` bytes that may appear between
/// frames (e.g., consecutive FEND delimiters with no data).
///
/// # Substitutions
///
/// | Escaped sequence   | Unescaped to |
/// |--------------------|--------------|
/// | `FESC TFEND` (0xDB 0xDC) | `FEND` (0xC0) |
/// | `FESC TFESC` (0xDB 0xDD) | `FESC` (0xDB) |
/// | Stray `FEND`       | Stripped (dropped) |
/// | Invalid escape seq | Silently skipped |
///
/// # State
///
/// Uses a single `escaped` boolean to track whether we are mid-escape
/// sequence. This is a simple two-state recogniser:
///
/// ```text
/// NORMAL → see FESC → ESCAPED
/// ESCAPED → see TFEND → emit FEND → NORMAL
/// ESCAPED → see TFESC → emit FESC → NORMAL
/// ESCAPED → see anything else → skip → NORMAL
/// ```
pub fn kiss_unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut escaped = false;
    for &b in data {
        if escaped {
            match b {
                TFEND => out.push(FEND),
                TFESC => out.push(FESC),
                // Invalid escape sequence — log a warning so operators can
                // notice noise/corruption issues, then drop both bytes.
                other => {
                    log::warn!("KISS unescape: invalid escape byte 0x{:02X} — dropping sequence", other);
                }
            }
            escaped = false;
        } else if b == FESC {
            escaped = true;
        } else if b != FEND {
            // Strip any stray FEND bytes that may appear between frames
            // (e.g., consecutive FENDs closing one frame and opening the next)
            out.push(b);
        }
        // FEND in non-escaped state is intentionally dropped — it's either
        // a frame delimiter that shouldn't be in the data, or a residual
        // byte between frames.
    }
    out
}

// ── Frame Building ─────────────────────────────────────────────────────────

/// Build a complete KISS frame ready for transmission.
///
/// Constructs a framed byte sequence by wrapping the raw AX.25 data with
/// KISS framing according to the following pipeline:
///
/// 1. **Start delimiter:** Prepend `FEND` (0xC0).
/// 2. **Command byte:** Append `(port << 4) | CMD_DATA` (port in upper nibble,
///    data command in lower nibble). Port 0 + CMD_DATA = `0x00`.
/// 3. **Escape payload:** Run `kiss_escape()` on the AX.25 data to protect
///    any literal `FEND`/`FESC` bytes inside the payload.
/// 4. **End delimiter:** Append `FEND` (0xC0).
pub fn build_kiss_frame(port: u8, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(data.len() + 4);
    frame.push(FEND);
    frame.push((port << 4) | CMD_DATA);
    frame.extend_from_slice(&kiss_escape(data));
    frame.push(FEND);
    frame
}

/// Build a KISS command frame (non-data: TXDELAY, P, SlotTime, etc.).
///
/// Per the KISS spec, command frames use a type byte with the command in the
/// low nibble and port in the high nibble, followed by a single parameter byte:
///   `FEND + type + param + FEND`
///
/// The parameter byte is NOT escaped — it's a single byte and never contains
/// FEND or FESC in practice. If it did, the spec's "all implementations must
/// ignore unsupported commands" rule means the TNC would silently disregard it.
pub fn build_kiss_command(port: u8, command: u8, param: u8) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4);
    frame.push(FEND);
    frame.push((port << 4) | (command & 0x0F));
    frame.push(param);
    frame.push(FEND);
    frame
}

/// Send a KISS command frame to configure the TNC (TXDELAY, P, SlotTime, etc.).
///
/// This is a convenience wrapper that builds a KISS command frame and writes it
/// to the TNC's TCP stream. Returns immediately after writing — KISS has no
/// acknowledgments, so there's nothing to wait for.
pub async fn send_kiss_command(
    client: &mut crate::session::TcpClient,
    command: u8,
    param: u8,
) -> Result<(), String> {
    let writer = client.writer()?;
    let frame = build_kiss_command(0, command, param);
    log::info!("kiss: sent cmd 0x{:02X} param={}", command, param);
    writer.write_all(&frame).await.map_err(|e| format!("KISS cmd write error: {}", e))
}

// ── Transmission ───────────────────────────────────────────────────────────

/// Send a raw AX.25 frame via KISS to the connected TNC.
///
/// This is the primary transmission function for the KISS protocol. It takes
/// a raw AX.25 frame (without KISS framing), wraps it with `build_kiss_frame`,
/// and writes it to the TNC's TCP stream.
///
/// # Arguments
///
/// * `client` — A mutable reference to the `TcpClient` holding the active
///   TCP connection to the KISS TNC.
/// * `frame` — The raw AX.25 bytes to transmit (address + control + PID +
///   info). KISS framing (FEND delimiters, byte-stuffing) is added internally.
///
/// # Errors
///
/// Returns `Err(String)` if:
/// - The client has no active TCP stream (`"Not connected to TNC"`).
/// - The underlying `write_all()` call fails (I/O error from the transport).
pub async fn send_kiss(
    client: &mut crate::session::TcpClient,
    frame: &[u8],
) -> Result<(), String> {
    // Phase 3: frame through the live `Link` adapter when attached. The
    // adapter derives the exact same KISS frame (build_kiss_frame), and we
    // retrieve the framed bytes from its capture buffer before writing —
    // byte-identical to the direct path. Compute the framed bytes FIRST so we
    // don't hold a borrow on `writer` while also touching `client.link`.
    let kiss_frame = if let Some(link) = client.link.as_mut() {
        // Frame through the live adapter, then pull the framed bytes back out.
        link.send_frame(crate::link::Outgoing { bytes: frame.to_vec() })
            .map_err(|e| format!("KISS link error: {e}"))?;
        link.take_last_frame().unwrap_or_else(|| build_kiss_frame(0, frame))
    } else {
        build_kiss_frame(0, frame)
    };
    let writer = client.writer()?;
    writer.write_all(&kiss_frame).await.map_err(|e| format!("KISS write error: {e}"))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kiss_escape_no_special() {
        let data = b"HELLO";
        assert_eq!(kiss_escape(data), b"HELLO");
    }

    #[test]
    fn test_kiss_escape_fend() {
        let data = [FEND];
        assert_eq!(kiss_escape(&data), [FESC, TFEND]);
    }

    #[test]
    fn test_kiss_escape_fesc() {
        let data = [FESC];
        assert_eq!(kiss_escape(&data), [FESC, TFESC]);
    }

    #[test]
    fn test_kiss_escape_multiple() {
        let data = [FEND, b'A', FESC, b'B', FEND];
        let expected = [FESC, TFEND, b'A', FESC, TFESC, b'B', FESC, TFEND];
        assert_eq!(kiss_escape(&data), expected);
    }

    #[test]
    fn test_kiss_unescape_roundtrip() {
        let original = b"\xC8Hello\xDBWorld\xC8";
        let escaped = kiss_escape(original);
        let unescaped = kiss_unescape(&escaped);
        assert_eq!(unescaped, original);
    }

    #[test]
    fn test_kiss_unescape_strips_stray_fend() {
        // Stray FEND between frames should be stripped
        let data = [b'H', b'i', FEND, b'!'];
        let result = kiss_unescape(&data);
        assert_eq!(result, b"Hi!");
    }

    #[test]
    fn test_build_kiss_frame_format() {
        let data = b"AX25DATA";
        let frame = build_kiss_frame(0, data);
        assert_eq!(frame[0], FEND);      // Start delimiter
        assert_eq!(frame[1], CMD_DATA);  // Port 0, data command
        assert_eq!(&frame[2..frame.len() - 1], data); // Payload (no special bytes)
        assert_eq!(frame[frame.len() - 1], FEND); // End delimiter
    }

    #[test]
    fn test_build_kiss_frame_with_special() {
        let data = [FEND, FESC];
        let frame = build_kiss_frame(0, &data);
        assert!(frame.len() > 4); // Should be longer due to escaping
        assert_eq!(frame[0], FEND);
        assert_eq!(frame[frame.len() - 1], FEND);
    }

    #[test]
    fn test_kiss_frame_roundtrip() {
        let original = b"\x82\xA0\xE6\xAE\x92\x40\x40\x60";
        let frame = build_kiss_frame(0, original);
        // Strip FENDs and command byte, then unescape
        let payload = &frame[2..frame.len() - 1];
        let result = kiss_unescape(payload);
        assert_eq!(result, original);
    }
}
