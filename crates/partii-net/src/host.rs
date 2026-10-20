//! A seat in a server-authoritative match on partii-host, over plain UDP.
//!
//! [`join`] sends the seat's JOIN (repeating it until the host answers), reads
//! on its own thread, answers the host's path probes, and hands you the
//! host's frames ([`SeatEvent::Frame`]: a `STATE` or `PONG` frame, type byte
//! first). Send your inputs with [`Seat::input`]: in a match everything is an
//! unreliable datagram, so put redundancy in them (see [`crate::netcode`]).
//!
//! With a session key in the [`Endpoint`] (what partii hands out with a
//! ticket), every packet carries a counter and a tag
//! ([`partii_protocol::host::seal`]). With no key, packets go untagged: that's
//! the local-development form a host without ticket keys accepts, where the
//! ticket is `"<match>\n<player>[\n<info JSON>]"`.

use base64::Engine;
use partii_protocol::host::{self as wire, Endpoint};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// No packet from the host for this long: the seat is down.
pub const SILENT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq)]
pub enum SeatEvent {
    /// A frame from the host (`STATE` or `PONG`), type byte first.
    Frame(Vec<u8>),
    /// The seat went down (refused, or the host went quiet). Sent once.
    Closed(String),
}

#[derive(Debug, Clone, Default)]
pub struct SeatOptions {
    /// Only connect to these hosts (an app that only ever talks to partii's
    /// game hosts can pin them here). Empty: any address.
    pub allowed_hosts: Vec<IpAddr>,
}

struct Shared {
    sock: UdpSocket,
    key: Option<Vec<u8>>,
    counter: AtomicU32,
    stop: AtomicBool,
}

impl Shared {
    fn send(&self, frame: &[u8]) {
        let packet = match &self.key {
            Some(k) => wire::seal(
                k,
                wire::TO_HOST,
                self.counter.fetch_add(1, Ordering::Relaxed) + 1,
                frame,
            ),
            None => frame.to_vec(),
        };
        let _ = self.sock.send(&packet);
    }
}

/// One joined seat. Dropping it closes the seat.
pub struct Seat {
    shared: Arc<Shared>,
}

impl Seat {
    /// Send an input datagram (your bytes; at most `MAX_FRAME - 1`).
    pub fn input(&self, bytes: &[u8]) -> Result<(), String> {
        self.frame(wire::INPUT, bytes)
    }

    /// Send a ping; the host answers with a `PONG` carrying the same bytes, straight from its network task.
    pub fn ping(&self, bytes: &[u8]) -> Result<(), String> {
        self.frame(wire::PING, bytes)
    }

    fn frame(&self, kind: u8, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() + 1 > wire::MAX_FRAME {
            return Err(format!(
                "{} bytes is over the {}-byte datagram limit",
                bytes.len(),
                wire::MAX_FRAME - 1
            ));
        }
        let mut f = Vec::with_capacity(bytes.len() + 1);
        f.push(kind);
        f.extend_from_slice(bytes);
        self.shared.send(&f);
        Ok(())
    }

    /// Stop reading; no more events.
    pub fn close(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.close();
    }
}

fn resolve(addr: &str, opts: &SeatOptions) -> Result<SocketAddr, String> {
    let a = addr
        .to_socket_addrs()
        .map_err(|_| format!("{addr} isn't an address"))?
        .next()
        .ok_or("no address")?;
    if !opts.allowed_hosts.is_empty() && !opts.allowed_hosts.contains(&a.ip()) {
        return Err(format!("{} isn't one of the allowed game hosts", a.ip()));
    }
    Ok(a)
}

