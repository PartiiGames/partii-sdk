//! A link to one peer: up to three paths (ws relay, TURN, direct), each
//! pinged and measured, with traffic on the best healthy one.
//!
//! - Play starts on whatever is open first (the ws relay is open the moment
//!   both peers are in the room).
//! - A better path takes over only when it scores better by ≥10% and ≥5 ms
//!   for a full second. Score = RTT, plus `WS_PENALTY_MS` for the ws relay,
//!   so a healthy WebRTC path wins unless it is clearly slower.
//! - A path that misses pongs for `DEAD_MS` loses traffic at once.
//! - Reliable messages ride the link's ARQ, so a switch never loses or
//!   reorders them.
//!
//! Sans-IO: the driver feeds datagrams and open/close changes in, and sends
//! what `poll` returns.

use crate::arq::{self, Ack, Fragment};
use std::collections::VecDeque;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum PathId {
    Ws = 0,
    Turn = 1,
    Direct = 2,
}

impl PathId {
    pub const ALL: [PathId; 3] = [PathId::Ws, PathId::Turn, PathId::Direct];
    pub fn name(self) -> &'static str {
        match self {
            PathId::Ws => "ws",
            PathId::Turn => "turn",
            PathId::Direct => "direct",
        }
    }
}

pub const PING_MS: f64 = 250.0;
pub const DEAD_MS: f64 = 1000.0;
/// The ws relay costs an edge request per packet, so while it is not
/// carrying play it is pinged rarely (the session socket has its own
/// keepalive) and judged over a longer window.
pub const WS_IDLE_PING_MS: f64 = 2000.0;
pub const WS_IDLE_DEAD_MS: f64 = 6000.0;
pub const SWITCH_HOLD_MS: f64 = 1000.0;
/// Leaving the ws relay for WebRTC needs less proof (the relay is the
/// expensive last rung, and a flap back costs nothing but a few packets).
pub const LEAVE_WS_HOLD_MS: f64 = 250.0;
/// The ws relay is the last rung: every packet is an edge request and it
/// rides TCP. Score it this much worse so play leaves it for any healthy
/// WebRTC path that is not clearly slower.
pub const WS_PENALTY_MS: f64 = 25.0;

const PING: u8 = 0x10;
const PONG: u8 = 0x11;
const UNREL: u8 = 0x20;
const REL: u8 = 0x30;
const ACK: u8 = 0x31;

#[derive(Debug, Clone, Default)]
pub struct PathState {
    pub open: bool,
    /// Smoothed round trip (ms) once a pong has come back.
    pub rtt: Option<f64>,
    pub last_pong: Option<f64>,
    last_ping: f64,
    ping_seq: u32,
    pub pings: u64,
    pub pongs: u64,
}

