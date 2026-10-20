//! Analytics: events and error reports, sent in batches on their own thread.
//!
//! `track` queues an event; the queue goes out every 30 s, at once for an
//! error, and at `end_session`. While offline the queue waits (and is kept
//! on disk at `end_session`, up to `MAX_QUEUED` events), so nothing is lost
//! to a flight or a dead Wi-Fi. A session is one launch: `session_start` is
//! queued when the account starts, `session_end {secs}` by `end_session`.
//! `install_panic_hook` writes a panic to the data folder; the next flush
//! (this launch, if it survived, or the next) reports it.
//!
//! With analytics off (`AccountConfig::analytics`, `set_analytics`), nothing
//! is queued or sent, and what was queued is dropped.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use partii_client::Client;
use serde_json::{json, Value};

use crate::{lock, now_ms, State};

/// Events kept while they can't be sent; the oldest go first.
pub const MAX_QUEUED: usize = 5_000;
const MAX_ERRORS_QUEUED: usize = 50;
const BATCH: usize = 100;
const ERRORS_BATCH: usize = 20;
const EVERY: Duration = Duration::from_secs(30);
const PANIC_FILE: &str = "partii-panic.json";
const PANICS_KEPT: usize = 5;

pub(crate) struct Analytics {
    q: Arc<(Mutex<Queue>, Condvar)>,
    started: Instant,
    /// Where the queue waits between launches.
    file: PathBuf,
}

#[derive(Default)]
struct Queue {
    enabled: bool,
    events: Vec<Value>,
    errors: Vec<Value>,
    owned: Option<bool>,
    /// Sends asked for (errors, end_session) and sends done: the thread sends
    /// now while `flush_req > flush_done`; end_session waits for its own.
    flush_req: u64,
    flush_done: u64,
}

pub(crate) struct Setup {
    pub client: Client,
    pub game: String,
    pub data_dir: PathBuf,
    pub device: String,
    pub platform: String,
    pub version: String,
    pub build: String,
    pub enabled: bool,
}

fn queue_file(dir: &Path, game: &str) -> PathBuf {
    dir.join(format!("partii-analytics-{}.json", crate::slot_for(game)))
}

/// The id of this launch (also in the panic hook's reports).
static SESSION: Mutex<Option<(String, String)>> = Mutex::new(None);

fn new_session() -> String {
    let mut b = [0u8; 12];
    let _ = getrandom::fill(&mut b);
    b.iter()
        .map(|x| {
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"[(*x & 63) as usize]
                as char
        })
        .collect()
}

/// The platform name partii uses for this build's OS.
pub fn this_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "ios" => "ios",
        "android" => "android",
        "linux" => "linux",
        _ => "",
    }
}

