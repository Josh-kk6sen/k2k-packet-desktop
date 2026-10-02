// ── NET/ROM Protocol Parsing ──────────────────────────────
//
// NET/ROM is a layer-3 routing protocol used on top of AX.25.
// It provides node-to-node routing across the amateur radio network.
//
// Frame types handled here:
//   1. NODES Routing Broadcast (PID=0xCF) — periodic network topology updates
//      Each entry is 21 bytes: node callsign (7) + node name (6) +
//      neighbor callsign (7) + quality (1)
//
//   2. Connected Data (PID=0x01) — data frames carried over NET/ROM circuits
//      Format: src_node (7) + dest_node (7) + type (1) + circuit_id (1) + sub_ci (1) + payload
//
// AX.25 spec reference: http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf

use crate::ax25::{decode_netrom_node, fmt_call, info_to_string};

const NETROM_ENTRY_SIZE: usize = 21; // Each NODES routing entry is 21 bytes: call(7) + name(6) + neighbor(7) + qual(1)
const MAX_NETROM_ENTRIES: usize = 30; // Cap parsing to avoid runaway loops on bad data

/// Parse a NET/ROM frame's info field and return (display_text, radio_header_suffix).
///
/// Handles both routing broadcasts (PID=0xCF) and connected data (PID=0x01).
/// Returns None if the info field can't be parsed as NET/ROM.
pub fn parse_netrom_frame(info: &[u8]) -> Option<(String, String)> {
    if info.len() < 7 { return None; }

    // ── Detect broadcast vs connected format from first byte ──
    // NODES routing broadcasts (PID=0xCF) start with 0xFF or 0x02 as a type byte,
    // followed by 6 bytes of raw ASCII node name.
    // Connected NET/ROM data (PID=0x01) starts with a shifted 7-byte callsign.
    let is_broadcast = info[0] == 0xFF || info[0] == 0x02;

    let (src_str, entry_offset) = if is_broadcast {
        // Broadcast header: [0]=type, [1..6]=node name (raw ASCII, NOT shifted)
        // Decode the node name directly — no shifting needed
        let name_raw = &info[1..7.min(info.len())];
        let name_end = name_raw.iter().position(|&b| b < 32 || b > 126).unwrap_or(name_raw.len());
        let name = String::from_utf8_lossy(&name_raw[..name_end]).trim().to_string();
        (name, 7) // entries start at byte 7
    } else {
        // Connected data header: [0..6]=source node (shifted callsign like AX.25)
        let src = decode_netrom_node(&info[0..7])?;
        let src_str = fmt_call(&src.0, src.1);
        (src_str, 7)
    };

    // ── NODES Routing Broadcast (PID=0xCF) ──
    //
    //    Format: source node (7 bytes) + zero or more entries of 21 bytes each:
    //      [0..7]   Node Callsign   (7 bytes, shifted AX.25 — decoded via byte>>1)
    //      [7..13]  Node Name       (6 bytes, RAW ASCII — NOT shifted!)
    //      [13..20] Neighbor Call   (7 bytes, shifted AX.25)
    //      [20]     Quality         (1 byte, 0–255, higher = better path)

    let mut body = String::new();
    body.push_str(&format!("NET/ROM Routing: {}\n", src_str));
    let mut entry_num = 0;
    let mut pos = entry_offset;

    while pos + NETROM_ENTRY_SIZE <= info.len() {
        let entry = &info[pos..pos + NETROM_ENTRY_SIZE];

        // Node callsign (shifted format, same as AX.25 addresses)
        if let Some((node_call, ssid)) = decode_netrom_node(&entry[0..7]) {
            // Validate that the callsign looks reasonable (not binary garbage)
            let call_valid = node_call.len() >= 1
                && node_call.chars().all(|c| c.is_ascii_alphanumeric());
            if !call_valid {
                pos += 1; // Skip forward and try again
                continue;
            }
            let node_str = fmt_call(&node_call, ssid);

            // Node name (RAW ASCII at offset 7–12, NOT shifted)
            // Find the first control character to trim padding
            let name_end = entry[7..13].iter().position(|&b| b < 32).unwrap_or(6);
            let name_str = String::from_utf8_lossy(&entry[7..7 + name_end]).trim().to_string();

            // Neighbor callsign (shifted, at offset 13–19)
            let neighbor = if let Some((nb_call, nb_ssid)) = decode_netrom_node(&entry[13..20]) {
                fmt_call(&nb_call, nb_ssid)
            } else {
                String::new()
            };

            // Quality (1 byte at offset 20)
            let quality = entry[20];

            body.push_str(&format!(
                "  {:9} {:<12} {:9} {}\n",
                node_str, name_str, neighbor, quality
            ));
            entry_num += 1;
            pos += NETROM_ENTRY_SIZE;
            if entry_num >= MAX_NETROM_ENTRIES { break; }
        } else {
            pos += 1;
        }
    }

    if entry_num > 0 {
        return Some((body.trim_end().to_string(), "NET/ROM routing".to_string()));
    }

    // ── Broadcast with no route entries (announcement-only) ──
    // When there's a valid broadcast header but no route entries found,
    // just show the node name.
    if is_broadcast && !src_str.is_empty() {
        return Some((
            format!("Node: {}", src_str),
            "NET/ROM announcement".to_string(),
        ));
    }

    // ── Connected NET/ROM Data (PID=0x01) ──
    if info.len() >= 17 {
        return parse_connected_data(info, &src_str);
    }
    None
}

