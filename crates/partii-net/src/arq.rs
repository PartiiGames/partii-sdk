//! Reliable, ordered delivery over any datagram path. Messages are cut into fragments; every fragment has its own
//! sequence number, is acked (cumulative + 64-bit bitfield), retransmitted on
//! timeout, and delivered strictly in order, so switching paths mid-stream
//! never loses or reorders a reliable message.
//!
//! Sans-IO: `send`/`on_data`/`on_ack`/`poll` take the time and return what
//! to put on the wire; the link decides which path carries it.

use std::collections::{BTreeMap, VecDeque};

/// Largest fragment payload: keeps each datagram under a safe path MTU.
pub const FRAG: usize = 1150;
/// Fragments in flight before `send` starts queueing.
pub const WINDOW: u32 = 2048;
const MIN_RTO_MS: f64 = 40.0;
const MAX_RTO_MS: f64 = 1000.0;

pub const START: u8 = 1;
pub const END: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub seq: u32,
    pub flags: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    /// Every seq below this has arrived.
    pub next: u32,
    /// Bit i set: seq `next + 1 + i` has arrived too.
    pub bits: u64,
}

struct InFlight {
    frag: Fragment,
    sent_at: f64,
    tries: u32,
}

/// The sending half.
pub struct Sender {
    next_seq: u32,
    queue: VecDeque<Fragment>,
    flight: BTreeMap<u32, InFlight>,
    /// Smoothed RTT and variance (ms), from acked first transmissions.
    srtt: Option<f64>,
    rttvar: f64,
    pub retransmits: u64,
}

impl Default for Sender {
    fn default() -> Self {
        Self {
            next_seq: 0,
            queue: VecDeque::new(),
            flight: BTreeMap::new(),
            srtt: None,
            rttvar: 0.0,
            retransmits: 0,
        }
    }
}

impl Sender {
    /// Queue one message (split into fragments).
    pub fn send(&mut self, msg: &[u8]) {
        let chunks: Vec<&[u8]> = if msg.is_empty() {
            vec![&[][..]]
        } else {
            msg.chunks(FRAG).collect()
        };
        let n = chunks.len();
        for (i, c) in chunks.into_iter().enumerate() {
            let mut flags = 0;
            if i == 0 {
                flags |= START;
            }
            if i + 1 == n {
                flags |= END;
            }
            self.queue.push_back(Fragment {
                seq: self.next_seq,
                flags,
                payload: c.to_vec(),
            });
            self.next_seq = self.next_seq.wrapping_add(1);
        }
    }

    pub fn rto(&self) -> f64 {
        match self.srtt {
            Some(s) => (s + 4.0 * self.rttvar).clamp(MIN_RTO_MS, MAX_RTO_MS),
            None => 200.0,
        }
    }

    pub fn in_flight(&self) -> usize {
        self.flight.len() + self.queue.len()
    }

    /// Fragments due on the wire now: new ones (within the window) and
    /// timed-out ones again.
    pub fn poll(&mut self, now: f64) -> Vec<Fragment> {
        let mut out = Vec::new();
        let rto = self.rto();
        for f in self.flight.values_mut() {
            let backoff = rto * 2f64.powi(f.tries.saturating_sub(1).min(4) as i32);
            if now - f.sent_at >= backoff.min(MAX_RTO_MS * 2.0) {
                f.sent_at = now;
                f.tries += 1;
                self.retransmits += 1;
                out.push(f.frag.clone());
            }
        }
        while (self.flight.len() as u32) < WINDOW {
            let Some(f) = self.queue.pop_front() else {
                break;
            };
            self.flight.insert(
                f.seq,
                InFlight {
                    frag: f.clone(),
                    sent_at: now,
                    tries: 1,
                },
            );
            out.push(f);
        }
        out
    }

