// ── K2K Packet Desktop — link/kiss.rs ───────────────────────────────────
//
// Phase 3: a real `Link` implementation for the KISS protocol, built over the
// existing pure framing functions in `crate::kiss` (kiss_escape / kiss_unescape /
// build_kiss_frame). This makes the transport seam a concrete, testable
// adapter so the engine and the kernel unit tests no longer need to branch on
// `if protocol == …`.
//
// Design intent (DESIGN/rewrite.md §3.2): the kernel drives `send_frame` /
// `drain`; the transport owns socket bytes. The adapter here is kept transport-
// agnostic at the seam: the caller supplies a byte sink for TX and the adapter
// exposes inbound KISS data via `feed()`, which `drain()` decodes into raw
// AX.25 frames.
//
// No `tauri` dependency — this module lives in the link layer (which may sit
// under app/ but must not pull the GUI into the kernel). It only touches
// `crate::kiss` pure functions and `crate::link` types.

use crate::kiss::{build_kiss_frame, kiss_unescape, FEND};
use crate::link::{Decoded, Incoming, Link, LinkError, Outgoing};

/// A KISS protocol link adapter.
///
/// `connect()` records the endpoint (host:port). Outbound frames are wrapped
/// in KISS framing via `build_kiss_frame` and handed to the caller-supplied TX
/// closure; inbound KISS bytes are fed in by the owning reader (`feed`) and
/// decoded back into raw AX.25 frames by `drain()`.
///
/// This is the transport half of the seam: it knows KISS framing but nothing
/// about AX.25 session semantics, so the same adapter serves client and server
/// roles and is trivially unit-testable with no socket.
pub struct KissLink {
    host: String,
    port: u16,
    connected: bool,
    /// Inbound KISS bytes awaiting decode into frames (transport buffer).
    rx_buffer: Vec<u8>,
    /// Optional TX sink; when `None`, `send_frame` buffers instead.
    tx_sink: Option<Box<dyn FnMut(&[u8]) + Send>>,
    /// Outbound KISS bytes captured when no sink is attached (for tests).
    captured_tx: Vec<u8>,
}

impl KissLink {
    pub fn new() -> Self {
        Self {
            host: String::new(),
            port: 0,
            connected: false,
            rx_buffer: Vec::new(),
            tx_sink: None,
            captured_tx: Vec::new(),
        }
    }

    /// Attach a TX sink (e.g. the TcpClient writer). Frames transmitted via
    /// `send_frame` are handed here already KISS-framed.
    pub fn with_tx_sink<F>(mut self, sink: F) -> Self
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        self.tx_sink = Some(Box::new(sink));
        self
    }

    /// Feed raw KISS bytes received from the TNC into the adapter's RX buffer.
    /// `drain()` will decode any complete frames and packetise the rest.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.rx_buffer.extend_from_slice(bytes);
        self.connected = true;
    }

    /// KISS bytes captured by `send_frame` when no TX sink is attached
    /// (primarily for tests: TX -> wire -> RX round-trips).
    pub fn captured_tx(&self) -> &[u8] {
        &self.captured_tx
    }

    /// Decode the buffered KISS bytes into raw AX.25 frames.
    ///
    /// The raw stream is split on `FEND` delimiters; each segment (minus the
    /// leading port/command byte) is unescaped into a raw AX.25 frame.
    fn split_and_decode(&self, buf: &[u8]) -> (Vec<Vec<u8>>, usize) {
        let mut frames = Vec::new();
        let mut seg: Vec<u8> = Vec::new();
        let mut consumed = 0usize;
        // Walk the buffer; FEND delimits frames.
        let mut i = 0usize;
        while i < buf.len() {
            if buf[i] == FEND {
                // A segment (between FENDs) is a KISS frame: drop its leading
                // port/command byte, unescape the rest.
                if !seg.is_empty() {
                    if seg.len() >= 1 {
                        let payload = &seg[1..]; // skip port/command byte
                        let frame = kiss_unescape(payload);
                        if !frame.is_empty() {
                            frames.push(frame);
                        }
                    }
                    seg.clear();
                }
                consumed = i + 1;
            } else {
                seg.push(buf[i]);
            }
            i += 1;
        }
        // Any trailing partial segment without a closing FEND stays buffered.
        (frames, consumed)
    }
}

