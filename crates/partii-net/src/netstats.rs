//! Room network stats (PROTOCOL.md "Network stats"): what each link's paths
//! did, sent to the edge over the session socket about every 30 s and once on
//! leave, so partii can see which path players' links use and how fast each
//! is. One compact `net_stats` frame per report:
//!
//! ```json
//! {"t":"net_stats","v":1,"final":false,"links":[{"mid":1,"age_ms":30010,"new":true,
//!   "path":"direct","switches":1,"on_ms":{"ws":740,"direct":29270},
//!   "paths":{"ws":{"open":true,"rtt":[41,63],"opened_ms":0},
//!            "turn":{"open":false,"fails":1},
//!            "direct":{"open":true,"rtt":[12,19],"opened_ms":740,"cand":["srflx","host"]}}}]}
//! ```
//!
//! - Only members' ids in the room (`mid`), never addresses or player ids.
//! - Sent only when the edge's `welcome` asked for it (`net_stats_ms`, in
//!   answer to the hello's `caps: ["net_stats"]`): an older edge never sees one.
//! - Each link is reported by one end: a non-host reports its link to the
//!   host; in a mesh, a link between two non-hosts is reported by the lower mid.
//! - Counters are for the interval since the last report: `on_ms` (time on
//!   each path), `switches`, `fails` (WebRTC opens that failed or timed out),
//!   `rtt` (`[p50, p95]` of the raw round trips, ms). Once per link:
//!   `new` (its first report), `opened_ms` (time from the link's start to the
//!   path's first open), `cand` (the selected candidate pair's local and
//!   remote types), `stuck_ws` (still on the ws relay after `STUCK_WS_MS` with
//!   no WebRTC path ever open). `ended: true`: the link is gone (the member
//!   left); its last numbers.

use crate::link::{Link, PathId};
use serde_json::{json, Map, Value};

/// The hello cap that asks the edge for `net_stats_ms` in its welcome.
pub const NET_STATS_CAP: &str = "net_stats";
/// The report's version (`v`).
pub const NET_STATS_VERSION: u32 = 1;
/// The shortest and longest report interval the engine accepts from a welcome.
pub const NET_STATS_MIN_MS: f64 = 5_000.0;
pub const NET_STATS_MAX_MS: f64 = 600_000.0;
/// A link still on the ws relay this long after it started, with no WebRTC
/// path ever open, is reported `stuck_ws` (once).
pub const STUCK_WS_MS: f64 = 30_000.0;
/// At most this many links in one report (ended ones included).
pub const MAX_LINKS: usize = 32;
/// Ended links kept for the next report, at most.
pub const MAX_ENDED: usize = 32;

/// An ICE candidate's type (RTCIceCandidateType).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandType {
    Host,
    Srflx,
    Prflx,
    Relay,
}

