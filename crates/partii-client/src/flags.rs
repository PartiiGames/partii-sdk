//! A game's feature flags from Partii (docs/FEATURE-FLAGS.md; the edge's `gamesFlags`): the Rust
//! twin of the web SDK's `flags.ts`.
//!
//! ```no_run
//! use partii_client::flags::{Flags, FlagsConfig, FlagValue, Range};
//! let flags = Flags::new(FlagsConfig {
//!     base: "https://api.partii.games".into(),
//!     key: "pk_…".into(),
//!     game: "tankii".into(),
//!     defaults: [
//!         ("net.shell_rows_on_news".to_string(), FlagValue::Bool(false)),
//!         ("net.shell_rows".to_string(), FlagValue::Int(2)),
//!     ].into(),
//!     ranges: [("net.shell_rows".to_string(), Range { min: Some(0), max: Some(4) })].into(),
//!     channel: Some("nightly".into()),
//!     platform: Some("windows".into()),
//!     cache_path: Some("/path/to/save/dir/partii-flags.json".into()),
//!     ..Default::default()
//! });
//! let rows = flags.get_int("net.shell_rows"); // at once: the kept answer, else the compiled default
//! std::thread::spawn({ let f = flags.clone(); move || { f.refresh(); } }); // never panics, never errors
//! let handshake = flags.match_values(); // the host's match flags, for the guests
//! ```
//!
//! - Never blocks a launch: [`Flags::new`] reads only the disk cache; [`Flags::get`] answers from
//!   memory. [`Flags::refresh`] is a blocking fetch with a short timeout (2 s by default): run it off
//!   the game's main thread. It never panics and never returns an error: offline, the last answer kept,
//!   else the compiled defaults.
//! - `If-None-Match` with the kept ETag; a 304 keeps the answer.
//! - Only values of the compiled default's type are taken (a bool flag never turns into a string); keys
//!   the game doesn't compile in are ignored; an int outside the edge's range (`ranges` in the answer)
//!   or the game's own ([`FlagsConfig::ranges`]) is ignored. A key Partii stops sending goes back to
//!   its compiled default.
//! - Works for guests and before sign-in: an install id (random, kept in the cache file) buckets %
//!   rollouts. [`Flags::set_player_token`] after a sign-in.
//! - Admins: [`Flags::admin`] (the overlay), [`Flags::override_flag`], [`Flags::clear_override`].

