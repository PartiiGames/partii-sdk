//! The engine core: session protocol, room state, one `Link` per member, and
//! the WebRTC lifecycle for each link's `turn` and `direct` paths. It does no
//! I/O. Drivers (native: tokio + tungstenite + webrtc-rs; web: web-sys)
//! feed it what happened and carry out the `Action`s it returns.

use crate::link::{Link, LinkEvent, PathId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};

/// How often the driver should call `tick` (ms).
pub const TICK_MS: f64 = 10.0;
/// Keepalive: send "ping" when the socket has been quiet this long…
const SOCK_PING_MS: f64 = 1000.0;
/// …and give the socket up when nothing has arrived for this long (a phone
/// that changed networks leaves a socket that never errors).
const SOCK_DEAD_MS: f64 = 2500.0;
const RECONNECT_BACKOFF_MS: [f64; 6] = [0.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0];
const PC_RETRY_MS: [f64; 5] = [1000.0, 2000.0, 4000.0, 8000.0, 10000.0];
/// A WebRTC path that has not opened by now is rebuilt.
const PC_OPEN_TIMEOUT_MS: f64 = 12_000.0;
pub use partii_protocol::room::{BROADCAST, RELAY_BUNDLE, RELAY_FRAME};

/// Fields left out of a JSON config (the web engine's) take their defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Edge base URL, e.g. `https://edge.partii.dev` (ws scheme derived).
    pub edge: String,
    /// Publishable key.
    pub key: String,
    /// Random per launch; reused on reconnect to resume.
    pub conn: String,
    /// Free-form client tag (`game/version platform`).
    pub client: String,
    /// Optional player token (signed-in player).
    pub player_token: Option<String>,
    /// Skip WebRTC paths (ws relay only); for tests and locked-down hosts.
    pub no_webrtc: bool,
    /// Opt in to the away grace (PROTOCOL.md "Away"); off by default, so an
    /// engine behaves exactly as before. On, the engine says `caps: ["away"]`
    /// in its hello: a dropped socket (a phone frozen in the background)
    /// keeps its seat for the room's `away_grace_ms` (default 30 s) instead
    /// of 20 s, others' presence arrives as [`Event::MemberAway`] /
    /// [`Event::MemberBack`] instead of [`Event::MemberSuspended`] /
    /// [`Event::MemberResumed`], and [`Core::away`] / [`Core::back`] work
    /// (off, they answer `Failed { code: "not_enabled" }` and send nothing).
    pub away: bool,
}

pub use partii_protocol::room::{Away, IceServer, Member, Room, AWAY_CAP};

/// What the game sees.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// The session socket is up (first time or resumed).
    Online {
        resumed: bool,
        relay: String,
    },
    /// The session socket dropped; the engine is reconnecting.
    Offline,
    Created {
        req: u32,
        room: Room,
    },
    Joined {
        req: u32,
        room: Room,
    },
    Failed {
        req: u32,
        code: String,
        message: String,
    },
    /// Comes right before the `Failed` for request `req` when partii refused it
    /// for being busy (`rate_limited`): wait this long before trying again.
    RateLimited {
        req: u32,
        retry_after_ms: u64,
    },
    MemberJoined {
        member: Member,
    },
    /// A member changed its own name or meta (`member_update`).
    MemberUpdated {
        member: Member,
    },
    MemberLeft {
        mid: u16,
        reason: String,
    },
    /// A member's socket dropped; its seat is held for a short grace. (What an
    /// engine that didn't opt in to the away grace hears, or any engine on an
    /// edge from before it; opted-in engines get [`Event::MemberAway`].)
    MemberSuspended {
        mid: u16,
    },
    /// A suspended member is back in its seat (old-style; see [`Event::MemberBack`]).
    MemberResumed {
        mid: u16,
    },
    /// A member is away (PROTOCOL.md "Away"): its app went to the background
    /// (`reason: "hint"`) or its socket dropped (`"dropped"`). It keeps its
    /// seat (still in `room.members`, status `suspended`) until `until`
    /// (server time, ms since the epoch; `grace_ms` is the hold's length) and
    /// then leaves with `MemberLeft { reason: "timeout" }` unless it is back
    /// first. Packets to it are dropped by the edge meanwhile; reliable ones
    /// are resent once it's back. Show it as "away", don't free its seat.
    MemberAway {
        mid: u16,
        reason: String,
        until: u64,
        grace_ms: u64,
    },
    /// An away member is back in its seat; its links come back up.
    MemberBack {
        mid: u16,
    },
    /// The edge took this engine's [`Core::away`]: its seat is held until
    /// `until` (server ms; `grace_ms` long). If it isn't back by then (a
    /// resume with the same `conn`, or [`Core::back`]), it leaves the room:
    /// with its socket still up it gets `RoomClosed { reason: "timeout" }`.
    AwayHeld {
        req: u32,
        reason: String,
        until: u64,
        grace_ms: u64,
    },
    RoomUpdated {
        room: Room,
    },
    /// A new engine (say, a reloaded web page that kept its `conn`) came back
    /// into the room its session was in: the same seat, nothing to join.
    Resumed {
        room: Room,
    },
    /// This engine is out of the room. `reason`: `ended`, `host_lost`,
    /// `kicked`, `version_mismatch`, `expired` (the session was forgotten
    /// while offline) or `timeout` (it said `away` and wasn't back in time,
    /// its socket still up: the seat went; join again for a new one).
    RoomClosed {
        reason: String,
    },
    /// The first usable path to this member is up.
    PeerConnected {
        mid: u16,
        path: PathId,
    },
    PathChanged {
        mid: u16,
        from: PathId,
        to: PathId,
        rtt: Option<f64>,
    },
    Message {
        from: u16,
        reliable: bool,
        bytes: Vec<u8>,
    },
    Error {
        message: String,
    },
}

