//! Against a real platform without email (`PARTII_API`, e.g. `partii dev up`
//! or staging, where a developer's sign-in code comes back in the reply;
//! skipped otherwise): a developer pushes two versions and the mirror
//! follows, fetching only what changed.

use partii_client::Client;
use partii_content::{local, sync, ContentConfig};
use serde_json::json;

fn b64(s: &str) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                A[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

#[test]
fn mirror_follows_pushed_versions() {
    let Ok(edge) = std::env::var("PARTII_API") else {
        eprintln!("skip: set PARTII_API");
        return;
    };
    let n = std::process::id();
    let a = Client {
        base: edge.clone(),
        ..Default::default()
    };
    let email = format!("content{n}@example.com");
    let code = a
        .json("developersLoginStart", &[], Some(json!({ "email": email })))
        .unwrap()["code"]
        .as_str()
        .expect("a platform without email returns the code")
        .to_string();
    let d = a
        .json(
            "developersLogin",
            &[],
            Some(json!({ "email": email, "code": code, "name": "Content Test" })),
        )
        .unwrap();
    let dev = Client {
        base: edge.clone(),
        developer_token: d["token"].as_str().map(str::to_string),
        ..Default::default()
    };
    let project = format!("content{n}");
    let game = format!("content-{n}");
    assert_eq!(
        dev.json(
            "projectsPut",
            &[("project", &project)],
            Some(json!({ "name": "Content" }))
        )
        .unwrap()["ok"],
        true
    );
    let key = dev
        .json(
            "keysCreate",
            &[("project", &project)],
            Some(json!({ "key_id": "game" })),
        )
        .unwrap()["key"]["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        dev.json(
            "gamesPut",
            &[("game", &game)],
            Some(json!({ "project": project, "title": "Content", "price_cents": 0 }))
        )
        .unwrap()["ok"],
        true
    );
    let push = |version: &str, files: serde_json::Value| {
        let r = dev
            .json(
                "contentPush",
                &[("game", &game)],
                Some(json!({ "version": version, "files": files })),
            )
            .unwrap();
        assert_eq!(r["ok"], true, "{r}");
    };
    let dir = std::env::temp_dir().join(format!("partii-content-live-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = ContentConfig {
        edge,
        key,
        game: game.clone(),
        channel: "stable".into(),
        dir: dir.clone(),
    };

    // Nothing pushed yet: an empty mirror, no error.
    let r = sync(&cfg);
    assert_eq!((r.version, r.error), (None, None));

    push(
        "1",
        json!({ "levels/1.json": b64("{\"walls\":1}"), "brains/hunt.py": b64("def tick(): pass\n") }),
    );
    let r = sync(&cfg);
    assert_eq!(r.error, None);
    assert_eq!(r.version.as_deref(), Some("1"));
    assert_eq!(r.changed, vec!["brains/hunt.py", "levels/1.json"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("levels/1.json")).unwrap(),
        "{\"walls\":1}"
    );

    push(
        "2",
        json!({ "levels/1.json": b64("{\"walls\":2}"), "levels/2.json": b64("{}") }),
    );
    let r = sync(&cfg);
    assert_eq!(r.version.as_deref(), Some("2"));
    assert_eq!(r.changed, vec!["levels/1.json", "levels/2.json"]);
    assert_eq!(r.removed, vec!["brains/hunt.py"]);
    assert!(!dir.join("brains/hunt.py").exists());
    assert_eq!(local(&dir).unwrap().version, "2");

    let r = sync(&cfg);
    assert!(
        r.changed.is_empty() && r.removed.is_empty() && r.error.is_none(),
        "{r:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
