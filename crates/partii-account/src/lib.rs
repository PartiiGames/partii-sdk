//! The player's partii account inside a game.
//!
//! * **Sign-in**: a guest account per device (a random id kept in the game's
//!   data folder), created on first launch with no form. `login` links
//!   email, Apple, Google, Discord or Steam later: the guest becomes (or
//!   merges into) that account, and this device signs in to it from then on.
//! * **Other calls** (player content, …): `client()` is an API client
//!   signed in as this player, for any operation in API.md.
//! * **Entitlements**: which SKUs the player owns. The last answer is cached
//!   for a week, so a paid game stays unlocked offline.
//! * **Cloud saves**: named JSON slots. Reads come from a local mirror;
//!   writes queue. When another device wrote first, the game's merge
//!   function combines both copies and the write is retried.
//! * **Checkout**: a store URL to open in the browser; `refresh` afterwards.
//! * **The partii launcher**: started from it (PARTII_LAUNCH_TICKET set), the
//!   game signs in as the launcher's player by itself; see
//!   `started_from_launcher`.
//! * **Analytics**: `track` events and `report_error`s, batched to the
//!   developer's dashboard (`analytics.rs`); `end_session` on quit.
//! * **Achievements**: `unlock_achievement` / `achievement_progress`. They
//!   queue on disk, so unlocks made offline reach partii later with the time
//!   they happened; `take_unlocked` lists the new ones for an in-game toast.
//!
//! All network work runs on one background thread; the frame never waits.
//!
//! ```no_run
//! use partii_account::{Account, AccountConfig};
//! let acct = Account::start(AccountConfig {
//!     game: "mygame".into(),
//!     key: Some("pk_mystudio_game_...".into()),
//!     data_dir: "/path/to/save/folder".into(),
//!     slots: vec!["progress".into()],
//!     ..AccountConfig::default()
//! });
//! // each frame / when needed:
//! if acct.owns("full") == Some(true) { /* unlock */ }
//! acct.write_save("progress", r#"{"level":3}"#);
//! acct.track("level_complete", serde_json::json!({ "mission": 3, "secs": 81.5 }));
//! // on quit:
//! acct.end_session(std::time::Duration::from_secs(2));
//! ```

mod analytics;
pub use analytics::{install_panic_hook, this_platform, MAX_QUEUED};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use partii_client::Client;
use serde_json::{json, Value};

/// Combine this device's copy of a slot with the platform's newer copy:
/// `(slot, mine, theirs) -> merged`. `None` keeps this device's copy.
pub type Merge = fn(&str, &str, &str) -> Option<String>;

#[derive(Clone)]
pub struct AccountConfig {
    /// partii edge base URL.
    pub edge: String,
    /// The game's publishable key.
    pub key: Option<String>,
    /// A player token to use instead of guest sign-in (tests, linked accounts).
    pub player_token: Option<String>,
    /// The game's id on partii.
    pub game: String,
    /// Where the device id and entitlement cache live (the game's save folder).
    pub data_dir: PathBuf,
    pub display_name: String,
    /// Save slots fetched at sign-in (others are fetched on first read).
    pub slots: Vec<String>,
    pub merge: Option<Merge>,
    /// The game's version, platform ("macos", "ios", …; `this_platform()`)
    /// and build ("steam", "demo", "appstore", …): every event carries them.
    pub version: String,
    pub platform: String,
    pub build: String,
    /// Send events and error reports (the player's "share anonymous stats").
    pub analytics: bool,
}

impl Default for AccountConfig {
    fn default() -> Self {
        AccountConfig {
            edge: "https://api.partii.games".into(),
            key: None,
            player_token: None,
            game: String::new(),
            data_dir: PathBuf::from("."),
            display_name: "Player".into(),
            slots: Vec::new(),
            merge: None,
            version: String::new(),
            platform: this_platform().into(),
            build: String::new(),
            analytics: true,
        }
    }
}

#[derive(Default)]
struct State {
    /// Signed in and the first fetch is done.
    ready: bool,
    player_id: Option<String>,
    player_token: Option<String>,
    /// Sign-in providers linked to the player ("device" alone: a guest).
    providers: Option<Vec<String>>,
    login: Option<Result<(), String>>,
    /// A "Sign in through Steam" page to open in the browser.
    browser_url: Option<String>,
    /// SKUs owned ("full", …); None until known (fresh install offline).
    owns: Option<Vec<String>>,
    /// slot → (rev, JSON text) as the platform has it.
    saves: HashMap<String, (i64, String)>,
    checkout: Option<Result<String, String>>,
    /// The outcome of `submit_receipt`: Err((code, message)).
    receipt: Option<Result<(), (String, String)>>,
    error: Option<String>,
    /// Writes queued and not yet answered by the platform.
    pending: usize,
    /// Achievement ids unlocked (as partii has them, plus this session's).
    achieved: std::collections::HashSet<String>,
    /// Achievements partii just confirmed as newly unlocked, for `take_unlocked`.
    newly: Vec<String>,
    /// Listening for a purchase (`watch_purchase`) until this instant.
    watching: Option<std::time::Instant>,
    /// A purchase landed while watching, for `take_purchased`.
    purchased: bool,
}

