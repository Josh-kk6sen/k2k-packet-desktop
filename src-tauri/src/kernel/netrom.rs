// ── NET/ROM Protocol Parsing ──────────────────────────────
//
// NET/ROM is a layer-3 routing protocol used on top of AX.25 (PID = 0xCF).
// Two different frame layouts arrive with that same PID, and the first
// information-field byte tells them apart:
//
//   1. NODES Routing Broadcast (UI frame to the "NODES" destination)
//      0xFF + 6 ASCII characters of the originating node's name, followed by
//      N × 21-byte route entries:
//        [0..7]   destination node address (shifted AX.25: 6 chars + ssid<<1)
//        [7..13]  destination node alias (6 raw ASCII characters)
//        [13..20] next-hop node address   (shifted, same encoding)
//        [20]     route quality (0-255, higher is better)
//      Every broadcast in the 145.050 captures is exactly 7 + 21·N bytes.
//
//   2. NET/ROM L3 data (inter-node crosslink frame — "The NET/ROM Protocol",
//      Structure of Inter-Node HDLC Frames)
//        [0..7]   originating node address (shifted AX.25)
//        [7..14]  destination node address (shifted AX.25)
//        [14]     time to live
//        [15]     circuit index
//        [16]     circuit id
//        [17]     TX sequence number
//        [18]     RX sequence number
//        [19]     opcode (low nibble) + flags (high nibble: 0x80 choke,
//                 0x40 NAK, 0x20 more-follows; on a connect ack 0x80 means
//                 "refused")
//        [20..]   opcode payload — user data for "information"; the
//                 proposed/accepted window size for connect request/ack (a
//                 connect request then carries originating user call (7) and
//                 originating node call (7)); nothing for the ack/disconnect
//                 opcodes
//
// A NET/ROM L3 data frame starts with a shifted callsign byte (0x40–0xFF),
// never 0xFF, which is why 0xFF is the discriminator between the two.
//
// AX.25 spec reference: http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf

use crate::ax25::{decode_netrom_node, fmt_call, info_to_string};

const NETROM_ENTRY_SIZE: usize = 21; // route entry: dest(7) + alias(6) + next hop(7) + quality(1)
const MAX_NETROM_ENTRIES: usize = 30; // Cap parsing to avoid runaway loops on bad data
const NETROM_L3_MIN: usize = 20; // origin(7) + dest(7) + ttl(1) + transport header(5)

// L4 opcodes — the low nibble of the opcode/flags byte.
const L4_CONNECT_REQUEST: u8 = 0x01;
const L4_CONNECT_ACK: u8 = 0x02;
const L4_DISCONNECT_REQUEST: u8 = 0x03;
const L4_DISCONNECT_ACK: u8 = 0x04;
const L4_INFO: u8 = 0x05;
const L4_INFO_ACK: u8 = 0x06;

/// Parse a NET/ROM frame's info field and return (display_text, kind).
///
/// Dispatches on the first byte: `0xFF` is a NODES routing broadcast, anything
/// else is an L3 crosslink frame. Returns None when the info field is neither,
/// so the caller can fall back to a hex dump instead of inventing a decode.
pub fn parse_netrom_frame(info: &[u8]) -> Option<(String, String)> {
    if info.len() < 7 {
        return None;
    }
    if info[0] == 0xFF {
        parse_routing_broadcast(info)
    } else {
        parse_l3_frame(info)
    }
}

/// NODES routing broadcast: `0xFF` + 6 ASCII name bytes + 21-byte entries.
fn parse_routing_broadcast(info: &[u8]) -> Option<(String, String)> {
    let name_raw = &info[1..7];
    let name_end = name_raw
        .iter()
        .position(|&b| b < 32 || b > 126)
        .unwrap_or(name_raw.len());
    let name = String::from_utf8_lossy(&name_raw[..name_end])
        .trim()
        .to_string();

    let mut body = String::new();
    let mut entry_num = 0;
    let mut pos = 7; // entries start after the 0xFF + 6-name header

    while pos + NETROM_ENTRY_SIZE <= info.len() {
        let entry = &info[pos..pos + NETROM_ENTRY_SIZE];

        // Destination node address (shifted format, same as AX.25 addresses).
        if let Some((node_call, ssid)) = decode_netrom_node(&entry[0..7]) {
            // Validate that the callsign looks reasonable (not binary garbage)
            let call_valid = !node_call.is_empty()
                && node_call.chars().all(|c| c.is_ascii_alphanumeric());
            if !call_valid {
                pos += 1; // Skip forward and try again
                continue;
            }
            let node_str = fmt_call(&node_call, ssid);

            // Alias: 6 raw ASCII bytes at offset 7–12, space padded.
            let alias_end = entry[7..13].iter().position(|&b| b < 32).unwrap_or(6);
            let alias_str = String::from_utf8_lossy(&entry[7..7 + alias_end])
                .trim()
                .to_string();

            // Next-hop node address (shifted, at offset 13–19).
            let next_hop = match decode_netrom_node(&entry[13..20]) {
                Some((nb_call, nb_ssid)) => fmt_call(&nb_call, nb_ssid),
                None => String::new(),
            };

            // Route quality (1 byte at offset 20).
            let quality = entry[20];

            if entry_num == 0 {
                body.push_str(&format!("NET/ROM Routing: {}\n", name));
            }
            body.push_str(&format!(
                "  {:9} {:<12} {:9} {}\n",
                node_str, alias_str, next_hop, quality
            ));
            entry_num += 1;
            pos += NETROM_ENTRY_SIZE;
            if entry_num >= MAX_NETROM_ENTRIES {
                break;
            }
        } else {
            pos += 1;
        }
    }

    if entry_num > 0 {
        return Some((body.trim_end().to_string(), "NET/ROM routing".to_string()));
    }

    // Announcement-only broadcast (0xFF + name, no route entries).
    if !name.is_empty() {
        return Some((
            format!("Node: {}", name),
            "NET/ROM announcement".to_string(),
        ));
    }
    None
}

