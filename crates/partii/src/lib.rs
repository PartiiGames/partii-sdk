//! The partii SDK for Rust games. Each part is its own crate; this one
//! re-exports them behind features:
//!
//! | Feature | Module | Crate |
//! |---|---|---|
//! | `net` (default) | [`net`] | `partii-net`: rooms, peer-to-peer, the partii-host transport |
//! | `account` (default) | [`account`] | `partii-account`: sign-in, ownership, saves, achievements |
//! | `client` (default) | [`client`] | `partii-client`: the API client |
//! | `content` | [`content`] | `partii-content`: live content channels |
//! | `server` | [`server`] | `partii-server`: write a partii-host game server |
//!
//! Always available: [`protocol`] (wire types) and [`netcode`] (reliability by redundancy).
//!
//! Bevy games: see [`bevy_partii`](https://docs.rs/bevy_partii).

#![cfg_attr(docsrs, feature(doc_cfg))]

pub use partii_netcode as netcode;
pub use partii_protocol as protocol;

#[cfg(feature = "account")]
pub use partii_account as account;
#[cfg(feature = "client")]
pub use partii_client as client;
#[cfg(feature = "content")]
pub use partii_content as content;
#[cfg(feature = "net")]
pub use partii_net as net;
#[cfg(feature = "server")]
pub use partii_server as server;