    pub fn on_ack(&mut self, ack: Ack, now: f64) {
        let mut acked: Vec<u32> = self
            .flight
            .keys()
            .copied()
            .filter(|s| seq_lt(*s, ack.next))
            .collect();
        for i in 0..64 {
            if ack.bits & (1 << i) != 0 {
                acked.push(ack.next.wrapping_add(1 + i));
            }
        }
        for s in acked {
            if let Some(f) = self.flight.remove(&s) {
                if f.tries == 1 {
                    self.sample(now - f.sent_at);
                }
            }
        }
    }

    fn sample(&mut self, rtt: f64) {
        match self.srtt {
            None => {
                self.srtt = Some(rtt);
                self.rttvar = rtt / 2.0;
            }
            Some(s) => {
                self.rttvar = 0.75 * self.rttvar + 0.25 * (s - rtt).abs();
                self.srtt = Some(0.875 * s + 0.125 * rtt);
            }
        }
    }

    /// After a path switch, resend everything unacked right away instead of
    /// waiting out the old path's timers.
    pub fn rush(&mut self) {
        for f in self.flight.values_mut() {
            f.sent_at = f64::NEG_INFINITY;
        }
    }
}

/// The receiving half.
#[derive(Default)]
pub struct Receiver {
    next: u32,
    held: BTreeMap<u32, Fragment>,
    partial: Vec<u8>,
    in_message: bool,
    /// An ack is owed since the last `ack()`.
    pub ack_due: bool,
}

impl Receiver {
    /// Take one fragment; returns the messages it completes, in order.
    pub fn on_data(&mut self, f: Fragment) -> Vec<Vec<u8>> {
        self.ack_due = true;
        if seq_lt(f.seq, self.next) || self.held.contains_key(&f.seq) {
            return Vec::new(); // duplicate
        }
        if f.seq.wrapping_sub(self.next) > WINDOW * 4 {
            return Vec::new(); // absurdly far ahead: not ours
        }
        self.held.insert(f.seq, f);
        let mut done = Vec::new();
        while let Some(f) = self.held.remove(&self.next) {
            self.next = self.next.wrapping_add(1);
            if f.flags & START != 0 {
                self.partial.clear();
                self.in_message = true;
            }
            if self.in_message {
                self.partial.extend_from_slice(&f.payload);
            }
            if f.flags & END != 0 && self.in_message {
                done.push(std::mem::take(&mut self.partial));
                self.in_message = false;
            }
        }
        done
    }

    pub fn ack(&mut self) -> Ack {
        self.ack_due = false;
        let mut bits = 0u64;
        for s in self.held.keys() {
            let d = s.wrapping_sub(self.next);
            if (1..=64).contains(&d) {
                bits |= 1 << (d - 1);
            }
        }
        Ack {
            next: self.next,
            bits,
        }
    }
}

