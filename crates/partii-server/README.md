# partii-server

Write server-authoritative game servers for [partii-host](https://partii.games) in Rust.

1. Implement `Game` (`init`, `on_join`, `on_leave`, `on_input`, `tick`).
2. `partii_server::export_game!(MyGame);`
3. `cargo build --release --target wasm32-unknown-unknown` with `crate-type = ["cdylib"]`. The `.wasm` is your server build.

Test it natively with `partii_server::testing::Sim`: a simulated host with players, packet loss and per-player datagram limits.

In a match everything is an unreliable datagram (≤ 1199 bytes, and ≤ `Host::max_datagram(player)`); use the re-exported `netcode` kit for sequence numbers, acks and redundant inputs.

License: MIT OR Apache-2.0.