enum Job {
    Write {
        slot: String,
        contents: String,
    },
    Refresh,
    Checkout,
    Login(Value),
    Receipt(Value),
    /// Carries a sender for the poller, held only while it polls.
    SteamBrowser(Sender<Job>),
    /// Signed in elsewhere (the browser poller): use this token from now on.
    SignedIn(String),
    /// An unlock or progress, already on disk: send what's queued.
    Achievements,
    /// Listen for a purchase of this game until then (the buy screen is open).
    WatchPurchase(std::time::Instant),
}

/// An achievement unlock or progress waiting to reach partii.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct QueuedAchievement {
    id: String,
    progress: Option<u64>,
    at: u64,
}

pub struct Account {
    st: Arc<Mutex<State>>,
    jobs: Sender<Job>,
    achievements_path: PathBuf,
    edge: String,
    key: Option<String>,
    game: String,
    analytics: analytics::Analytics,
    session: String,
}

/// A save slot name for a file path ("mygame/campaign-saves.json" → "campaign-saves").
pub fn slot_for(file: &str) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    base.trim_end_matches(".json")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// This install's id, made once and kept in `dir`. Creation is exclusive
/// (`O_EXCL`), so two first launches racing agree on one id. (Not a hard
/// link: Android's SELinux policy forbids links in app storage.)
pub fn device_id(dir: &Path) -> String {
    use std::io::Write;
    let path = dir.join("partii-device");
    let read = || {
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() >= 16)
    };
    if let Some(id) = read() {
        return id;
    }
    let mut b = [0u8; 18];
    let _ = getrandom::fill(&mut b);
    let id: String = b
        .iter()
        .map(|x| {
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"[(*x & 63) as usize]
                as char
        })
        .collect();
    let _ = std::fs::create_dir_all(dir);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            if f.write_all(id.as_bytes())
                .and_then(|_| f.sync_all())
                .is_ok()
            {
                return id;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another launch won; its id may still be landing.
            for _ in 0..50 {
                if let Some(theirs) = read() {
                    return theirs;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Err(_) => {}
    }
    read().unwrap_or(id)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    game: String,
    owns: Vec<String>,
    /// The platform's signed copy (kept for support and future checks).
    signed: String,
    /// Unix ms after which the cache no longer unlocks anything.
    until: u64,
}

const WEEK_MS: u64 = 7 * 86_400_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Keep what a sign-in answer says about the player.
fn signed_in(st: &Mutex<State>, v: &Value) {
    let mut s = lock(st);
    if let Some(t) = v["player_token"].as_str() {
        s.player_token = Some(t.to_string());
    }
    if let Some(id) = v["player"]["id"].as_str() {
        s.player_id = Some(id.to_string());
    }
    if let Some(p) = v["player"]["providers"].as_array() {
        s.providers = Some(
            p.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        );
    }
}

/// The one-time ticket the partii launcher passes a game it starts (PARTII_LAUNCH_TICKET).
fn launch_ticket() -> Option<String> {
    std::env::var("PARTII_LAUNCH_TICKET")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| t.starts_with("lt_"))
}

/// Was this game started from the partii launcher? (It signs in by itself;
/// a game may hide its own sign-in screen.)
pub fn started_from_launcher() -> bool {
    launch_ticket().is_some()
}

fn lock(st: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    st.lock().unwrap_or_else(|p| p.into_inner())
}