/// Wrapping sequence comparison: is `a` before `b`?
pub fn seq_lt(a: u32, b: u32) -> bool {
    (b.wrapping_sub(a) as i32) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lossy, reordering wire between a sender and a receiver, with a
    /// deterministic PRNG.
    struct Wire {
        rng: u64,
        loss: f64,
        delay: f64,
        jitter: f64,
        q: Vec<(f64, Fragment)>,
        acks: Vec<(f64, Ack)>,
    }

    impl Wire {
        fn new(seed: u64, loss: f64, delay: f64, jitter: f64) -> Self {
            Self {
                rng: seed,
                loss,
                delay,
                jitter,
                q: vec![],
                acks: vec![],
            }
        }
        fn rand(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            (self.rng % 1_000_000) as f64 / 1_000_000.0
        }
        fn push(&mut self, now: f64, f: Fragment) {
            if self.rand() < self.loss {
                return;
            }
            let at = now + self.delay + self.jitter * self.rand();
            self.q.push((at, f.clone()));
            if self.rand() < 0.02 {
                self.q.push((at + 3.0, f)); // a duplicate
            }
        }
        fn push_ack(&mut self, now: f64, a: Ack) {
            if self.rand() < self.loss {
                return;
            }
            let at = now + self.delay + self.jitter * self.rand();
            self.acks.push((at, a));
        }
    }

    fn run(
        msgs: &[Vec<u8>],
        seed: u64,
        loss: f64,
        delay: f64,
        jitter: f64,
    ) -> (Vec<Vec<u8>>, f64, u64) {
        let mut tx = Sender::default();
        let mut rx = Receiver::default();
        let mut wire = Wire::new(seed, loss, delay, jitter);
        for m in msgs {
            tx.send(m);
        }
        let mut got = Vec::new();
        let mut now = 0.0;
        while got.len() < msgs.len() && now < 120_000.0 {
            for f in tx.poll(now) {
                wire.push(now, f);
            }
            let (due, rest): (Vec<_>, Vec<_>) = wire.q.drain(..).partition(|(t, _)| *t <= now);
            wire.q = rest;
            for (_, f) in due {
                got.extend(rx.on_data(f));
            }
            if rx.ack_due {
                let a = rx.ack();
                wire.push_ack(now, a);
            }
            let (due, rest): (Vec<_>, Vec<_>) = wire.acks.drain(..).partition(|(t, _)| *t <= now);
            wire.acks = rest;
            for (_, a) in due {
                tx.on_ack(a, now);
            }
            now += 5.0;
        }
        (got, now, tx.retransmits)
    }

    fn msgs(n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| (0..(i * 37 % 4000)).map(|b| (b ^ i) as u8).collect())
            .collect()
    }

    #[test]
    fn delivers_everything_in_order_on_a_clean_wire() {
        let m = msgs(200);
        let (got, _, re) = run(&m, 1, 0.0, 20.0, 0.0);
        assert_eq!(got, m);
        assert_eq!(re, 0);
    }

    #[test]
    fn survives_loss_reordering_and_duplicates() {
        for seed in 1..20 {
            let m = msgs(150);
            let (got, t, _) = run(&m, seed, 0.2, 30.0, 40.0);
            assert_eq!(got, m, "seed {seed}");
            assert!(t < 60_000.0, "seed {seed} took {t} ms");
        }
    }

    #[test]
    fn big_messages_fragment_and_reassemble() {
        let big: Vec<u8> = (0..200_000).map(|i| (i * 7) as u8).collect();
        let m = vec![big.clone(), vec![], vec![1, 2, 3], big];
        let (got, _, _) = run(&m, 7, 0.1, 25.0, 10.0);
        assert_eq!(got, m);
    }

    #[test]
    fn acks_describe_holes() {
        let mut rx = Receiver::default();
        for seq in [0, 1, 3, 5] {
            rx.on_data(Fragment {
                seq,
                flags: START | END,
                payload: vec![],
            });
        }
        assert_eq!(
            rx.ack(),
            Ack {
                next: 2,
                bits: 0b101
            }
        );
    }

    #[test]
    fn sequence_numbers_wrap() {
        assert!(seq_lt(u32::MAX, 0));
        assert!(!seq_lt(0, u32::MAX));
        let mut tx = Sender {
            next_seq: u32::MAX - 3,
            ..Default::default()
        };
        let mut rx = Receiver {
            next: u32::MAX - 3,
            ..Default::default()
        };
        for i in 0..10u8 {
            tx.send(&[i]);
        }
        let mut got = vec![];
        for f in tx.poll(0.0) {
            got.extend(rx.on_data(f));
        }
        tx.on_ack(rx.ack(), 10.0);
        assert_eq!(got, (0..10u8).map(|i| vec![i]).collect::<Vec<_>>());
        assert_eq!(tx.in_flight(), 0);
    }

    #[test]
    fn rush_resends_immediately() {
        let mut tx = Sender::default();
        tx.send(b"x");
        assert_eq!(tx.poll(0.0).len(), 1);
        assert!(tx.poll(10.0).is_empty());
        tx.rush();
        assert_eq!(tx.poll(11.0).len(), 1);
    }
}