/// NET/ROM L3 crosslink frame: origin(7) + dest(7) + ttl(1) + L4 header(5).
fn parse_l3_frame(info: &[u8]) -> Option<(String, String)> {
    if info.len() < NETROM_L3_MIN {
        return None;
    }
    let origin = node_label(&info[0..7])?;
    let dest = node_label(&info[7..14])?;

    let ttl = info[14];
    let ci = info[15];
    let cid = info[16];
    let tx = info[17];
    let rx = info[18];
    let opbyte = info[19];
    let opcode = opbyte & 0x0F;

    let mut flags = Vec::new();
    if opbyte & 0x40 != 0 {
        flags.push("NAK");
    }
    if opbyte & 0x20 != 0 {
        flags.push("more-follows");
    }
    let flag_str = if flags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", flags.join(" "))
    };

    match opcode {
        L4_CONNECT_REQUEST => {
            let window = *info.get(20).unwrap_or(&0);
            let mut line = format!(
                "NET/ROM connect request: {} -> {}\n  cct {}/{} ttl {} window {}",
                origin, dest, ci, cid, ttl, window
            );
            // Originating user call followed by the originating node call.
            if let (Some(user), Some(node)) = (
                info.get(21..28).and_then(node_label),
                info.get(28..35).and_then(node_label),
            ) {
                line.push_str(&format!("  from {} via {}", user, node));
            }
            Some((line, "NET/ROM connect request".to_string()))
        }
        L4_CONNECT_ACK => {
            let window = *info.get(20).unwrap_or(&0);
            if opbyte & 0x80 != 0 {
                Some((
                    format!(
                        "NET/ROM connect refused: {} -> {}\n  cct {}/{} ttl {}",
                        origin, dest, ci, cid, ttl
                    ),
                    "NET/ROM connect refused".to_string(),
                ))
            } else {
                Some((
                    format!(
                        "NET/ROM connect ack: {} -> {}\n  cct {}/{} ttl {} window {}",
                        origin, dest, ci, cid, ttl, window
                    ),
                    "NET/ROM connect ack".to_string(),
                ))
            }
        }
        L4_DISCONNECT_REQUEST => Some((
            format!(
                "NET/ROM disconnect request: {} -> {}\n  cct {}/{} ttl {}",
                origin, dest, ci, cid, ttl
            ),
            "NET/ROM disconnect".to_string(),
        )),
        L4_DISCONNECT_ACK => Some((
            format!(
                "NET/ROM disconnect ack: {} -> {}\n  cct {}/{} ttl {}",
                origin, dest, ci, cid, ttl
            ),
            "NET/ROM disconnect ack".to_string(),
        )),
        L4_INFO => {
            let mut body = format!(
                "NET/ROM data: {} -> {}\n  cct {}/{} ttl {} tx {} rx {}{}",
                origin, dest, ci, cid, ttl, tx, rx, flag_str
            );
            let payload = &info[20..];
            if !payload.is_empty() {
                let text = info_to_string(payload);
                // The BBS sends CR-terminated lines; render them as real line
                // breaks instead of literal \r glyphs.
                let text = text
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .trim_end_matches('\n')
                    .to_string();
                if !text.is_empty() {
                    body.push('\n');
                    body.push_str(&text);
                }
            }
            Some((body, "NET/ROM data".to_string()))
        }
        L4_INFO_ACK => Some((
            format!(
                "NET/ROM data ack: {} -> {}\n  cct {}/{} ttl {} tx {} rx {}{}",
                origin, dest, ci, cid, ttl, tx, rx, flag_str
            ),
            "NET/ROM data ack".to_string(),
        )),
        _ => None, // unknown opcode — the caller shows the hex dump instead
    }
}

