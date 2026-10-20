//! partii-host's datagrams. Every transport (plain UDP, QUIC datagrams,
//! WebTransport datagrams, WebSocket binary messages) carries the same frames:
//! one type byte, then the payload.
//!
//! ```text
//! client → host   JOIN <ticket>   INPUT <bytes>   PING <bytes>   PROBE_ACK <size u16>
//! host → client   STATE <bytes>  PONG <the ping's bytes>        DENIED <reason>   PROBE <size u16>…
//! ```
//!
//! On plain UDP every packet also carries a counter and a tag ([`seal`], [`open`]),
//! because there's no TLS: `frame | counter u32 LE | HMAC-SHA256(key, dir | counter | frame)[..16]`.
//! It authenticates; it does not encrypt.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const JOIN: u8 = 0x00;
pub const PING: u8 = 0x01;
pub const INPUT: u8 = 0x02;
pub const STATE: u8 = 0x03;
pub const PONG: u8 = 0x11;
/// The join was refused; the body says why.
pub const DENIED: u8 = 0x12;
/// Path probing on plain UDP: the host sends `PROBE <size u16>` padded so the
/// whole packet is that size; the client answers `PROBE_ACK <size u16>`.
pub const PROBE: u8 = 0x14;
pub const PROBE_ACK: u8 = 0x15;

/// Frames stay under this many bytes (type byte included).
pub const MAX_FRAME: usize = 1200;

/// Direction byte in a tag: client to host.
pub const TO_HOST: u8 = 0;
/// Direction byte in a tag: host to client.
pub const TO_CLIENT: u8 = 1;
pub const TAG_LEN: usize = 16;
/// Counter plus tag.
pub const TRAILER: usize = 4 + TAG_LEN;

/// Where and how to join one seat, as the jam (or a matchmaker) hands it out over TLS.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    /// `ip:port` of the host's plain UDP listener.
    #[serde(default)]
    pub udp: String,
    /// The signed join ticket (opaque to clients).
    pub ticket: String,
    /// The seat's UDP session key, base64url (32 bytes).
    #[serde(default)]
    pub key: String,
}

/// A seat's info as the game server sees it in `on_join`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JoinInfo {
    #[serde(default)]
    pub name: String,
    /// Which local player on that device (split screen), when there are several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<u32>,
    /// Anything else the game put there.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// HMAC-SHA256 over the concatenation of `parts`.
pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// `frame | counter | tag`, for sending in direction `dir`.
pub fn seal(key: &[u8], dir: u8, counter: u32, frame: &[u8]) -> Vec<u8> {
    let c = counter.to_le_bytes();
    let mut p = Vec::with_capacity(frame.len() + TRAILER);
    p.extend_from_slice(frame);
    p.extend_from_slice(&c);
    p.extend_from_slice(&hmac(key, &[&[dir], &c, frame])[..TAG_LEN]);
    p
}

/// The frame and its counter, when the tag checks out (constant-time compare).
pub fn open<'a>(key: &[u8], dir: u8, packet: &'a [u8]) -> Option<(&'a [u8], u32)> {
    if packet.len() < TRAILER + 1 {
        return None;
    }
    let (frame, trailer) = packet.split_at(packet.len() - TRAILER);
    let want = hmac(key, &[&[dir], &trailer[..4], frame]);
    let diff = want[..TAG_LEN]
        .iter()
        .zip(&trailer[4..])
        .fold(0u8, |d, (a, b)| d | (a ^ b));
    (diff == 0).then(|| (frame, u32::from_le_bytes(trailer[..4].try_into().unwrap())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_and_tags_round_trip() {
        let mac = hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let key = [7u8; 32];
        let p = seal(&key, TO_HOST, 5, b"\x02{}");
        assert_eq!(open(&key, TO_HOST, &p), Some((&b"\x02{}"[..], 5)));
        assert_eq!(open(&key, TO_CLIENT, &p), None);
        let mut bad = p.clone();
        bad[0] ^= 1;
        assert_eq!(open(&key, TO_HOST, &bad), None);
    }

    #[test]
    fn join_info_keeps_extra_fields() {
        let i: JoinInfo =
            serde_json::from_str(r#"{"name":"Sam","local":2,"tankii":{"scenario":"ffa"}}"#)
                .unwrap();
        assert_eq!((i.name.as_str(), i.local), ("Sam", Some(2)));
        assert!(i.extra.contains_key("tankii"));
    }
}
