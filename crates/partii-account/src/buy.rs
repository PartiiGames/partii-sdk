//! `buy(item)` for native games (Partii's docs/BUYING.md): Partii's buy sheet in the player's browser,
//! and the game waits for the edge to say how it ended (`playersBuyStatus`, polled). The player confirms
//! on Partii, never in the game; nothing the browser says is trusted.
//!
//! ```ignore
//! let pid = account.buy("gold-tank");        // opens partii.games/buy/<game>/gold-tank in the browser
//! account.buy_game();                        // or the game itself (its library copy: owns("full"))
//! // each frame:
//! if let Some(done) = account.take_buy() {
//!     if let BuyStatus::Bought { .. } = done.status { unlock("gold-tank") }
//! }
//! // on every launch: account.item_count("gold-tank") (playersItems) says what's owned.
//! ```
//!
//! The sheet buys as the GAME's player, whoever the browser is signed in as: `buy` asks the edge for a
//! buy link first (`playersBuyLink`: the sheet's address with a one-time code, 5 minutes, one use). A
//! guest signs up on the sheet, which upgrades this very player; if they sign in to an account they
//! already had, the guest joins it, `playersBuyStatus` answers `moved_to`, and the game signs in again
//! (its device guest call lands on that account). An edge without buy links gets the plain address,
//! which buys as whoever the browser is signed in as.

use partii_client::Client;
use serde_json::json;
use std::time::{Duration, Instant};

/// How a buy ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuyStatus {
    /// Bought: the player now holds `count` of the item.
    Bought { count: i64 },
    /// The sheet closed without buying.
    Cancelled,
    /// The payment went through but the item couldn't be added (its Pops are in the player's wallet).
    Failed(String),
    /// No answer in time (or the sheet was signed in as another account).
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuyOutcome {
    pub purchase_id: String,
    pub item: String,
    pub status: BuyStatus,
}

/// The item id that means the game itself (its library copy, `owns("full")`).
pub const GAME: &str = "game";

/// How long a buy is waited for.
pub const BUY_WAIT: Duration = Duration::from_secs(15 * 60);
const POLL: Duration = Duration::from_secs(2);

