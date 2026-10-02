// ── K2K Packet Desktop — link/agw.rs ────────────────────────────────────
//
// Phase 3: a real `Link` implementation for the AGWPE protocol, built over the
// existing pure framing functions in `crate::agw` (build_header / build_frame /
// parse_frame / build_connect / build_register …). Once the engine routes
// through `Link`, the `if protocol == …` farm in commands.rs collapses: AGW is
// just another Link, and its frame routing becomes pure and unit-testable.
//
// Like link/kiss.rs, this adapter knows AGW framing but nothing about AX.25
// session semantics. No `tauri` dependency here.

use crate::agw::{build_frame, build_register, parse_frame};
use crate::link::{Decoded, Incoming, Link, LinkError, Outgoing};

/// An AGWPE protocol link adapter.
///
/// Outbound frames are encoded with `build_frame`; inbound AGW data units are
/// fed via `feed()` and split into `Decoded::Incoming(Incoming::Agw{..})` by
/// `drain()`. Connection lifecycle is a simple state flag at this seam (the
/// AGW register/connect handshake stays in the transport wiring).
pub struct AgwLink {
    host: String,
    port: u16,
    connected: bool,
    callsign: String,
    ssid: u8,
    rx_buffer: Vec<u8>,
    captured_tx: Vec<u8>,
}

impl AgwLink {
    pub fn new(callsign: &str, ssid: u8) -> Self {
        Self {
            host: String::new(),
            port: 0,
            connected: false,
            callsign: callsign.to_string(),
            ssid,
            rx_buffer: Vec::new(),
            captured_tx: Vec::new(),
        }
    }

    /// Feed inbound AGW data bytes into the adapter's RX buffer. Each 36-byte
    /// header + declared payload is one AGW frame.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.rx_buffer.extend_from_slice(bytes);
        self.connected = true;
    }

    /// Build the AGWPE `register` frame for this link's callsign.
    pub fn register_frame(&self) -> Vec<u8> {
        build_register(&self.callsign, self.ssid)
    }

    /// AGW bytes captured by `send_frame` (primarily for test round-trips).
    pub fn captured_tx(&self) -> &[u8] {
        &self.captured_tx
    }
}

impl Link for AgwLink {
    fn connect(&mut self, host: &str, port: u16) -> Result<(), LinkError> {
        self.host = host.to_string();
        self.port = port;
        self.connected = true;
        self.rx_buffer.clear();
        Ok(())
    }

    fn send_frame(&mut self, out: Outgoing) -> Result<(), LinkError> {
        if !self.connected {
            return Err(LinkError::NotConnected);
        }
        // Reuse AGW's raw data frame builder (kind D). The lower layers encode
        // the exact frame; this seam hands it to the transport.
        let framed = build_frame(0, b'D', 0xF0, &self.callsign, &self.callsign, &out.bytes);
        self.captured_tx.extend_from_slice(&framed);
        Ok(())
    }

    fn drain(&mut self) -> Vec<Decoded> {
        let mut out = Vec::new();
        let need = crate::agw::AGW_HEADER_SIZE;
        loop {
            if self.rx_buffer.len() < need {
                break;
            }
            if let Some(frame) = parse_frame(&self.rx_buffer) {
                let total = need + frame.data_len as usize;
                let take = total.min(self.rx_buffer.len());
                self.rx_buffer.drain(..take);
                out.push(Decoded::Incoming(Incoming::Agw {
                    command: frame.kind,
                    data: frame.data,
                }));
            } else {
                // Incomplete/unsupported header — drop one byte and retry.
                self.rx_buffer.drain(..1);
            }
        }
        out
    }

    fn close(&mut self) {
        self.connected = false;
        self.rx_buffer.clear();
        self.captured_tx.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agw::{build_header, AGW_HEADER_SIZE};

    #[test]
    fn register_frame_builds_valid_header() {
        let link = AgwLink::new("KK6SEN", 15);
        let reg = link.register_frame();
        assert_eq!(reg.len(), AGW_HEADER_SIZE);
        // Kind byte at offset 4 should be the register command 'X'.
        assert_eq!(reg[4], crate::agw::CMD_REGISTER);
    }

    #[test]
    fn drain_splits_agw_data_frames() {
        let mut link = AgwLink::new("KK6SEN", 0);
        link.connect("127.0.0.1", 8000).unwrap();
        let payload = b"hello";
        // Build an AGW data frame: header + payload.
        let mut frame = build_header(0, b'D', 0xF0, "KK6SEN", "KJOHN", payload.len() as u32);
        frame.extend_from_slice(payload);
        link.feed(&frame);
        let decoded = link.drain();
        assert_eq!(decoded.len(), 1);
        match &decoded[0] {
            Decoded::Incoming(Incoming::Agw { command, data }) => {
                assert_eq!(*command, b'D');
                assert_eq!(data, payload);
            }
            other => panic!("expected AGW data, got {other:?}"),
        }
    }

    #[test]
    fn not_connected_rejects_send() {
        let mut link = AgwLink::new("KK6SEN", 0);
        let err = link.send_frame(Outgoing { bytes: vec![1] }).unwrap_err();
        assert!(matches!(err, LinkError::NotConnected));
    }

    #[test]
    fn send_frame_buffers_captured_tx() {
        let mut link = AgwLink::new("KK6SEN", 0);
        link.connect("127.0.0.1", 8000).unwrap();
        link.send_frame(Outgoing { bytes: vec![1, 2, 3] }).unwrap();
        assert!(!link.captured_tx.is_empty());
        // First byte is the (LE) port byte; header starts at offset 0.
        assert_eq!(link.captured_tx.len() >= AGW_HEADER_SIZE, true);
    }
}