impl Account {
    pub fn start(cfg: AccountConfig) -> Account {
        let st = Arc::new(Mutex::new(State::default()));
        let cache_path = cfg
            .data_dir
            .join(format!("partii-entitlements-{}.json", slot_for(&cfg.game)));
        let achievements_path = cfg
            .data_dir
            .join(format!("partii-achievements-{}.json", slot_for(&cfg.game)));
        let ach_path = achievements_path.clone();
        // Offline: last week's entitlements still unlock.
        if let Some(c) = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Cached>(&s).ok())
        {
            if c.until > now_ms() && c.game == cfg.game {
                lock(&st).owns = Some(c.owns);
            }
        }
        let (edge, key, game) = (cfg.edge.clone(), cfg.key.clone(), cfg.game.clone());
        let (analytics, session) = analytics::Analytics::start(
            analytics::Setup {
                client: Client {
                    base: cfg.edge.clone(),
                    key: cfg.key.clone(),
                    ..Default::default()
                },
                game: cfg.game.clone(),
                data_dir: cfg.data_dir.clone(),
                device: device_id(&cfg.data_dir),
                platform: cfg.platform.clone(),
                version: cfg.version.clone(),
                build: cfg.build.clone(),
                enabled: cfg.analytics,
            },
            st.clone(),
        );
        let (tx, rx) = channel::<Job>();
        let shared = st.clone();
        std::thread::Builder::new()
            .name("partii-account".into())
            .spawn(move || {
                let mut c = Client {
                    base: cfg.edge.clone(),
                    key: cfg.key.clone(),
                    player_token: cfg.player_token.clone(),
                    ..Default::default()
                };
                // Sign in (retrying while offline), then the first fetch.
                let mut wait = 1u64;
                while c.player_token.is_none() {
                    let device = device_id(&cfg.data_dir);
                    match c.json(
                        "playersGuest",
                        &[],
                        Some(json!({ "device_id": device, "display_name": cfg.display_name })),
                    ) {
                        Ok(v) if v["ok"] == true => {
                            c.player_token = v["player_token"].as_str().map(str::to_string);
                            signed_in(&shared, &v);
                        }
                        Ok(v) => {
                            lock(&shared).error = Some(
                                v["error"]["message"]
                                    .as_str()
                                    .unwrap_or("sign-in refused")
                                    .to_string(),
                            )
                        }
                        Err(e) => lock(&shared).error = Some(e.message),
                    }
                    if c.player_token.is_none() {
                        std::thread::sleep(std::time::Duration::from_secs(wait));
                        wait = (wait * 2).min(30);
                    }
                }
                // Started from the partii launcher: sign in as its player (this
                // device's guest merges into them). If the ticket is stale or
                // used, the game just stays the guest.
                if let Some(ticket) = launch_ticket() {
                    if let Ok(v) = c.json(
                        "playersLogin",
                        &[],
                        Some(json!({ "provider": "launch", "ticket": ticket, "game": cfg.game })),
                    ) {
                        if v["ok"] == true {
                            c.player_token = v["player_token"]
                                .as_str()
                                .map(str::to_string)
                                .or(c.player_token.take());
                            signed_in(&shared, &v);
                        }
                    }
                }
                lock(&shared).player_token = c.player_token.clone();
                if lock(&shared).providers.is_none() {
                    if let Ok(v) = c.json("playersMe", &[], None) {
                        signed_in(&shared, &v);
                    }
                }
                refresh(&c, &cfg.game, &shared, &cache_path);
                for slot in &cfg.slots {
                    fetch(&c, &cfg.game, &shared, slot);
                }
                sync_achievements(&c, &cfg.game, &shared, &ach_path);
                {
                    let mut s = lock(&shared);
                    s.ready = true;
                    s.error = None;
                }
                for job in rx {
                    match job {
                        Job::Refresh => {
                            refresh(&c, &cfg.game, &shared, &cache_path);
                            send_achievements(&c, &cfg.game, &shared, &ach_path);
                        }
                        Job::Achievements => send_achievements(&c, &cfg.game, &shared, &ach_path),
                        Job::WatchPurchase(until) => {
                            // Its own thread: the wait holds a request open for up to 25 s.
                            let first = {
                                let mut s = lock(&shared);
                                let first = s.watching.is_none();
                                s.watching = Some(s.watching.map_or(until, |w| w.max(until)));
                                first
                            };
                            if first {
                                let (c, game, st, cache) = (
                                    c.clone(),
                                    cfg.game.clone(),
                                    shared.clone(),
                                    cache_path.clone(),
                                );
                                std::thread::spawn(move || watch_purchase(&c, &game, &st, &cache));
                            }
                        }
                        Job::Write { slot, contents } => {
                            write_save(&c, &cfg, &shared, &slot, contents);
                            let mut s = lock(&shared);
                            s.pending = s.pending.saturating_sub(1);
                        }
                        Job::Checkout => {
                            let out = match c.json(
                                "storeCheckout",
                                &[("game", cfg.game.as_str())],
                                Some(json!({})),
                            ) {
                                Ok(v) if v["ok"] == true => match v["url"].as_str() {
                                    Some(u) => Ok(u.to_string()),
                                    // The fake store (dev) completes at once.
                                    None => {
                                        refresh(&c, &cfg.game, &shared, &cache_path);
                                        Err("completed".into())
                                    }
                                },
                                Ok(v) => Err(v["error"]["message"]
                                    .as_str()
                                    .unwrap_or("checkout failed")
                                    .to_string()),
                                Err(e) => Err(e.message),
                            };
                            lock(&shared).checkout = Some(out);
                        }
                        Job::Login(body) => {
                            // With the guest's token along, the platform links the
                            // identity to it, or merges it into the account.
                            let out = match c.json("playersLogin", &[], Some(body)) {
                                Ok(v) if v["ok"] == true => {
                                    c.player_token = v["player_token"].as_str().map(str::to_string);
                                    signed_in(&shared, &v);
                                    after_login(&c, &cfg, &shared, &cache_path);
                                    sync_achievements(&c, &cfg.game, &shared, &ach_path);
                                    Ok(())
                                }
                                Ok(v) => Err(v["error"]["message"]
                                    .as_str()
                                    .unwrap_or("sign-in refused")
                                    .to_string()),
                                Err(e) => Err(e.message),
                            };
                            lock(&shared).login = Some(out);
                        }
                        Job::Receipt(body) => {
                            let out = match c.json("playersReceipt", &[], Some(body)) {
                                Ok(v) if v["ok"] == true => {
                                    refresh(&c, &cfg.game, &shared, &cache_path);
                                    Ok(())
                                }
                                Ok(v) => Err((
                                    v["error"]["code"].as_str().unwrap_or("refused").to_string(),
                                    v["error"]["message"]
                                        .as_str()
                                        .unwrap_or("the purchase wasn't accepted")
                                        .to_string(),
                                )),
                                Err(e) => Err(("network".to_string(), e.message)),
                            };
                            lock(&shared).receipt = Some(out);
                        }
                        Job::SteamBrowser(jobs) => {
                            // The guest's token goes along, so the guest merges into the Steam account.
                            match c.json("steamOpenidStart", &[], Some(json!({ "game": cfg.game })))
                            {
                                Ok(v) if v["ok"] == true => {
                                    lock(&shared).browser_url =
                                        v["url"].as_str().map(str::to_string);
                                    let poll = v["poll"].as_str().unwrap_or_default().to_string();
                                    let (pc, ps, back) = (c.clone(), shared.clone(), jobs);
                                    std::thread::spawn(move || {
                                        poll_browser_login(pc, poll, ps, back)
                                    });
                                }
                                Ok(v) => {
                                    lock(&shared).login = Some(Err(v["error"]["message"]
                                        .as_str()
                                        .unwrap_or("Steam sign-in refused")
                                        .to_string()))
                                }
                                Err(e) => lock(&shared).login = Some(Err(e.message)),
                            }
                        }
                        Job::SignedIn(token) => {
                            c.player_token = Some(token);
                            after_login(&c, &cfg, &shared, &cache_path);
                            sync_achievements(&c, &cfg.game, &shared, &ach_path);
                            lock(&shared).login = Some(Ok(()));
                        }
                    }
                }
            })
            .expect("partii-account thread");
        Account {
            st,
            jobs: tx,
            achievements_path,
            edge,
            key,
            game,
            analytics,
            session,
        }
    }

    /// Signed in and the first fetch is done.
    pub fn ready(&self) -> bool {
        lock(&self.st).ready
    }
    pub fn player_id(&self) -> Option<String> {
        lock(&self.st).player_id.clone()
    }
    /// The player token (None until signed in). It changes after `login`.
    pub fn player_token(&self) -> Option<String> {
        lock(&self.st).player_token.clone()
    }
    /// An API client signed in as this player, for any operation (player
    /// content: `ugcPublish`, `ugcVote`, …). Calls block, so make them off
    /// the frame. None until signed in; ask again after `login`.
    pub fn client(&self) -> Option<partii_client::Client> {
        let token = self.player_token()?;
        Some(Client {
            base: self.edge.clone(),
            key: self.key.clone(),
            player_token: Some(token),
            ..Default::default()
        })
    }
    /// Only a device guest (no email, Steam, … linked)? None until known.
    /// Publishing player content needs a signed-in player.
    pub fn is_guest(&self) -> Option<bool> {
        lock(&self.st)
            .providers
            .as_ref()
            .map(|p| p.iter().all(|x| x == "device"))
    }
    /// The providers linked to the player ("device", "steam", "email", …).
    pub fn providers(&self) -> Vec<String> {
        lock(&self.st).providers.clone().unwrap_or_default()
    }
    /// Sign in with a provider; the guest becomes (or merges into) that
    /// account. `fields` are the provider's: Steam `{"ticket": hex}`, email
    /// `{"email", "code"}` (send the code first with `playersEmailStart`
    /// through `client()`), Apple/Google `{"id_token"}`, Discord `{"code"}`.
    /// The outcome arrives via `take_login`; saves and entitlements are
    /// fetched again.
    pub fn login(&self, provider: &str, fields: Value) {
        let mut body = if fields.is_object() {
            fields
        } else {
            json!({})
        };
        body["provider"] = json!(provider);
        if body.get("game").is_none() {
            body["game"] = json!(self.game);
        }
        lock(&self.st).login = None;
        let _ = self.jobs.send(Job::Login(body));
    }
    pub fn take_login(&self) -> Option<Result<(), String>> {
        lock(&self.st).login.take()
    }
    /// Hand the platform a purchase made in a platform store, to verify and
    /// unlock the game on every platform: App Store `("appstore",
    /// {"transaction": "<StoreKit 2 Transaction.id>"})`, Google Play
    /// `("play", {"purchase_token", "product_id"})`. Use it for new
    /// purchases and for restores (each of `Transaction.currentEntitlements`).
    /// The outcome arrives via `take_receipt`; on success `owns` is fetched
    /// again. Errors are (code, message); `claimed_by_another_account` means
    /// the purchase belongs to another signed-in partii account (ask the
    /// player to sign in to it). Finish the StoreKit transaction only after Ok.
    pub fn submit_receipt(&self, store: &str, fields: Value) {
        let mut body = if fields.is_object() {
            fields
        } else {
            json!({})
        };
        body["store"] = json!(store);
        body["game"] = json!(self.game);
        lock(&self.st).receipt = None;
        let _ = self.jobs.send(Job::Receipt(body));
    }
    pub fn take_receipt(&self) -> Option<Result<(), (String, String)>> {
        lock(&self.st).receipt.take()
    }
    /// "Sign in through Steam" in the browser: needs no Steam key, so it
    /// works for every game (and is the fallback when `login("steam", …)`
    /// answers `steam_not_set_up`). Open the URL from `take_browser_url` in
    /// the browser; the outcome arrives via `take_login`.
    pub fn steam_browser_login(&self) {
        let mut s = lock(&self.st);
        s.login = None;
        s.browser_url = None;
        drop(s);
        let _ = self.jobs.send(Job::SteamBrowser(self.jobs.clone()));
    }
    pub fn take_browser_url(&self) -> Option<String> {
        lock(&self.st).browser_url.take()
    }
    /// Does the player own `sku` (e.g. "full")? None while unknown.
    pub fn owns(&self, sku: &str) -> Option<bool> {
        lock(&self.st)
            .owns
            .as_ref()
            .map(|o| o.iter().any(|s| s == sku))
    }
    /// The last sign-in or network problem (cleared once signed in).
    pub fn error(&self) -> Option<String> {
        lock(&self.st).error.clone()
    }
    /// Ask the platform again (after a purchase in the browser).
    pub fn refresh(&self) {
        let _ = self.jobs.send(Job::Refresh);
    }
    /// Listen for this player buying the game, for `for_` (keep calling while a buy
    /// screen is open). partii answers within about a second of the purchase;
    /// `owns` then turns true and `take_purchased` returns true once.
    pub fn watch_purchase(&self, for_: std::time::Duration) {
        let _ = self
            .jobs
            .send(Job::WatchPurchase(std::time::Instant::now() + for_));
    }
    /// True once after a purchase that `watch_purchase` heard about.
    pub fn take_purchased(&self) -> bool {
        std::mem::take(&mut lock(&self.st).purchased)
    }
    /// Start buying the game; the URL to open arrives via `take_checkout`.
    pub fn start_checkout(&self) {
        let _ = self.jobs.send(Job::Checkout);
    }
    pub fn take_checkout(&self) -> Option<Result<String, String>> {
        lock(&self.st).checkout.take()
    }
    /// A slot's JSON text as last seen (None if empty or not fetched).
    pub fn read_save(&self, slot: &str) -> Option<String> {
        lock(&self.st)
            .saves
            .get(slot)
            .map(|(_, t)| t.clone())
            .filter(|t| t != "null")
    }
    /// The slot's revision as last seen (0 = never written).
    pub fn save_rev(&self, slot: &str) -> i64 {
        lock(&self.st).saves.get(slot).map(|(r, _)| *r).unwrap_or(0)
    }
    /// Queue a write of JSON text (it is stored as parsed JSON) to a slot.
    pub fn write_save(&self, slot: &str, contents: &str) -> bool {
        lock(&self.st).pending += 1;
        let ok = self
            .jobs
            .send(Job::Write {
                slot: slot.to_string(),
                contents: contents.to_string(),
            })
            .is_ok();
        if !ok {
            let mut s = lock(&self.st);
            s.pending = s.pending.saturating_sub(1);
        }
        ok
    }
    /// Writes not yet stored by the platform. Wait for 0 (briefly) before
    /// quitting if a save must not be lost; writes made offline wait for
    /// sign-in.
    pub fn pending_writes(&self) -> usize {
        lock(&self.st).pending
    }

    /// Record an event: a name like "level_complete" ([a-z][a-z0-9_.]*)
    /// and flat props (strings up to 200 chars, numbers, bools). Queued and
    /// sent in batches; nothing happens with analytics off.
    pub fn track(&self, name: &str, props: Value) {
        self.analytics.track(&self.session, name, props);
    }
    /// Report a bug: kind "panic", "error" or "warning", a message, an
    /// optional backtrace, and flat context (screen, mode, mission, …).
    /// Reports of the same bug group together on the dashboard. Sent at once.
    pub fn report_error(&self, kind: &str, message: &str, backtrace: Option<&str>, context: Value) {
        self.analytics
            .report_error(&self.session, kind, message, backtrace, context);
    }
    /// The player's "share anonymous stats" switch. Off drops what's queued.
    pub fn set_analytics(&self, on: bool) {
        self.analytics.set_enabled(on);
    }
    /// Full game (true) or demo (false), for the dashboard's filter.
    pub fn set_owned(&self, owned: bool) {
        self.analytics.set_owned(owned);
    }
    /// This launch's session id (events and error reports carry it).
    pub fn session_id(&self) -> &str {
        &self.session
    }
    /// Unlock an achievement (its id from `partii achievements set`). Safe to
    /// call every time it's earned: it's sent once, queued on disk while
    /// offline, and `achieved` is true at once.
    pub fn unlock_achievement(&self, id: &str) {
        self.queue_achievement(id, None);
    }
    /// Progress toward an achievement with a goal (steps so far, e.g. rounds
    /// won); it unlocks when progress reaches the goal. Progress only goes up.
    pub fn achievement_progress(&self, id: &str, progress: u64) {
        self.queue_achievement(id, Some(progress));
    }
    fn queue_achievement(&self, id: &str, progress: Option<u64>) {
        {
            // The state lock also guards the queue file (the worker edits it too).
            let mut s = lock(&self.st);
            if s.achieved.contains(id) {
                return;
            }
            if progress.is_none() {
                s.achieved.insert(id.to_string());
            }
            let mut q = read_queue(&self.achievements_path);
            q.retain(|a| !(a.id == id && progress.is_some() && a.progress.is_some()));
            q.push(QueuedAchievement {
                id: id.to_string(),
                progress,
                at: now_ms(),
            });
            write_queue(&self.achievements_path, &q);
        }
        let _ = self.jobs.send(Job::Achievements);
    }
    /// Unlocked (as partii has it, or unlocked on this device and queued).
    pub fn achieved(&self, id: &str) -> bool {
        lock(&self.st).achieved.contains(id)
    }
    /// Achievements partii just confirmed as newly unlocked, once each:
    /// show a toast for them.
    pub fn take_unlocked(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.st).newly)
    }

    /// On quit: records session_end {secs} and sends what's queued, waiting
    /// up to `wait`. True when everything went; the rest is sent next launch.
    pub fn end_session(&self, wait: std::time::Duration) -> bool {
        self.analytics.end_session(&self.session, wait)
    }
}

