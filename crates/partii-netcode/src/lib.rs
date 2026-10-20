//! Reliability by redundancy, for games that send only unreliable datagrams in
//! a match (the partii rule): nothing is retransmitted; instead every packet
//! carries enough recent history that a lost packet rarely matters.
//!
//! - [`Header`]: every packet's sequence number, plus an ack of the newest
//!   packet received and a 32-bit bitfield of the 32 before it.
//! - [`Acks`]: tracks both directions, so a sender learns which of its packets
//!   arrived (to pick a delta baseline) and a receiver drops duplicates.
//! - [`InputHistory`]: a client sends its last N inputs in every packet; the
//!   server applies each input once, in order.
//! - [`delta`] / [`apply_delta`]: send state as the bytes that changed since the
//!   last snapshot the other side acked, or in full when there is none.
//!
//! Wire layouts are little-endian and identical to partii's JS netcode kit.

#![no_std]
extern crate alloc;
use alloc::vec::Vec;

/// Is sequence number `a` newer than `b`, allowing for u16 wraparound?
pub fn seq_newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// Every packet starts with this (8 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Header {
    pub seq: u16,
    /// The newest packet received from the other side.
    pub ack: u16,
    /// Bit i set: packet `ack - 1 - i` was received too.
    pub ack_bits: u32,
}

impl Header {
    pub const LEN: usize = 8;
    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.ack.to_le_bytes());
        out.extend_from_slice(&self.ack_bits.to_le_bytes());
    }
    pub fn read(b: &[u8]) -> Option<(Header, &[u8])> {
        if b.len() < Self::LEN {
            return None;
        }
        let h = Header {
            seq: u16::from_le_bytes([b[0], b[1]]),
            ack: u16::from_le_bytes([b[2], b[3]]),
            ack_bits: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        };
        Some((h, &b[Self::LEN..]))
    }
}

/// One side of a connection: numbers outgoing packets, tracks what arrived, and
/// learns which of its own packets the other side got.
#[derive(Clone, Debug, Default)]
pub struct Acks {
    next_seq: u16,
    /// Newest packet received, and the 32 before it.
    recv_newest: Option<u16>,
    recv_bits: u32,
    /// Newest of our packets the other side has acked.
    pub newest_acked: Option<u16>,
}

impl Acks {
    pub fn new() -> Acks {
        Acks::default()
    }

    /// The header for the next outgoing packet.
    pub fn next_header(&mut self) -> Header {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        Header {
            seq,
            ack: self.recv_newest.unwrap_or(0xFFFF),
            ack_bits: if self.recv_newest.is_some() {
                self.recv_bits
            } else {
                0
            },
        }
    }

    /// Records an incoming packet. Returns false for a duplicate or one older
    /// than the 32-packet window (drop those).
    pub fn receive(&mut self, h: &Header) -> bool {
        match self.recv_newest {
            None => {
                self.recv_newest = Some(h.seq);
                self.recv_bits = 0;
            }
            Some(newest) if seq_newer(h.seq, newest) => {
                let shift = h.seq.wrapping_sub(newest) as u32;
                self.recv_bits = if shift >= 33 {
                    0
                } else {
                    ((self.recv_bits as u64) << shift | 1u64 << (shift - 1)) as u32
                };
                self.recv_newest = Some(h.seq);
            }
            Some(newest) => {
                let back = newest.wrapping_sub(h.seq) as u32;
                if back == 0 || back > 32 || self.recv_bits & (1 << (back - 1)) != 0 {
                    return false;
                }
                self.recv_bits |= 1 << (back - 1);
            }
        }
        if h.ack != 0xFFFF
            && self
                .newest_acked
                .map(|a| seq_newer(h.ack, a))
                .unwrap_or(true)
        {
            self.newest_acked = Some(h.ack);
        }
        true
    }

    /// Did the other side's header acknowledge our packet `seq`?
    pub fn acked_by(h: &Header, seq: u16) -> bool {
        if h.ack == seq {
            return true;
        }
        let back = h.ack.wrapping_sub(seq) as u32;
        (1..=32).contains(&back) && h.ack_bits & (1 << (back - 1)) != 0
    }
}

/// A client's recent inputs; each packet carries the last `n` of them.
///
/// Layout: [count u8] then per input, oldest first: [input seq u16][len u16][bytes].
#[derive(Clone, Debug)]
pub struct InputHistory {
    n: usize,
    next: u16,
    recent: Vec<(u16, Vec<u8>)>,
}

impl InputHistory {
    pub fn new(n: usize) -> InputHistory {
        InputHistory {
            n: n.clamp(1, 255),
            next: 0,
            recent: Vec::new(),
        }
    }

