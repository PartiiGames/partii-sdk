# Changelog

## Unreleased

- `partii-client`: the referral portal (`referralsVisit`, `referralsHistory`, `referralsDashboard`; CLI `partii referrals visit|history|dashboard`) and a project's Stripe dashboard link (`payoutsDashboard`; CLI `partii payouts dashboard`). Additive.

## 0.1.0 (unreleased)

First public release.

- `partii-protocol`, `partii-netcode`, `partii-client`, `partii-net`, `partii-account`, `partii-content`: moved out of partii's platform repository (formerly `partii-engine` and `partii-api`).
- `partii-net::host`: a seat in a partii-host match over plain UDP (tagged packets, JOIN retries, path-probe answers). `join_with` sets how long a quiet host may stay quiet before the seat closes, so a client notices a dead host fast and asks for the match's new endpoint (partii's directory moves matches off dead hosts, at a new generation).
- `partii-server`: the `Game` trait, `export_game!` for the partii-host ABI v0.1, and `testing::Sim`.
- `bevy_partii`: Bevy 0.20 plugin.
- `partii-client`: hosted servers (`serversStart`, `serversSeat`, `serversStop`, `serversList`) and server builds (`buildsUpload` with `kind=server` and its benchmark settings). Additive.
- `partii-webrtc`, `partii-rtc`, `partii-rtc-sctp`, `partii-rtc-datachannel`: webrtc-rs 0.21 forks with game-tuned SCTP timers.