impl PathState {
    pub fn healthy(&self, now: f64) -> bool {
        self.healthy_within(now, DEAD_MS)
    }
    fn healthy_within(&self, now: f64, dead: f64) -> bool {
        self.open && self.last_pong.is_some_and(|t| now - t < dead)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkEvent {
    Message {
        reliable: bool,
        bytes: Vec<u8>,
    },
    Switched {
        from: PathId,
        to: PathId,
        rtt: Option<f64>,
    },
}

pub struct Link {
    pub paths: [PathState; 3],
    current: PathId,
    candidate: Option<(PathId, f64)>,
    tx: arq::Sender,
    rx: arq::Receiver,
    out: VecDeque<(PathId, Vec<u8>)>,
    unrel: VecDeque<Vec<u8>>,
    events: VecDeque<LinkEvent>,
    /// Ever had a usable path (the peer counts as connected from then on).
    pub connected_at: Option<f64>,
}

impl Default for Link {
    fn default() -> Self {
        Self {
            paths: Default::default(),
            current: PathId::Ws,
            candidate: None,
            tx: arq::Sender::default(),
            rx: arq::Receiver::default(),
            out: VecDeque::new(),
            unrel: VecDeque::new(),
            events: VecDeque::new(),
            connected_at: None,
        }
    }
}

impl Link {
    pub fn current(&self) -> PathId {
        self.current
    }
    pub fn path(&self, p: PathId) -> &PathState {
        &self.paths[p as usize]
    }
    /// Is path `p` alive, judged by its own ping cadence?
    fn alive(&self, p: PathId, now: f64) -> bool {
        let dead = if p == PathId::Ws && self.current != PathId::Ws {
            WS_IDLE_DEAD_MS
        } else {
            DEAD_MS
        };
        self.path(p).healthy_within(now, dead)
    }
    fn path_mut(&mut self, p: PathId) -> &mut PathState {
        &mut self.paths[p as usize]
    }
    pub fn retransmits(&self) -> u64 {
        self.tx.retransmits
    }
    pub fn pending_reliable(&self) -> usize {
        self.tx.in_flight()
    }

    pub fn set_open(&mut self, p: PathId, open: bool, now: f64) {
        let s = self.path_mut(p);
        if s.open == open {
            return;
        }
        s.open = open;
        if open {
            // Ping at once so the path gets measured quickly.
            s.last_ping = f64::NEG_INFINITY;
        } else {
            s.last_pong = None;
            s.rtt = None;
        }
        self.select(now);
    }

    pub fn send(&mut self, reliable: bool, bytes: &[u8]) {
        if reliable {
            self.tx.send(bytes);
        } else {
            self.unrel.push_back(bytes.to_vec());
        }
    }

    pub fn next_event(&mut self) -> Option<LinkEvent> {
        self.events.pop_front()
    }

    /// Take one datagram that arrived on path `p`.
    pub fn on_datagram(&mut self, p: PathId, d: &[u8], now: f64) {
        let Some((&kind, body)) = d.split_first() else {
            return;
        };
        match kind {
            PING if body.len() == 12 => {
                let mut pong = vec![PONG];
                pong.extend_from_slice(body);
                self.out.push_back((p, pong));
            }
            PONG if body.len() == 12 => {
                let sent = f64::from_le_bytes(body[4..12].try_into().unwrap());
                let sample = (now - sent).max(0.0);
                let s = self.path_mut(p);
                s.pongs += 1;
                s.last_pong = Some(now);
                s.rtt = Some(match s.rtt {
                    None => sample,
                    Some(r) => 0.8 * r + 0.2 * sample,
                });
                self.select(now);
            }
            UNREL => self.events.push_back(LinkEvent::Message {
                reliable: false,
                bytes: body.to_vec(),
            }),
            REL if body.len() >= 5 => {
                let seq = u32::from_le_bytes(body[0..4].try_into().unwrap());
                let f = Fragment {
                    seq,
                    flags: body[4],
                    payload: body[5..].to_vec(),
                };
                for m in self.rx.on_data(f) {
                    self.events.push_back(LinkEvent::Message {
                        reliable: true,
                        bytes: m,
                    });
                }
            }
            ACK if body.len() == 12 => {
                let next = u32::from_le_bytes(body[0..4].try_into().unwrap());
                let bits = u64::from_le_bytes(body[4..12].try_into().unwrap());
                self.tx.on_ack(Ack { next, bits }, now);
            }
            _ => {}
        }
    }

    /// Everything due on the wire now, tagged with the path to send it on.
    pub fn poll(&mut self, now: f64) -> Vec<(PathId, Vec<u8>)> {
        self.select(now);
        let mut out: Vec<(PathId, Vec<u8>)> = self.out.drain(..).collect();
        let cur = self.current;
        for p in PathId::ALL {
            let every = if p == PathId::Ws && cur != PathId::Ws {
                WS_IDLE_PING_MS
            } else {
                PING_MS
            };
            let s = self.path_mut(p);
            if s.open && now - s.last_ping >= every {
                s.last_ping = now;
                s.ping_seq = s.ping_seq.wrapping_add(1);
                s.pings += 1;
                let mut d = vec![PING];
                d.extend_from_slice(&s.ping_seq.to_le_bytes());
                d.extend_from_slice(&now.to_le_bytes());
                out.push((p, d));
            }
        }
        if !self.usable(cur, now) {
            // Nothing to carry traffic yet; reliable data waits in the ARQ.
            self.unrel.clear();
            return out;
        }
        for b in self.unrel.drain(..) {
            let mut d = Vec::with_capacity(b.len() + 1);
            d.push(UNREL);
            d.extend_from_slice(&b);
            out.push((cur, d));
        }
        for f in self.tx.poll(now) {
            let mut d = Vec::with_capacity(f.payload.len() + 6);
            d.push(REL);
            d.extend_from_slice(&f.seq.to_le_bytes());
            d.push(f.flags);
            d.extend_from_slice(&f.payload);
            out.push((cur, d));
        }
        if self.rx.ack_due {
            let a = self.rx.ack();
            let mut d = vec![ACK];
            d.extend_from_slice(&a.next.to_le_bytes());
            d.extend_from_slice(&a.bits.to_le_bytes());
            out.push((cur, d));
        }
        out
    }

    /// Can `p` carry traffic? A healthy path can. The ws relay can as soon
    /// as it is open (it runs over a reliable socket the edge already
    /// checked), so play starts before the first pong.
    fn usable(&self, p: PathId, now: f64) -> bool {
        let s = self.path(p);
        self.alive(p, now) || (p == PathId::Ws && s.open && s.last_pong.is_none())
    }

    fn select(&mut self, now: f64) {
        let cur = self.current;
        let cur_ok = self.usable(cur, now);
        if cur_ok && self.connected_at.is_none() {
            self.connected_at = Some(now);
        }
        let score = |p: PathId| {
            self.path(p)
                .rtt
                .map(|r| r + if p == PathId::Ws { WS_PENALTY_MS } else { 0.0 })
        };
        let best = PathId::ALL
            .into_iter()
            .filter(|p| *p != cur && self.alive(*p, now))
            .min_by(|a, b| {
                score(*a)
                    .unwrap_or(f64::MAX)
                    .total_cmp(&score(*b).unwrap_or(f64::MAX))
            });
        if !cur_ok {
            // Fail over now: the best measured path, else the ws relay.
            let to = best
                .or_else(|| self.usable(PathId::Ws, now).then_some(PathId::Ws))
                .filter(|p| *p != cur);
            if let Some(to) = to {
                self.switch(to, now);
            }
            return;
        }
        let Some(b) = best else {
            self.candidate = None;
            return;
        };
        let (rb, rc) = (score(b).unwrap_or(f64::MAX), score(cur));
        let better = match rc {
            // Current not measured yet (fresh ws): any measured path is better.
            None => true,
            Some(rc) => rb < rc * 0.9 && rb < rc - 5.0,
        };
        if !better {
            self.candidate = None;
            return;
        }
        match self.candidate {
            Some((c, since)) if c == b => {
                let hold = if cur == PathId::Ws {
                    LEAVE_WS_HOLD_MS
                } else {
                    SWITCH_HOLD_MS
                };
                if now - since >= hold || rc.is_none() {
                    self.switch(b, now);
                }
            }
            _ => {
                self.candidate = Some((b, now));
                if rc.is_none() {
                    self.switch(b, now);
                }
            }
        }
    }

    fn switch(&mut self, to: PathId, now: f64) {
        let from = self.current;
        self.current = to;
        self.candidate = None;
        self.tx.rush();
        if self.connected_at.is_none() && self.usable(to, now) {
            self.connected_at = Some(now);
        }
        self.events.push_back(LinkEvent::Switched {
            from,
            to,
            rtt: self.path(to).rtt,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two links joined by three simulated paths, each with its own one-way
    /// delay and an up/down switch.
    struct Sim {
        a: Link,
        b: Link,
        delay: [f64; 3],
        up: [bool; 3],
        q: Vec<(f64, bool, PathId, Vec<u8>)>, // (deliver at, to_b, path, bytes)
        now: f64,
        got_a: Vec<LinkEvent>,
        got_b: Vec<LinkEvent>,
    }

    impl Sim {
        fn new(delay: [f64; 3]) -> Self {
            Self {
                a: Link::default(),
                b: Link::default(),
                delay,
                up: [false; 3],
                q: vec![],
                now: 0.0,
                got_a: vec![],
                got_b: vec![],
            }
        }
        fn open(&mut self, p: PathId, open: bool) {
            self.up[p as usize] = open;
            self.a.set_open(p, open, self.now);
            self.b.set_open(p, open, self.now);
        }
        /// The path silently stops carrying packets (still "open").
        fn blackhole(&mut self, p: PathId) {
            self.up[p as usize] = false;
        }
        fn step(&mut self, ms: f64) {
            let end = self.now + ms;
            while self.now < end {
                for (to_b, out) in [
                    (true, self.a.poll(self.now)),
                    (false, self.b.poll(self.now)),
                ] {
                    for (p, d) in out {
                        if self.up[p as usize] {
                            self.q.push((self.now + self.delay[p as usize], to_b, p, d));
                        }
                    }
                }
                let now = self.now;
                let (due, rest): (Vec<_>, Vec<_>) = self.q.drain(..).partition(|(t, ..)| *t <= now);
                self.q = rest;
                for (_, to_b, p, d) in due {
                    if to_b {
                        self.b.on_datagram(p, &d, now);
                    } else {
                        self.a.on_datagram(p, &d, now);
                    }
                }
                while let Some(e) = self.a.next_event() {
                    self.got_a.push(e);
                }
                while let Some(e) = self.b.next_event() {
                    self.got_b.push(e);
                }
                self.now += 2.0;
            }
        }
        fn msgs_b(&self, reliable: bool) -> Vec<Vec<u8>> {
            self.got_b
                .iter()
                .filter_map(|e| match e {
                    LinkEvent::Message { reliable: r, bytes } if *r == reliable => {
                        Some(bytes.clone())
                    }
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    fn plays_on_the_ws_relay_at_once() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        s.open(PathId::Ws, true);
        s.a.send(true, b"hello");
        s.a.send(false, b"pos");
        s.step(100.0);
        assert_eq!(s.msgs_b(true), vec![b"hello".to_vec()]);
        assert_eq!(s.msgs_b(false), vec![b"pos".to_vec()]);
        assert_eq!(s.a.current(), PathId::Ws);
        assert!(s.a.connected_at.unwrap() < 1.0);
    }

    #[test]
    fn moves_to_turn_then_direct_as_they_measure_faster() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        s.open(PathId::Ws, true);
        s.step(500.0);
        s.open(PathId::Turn, true);
        s.step(100.0);
        assert_eq!(
            s.a.current(),
            PathId::Ws,
            "holds briefly before leaving the relay"
        );
        s.step(600.0);
        assert_eq!(s.a.current(), PathId::Turn);
        s.open(PathId::Direct, true);
        s.step(1500.0);
        assert_eq!(s.a.current(), PathId::Direct);
        // 10 ms round trip plus the sim's 2 ms step on each hop.
        let rtt = s.a.path(PathId::Direct).rtt.unwrap();
        assert!((9.0..17.0).contains(&rtt), "rtt {rtt}");
    }

    #[test]
    fn stays_put_when_the_other_path_is_barely_faster() {
        let mut s = Sim::new([40.0, 20.0, 19.0]);
        s.open(PathId::Ws, true);
        s.open(PathId::Turn, true);
        s.step(2500.0);
        assert_eq!(s.a.current(), PathId::Turn);
        s.open(PathId::Direct, true);
        s.step(3000.0);
        assert_eq!(
            s.a.current(),
            PathId::Turn,
            "19 vs 20 ms is not worth a switch"
        );
    }

    #[test]
    fn fails_over_from_a_dead_path_without_losing_reliable_messages() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        for p in PathId::ALL {
            s.open(p, true);
        }
        s.step(3000.0);
        assert_eq!(s.a.current(), PathId::Direct);
        for i in 0..200u32 {
            s.a.send(true, &i.to_le_bytes());
        }
        s.step(4.0);
        s.blackhole(PathId::Direct);
        let t0 = s.now;
        s.step(1600.0);
        assert_eq!(s.a.current(), PathId::Turn, "failed over");
        let switched_at = s.got_a.iter().rposition(|e| {
            matches!(
                e,
                LinkEvent::Switched {
                    to: PathId::Turn,
                    ..
                }
            )
        });
        assert!(switched_at.is_some());
        s.step(2000.0);
        let want: Vec<Vec<u8>> = (0..200u32).map(|i| i.to_le_bytes().to_vec()).collect();
        assert_eq!(s.msgs_b(true), want, "all reliable messages, in order");
        assert!(s.now - t0 < 4000.0);
    }

    #[test]
    fn falls_back_to_ws_when_every_webrtc_path_dies() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        for p in PathId::ALL {
            s.open(p, true);
        }
        s.step(3000.0);
        s.blackhole(PathId::Direct);
        s.blackhole(PathId::Turn);
        s.step(1500.0);
        assert_eq!(s.a.current(), PathId::Ws);
        s.a.send(true, b"still here");
        s.step(500.0);
        assert!(s.msgs_b(true).contains(&b"still here".to_vec()));
    }

    #[test]
    fn leaves_a_fast_ws_relay_for_webrtc() {
        // Even when the relay is quick, a healthy WebRTC path takes over.
        let mut s = Sim::new([3.0, 20.0, 1.0]);
        s.open(PathId::Ws, true);
        s.step(600.0);
        s.open(PathId::Direct, true);
        s.step(1500.0);
        assert_eq!(s.a.current(), PathId::Direct);
        assert_eq!(s.b.current(), PathId::Direct);
    }

    #[test]
    fn the_idle_ws_relay_is_pinged_rarely_but_still_catches_a_failover() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        s.open(PathId::Ws, true);
        s.open(PathId::Direct, true);
        s.step(2000.0);
        assert_eq!(s.a.current(), PathId::Direct);
        let before = s.a.path(PathId::Ws).pings;
        s.step(10_000.0);
        let ws_pings = s.a.path(PathId::Ws).pings - before;
        assert!(ws_pings <= 6, "idle relay pinged {ws_pings} times in 10 s");
        s.blackhole(PathId::Direct);
        s.step(1500.0);
        assert_eq!(s.a.current(), PathId::Ws, "still the fallback");
        s.a.send(true, b"after");
        s.step(500.0);
        assert!(s.msgs_b(true).contains(&b"after".to_vec()));
    }

    #[test]
    fn a_closed_path_drops_out_immediately() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        s.open(PathId::Ws, true);
        s.open(PathId::Turn, true);
        s.step(2500.0);
        assert_eq!(s.a.current(), PathId::Turn);
        s.open(PathId::Turn, false);
        assert_eq!(s.a.current(), PathId::Ws);
    }

    #[test]
    fn nothing_open_means_reliable_data_waits() {
        let mut s = Sim::new([40.0, 20.0, 5.0]);
        s.a.send(true, b"queued");
        s.step(200.0);
        assert!(s.msgs_b(true).is_empty());
        s.open(PathId::Turn, true);
        s.step(500.0);
        assert_eq!(s.msgs_b(true), vec![b"queued".to_vec()]);
    }
}
