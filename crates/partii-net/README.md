# partii-net

Networking for games on [partii](https://partii.games):

- **Rooms:** create a room, share its join code, and get members, events and recovery (reconnects resume the same seat).
- **Peer-to-peer:** WebRTC data channels between players (unreliable and reliable), with partii's TURN and WebSocket relays as fallbacks.
- **partii-host:** the plain-UDP client transport for server-authoritative matches on partii-host ([`host`] module).
- **Netcode kit:** [`netcode`] re-exports [`partii-netcode`](https://docs.rs/partii-netcode): sequence numbers, acks and redundant inputs over unreliable datagrams.

## Away (phones in the background)

Opt in to partii's away grace with `Config { away: true, .. }` (`"away": true` in `WebEngine`'s JSON config); it's off by default, so an engine behaves exactly as before. Opted in, when a phone is backgrounded and its socket drops, its seat is held for the room's `away_grace_ms` (default 30 s; set it with `away_grace_ms` in `create_with` settings or an `update`) and the reconnect lands in the same seat. Call `engine.away()` when the app goes to the background and `engine.back()` when it returns. Others see `Event::MemberAway { mid, reason, until, .. }` and `Event::MemberBack { mid }`; a member not back in time leaves with `MemberLeft { reason: "timeout" }`. Off, a dropped seat gets the 20 s socket grace, others' presence arrives as `MemberSuspended` / `MemberResumed`, and `away()` / `back()` answer `Failed { code: "not_enabled" }` without sending anything. Protocol: <https://partii.games/docs/PROTOCOL.md> ("Away").

Native builds use tokio; `wasm32` builds use the browser's WebSocket and WebRTC.

License: MIT OR Apache-2.0.
