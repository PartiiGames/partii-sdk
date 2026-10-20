//! partii networking for games. One implementation of rooms, signaling,
//! peer links and recovery, used natively by games and compiled to wasm for
//! web pages; plus the partii-host client transport ([`host`]) and the
//! netcode kit ([`netcode`]).
//!
//! Guide: <https://partii.games/docs/GUIDE.md>; wire protocol:
//! <https://partii.games/docs/PROTOCOL.md>.
pub mod arq;
pub mod core;
pub mod link;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub mod native;
pub use core::{Config, Event, Member, Room, BROADCAST};
pub use link::PathId;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub use native::Engine;
#[cfg(target_arch = "wasm32")]
pub mod web;
pub use partii_netcode as netcode;
pub use partii_protocol as protocol;
#[cfg(target_arch = "wasm32")]
pub use web::Engine;
#[cfg(not(target_arch = "wasm32"))]
pub mod host;
