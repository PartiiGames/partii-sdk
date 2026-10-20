//! The partii-host game-server ABI (v0.1): what a `wasm32-unknown-unknown`
//! module exports and imports. [`partii-server`](https://docs.rs/partii-server)
//! implements it for you; these are the names and limits.

/// The ABI version these names describe.
pub const VERSION: &str = "0.1";

/// The import module the host provides.
pub const IMPORT_MODULE: &str = "env";

/// What a module exports.
pub mod exports {
    pub const MEMORY: &str = "memory";
    pub const ALLOC: &str = "alloc";
    pub const INIT: &str = "init";
    pub const ON_JOIN: &str = "on_join";
    pub const ON_LEAVE: &str = "on_leave";
    pub const ON_INPUT: &str = "on_input";
    pub const TICK: &str = "tick";
}

/// What the host provides (module `env`).
pub mod imports {
    pub const NOW_MS: &str = "now_ms";
    pub const SEND: &str = "send";
    pub const BROADCAST: &str = "broadcast";
    pub const LOG: &str = "log";
    pub const RANDOM_U32: &str = "random_u32";
    pub const MAX_DATAGRAM: &str = "max_datagram";
}

/// `send`/`broadcast` result: sent (or queued).
pub const SEND_OK: i32 = 0;
/// `send`/`broadcast` result: an unreliable send over [`MAX_PAYLOAD`]; dropped.
pub const SEND_OVERSIZE: i32 = -1;
/// `send` result: no such player.
pub const SEND_NO_PLAYER: i32 = -2;

/// The largest unreliable payload any path carries (1200 with partii's frame byte).
pub const MAX_PAYLOAD: usize = 1199;
/// What to assume for a player whose path hasn't been measured yet.
pub const UNMEASURED_PAYLOAD: usize = 479;

/// Default per-tick CPU budget (ms); a match may ask for 1–50.
pub const DEFAULT_STEP_BUDGET_MS: u32 = 8;
/// Compile + instantiate + `init` get this long (ms), outside the tick budget.
pub const START_LIMIT_MS: u32 = 2000;
/// Default tick rate; a match may ask for a divisor of 60.
pub const DEFAULT_TICK_HZ: u32 = 60;
