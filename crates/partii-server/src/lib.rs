//! Game servers for [partii-host](https://partii.games/docs/HOSTING.md), in Rust.
//!
//! Implement [`Game`], export it with [`export_game!`], and build for
//! `wasm32-unknown-unknown`. The `.wasm` is the server build: partii-host runs
//! it in its own sandbox, ticks it at 60 Hz (or a divisor), and moves every
//! player's datagrams in and out.
//!
//! ```
//! use partii_server::{export_game, Game, Host, JoinInfo, PlayerId};
//!
//! #[derive(Default)]
//! struct Echo { players: Vec<(PlayerId, [u8; 8])> }
//!
//! impl Game for Echo {
//!     fn init(_seed: u64) -> Self { Echo::default() }
//!     fn on_join(&mut self, host: &mut Host, player: PlayerId, info: JoinInfo) {
//!         host.log(&format!("{} joined", info.name));
//!         self.players.push((player, [0; 8]));
//!     }
//!     fn on_leave(&mut self, _host: &mut Host, player: PlayerId) {
//!         self.players.retain(|p| p.0 != player);
//!     }
//!     fn on_input(&mut self, _host: &mut Host, player: PlayerId, input: &[u8]) {
//!         if let (Some(p), Ok(xy)) = (self.players.iter_mut().find(|p| p.0 == player), input.try_into()) {
//!             p.1 = xy;
//!         }
//!     }
//!     fn tick(&mut self, host: &mut Host, _now_ms: f64) {
//!         let mut out = Vec::new();
//!         for (id, xy) in &self.players {
//!             out.extend_from_slice(&id.0.to_le_bytes());
//!             out.extend_from_slice(xy);
//!         }
//!         let _ = host.broadcast(&out);
//!     }
//! }
//!
//! export_game!(Echo);
//! ```
//!
//! ## The rules partii-host enforces
//!
//! - **Unreliable datagrams only in a match**, at most [`MAX_PAYLOAD`] bytes,
//!   and at most [`Host::max_datagram`] for each player (some home networks
//!   carry less). Put reliability in the data: [`netcode`] has sequence
//!   numbers, acks and redundant inputs.
//! - **A CPU budget per tick** (8 ms by default). A call past it is stopped,
//!   that tick is skipped, and three stops end the match. Do heavy setup in
//!   [`Game::init`], which gets 2 s.
//! - **No clock but `now_ms`, no threads, files or sockets.**
//!
//! ## Testing
//!
//! [`testing::Sim`] runs your `Game` natively with a simulated host: players
//! join and leave, inputs arrive (optionally lost), and everything the game
//! sends is recorded per player, with the same size rules as partii-host.

pub use partii_netcode as netcode;
pub use partii_protocol::abi::{MAX_PAYLOAD, UNMEASURED_PAYLOAD};
pub use partii_protocol::host::JoinInfo;

mod backend;
#[cfg(not(target_arch = "wasm32"))]
pub mod testing;

#[doc(hidden)]
pub mod __private;

/// A player in this match: unique, never reused, starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct PlayerId(pub u32);

/// Why a send didn't go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Over [`MAX_PAYLOAD`] (unreliable sends are refused, not split).
    Oversize,
    /// No such player in the match.
    NoPlayer,
}

/// Your game server. partii-host calls these on one thread, in order: each
/// tick it delivers that tick's joins, leaves and inputs, then calls `tick`.
pub trait Game: Sized + 'static {
    /// Once, before anything else, with a 64-bit random seed. Not under the
    /// tick budget (2 s): build maps, parse data, warm up hot paths here.
    fn init(seed: u64) -> Self;
    fn on_join(&mut self, _host: &mut Host, _player: PlayerId, _info: JoinInfo) {}
    fn on_leave(&mut self, _host: &mut Host, _player: PlayerId) {}
    /// A player's input datagram, as their client sent it.
    fn on_input(&mut self, _host: &mut Host, _player: PlayerId, _input: &[u8]) {}
    fn tick(&mut self, host: &mut Host, now_ms: f64);
}

