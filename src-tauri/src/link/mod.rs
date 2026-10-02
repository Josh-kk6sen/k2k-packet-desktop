// ── K2K Packet Desktop — Link layer (transport adapters) ──────────────
//
// Phase 1 scaffolding: ONE `Link` trait that all transports (KISS, AGW, and a
// test "fake") implement, so the kernel and commands no longer branch on
// `if protocol == …`.
//
// Design intent (from DESIGN/rewrite.md §3.2): the transport is a `seam`. The
// engine drives send_frame/drain; the transports own the socket bytes. Kernel
// unit tests run against a Fake link, proving behaviour is protocol-agnostic.
//
// NOTE: this is scaffolding for the blueprint. In Phase 1 the existing
// kiss.rs / agw.rs still do the real I/O inside commands.rs; wiring them onto
// this trait (and a Fake impl) is a later phase. The trait is defined here now
// so the seam exists and the boundary is explicit.

use std::error::Error;

pub mod agw;
pub mod kiss;
#[cfg(test)]
pub mod fake;


/// One inbound decoded unit the link hands to the engine.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// Raw AX.25 bytes already framed (KISS path).
    Frame(Vec<u8>),
    /// Structured AGWPE protocol message (AGW path).
    Agw { command: u8, data: Vec<u8> },
}

/// Result of a non-blocking read: either an inbound unit or a link event.
#[derive(Debug, Clone)]
pub enum Decoded {
    Incoming(Incoming),
    Connected,
    Disconnected,
}

/// A single outbound frame to transmit.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub bytes: Vec<u8>,
}

/// Errors surfaced by a Transport. Callers convert to the app's Result<String>
/// at the rim (app layer) so the kernel never sees Tauri error types.
#[derive(Debug)]
pub enum LinkError {
    NotConnected,
    Io(std::io::Error),
    Protocol(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::NotConnected => write!(f, "link not connected"),
            LinkError::Io(e) => write!(f, "link i/o: {e}"),
            LinkError::Protocol(s) => write!(f, "link protocol: {s}"),
        }
    }
}
impl Error for LinkError {}

/// Protocol-agnosticism proof (DESIGN/rewrite.md §3.2): the *same* AX.25
/// bytes, encoded and decoded through the KISS Link and the AGW Link, come
/// back identical. A bug that is "KISS-only" or "AGW-only" cannot hide behind
/// the seam — either both transports deliver the frame or neither does.
#[cfg(test)]
mod protocol_agnostic_tests {
    use super::*;
    use crate::link::agw::AgwLink;
    use crate::link::kiss::KissLink;

    // A minimal valid AX.25 frame (UI): dest KK6SEN, src KJOHN, pid F0, "hi".
    fn ax25_ui() -> Vec<u8> {
        crate::kernel::ax25::build_ui("KK6SEN", 0, "KJOHN", 0, b"hi", &[])
    }

    #[test]
    fn same_ax25_frame_survives_both_kiss_and_agw() {
        let bytes = ax25_ui();

        // KISS path
        let mut kiss = KissLink::new();
        kiss.connect("h", 1).unwrap();
        // Simulate TX -> the wire -> RX by re-framing what the adapter would send.
        let mut tx = KissLink::new();
        tx.connect("h", 1).unwrap();
        tx.send_frame(Outgoing { bytes: bytes.clone() }).unwrap();
        kiss.feed(&tx.captured_tx());
        let kiss_decoded = kiss.drain();

        // AGW path
        let mut agw_src = AgwLink::new("KK6SEN", 0);
        agw_src.connect("h", 1).unwrap();
        agw_src.send_frame(Outgoing { bytes: bytes.clone() }).unwrap();
        // AGW data frame: header(36) + payload. Parse out just the AX.25 bytes.
        let mut agw = AgwLink::new("KK6SEN", 0);
        agw.connect("h", 1).unwrap();
        agw.feed(&agw_src.captured_tx());
        let agw_decoded = agw.drain();

        // Both produced one inbound unit.
        assert_eq!(kiss_decoded.len(), 1);
        assert_eq!(agw_decoded.len(), 1);

        // KISS delivers the raw AX.25 frame unchanged.
        match &kiss_decoded[0] {
            Decoded::Incoming(Incoming::Frame(f)) => assert_eq!(f, &bytes),
            other => panic!("kiss should give raw frame, got {other:?}"),
        }
        // AGW carries the payload as a data unit; the payload IS the frame.
        match &agw_decoded[0] {
            Decoded::Incoming(Incoming::Agw { data, .. }) => {
                assert_eq!(data, &bytes, "AGW payload must equal the AX.25 frame")
            }
            other => panic!("agw should give data unit, got {other:?}"),
        }
    }
}


/// The single transport contract that every radio link implements.
pub trait Link: Send {
    /// Connect to the TNC / Direwolf endpoint described by the connection settings.
    fn connect(&mut self, _host: &str, _port: u16) -> Result<(), LinkError>;
    /// Transmit one framed unit.
    fn send_frame(&mut self, out: Outgoing) -> Result<(), LinkError>;
    /// Non-blocking read of any ready inbound decoded units.
    fn drain(&mut self) -> Vec<Decoded>;
    /// Tear the link down.
    fn close(&mut self);

    /// Return the most recent outbound unit (already transport-framed), if the
    /// implementation captured one. Defaults to `None`; the live KISS/AGW
    /// adapters capture framed bytes so the async writer can emit them.
    fn take_last_frame(&mut self) -> Option<Vec<u8>> {
        None
    }
}