use crate::{encode, CallError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a fetch may take, by default.
pub const FLAGS_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an admin call (overlay, override, clear) may take.
pub const ADMIN_TIMEOUT: Duration = Duration::from_secs(10);

/// A flag's value: the types Partii's flags have (bool, int, enum as text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl FlagValue {
    fn to_json(&self) -> Value {
        match self {
            FlagValue::Bool(b) => json!(b),
            FlagValue::Int(n) => json!(n),
            FlagValue::Str(s) => json!(s),
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            FlagValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            FlagValue::Int(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FlagValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Partii's scope for a flag it sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only this player's game.
    Client,
    /// The game uses the host's value for everyone in the match.
    Match,
}

/// An int flag's range, inclusive; either side optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Range {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

impl Range {
    pub fn contains(&self, v: i64) -> bool {
        self.min.is_none_or(|m| v >= m) && self.max.is_none_or(|m| v <= m)
    }
    /// Both ranges at once (the tighter bound each side).
    fn and(a: Option<Range>, b: Option<Range>) -> Option<Range> {
        let side = |x: Option<i64>, y: Option<i64>, f: fn(i64, i64) -> i64| match (x, y) {
            (Some(x), Some(y)) => Some(f(x, y)),
            (x, y) => x.or(y),
        };
        match (a, b) {
            (None, None) => None,
            (a, b) => {
                let (a, b) = (a.unwrap_or_default(), b.unwrap_or_default());
                Some(Range {
                    min: side(a.min, b.min, i64::max),
                    max: side(a.max, b.max, i64::min),
                })
            }
        }
    }
}

/// What [`Flags::new`] needs. `Default` gives empty strings, no defaults, a 2 s timeout, memory only.
#[derive(Debug, Clone)]
pub struct FlagsConfig {
    /// Platform API base URL.
    pub base: String,
    /// The game's publishable key.
    pub key: String,
    pub game: String,
    /// Every flag the game knows, with the value compiled into it.
    pub defaults: BTreeMap<String, FlagValue>,
    /// The game's own int ranges (as compiled in): a value outside is ignored, as is one outside the edge's.
    pub ranges: BTreeMap<String, Range>,
    /// The signed-in player's token: player lists, rollouts by player, admin calls. Optional.
    pub player_token: Option<String>,
    /// The build channel (nightly, demo, full, ...) and platform (windows, macos, linux, ...).
    pub channel: Option<String>,
    pub platform: Option<String>,
    /// A stable id for this install (default: a random one kept in the cache file).
    pub install: Option<String>,
    /// The JSON file the last answer (and the install id) is kept in. None: memory only.
    pub cache_path: Option<PathBuf>,
    /// How long a fetch may take.
    pub timeout: Duration,
}

impl Default for FlagsConfig {
    fn default() -> Self {
        FlagsConfig {
            base: String::new(),
            key: String::new(),
            game: String::new(),
            defaults: BTreeMap::new(),
            ranges: BTreeMap::new(),
            player_token: None,
            channel: None,
            platform: None,
            install: None,
            cache_path: None,
            timeout: FLAGS_TIMEOUT,
        }
    }
}

/// Where the values came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The compiled defaults: Partii not reached yet, nothing kept.
    Defaults,
    /// The answer kept on disk.
    Cache,
    /// A fresh answer.
    Network,
    /// 304: the kept answer is current.
    Unchanged,
}

/// What a [`Flags::refresh`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refresh {
    pub ok: bool,
    /// Network or Unchanged when ok; else Cache or Defaults (what the game is using).
    pub source: Source,
    /// Why it failed: "timeout", "network: …", an error code or "status 500".
    pub error: Option<String>,
}

/// The answer kept (on disk as JSON).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    values: Map<String, Value>,
    #[serde(default)]
    scopes: BTreeMap<String, String>,
    #[serde(default)]
    ranges: BTreeMap<String, Range>,
    #[serde(default)]
    overrides: Vec<String>,
    #[serde(default = "default_refresh")]
    refresh_s: u64,
    #[serde(default)]
    at: u64,
}

fn default_refresh() -> u64 {
    300
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    kept: Option<Kept>,
}

#[derive(Debug)]
struct State {
    kept: Option<Kept>,
    source: Source,
    install: String,
    player_token: Option<String>,
}

struct Inner {
    cfg: FlagsConfig,
    state: Mutex<State>,
    /// One fetch at a time.
    fetching: Mutex<()>,
}

/// The game's flags: cheap to clone (shared), `Send + Sync`, so a thread can refresh while the game reads.
#[derive(Clone)]
pub struct Flags(Arc<Inner>);

impl std::fmt::Debug for Flags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flags")
            .field("game", &self.0.cfg.game)
            .field("source", &self.source())
            .finish()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A random install id, i_ and 20 of a-z 0-9 (std only: randomly keyed hashers).
fn random_install() -> String {
    use std::hash::{BuildHasher, Hasher};
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::from("i_");
    let mut n = 0u64;
    while out.len() < 22 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(now_ms() ^ n);
        h.write_u32(std::process::id());
        let mut x = h.finish();
        for _ in 0..8 {
            if out.len() >= 22 {
                break;
            }
            out.push(A[(x % A.len() as u64) as usize] as char);
            x /= A.len() as u64;
        }
        n += 1;
    }
    out
}

fn read_cache(path: &Option<PathBuf>) -> CacheFile {
    path.as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
        .unwrap_or_default()
}