    /// Adds this tick's input and returns the payload to send (it repeats the last n).
    pub fn push(&mut self, input: &[u8]) -> Vec<u8> {
        self.recent.push((self.next, input.to_vec()));
        self.next = self.next.wrapping_add(1);
        if self.recent.len() > self.n {
            self.recent.remove(0);
        }
        let mut out = Vec::new();
        out.push(self.recent.len() as u8);
        for (seq, b) in &self.recent {
            out.extend_from_slice(&seq.to_le_bytes());
            out.extend_from_slice(&(b.len() as u16).to_le_bytes());
            out.extend_from_slice(b);
        }
        out
    }
}

/// The server side: applies each input once, in order.
#[derive(Clone, Debug, Default)]
pub struct InputReceiver {
    last: Option<u16>,
}

impl InputReceiver {
    pub fn new() -> InputReceiver {
        InputReceiver::default()
    }

    /// Calls `apply(seq, input)` for each input in the payload not seen before, oldest first.
    /// Returns how many inputs were lost for good (gaps the redundancy didn't cover).
    pub fn receive(&mut self, payload: &[u8], mut apply: impl FnMut(u16, &[u8])) -> u32 {
        let Some((&count, mut rest)) = payload.split_first() else {
            return 0;
        };
        let mut lost = 0;
        for _ in 0..count {
            if rest.len() < 4 {
                break;
            }
            let seq = u16::from_le_bytes([rest[0], rest[1]]);
            let len = u16::from_le_bytes([rest[2], rest[3]]) as usize;
            if rest.len() < 4 + len {
                break;
            }
            let input = &rest[4..4 + len];
            rest = &rest[4 + len..];
            if self.last.map(|l| seq_newer(seq, l)).unwrap_or(true) {
                if let Some(l) = self.last {
                    lost += seq.wrapping_sub(l) as u32 - 1;
                }
                apply(seq, input);
                self.last = Some(seq);
            }
        }
        lost
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_varint(b: &[u8], i: &mut usize) -> Option<u32> {
    let mut v = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= ((byte & 0x7F) as u32) << shift;
        if byte < 0x80 {
            return Some(v);
        }
    }
    None
}

/// The bytes of `current` that differ from `baseline`.
///
/// Layout: [new length varint] then runs of [unchanged count varint][changed count varint][changed bytes].
/// With an empty baseline this is the full state.
pub fn delta(baseline: &[u8], current: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, current.len() as u32);
    let same = |i: usize| i < baseline.len() && baseline[i] == current[i];
    let mut i = 0;
    while i < current.len() {
        let start = i;
        while i < current.len() && same(i) {
            i += 1;
        }
        if i == current.len() {
            break;
        }
        let skip = i - start;
        let run_start = i;
        // A changed run ends at 4+ unchanged bytes in a row (shorter gaps are cheaper inline).
        while i < current.len() && !(same(i) && (i..(i + 4).min(current.len())).all(same)) {
            i += 1;
        }
        put_varint(&mut out, skip as u32);
        put_varint(&mut out, (i - run_start) as u32);
        out.extend_from_slice(&current[run_start..i]);
    }
    out
}

/// Rebuilds the state from a baseline and a [`delta`] of it. None if the delta is malformed.
pub fn apply_delta(baseline: &[u8], d: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    let len = get_varint(d, &mut i)? as usize;
    let mut out: Vec<u8> = (0..len)
        .map(|k| baseline.get(k).copied().unwrap_or(0))
        .collect();
    let mut pos = 0usize;
    while i < d.len() {
        pos += get_varint(d, &mut i)? as usize;
        let run = get_varint(d, &mut i)? as usize;
        if pos + run > len || i + run > d.len() {
            return None;
        }
        out[pos..pos + run].copy_from_slice(&d[i..i + run]);
        i += run;
        pos += run;
    }
    Some(out)
}

/// Server side of state sync: remembers recent snapshots by packet seq and
/// deltas each new one against the newest snapshot the client acked.
///
/// Payload: [baseline seq u16, 0xFFFF = none][delta].
#[derive(Clone, Debug, Default)]
pub struct StateSender {
    sent: Vec<(u16, Vec<u8>)>,
}

impl StateSender {
    pub fn new() -> StateSender {
        StateSender::default()
    }

    /// `seq` is this packet's header seq; `acks` is this connection's ack state.
    pub fn encode(&mut self, seq: u16, acks: &Acks, state: &[u8]) -> Vec<u8> {
        let base = acks
            .newest_acked
            .and_then(|a| self.sent.iter().find(|(s, _)| *s == a));
        let mut out = Vec::new();
        match base {
            Some((s, b)) => {
                out.extend_from_slice(&s.to_le_bytes());
                out.extend_from_slice(&delta(b, state));
            }
            None => {
                out.extend_from_slice(&0xFFFFu16.to_le_bytes());
                out.extend_from_slice(&delta(&[], state));
            }
        }
        self.sent.push((seq, state.to_vec()));
        if self.sent.len() > 64 {
            self.sent.remove(0);
        }
        out
    }
}

