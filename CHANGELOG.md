# Changelog

## 0.2.0 (unreleased)

Every SDK crate moves to 0.2.0 (one workspace version). Minor bump: new public fields and enum variants.

- `partii-net`: the **away grace** (PROTOCOL.md "Away"), **opt-in**: `Config { away: true, .. }` (JSON `"away": true` for `WebEngine`). Off by default, so a game that only bumps the SDK sees exactly what it saw before (hello `caps: ["reset"]`, the 20 s grace, `MemberSuspended` / `MemberResumed`). Opted in (hello `caps: ["reset", "away"]`), a dropped socket (a phone frozen in the background) keeps its seat for the room's `away_grace_ms` (default 30 s, 5 s to 5 min) instead of the 20 s socket grace, and comes back into the same seat on the same `conn`.
  - New events `MemberAway { mid, reason: "hint" | "dropped", until, grace_ms }` and `MemberBack { mid }` (a `reset` back restarts the reliable link, as `member_resumed {reset}` did). Opted-in engines get these **instead of** `MemberSuspended` / `MemberResumed`; those still arrive from an edge or room that sends the old frames. The ws path to an away member closes (the edge drops packets to it; reliable sends wait and go out when it's back) and its WebRTC paths restart on `MemberBack`.
  - `away()` / `back()` on `Core`, the native and web `Engine` and `WebEngine`: hint `{"t":"away"}` when the app goes to the background and `{"t":"back"}` when it returns on a socket that survived (a reconnect is back on its own). `away()`'s reply is the new `Event::AwayHeld { req, reason, until, grace_ms }`; `Core::away_hold()` reads it back. An away engine whose socket stays up past `until` gets `RoomClosed { reason: "timeout" }` (the seat is gone; join again).
  - `away_grace_ms` in `create_with` settings and `update` patches (host only).
  - Without `Config::away`, `away()` / `back()` send nothing and answer `Failed { code: "not_enabled" }`.
  - `Config` deserializes with defaults for every missing field, so `WebEngine`'s `{edge, key}` config works as documented. Code that builds a `Config` literally needs `..Default::default()` (or the new `away` field).
- `partii-protocol`: `Member.away: Option<Away>` (`{reason, until}`), `Room.away_grace_ms`, the `Away` type and `AWAY_CAP`, `DEFAULT_AWAY_GRACE_MS`, `MIN_AWAY_GRACE_MS`, `MAX_AWAY_GRACE_MS`, `AWAY_HINT`, `AWAY_DROPPED`.
- `partii-client`: the referral portal (`referralsVisit`, `referralsHistory`, `referralsDashboard`; CLI `partii referrals visit|history|dashboard`) a project's Stripe dashboard link (`payoutsDashboard`; CLI `partii payouts dashboard`), and the sales of the projects a player runs as a developer (`playerProjectsEarnings`; CLI `partii earnings projects`). Additive.

## 0.1.0 (unreleased)

First public release.

- `partii-protocol`, `partii-netcode`, `partii-client`, `partii-net`, `partii-account`, `partii-content`: moved out of partii's platform repository (formerly `partii-engine` and `partii-api`).
- `partii-net::host`: a seat in a partii-host match over plain UDP (tagged packets, JOIN retries, path-probe answers). `join_with` sets how long a quiet host may stay quiet before the seat closes, so a client notices a dead host fast and asks for the match's new endpoint (partii's directory moves matches off dead hosts, at a new generation).
- `partii-server`: the `Game` trait, `export_game!` for the partii-host ABI v0.1, and `testing::Sim`.
- `bevy_partii`: Bevy 0.20 plugin.
- `partii-client`: hosted servers (`serversStart`, `serversSeat`, `serversStop`, `serversList`) and server builds (`buildsUpload` with `kind=server` and its benchmark settings). Additive.
- `partii-webrtc`, `partii-rtc`, `partii-rtc-sctp`, `partii-rtc-datachannel`: webrtc-rs 0.21 forks with game-tuned SCTP timers.