/// partii.games for an edge (`PARTII_STORE` overrides): staging edges buy on staging.partii.games.
pub fn store_for(edge: &str) -> String {
    if let Ok(s) = std::env::var("PARTII_STORE") {
        if s.starts_with("https://") || s.starts_with("http://") {
            return s.trim_end_matches('/').to_string();
        }
    }
    if edge.contains("staging") {
        "https://staging.partii.games".into()
    } else {
        "https://partii.games".into()
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The sheet's address.
pub fn buy_url(store: &str, game: &str, item: &str, purchase_id: &str, quantity: u32) -> String {
    let mut u = format!(
        "{}/buy/{}/{}?purchase_id={}",
        store.trim_end_matches('/'),
        enc(game),
        enc(item),
        enc(purchase_id)
    );
    if quantity > 1 {
        u.push_str(&format!("&quantity={quantity}"));
    }
    u
}

/// The sheet's address from a `playersBuyLink` answer (None: refused, or an edge without buy links).
pub fn link_url(v: &serde_json::Value) -> Option<String> {
    if v["ok"] != true {
        return None;
    }
    v["url"]
        .as_str()
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_string)
}

/// Ask the edge for a buy link (the sheet buys as this player). None on an older edge or any failure:
/// the caller opens the plain `buy_url` instead.
pub fn request_link(
    c: &Client,
    game: &str,
    item: &str,
    purchase_id: &str,
    quantity: u32,
) -> Option<String> {
    let mut body = json!({ "game": game, "item": item, "purchase_id": purchase_id });
    if quantity > 1 {
        body["quantity"] = json!(quantity);
    }
    c.json("playersBuyLink", &[], Some(body))
        .ok()
        .and_then(|v| link_url(&v))
}

/// The account this player joined on the sheet (`moved_to`), if it did.
pub fn moved_to(v: &serde_json::Value) -> Option<String> {
    v["moved_to"]["id"].as_str().map(str::to_string)
}

/// A new purchase id (one per purchase; the same one again never buys twice).
pub fn new_purchase_id() -> String {
    let mut b = [0u8; 10];
    let _ = getrandom::fill(&mut b);
    format!(
        "pur_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

/// Open an https URL in the system browser. False when it couldn't (the game shows the URL instead).
pub fn open_in_browser(url: &str) -> bool {
    // Tests never open a browser (the address goes to take_buy_url instead).
    if cfg!(test) || !url.starts_with("https://") && !url.starts_with("http://") {
        return false;
    }
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    // rundll32 takes the URL as one argument (cmd's `start` would split it at &).
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(not(any(unix, target_os = "windows")))]
    let r: std::io::Result<std::process::Child> = Err(std::io::Error::other("no browser"));
    r.is_ok()
}

/// What `playersBuyStatus` answered, as an outcome once it's final (None: still open).
pub fn outcome_of(v: &serde_json::Value) -> Option<BuyStatus> {
    match v["status"].as_str().unwrap_or("") {
        "bought" => Some(BuyStatus::Bought {
            count: v["count"].as_i64().unwrap_or(1),
        }),
        "cancelled" => Some(BuyStatus::Cancelled),
        "failed" => Some(BuyStatus::Failed(
            v["error"].as_str().unwrap_or("failed").to_string(),
        )),
        // Bought, then refunded before this answer: not the player's any more.
        "refunded" => Some(BuyStatus::Failed("refunded".into())),
        _ => None,
    }
}

/// Poll the edge until the buy is final or `until`. `wait` sleeps between polls (tests pass a no-op).
pub fn poll_buy(
    c: &Client,
    purchase_id: &str,
    until: Instant,
    wait: &dyn Fn(Duration),
) -> BuyStatus {
    poll_buy_following(c, purchase_id, until, wait).0
}

/// `poll_buy`, and the account the player joined on the sheet (`moved_to`) if they signed in to one there.
pub fn poll_buy_following(
    c: &Client,
    purchase_id: &str,
    until: Instant,
    wait: &dyn Fn(Duration),
) -> (BuyStatus, Option<String>) {
    let mut moved = None;
    loop {
        if let Ok(v) = c.json("playersBuyStatus", &[("purchase_id", purchase_id)], None) {
            moved = moved_to(&v).or(moved);
            if let Some(s) = outcome_of(&v) {
                return (s, moved);
            }
        }
        if Instant::now() >= until {
            return (BuyStatus::TimedOut, moved);
        }
        wait(POLL);
    }
}

/// Tell the edge the game gave up waiting (so a later status says cancelled, unless it was paid).
pub fn cancel_buy(c: &Client, purchase_id: &str, game: &str, item: &str) {
    let _ = c.json(
        "playersBuyCancel",
        &[("purchase_id", purchase_id)],
        Some(json!({ "game": game, "item": item })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sheet_url_and_store() {
        assert_eq!(
            buy_url(
                "https://partii.games/",
                "tankii",
                "gold-tank",
                "pur_ab12",
                1
            ),
            "https://partii.games/buy/tankii/gold-tank?purchase_id=pur_ab12"
        );
        assert_eq!(
            buy_url("https://partii.games", "tankii", "gem", "pur_x", 3),
            "https://partii.games/buy/tankii/gem?purchase_id=pur_x&quantity=3"
        );
        assert_eq!(
            store_for("https://api-staging.partii.games"),
            "https://staging.partii.games"
        );
        assert_eq!(
            store_for("https://api.partii.games"),
            "https://partii.games"
        );
        let a = new_purchase_id();
        assert!(a.starts_with("pur_") && a.len() == 24 && a != new_purchase_id());
    }

    #[test]
    fn only_final_statuses_end_the_wait() {
        assert_eq!(
            outcome_of(&json!({"status": "bought", "count": 2})),
            Some(BuyStatus::Bought { count: 2 })
        );
        assert_eq!(
            outcome_of(&json!({"status": "cancelled"})),
            Some(BuyStatus::Cancelled)
        );
        assert_eq!(
            outcome_of(&json!({"status": "failed", "error": "owned"})),
            Some(BuyStatus::Failed("owned".into()))
        );
        assert_eq!(
            outcome_of(&json!({"status": "refunded"})),
            Some(BuyStatus::Failed("refunded".into()))
        );
        assert_eq!(outcome_of(&json!({"status": "open"})), None);
        assert_eq!(outcome_of(&json!({"status": "none"})), None);
    }

    #[test]
    fn a_link_answer_or_nothing() {
        assert_eq!(link_url(&json!({"ok": true, "url": "https://partii.games/buy/tankii/game?purchase_id=pur_1&link=bl_x"})).as_deref(), Some("https://partii.games/buy/tankii/game?purchase_id=pur_1&link=bl_x"));
        // An older edge (no such route) or a refusal: the plain address instead.
        assert_eq!(
            link_url(&json!({"ok": false, "error": {"code": "not_found"}})),
            None
        );
        assert_eq!(
            link_url(&json!({"ok": true, "url": "javascript:alert(1)"})),
            None
        );
        assert_eq!(
            moved_to(&json!({"status": "bought", "moved_to": {"id": "p_ada"}})).as_deref(),
            Some("p_ada")
        );
        assert_eq!(moved_to(&json!({"status": "bought"})), None);
        let c = Client {
            base: "http://127.0.0.1:9".into(),
            ..Default::default()
        };
        assert_eq!(request_link(&c, "tankii", "game", "pur_x", 1), None);
    }

    #[test]
    fn no_edge_times_out() {
        let c = Client {
            base: "http://127.0.0.1:9".into(),
            ..Default::default()
        };
        assert_eq!(
            poll_buy(&c, "pur_x", Instant::now(), &|_| {}),
            BuyStatus::TimedOut
        );
    }
}