fn read_queue(path: &Path) -> Vec<QueuedAchievement> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_queue(path: &Path, q: &[QueuedAchievement]) {
    if q.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let _ = std::fs::write(path, serde_json::to_string(q).unwrap_or_default());
    }
}

/// Send queued achievements in order. Answered ones leave the queue (refusals
/// too: an id the game doesn't define won't start working by retrying); a
/// network failure keeps the rest for later.
fn send_achievements(c: &Client, game: &str, st: &Mutex<State>, path: &Path) {
    if c.player_token.is_none() {
        return;
    }
    loop {
        let Some(a) = read_queue(path).first().cloned() else {
            return;
        };
        let mut body = json!({ "at": a.at });
        if let Some(p) = a.progress {
            body["progress"] = json!(p);
        }
        let reply = c.json(
            "achievementsUnlock",
            &[("game", game), ("achievement", a.id.as_str())],
            Some(body),
        );
        if matches!(&reply, Err(_)) || matches!(&reply, Ok(v) if v["error"]["code"] == "internal") {
            return;
        }
        let mut s = lock(st);
        if let Ok(v) = &reply {
            if v["unlocked"] == true {
                s.achieved.insert(a.id.clone());
            }
            if v["newly"] == true {
                s.newly.push(a.id.clone());
            }
        }
        // The game may have queued more meanwhile: drop just the one sent.
        let mut q = read_queue(path);
        if let Some(i) = q.iter().position(|d| d.id == a.id && d.at == a.at) {
            q.remove(i);
        }
        write_queue(path, &q);
    }
}

