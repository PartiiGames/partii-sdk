//! Where a [`Host`](crate::Host) call goes: partii-host's imports on wasm32,
//! the simulated host of [`testing::Sim`](crate::testing::Sim) elsewhere.

#[cfg(target_arch = "wasm32")]
mod imp {
    #[link(wasm_import_module = "env")]
    extern "C" {
        #[link_name = "now_ms"]
        fn host_now_ms() -> f64;
        #[link_name = "send"]
        fn host_send(player: u32, ptr: *const u8, len: usize, reliable: u32) -> i32;
        #[link_name = "broadcast"]
        fn host_broadcast(ptr: *const u8, len: usize, reliable: u32) -> i32;
        #[link_name = "log"]
        fn host_log(ptr: *const u8, len: usize);
        #[link_name = "random_u32"]
        fn host_random_u32() -> u32;
        #[link_name = "max_datagram"]
        fn host_max_datagram(player: u32) -> u32;
    }
    // SAFETY (all): partii-host provides these imports with exactly these signatures (ABI v0.1).
    pub fn now_ms() -> f64 {
        unsafe { host_now_ms() }
    }
    pub fn send(player: u32, b: &[u8], reliable: bool) -> i32 {
        unsafe { host_send(player, b.as_ptr(), b.len(), reliable as u32) }
    }
    pub fn broadcast(b: &[u8], reliable: bool) -> i32 {
        unsafe { host_broadcast(b.as_ptr(), b.len(), reliable as u32) }
    }
    pub fn log(s: &str) {
        unsafe { host_log(s.as_ptr(), s.len()) }
    }
    pub fn random_u32() -> u32 {
        unsafe { host_random_u32() }
    }
    pub fn max_datagram(player: u32) -> u32 {
        unsafe { host_max_datagram(player) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::cell::RefCell;

    /// The simulated host, installed by `testing::Sim` around each call.
    pub trait Backend {
        fn now_ms(&self) -> f64;
        fn send(&mut self, player: u32, b: &[u8], reliable: bool) -> i32;
        fn broadcast(&mut self, b: &[u8], reliable: bool) -> i32;
        fn log(&mut self, s: &str);
        fn random_u32(&mut self) -> u32;
        fn max_datagram(&self, player: u32) -> u32;
    }

    thread_local! {
        pub static CURRENT: RefCell<Option<*mut dyn Backend>> = const { RefCell::new(None) };
    }

    fn with<R>(f: impl FnOnce(&mut dyn Backend) -> R, none: R) -> R {
        match CURRENT.with(|c| *c.borrow()) {
            // SAFETY: Sim installs a pointer to itself only for the duration of a Game call on this thread.
            Some(b) => f(unsafe { &mut *b }),
            None => none,
        }
    }
    pub fn now_ms() -> f64 {
        with(|b| b.now_ms(), 0.0)
    }
    pub fn send(player: u32, bytes: &[u8], reliable: bool) -> i32 {
        with(|b| b.send(player, bytes, reliable), 0)
    }
    pub fn broadcast(bytes: &[u8], reliable: bool) -> i32 {
        with(|b| b.broadcast(bytes, reliable), 0)
    }
    pub fn log(s: &str) {
        with(|b| b.log(s), ())
    }
    pub fn random_u32() -> u32 {
        with(|b| b.random_u32(), 4)
    }
    pub fn max_datagram(player: u32) -> u32 {
        with(|b| b.max_datagram(player), 0)
    }
}

pub use imp::*;
