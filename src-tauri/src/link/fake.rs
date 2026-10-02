// ── K2K Packet Desktop — link/fake.rs ───────────────────────────────────
//
// Phase 3: an in-memory `Link` implementation used by kernel unit tests.
//
// The design payoff (DESIGN/rewrite.md §3.2): "the kernel tests run against a
// **fake** in-memory Link, so the same AX.25 tests pass regardless of
// transport — which is exactly the confidence that's been missing when a bug
// was 'KISS-only' vs 'AGW-only'." This fake records outbound frames and serves
// a scripted sequence of inbound frames, so a session test can drive a
// connection handshake and assert the produced effects / outbound bytes with
// no socket and no Tauri.
//
// No `tauri` dependency here.

use crate::link::{Decoded, Incoming, Link, LinkError, Outgoing};

/// An in-memory `Link` for tests.
///
/// - `connect()` sets connected without touching the network.
/// - Outbound frames (`send_frame`) are appended to `tx`.
/// - Inbound frames are pushed into `scripted_rx` by the test and served by
///   `drain()` (FIFO).
#[derive(Default)]
pub struct FakeLink {
    host: String,
    port: u16,
    connected: bool,
    /// Outbound frames captured by the test (raw bytes).
    pub tx: Vec<Vec<u8>>,
    /// Inbound frames the test has queued for the kernel to read.
    scripted_rx: Vec<Incoming>,
    /// Closed flag so tests can assert teardown.
    pub closed: bool,
}

impl FakeLink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an inbound raw AX.25 frame for the kernel to drain.
    pub fn push_frame(&mut self, bytes: Vec<u8>) {
        self.scripted_rx.push(Incoming::Frame(bytes));
    }

    /// Queue an inbound AGW data unit for the kernel to drain.
    pub fn push_agw(&mut self, command: u8, data: Vec<u8>) {
        self.scripted_rx.push(Incoming::Agw { command, data });
    }

    /// True once any outbound frame has been sent.
    pub fn has_tx(&self) -> bool {
        !self.tx.is_empty()
    }
}

impl Link for FakeLink {
    fn connect(&mut self, host: &str, port: u16) -> Result<(), LinkError> {
        self.host = host.to_string();
        self.port = port;
        self.connected = true;
        Ok(())
    }

    fn send_frame(&mut self, out: Outgoing) -> Result<(), LinkError> {
        if !self.connected {
            return Err(LinkError::NotConnected);
        }
        self.tx.push(out.bytes);
        Ok(())
    }

    fn drain(&mut self) -> Vec<Decoded> {
        self.scripted_rx
            .drain(..)
            .map(Decoded::Incoming)
            .collect()
    }

    fn close(&mut self) {
        self.connected = false;
        self.closed = true;
        self.scripted_rx.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_tx_in_order() {
        let mut l = FakeLink::new();
        l.connect("h", 1).unwrap();
        l.send_frame(Outgoing { bytes: vec![1] }).unwrap();
        l.send_frame(Outgoing { bytes: vec![2, 3] }).unwrap();
        assert_eq!(l.tx, vec![vec![1], vec![2, 3]]);
    }

    #[test]
    fn serves_scripted_rx_fifo() {
        let mut l = FakeLink::new();
        l.connect("h", 1).unwrap();
        l.push_frame(vec![0xAA]);
        l.push_frame(vec![0xBB, 0xCC]);
        let d = l.drain();
        assert_eq!(d.len(), 2);
        match &d[0] {
            Decoded::Incoming(Incoming::Frame(f)) => assert_eq!(f, &[0xAA]),
            _ => panic!("expected frame"),
        }
        assert!(l.drain().is_empty(), "queue drained");
    }

    #[test]
    fn close_flags_and_clears() {
        let mut l = FakeLink::new();
        l.connect("h", 1).unwrap();
        l.push_frame(vec![1]);
        l.close();
        assert!(l.closed);
        assert!(l.drain().is_empty());
        assert!(matches!(
            l.send_frame(Outgoing { bytes: vec![1] }),
            Err(LinkError::NotConnected)
        ));
    }
}