/// Parse a connected NET/ROM data frame (PID=0x01):
/// source_node (7) + dest_node (7) + type(1) + CI(1) + sub_CI(1) + payload.
fn parse_connected_data(info: &[u8], src_str: &str) -> Option<(String, String)> {
    // info[0..7]   = source node (already decoded as src above)
    // info[7..14]  = destination node
    let dest = decode_netrom_node(&info[7..14])?;
    let dest_str = fmt_call(&dest.0, dest.1);

    let node_type = info[14];
    let is_data = node_type == 0x00;

    let (display, header_suffix) = if is_data {
        // Circuit ID at byte 15, sub-CI at byte 16 (always 0x02 for 2-hop route)
        let ci = info[15];
        let payload = if info.len() > 17 {
            format!(" {}", info_to_string(&info[17..]))
        } else {
            String::new()
        };
        (
            format!("{}->{} cct {}/02{}", src_str, dest_str, ci, payload),
            "NET/ROM data".to_string(),
        )
    } else {
        (
            format!("{}->{} type={:02X}", src_str, dest_str, node_type),
            "NET/ROM".to_string(),
        )
    };
    Some((display, header_suffix))
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

    #[test]
    fn test_parse_netrom_too_short() {
        assert!(parse_netrom_frame(&[0u8; 3]).is_none());
    }

    #[test]
    fn test_parse_netrom_connected_data() {
        // Build a minimum connected data frame: 7 src + 7 dest + 1 type + 1 CI + 1 sub_CI
        let mut frame = Vec::new();
        frame.extend_from_slice(&make_node("KK6SEN", 0));
        frame.extend_from_slice(&make_node("KB7XYZ", 5));
        frame.push(0x00); // type = data
        frame.push(0x01); // circuit ID
        frame.push(0x02); // sub-CI (2-hop route)
        frame.extend_from_slice(b"Hello");

        let (display, suffix) = parse_netrom_frame(&frame).unwrap();
        assert_eq!(suffix, "NET/ROM data");
        assert!(display.contains("KK6SEN"));
        assert!(display.contains("KB7XYZ"));
    }

    #[test]
    fn test_decode_netrom_node() {
        let node = make_node("KK6SEN", 0);
        let (call, ssid) = decode_netrom_node(&node).unwrap();
        assert_eq!(call, "KK6SEN");
        assert_eq!(ssid, 0);
    }
}