/// After sign-in: send what's queued, then learn what's already unlocked.
fn sync_achievements(c: &Client, game: &str, st: &Mutex<State>, path: &Path) {
    send_achievements(c, game, st, path);
    if let Ok(v) = c.json("playersAchievements", &[("game", game)], None) {
        if let Some(list) = v["achievements"].as_array() {
            let mut s = lock(st);
            for a in list {
                if a["unlocked_at"].is_number() {
                    if let Some(id) = a["id"].as_str() {
                        s.achieved.insert(id.to_string());
                    }
                }
            }
        }
    }
}

/// After signing in to an account: a merge can change what the player owns and has saved.
fn after_login(c: &Client, cfg: &AccountConfig, st: &Mutex<State>, cache: &Path) {
    refresh(c, &cfg.game, st, cache);
    let slots: Vec<String> = lock(st)
        .saves
        .keys()
        .cloned()
        .chain(cfg.slots.iter().cloned())
        .collect();
    for slot in slots {
        fetch(c, &cfg.game, st, &slot);
    }
}

/// Poll a browser sign-in (every 2 s, up to its 30 minutes) on its own thread,
/// so queued saves keep flowing meanwhile.
fn poll_browser_login(c: Client, poll: String, st: Arc<Mutex<State>>, jobs: Sender<Job>) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(31 * 60);
    while std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_secs(2));
        let Ok(v) = c.json("steamOpenidPoll", &[], Some(json!({ "poll": poll }))) else {
            continue;
        };
        match v["status"].as_str() {
            Some("pending") => continue,
            Some("done") => {
                signed_in(&st, &v);
                lock(&st).browser_url = None;
                if let Some(t) = v["player_token"].as_str() {
                    let _ = jobs.send(Job::SignedIn(t.to_string()));
                }
                return;
            }
            _ => break,
        }
    }
    let mut s = lock(&st);
    s.browser_url = None;
    s.login = Some(Err("Steam sign-in wasn't finished in time".into()));
}

