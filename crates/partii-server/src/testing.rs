//! Run a [`Game`] natively against a simulated partii-host.
//!
//! ```
//! use partii_server::testing::Sim;
//! # use partii_server::{Game, Host, PlayerId};
//! # struct Count(u32);
//! # impl Game for Count {
//! #     fn init(_: u64) -> Self { Count(0) }
//! #     fn on_input(&mut self, _: &mut Host, _: PlayerId, _: &[u8]) { self.0 += 1 }
//! #     fn tick(&mut self, h: &mut Host, _: f64) { let _ = h.broadcast(&self.0.to_le_bytes()); }
//! # }
//! let mut sim = Sim::<Count>::new(60, 1);
//! let p = sim.join("Sam");
//! sim.input(p, b"go");
//! sim.step();
//! assert_eq!(sim.received(p).last().unwrap(), &1u32.to_le_bytes());
//! ```

use crate::backend::{Backend, CURRENT};
use crate::{Game, Host, JoinInfo, PlayerId};
use partii_protocol::abi::{MAX_PAYLOAD, SEND_NO_PLAYER, SEND_OK, SEND_OVERSIZE};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
struct Player {
    max_datagram: u32,
    received: Vec<Vec<u8>>,
    reliable: Vec<Vec<u8>>,
}

struct State {
    now_ms: f64,
    rng: u64,
    players: BTreeMap<u32, Player>,
    log: Vec<String>,
    oversize_sends: u32,
    over_limit_drops: u32,
    /// Chance (0..1) that a datagram from the game to a player, or an input to the game, is lost.
    loss: f64,
}

impl State {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn lost(&mut self) -> bool {
        self.loss > 0.0 && ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < self.loss
    }
    fn deliver(&mut self, player: u32, b: &[u8], reliable: bool) -> i32 {
        if !reliable && b.len() > MAX_PAYLOAD {
            self.oversize_sends += 1;
            self.log.push(format!("oversize send ({} bytes)", b.len()));
            return SEND_OVERSIZE;
        }
        let lost = !reliable && self.lost();
        let Some(p) = self.players.get_mut(&player) else {
            return SEND_NO_PLAYER;
        };
        if reliable {
            p.reliable.push(b.to_vec());
        } else if p.max_datagram != 0 && b.len() as u32 > p.max_datagram {
            // Like partii-host: over this player's path limit, it isn't sent to them.
            self.over_limit_drops += 1;
        } else if !lost {
            p.received.push(b.to_vec());
        }
        SEND_OK
    }
}

impl Backend for State {
    fn now_ms(&self) -> f64 {
        self.now_ms
    }
    fn send(&mut self, player: u32, b: &[u8], reliable: bool) -> i32 {
        self.deliver(player, b, reliable)
    }
    fn broadcast(&mut self, b: &[u8], reliable: bool) -> i32 {
        if !reliable && b.len() > MAX_PAYLOAD {
            return self.deliver(0, b, false);
        }
        let ids: Vec<u32> = self.players.keys().copied().collect();
        for id in ids {
            self.deliver(id, b, reliable);
        }
        SEND_OK
    }
    fn log(&mut self, s: &str) {
        self.log.push(s.to_string());
    }
    fn random_u32(&mut self) -> u32 {
        self.next() as u32
    }
    fn max_datagram(&self, player: u32) -> u32 {
        self.players.get(&player).map_or(0, |p| p.max_datagram)
    }
}

/// A match with a simulated host. Inputs queue until [`Sim::step`], which
/// delivers them and then ticks, as partii-host does.
pub struct Sim<G: Game> {
    game: G,
    state: Box<State>,
    tick_ms: f64,
    next_player: u32,
    queue: Vec<Event>,
}

enum Event {
    Join(u32, JoinInfo),
    Leave(u32),
    Input(u32, Vec<u8>),
}

impl<G: Game> Sim<G> {
    /// A match ticking at `tick_hz`, with `seed` for the game and the simulated network.
    pub fn new(tick_hz: u32, seed: u64) -> Sim<G> {
        let mut state = Box::new(State {
            now_ms: 0.0,
            rng: seed,
            players: BTreeMap::new(),
            log: Vec::new(),
            oversize_sends: 0,
            over_limit_drops: 0,
            loss: 0.0,
        });
        let game = Self::call_with(&mut state, || G::init(seed));
        Sim {
            game,
            state,
            tick_ms: 1000.0 / tick_hz.max(1) as f64,
            next_player: 1,
            queue: Vec::new(),
        }
    }

    fn call_with<R>(state: &mut State, f: impl FnOnce() -> R) -> R {
        let ptr: *mut dyn Backend = state as &mut dyn Backend;
        CURRENT.with(|c| *c.borrow_mut() = Some(ptr));
        let r = f();
        CURRENT.with(|c| *c.borrow_mut() = None);
        r
    }

    /// Lose this fraction (0..1) of datagrams in both directions.
    pub fn set_loss(&mut self, loss: f64) {
        self.state.loss = loss.clamp(0.0, 1.0);
    }

    /// A player joins (delivered on the next step). Their path carries [`MAX_PAYLOAD`] until [`Sim::set_max_datagram`].
    pub fn join(&mut self, name: &str) -> PlayerId {
        self.join_with(JoinInfo {
            name: name.into(),
            ..Default::default()
        })
    }

