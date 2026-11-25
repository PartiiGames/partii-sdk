//! net-probe: measure partii-net's paths between two real machines.
//!
//! ```text
//! net-probe host   [--secs 90] [--code-file /tmp/probe-code]
//! net-probe member --code ABCDE [--path auto|ws|turn|direct] [--secs 45]
//!                  [--hz 30] [--block-udp]
//!                  [--blackhole webrtc|turn|direct|current --blackhole-at 20 --restore-at 40
//!                   --blackhole-mode app|iptables]
//!                  [--jitter 450 --jitter-from 10 --jitter-loss 0.02]
//! ```
//!
//! `--jitter MS` makes the WebRTC paths alive but jittery (iptables, needs
//! root): from `--jitter-from` s on, all UDP stalls for MS every 3-5 s and
//! `--jitter-loss` of incoming UDP is dropped. The ws relay is untouched.
//!
//! Env: `PARTII_KEY` (a publishable key; never printed), `PARTII_EDGE`
//! (default: staging). The host creates a star room and echoes everything;
//! a member sends sequenced packets at `--hz` (every third one also
//! reliable) and prints ONE JSON object on stdout: per path whether it
//! opened, when, RTT p50/p95/p99, loss, the ICE pair; every path switch; the
//! reliable stream's order; and the failover timings.

use partii_net::{Config, Engine, Event, PathId, Topology};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::process::Command;
use std::time::{Duration, Instant};

const STAGING: &str = "https://partii-edge-staging.partii-games.workers.dev";
const UNREL: u8 = 1;
const REL: u8 = 2;

struct Args {
    role: String,
    code: Option<String>,
    path: String,
    secs: f64,
    hz: f64,
    code_file: String,
    block_udp: bool,
    blackhole: Option<String>,
    blackhole_at: f64,
    restore_at: f64,
    blackhole_mode: String,
    jitter: Option<f64>,
    jitter_from: f64,
    jitter_loss: f64,
    label: String,
}

fn args() -> Args {
    let mut a = Args {
        role: String::new(),
        code: None,
        path: "auto".into(),
        secs: 45.0,
        hz: 30.0,
        code_file: "/tmp/probe-code".into(),
        block_udp: false,
        blackhole: None,
        blackhole_at: 20.0,
        restore_at: 40.0,
        blackhole_mode: "app".into(),
        jitter: None,
        jitter_from: 10.0,
        jitter_loss: 0.02,
        label: String::new(),
    };
    let mut it = std::env::args().skip(1);
    a.role = it.next().unwrap_or_default();
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_default();
        match k.as_str() {
            "--code" => a.code = Some(v()),
            "--path" => a.path = v(),
            "--secs" => a.secs = v().parse().unwrap_or(a.secs),
            "--hz" => a.hz = v().parse().unwrap_or(a.hz),
            "--code-file" => a.code_file = v(),
            "--block-udp" => a.block_udp = true,
            "--blackhole" => a.blackhole = Some(v()),
            "--blackhole-at" => a.blackhole_at = v().parse().unwrap_or(a.blackhole_at),
            "--restore-at" => a.restore_at = v().parse().unwrap_or(a.restore_at),
            "--blackhole-mode" => a.blackhole_mode = v(),
            "--jitter" => a.jitter = v().parse().ok(),
            "--jitter-from" => a.jitter_from = v().parse().unwrap_or(a.jitter_from),
            "--jitter-loss" => a.jitter_loss = v().parse().unwrap_or(a.jitter_loss),
            "--label" => a.label = v(),
            _ => {
                eprintln!("net-probe: unknown argument {k}");
                std::process::exit(2);
            }
        }
    }
    if a.role != "host" && a.role != "member" {
        eprintln!(
            "usage: net-probe host|member [--code C] [--path auto|ws|turn|direct] [--secs S]"
        );
        std::process::exit(2);
    }
    a
}

