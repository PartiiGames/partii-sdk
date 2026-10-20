//! Two players in one room, moving. Headless (prints instead of drawing), so it
//! runs anywhere; swap `ScheduleRunnerPlugin` for `DefaultPlugins` and draw a
//! sprite per player for the real thing.
//!
//! ```sh
//! export PARTII_API=https://api.partii.games PARTII_KEY=pk_yourstudio_yourgame_... PARTII_GAME=yourgame
//! cargo run -- create          # prints a 5-letter room code
//! cargo run -- join BKTRM      # in another terminal (or on another machine)
//! ```

use bevy_app::{App, AppExit, ScheduleRunnerPlugin, Startup, Update};
use bevy_ecs::prelude::*;
use bevy_partii::net::{Event, BROADCAST};
use bevy_partii::{PartiiAccount, PartiiPlugin, PeerMessage, RoomEvent, Rooms};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Resource)]
struct Mode(Option<String>);

#[derive(Resource, Default)]
struct Players {
    me: (f32, f32),
    others: BTreeMap<u16, (f32, f32)>,
    started: Option<Instant>,
    printed: u32,
}

fn main() {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let mut args = std::env::args().skip(1);
    let mode = match (args.next().as_deref(), args.next()) {
        (Some("join"), Some(code)) => Some(code),
        _ => None,
    };
    let data = std::env::temp_dir().join(format!("bevy-rooms-{}", std::process::id()));
    App::new()
        .add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(16)))
        .add_plugins(
            PartiiPlugin::new(&env("PARTII_API", "https://api.partii.games"), &env("PARTII_KEY", ""), &env("PARTII_GAME", "partii"))
                .client("bevy-rooms/0.1")
                .net_version("bevy-rooms-1")
                .account(data, "Bevy player"),
        )
        .insert_resource(Mode(mode))
        .init_resource::<Players>()
        .add_systems(Startup, start)
        .add_systems(Update, (on_room, on_peer, move_and_send))
        .run();
}

fn start(rooms: Res<Rooms>, mode: Res<Mode>) {
    match &mode.0 {
        Some(code) => {
            rooms.join(code, "joiner");
        }
        None => {
            rooms.create(2, "host");
        }
    }
}

fn on_room(mut events: MessageReader<RoomEvent>, mut players: ResMut<Players>, acct: Option<Res<PartiiAccount>>, mut exit: MessageWriter<AppExit>) {
    for RoomEvent(e) in events.read() {
        match e {
            Event::Created { room, .. } => println!("room {} created: run `cargo run -- join {}`", room.code, room.code),
            Event::Joined { room, .. } => println!("joined room {} as member {}", room.code, room.you),
            Event::MemberJoined { member } => println!("{} joined (member {})", member.name, member.mid),
            Event::Failed { code, message, .. } => {
                println!("failed: {code}: {message}");
                exit.write(AppExit::error());
            }
            _ => {}
        }
        if matches!(e, Event::Created { .. } | Event::Joined { .. }) {
            players.started = Some(Instant::now());
            if let Some(a) = &acct {
                println!("signed in as guest: {}", a.player_id().unwrap_or_else(|| "(still signing in)".into()));
            }
        }
    }
}

fn on_peer(mut msgs: MessageReader<PeerMessage>, mut players: ResMut<Players>) {
    for m in msgs.read() {
        if let (Some(x), Some(y)) = (m.bytes.get(0..4), m.bytes.get(4..8)) {
            let p = (f32::from_le_bytes(x.try_into().unwrap()), f32::from_le_bytes(y.try_into().unwrap()));
            players.others.insert(m.from, p);
        }
    }
}

fn move_and_send(rooms: Res<Rooms>, mut players: ResMut<Players>, mut exit: MessageWriter<AppExit>) {
    let Some(t0) = players.started else { return };
    if rooms.room().is_none() {
        return;
    }
    let t = t0.elapsed().as_secs_f32();
    players.me = (t.cos() * 10.0, t.sin() * 10.0);
    let mut b = players.me.0.to_le_bytes().to_vec();
    b.extend_from_slice(&players.me.1.to_le_bytes());
    // Positions are state: unreliable, and the next one replaces a lost one.
    rooms.send(BROADCAST, false, b);
    if t as u32 > players.printed {
        players.printed = t as u32;
        println!("me ({:.1}, {:.1})  others {:?}", players.me.0, players.me.1, players.others);
    }
    if std::env::var("BEVY_ROOMS_SECS").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|s| t > s) {
        exit.write(AppExit::Success);
    }
}
