//! Against a real platform (`PARTII_API` + `PARTII_KEY`, e.g. `partii dev
//! up`; skipped otherwise): guest sign-in is stable per device, a second
//! device's stale write conflicts, merges and lands, and a third device
//! reads the merged save. A player who bought nothing owns nothing.

use std::time::{Duration, Instant};

use partii_account::{merge_objects, Account, AccountConfig};
use serde_json::{json, Value};

fn wait(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < until {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out: {what}");
}

#[test]
fn saves_merge_across_devices_of_one_player() {
    let (Ok(edge), Ok(key)) = (std::env::var("PARTII_API"), std::env::var("PARTII_KEY")) else {
        eprintln!("skip: set PARTII_API and PARTII_KEY");
        return;
    };
    let dir = std::env::temp_dir().join(format!("partii-account-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = AccountConfig {
        edge,
        key: Some(key),
        game: "account-test".into(),
        data_dir: dir.clone(),
        slots: vec!["progress".into()],
        merge: Some(merge_objects),
        ..AccountConfig::default()
    };
    let a = Account::start(cfg.clone());
    let b = Account::start(cfg.clone());
    wait("both signed in", || a.ready() && b.ready());
    assert!(a.player_id().is_some());
    assert_eq!(
        a.player_id(),
        b.player_id(),
        "one device id → one guest player"
    );
    assert_eq!(a.owns("full"), Some(false), "nothing bought");

    a.write_save("progress", r#"{"a":1}"#);
    wait("a's write", || a.save_rev("progress") >= 1);
    wait("a's queue drained", || a.pending_writes() == 0);
    // b still thinks the slot is empty: its write conflicts and merges.
    b.write_save("progress", r#"{"b":2}"#);
    wait("b's merged write", || b.save_rev("progress") >= 2);
    let merged: Value = serde_json::from_str(&b.read_save("progress").unwrap()).unwrap();
    assert_eq!(merged, json!({"a":1,"b":2}));

    let c = Account::start(cfg);
    wait("third device", || c.ready());
    let seen: Value = serde_json::from_str(&c.read_save("progress").unwrap()).unwrap();
    assert_eq!(seen, json!({"a":1,"b":2}));
    let _ = std::fs::remove_dir_all(dir);
}

/// A guest links a provider (`fake` in dev and staging) and becomes a
/// signed-in player, keeping its id; `client()` then calls player operations
/// as that player. With `PARTII_UGC_GAME` (a game on the platform), it also
/// publishes and unpublishes a Level.
#[test]
fn a_guest_signs_in_and_calls_player_operations() {
    let (Ok(edge), Ok(key)) = (std::env::var("PARTII_API"), std::env::var("PARTII_KEY")) else {
        eprintln!("skip: set PARTII_API and PARTII_KEY");
        return;
    };
    let game = std::env::var("PARTII_UGC_GAME").ok();
    let dir = std::env::temp_dir().join(format!(
        "partii-account-login-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    let a = Account::start(AccountConfig {
        edge,
        key: Some(key),
        game: game.clone().unwrap_or("account-test".into()),
        data_dir: dir.clone(),
        ..AccountConfig::default()
    });
    wait("signed in", || a.ready());
    assert_eq!(a.is_guest(), Some(true));
    let guest_id = a.player_id().unwrap();
    let guest_token = a.player_token().unwrap();
    let me = a.client().unwrap().json("playersMe", &[], None).unwrap();
    assert_eq!(me["player"]["id"], json!(guest_id));

    a.login(
        "fake",
        json!({ "subject": format!("account-login-{}", guest_id) }),
    );
    let mut out = None;
    wait("login", || {
        out = a.take_login();
        out.is_some()
    });
    assert_eq!(out.unwrap(), Ok(()));
    assert_eq!(a.is_guest(), Some(false));
    assert_eq!(
        a.player_id().as_deref(),
        Some(guest_id.as_str()),
        "the guest became the account"
    );
    assert_ne!(a.player_token().unwrap(), guest_token);
    assert!(a.providers().contains(&"fake".to_string()));

    if let Some(game) = game {
        let c = a.client().unwrap();
        let data = format!(r#"{{"level":"account-test","at":{}}}"#, std::process::id());
        let sha = sha256_hex(data.as_bytes());
        let start = c
            .json(
                "ugcUploadStart",
                &[("game", &game)],
                Some(json!({ "size": data.len(), "sha256": sha })),
            )
            .unwrap();
        assert_eq!(start["ok"], true, "{start}");
        assert!(start["upload"]["url"].is_string() || start["exists"] == true);
        // (The bytes are PUT by the game's own HTTP stack; this test only checks the identity reaches player operations.)
        let mine = c.json("ugcMine", &[("game", &game)], None).unwrap();
        assert_eq!(mine["ok"], true, "{mine}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Steam: a ticket for a game without a Steam key says so (the cue for the
/// browser), and the browser sign-in hands out a steamcommunity.com URL.
/// (Finishing it needs a person at a browser.) Needs `PARTII_STEAM_GAME`: a
/// game on the platform with no Steam key.
#[test]
fn steam_sign_in_paths() {
    let (Ok(edge), Ok(key), Ok(game)) = (
        std::env::var("PARTII_API"),
        std::env::var("PARTII_KEY"),
        std::env::var("PARTII_STEAM_GAME"),
    ) else {
        eprintln!("skip: set PARTII_API, PARTII_KEY and PARTII_STEAM_GAME");
        return;
    };
    let dir = std::env::temp_dir().join(format!("partii-account-steam-{}", std::process::id()));
    let a = Account::start(AccountConfig {
        edge,
        key: Some(key),
        game,
        data_dir: dir.clone(),
        ..AccountConfig::default()
    });
    wait("signed in", || a.ready());
    a.login(
        "steam",
        json!({ "ticket": "abcdef0123456789", "identity": "partii" }),
    );
    let mut out = None;
    wait("ticket login answer", || {
        out = a.take_login();
        out.is_some()
    });
    let err = out.unwrap().unwrap_err();
    assert!(err.contains("isn't set up"), "{err}");
    a.steam_browser_login();
    let mut url = None;
    wait("browser url", || {
        url = a.take_browser_url();
        url.is_some()
    });
    assert!(url
        .unwrap()
        .starts_with("https://steamcommunity.com/openid/login?"));
    assert_eq!(a.is_guest(), Some(true));
    let _ = std::fs::remove_dir_all(dir);
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Events and error reports reach the dashboard: with `PARTII_ANALYTICS_GAME`
/// (a game of PARTII_KEY's project) and `PARTII_TOKEN` (its developer) to
/// read them back. A panic on another thread is reported by the hook.
#[test]
fn events_and_errors_reach_the_dashboard() {
    let (Ok(edge), Ok(key), Ok(game), Ok(dev)) = (
        std::env::var("PARTII_API"),
        std::env::var("PARTII_KEY"),
        std::env::var("PARTII_ANALYTICS_GAME"),
        std::env::var("PARTII_TOKEN"),
    ) else {
        eprintln!("skip: set PARTII_API, PARTII_KEY, PARTII_ANALYTICS_GAME and PARTII_TOKEN");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "partii-account-analytics-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    partii_account::install_panic_hook(&dir);
    let a = Account::start(AccountConfig {
        edge: edge.clone(),
        key: Some(key),
        game: game.clone(),
        data_dir: dir.clone(),
        version: "9.9.9-test".into(),
        build: "test".into(),
        ..AccountConfig::default()
    });
    a.set_owned(false);
    let mark = format!("m{}", std::process::id());
    a.track("live_test", json!({ "mark": mark, "n": 1 }));
    let _ = std::thread::spawn(|| panic!("live test panic on a worker thread (index 3)")).join();
    a.report_error(
        "warning",
        "live test warning",
        None,
        json!({ "screen": "test" }),
    );
    assert!(a.end_session(Duration::from_secs(20)), "everything sent");
    let d = partii_client::Client {
        base: edge,
        developer_token: Some(dev),
        ..Default::default()
    };
    let by = d
        .json(
            "analyticsEvents",
            &[
                ("game", &game),
                ("name", "live_test"),
                ("by", "mark"),
                ("days", "1"),
            ],
            None,
        )
        .unwrap();
    assert!(
        by["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["value"] == json!(mark)),
        "{by}"
    );
    let groups = d
        .json(
            "errorsList",
            &[
                ("game", &game),
                ("status", "all"),
                ("version", "9.9.9-test"),
            ],
            None,
        )
        .unwrap();
    let msgs: Vec<&str> = groups["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g["message"].as_str())
        .collect();
    assert!(msgs.contains(&"live test warning"), "{msgs:?}");
    assert!(
        msgs.iter().any(|m| m.starts_with("live test panic")),
        "{msgs:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
