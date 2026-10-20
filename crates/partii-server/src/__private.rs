//! What [`export_game!`](crate::export_game) expands to. Not public API.
//!
//! The game lives behind a raw pointer, not a `RefCell`: when partii-host stops
//! a call that ran past the tick budget, a `RefCell` borrow would stay taken and
//! every later call would panic. A stopped call just abandons its `&mut`; the
//! next tick enters again. (The host is single-threaded per match.)

use crate::{Game, Host, JoinInfo, PlayerId};
use core::cell::Cell;

/// A buffer the host fills and passes back to one of the calls below, which take ownership of it.
pub fn alloc(len: usize) -> *mut u8 {
    Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
}

fn take(ptr: *mut u8, len: usize) -> Box<[u8]> {
    if len == 0 || ptr.is_null() {
        return Box::new([]);
    }
    // SAFETY: the host passes back exactly a buffer it got from `alloc(len)`.
    unsafe { Box::from_raw(core::ptr::slice_from_raw_parts_mut(ptr, len)) }
}

fn with<G: Game>(slot: &Cell<*mut G>, f: impl FnOnce(&mut G, &mut Host)) {
    let g = slot.get();
    if g.is_null() {
        return;
    }
    // SAFETY: one thread per match, and calls never nest; see the module docs.
    f(unsafe { &mut *g }, &mut Host::__new())
}

pub fn init<G: Game>(slot: &Cell<*mut G>, seed_lo: u32, seed_hi: u32) {
    std::panic::set_hook(Box::new(|p| {
        let at = p
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        crate::backend::log(&format!(
            "PANIC {}{at}",
            p.payload_as_str().unwrap_or("(no message)")
        ));
    }));
    let g = Box::new(G::init((seed_hi as u64) << 32 | seed_lo as u64));
    slot.set(Box::into_raw(g));
}

pub fn on_join<G: Game>(slot: &Cell<*mut G>, player: u32, ptr: *mut u8, len: usize) {
    let raw = take(ptr, len);
    let info: JoinInfo = serde_json::from_slice(&raw).unwrap_or_default();
    with(slot, |g, h| g.on_join(h, PlayerId(player), info))
}

pub fn on_leave<G: Game>(slot: &Cell<*mut G>, player: u32) {
    with(slot, |g, h| g.on_leave(h, PlayerId(player)))
}

pub fn on_input<G: Game>(slot: &Cell<*mut G>, player: u32, ptr: *mut u8, len: usize) {
    let input = take(ptr, len);
    with(slot, |g, h| g.on_input(h, PlayerId(player), &input))
}

pub fn tick<G: Game>(slot: &Cell<*mut G>, now_ms: f64) {
    with(slot, |g, h| g.tick(h, now_ms))
}