/// What the host lets a game do. Passed to every [`Game`] call.
pub struct Host {
    _private: (),
}

impl Host {
    #[doc(hidden)]
    pub fn __new() -> Host {
        Host { _private: () }
    }

    /// Milliseconds since the match started (monotonic).
    pub fn now_ms(&self) -> f64 {
        backend::now_ms()
    }

    /// An unreliable datagram to one player.
    pub fn send(&mut self, player: PlayerId, bytes: &[u8]) -> Result<(), SendError> {
        code(backend::send(player.0, bytes, false))
    }

    /// An unreliable datagram to every player.
    pub fn broadcast(&mut self, bytes: &[u8]) -> Result<(), SendError> {
        code(backend::broadcast(bytes, false))
    }

    /// Bytes on the player's reliable stream. **Setup only** (big first
    /// downloads, over QUIC/WebTransport); plain-UDP players have no reliable
    /// stream, so never depend on it in a match.
    pub fn send_reliable(&mut self, player: PlayerId, bytes: &[u8]) -> Result<(), SendError> {
        code(backend::send(player.0, bytes, true))
    }

    /// The largest unreliable payload that reaches this player right now
    /// (it changes as the path is measured; [`UNMEASURED_PAYLOAD`] until then).
    pub fn max_datagram(&self, player: PlayerId) -> usize {
        match backend::max_datagram(player.0) {
            0 => UNMEASURED_PAYLOAD,
            n => n as usize,
        }
    }

    /// A line in the match's log (the host keeps the last 50).
    pub fn log(&mut self, line: &str) {
        backend::log(line)
    }

    /// Not cryptographic.
    pub fn random_u32(&mut self) -> u32 {
        backend::random_u32()
    }
}

fn code(r: i32) -> Result<(), SendError> {
    match r {
        partii_protocol::abi::SEND_OVERSIZE => Err(SendError::Oversize),
        partii_protocol::abi::SEND_NO_PLAYER => Err(SendError::NoPlayer),
        _ => Ok(()),
    }
}

/// Export a [`Game`] as the partii-host ABI (`alloc`, `init`, `on_join`,
/// `on_leave`, `on_input`, `tick`). Use it once, in a `cdylib` crate built for
/// `wasm32-unknown-unknown`. On other targets it does nothing, so the same
/// crate also builds natively for tests.
#[macro_export]
macro_rules! export_game {
    ($game:ty) => {
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            ::std::thread_local! {
                static GAME: ::core::cell::Cell<*mut $game> = const { ::core::cell::Cell::new(::core::ptr::null_mut()) };
            }
            #[no_mangle]
            pub extern "C" fn alloc(len: usize) -> *mut u8 {
                $crate::__private::alloc(len)
            }
            #[no_mangle]
            pub extern "C" fn init(seed_lo: u32, seed_hi: u32) {
                GAME.with(|g| $crate::__private::init::<$game>(g, seed_lo, seed_hi))
            }
            #[no_mangle]
            pub extern "C" fn on_join(player: u32, ptr: *mut u8, len: usize) {
                GAME.with(|g| $crate::__private::on_join::<$game>(g, player, ptr, len))
            }
            #[no_mangle]
            pub extern "C" fn on_leave(player: u32) {
                GAME.with(|g| $crate::__private::on_leave::<$game>(g, player))
            }
            #[no_mangle]
            pub extern "C" fn on_input(player: u32, ptr: *mut u8, len: usize) {
                GAME.with(|g| $crate::__private::on_input::<$game>(g, player, ptr, len))
            }
            #[no_mangle]
            pub extern "C" fn tick(now_ms: f64) {
                GAME.with(|g| $crate::__private::tick::<$game>(g, now_ms))
            }
        };
    };
}