fn ms(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

fn pct(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    Some((sorted[i] * 10.0).round() / 10.0)
}

/// Cloudflare's trace for a host: which colo answered (best effort, curl).
fn colo(url_host: &str) -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-s",
            "-m",
            "5",
            &format!("https://{url_host}/cdn-cgi/trace"),
        ])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("colo=").map(str::to_string))
}

/// UDP dropped both ways, v4 and v6, as a UDP-blocking firewall does:
/// loopback and DNS to this machine's resolvers stay open (DNS to anything
/// else is dropped too, so TURN over UDP 53 cannot sneak through). Needs root.
fn iptables_udp(on: bool) -> Result<(), String> {
    let op = if on { "-I" } else { "-D" };
    let resolvers: Vec<String> = std::fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("nameserver").map(|a| a.trim().to_string()))
        .collect();
    for (bin, v6) in [("iptables", false), ("ip6tables", true)] {
        let mut rules: Vec<Vec<String>> = vec![
            vec![
                "OUTPUT".into(),
                "-p".into(),
                "udp".into(),
                "-j".into(),
                "DROP".into(),
            ],
            vec![
                "INPUT".into(),
                "-p".into(),
                "udp".into(),
                "-j".into(),
                "DROP".into(),
            ],
        ];
        for r in resolvers.iter().filter(|r| r.contains(':') == v6) {
            rules.push(vec![
                "OUTPUT".into(),
                "-p".into(),
                "udp".into(),
                "-d".into(),
                r.clone(),
                "--dport".into(),
                "53".into(),
                "-j".into(),
                "ACCEPT".into(),
            ]);
            rules.push(vec![
                "INPUT".into(),
                "-p".into(),
                "udp".into(),
                "-s".into(),
                r.clone(),
                "--sport".into(),
                "53".into(),
                "-j".into(),
                "ACCEPT".into(),
            ]);
        }
        rules.push(vec![
            "OUTPUT".into(),
            "-o".into(),
            "lo".into(),
            "-j".into(),
            "ACCEPT".into(),
        ]);
        rules.push(vec![
            "INPUT".into(),
            "-i".into(),
            "lo".into(),
            "-j".into(),
            "ACCEPT".into(),
        ]);
        // -I prepends, so the accepts (inserted last) sit above the drops.
        for r in rules {
            match Command::new(bin).arg(op).args(&r).status() {
                Ok(s) if s.success() || !on => {}
                Ok(s) => return Err(format!("{bin} {op} {}: {s}", r.join(" "))),
                Err(e) if on => return Err(format!("{bin}: {e}")),
                Err(_) => {}
            }
        }
    }
    Ok(())
}

/// Random loss on incoming UDP (`p` of 1), v4 and v6. Needs root.
fn iptables_udp_loss(on: bool, p: f64) -> Result<(), String> {
    let op = if on { "-I" } else { "-D" };
    let prob = format!("{p}");
    for bin in ["iptables", "ip6tables"] {
        let r = [
            op,
            "INPUT",
            "-p",
            "udp",
            "-m",
            "statistic",
            "--mode",
            "random",
            "--probability",
            &prob,
            "-j",
            "DROP",
        ];
        match Command::new(bin).args(r).status() {
            Ok(s) if s.success() || !on => {}
            Ok(s) => return Err(format!("{bin} {}: {s}", r.join(" "))),
            Err(e) if on => return Err(format!("{bin}: {e}")),
            Err(_) => {}
        }
    }
    Ok(())
}