impl Analytics {
    pub(crate) fn start(s: Setup, st: Arc<Mutex<State>>) -> (Analytics, String) {
        let session = new_session();
        *SESSION.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((session.clone(), s.version.clone()));
        let mut q = Queue {
            enabled: s.enabled,
            ..Default::default()
        };
        // What an earlier launch couldn't send.
        let file = queue_file(&s.data_dir, &s.game);
        if let Some(v) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            if s.enabled {
                q.events = v["events"].as_array().cloned().unwrap_or_default();
                q.errors = v["errors"].as_array().cloned().unwrap_or_default();
            }
            let _ = std::fs::remove_file(&file);
        }
        if s.enabled {
            q.events.push(json!({ "name": "session_start", "at": now_ms(), "session": session, "props": { "platform": s.platform, "version": s.version, "build": s.build } }));
        }
        let q = Arc::new((Mutex::new(q), Condvar::new()));
        let shared = q.clone();
        let sess = session.clone();
        let file = queue_file(&s.data_dir, &s.game);
        std::thread::Builder::new()
            .name("partii-analytics".into())
            .spawn(move || run(s, shared, st, sess))
            .expect("partii-analytics thread");
        (
            Analytics {
                q,
                started: Instant::now(),
                file,
            },
            session,
        )
    }

    fn with<R>(&self, f: impl FnOnce(&mut Queue) -> R) -> R {
        let mut g = self.q.0.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }

    pub(crate) fn track(&self, session: &str, name: &str, props: Value) {
        self.with(|q| {
            if !q.enabled {
                return;
            }
            q.events.push(json!({ "name": name, "at": now_ms(), "session": session, "props": if props.is_object() { props } else { json!({}) } }));
            if q.events.len() > MAX_QUEUED {
                let over = q.events.len() - MAX_QUEUED;
                q.events.drain(..over);
            }
        });
    }

    pub(crate) fn report_error(
        &self,
        session: &str,
        kind: &str,
        message: &str,
        backtrace: Option<&str>,
        context: Value,
    ) {
        self.with(|q| {
            if !q.enabled {
                return;
            }
            q.errors.push(error_json(
                kind,
                message,
                backtrace,
                context,
                session,
                now_ms(),
            ));
            if q.errors.len() > MAX_ERRORS_QUEUED {
                q.errors.remove(0);
            }
            q.flush_req += 1;
        });
        self.q.1.notify_all();
    }

    pub(crate) fn set_enabled(&self, on: bool) {
        self.with(|q| {
            q.enabled = on;
            if !on {
                q.events.clear();
                q.errors.clear();
            }
        });
    }

    pub(crate) fn set_owned(&self, owned: bool) {
        self.with(|q| q.owned = Some(owned));
    }

    /// Queue session_end, send everything, and wait up to `wait` for it.
    /// True when all of it was sent; what wasn't is kept on disk for the
    /// next launch.
    pub(crate) fn end_session(&self, session: &str, wait: Duration) -> bool {
        let secs = (self.started.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        self.track(session, "session_end", json!({ "secs": secs }));
        let (m, cv) = &*self.q;
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        g.flush_req += 1;
        let mine = g.flush_req;
        cv.notify_all();
        let until = Instant::now() + wait;
        while g.flush_done < mine {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = cv
                .wait_timeout(g, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        let all_sent = g.events.is_empty() && g.errors.is_empty();
        if !all_sent && g.enabled {
            let _ = std::fs::write(
                &self.file,
                json!({ "events": g.events, "errors": g.errors }).to_string(),
            );
        }
        all_sent
    }
}

fn error_json(
    kind: &str,
    message: &str,
    backtrace: Option<&str>,
    context: Value,
    session: &str,
    at: u64,
) -> Value {
    let mut e = json!({ "kind": kind, "message": message, "session": session, "at": at, "context": if context.is_object() { context } else { json!({}) } });
    if let Some(b) = backtrace {
        e["backtrace"] = json!(b);
    }
    e
}

/// What happened to one send.
enum Sent {
    Ok,
    /// Offline or the platform is busy: keep it and try later.
    Later,
    /// Refused as malformed: drop it (sending it again won't help).
    Drop,
}

fn post(c: &Client, op: &str, game: &str, body: Value) -> Sent {
    match c.json(op, &[("game", game)], Some(body)) {
        Ok(v) if v["ok"] == true => Sent::Ok,
        Ok(v) => match v["error"]["code"].as_str() {
            Some("bad_request") | Some("not_found") | Some("unauthorized") => Sent::Drop,
            _ => Sent::Later,
        },
        Err(_) => Sent::Later,
    }
}

fn run(s: Setup, q: Arc<(Mutex<Queue>, Condvar)>, st: Arc<Mutex<State>>, session: String) {
    let (m, cv) = &*q;
    let mut wait = EVERY;
    loop {
        // Sleep until the next tick or a flush.
        let asked = {
            let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
            let until = Instant::now() + wait;
            while g.flush_req == g.flush_done {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                g = cv
                    .wait_timeout(g, left)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
            g.flush_req
        };
        // A panic written by the hook (an earlier launch, or a thread of this one).
        let panic_path = s.data_dir.join(PANIC_FILE);
        if let Some(list) = std::fs::read_to_string(&panic_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        {
            let _ = std::fs::remove_file(&panic_path);
            let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
            if g.enabled {
                for p in list {
                    let ctx = json!({ "thread": p["thread"], "location": p["location"], "version_at_crash": p["version"], "previous_launch": p["session"] != json!(session) });
                    g.errors.push(error_json(
                        "panic",
                        p["message"].as_str().unwrap_or("panic"),
                        p["backtrace"].as_str(),
                        ctx,
                        p["session"].as_str().unwrap_or(""),
                        p["at"].as_u64().unwrap_or_else(now_ms),
                    ));
                }
            }
        }
        let mut c = s.client.clone();
        c.player_token = lock(&st).player_token.clone();
        let mut ok = true;
        // Errors first, then events, a batch at a time.
        loop {
            let (batch, owned, is_errors) = {
                let g = m.lock().unwrap_or_else(|p| p.into_inner());
                if !g.enabled {
                    break;
                }
                if !g.errors.is_empty() {
                    (
                        g.errors
                            .iter()
                            .take(ERRORS_BATCH)
                            .cloned()
                            .collect::<Vec<_>>(),
                        g.owned,
                        true,
                    )
                } else if !g.events.is_empty() {
                    (
                        g.events.iter().take(BATCH).cloned().collect::<Vec<_>>(),
                        g.owned,
                        false,
                    )
                } else {
                    break;
                }
            };
            let n = batch.len();
            let mut body = json!({ "device": s.device, "platform": s.platform, "version": s.version, "build": s.build });
            if let Some(o) = owned {
                body["owned"] = json!(o);
            }
            body[if is_errors { "errors" } else { "events" }] = Value::Array(batch);
            match post(
                &c,
                if is_errors {
                    "errorsIngest"
                } else {
                    "analyticsIngest"
                },
                &s.game,
                body,
            ) {
                Sent::Ok | Sent::Drop => {
                    let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
                    let list = if is_errors {
                        &mut g.errors
                    } else {
                        &mut g.events
                    };
                    let n = n.min(list.len());
                    list.drain(..n);
                }
                Sent::Later => {
                    ok = false;
                    break;
                }
            }
        }
        let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
        g.flush_done = g.flush_done.max(asked);
        // Offline: keep what's queued on disk (a crash or kill loses nothing sent before).
        if !ok && g.enabled {
            let _ = std::fs::write(
                queue_file(&s.data_dir, &s.game),
                json!({ "events": g.events, "errors": g.errors }).to_string(),
            );
        } else {
            let _ = std::fs::remove_file(queue_file(&s.data_dir, &s.game));
        }
        wait = if ok {
            EVERY
        } else {
            (wait * 2).min(Duration::from_secs(300))
        };
        drop(g);
        cv.notify_all();
    }
}

/// Write panics to `data_dir` so the account reports them (at once if the
/// game survives, else at the next launch). Call it first thing in main;
/// the existing hook (the usual stderr message) still runs.
pub fn install_panic_hook(data_dir: impl Into<PathBuf>) {
    let dir: PathBuf = data_dir.into();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "panic".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let (session, version) = SESSION
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default();
        let path = dir.join(PANIC_FILE);
        let mut list: Vec<Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        list.push(json!({ "message": message, "location": location, "backtrace": backtrace, "thread": std::thread::current().name().unwrap_or(""), "session": session, "version": version, "at": now_ms() }));
        let over = list.len().saturating_sub(PANICS_KEPT);
        list.drain(..over);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, Value::Array(list).to_string());
        previous(info);
    }));
}