impl Link for KissLink {
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
        let framed = build_kiss_frame(0, &out.bytes);
        match &mut self.tx_sink {
            Some(sink) => sink(&framed),
            None => self.captured_tx = framed,
        }
        Ok(())
    }

    fn take_last_frame(&mut self) -> Option<Vec<u8>> {
        if self.captured_tx.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.captured_tx))
        }
    }

    fn drain(&mut self) -> Vec<Decoded> {
        let mut out = Vec::new();
        if self.rx_buffer.is_empty() {
            return out;
        }
        let (frames, consumed) = self.split_and_decode(&self.rx_buffer);
        self.rx_buffer.drain(..consumed);
        for f in frames {
            out.push(Decoded::Incoming(Incoming::Frame(f)));
        }
        out
    }

    fn close(&mut self) {
        self.connected = false;
        self.rx_buffer.clear();
        self.captured_tx.clear();
    }
}

impl Default for KissLink {
    fn default() -> Self {
        Self::new()
    }
}

// Ensure boxed TX sink is Send-safe for the trait (Link: Send).
#[allow(dead_code)]
fn _assert_send<T: Send>() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiss::kiss_escape;

    fn raw_ax25() -> Vec<u8> {
        // A plausible minimal AX.25 frame (not necessarily valid, just bytes
        // that must survive KISS framing): dest + src + control.
        vec![0x82, 0xA0, 0xE6, 0xAE, 0x92, 0x40, 0x40, 0x40, 0x60, 0x00, 0xF0, b'H', b'i']
    }

    #[test]
    fn send_frame_produces_kiss_framed_bytes() {
        let mut link = KissLink::new();
        link.connect("127.0.0.1", 8001).unwrap();
        let bytes = raw_ax25();
        link.send_frame(Outgoing { bytes: bytes.clone() }).unwrap();
        // captured_tx holds FEND-wrapped, escaped bytes.
        assert!(!link.captured_tx.is_empty());
        assert_eq!(link.captured_tx[0], FEND);
        assert_eq!(*link.captured_tx.last().unwrap(), FEND);
    }

    #[test]
    fn feed_and_drain_roundtrips_frame() {
        let mut link = KissLink::new();
        link.connect("127.0.0.1", 8001).unwrap();
        let bytes = raw_ax25();
        // Build a KISS frame the way the wire would carry it.
        let framed = build_kiss_frame(0, &bytes);
        link.feed(&framed);
        let decoded = link.drain();
        assert_eq!(decoded.len(), 1);
        match &decoded[0] {
            Decoded::Incoming(Incoming::Frame(f)) => assert_eq!(f, &bytes),
            other => panic!("expected raw frame, got {other:?}"),
        }
    }

    #[test]
    fn partial_segment_waits_for_fend() {
        let mut link = KissLink::new();
        link.connect("127.0.0.1", 8001).unwrap();
        let bytes = raw_ax25();
        let framed = build_kiss_frame(0, &bytes);
        // Feed only up to (but not including) the closing FEND.
        link.feed(&framed[..framed.len() - 1]);
        assert!(link.drain().is_empty(), "no closing FEND -> no frame yet");
        // Now feed the closing FEND.
        link.feed(&framed[framed.len() - 1..]);
        let decoded = link.drain();
        assert_eq!(decoded.len(), 1);
    }

    #[test]
    fn escape_roundtrip_matches_kiss() {
        // The adapter reuses crate::kiss::kiss_escape/unescape — sanity check.
        let b = [0xC0u8, 0xDB, b'A'];
        let esc = kiss_escape(&b);
        assert_eq!(kiss_unescape(&esc), b);
    }

    #[test]
    fn not_connected_rejects_send() {
        let mut link = KissLink::new();
        let err = link.send_frame(Outgoing { bytes: vec![1, 2, 3] }).unwrap_err();
        assert!(matches!(err, LinkError::NotConnected));
    }

    #[test]
    fn with_tx_sink_forwards_framed_bytes() {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut link = KissLink::new();
        link = {
            let sent = sent.clone();
            link.with_tx_sink(move |b| sent.lock().unwrap().extend_from_slice(b))
        };
        link.connect("h", 1).unwrap();
        let bytes = raw_ax25();
        link.send_frame(Outgoing { bytes }).unwrap();
        let got = sent.lock().unwrap();
        assert!(got.len() > 4, "sink received framed bytes");
        assert_eq!(got[0], FEND);
    }
}