/// Decode a 7-byte NET/ROM node address and render it as `CALL-SSID`.
/// Rejects anything that does not look like a callsign so that non-NET/ROM
/// payloads fall through to the hex dump rather than printing noise.
fn node_label(addr: &[u8]) -> Option<String> {
    let (call, ssid) = decode_netrom_node(addr)?;
    if call.is_empty()
        || !call
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    Some(fmt_call(&call, ssid))
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake 7-byte NET/ROM node from a callsign string.
    fn make_node(call: &str, ssid: u8) -> Vec<u8> {
        let mut buf = vec![0u8; 7];
        let chars: Vec<char> = call.to_uppercase().chars().collect();
        for i in 0..6.min(chars.len()) {
            buf[i] = (chars[i] as u8) << 1;
        }
        for i in chars.len()..6 {
            buf[i] = b' ' << 1;
        }
        buf[6] = (ssid & 0x0F) << 1 | 0x60;
        buf
    }

    /// Decode a space-separated hex string into bytes (test fixture helper).
    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|t| u8::from_str_radix(t, 16).unwrap())
            .collect()
    }

    #[test]
    fn test_parse_netrom_too_short() {
        assert!(parse_netrom_frame(&[0u8; 3]).is_none());
    }

    #[test]
    fn test_decode_netrom_node() {
        let node = make_node("KK6SEN", 0);
        let (call, ssid) = decode_netrom_node(&node).unwrap();
        assert_eq!(call, "KK6SEN");
        assert_eq!(ssid, 0);
    }

    /// Real capture (145.050): K6BER-5's NODES broadcast, 7 + 6·21 = 133 bytes.
    #[test]
    fn test_real_routing_broadcast() {
        let info = hex(
            "ff 42 45 52 52 59 20 9c 6c 84 a6 a6 40 08 23 44 49 47 49 20 9c 6c 84 a6 a6 40 08 46 \
             96 92 6c b4 90 88 0a 53 43 4c 41 52 41 96 92 6c b4 90 88 0a 46 96 8c 6c 88 a2 aa 12 \
             42 41 4e 4e 45 52 96 8c 6c 88 a2 aa 12 46 ae 84 6c b2 9c 9a 06 42 45 54 48 45 4c \
             ae 84 6c b2 9c 9a 06 46 ae 6c 82 84 94 40 0a 42 55 4c 4e 20 20 ae 6c 82 84 94 40 0a 46 \
             96 9a 6c 98 b2 ae 08 43 4f 4f 4c 20 20 96 9a 6c 98 b2 ae 08 46",
        );
        assert_eq!(info.len(), 133);

        let (display, kind) = parse_netrom_frame(&info).unwrap();
        assert_eq!(kind, "NET/ROM routing");
        let rows: Vec<&str> = display.lines().collect();
        assert_eq!(rows[0], "NET/ROM Routing: BERRY");
        assert_eq!(rows.len(), 7); // header + 6 entries
        assert!(display.contains("N6BSS-4   #DIGI        N6BSS-4"), "{}", display);
        assert!(display.contains("KI6ZHD-5  SCLARA       KI6ZHD-5"), "{}", display);
        assert!(display.contains("KF6DQU-9  BANNER       KF6DQU-9"), "{}", display);
        assert!(display.contains("KM6LYW-4  COOL         KM6LYW-4  70"), "{}", display);
    }

    /// Real capture: announcement-only broadcast (0xFF + name, no entries).
    #[test]
    fn test_real_announcement_only() {
        let info = hex("ff 42 45 52 52 59 20");
        let (display, kind) = parse_netrom_frame(&info).unwrap();
        assert_eq!(kind, "NET/ROM announcement");
        assert_eq!(display, "Node: BERRY");
    }

    /// Real capture: NET/ROM L3 information frame — the exact frame that used
    /// to decode as a routing table full of noise (#DIGI-style garbage rows).
    #[test]
    fn test_real_l3_information_frame() {
        let info = hex(
            "96 8c 6c 82 9c b0 08 96 6c 8c 84 40 40 0b 19 01 14 02 02 25 \
             4d 53 47 23 20 20 53 54 20 53 49 5a 45 20 20 54 4f 20 20 20 20 20 46 52 4f 4d 20 \
             20 20 44 41 54 45 0d",
        );

        let (display, kind) = parse_netrom_frame(&info).unwrap();
        assert_eq!(kind, "NET/ROM data");
        assert!(!display.contains("Routing"), "{}", display);
        assert!(
            display.starts_with("NET/ROM data: KF6ANX-4 -> K6FB-5"),
            "{}",
            display
        );
        assert!(
            display.contains("cct 1/20 ttl 25 tx 2 rx 2 [more-follows]"),
            "{}",
            display
        );
        assert!(
            display.contains("MSG#  ST SIZE  TO     FROM   DATE"),
            "{}",
            display
        );
    }

    /// Real capture: the 32-byte continuation frame of the same message.
    #[test]
    fn test_real_l3_information_frame_short() {
        let info = hex(
            "96 8c 6c 82 9c b0 08 96 6c 8c 84 40 40 0b 19 01 14 03 02 05 2c 20 6f 72 20 48 65 6c 70 20 3e 0d",
        );
        let (display, _) = parse_netrom_frame(&info).unwrap();
        assert!(
            display.starts_with("NET/ROM data: KF6ANX-4 -> K6FB-5"),
            "{}",
            display
        );
        assert!(display.contains("tx 3 rx 2"), "{}", display);
        assert!(!display.contains("more-follows"), "{}", display);
        assert!(display.ends_with(", or Help >"), "{}", display);
    }

    /// Real capture: connect ack — 14 + ttl + 5-byte header + window = 21 bytes.
    #[test]
    fn test_real_l3_connect_ack() {
        let info = hex("96 8c 6c 82 9c b0 09 96 6c 8c 84 40 40 0a 19 01 14 00 0c 02 04");
        let (display, kind) = parse_netrom_frame(&info).unwrap();
        assert_eq!(kind, "NET/ROM connect ack");
        assert!(
            display.starts_with("NET/ROM connect ack: KF6ANX-4 -> K6FB-5"),
            "{}",
            display
        );
        assert!(display.contains("window 4"), "{}", display);
    }

    /// Real capture: disconnect request (20-byte info field, no opcode payload).
    #[test]
    fn test_real_l3_disconnect() {
        let info = hex("96 8c 6c 82 9c b0 08 96 6c 8c 84 40 40 0b 19 01 14 00 00 03");
        let (display, kind) = parse_netrom_frame(&info).unwrap();
        assert_eq!(kind, "NET/ROM disconnect");
        assert!(
            display.starts_with("NET/ROM disconnect request: KF6ANX-4 -> K6FB-5"),
            "{}",
            display
        );
    }

    /// Non-NET/ROM payload: not 0xFF, not a pair of callsigns.
    #[test]
    fn test_plain_text_is_not_netrom() {
        assert!(parse_netrom_frame(b"Hello from 5W Garage").is_none());
    }

    /// Real capture: connect-ack-era banner frame; CR line endings in the
    /// payload must render as real line breaks, not literal \r glyphs.
    #[test]
    fn test_real_l3_banner_cr_line_breaks() {
        let info = hex(
            "96 8c 6c 82 9c b0 08 96 6c 8c 84 40 40 0b 19 01 18 01 01 05 5b 4b 50 43 33 50 2d 38 \
             2e 33 2d 48 4d 24 5d 0d 32 34 31 30 37 20 42 59 54 45 53 20 41 56 41 49 4c 41 42 4c \
             45 0d",
        );
        let (display, kind) = parse_netrom_frame(&info).unwrap();
        assert_eq!(kind, "NET/ROM data");
        assert!(!display.contains('\r'), "{:?}", display);
        assert!(
            display.ends_with("[KPC3P-8.3-HM$]\n24107 BYTES AVAILABLE"),
            "{:?}",
            display
        );
    }

    /// Unknown L4 opcode falls through to the caller's hex dump.
    #[test]
    fn test_unknown_opcode_is_none() {
        let mut info = make_node("KK6SEN", 0);
        info.extend_from_slice(&make_node("KB7XYZ", 5));
        info.push(0x19); // ttl
        info.push(0x01); // circuit index
        info.push(0x02); // circuit id
        info.push(0x00); // tx
        info.push(0x00); // rx
        info.push(0x09); // opcode 9 — undefined
        assert!(parse_netrom_frame(&info).is_none());
    }

    /// A hand-built information frame still parses end to end.
    #[test]
    fn test_parse_netrom_connected_data() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&make_node("KK6SEN", 4));
        frame.extend_from_slice(&make_node("KB7XYZ", 5));
        frame.push(0x19); // ttl
        frame.push(0x01); // circuit index
        frame.push(0x07); // circuit id
        frame.push(0x01); // tx sequence
        frame.push(0x01); // rx sequence
        frame.push(0x05); // opcode: information
        frame.extend_from_slice(b"Hello");

        let (display, kind) = parse_netrom_frame(&frame).unwrap();
        assert_eq!(kind, "NET/ROM data");
        assert!(display.contains("KK6SEN-4 -> KB7XYZ-5"), "{}", display);
        assert!(display.contains("Hello"), "{}", display);
    }
}