/// Join a seat. `on` is called from the seat's reader thread.
pub fn join(
    endpoint: &Endpoint,
    opts: &SeatOptions,
    on: impl Fn(SeatEvent) + Send + 'static,
) -> Result<Seat, String> {
    let to = resolve(&endpoint.udp, opts)?;
    let key = if endpoint.key.trim().is_empty() {
        None
    } else {
        let k = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(endpoint.key.trim())
            .map_err(|_| "bad session key")?;
        if k.len() != 32 {
            return Err("bad session key".into());
        }
        Some(k)
    };
    if endpoint.ticket.is_empty() || endpoint.ticket.len() > 2048 {
        return Err("bad ticket".into());
    }
    let sock = UdpSocket::bind(if to.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })
        .map_err(|e| e.to_string())?;
    sock.connect(to).map_err(|e| e.to_string())?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| e.to_string())?;
    let shared = Arc::new(Shared {
        sock: sock.try_clone().map_err(|e| e.to_string())?,
        key,
        counter: AtomicU32::new(0),
        stop: AtomicBool::new(false),
    });
    let mut join = vec![wire::JOIN];
    join.extend_from_slice(endpoint.ticket.as_bytes());
    shared.send(&join);
    let s = shared.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        let (mut last_in, mut heard, mut joined) = (0u32, Instant::now(), false);
        let reason = loop {
            if s.stop.load(Ordering::Relaxed) {
                break None;
            }
            match sock.recv(&mut buf) {
                Ok(n) if n > 0 && buf[0] == wire::DENIED => {
                    break Some(format!("refused: {}", String::from_utf8_lossy(&buf[1..n])))
                }
                Ok(n) => {
                    let frame = match &s.key {
                        Some(k) => match wire::open(k, wire::TO_CLIENT, &buf[..n]) {
                            // Counters only go up: older packets are replays or late.
                            Some((f, c)) if c > last_in => {
                                last_in = c;
                                f
                            }
                            _ => continue,
                        },
                        None => &buf[..n],
                    };
                    heard = Instant::now();
                    joined = true;
                    match frame.first() {
                        Some(&wire::STATE) | Some(&wire::PONG) => {
                            on(SeatEvent::Frame(frame.to_vec()))
                        }
                        Some(&wire::PROBE) if frame.len() >= 3 => {
                            s.send(&[wire::PROBE_ACK, frame[1], frame[2]])
                        }
                        _ => {}
                    }
                }
                Err(_) if heard.elapsed() > SILENT => {
                    break Some("no word from the game server".into())
                }
                // The JOIN is a datagram too: say it again until the host answers.
                Err(_) if !joined => s.send(&join),
                Err(_) => {}
            }
        };
        if let Some(why) = reason {
            on(SeatEvent::Closed(why));
        }
    });
    Ok(Seat { shared })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A tiny host on loopback: answers the JOIN with a tagged STATE and probes once.
    #[test]
    fn joins_receives_state_and_answers_probes() {
        let host = UdpSocket::bind("127.0.0.1:0").unwrap();
        let key = [9u8; 32];
        let ep = Endpoint {
            udp: host.local_addr().unwrap().to_string(),
            ticket: "t".into(),
            key: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key),
        };
        let (tx, rx) = mpsc::channel();
        let seat = join(&ep, &SeatOptions::default(), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        let mut buf = [0u8; 2048];
        let (n, from) = host.recv_from(&mut buf).unwrap();
        let (f, _) = wire::open(&key, wire::TO_HOST, &buf[..n]).unwrap();
        assert_eq!(f, b"\x00t");
        host.send_to(&wire::seal(&key, wire::TO_CLIENT, 1, b"\x03hello"), from)
            .unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            SeatEvent::Frame(b"\x03hello".to_vec())
        );
        host.send_to(
            &wire::seal(&key, wire::TO_CLIENT, 2, &[wire::PROBE, 0xE8, 0x03]),
            from,
        )
        .unwrap();
        let (n, _) = host.recv_from(&mut buf).unwrap();
        assert_eq!(
            wire::open(&key, wire::TO_HOST, &buf[..n]).unwrap().0,
            &[wire::PROBE_ACK, 0xE8, 0x03]
        );
        seat.input(b"in").unwrap();
        let (n, _) = host.recv_from(&mut buf).unwrap();
        assert_eq!(
            wire::open(&key, wire::TO_HOST, &buf[..n]).unwrap().0,
            b"\x02in"
        );
        // A replayed (old-counter) packet is ignored.
        host.send_to(&wire::seal(&key, wire::TO_CLIENT, 1, b"\x03old"), from)
            .unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn allow_list() {
        let opts = SeatOptions {
            allowed_hosts: vec!["203.0.113.7".parse().unwrap()],
        };
        assert!(resolve("203.0.113.7:4435", &opts).is_ok());
        assert!(resolve("1.1.1.1:53", &opts).is_err());
    }
}
