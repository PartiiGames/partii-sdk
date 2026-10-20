# Changelog

## 0.1.0 (unreleased)

First public release.

- `partii-protocol`, `partii-netcode`, `partii-client`, `partii-net`, `partii-account`, `partii-content`: moved out of partii's platform repository (formerly `partii-engine` and `partii-api`).
- `partii-net::host`: a seat in a partii-host match over plain UDP (tagged packets, JOIN retries, path-probe answers).
- `partii-server`: the `Game` trait, `export_game!` for the partii-host ABI v0.1, and `testing::Sim`.
- `bevy_partii`: Bevy 0.20 plugin.
- `partii-webrtc`, `partii-rtc`, `partii-rtc-sctp`, `partii-rtc-datachannel`: webrtc-rs 0.21 forks with game-tuned SCTP timers.