/// The jitter script on its own thread (iptables calls take a few ms each
/// and must not hold up the send loop). Returns the stalls it made, as
/// (start, end) ms since `t0`.
fn jitter_thread(
    t0: Instant,
    stall: f64,
    from_s: f64,
    loss: f64,
    end_ms: f64,
) -> std::thread::JoinHandle<(Vec<(f64, f64)>, Vec<String>)> {
    std::thread::spawn(move || {
        let mut stalls = Vec::new();
        let mut errors = Vec::new();
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15 ^ (std::process::id() as u64);
        let mut rand = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64
        };
        let wait_until = |t: f64| {
            let now = ms(t0);
            if t > now {
                std::thread::sleep(Duration::from_secs_f64((t - now) / 1000.0));
            }
        };
        wait_until(from_s * 1000.0);
        if loss > 0.0 {
            if let Err(m) = iptables_udp_loss(true, loss) {
                errors.push(format!("jitter loss: {m}"));
            }
        }
        let mut t = ms(t0) + 1000.0;
        while t + stall < end_ms - 2000.0 {
            wait_until(t);
            let a = ms(t0);
            if let Err(m) = iptables_udp(true) {
                errors.push(format!("jitter stall: {m}"));
                break;
            }
            wait_until(a + stall);
            let _ = iptables_udp(false);
            stalls.push((a, ms(t0)));
            t = a + 3000.0 + 2000.0 * rand();
        }
        if loss > 0.0 {
            let _ = iptables_udp_loss(false, loss);
        }
        (stalls, errors)
    })
}

fn engine(path: &str) -> Engine {
    let key = std::env::var("PARTII_KEY").unwrap_or_default();
    if key.is_empty() {
        println!("{}", json!({"error": "PARTII_KEY is not set"}));
        std::process::exit(1);
    }
    let edge = std::env::var("PARTII_EDGE").unwrap_or_else(|_| STAGING.into());
    let paths = match path {
        "ws" => vec![PathId::Ws],
        "turn" => vec![PathId::Turn],
        "direct" => vec![PathId::Direct],
        _ => vec![],
    };
    Engine::start(Config {
        edge,
        key,
        conn: Engine::new_conn_id(),
        client: "net-probe/0.1".into(),
        no_webrtc: path == "ws",
        paths,
        ..Default::default()
    })
}

fn wait_online(e: &Engine, secs: u64) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        match e.next_event(Duration::from_millis(200)) {
            Some(Event::Online { .. }) => return Ok(()),
            Some(Event::Error { message }) => return Err(message),
            _ => {}
        }
    }
    Err(format!("not online within {secs} s"))
}