fn fetch(c: &Client, game: &str, st: &Mutex<State>, slot: &str) {
    if let Ok(v) = c.json("savesGet", &[("game", game), ("slot", slot)], None) {
        if v["ok"] == true {
            let rev = v["save"]["rev"].as_i64().unwrap_or(0);
            let text = serde_json::to_string(&v["save"]["data"]).unwrap_or_default();
            lock(st).saves.insert(slot.to_string(), (rev, text));
        }
    }
}

/// Hold playersEntitlementsWait open until the game is owned or the watch ends.
fn watch_purchase(c: &Client, game: &str, st: &Mutex<State>, cache: &Path) {
    loop {
        let until = lock(st).watching;
        let Some(until) = until.filter(|u| *u > std::time::Instant::now()) else {
            break;
        };
        let secs = until
            .saturating_duration_since(std::time::Instant::now())
            .as_secs()
            .clamp(1, 25)
            .to_string();
        match c.json(
            "playersEntitlementsWait",
            &[("game", game), ("timeout", secs.as_str())],
            None,
        ) {
            Ok(v) if v["owned"] == true => {
                refresh(c, game, st, cache);
                lock(st).purchased = true;
                break;
            }
            Ok(v) if v["ok"] == true => {}
            // Offline or refused: wait a little before asking again.
            _ => std::thread::sleep(std::time::Duration::from_secs(3)),
        }
    }
    lock(st).watching = None;
}