    pub fn join_with(&mut self, info: JoinInfo) -> PlayerId {
        let id = self.next_player;
        self.next_player += 1;
        self.state.players.insert(
            id,
            Player {
                max_datagram: MAX_PAYLOAD as u32,
                ..Default::default()
            },
        );
        self.queue.push(Event::Join(id, info));
        PlayerId(id)
    }

    pub fn leave(&mut self, player: PlayerId) {
        self.queue.push(Event::Leave(player.0));
    }

    /// An input datagram from a player (it may be lost, see [`Sim::set_loss`]).
    pub fn input(&mut self, player: PlayerId, bytes: &[u8]) {
        if !self.state.lost() {
            self.queue.push(Event::Input(player.0, bytes.to_vec()));
        }
    }

    /// What this player's path carries (as partii-host's path probing would find).
    pub fn set_max_datagram(&mut self, player: PlayerId, bytes: u32) {
        if let Some(p) = self.state.players.get_mut(&player.0) {
            p.max_datagram = bytes;
        }
    }

    /// Deliver queued joins, leaves and inputs, then tick once.
    pub fn step(&mut self) {
        let events = std::mem::take(&mut self.queue);
        let game = &mut self.game;
        let now = self.state.now_ms;
        Self::call_with(&mut self.state, || {
            let mut h = Host::__new();
            for e in events {
                match e {
                    Event::Join(id, info) => game.on_join(&mut h, PlayerId(id), info),
                    Event::Leave(id) => game.on_leave(&mut h, PlayerId(id)),
                    Event::Input(id, b) => game.on_input(&mut h, PlayerId(id), &b),
                }
            }
            game.tick(&mut h, now);
        });
        self.state.now_ms += self.tick_ms;
    }

    /// Step `n` times.
    pub fn run(&mut self, n: usize) {
        for _ in 0..n {
            self.step();
        }
    }

    /// Every unreliable datagram this player received, oldest first.
    pub fn received(&self, player: PlayerId) -> &[Vec<u8>] {
        self.state
            .players
            .get(&player.0)
            .map_or(&[], |p| &p.received)
    }

    /// Reliable (setup) bytes sent to this player.
    pub fn received_reliable(&self, player: PlayerId) -> &[Vec<u8>] {
        self.state
            .players
            .get(&player.0)
            .map_or(&[], |p| &p.reliable)
    }

    /// Clear what players received so far.
    pub fn clear_received(&mut self) {
        for p in self.state.players.values_mut() {
            p.received.clear();
            p.reliable.clear();
        }
    }

    pub fn log(&self) -> &[String] {
        &self.state.log
    }

    /// Unreliable sends refused for being over [`MAX_PAYLOAD`].
    pub fn oversize_sends(&self) -> u32 {
        self.state.oversize_sends
    }

    /// Datagrams not sent because they were over a player's path limit.
    pub fn over_limit_drops(&self) -> u32 {
        self.state.over_limit_drops
    }

    pub fn now_ms(&self) -> f64 {
        self.state.now_ms
    }

    pub fn game(&self) -> &G {
        &self.game
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo {
        seen: Vec<(PlayerId, Vec<u8>)>,
    }
    impl Game for Echo {
        fn init(_: u64) -> Self {
            Echo { seen: vec![] }
        }
        fn on_join(&mut self, h: &mut Host, p: PlayerId, info: JoinInfo) {
            h.log(&format!("join {} {}", p.0, info.name));
            let _ = h.send_reliable(p, b"welcome");
        }
        fn on_input(&mut self, _: &mut Host, p: PlayerId, i: &[u8]) {
            self.seen.push((p, i.to_vec()));
        }
        fn tick(&mut self, h: &mut Host, now: f64) {
            assert_eq!(now, h.now_ms());
            let _ = h.broadcast(&[1u8; 600]);
            assert_eq!(h.broadcast(&[0u8; 2000]), Err(crate::SendError::Oversize));
            assert_eq!(h.send(PlayerId(99), b"x"), Err(crate::SendError::NoPlayer));
        }
    }

    #[test]
    fn joins_inputs_ticks_and_size_rules() {
        let mut sim = Sim::<Echo>::new(60, 7);
        let a = sim.join("Ann");
        let b = sim.join("Bo");
        sim.set_max_datagram(b, 500);
        sim.input(a, b"left");
        sim.step();
        assert_eq!(sim.game().seen, vec![(a, b"left".to_vec())]);
        assert_eq!(sim.received_reliable(a), &[b"welcome".to_vec()]);
        assert_eq!(sim.received(a).len(), 1, "600 bytes reach a 1199-byte path");
        assert_eq!(sim.received(b).len(), 0, "but not a 500-byte one");
        assert_eq!((sim.oversize_sends(), sim.over_limit_drops()), (1, 1));
        assert!(sim.log().iter().any(|l| l == "join 1 Ann"));
        assert!((sim.now_ms() - 1000.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn loss_drops_some_datagrams() {
        let mut sim = Sim::<Echo>::new(60, 1);
        let a = sim.join("Ann");
        sim.set_loss(0.5);
        sim.run(400);
        let got = sim.received(a).len();
        assert!((120..280).contains(&got), "{got} of 400 at 50% loss");
    }
}