fn edge_host() -> String {
    std::env::var("PARTII_EDGE")
        .unwrap_or_else(|_| STAGING.into())
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

fn main() {
    let a = args();
    if a.role == "host" {
        host(&a)
    } else {
        member(&a)
    }
}

fn host(a: &Args) {
    let t0 = Instant::now();
    let e = engine("auto");
    if let Err(m) = wait_online(&e, 20) {
        println!("{}", json!({"role": "host", "error": m}));
        std::process::exit(1);
    }
    let region = std::env::var("FLY_REGION").unwrap_or_default();
    let mut settings = Topology::Star.settings();
    settings["max_members"] = json!(32);
    let req = e.create_with(
        "relay-test",
        "game",
        &format!("probe-host-{region}"),
        json!({"region": region}),
        settings,
    );
    let deadline = t0 + Duration::from_secs_f64(a.secs);
    let mut code = None;
    let mut switches: Vec<Value> = Vec::new();
    let mut echoed = 0u64;
    while Instant::now() < deadline {
        let Some(ev) = e.next_event(Duration::from_millis(50)) else {
            continue;
        };
        match ev {
            Event::Created { req: r, room } if r == req => {
                let _ = std::fs::write(&a.code_file, &room.code);
                eprintln!(
                    "{}",
                    json!({"ev": "created", "code": room.code, "ms": ms(t0)})
                );
                code = Some(room.code);
            }
            Event::Failed {
                req: r,
                code: c,
                message,
            } if r == req => {
                println!(
                    "{}",
                    json!({"role": "host", "error": format!("create: {c}: {message}")})
                );
                std::process::exit(1);
            }
            Event::Message {
                from,
                reliable,
                bytes,
            } => {
                echoed += 1;
                e.send(from, reliable, bytes);
            }
            Event::PathChanged { mid, from, to, rtt } => switches
                .push(json!({"t_ms": ms(t0), "mid": mid, "from": from, "to": to, "rtt": rtt})),
            _ => {}
        }
    }
    let out = json!({
        "role": "host", "label": a.label, "region": region, "code": code,
        "edge_colo": colo(&edge_host()), "echoed": echoed, "switches": switches, "stats": e.stats(),
    });
    println!("{out}");
}

#[derive(Default)]
struct PathAcc {
    rtts: Vec<f64>,
    sent: u64,
    echoed: u64,
    open_ms: Option<f64>,
    first_rtt_ms: Option<f64>,
    ice: Value,
    link_rtt: Option<f64>,
}

fn member(a: &Args) {
    let mut errors: Vec<String> = Vec::new();
    let mut udp_blocked = false;
    if a.block_udp {
        match iptables_udp(true) {
            Ok(()) => udp_blocked = true,
            Err(m) => errors.push(format!("block udp: {m}")),
        }
    }
    let t_start = Instant::now();
    let e = engine(&a.path);
    let region = std::env::var("FLY_REGION").unwrap_or_default();
    if let Err(m) = wait_online(&e, 20) {
        finish_err(a, &region, &m, udp_blocked);
    }
    let online_ms = ms(t_start);
    let Some(code) = a.code.clone() else {
        finish_err(a, &region, "member needs --code", udp_blocked);
    };
    let t0 = Instant::now();
    let req = e.join_with(
        &code,
        "game",
        &format!("probe-{region}-{}", a.path),
        json!({"region": region, "path": a.path}),
        Topology::Star.settings(),
    );
    let mut host: Option<u16> = None;
    let mut join_ms = None;
    let mut cur: Option<PathId> = None;
    let mut switches: Vec<Value> = Vec::new();
    let mut acc: BTreeMap<PathId, PathAcc> = PathId::ALL
        .iter()
        .map(|p| (*p, PathAcc::default()))
        .collect();
    // seq -> (sent at ms, path at send)
    let mut sent_at: Vec<(f64, Option<PathId>)> = Vec::new();
    let mut got: HashSet<u32> = HashSet::new();
    let mut first_echo_ms: Option<f64> = None;
    let mut echo_times: Vec<f64> = Vec::new();
    let mut rel_sent = 0u32;
    let mut rel_next = 0u32;
    let mut rel_out_of_order = 0u32;
    let mut rel_max_delay = 0f64;
    let mut rel_delays: Vec<f64> = Vec::new();
    let period = 1000.0 / a.hz;
    let mut next_send = 0f64;
    let mut next_stats = 0f64;
    let end_ms = a.secs * 1000.0;
    let mut blackholed: Vec<PathId> = Vec::new();
    let mut hole_at: Option<f64> = None;
    let mut restore_at_ms: Option<f64> = None;
    let mut hole_path_before: Option<PathId> = None;
    let mut seq: u32 = 0;
    let fail_join_ms = 20_000.0;
    let jitter = a
        .jitter
        .map(|stall| jitter_thread(t0, stall, a.jitter_from, a.jitter_loss, end_ms));
    loop {
        let now = ms(t0);
        if now >= end_ms {
            break;
        }
        if host.is_none() && now > fail_join_ms {
            errors.push("not in the room within 20 s".into());
            break;
        }
        // Failover script.
        if let Some(which) = &a.blackhole {
            if hole_at.is_none() && now >= a.blackhole_at * 1000.0 {
                hole_at = Some(now);
                hole_path_before = cur;
                let targets: Vec<PathId> = match which.as_str() {
                    "webrtc" => vec![PathId::Turn, PathId::Direct],
                    "turn" => vec![PathId::Turn],
                    "direct" => vec![PathId::Direct],
                    "ws" => vec![PathId::Ws],
                    _ => cur.into_iter().collect(),
                };
                if a.blackhole_mode == "iptables" {
                    if let Err(m) = iptables_udp(true) {
                        errors.push(format!("blackhole: {m}"));
                    }
                } else {
                    for p in &targets {
                        e.blackhole(*p, true);
                    }
                }
                blackholed = targets;
            }
            if hole_at.is_some() && restore_at_ms.is_none() && now >= a.restore_at * 1000.0 {
                restore_at_ms = Some(now);
                if a.blackhole_mode == "iptables" {
                    let _ = iptables_udp(false);
                } else {
                    for p in &blackholed {
                        e.blackhole(*p, false);
                    }
                }
            }
        }
        if let Some(h) = host {
            if now >= next_send {
                next_send = if next_send == 0.0 {
                    now + period
                } else {
                    next_send + period
                };
                let mut b = vec![UNREL];
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&now.to_le_bytes());
                e.send(h, false, b);
                sent_at.push((now, cur));
                if seq % 3 == 0 {
                    let mut r = vec![REL];
                    r.extend_from_slice(&rel_sent.to_le_bytes());
                    r.extend_from_slice(&now.to_le_bytes());
                    e.send(h, true, r);
                    rel_sent += 1;
                }
                seq += 1;
            }
        }
        if now >= next_stats {
            next_stats = now + 100.0;
            let st = e.stats();
            if let Some(h) = host {
                let peer = &st["peers"][h.to_string()];
                for p in PathId::ALL {
                    let s = &peer["paths"][p.name()];
                    let a = acc.get_mut(&p).unwrap();
                    if s["open"] == true && a.open_ms.is_none() {
                        a.open_ms = Some(now);
                    }
                    if let Some(r) = s["rtt"].as_f64() {
                        if a.first_rtt_ms.is_none() {
                            a.first_rtt_ms = Some(now);
                        }
                        a.link_rtt = Some((r * 10.0).round() / 10.0);
                    }
                    if !s["ice"].is_null() {
                        a.ice = s["ice"].clone();
                    }
                }
            }
        }
        for ev in e.poll() {
            match ev {
                Event::Joined { req: r, room } if r == req => {
                    host = Some(room.host);
                    join_ms = Some(ms(t0));
                }
                Event::Failed {
                    req: r,
                    code: c,
                    message,
                } if r == req => {
                    errors.push(format!("join: {c}: {message}"));
                }
                Event::PeerConnected { path, .. } => {
                    cur = Some(path);
                    switches.push(json!({"t_ms": ms(t0), "to": path, "first": true}));
                }
                Event::PathChanged { from, to, rtt, .. } => {
                    cur = Some(to);
                    switches.push(json!({"t_ms": ms(t0), "from": from, "to": to, "rtt": rtt}));
                }
                Event::Message { bytes, .. } if bytes.len() == 13 => {
                    let t = ms(t0);
                    let s = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
                    let sent = f64::from_le_bytes(bytes[5..13].try_into().unwrap());
                    let rtt = t - sent;
                    if bytes[0] == UNREL {
                        if !got.insert(s) {
                            continue;
                        }
                        first_echo_ms.get_or_insert(t);
                        echo_times.push(t);
                        if let Some(p) = cur {
                            acc.get_mut(&p).unwrap().rtts.push(rtt);
                        }
                        if let Some((_, Some(p))) = sent_at.get(s as usize) {
                            acc.get_mut(p).unwrap().echoed += 1;
                        }
                    } else if bytes[0] == REL {
                        if s != rel_next {
                            rel_out_of_order += 1;
                        }
                        rel_next = rel_next.max(s + 1);
                        rel_max_delay = rel_max_delay.max(rtt);
                        rel_delays.push(rtt);
                    }
                }
                Event::RoomClosed { reason } => errors.push(format!("room closed: {reason}")),
                Event::Offline => errors.push(format!("offline at {} ms", ms(t0))),
                Event::Error { message } => errors.push(message),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let stats = e.stats();
    let jitter = jitter.map(|h| {
        let (stalls, errs) = h.join().unwrap_or_default();
        errors.extend(errs);
        let from = stalls.first().map(|s| s.0).unwrap_or(end_ms);
        let mins = ((end_ms - from) / 60_000.0).max(1e-9);
        let n_sw = switches
            .iter()
            .filter(|s| s["t_ms"].as_f64().is_some_and(|t| t >= from) && s["first"].is_null())
            .count();
        let held: Vec<f64> = stalls.iter().map(|(a, b)| (b - a).round()).collect();
        // Longest echo gap inside each stall window (+1 s after it).
        let mut gaps: Vec<f64> = stalls
            .iter()
            .map(|(a0, b0)| {
                let mut prev = echo_times
                    .iter()
                    .copied()
                    .filter(|t| *t <= *a0)
                    .last()
                    .unwrap_or(*a0);
                let mut g = 0f64;
                for t in echo_times
                    .iter()
                    .copied()
                    .filter(|t| *t > *a0 && *t <= b0 + 1000.0)
                {
                    g = g.max(t - prev);
                    prev = t;
                }
                g.round()
            })
            .collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let peer = stats["peers"]
            .as_object()
            .and_then(|m| m.values().next())
            .cloned()
            .unwrap_or(Value::Null);
        json!({
            "stall_ms": a.jitter, "loss": a.jitter_loss, "stalls": stalls.len(), "held_ms": held,
            "minutes": (mins * 100.0).round() / 100.0,
            "switches": n_sw, "switches_per_min": ((n_sw as f64 / mins) * 100.0).round() / 100.0,
            "detours": peer["detours"], "detour_ms": peer["detour_ms"],
            "suspect_after_ms": peer["suspect_after_ms"],
            "echo_gap_in_stall_p50": pct(&gaps, 0.5), "echo_gap_in_stall_max": gaps.last(),
        })
    });
    // Let the last reliable echoes land before judging the stream.
    let tail_deadline = Instant::now() + Duration::from_millis(3000);
    while rel_next < rel_sent && Instant::now() < tail_deadline {
        for ev in e.poll() {
            if let Event::Message { bytes, .. } = ev {
                if bytes.len() == 13 && bytes[0] == REL {
                    let s = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
                    if s != rel_next {
                        rel_out_of_order += 1;
                    }
                    rel_next = rel_next.max(s + 1);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if udp_blocked {
        let _ = iptables_udp(false);
    }
    if a.blackhole_mode == "iptables" && hole_at.is_some() && restore_at_ms.is_none() {
        let _ = iptables_udp(false);
    }

    // Loss over the steady window: sent after the first echo, more than
    // 1.5 s before the end.
    let first = first_echo_ms.unwrap_or(f64::MAX);
    let cutoff = end_ms - 1500.0;
    let mut steady_sent = 0u64;
    let mut steady_got = 0u64;
    for (i, (t, p)) in sent_at.iter().enumerate() {
        if *t < first || *t > cutoff {
            continue;
        }
        steady_sent += 1;
        let ok = got.contains(&(i as u32));
        if ok {
            steady_got += 1;
        }
        if let Some(p) = p {
            acc.get_mut(p).unwrap().sent += 1;
        }
    }
    // Per path "echoed" above counted all; recount over the steady window.
    for a in acc.values_mut() {
        a.echoed = 0;
    }
    for (i, (t, p)) in sent_at.iter().enumerate() {
        if *t < first || *t > cutoff {
            continue;
        }
        if let (Some(p), true) = (p, got.contains(&(i as u32))) {
            acc.get_mut(p).unwrap().echoed += 1;
        }
    }
    let paths: serde_json::Map<String, Value> = acc
        .iter_mut()
        .map(|(p, x)| {
            x.rtts.sort_by(|a, b| a.total_cmp(b));
            let loss = (x.sent > 0).then(|| 1.0 - x.echoed as f64 / x.sent as f64);
            (
                p.name().to_string(),
                json!({
                    "opened": x.open_ms.is_some(), "open_ms": x.open_ms, "measured_ms": x.first_rtt_ms,
                    "samples": x.rtts.len(), "p50": pct(&x.rtts, 0.5), "p95": pct(&x.rtts, 0.95), "p99": pct(&x.rtts, 0.99),
                    "link_rtt": x.link_rtt, "sent": x.sent, "loss": loss.map(|l| (l * 10000.0).round() / 10000.0), "ice": x.ice,
                }),
            )
        })
        .collect();
    let mut all: Vec<f64> = acc.values().flat_map(|x| x.rtts.iter().copied()).collect();
    all.sort_by(|a, b| a.total_cmp(b));
    rel_delays.sort_by(|a, b| a.total_cmp(b));

    // Failover: when play left the dead path, the longest echo gap around
    // it, and when a WebRTC path took over again after the restore.
    let failover = hole_at.map(|h| {
        let switched = switches.iter().find_map(|s| {
            let t = s["t_ms"].as_f64()?;
            (t >= h && s["first"].is_null() && Some(s["from"].clone()) != Some(s["to"].clone()))
                .then(|| (t, s["to"].clone()))
        });
        let window_end = restore_at_ms.unwrap_or(end_ms);
        let mut gap = 0f64;
        let mut prev = echo_times
            .iter()
            .copied()
            .filter(|t| *t <= h)
            .last()
            .unwrap_or(h);
        for t in echo_times
            .iter()
            .copied()
            .filter(|t| *t > h && *t <= window_end)
        {
            gap = gap.max(t - prev);
            prev = t;
        }
        let back = restore_at_ms.and_then(|r| {
            switches.iter().find_map(|s| {
                let t = s["t_ms"].as_f64()?;
                (t >= r && s["to"] != json!("ws")).then(|| ((t - r).round(), s["to"].clone()))
            })
        });
        json!({
            "what": a.blackhole, "mode": a.blackhole_mode, "path_before": hole_path_before,
            "at_ms": h, "switch_ms": switched.as_ref().map(|(t, _)| (t - h).round()),
            "switched_to": switched.map(|(_, p)| p), "play_gap_ms": gap.round(),
            "restore_at_ms": restore_at_ms, "promote_back_ms": back.as_ref().map(|(t, _)| *t),
            "promoted_to": back.map(|(_, p)| p),
        })
    });
    let out = json!({
        "role": "member", "label": a.label, "region": region, "code": code, "path_mode": a.path,
        "udp_blocked": udp_blocked, "edge_colo": colo(&edge_host()),
        "online_ms": online_ms, "join_ms": join_ms, "first_echo_ms": first_echo_ms,
        "final_path": cur,
        "overall": {"sent": steady_sent, "echoed": steady_got,
            "loss": (steady_sent > 0).then(|| ((1.0 - steady_got as f64 / steady_sent as f64) * 10000.0).round() / 10000.0),
            "p50": pct(&all, 0.5), "p95": pct(&all, 0.95), "p99": pct(&all, 0.99)},
        "paths": paths,
        "reliable": {"sent": rel_sent, "received": rel_next, "missing": rel_sent.saturating_sub(rel_next),
            "out_of_order": rel_out_of_order, "max_rtt_ms": (rel_max_delay * 10.0).round() / 10.0,
            "p50": pct(&rel_delays, 0.5), "p99": pct(&rel_delays, 0.99)},
        "switches": switches, "failover": failover, "jitter": jitter, "errors": errors, "stats": stats,
    });
    println!("{out}");
}

fn finish_err(a: &Args, region: &str, m: &str, udp_blocked: bool) -> ! {
    if udp_blocked {
        let _ = iptables_udp(false);
    }
    println!(
        "{}",
        json!({"role": "member", "label": a.label, "region": region, "path_mode": a.path, "udp_blocked": udp_blocked, "errors": [m]})
    );
    std::process::exit(1);
}
