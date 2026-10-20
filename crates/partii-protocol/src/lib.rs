//! partii's wire types, shared by game clients ([`partii-net`]), game servers
//! ([`partii-server`]) and partii itself. Nothing here does I/O.
//!
//! - [`room`]: the live room protocol's records (rooms, members, ICE servers)
//!   and relay framing.
//! - [`host`]: partii-host's datagram frames, join tickets and the tags plain
//!   UDP packets carry.
//! - [`abi`]: the names and limits of the partii-host game-server ABI.
//!
//! [`partii-net`]: https://docs.rs/partii-net
//! [`partii-server`]: https://docs.rs/partii-server

pub mod abi;
pub mod host;
pub mod room;

pub use room::{IceServer, Member, Room, BROADCAST};