fn write_cache(path: &Option<PathBuf>, c: &CacheFile) {
    let Some(p) = path else { return };
    let Ok(bytes) = serde_json::to_vec(c) else {
        return;
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

struct Http {
    status: u16,
    etag: Option<String>,
    body: Value,
}

/// One request with a timeout and extra headers (the generic `Client::call` has neither).
fn request(
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Http, ureq::Error> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut res = match method {
        "GET" | "DELETE" => {
            let mut r = if method == "GET" {
                agent.get(url)
            } else {
                agent.delete(url)
            };
            for (k, v) in headers {
                r = r.header(*k, v);
            }
            r.call()?
        }
        _ => {
            let mut r = agent.put(url);
            for (k, v) in headers {
                r = r.header(*k, v);
            }
            r.send_json(body.cloned().unwrap_or_else(|| json!({})))?
        }
    };
    let status = res.status().as_u16();
    let etag = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = if status == 304 {
        Value::Null
    } else {
        let bytes = res.body_mut().read_to_vec()?;
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Ok(Http { status, etag, body })
}

fn net_error(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "timeout".into(),
        e => format!("network: {e}"),
    }
}

impl Flags {
    /// Loads the kept answer from the cache file (never the network, never panics). Call
    /// [`Flags::refresh`] (off the main thread) to fetch.
    pub fn new(cfg: FlagsConfig) -> Flags {
        let mut cache = read_cache(&cfg.cache_path);
        let install = match (&cfg.install, &cache.install) {
            (Some(i), _) => i.clone(),
            (None, Some(i)) if !i.is_empty() => i.clone(),
            _ => {
                let i = random_install();
                cache.install = Some(i.clone());
                write_cache(&cfg.cache_path, &cache);
                i
            }
        };
        let source = if cache.kept.is_some() {
            Source::Cache
        } else {
            Source::Defaults
        };
        let state = State {
            kept: cache.kept,
            source,
            install,
            player_token: cfg.player_token.clone(),
        };
        Flags(Arc::new(Inner {
            cfg,
            state: Mutex::new(state),
            fetching: Mutex::new(()),
        }))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock still holds good data (nothing here panics while holding it).
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A kept value, if it's of the default's type (and an int within both ranges).
    fn fits(&self, kept: Option<&Kept>, key: &str) -> Option<FlagValue> {
        let kept = kept?;
        let v = kept.values.get(key)?;
        match self.0.cfg.defaults.get(key)? {
            FlagValue::Bool(_) => v.as_bool().map(FlagValue::Bool),
            FlagValue::Int(_) => {
                let n = v.as_i64()?;
                let ok = self.0.cfg.ranges.get(key).is_none_or(|r| r.contains(n))
                    && kept.ranges.get(key).is_none_or(|r| r.contains(n));
                ok.then_some(FlagValue::Int(n))
            }
            FlagValue::Str(_) => v.as_str().map(|s| FlagValue::Str(s.to_string())),
        }
    }

    /// A flag's value: the last answer's, else the compiled default. None: the game has no such flag.
    pub fn get(&self, key: &str) -> Option<FlagValue> {
        let st = self.state();
        self.fits(st.kept.as_ref(), key)
            .or_else(|| self.0.cfg.defaults.get(key).cloned())
    }
    /// A bool flag (false when the game has no such bool flag).
    pub fn get_bool(&self, key: &str) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
    }
    /// An int flag (0 when the game has no such int flag).
    pub fn get_int(&self, key: &str) -> i64 {
        self.get(key).and_then(|v| v.as_int()).unwrap_or(0)
    }
    /// An enum flag's value (empty when the game has no such enum flag).
    pub fn get_str(&self, key: &str) -> String {
        self.get(key)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }
    /// Every flag the game knows, with its value now.
    pub fn all(&self) -> BTreeMap<String, FlagValue> {
        let st = self.state();
        self.0
            .cfg
            .defaults
            .iter()
            .map(|(k, d)| {
                (
                    k.clone(),
                    self.fits(st.kept.as_ref(), k).unwrap_or_else(|| d.clone()),
                )
            })
            .collect()
    }
    /// Partii's scope for a flag it sent, else None.
    pub fn scope(&self, key: &str) -> Option<Scope> {
        match self.state().kept.as_ref()?.scopes.get(key)?.as_str() {
            "match" => Some(Scope::Match),
            "client" => Some(Scope::Client),
            _ => None,
        }
    }
    /// An int flag's range: the game's own narrowed by the edge's, else None.
    pub fn range(&self, key: &str) -> Option<Range> {
        let edge = self
            .state()
            .kept
            .as_ref()
            .and_then(|k| k.ranges.get(key).copied());
        Range::and(self.0.cfg.ranges.get(key).copied(), edge)
    }
    /// The values of every match flag, to send to guests in the match handshake (the host's).
    pub fn match_values(&self) -> BTreeMap<String, FlagValue> {
        let st = self.state();
        let Some(kept) = st.kept.as_ref() else {
            return BTreeMap::new();
        };
        kept.scopes
            .iter()
            .filter(|(_, s)| s.as_str() == "match")
            .filter_map(|(k, _)| {
                let d = self.0.cfg.defaults.get(k)?;
                Some((
                    k.clone(),
                    self.fits(Some(kept), k).unwrap_or_else(|| d.clone()),
                ))
            })
            .collect()
    }
    /// This value is this admin's own override.
    pub fn overridden(&self, key: &str) -> bool {
        self.state()
            .kept
            .as_ref()
            .is_some_and(|k| k.overrides.iter().any(|o| o == key))
    }
    /// Where the values came from.
    pub fn source(&self) -> Source {
        self.state().source
    }
    /// The install id rollouts bucket by (without a player token).
    pub fn install(&self) -> String {
        self.state().install.clone()
    }
    /// How often to fetch again (seconds; at least 30): the edge's suggestion, else 300.
    pub fn refresh_s(&self) -> u64 {
        self.state()
            .kept
            .as_ref()
            .map_or(300, |k| k.refresh_s)
            .max(30)
    }
    /// After a sign-in (or out): the next fetch and admin calls use it.
    pub fn set_player_token(&self, token: Option<String>) {
        self.state().player_token = token;
    }

    fn save(&self, kept: &Kept, install: &str) {
        write_cache(
            &self.0.cfg.cache_path,
            &CacheFile {
                install: Some(install.to_string()),
                kept: Some(kept.clone()),
            },
        );
    }

    /// Fetch now, blocking up to the timeout (run it off the main thread; one at a time). Never
    /// panics or errors: on failure the kept values (or the defaults) stay.
    pub fn refresh(&self) -> Refresh {
        let _one = self.0.fetching.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = &self.0.cfg;
        let (etag, token, install) = {
            let st = self.state();
            (
                st.kept.as_ref().and_then(|k| k.etag.clone()),
                st.player_token.clone(),
                st.install.clone(),
            )
        };
        let mut q = vec![format!("install={}", encode(&install))];
        if let Some(c) = &cfg.channel {
            q.push(format!("channel={}", encode(c)));
        }
        if let Some(p) = &cfg.platform {
            q.push(format!("platform={}", encode(p)));
        }
        let url = format!(
            "{}/v1/games/{}/flags?{}",
            cfg.base.trim_end_matches('/'),
            encode(&cfg.game),
            q.join("&")
        );
        let mut headers = vec![("x-partii-key", cfg.key.clone())];
        if let Some(t) = token.filter(|t| !t.is_empty()) {
            headers.push(("authorization", format!("Bearer {t}")));
        }
        if let Some(e) = &etag {
            headers.push(("if-none-match", e.clone()));
        }
        let fail = |st: &State, error: String| Refresh {
            ok: false,
            source: if st.kept.is_some() {
                Source::Cache
            } else {
                Source::Defaults
            },
            error: Some(error),
        };
        let res = request("GET", &url, &headers, None, cfg.timeout);
        let mut st = self.state();
        let res = match res {
            Ok(r) => r,
            Err(e) => return fail(&st, net_error(&e)),
        };
        if res.status == 304 {
            let Some(kept) = st.kept.as_mut() else {
                return fail(&st, "status 304 with nothing kept".into());
            };
            kept.at = now_ms();
            let kept = kept.clone();
            st.source = Source::Unchanged;
            self.save(&kept, &st.install);
            return Refresh {
                ok: true,
                source: Source::Unchanged,
                error: None,
            };
        }
        let b = &res.body;
        if !(200..300).contains(&res.status) || b.get("ok") != Some(&Value::Bool(true)) {
            let code = b
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("status {}", res.status));
            return fail(&st, code);
        }
        let parse = |k: &str| b.get(k).cloned().unwrap_or(Value::Null);
        let kept = Kept {
            etag: b
                .get("etag")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(res.etag),
            values: parse("values").as_object().cloned().unwrap_or_default(),
            scopes: serde_json::from_value(parse("scopes")).unwrap_or_default(),
            ranges: serde_json::from_value(parse("ranges")).unwrap_or_default(),
            overrides: serde_json::from_value(parse("overrides")).unwrap_or_default(),
            refresh_s: b.get("refresh_s").and_then(Value::as_u64).unwrap_or(300),
            at: now_ms(),
        };
        self.save(&kept, &st.install);
        st.kept = Some(kept);
        st.source = Source::Network;
        Refresh {
            ok: true,
            source: Source::Network,
            error: None,
        }
    }

    /// An admin call: the player token (and key) as headers, a JSON answer, or a CallError.
    fn admin_call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, CallError> {
        let cfg = &self.0.cfg;
        let token = self.state().player_token.clone().filter(|t| !t.is_empty());
        let Some(token) = token else {
            return Err(CallError {
                code: "unauthorized",
                message: "a signed-in player is needed (player_token)".into(),
                hint: "Flags::set_player_token after the player signs in".into(),
            });
        };
        let url = format!(
            "{}/v1/games/{}/flags/{path}",
            cfg.base.trim_end_matches('/'),
            encode(&cfg.game)
        );
        let headers = [
            ("x-partii-key", cfg.key.clone()),
            ("authorization", format!("Bearer {token}")),
        ];
        let r = request(method, &url, &headers, body, ADMIN_TIMEOUT).map_err(|e| CallError {
            code: "network",
            message: format!("{url}: {}", net_error(&e)),
            hint: String::new(),
        })?;
        if r.body.get("ok") == Some(&Value::Bool(true)) {
            return Ok(r.body);
        }
        let s = |p: &str| {
            r.body
                .pointer(p)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Err(CallError {
            code: match s("/error/code").as_deref() {
                Some("forbidden") => "forbidden",
                Some("not_found") => "not_found",
                Some("bad_request") => "bad_request",
                Some("unauthorized") => "unauthorized",
                _ => "error",
            },
            message: s("/error/message").unwrap_or_else(|| format!("status {}", r.status)),
            hint: s("/error/hint").unwrap_or_default(),
        })
    }

    fn query(&self) -> String {
        let cfg = &self.0.cfg;
        let mut q = vec![format!("install={}", encode(&self.install()))];
        if let Some(c) = &cfg.channel {
            q.push(format!("channel={}", encode(c)));
        }
        if let Some(p) = &cfg.platform {
            q.push(format!("platform={}", encode(p)));
        }
        q.join("&")
    }

    /// Every flag with its definition (type, default, values, min, max, scope, …), its value for you
    /// now and your override (the admin overlay, `gamesFlagsAdmin`). 403 (`forbidden`) for anyone
    /// not an admin.
    pub fn admin(&self) -> Result<Value, CallError> {
        self.admin_call("GET", &format!("admin?{}", self.query()), None)
    }

    /// An admin's own value for a flag (also while hosting), for `hours` (default 12), then a
    /// refresh. An int outside the flag's range is refused (`bad_request`).
    pub fn override_flag(
        &self,
        key: &str,
        value: FlagValue,
        hours: Option<f64>,
    ) -> Result<Value, CallError> {
        let mut body = json!({ "value": value.to_json() });
        if let Some(h) = hours {
            body["hours"] = json!(h);
        }
        let r = self.admin_call("PUT", &format!("overrides/{}", encode(key)), Some(&body))?;
        self.refresh();
        Ok(r)
    }

    /// Clear your own override of a flag, then a refresh.
    pub fn clear_override(&self, key: &str) -> Result<Value, CallError> {
        let r = self.admin_call("DELETE", &format!("overrides/{}", encode(key)), None)?;
        self.refresh();
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A request the test server saw.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    type Answer = Box<dyn Fn(&Seen) -> (u16, Option<Value>, Option<&'static str>) + Send>;

    /// A local HTTP server answering each request with answer(request); the requests come back on rx.
    fn server(answer: Answer) -> (String, mpsc::Receiver<Seen>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap_or("").to_string(),
                    parts.next().unwrap_or("").to_string(),
                );
                let mut headers = BTreeMap::new();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let seen = Seen {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).into(),
                };
                let (status, json, etag) = answer(&seen);
                let _ = tx.send(seen);
                let text = json.map(|j| j.to_string()).unwrap_or_default();
                let mut out = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                    text.len()
                );
                if !text.is_empty() {
                    out.push_str("content-type: application/json\r\n");
                }
                if let Some(e) = etag {
                    out.push_str(&format!("etag: {e}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&text);
                let _ = s.write_all(out.as_bytes());
            }
        });
        (base, rx)
    }

    fn ok(values: Value, extra: Value) -> Value {
        let mut b = json!({ "ok": true, "game": "tankii", "env": "prod", "values": values, "scopes": {}, "ranges": {}, "overrides": [], "etag": "\"e1\"", "refresh_s": 300 });
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    }

    fn cfg(base: &str) -> FlagsConfig {
        FlagsConfig {
            base: format!("{base}/"),
            key: "pk_x".into(),
            game: "tankii".into(),
            defaults: [
                ("net.shell_rows_on_news".to_string(), FlagValue::Bool(false)),
                ("net.copies".to_string(), FlagValue::Int(1)),
                ("ui.theme".to_string(), FlagValue::Str("neon".into())),
            ]
            .into(),
            ..Default::default()
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "partii-flags-{}-{name}-{}",
            std::process::id(),
            now_ms()
        ));
        d.join("flags.json")
    }

    #[test]
    fn defaults_at_once_then_the_fetch_with_key_token_install_channel_platform() {
        let (base, rx) = server(Box::new(|_| {
            (
                200,
                Some(ok(
                    json!({ "net.copies": 2, "net.shell_rows_on_news": "yes", "unknown.key": true }),
                    json!({ "scopes": { "net.copies": "match" } }),
                )),
                None,
            )
        }));
        let f = Flags::new(FlagsConfig {
            player_token: Some("pt_abc".into()),
            channel: Some("nightly".into()),
            platform: Some("windows".into()),
            ..cfg(&base)
        });
        assert_eq!(f.get_int("net.copies"), 1);
        assert_eq!(f.source(), Source::Defaults);
        assert_eq!(
            f.refresh(),
            Refresh {
                ok: true,
                source: Source::Network,
                error: None
            }
        );
        assert_eq!(f.get_int("net.copies"), 2);
        // Wrong type: the default; unknown keys: ignored.
        assert!(!f.get_bool("net.shell_rows_on_news"));
        assert_eq!(f.get_str("ui.theme"), "neon");
        assert_eq!(f.get("unknown.key"), None);
        assert_eq!(f.all().len(), 3);
        assert_eq!(f.scope("net.copies"), Some(Scope::Match));
        assert_eq!(
            f.match_values(),
            [("net.copies".to_string(), FlagValue::Int(2))].into()
        );
        let s = rx.recv().unwrap();
        assert_eq!(s.method, "GET");
        let install = f.install();
        assert!(
            install.starts_with("i_") && install.len() == 22,
            "{install}"
        );
        assert_eq!(
            s.path,
            format!("/v1/games/tankii/flags?install={install}&channel=nightly&platform=windows")
        );
        assert_eq!(s.headers["x-partii-key"], "pk_x");
        assert_eq!(s.headers["authorization"], "Bearer pt_abc");
        assert!(!s.headers.contains_key("if-none-match"));
    }

    #[test]
    fn the_answer_is_kept_on_disk_with_if_none_match_and_304() {
        let path = tmp_path("kept");
        let (base, _rx) = server(Box::new(|_| {
            (
                200,
                Some(ok(json!({ "ui.theme": "dark" }), json!({}))),
                Some("\"e1\""),
            )
        }));
        let first = Flags::new(FlagsConfig {
            cache_path: Some(path.clone()),
            ..cfg(&base)
        });
        first.refresh();
        let install = first.install();
        // The next launch: the kept answer at once, the same install, If-None-Match, 304 keeps it.
        let (base, rx) = server(Box::new(|_| (304, None, None)));
        let next = Flags::new(FlagsConfig {
            cache_path: Some(path.clone()),
            ..cfg(&base)
        });
        assert_eq!(next.source(), Source::Cache);
        assert_eq!(next.get_str("ui.theme"), "dark");
        assert_eq!(next.install(), install);
        assert_eq!(
            next.refresh(),
            Refresh {
                ok: true,
                source: Source::Unchanged,
                error: None
            }
        );
        assert_eq!(rx.recv().unwrap().headers["if-none-match"], "\"e1\"");
        assert_eq!(next.get_str("ui.theme"), "dark");
        // A broken cache file: the defaults, no panic.
        std::fs::write(&path, b"{not json").unwrap();
        let broken = Flags::new(FlagsConfig {
            cache_path: Some(path.clone()),
            ..cfg("http://127.0.0.1:9")
        });
        assert_eq!(broken.source(), Source::Defaults);
        assert_eq!(broken.get_str("ui.theme"), "neon");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn offline_errors_and_timeouts_keep_the_last_values() {
        // Nothing listening: the defaults.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}", l.local_addr().unwrap());
        drop(l);
        let f = Flags::new(cfg(&dead));
        let r = f.refresh();
        assert!(!r.ok && r.source == Source::Defaults && r.error.unwrap().starts_with("network"));
        assert_eq!(f.get_int("net.copies"), 1);
        // A server that never answers: a timeout, quickly.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let hang = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let _held: Vec<_> = l.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(5));
        });
        let f = Flags::new(FlagsConfig {
            timeout: Duration::from_millis(300),
            ..cfg(&hang)
        });
        let t = std::time::Instant::now();
        assert_eq!(f.refresh().error.as_deref(), Some("timeout"));
        assert!(t.elapsed() < Duration::from_secs(2));
        // An error after a good answer: the kept values stay.
        let (base, _rx) = server(Box::new({
            let n = std::sync::atomic::AtomicUsize::new(0);
            move |_| {
                if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    (200, Some(ok(json!({ "net.copies": 3 }), json!({}))), None)
                } else {
                    (
                        429,
                        Some(
                            json!({ "ok": false, "error": { "code": "rate_limited", "message": "too many" } }),
                        ),
                        None,
                    )
                }
            }
        }));
        let f = Flags::new(cfg(&base));
        f.refresh();
        assert_eq!(
            f.refresh(),
            Refresh {
                ok: false,
                source: Source::Cache,
                error: Some("rate_limited".into())
            }
        );
        assert_eq!(f.get_int("net.copies"), 3);
    }

    #[test]
    fn int_ranges_from_the_edge_and_the_game() {
        let answers = Arc::new(Mutex::new(vec![
            ok(
                json!({ "net.copies": 2 }),
                json!({ "ranges": { "net.copies": { "min": 0, "max": 4 } }, "etag": "\"e4\"" }),
            ),
            ok(
                json!({ "net.copies": 3 }),
                json!({ "ranges": { "net.copies": { "max": 2 } }, "etag": "\"e3\"" }),
            ),
            ok(
                json!({ "net.copies": 4 }),
                json!({ "ranges": { "net.copies": { "min": 0, "max": 4 } }, "etag": "\"e2\"" }),
            ),
            ok(
                json!({ "net.copies": 9 }),
                json!({ "ranges": { "net.copies": { "min": 0, "max": 4 } } }),
            ),
        ]));
        let a = answers.clone();
        let (base, _rx) = server(Box::new(move |_| (200, a.lock().unwrap().pop(), None)));
        let f = Flags::new(FlagsConfig {
            ranges: [(
                "net.copies".to_string(),
                Range {
                    min: Some(1),
                    max: Some(3),
                },
            )]
            .into(),
            ..cfg(&base)
        });
        f.refresh();
        assert_eq!(f.get_int("net.copies"), 1, "9 is outside both");
        assert_eq!(
            f.range("net.copies"),
            Some(Range {
                min: Some(1),
                max: Some(3)
            })
        );
        f.refresh();
        assert_eq!(f.get_int("net.copies"), 1, "4 is outside the game's own");
        f.refresh();
        assert_eq!(f.get_int("net.copies"), 1, "3 is outside the edge's");
        assert_eq!(
            f.range("net.copies"),
            Some(Range {
                min: Some(1),
                max: Some(2)
            })
        );
        f.refresh();
        assert_eq!(f.get_int("net.copies"), 2);
        assert_eq!(f.range("ui.theme"), None);
    }

    #[test]
    fn admin_overlay_override_and_clear() {
        let (base, rx) = server(Box::new(|s| match (s.method.as_str(), s.path.as_str()) {
            ("GET", p) if p.contains("/flags/admin") => (
                200,
                Some(json!({ "ok": true, "can_edit": true, "flags": [] })),
                None,
            ),
            ("PUT", _) if s.body.contains("99") => (
                400,
                Some(
                    json!({ "ok": false, "error": { "code": "bad_request", "message": "value: net.copies=99 is out of range, above its max (4)" } }),
                ),
                None,
            ),
            ("PUT", _) => (
                200,
                Some(json!({ "ok": true, "key": "net.copies", "value": 3 })),
                None,
            ),
            ("DELETE", _) => (200, Some(json!({ "ok": true, "cleared": true })), None),
            _ => (
                200,
                Some(ok(
                    json!({ "net.copies": 3 }),
                    json!({ "overrides": ["net.copies"] }),
                )),
                None,
            ),
        }));
        let f = Flags::new(FlagsConfig {
            channel: Some("full".into()),
            ..cfg(&base)
        });
        let e = f.admin().unwrap_err();
        assert_eq!(e.code, "unauthorized");
        f.set_player_token(Some("pt_admin".into()));
        assert_eq!(f.admin().unwrap()["can_edit"], json!(true));
        let s = rx.recv().unwrap();
        assert_eq!(
            s.path,
            format!(
                "/v1/games/tankii/flags/admin?install={}&channel=full",
                f.install()
            )
        );
        assert_eq!(s.headers["authorization"], "Bearer pt_admin");
        let e = f
            .override_flag("net.copies", FlagValue::Int(99), None)
            .unwrap_err();
        assert_eq!(e.code, "bad_request");
        assert!(e.message.contains("out of range"));
        let _ = rx.recv().unwrap();
        f.override_flag("net.copies", FlagValue::Int(3), Some(2.0))
            .unwrap();
        let s = rx.recv().unwrap();
        assert_eq!(
            (s.method.as_str(), s.path.as_str()),
            ("PUT", "/v1/games/tankii/flags/overrides/net.copies")
        );
        assert_eq!(
            serde_json::from_str::<Value>(&s.body).unwrap(),
            json!({ "value": 3, "hours": 2.0 })
        );
        // Then a refresh.
        assert_eq!(rx.recv().unwrap().method, "GET");
        assert_eq!(f.get_int("net.copies"), 3);
        assert!(f.overridden("net.copies"));
        f.clear_override("net.copies").unwrap();
        assert_eq!(rx.recv().unwrap().method, "DELETE");
    }
}
