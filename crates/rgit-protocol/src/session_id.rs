// SPDX-License-Identifier: MIT
//! The `session-id` capability (`Documentation/gitprotocol-capabilities.adoc`,
//! `transfer.advertiseSID` in `Documentation/config/transfer.adoc`).
//!
//! A server advertises its session id; the client records it as the
//! trace2 value `transfer/server-sid` and, when `transfer.advertiseSID` is
//! set, sends its own session id back in its request. The setting is
//! process wide: [`set_advertise`] is called once the configuration is
//! known.

use std::sync::atomic::{AtomicBool, Ordering};

use rgit_trace::trace2;

use crate::capabilities::{Capabilities, ProtocolVersion};

static ADVERTISE: AtomicBool = AtomicBool::new(false);

/// `transfer.advertiseSID`: send our session id to servers that advertise
/// `session-id`.
pub fn set_advertise(on: bool) {
    ADVERTISE.store(on, Ordering::Relaxed);
}

/// The `session-id=<sid>` capability to request, if the server
/// advertised `session-id` and advertising is on.
pub fn capability(server: &Capabilities) -> Option<String> {
    (ADVERTISE.load(Ordering::Relaxed) && server.contains("session-id"))
        .then(|| format!("session-id={}", trace2::session_id()))
}

/// Record the server's session id (trace2 `transfer/server-sid`).
pub fn note_server(server: &Capabilities) {
    if let Some(sid) = server.session_id() {
        trace2::data("transfer", "server-sid", sid, true);
    }
}

/// Record the protocol version the server answered with (trace2
/// `transfer/negotiated-version`).
pub fn note_version(version: ProtocolVersion) {
    let v = match version {
        ProtocolVersion::V0 => "0",
        ProtocolVersion::V1 => "1",
        ProtocolVersion::V2 => "2",
    };
    trace2::data("transfer", "negotiated-version", v, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_only_when_advertised_and_enabled() {
        let with = Capabilities::parse_v0(b"agent=x session-id=abc");
        let without = Capabilities::parse_v0(b"agent=x");
        set_advertise(false);
        assert_eq!(capability(&with), None);
        set_advertise(true);
        assert_eq!(capability(&without), None);
        let cap = capability(&with).expect("session-id requested");
        let sid = cap.strip_prefix("session-id=").expect("prefix");
        assert!(!sid.is_empty() && !sid.contains(char::is_whitespace));
        // Stable within the process.
        assert_eq!(capability(&with), Some(cap));
        set_advertise(false);
    }
}
