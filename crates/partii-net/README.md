# partii-net

Networking for games on [partii](https://partii.games):

- **Rooms:** create a room, share its join code, and get members, events and recovery (reconnects resume the same seat).
- **Peer-to-peer:** WebRTC data channels between players (unreliable and reliable), with partii's TURN and WebSocket relays as fallbacks.
- **partii-host:** the plain-UDP client transport for server-authoritative matches on partii-host ([`host`] module).
- **Netcode kit:** [`netcode`] re-exports [`partii-netcode`](https://docs.rs/partii-netcode): sequence numbers, acks and redundant inputs over unreliable datagrams.

Native builds use tokio; `wasm32` builds use the browser's WebSocket and WebRTC.

License: MIT OR Apache-2.0.