/// Client side of state sync: keeps the snapshots it received, by packet seq.
#[derive(Clone, Debug, Default)]
pub struct StateReceiver {
    got: Vec<(u16, Vec<u8>)>,
}

impl StateReceiver {
    pub fn new() -> StateReceiver {
        StateReceiver::default()
    }

    /// Decodes the state carried by packet `seq`. None when its baseline is gone (wait for the next).
    pub fn decode(&mut self, seq: u16, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 2 {
            return None;
        }
        let base_seq = u16::from_le_bytes([payload[0], payload[1]]);
        let state = if base_seq == 0xFFFF {
            apply_delta(&[], &payload[2..])?
        } else {
            let (_, base) = self.got.iter().find(|(s, _)| *s == base_seq)?;
            apply_delta(base, &payload[2..])?
        };
        self.got.push((seq, state.clone()));
        if self.got.len() > 64 {
            self.got.remove(0);
        }
        Some(state)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::vec;

    #[test]
    fn acks_track_received_packets() {
        let mut a = Acks::new();
        let mut b = Acks::new();
        let h0 = a.next_header();
        let _h1 = a.next_header(); // lost
        let h2 = a.next_header();
        assert!(b.receive(&h0));
        assert!(b.receive(&h2));
        assert!(!b.receive(&h2), "duplicate");
        let reply = b.next_header();
        assert_eq!(reply.ack, 2);
        assert!(Acks::acked_by(&reply, 2));
        assert!(!Acks::acked_by(&reply, 1));
        assert!(Acks::acked_by(&reply, 0));
        a.receive(&reply);
        assert_eq!(a.newest_acked, Some(2));
    }

    #[test]
    fn seq_wraps() {
        assert!(seq_newer(0, 0xFFFF));
        assert!(!seq_newer(0xFFFF, 0));
        let mut b = Acks::new();
        assert!(b.receive(&Header {
            seq: 0xFFFE,
            ack: 0xFFFF,
            ack_bits: 0
        }));
        assert!(b.receive(&Header {
            seq: 1,
            ack: 0xFFFF,
            ack_bits: 0
        }));
        let h = b.next_header();
        assert!(Acks::acked_by(&h, 0xFFFE) && Acks::acked_by(&h, 1) && !Acks::acked_by(&h, 0));
    }

    #[test]
    fn redundant_inputs_survive_loss() {
        let mut hist = InputHistory::new(4);
        let mut rx = InputReceiver::new();
        let mut applied = vec![];
        for i in 0u8..21 {
            let p = hist.push(&[i]);
            if i % 3 == 1 {
                continue; // every third packet lost
            }
            let lost = rx.receive(&p, |_, b| applied.push(b[0]));
            assert_eq!(lost, 0);
        }
        assert_eq!(applied, (0u8..21).collect::<Vec<_>>());
    }

    #[test]
    fn deltas_round_trip() {
        let a: Vec<u8> = (0..200).map(|i| i as u8).collect();
        let mut b = a.clone();
        b[10] = 0;
        b[11] = 1;
        b[150] = 9;
        b.extend_from_slice(&[7, 7, 7]);
        let d = delta(&a, &b);
        assert!(d.len() < 20, "small delta: {}", d.len());
        assert_eq!(apply_delta(&a, &d).unwrap(), b);
        let shorter = &b[..50];
        assert_eq!(apply_delta(&b, &delta(&b, shorter)).unwrap(), shorter);
        assert_eq!(apply_delta(&[], &delta(&[], &b)).unwrap(), b);
    }

    #[test]
    fn state_sync_uses_acked_baseline() {
        let (mut srv, mut cli) = (Acks::new(), Acks::new());
        let (mut tx, mut rx) = (StateSender::new(), StateReceiver::new());
        let s1 = vec![1u8; 100];
        let h = srv.next_header();
        let p = tx.encode(h.seq, &srv, &s1);
        assert_eq!(u16::from_le_bytes([p[0], p[1]]), 0xFFFF);
        cli.receive(&h);
        assert_eq!(rx.decode(h.seq, &p).unwrap(), s1);
        srv.receive(&cli.next_header());
        let mut s2 = s1.clone();
        s2[5] = 2;
        let h = srv.next_header();
        let p = tx.encode(h.seq, &srv, &s2);
        assert!(p.len() < 10);
        assert_eq!(rx.decode(h.seq, &p).unwrap(), s2);
    }
}
