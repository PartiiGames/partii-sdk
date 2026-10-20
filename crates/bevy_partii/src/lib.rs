//! [Bevy](https://bevy.org) plugin for [partii](https://partii.games).
//!
//! ```no_run
//! use bevy_app::{App, Update};
//! use bevy_ecs::prelude::*;
//! use bevy_partii::{PartiiPlugin, Rooms, RoomEvent};
//!
//! fn main() {
//!     App::new()
//!         .add_plugins(PartiiPlugin::new("https://api.partii.games", "pk_mystudio_mygame_...", "mygame").client("mygame/0.1"))
//!         .add_systems(Update, on_rooms)
//!         .run();
//! }
//!
//! fn on_rooms(mut events: MessageReader<RoomEvent>, rooms: Res<Rooms>) {
//!     for RoomEvent(e) in events.read() {
//!         println!("{e:?} (room {:?})", rooms.room().map(|r| r.code.clone()));
//!     }
//! }
//! ```
//!
//! - **[`Rooms`]** (feature `net`): create a room, join by code, send to
//!   members; everything the engine reports arrives as [`RoomEvent`] messages,
//!   and [`PeerMessage`]s carry game bytes from other members.
//! - **[`MatchSeat`]** (feature `net`): a seat in a server-authoritative match
//!   on partii-host; frames arrive as [`SeatMessage`]s.
//! - **[`PartiiAccount`]** (feature `account`): guest sign-in, ownership,
//!   saves, checkout and achievements; new unlocks arrive as
//!   [`AchievementUnlocked`] messages.

use bevy_app::{App, Plugin, PreUpdate};
use bevy_ecs::prelude::*;

pub use partii_protocol as protocol;

#[cfg(feature = "account")]
pub use partii_account as account;
#[cfg(feature = "net")]
pub use partii_net as net;

/// Adds partii to an app. Rooms connect at startup when `net` is on; the
/// account signs in when `account` is on and a data folder is set.
#[derive(Clone)]
pub struct PartiiPlugin {
    pub edge: String,
    pub key: String,
    pub game: String,
    /// Client tag, `game/version platform`.
    pub client: String,
    /// The game's build fingerprint for rooms (`net_version`): players on
    /// another build are refused with `version_mismatch`.
    pub net_version: Option<String>,
    /// Where the account keeps its device id and caches (the game's save folder).
    /// `None`: no account.
    pub data_dir: Option<std::path::PathBuf>,
    pub display_name: String,
}

impl PartiiPlugin {
    pub fn new(edge: &str, key: &str, game: &str) -> Self {
        PartiiPlugin {
            edge: edge.into(),
            key: key.into(),
            game: game.into(),
            client: game.into(),
            net_version: None,
            data_dir: None,
            display_name: "Player".into(),
        }
    }
    pub fn client(mut self, tag: &str) -> Self {
        self.client = tag.into();
        self
    }
    pub fn net_version(mut self, v: &str) -> Self {
        self.net_version = Some(v.into());
        self
    }
    pub fn account(mut self, data_dir: impl Into<std::path::PathBuf>, display_name: &str) -> Self {
        self.data_dir = Some(data_dir.into());
        self.display_name = display_name.into();
        self
    }
}

impl Plugin for PartiiPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(feature = "net")]
        {
            let engine = partii_net::Engine::start(partii_net::core::Config {
                edge: self.edge.clone(),
                key: self.key.clone(),
                conn: partii_net::Engine::new_conn_id(),
                client: self.client.clone(),
                ..Default::default()
            });
            app.insert_resource(Rooms {
                engine,
                game: self.game.clone(),
                net_version: self.net_version.clone(),
                room: None,
            })
            .add_message::<RoomEvent>()
            .add_message::<PeerMessage>()
            .add_message::<SeatMessage>()
            .add_systems(PreUpdate, (pump_rooms, pump_seats));
        }
        #[cfg(feature = "account")]
        {
            app.add_message::<AchievementUnlocked>()
                .add_systems(PreUpdate, pump_account);
            if let Some(dir) = &self.data_dir {
                let acct = partii_account::Account::start(partii_account::AccountConfig {
                    edge: self.edge.clone(),
                    key: Some(self.key.clone()),
                    game: self.game.clone(),
                    data_dir: dir.clone(),
                    display_name: self.display_name.clone(),
                    ..Default::default()
                });
                app.insert_resource(PartiiAccount(acct));
            }
        }
        let _ = app;
    }
}

/// Everything the room engine reports: online/offline, created/joined,
/// members coming and going, failures (`version_mismatch`, `full`, …).
/// Game bytes from other members arrive as [`PeerMessage`] instead.
#[cfg(feature = "net")]
#[derive(Message, Debug, Clone)]
pub struct RoomEvent(pub partii_net::Event);

/// Bytes another member sent.
#[cfg(feature = "net")]
#[derive(Message, Debug, Clone)]
pub struct PeerMessage {
    pub from: u16,
    pub reliable: bool,
    pub bytes: Vec<u8>,
}