fn refresh(c: &Client, game: &str, st: &Mutex<State>, cache: &Path) {
    let Ok(v) = c.json("playersEntitlements", &[("game", game)], None) else {
        return;
    };
    if v["ok"] != true {
        return;
    }
    let owns: Vec<String> = v["entitlements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["game"] == game)
        .filter_map(|e| e["sku"].as_str().map(str::to_string))
        .collect();
    lock(st).owns = Some(owns.clone());
    let cached = Cached {
        game: game.into(),
        owns,
        signed: v["signed"].as_str().unwrap_or("").to_string(),
        until: now_ms() + WEEK_MS,
    };
    if let Ok(s) = serde_json::to_string(&cached) {
        let _ = std::fs::write(cache, s);
    }
}

/// Write a save; on a conflict, merge with the newer copy and write again.
fn write_save(c: &Client, cfg: &AccountConfig, st: &Mutex<State>, slot: &str, contents: String) {
    let Ok(mut data) = serde_json::from_str::<Value>(&contents) else {
        return;
    };
    let game = cfg.game.as_str();
    for _ in 0..3 {
        let base = lock(st).saves.get(slot).map(|(r, _)| *r).unwrap_or(0);
        match c.json(
            "savesPut",
            &[("game", game), ("slot", slot)],
            Some(json!({ "base_rev": base, "data": data })),
        ) {
            Ok(v) if v["ok"] == true => {
                let rev = v["save"]["rev"].as_i64().unwrap_or(base + 1);
                lock(st).saves.insert(
                    slot.to_string(),
                    (rev, serde_json::to_string(&data).unwrap_or_default()),
                );
                return;
            }
            Ok(v) if v["error"]["code"] == "conflict" => {
                let theirs = &v["theirs"];
                let rev = theirs["rev"].as_i64().unwrap_or(0);
                let their_text = serde_json::to_string(&theirs["data"]).unwrap_or_default();
                lock(st)
                    .saves
                    .insert(slot.to_string(), (rev, their_text.clone()));
                let mine = serde_json::to_string(&data).unwrap_or_default();
                if let Some(merged) = cfg
                    .merge
                    .and_then(|m| m(slot, &mine, &their_text))
                    .and_then(|m| serde_json::from_str(&m).ok())
                {
                    data = merged;
                }
            }
            _ => return,
        }
    }
}

/// A merge that unions two JSON objects (this device's keys win). Handy
/// for saves that are maps of independent entries.
pub fn merge_objects(_slot: &str, mine: &str, theirs: &str) -> Option<String> {
    let (Ok(Value::Object(m)), Ok(Value::Object(mut t))) = (
        serde_json::from_str::<Value>(mine),
        serde_json::from_str::<Value>(theirs),
    ) else {
        return None;
    };
    for (k, v) in m {
        t.insert(k, v);
    }
    serde_json::to_string(&Value::Object(t)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_map_to_slots() {
        assert_eq!(slot_for("tankii/campaign-saves.json"), "campaign-saves");
        assert_eq!(slot_for("my save!.json"), "my-save-");
    }

    #[test]
    fn device_id_is_made_once() {
        let dir = std::env::temp_dir().join(format!("partii-account-test-{}", now_ms()));
        let a = device_id(&dir);
        assert!(a.len() >= 16);
        assert_eq!(device_id(&dir), a);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn racing_first_launches_agree_on_one_device_id() {
        let dir = std::env::temp_dir().join(format!("partii-account-race-{}", now_ms()));
        let ids: Vec<String> = (0..8)
            .map(|_| {
                let d = dir.clone();
                std::thread::spawn(move || device_id(&d))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert!(ids.iter().all(|i| i == &ids[0]), "{ids:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn analytics_wait_on_disk_while_offline() {
        let dir = std::env::temp_dir().join(format!("partii-account-offline-{}", now_ms()));
        // Nothing listens on port 9: every send fails like a dead network.
        let cfg = AccountConfig {
            edge: "http://127.0.0.1:9".into(),
            key: Some("pk_x".into()),
            game: "offline".into(),
            data_dir: dir.clone(),
            version: "1.0.0".into(),
            ..AccountConfig::default()
        };
        let a = Account::start(cfg.clone());
        a.track("level_complete", json!({ "mission": 1 }));
        a.report_error("error", "boom", None, json!({ "screen": "menu" }));
        assert!(!a.end_session(std::time::Duration::from_secs(5)));
        let kept: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("partii-analytics-offline.json")).unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = kept["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["session_start", "level_complete", "session_end"]);
        assert_eq!(kept["errors"][0]["message"], "boom");
        assert_eq!(kept["events"][1]["session"], json!(a.session_id()));
        // The player turns stats off: the next launch drops what waited.
        let b = Account::start(AccountConfig {
            analytics: false,
            ..cfg
        });
        b.track("x", json!({}));
        assert!(b.end_session(std::time::Duration::from_millis(500)));
        assert!(!dir.join("partii-analytics-offline.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn achievements_queue_on_disk_while_offline() {
        let dir = std::env::temp_dir().join(format!("partii-account-ach-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = AccountConfig {
            edge: "http://127.0.0.1:9".into(),
            key: Some("pk_x".into()),
            game: "offline".into(),
            data_dir: dir.clone(),
            analytics: false,
            ..AccountConfig::default()
        };
        let a = Account::start(cfg);
        a.unlock_achievement("first_win");
        a.unlock_achievement("first_win");
        a.achievement_progress("hundred", 10);
        a.achievement_progress("hundred", 25);
        assert!(a.achieved("first_win"));
        assert!(!a.achieved("hundred"));
        let q: Vec<QueuedAchievement> = serde_json::from_str(
            &std::fs::read_to_string(dir.join("partii-achievements-offline.json")).unwrap(),
        )
        .unwrap();
        // One unlock (not two), and only the latest progress.
        assert_eq!(
            q.iter()
                .map(|x| (x.id.as_str(), x.progress))
                .collect::<Vec<_>>(),
            [("first_win", None), ("hundred", Some(25))]
        );
        assert!(a.take_unlocked().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A tiny stand-in for partii: every call answers ok; the purchase wait says
    /// "owned" on its second call, and entitlements list the game from then on.
    fn fake_partii() -> String {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut waits = 0;
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; len];
                let _ = std::io::Read::read_exact(&mut r, &mut body);
                let path = line.split(' ').nth(1).unwrap_or("");
                let owned = waits >= 2;
                let reply = if path.starts_with("/v1/players/me/entitlements/wait") {
                    waits += 1;
                    json!({ "ok": true, "owned": waits >= 2, "entitlements": [] })
                } else if path.starts_with("/v1/players/me/entitlements") {
                    json!({ "ok": true, "entitlements": if owned { json!([{ "game": "buyme", "sku": "full" }]) } else { json!([]) }, "signed": "" })
                } else if path.starts_with("/v1/players/guest") {
                    json!({ "ok": true, "player": { "id": "p1", "providers": ["device"] }, "player_token": "pt" })
                } else {
                    json!({ "ok": true })
                };
                let b = reply.to_string();
                let mut s = s;
                let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", b.len(), b);
            }
        });
        addr
    }

    #[test]
    fn watch_purchase_hears_the_purchase() {
        let dir = std::env::temp_dir().join(format!("partii-account-buy-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = AccountConfig {
            edge: fake_partii(),
            key: Some("pk_x".into()),
            game: "buyme".into(),
            data_dir: dir.clone(),
            analytics: false,
            ..AccountConfig::default()
        };
        let a = Account::start(cfg);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !a.ready() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(a.owns("full"), Some(false));
        a.watch_purchase(std::time::Duration::from_secs(30));
        while !a.take_purchased() {
            assert!(
                std::time::Instant::now() < until,
                "the purchase never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(a.owns("full"), Some(true));
        assert!(!a.take_purchased());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn merge_objects_unions_and_mine_wins() {
        let m = merge_objects("s", r#"{"a":1,"b":2}"#, r#"{"b":9,"c":3}"#).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&m).unwrap(),
            json!({"a":1,"b":2,"c":3})
        );
        assert_eq!(merge_objects("s", "[1]", "{}"), None);
    }
}
