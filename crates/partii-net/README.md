# partii-net

Networking for games on [partii](https://partii.games):

- **Rooms:** create a room, share its join code, and get members, events and recovery (reconnects resume the same seat).
- **Peer-to-peer:** WebRTC data channels between players (unreliable and reliable), with partii's TURN and WebSocket relays as fallbacks.
- **partii-host:** the plain-UDP client transport for server-authoritative matches on partii-host ([`host`] module).
- **Netcode kit:** [`netcode`] re-exports [`partii-netcode`](https://docs.rs/partii-netcode): sequence numbers, acks and redundant inputs over unreliable datagrams.

## Away (phones in the background)

Opt in to partii's away grace with `Config { away: true, .. }` (`"away": true` in `WebEngine`'s JSON config); it's off by default, so an engine behaves exactly as before. Opted in, when a phone is backgrounded and its socket drops, its seat is held for the room's `away_grace_ms` (default 30 s; set it with `away_grace_ms` in `create_with` settings or an `update`) and the reconnect lands in the same seat. Call `engine.away()` when the app goes to the background and `engine.back()` when it returns. Others see `Event::MemberAway { mid, reason, until, .. }` and `Event::MemberBack { mid }`; a member not back in time leaves with `MemberLeft { reason: "timeout" }`. Off, a dropped seat gets the 20 s socket grace, others' presence arrives as `MemberSuspended` / `MemberResumed`, and `away()` / `back()` answer `Failed { code: "not_enabled" }` without sending anything. Protocol: <https://partii.games/docs/PROTOCOL.md> ("Away").

## Topology: mesh or star (host-only links)

By default a room is a **mesh**: every member links to every other (a WebRTC connection per pair, TURN and direct). A room where only the host link carries play (a host screen with phone controllers) can be a **star** instead: each member links only to the room's host (`room.host`, the member that created it) and the host links to everyone. A room of `n` keeps `n - 1` links instead of `n (n - 1) / 2`: a host, six phones and a guest go from 28 peer connections to 7, and each phone keeps one.

```rust
use partii_net::Topology;
// The host picks it when it creates the room (merge it into any other settings):
engine.create_with("tankii", "game", "Host", meta, json!({"max_members": 8, "topology": "star"}));
// or Topology::Star.settings() for just {"topology": "star"}.
```

- **The creator decides.** Its engine tells every member the room's topology over signaling as it links to them, and members follow it whatever they asked for: a phone page that calls plain `join` becomes a star member of a star room, and a member that asked for a star in a mesh room meshes. Members hold their member-to-member links (not the host link) until they hear it, at most `TOPOLOGY_WAIT_MS` (2 s); a host on an older SDK says nothing, and then each member's own choice stands. A `topology` field on the room itself (when partii's edge carries one) wins at once.
- **Sends in a star.** A member's `BROADCAST` reaches the host only. A send from a member to another member has no link to go on: it is dropped and `Event::Error` says so (once per member per room). Send to the host and have the game forward it. The host sends to anyone.
- **Switching.** The host can switch a running room with `update(json!({"topology": "star"}))` (or `"mesh"`); members follow. A host page that reloads as a fresh engine is back on a mesh until it sends that update.
- **Host migration.** partii's edge doesn't migrate hosts: when the host leaves, the room closes (`RoomClosed { reason: "host_left" }`, or `"host_lost"` while it may reclaim the code). Should a room ever name a new host (`room_updated` with another `host`), the star rebuilds around it: the new host links to everyone, the others to it only.
- `engine.stats()["topology"]` says which one an engine follows; `Core::topology()` / `Core::linked(mid)` too.

Native builds use tokio; `wasm32` builds use the browser's WebSocket and WebRTC.

License: MIT OR Apache-2.0.