/// The room engine as a resource.
#[cfg(feature = "net")]
#[derive(Resource)]
pub struct Rooms {
    engine: partii_net::Engine,
    game: String,
    net_version: Option<String>,
    room: Option<partii_net::Room>,
}

#[cfg(feature = "net")]
impl Rooms {
    /// Create a room; its code arrives in a `Created` event. Returns the request id.
    pub fn create(&self, max_members: u32, name: &str) -> u32 {
        let mut settings = serde_json::json!({ "max_members": max_members });
        if let Some(v) = &self.net_version {
            settings["net_version"] = v.clone().into();
        }
        self.engine
            .create_with(&self.game, "game", name, serde_json::Value::Null, settings)
    }

    /// Join by code (a phone-friendly 5-letter code). Returns the request id.
    pub fn join(&self, code: &str, name: &str) -> u32 {
        let settings = match &self.net_version {
            Some(v) => serde_json::json!({ "net_version": v }),
            None => serde_json::json!({}),
        };
        self.engine
            .join_with(code, "game", name, serde_json::Value::Null, settings)
    }

    pub fn leave(&mut self) {
        self.engine.leave();
        self.room = None;
    }

    /// Send to a member (`partii_net::BROADCAST` for everyone). Unreliable is the
    /// default for game state; reliable is for things that must arrive.
    pub fn send(&self, to: u16, reliable: bool, bytes: Vec<u8>) {
        self.engine.send(to, reliable, bytes);
    }

    /// The room this game is in, as last reported.
    pub fn room(&self) -> Option<&partii_net::Room> {
        self.room.as_ref()
    }

    /// The engine, for anything not wrapped here.
    pub fn engine(&self) -> &partii_net::Engine {
        &self.engine
    }
}

#[cfg(feature = "net")]
fn pump_rooms(
    mut rooms: ResMut<Rooms>,
    mut events: MessageWriter<RoomEvent>,
    mut peers: MessageWriter<PeerMessage>,
) {
    use partii_net::Event as E;
    for e in rooms.engine.poll() {
        match e {
            E::Message {
                from,
                reliable,
                bytes,
            } => {
                peers.write(PeerMessage {
                    from,
                    reliable,
                    bytes,
                });
            }
            e => {
                match &e {
                    E::Created { room, .. }
                    | E::Joined { room, .. }
                    | E::RoomUpdated { room }
                    | E::Resumed { room } => rooms.room = Some(room.clone()),
                    E::RoomClosed { .. } => rooms.room = None,
                    _ => {}
                }
                events.write(RoomEvent(e));
            }
        }
    }
}

/// A seat in a server-authoritative match on partii-host. Insert one with
/// [`MatchSeat::join`] once you have an endpoint (from a jam or a matchmaker).
#[cfg(feature = "net")]
#[derive(Resource)]
pub struct MatchSeat {
    seat: partii_net::host::Seat,
    rx: std::sync::Mutex<std::sync::mpsc::Receiver<partii_net::host::SeatEvent>>,
}

#[cfg(feature = "net")]
impl MatchSeat {
    pub fn join(endpoint: &partii_protocol::host::Endpoint) -> Result<MatchSeat, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let seat = partii_net::host::join(endpoint, &Default::default(), move |e| {
            let _ = tx.send(e);
        })?;
        Ok(MatchSeat {
            seat,
            rx: std::sync::Mutex::new(rx),
        })
    }

    /// An input datagram (unreliable; put the last few inputs in each, see `partii_net::netcode`).
    pub fn input(&self, bytes: &[u8]) -> Result<(), String> {
        self.seat.input(bytes)
    }

    pub fn ping(&self, bytes: &[u8]) -> Result<(), String> {
        self.seat.ping(bytes)
    }
}

/// A frame from partii-host (`STATE` or `PONG`, type byte first), or the seat closing.
#[cfg(feature = "net")]
#[derive(Message, Debug, Clone)]
pub struct SeatMessage(pub partii_net::host::SeatEvent);

#[cfg(feature = "net")]
fn pump_seats(seat: Option<Res<MatchSeat>>, mut out: MessageWriter<SeatMessage>) {
    let Some(seat) = seat else { return };
    let rx = seat.rx.lock().unwrap();
    while let Ok(e) = rx.try_recv() {
        out.write(SeatMessage(e));
    }
}

/// The player's partii account as a resource (present when the plugin was given a data folder).
#[cfg(feature = "account")]
#[derive(Resource)]
pub struct PartiiAccount(pub partii_account::Account);

#[cfg(feature = "account")]
impl std::ops::Deref for PartiiAccount {
    type Target = partii_account::Account;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// An achievement this player just unlocked (for an in-game toast).
#[cfg(feature = "account")]
#[derive(Message, Debug, Clone)]
pub struct AchievementUnlocked(pub String);

#[cfg(feature = "account")]
fn pump_account(acct: Option<Res<PartiiAccount>>, mut out: MessageWriter<AchievementUnlocked>) {
    if let Some(a) = acct {
        for id in a.take_unlocked() {
            out.write(AchievementUnlocked(id));
        }
    }
}