/// What the driver must do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Open the session socket to this URL (resume if `conn` was used before).
    Connect {
        url: String,
    },
    /// Send a JSON text frame on the session socket.
    SendText(String),
    /// Send a binary relay frame on the session socket.
    SendBinary(Vec<u8>),
    /// Build a peer connection. `relay_only` sets the ICE policy to relay.
    CreatePc {
        mid: u16,
        path: PathId,
        gen: u32,
        offerer: bool,
        ice: Vec<IceServer>,
        relay_only: bool,
    },
    /// Hand a remote signal to the peer connection.
    PcSignal {
        mid: u16,
        path: PathId,
        gen: u32,
        signal: Value,
    },
    ClosePc {
        mid: u16,
        path: PathId,
    },
    /// Send a datagram on a peer connection's data channel.
    PcSend {
        mid: u16,
        path: PathId,
        bytes: Vec<u8>,
    },
    Emit(Event),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PcPhase {
    Connecting { since: f64 },
    Open,
    Waiting { retry_at: f64 },
}

#[derive(Debug, Clone)]
struct Pc {
    gen: u32,
    phase: PcPhase,
    failures: usize,
}

struct Peer {
    link: Link,
    connected: bool,
    pcs: BTreeMap<PathId, Pc>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Req {
    Create,
    Join,
    Away,
    Other,
}

enum Sock {
    Down { retry_at: f64 },
    Connecting,
    Hello,
    Up,
}

/// How long a create/join made before the socket is up waits for it.
pub const OFFLINE_QUEUE_MS: f64 = 10_000.0;

pub struct Core {
    cfg: Config,
    sock: Sock,
    ever_up: bool,
    /// Failed connects since the socket was last up (drives the backoff).
    attempt: usize,
    ice: Vec<IceServer>,
    relay: String,
    room: Option<Room>,
    peers: BTreeMap<u16, Peer>,
    next_req: u32,
    /// Creates/joins asked for before the socket was up: (id, message,
    /// first tick seen). Sent on `Online`; failed `offline` after
    /// `OFFLINE_QUEUE_MS`.
    queued: Vec<(u32, Value, Option<f64>)>,
    reqs: BTreeMap<u32, Req>,
    out: VecDeque<Action>,
    last_rx: f64,
    last_ping: f64,
    /// This engine's own away hold (an `away` the edge took), until it's back.
    away: Option<Away>,
}

impl Core {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            sock: Sock::Down { retry_at: 0.0 },
            ever_up: false,
            attempt: 0,
            ice: Vec::new(),
            relay: "ws".into(),
            room: None,
            peers: BTreeMap::new(),
            next_req: 1,
            queued: Vec::new(),
            reqs: BTreeMap::new(),
            out: VecDeque::new(),
            last_rx: 0.0,
            last_ping: 0.0,
            away: None,
        }
    }

    pub fn room(&self) -> Option<&Room> {
        self.room.as_ref()
    }
    pub fn me(&self) -> Option<u16> {
        self.room.as_ref().map(|r| r.you)
    }
    pub fn link(&self, mid: u16) -> Option<&Link> {
        self.peers.get(&mid).map(|p| &p.link)
    }
    pub fn online(&self) -> bool {
        matches!(self.sock, Sock::Up)
    }
    /// This engine's own away hold, between an `away` the edge took
    /// ([`Event::AwayHeld`]) and being back (`back`, a resume, or leaving).
    pub fn away_hold(&self) -> Option<&Away> {
        self.away.as_ref()
    }

    /// Per-member link state: the path in use and what each path measures.
    pub fn stats(&self) -> Value {
        let peers: serde_json::Map<String, Value> = self
            .peers
            .iter()
            .map(|(mid, p)| {
                let paths: serde_json::Map<String, Value> = PathId::ALL
                    .iter()
                    .map(|id| {
                        let s = p.link.path(*id);
                        (id.name().to_string(), json!({"open": s.open, "rtt": s.rtt, "pings": s.pings, "pongs": s.pongs}))
                    })
                    .collect();
                (mid.to_string(), json!({"current": p.link.current(), "retransmits": p.link.retransmits(), "paths": paths}))
            })
            .collect();
        json!({"online": self.online(), "relay": self.relay, "peers": peers})
    }

    /// Everything the driver should do now.
    pub fn drain(&mut self) -> Vec<Action> {
        self.out.drain(..).collect()
    }

    fn emit(&mut self, e: Event) {
        self.out.push_back(Action::Emit(e));
    }
    fn text(&mut self, v: Value) {
        self.out.push_back(Action::SendText(v.to_string()));
    }

    pub fn url(&self) -> String {
        let base = self.cfg.edge.trim_end_matches('/');
        let ws = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_string()
        };
        format!("{ws}/v1/connect?conn={}", self.cfg.conn)
    }

    // ── Requests from the game ──

    fn request(&mut self, kind: Req, mut v: Value) -> u32 {
        let id = self.next_req;
        self.next_req += 1;
        self.reqs.insert(id, kind);
        v["id"] = json!(id);
        if self.online() {
            self.text(v);
        } else if matches!(kind, Req::Create | Req::Join) {
            // Games call create/join from their first frame: hold it until
            // the socket is up rather than failing.
            self.queued.push((id, v, None));
        } else {
            self.reqs.remove(&id);
            self.emit(Event::Failed {
                req: id,
                code: "offline".into(),
                message: "not connected to partii yet".into(),
            });
        }
        id
    }

    /// Send held creates/joins (just came online) or fail the stale ones.
    fn pump_queued(&mut self, now: Option<f64>) {
        if self.queued.is_empty() {
            return;
        }
        if self.online() {
            for (_, v, _) in std::mem::take(&mut self.queued) {
                self.text(v);
            }
            return;
        }
        let Some(now) = now else { return };
        let mut keep = Vec::new();
        for (id, v, since) in std::mem::take(&mut self.queued) {
            let since = since.unwrap_or(now);
            if now - since > OFFLINE_QUEUE_MS {
                self.reqs.remove(&id);
                self.emit(Event::Failed {
                    req: id,
                    code: "offline".into(),
                    message: "could not reach partii".into(),
                });
            } else {
                keep.push((id, v, Some(since)));
            }
        }
        self.queued = keep;
    }

    pub fn create(
        &mut self,
        game: &str,
        max_members: Option<u32>,
        kind: &str,
        name: &str,
        meta: Value,
    ) -> u32 {
        self.create_with(game, kind, name, meta, json!({"max_members": max_members}))
    }
    /// `create` with room settings from the start: `{"max_members": 32, "kinds": {"game": {"joinable": false}}}`.
    /// `away_grace_ms` (5000–300000, default 30000) sets how long an away member keeps its seat.
    pub fn create_with(
        &mut self,
        game: &str,
        kind: &str,
        name: &str,
        meta: Value,
        settings: Value,
    ) -> u32 {
        let mut v = json!({"t": "create", "game": game, "kind": kind, "name": name, "meta": meta});
        if let Value::Object(o) = settings {
            for (k, x) in o {
                if !matches!(k.as_str(), "t" | "game" | "kind" | "name" | "meta") {
                    v[k] = x;
                }
            }
        }
        self.request(Req::Create, v)
    }
    pub fn join(&mut self, code: &str, kind: &str, name: &str, meta: Value) -> u32 {
        self.join_with(code, kind, name, meta, Value::Null)
    }
    /// `join` with extra fields, e.g. `{"net_version": "a1b2c3d4e5f6"}`: a room on another
    /// build answers `Failed { code: "version_mismatch", .. }` (tell the player to update).
    pub fn join_with(
        &mut self,
        code: &str,
        kind: &str,
        name: &str,
        meta: Value,
        settings: Value,
    ) -> u32 {
        let mut v = json!({"t": "join", "code": code, "kind": kind, "name": name, "meta": meta});
        if let Value::Object(o) = settings {
            for (k, x) in o {
                if !matches!(k.as_str(), "t" | "code" | "kind" | "name" | "meta") {
                    v[k] = x;
                }
            }
        }
        self.request(Req::Join, v)
    }
    pub fn leave(&mut self) -> u32 {
        let id = self.request(Req::Other, json!({"t": "leave"}));
        self.left_room();
        id
    }
    /// Host only: change the room, e.g. `{"joinable": false}` or
    /// `{"away_grace_ms": 60000}` (holds already running keep their end).
    pub fn update(&mut self, patch: Value) -> u32 {
        let mut v = patch;
        v["t"] = json!("update");
        self.request(Req::Other, v)
    }
    pub fn kick(&mut self, mid: u16) -> u32 {
        self.request(Req::Other, json!({"t": "kick", "mid": mid}))
    }
    /// Change your own member name and/or meta; the others get `MemberUpdated`.
    pub fn member_update(&mut self, name: Option<&str>, meta: Option<Value>) -> u32 {
        let mut v = json!({"t": "member_update"});
        if let Some(n) = name {
            v["name"] = json!(n);
        }
        if let Some(m) = meta {
            v["meta"] = m;
        }
        self.request(Req::Other, v)
    }
    /// The app is going to the background (PROTOCOL.md "Away"): hold this
    /// engine's seat for the room's `away_grace_ms`. The reply is
    /// [`Event::AwayHeld`] (or `Failed`: `not_in_room`, or `offline`, which is
    /// harmless: a dropped socket is away on its own). The socket may drop
    /// any time after; reconnecting with the same `conn` is back on its own.
    /// Saying it twice changes nothing. Needs [`Config::away`]; without it
    /// this sends nothing and answers `Failed { code: "not_enabled" }`.
    pub fn away(&mut self) -> u32 {
        if !self.cfg.away {
            return self.not_enabled();
        }
        self.request(Req::Away, json!({"t": "away"}))
    }
    /// Back in the foreground on a socket that never dropped: the others get
    /// `MemberBack`. A no-op when not away. `Failed` only for `not_in_room`
    /// (the seat already went; see `RoomClosed { reason: "timeout" }`) or
    /// `offline` (harmless: the reconnect is back on its own). Needs
    /// [`Config::away`], like `away`.
    pub fn back(&mut self) -> u32 {
        if !self.cfg.away {
            return self.not_enabled();
        }
        self.away = None;
        self.request(Req::Other, json!({"t": "back"}))
    }
    /// A request this engine wasn't configured for: a request id that fails at once, nothing sent.
    fn not_enabled(&mut self) -> u32 {
        let id = self.next_req;
        self.next_req += 1;
        self.emit(Event::Failed {
            req: id,
            code: "not_enabled".into(),
            message: "the away grace is off: start the engine with Config { away: true, .. }".into(),
        });
        id
    }
    pub fn report(&mut self, metrics: Value) {
        if self.online() {
            self.text(json!({"t": "report", "metrics": metrics}));
        }
    }

    /// Queue for one member (or `BROADCAST`) without flushing; `flush`
    /// sends everything queued, bundled per member.
    pub fn queue(&mut self, to: u16, reliable: bool, bytes: &[u8]) {
        let targets: Vec<u16> = if to == BROADCAST {
            self.peers.keys().copied().collect()
        } else {
            vec![to]
        };
        for mid in targets {
            if let Some(p) = self.peers.get_mut(&mid) {
                p.link.send(reliable, bytes);
            }
        }
    }

    pub fn flush(&mut self, now: f64) {
        let mids: Vec<u16> = self.peers.keys().copied().collect();
        for mid in mids {
            self.flush_link(mid, now);
        }
    }

    /// Send to one member (or `BROADCAST`) now.
    pub fn send(&mut self, to: u16, reliable: bool, bytes: &[u8], now: f64) {
        let targets: Vec<u16> = if to == BROADCAST {
            self.peers.keys().copied().collect()
        } else {
            vec![to]
        };
        for mid in targets {
            if let Some(p) = self.peers.get_mut(&mid) {
                p.link.send(reliable, bytes);
            }
            self.flush_link(mid, now);
        }
    }

    // ── Driver input ──

    pub fn tick(&mut self, now: f64) {
        self.pump_queued(Some(now));
        if matches!(self.sock, Sock::Up | Sock::Hello) {
            if now - self.last_rx > SOCK_DEAD_MS {
                // Silent socket: drop it and reconnect (the session resumes).
                self.on_socket_closed(now);
            } else if now - self.last_rx > SOCK_PING_MS && now - self.last_ping > SOCK_PING_MS {
                self.last_ping = now;
                self.out.push_back(Action::SendText("ping".into()));
            }
        }
        if let Sock::Down { retry_at } = self.sock {
            if now >= retry_at {
                self.sock = Sock::Connecting;
                let url = self.url();
                self.out.push_back(Action::Connect { url });
            }
        }
        let mids: Vec<u16> = self.peers.keys().copied().collect();
        for mid in mids {
            self.pc_timers(mid, now);
            self.flush_link(mid, now);
        }
    }

    pub fn on_socket_open(&mut self, now: f64) {
        self.sock = Sock::Hello;
        self.last_rx = now;
        // `fresh`: this engine has never been welcomed, so if its session is
        // an old one (a reloaded page), its links start over. `caps`: it can
        // restart its own link to a member that comes back fresh.
        // `away`: hold its seat for the room's away grace and tell it about
        // others with member_away / member_back (only with `Config::away`).
        let caps: &[&str] = if self.cfg.away {
            &["reset", AWAY_CAP]
        } else {
            &["reset"]
        };
        let mut hello = json!({"t": "hello", "v": 1, "key": self.cfg.key, "client": self.cfg.client, "caps": caps, "fresh": !self.ever_up});
        if let Some(t) = &self.cfg.player_token {
            hello["player_token"] = json!(t);
        }
        self.text(hello);
    }

    pub fn on_socket_closed(&mut self, now: f64) {
        let was_up = matches!(self.sock, Sock::Up);
        let attempt = if was_up { 0 } else { self.attempt + 1 };
        self.attempt = attempt;
        let wait = RECONNECT_BACKOFF_MS[attempt.min(RECONNECT_BACKOFF_MS.len() - 1)];
        self.sock = Sock::Down {
            retry_at: now + wait,
        };
        for (_, p) in self.peers.iter_mut() {
            p.link.set_open(PathId::Ws, false, now);
        }
        // Requests in flight on the dead socket will never be answered.
        for (id, _) in std::mem::take(&mut self.reqs) {
            self.emit(Event::Failed {
                req: id,
                code: "offline".into(),
                message: "connection dropped".into(),
            });
        }
        if was_up {
            self.emit(Event::Offline);
        }
    }

    pub fn on_text(&mut self, text: &str, now: f64) {
        self.last_rx = now;
        if text == "pong" {
            return;
        }
        let Ok(m) = serde_json::from_str::<Value>(text) else {
            return;
        };
        match m["t"].as_str().unwrap_or("") {
            "welcome" => self.welcome(&m, now),
            "ok" | "err" => self.reply(&m, now),
            "member_joined" => {
                if let Ok(member) = serde_json::from_value::<Member>(m["member"].clone()) {
                    if let Some(r) = self.room.as_mut() {
                        r.members.retain(|x| x.mid != member.mid);
                        r.members.push(member.clone());
                    }
                    let mid = member.mid;
                    self.emit(Event::MemberJoined { member });
                    self.add_peer(mid, now);
                }
            }
            "member_updated" => {
                if let Ok(member) = serde_json::from_value::<Member>(m["member"].clone()) {
                    if let Some(r) = self.room.as_mut() {
                        if let Some(x) = r.members.iter_mut().find(|x| x.mid == member.mid) {
                            *x = member.clone();
                        }
                    }
                    self.emit(Event::MemberUpdated { member });
                }
            }
            "member_left" => {
                let mid = m["mid"].as_u64().unwrap_or(0) as u16;
                self.drop_peer(mid);
                if let Some(r) = self.room.as_mut() {
                    r.members.retain(|x| x.mid != mid);
                }
                self.emit(Event::MemberLeft {
                    mid,
                    reason: m["reason"].as_str().unwrap_or("left").into(),
                });
            }
            "member_suspended" | "member_resumed" | "member_away" | "member_back" => {
                self.presence(&m, now)
            }
            "room_updated" => {
                if let Ok(mut room) = serde_json::from_value::<Room>(m["room"].clone()) {
                    room.reclaim = self.room.as_ref().and_then(|r| r.reclaim.clone());
                    self.room = Some(room.clone());
                    self.emit(Event::RoomUpdated { room });
                }
            }
            "room_closed" => {
                self.left_room();
                self.emit(Event::RoomClosed {
                    reason: m["reason"].as_str().unwrap_or("ended").into(),
                });
            }
            "signal" => {
                let from = m["from"].as_u64().unwrap_or(0) as u16;
                self.on_signal(from, &m["data"], now);
            }
            "ice" => {
                if let Ok(ice) = serde_json::from_value::<Vec<IceServer>>(m["ice_servers"].clone())
                {
                    self.ice = ice;
                }
                self.relay = m["relay"].as_str().unwrap_or("ws").into();
            }
            _ => {}
        }
    }

    fn online_now(&self) -> bool {
        matches!(self.sock, Sock::Up)
    }

    /// A member is away or back: `member_away` / `member_back` (we opted in)
    /// or `member_suspended` / `member_resumed` (an old edge, or `Config::away` off).
    fn presence(&mut self, m: &Value, now: f64) {
        let t = m["t"].as_str().unwrap_or("");
        let mid = m["mid"].as_u64().unwrap_or(0) as u16;
        let gone = matches!(t, "member_suspended" | "member_away");
        if !gone && m["reset"] == true && self.peers.contains_key(&mid) {
            // It came back as a new engine: its link starts from zero, so ours does too.
            self.drop_peer(mid);
            self.add_peer(mid, now);
        }
        let away = (t == "member_away").then(|| Away {
            reason: if m["reason"] == "hint" { "hint" } else { "dropped" }.into(),
            until: m["until"].as_f64().map_or(0, |x| x.max(0.0) as u64),
        });
        let default_grace = self
            .room
            .as_ref()
            .and_then(|r| r.away_grace_ms)
            .unwrap_or(partii_protocol::room::DEFAULT_AWAY_GRACE_MS);
        if let Some(r) = self.room.as_mut() {
            if let Some(x) = r.members.iter_mut().find(|x| x.mid == mid) {
                x.status = if gone { "suspended" } else { "active" }.into();
                if !gone {
                    x.away = None;
                } else if away.is_some() {
                    x.away = away.clone();
                }
            }
        }
        // Its relay socket is down (or back): the edge drops packets to an away
        // member, so the ws path closes and reliable sends wait for its return;
        // WebRTC paths judge themselves (and restart when it's back).
        let open = !gone && self.online_now();
        if let Some(p) = self.peers.get_mut(&mid) {
            p.link.set_open(PathId::Ws, open, now);
        }
        if !gone {
            self.kick_unopened(mid, now);
        }
        let ev = match (t, away) {
            ("member_away", Some(a)) => Event::MemberAway {
                mid,
                reason: a.reason,
                until: a.until,
                grace_ms: m["grace_ms"].as_u64().unwrap_or(default_grace),
            },
            ("member_back", _) => Event::MemberBack { mid },
            ("member_suspended", _) => Event::MemberSuspended { mid },
            _ => Event::MemberResumed { mid },
        };
        self.emit(ev);
    }

    pub fn on_binary(&mut self, b: &[u8], now: f64) {
        self.last_rx = now;
        if b.len() < 3 || (b[0] != RELAY_FRAME && b[0] != RELAY_BUNDLE) {
            return;
        }
        let from = u16::from_be_bytes([b[1], b[2]]);
        let datagrams: Vec<&[u8]> = if b[0] == RELAY_FRAME {
            vec![&b[3..]]
        } else {
            let mut v = Vec::new();
            let n = *b.get(3).unwrap_or(&0) as usize;
            let mut i = 4;
            for _ in 0..n {
                let Some(len) = b
                    .get(i..i + 2)
                    .map(|l| u16::from_le_bytes([l[0], l[1]]) as usize)
                else {
                    break;
                };
                let Some(d) = b.get(i + 2..i + 2 + len) else {
                    break;
                };
                v.push(d);
                i += 2 + len;
            }
            v
        };
        if let Some(p) = self.peers.get_mut(&from) {
            if !p.link.path(PathId::Ws).open {
                p.link.set_open(PathId::Ws, true, now);
            }
            for d in datagrams {
                p.link.on_datagram(PathId::Ws, d, now);
            }
        }
        self.flush_link(from, now);
    }

    /// The driver's peer connection produced a signal to send.
    pub fn on_pc_signal(&mut self, mid: u16, path: PathId, gen: u32, mut signal: Value) {
        signal["pc"] = json!(path.name());
        signal["gen"] = json!(gen);
        if self.online() {
            self.text(json!({"t": "signal", "to": mid, "data": signal}));
        }
    }

    pub fn on_pc_open(&mut self, mid: u16, path: PathId, gen: u32, now: f64) {
        let Some(p) = self.peers.get_mut(&mid) else {
            return;
        };
        let Some(pc) = p.pcs.get_mut(&path) else {
            return;
        };
        if pc.gen != gen {
            return;
        }
        pc.phase = PcPhase::Open;
        pc.failures = 0;
        p.link.set_open(path, true, now);
        self.flush_link(mid, now);
    }

    pub fn on_pc_closed(&mut self, mid: u16, path: PathId, gen: u32, now: f64) {
        let Some(p) = self.peers.get_mut(&mid) else {
            return;
        };
        let Some(pc) = p.pcs.get_mut(&path) else {
            return;
        };
        if pc.gen != gen {
            return;
        }
        p.link.set_open(path, false, now);
        let wait = PC_RETRY_MS[pc.failures.min(PC_RETRY_MS.len() - 1)];
        pc.failures += 1;
        pc.phase = PcPhase::Waiting {
            retry_at: now + wait,
        };
        self.out.push_back(Action::ClosePc { mid, path });
        self.flush_link(mid, now);
    }

    pub fn on_pc_data(&mut self, mid: u16, path: PathId, bytes: &[u8], now: f64) {
        if let Some(p) = self.peers.get_mut(&mid) {
            p.link.on_datagram(path, bytes, now);
        }
        self.flush_link(mid, now);
    }

    // ── Internals ──

    fn welcome(&mut self, m: &Value, now: f64) {
        self.sock = Sock::Up;
        self.ever_up = true;
        self.attempt = 0;
        // A welcome ends any away hold: a resume is back in the seat, and
        // anything else means the seat is gone.
        self.away = None;
        self.ice = serde_json::from_value(m["ice_servers"].clone()).unwrap_or_default();
        self.relay = m["relay"].as_str().unwrap_or("ws").into();
        let resumed = m["resumed"].as_bool().unwrap_or(false);
        if resumed {
            if let Ok(room) = serde_json::from_value::<Room>(m["room"].clone()) {
                if self.room.is_none() {
                    // A fresh engine on an old session: learn the room and its members.
                    self.room = Some(room.clone());
                    let others: Vec<u16> = room
                        .members
                        .iter()
                        .filter(|x| x.mid != room.you)
                        .map(|x| x.mid)
                        .collect();
                    self.emit(Event::Resumed { room });
                    for mid in others {
                        self.add_peer(mid, now);
                    }
                    let relay = self.relay.clone();
                    self.emit(Event::Online { resumed, relay });
                    self.pump_queued(None);
                    return;
                }
                self.room = Some(room);
                for (_, p) in self.peers.iter_mut() {
                    p.link.set_open(PathId::Ws, true, now);
                }
                for mid in self.peers.keys().copied().collect::<Vec<_>>() {
                    self.kick_unopened(mid, now);
                }
            } else if self.room.is_some() {
                self.left_room();
                self.emit(Event::RoomClosed {
                    reason: "expired".into(),
                });
            }
        } else if self.room.is_some() {
            // The edge forgot us (grace ran out): the room is gone for us.
            self.left_room();
            self.emit(Event::RoomClosed {
                reason: "expired".into(),
            });
        }
        let relay = self.relay.clone();
        self.emit(Event::Online { resumed, relay });
        self.pump_queued(None);
    }

    fn reply(&mut self, m: &Value, now: f64) {
        let Some(id) = m["id"].as_u64().map(|x| x as u32) else {
            if m["t"] == "err" {
                let message = m["message"].as_str().unwrap_or("?").to_string();
                self.emit(Event::Error { message });
            }
            return;
        };
        let kind = self.reqs.remove(&id).unwrap_or(Req::Other);
        if kind == Req::Away && m["t"] == "ok" {
            let a = Away {
                reason: m["reason"].as_str().unwrap_or("hint").into(),
                until: m["until"].as_f64().map_or(0, |x| x.max(0.0) as u64),
            };
            let grace_ms = m["grace_ms"].as_u64().unwrap_or(0);
            self.away = Some(a.clone());
            self.emit(Event::AwayHeld {
                req: id,
                reason: a.reason,
                until: a.until,
                grace_ms,
            });
            return;
        }
        if m["t"] == "err" {
            if let Some(ms) = m["retry_after_ms"].as_u64() {
                self.emit(Event::RateLimited {
                    req: id,
                    retry_after_ms: ms,
                });
            }
            self.emit(Event::Failed {
                req: id,
                code: m["code"].as_str().unwrap_or("internal").into(),
                message: m["message"].as_str().unwrap_or("").into(),
            });
            return;
        }
        if matches!(kind, Req::Create | Req::Join) {
            let Ok(room) = serde_json::from_value::<Room>(m["room"].clone()) else {
                return;
            };
            self.room = Some(room.clone());
            // The room first, then its peers: games learn who is who from
            // Joined before any PeerConnected arrives.
            let others: Vec<u16> = room
                .members
                .iter()
                .filter(|x| x.mid != room.you)
                .map(|x| x.mid)
                .collect();
            self.emit(if kind == Req::Create {
                Event::Created { req: id, room }
            } else {
                Event::Joined { req: id, room }
            });
            for mid in others {
                self.add_peer(mid, now);
            }
        } else if let Ok(mut room) = serde_json::from_value::<Room>(m["room"].clone()) {
            room.reclaim = self.room.as_ref().and_then(|r| r.reclaim.clone());
            self.room = Some(room);
        } else if let Ok(member) = serde_json::from_value::<Member>(m["member"].clone()) {
            if let Some(x) = self
                .room
                .as_mut()
                .and_then(|r| r.members.iter_mut().find(|x| x.mid == member.mid))
            {
                *x = member;
            }
        }
    }

    fn left_room(&mut self) {
        self.room = None;
        self.away = None;
        let mids: Vec<u16> = self.peers.keys().copied().collect();
        for mid in mids {
            self.drop_peer(mid);
        }
    }

    fn add_peer(&mut self, mid: u16, now: f64) {
        if self.peers.contains_key(&mid) {
            return;
        }
        let mut link = Link::default();
        link.set_open(PathId::Ws, self.online(), now);
        self.peers.insert(
            mid,
            Peer {
                link,
                connected: false,
                pcs: BTreeMap::new(),
            },
        );
        if !self.cfg.no_webrtc {
            for path in self.webrtc_paths() {
                self.peers.get_mut(&mid).unwrap().pcs.insert(
                    path,
                    Pc {
                        gen: 0,
                        phase: PcPhase::Waiting { retry_at: now },
                        failures: 0,
                    },
                );
            }
        }
        self.flush_link(mid, now);
    }

    fn webrtc_paths(&self) -> Vec<PathId> {
        // No relay credentials: only a direct attempt (host/STUN candidates).
        if self.relay == "turn" {
            vec![PathId::Turn, PathId::Direct]
        } else {
            vec![PathId::Direct]
        }
    }

    fn drop_peer(&mut self, mid: u16) {
        if let Some(p) = self.peers.remove(&mid) {
            for path in p.pcs.keys() {
                self.out.push_back(Action::ClosePc { mid, path: *path });
            }
        }
    }

    /// The lower member id offers on every path of a pair.
    fn offerer(&self, mid: u16) -> bool {
        self.me().is_some_and(|me| me < mid)
    }

    fn ice_for(&self, path: PathId) -> (Vec<IceServer>, bool) {
        match path {
            PathId::Turn => (self.ice.clone(), true),
            // Direct: host and server-reflexive candidates only.
            _ => (
                self.ice
                    .iter()
                    .filter_map(|s| {
                        let urls: Vec<String> = s
                            .urls
                            .iter()
                            .filter(|u| u.starts_with("stun:"))
                            .cloned()
                            .collect();
                        (!urls.is_empty()).then(|| IceServer {
                            urls,
                            username: None,
                            credential: None,
                        })
                    })
                    .collect(),
                false,
            ),
        }
    }

    fn pc_timers(&mut self, mid: u16, now: f64) {
        let offerer = self.offerer(mid);
        let online = self.online();
        let Some(p) = self.peers.get(&mid) else {
            return;
        };
        let mut todo = Vec::new();
        for (path, pc) in &p.pcs {
            match pc.phase {
                PcPhase::Waiting { retry_at } if offerer && online && now >= retry_at => {
                    todo.push((*path, true))
                }
                PcPhase::Connecting { since } if now - since > PC_OPEN_TIMEOUT_MS => {
                    todo.push((*path, false))
                }
                _ => {}
            }
        }
        for (path, start) in todo {
            if start {
                self.start_pc(mid, path, None, now);
            } else {
                let gen = self.peers[&mid].pcs[&path].gen;
                self.on_pc_closed(mid, path, gen, now);
            }
        }
    }

    /// After a resume (ours or theirs), get every unopened WebRTC path going
    /// again now: signals sent while one side was away may be lost. The
    /// offerer restarts; the answerer asks it to.
    fn kick_unopened(&mut self, mid: u16, now: f64) {
        if self.cfg.no_webrtc {
            return;
        }
        let offerer = self.offerer(mid);
        let Some(p) = self.peers.get_mut(&mid) else {
            return;
        };
        let mut ask = Vec::new();
        for (path, pc) in p.pcs.iter_mut() {
            if pc.phase == PcPhase::Open {
                continue;
            }
            if offerer {
                pc.phase = PcPhase::Waiting { retry_at: now };
                pc.failures = 0;
            } else {
                ask.push(*path);
            }
        }
        for path in ask {
            self.on_pc_signal(
                mid,
                path,
                self.peers[&mid].pcs[&path].gen,
                json!({"type": "restart"}),
            );
        }
    }

    fn start_pc(&mut self, mid: u16, path: PathId, gen: Option<u32>, now: f64) {
        let offerer = gen.is_none();
        let (ice, relay_only) = self.ice_for(path);
        let Some(p) = self.peers.get_mut(&mid) else {
            return;
        };
        let pc = p.pcs.entry(path).or_insert(Pc {
            gen: 0,
            phase: PcPhase::Waiting { retry_at: now },
            failures: 0,
        });
        pc.gen = gen.unwrap_or(pc.gen + 1);
        pc.phase = PcPhase::Connecting { since: now };
        let g = pc.gen;
        self.out.push_back(Action::ClosePc { mid, path });
        self.out.push_back(Action::CreatePc {
            mid,
            path,
            gen: g,
            offerer,
            ice,
            relay_only,
        });
    }

    fn on_signal(&mut self, from: u16, data: &Value, now: f64) {
        let path = match data["pc"].as_str() {
            Some("turn") => PathId::Turn,
            Some("direct") => PathId::Direct,
            _ => return,
        };
        let gen = data["gen"].as_u64().unwrap_or(0) as u32;
        if !self.peers.contains_key(&from) {
            // A signal can beat member_joined; the room says who is here.
            if !self
                .room
                .as_ref()
                .is_some_and(|r| r.members.iter().any(|m| m.mid == from))
            {
                return;
            }
            self.add_peer(from, now);
        }
        if self.cfg.no_webrtc {
            return;
        }
        if data["type"] == "restart" {
            if self.offerer(from) {
                if let Some(pc) = self.peers.get_mut(&from).and_then(|p| p.pcs.get_mut(&path)) {
                    if pc.phase != PcPhase::Open {
                        pc.phase = PcPhase::Waiting { retry_at: now };
                        pc.failures = 0;
                    }
                }
            }
            return;
        }
        let known = self.peers[&from].pcs.get(&path).map(|p| p.gen);
        let is_offer = data["type"] == "offer";
        if is_offer
            && !self.offerer(from)
            && known.is_none_or(|k| {
                gen > k
                    || matches!(self.peers[&from].pcs[&path].phase, PcPhase::Waiting { .. })
                        && gen >= k
            })
        {
            self.start_pc(from, path, Some(gen), now);
        }
        if self.peers[&from].pcs.get(&path).map(|p| p.gen) == Some(gen) {
            self.out.push_back(Action::PcSignal {
                mid: from,
                path,
                gen,
                signal: data.clone(),
            });
        }
    }

    /// Route the link's due datagrams onto their paths and surface its events.
    fn flush_link(&mut self, mid: u16, now: f64) {
        let Some(p) = self.peers.get_mut(&mid) else {
            return;
        };
        let datagrams = p.link.poll(now);
        let mut events = Vec::new();
        while let Some(e) = p.link.next_event() {
            events.push(e);
        }
        if !p.connected && p.link.connected_at.is_some() {
            p.connected = true;
            events.insert(
                0,
                LinkEvent::Switched {
                    from: p.link.current(),
                    to: p.link.current(),
                    rtt: None,
                },
            );
        }
        let mut ws: Vec<Vec<u8>> = Vec::new();
        for (path, d) in datagrams {
            match path {
                PathId::Ws => ws.push(d),
                _ => self.out.push_back(Action::PcSend {
                    mid,
                    path,
                    bytes: d,
                }),
            }
        }
        if matches!(self.sock, Sock::Up) && !ws.is_empty() {
            if let [one] = &ws[..] {
                let mut f = Vec::with_capacity(one.len() + 3);
                f.push(RELAY_FRAME);
                f.extend_from_slice(&mid.to_be_bytes());
                f.extend_from_slice(one);
                self.out.push_back(Action::SendBinary(f));
            } else {
                for chunk in ws.chunks(255) {
                    let mut f = vec![RELAY_BUNDLE];
                    f.extend_from_slice(&mid.to_be_bytes());
                    f.push(chunk.len() as u8);
                    for d in chunk {
                        f.extend_from_slice(&(d.len() as u16).to_le_bytes());
                        f.extend_from_slice(d);
                    }
                    self.out.push_back(Action::SendBinary(f));
                }
            }
        }
        for e in events {
            match e {
                LinkEvent::Message { reliable, bytes } => self.emit(Event::Message {
                    from: mid,
                    reliable,
                    bytes,
                }),
                LinkEvent::Switched { from, to, .. } if from == to => {
                    self.emit(Event::PeerConnected { mid, path: to })
                }
                LinkEvent::Switched { from, to, rtt } => {
                    self.emit(Event::PathChanged { mid, from, to, rtt })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
