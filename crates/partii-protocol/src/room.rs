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
    /// The partii player this member's session proved with a `player_token`
    /// (set only by the edge; only the host's views carry it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_id: Option<String>,
    /// While the member is away (`status` is `suspended`): why, and until when
    /// its seat is held. Only engines that opted in (hello `caps: ["away"]`) see it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away: Option<Away>,
}

/// The hello capability that opts an engine in to the away grace
/// (PROTOCOL.md "Away").
pub const AWAY_CAP: &str = "away";
/// A room's away grace when the host sets none (ms).
pub const DEFAULT_AWAY_GRACE_MS: u64 = 30_000;
/// The edge clamps `away_grace_ms` to `MIN_AWAY_GRACE_MS..=MAX_AWAY_GRACE_MS`.
pub const MIN_AWAY_GRACE_MS: u64 = 5_000;
pub const MAX_AWAY_GRACE_MS: u64 = 300_000;

/// Why a member is away: it said so (`"hint"`, an `away` request), or its
/// socket dropped (`"dropped"`).
pub const AWAY_HINT: &str = "hint";
pub const AWAY_DROPPED: &str = "dropped";

/// A member's away hold: `member.away`, and the body of `member_away`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Away {
    /// [`AWAY_HINT`] or [`AWAY_DROPPED`].
    pub reason: String,
    /// Server time (ms since the epoch) when the seat goes: the member leaves
    /// with reason `timeout` unless it is back first.
    pub until: u64,
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
    /// How long an away member keeps its seat (ms; the host sets it with
    /// `away_grace_ms` on create or update, default [`DEFAULT_AWAY_GRACE_MS`]).
    /// Only engines that opted in (hello `caps: ["away"]`) see it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away_grace_ms: Option<u64>,
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
        let old: Room = serde_json::from_str(r#"{"code":"A","game":"g","host":1,"you":1,"joinable":true,"max_members":8,"members":[{"mid":1,"kind":"game","name":"","status":"active"}]}"#).unwrap();
        assert_eq!(old.away_grace_ms, None, "a room from an old edge (or not opted in) parses");
        assert_eq!(old.members[0].away, None);
        assert!(
            valid_net_version("a1b2c3d4e5f6")
                && !valid_net_version("")
                && !valid_net_version("a b")
        );
    }

    #[test]
    fn away_members_and_the_rooms_grace_parse() {
        let r: Room = serde_json::from_str(r#"{"code":"A","game":"g","host":1,"you":2,"joinable":true,"max_members":8,"away_grace_ms":45000,"meta":null,
            "members":[{"mid":1,"kind":"game","name":"H","meta":null,"status":"active"},
                       {"mid":2,"kind":"phone","name":"P","meta":null,"status":"suspended","away":{"reason":"hint","until":1761500000000}}]}"#).unwrap();
        assert_eq!(r.away_grace_ms, Some(45_000));
        assert_eq!(r.members[0].away, None);
        assert_eq!(
            r.members[1].away,
            Some(Away { reason: AWAY_HINT.into(), until: 1_761_500_000_000 })
        );
        let back: Room = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}
