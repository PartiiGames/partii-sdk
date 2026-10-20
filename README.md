# partii SDK

Rust crates for games on [partii](https://partii.games): multiplayer rooms and join codes, peer-to-peer networking with relays, server-authoritative matches on partii-host, player accounts, ownership, saves and achievements.

| Crate | What it's for |
|---|---|
| [`partii`](crates/partii) | One dependency with features for everything below |
| [`bevy_partii`](crates/bevy_partii) | Bevy plugin: rooms, peer messages, match seats, account and achievements as resources and messages |
| [`partii-net`](crates/partii-net) | Rooms, WebRTC peer-to-peer with TURN/WebSocket relay fallback, recovery, the partii-host UDP transport |
| [`partii-server`](crates/partii-server) | Write a partii-host game server: a `Game` trait, `export_game!`, and a native test harness |
| [`partii-netcode`](crates/partii-netcode) | Reliability by redundancy over unreliable datagrams (`no_std`) |
| [`partii-account`](crates/partii-account) | Guest sign-in, ownership (offline-cached), cloud saves, checkout, achievements, analytics |
| [`partii-content`](crates/partii-content) | Mirror a live content channel into a folder |
| [`partii-client`](crates/partii-client) | The public API client (every operation, generated from the OpenAPI spec) |
| [`partii-protocol`](crates/partii-protocol) | Wire types: rooms, `net_version`, partii-host frames, tickets, ABI |
| `partii-webrtc`, `partii-rtc*` | partii's fork of [webrtc-rs](https://github.com/webrtc-rs) with game-tuned timers |

## Start here

- **A Bevy game with rooms:** [`examples/bevy-rooms`](examples/bevy-rooms): guest sign-in, create or join a room by code, two players moving.
- **A game server on partii-host:** [`examples/echo-server`](examples/echo-server): `cargo build --release --target wasm32-unknown-unknown`, and the `.wasm` is your server build.
- **Docs:** <https://partii.games/docs/GUIDE.md>, the API at <https://partii.games/docs/API.md>.

## The netcode rule

In a match, everything is an unreliable datagram (at most 1199 bytes, and at most what each player's path carries). Reliability comes from redundancy: each packet carries the last few inputs or states, plus sequence numbers and acks. `partii-netcode` does this.

## Stability

Semantic versioning. Public structs derive `Default`, so build them with `..Default::default()` and new fields won't break you. Releases are checked with `cargo-semver-checks`.

**MSRV:** Rust 1.85 (bevy_partii: the Bevy version's MSRV, 1.89 for Bevy 0.20).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The `webrtc/` crates are forks of webrtc-rs under the same dual license; their original copyright notices are kept.
