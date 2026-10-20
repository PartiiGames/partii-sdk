//! The live room protocol (wire format v1): what the edge says about rooms and
//! their members, and how relayed game packets are framed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version a client sends in `hello`.
pub const PROTOCOL_VERSION: u32 = 1;

/// A relayed packet: `[0x01][to u16 BE][payload]`.
pub const RELAY_FRAME: u8 = 0x01;
/// Several datagrams to one member in one relay frame: `[0x02][to][n]{[len u16 LE][bytes]}`.
/// Every relayed frame is an edge request, so a flush goes out as one.
pub const RELAY_BUNDLE: u8 = 0x02;
/// Send to every other member.
pub const BROADCAST: u16 = 0xffff;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// Derives `Default` so code that builds one can end with `..Default::default()`
/// and keep compiling when fields are added.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub mid: u16,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub meta: Value,
    pub status: String,
    /// The member's game build fingerprint, when it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_version: Option<String>,
}

/// Derives `Default` for the same reason as [`Member`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub code: String,
    pub game: String,
    pub host: u16,
    pub you: u16,
    pub joinable: bool,
    pub max_members: u32,
    /// Per-kind rules the host set, e.g. `{"game": {"joinable": false}, "phone": {"max": 16}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Value>,
    /// The host's reclaim token (only the host has it). If the host's session
    /// times out, creating with `{"reclaim": {"code": code, "token": reclaim}}`
    /// within 10 minutes gets the same code back, so printed QR codes keep working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaim: Option<String>,
    /// The host's game build fingerprint, when it sent one (`net_version` on create).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_version: Option<String>,
    #[serde(default)]
    pub meta: Value,
    pub members: Vec<Member>,
}

/// Is `v` a valid `net_version`: 1–64 characters of `[A-Za-z0-9._:-]`?
/// (A non-negative integer is also accepted on the wire; it's kept as its decimal string.)
pub fn valid_net_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// The error code a join gets when its `net_version` differs from the room's.
pub const VERSION_MISMATCH: &str = "version_mismatch";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooms_round_trip_and_default() {
        let r = Room {
            code: "BKTRM".into(),
            net_version: Some("a1b2".into()),
            members: vec![Member {
                mid: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        let back: Room = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(
            valid_net_version("a1b2c3d4e5f6")
                && !valid_net_version("")
                && !valid_net_version("a b")
        );
    }
}