impl CandType {
    pub fn parse(s: &str) -> Option<CandType> {
        match s {
            "host" => Some(CandType::Host),
            "srflx" => Some(CandType::Srflx),
            "prflx" => Some(CandType::Prflx),
            "relay" => Some(CandType::Relay),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            CandType::Host => "host",
            CandType::Srflx => "srflx",
            CandType::Prflx => "prflx",
            CandType::Relay => "relay",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PathTrack {
    /// Ms from the link's start to this path's first open.
    opened: Option<f64>,
    opened_said: bool,
    fails: u32,
    cand: Option<(CandType, CandType)>,
    cand_said: bool,
}

/// One link's numbers for net-stats reports.
#[derive(Debug, Clone)]
pub struct LinkStats {
    created: f64,
    reported: bool,
    cur: PathId,
    since: f64,
    on_ms: [f64; 3],
    switches: u32,
    paths: [PathTrack; 3],
    webrtc_ever: bool,
    stuck_said: bool,
}

/// `q` (0..1) of sorted samples, nearest rank.
pub fn percentile(sorted: &[f32], q: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

impl LinkStats {
    pub fn new(now: f64) -> Self {
        Self {
            created: now,
            reported: false,
            cur: PathId::Ws,
            since: now,
            on_ms: [0.0; 3],
            switches: 0,
            paths: Default::default(),
            webrtc_ever: false,
            stuck_said: false,
        }
    }

    /// Note paths open for the first time (called whenever the link is flushed).
    pub fn observe(&mut self, link: &Link, now: f64) {
        for p in PathId::ALL {
            let t = &mut self.paths[p as usize];
            if t.opened.is_none() && link.path(p).open {
                t.opened = Some((now - self.created).max(0.0));
                if p != PathId::Ws {
                    self.webrtc_ever = true;
                }
            }
        }
    }

    pub fn switched(&mut self, from: PathId, to: PathId, now: f64) {
        if from == to {
            return;
        }
        self.on_ms[self.cur as usize] += (now - self.since).max(0.0);
        self.cur = to;
        self.since = now;
        self.switches += 1;
    }

    /// A WebRTC path failed to open (closed or timed out while connecting).
    pub fn failed(&mut self, p: PathId) {
        let t = &mut self.paths[p as usize];
        t.fails = t.fails.saturating_add(1);
    }

    /// The selected candidate pair's types for path `p` (the driver read them
    /// from the peer connection's stats once it opened).
    pub fn candidates(&mut self, p: PathId, local: CandType, remote: CandType) {
        let t = &mut self.paths[p as usize];
        if t.cand.is_none() {
            t.cand = Some((local, remote));
        }
    }

    /// This link's report entry; the interval's counters start over.
    /// `webrtc`: the engine tries WebRTC on this link (else it can't be stuck on ws).
    pub fn report(
        &mut self,
        mid: u16,
        link: &mut Link,
        now: f64,
        ended: bool,
        webrtc: bool,
    ) -> Value {
        self.observe(link, now);
        self.on_ms[self.cur as usize] += (now - self.since).max(0.0);
        self.since = now;
        let age = (now - self.created).max(0.0);
        let mut e = Map::new();
        e.insert("mid".into(), json!(mid));
        e.insert("age_ms".into(), json!(age.round() as u64));
        if !self.reported {
            self.reported = true;
            e.insert("new".into(), json!(true));
        }
        e.insert("path".into(), json!(link.current().name()));
        if self.switches > 0 {
            e.insert("switches".into(), json!(self.switches));
        }
        let mut on = Map::new();
        for p in PathId::ALL {
            let ms = self.on_ms[p as usize].round() as u64;
            if ms > 0 {
                on.insert(p.name().into(), json!(ms));
            }
        }
        if !on.is_empty() {
            e.insert("on_ms".into(), Value::Object(on));
        }
        let mut paths = Map::new();
        for p in PathId::ALL {
            let mut samples = link.take_rtt_samples(p);
            let t = &mut self.paths[p as usize];
            let open = link.path(p).open;
            let mut o = Map::new();
            if !samples.is_empty() {
                samples.sort_by(|a, b| a.total_cmp(b));
                let ms = |q| (percentile(&samples, q) as f64).round() as u64;
                o.insert("rtt".into(), json!([ms(0.5), ms(0.95)]));
            }
            if let (Some(ms), false) = (t.opened, t.opened_said) {
                t.opened_said = true;
                o.insert("opened_ms".into(), json!(ms.round() as u64));
            }
            if t.fails > 0 {
                o.insert("fails".into(), json!(t.fails));
                t.fails = 0;
            }
            if let (Some((l, r)), false) = (t.cand, t.cand_said) {
                t.cand_said = true;
                o.insert("cand".into(), json!([l.name(), r.name()]));
            }
            if open || !o.is_empty() {
                o.insert("open".into(), json!(open));
                paths.insert(p.name().into(), Value::Object(o));
            }
        }
        if !paths.is_empty() {
            e.insert("paths".into(), Value::Object(paths));
        }
        self.switches = 0;
        self.on_ms = [0.0; 3];
        if webrtc
            && !self.stuck_said
            && !self.webrtc_ever
            && age >= STUCK_WS_MS
            && link.current() == PathId::Ws
        {
            self.stuck_said = true;
            e.insert("stuck_ws".into(), json!(true));
        }
        if ended {
            e.insert("ended".into(), json!(true));
        }
        Value::Object(e)
    }
}

/// The `net_stats` frame for some link entries.
pub fn frame(links: Vec<Value>, final_: bool) -> Value {
    json!({"t": "net_stats", "v": NET_STATS_VERSION, "final": final_, "links": links})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let s: Vec<f32> = (1..=100).map(|x| x as f32).collect();
        assert_eq!(percentile(&s, 0.5), 50.0);
        assert_eq!(percentile(&s, 0.95), 95.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn a_link_reports_time_on_paths_and_its_one_time_facts_once() {
        let mut link = Link::default();
        let mut st = LinkStats::new(1000.0);
        link.set_open(PathId::Ws, true, 1000.0);
        st.observe(&link, 1000.0);
        link.set_open(PathId::Direct, true, 1800.0);
        st.observe(&link, 1800.0);
        st.switched(PathId::Ws, PathId::Direct, 2000.0);
        st.failed(PathId::Turn);
        st.candidates(PathId::Direct, CandType::Srflx, CandType::Host);
        let r = st.report(3, &mut link, 31_000.0, false, true);
        assert_eq!(r["mid"], 3);
        assert_eq!(r["new"], true);
        assert_eq!(r["age_ms"], 30_000);
        assert_eq!(r["switches"], 1);
        assert_eq!(r["on_ms"], json!({"ws": 1000, "direct": 29_000}));
        assert_eq!(r["paths"]["direct"]["opened_ms"], 800);
        assert_eq!(r["paths"]["direct"]["cand"], json!(["srflx", "host"]));
        assert_eq!(r["paths"]["turn"], json!({"fails": 1, "open": false}));
        assert_eq!(r["paths"]["ws"]["opened_ms"], 0);
        assert!(r.get("stuck_ws").is_none(), "a WebRTC path opened");
        let r2 = st.report(3, &mut link, 61_000.0, true, true);
        assert!(r2.get("new").is_none());
        assert!(r2.get("switches").is_none());
        assert_eq!(r2["on_ms"], json!({"direct": 30_000}));
        assert!(r2["paths"]["direct"].get("opened_ms").is_none());
        assert!(r2["paths"]["direct"].get("cand").is_none());
        assert!(r2["paths"].get("turn").is_none(), "nothing new on turn");
        assert_eq!(r2["ended"], true);
    }

    #[test]
    fn stuck_on_ws_is_said_once_after_30_s_without_webrtc() {
        let mut link = Link::default();
        let mut st = LinkStats::new(0.0);
        link.set_open(PathId::Ws, true, 0.0);
        assert!(st
            .report(2, &mut link, 20_000.0, false, true)
            .get("stuck_ws")
            .is_none());
        assert_eq!(
            st.report(2, &mut link, 40_000.0, false, true)["stuck_ws"],
            true
        );
        assert!(st
            .report(2, &mut link, 70_000.0, false, true)
            .get("stuck_ws")
            .is_none());
        // An engine that never tries WebRTC isn't stuck.
        let mut st2 = LinkStats::new(0.0);
        assert!(st2
            .report(2, &mut link, 40_000.0, false, false)
            .get("stuck_ws")
            .is_none());
    }
}
