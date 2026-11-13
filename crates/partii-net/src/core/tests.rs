use super::*;

fn core(no_webrtc: bool) -> Core {
    Core::new(Config {
        edge: "https://edge.example".into(),
        key: "pk".into(),
        conn: "c".repeat(20),
        client: "t".into(),
        player_token: None,
        no_webrtc,
        away: false,
    })
}

fn online(c: &mut Core, relay: &str) {
    c.tick(0.0);
    c.on_socket_open(0.0);
    let ice = if relay == "turn" {
        json!([{"urls": ["stun:s:3478", "turn:t:3478", "turns:t:443"], "username": "u", "credential": "p"}])
    } else {
        json!([])
    };
    c.on_text(
        &json!({"t": "welcome", "resumed": false, "relay": relay, "ice_servers": ice}).to_string(),
        0.0,
    );
    c.drain();
}

fn room(you: u16, mids: &[u16]) -> Value {
    json!({
        "code": "ABCDE", "game": "g", "host": 1, "you": you, "joinable": true, "max_members": 8, "meta": null,
        "members": mids.iter().map(|m| json!({"mid": m, "kind": "game", "name": "", "meta": null, "status": "active"})).collect::<Vec<_>>()
    })
}

fn texts(actions: &[Action]) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| {
            if let Action::SendText(t) = a {
                serde_json::from_str(t).ok()
            } else {
                None
            }
        })
        .collect()
}
fn events(actions: &[Action]) -> Vec<Event> {
    actions
        .iter()
        .filter_map(|a| {
            if let Action::Emit(e) = a {
                Some(e.clone())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn derives_the_socket_url() {
    assert_eq!(
        core(true).url(),
        format!("wss://edge.example/v1/connect?conn={}", "c".repeat(20))
    );
    let mut c = core(true);
    c.cfg.edge = "http://127.0.0.1:8787/".into();
    assert!(c.url().starts_with("ws://127.0.0.1:8787/v1/connect?conn="));
}

#[test]
fn connects_then_says_hello_then_goes_online() {
    let mut c = core(true);
    c.tick(0.0);
    assert!(matches!(c.drain()[..], [Action::Connect { .. }]));
    c.on_socket_open(0.0);
    let hello = texts(&c.drain());
    assert_eq!(hello[0]["t"], "hello");
    assert_eq!(hello[0]["v"], 1);
    c.on_text(
        r#"{"t":"welcome","resumed":false,"relay":"ws","ice_servers":[]}"#,
        1.0,
    );
    assert_eq!(
        events(&c.drain()),
        vec![Event::Online {
            resumed: false,
            relay: "ws".into()
        }]
    );
    assert!(c.online());
}

#[test]
fn a_create_before_online_is_sent_once_the_socket_is_up() {
    let mut c = core(true);
    let id = c.create("g", Some(4), "game", "Ann", Value::Null);
    assert!(events(&c.drain()).is_empty(), "held, not failed");
    online_keep(&mut c);
    let sent = texts(&c.drain());
    let create = sent
        .iter()
        .find(|t| t["t"] == "create")
        .expect("create sent after welcome");
    assert_eq!(create["id"], id);
    assert_eq!(create["name"], "Ann");
}

#[test]
fn a_held_join_fails_offline_when_partii_stays_unreachable() {
    let mut c = core(true);
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.tick(0.0);
    c.drain();
    c.tick(OFFLINE_QUEUE_MS - 1.0);
    assert!(events(&c.drain())
        .iter()
        .all(|e| !matches!(e, Event::Failed { .. })));
    c.tick(OFFLINE_QUEUE_MS + 1.0);
    assert!(events(&c.drain())
        .iter()
        .any(|e| matches!(e, Event::Failed { req, code, .. } if *req == id && code == "offline")));
}

#[test]
fn other_requests_still_fail_fast_while_offline() {
    let mut c = core(true);
    let id = c.kick(2);
    assert!(
        matches!(&events(&c.drain())[0], Event::Failed { req, code, .. } if *req == id && code == "offline")
    );
}

/// Like `online`, but leaves the actions for the caller to inspect.
fn online_keep(c: &mut Core) {
    c.tick(0.0);
    c.on_socket_open(0.0);
    c.on_text(
        r#"{"t":"welcome","resumed":false,"relay":"ws","ice_servers":[]}"#,
        0.0,
    );
}

#[test]
fn create_then_member_joins_and_packets_flow_over_ws() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.create("g", Some(4), "game", "H", Value::Null);
    assert_eq!(texts(&c.drain())[0]["t"], "create");
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1])}).to_string(),
        5.0,
    );
    assert!(
        matches!(&events(&c.drain())[0], Event::Created { req, room } if *req == id && room.you == 1)
    );
    c.on_text(&json!({"t": "member_joined", "member": {"mid": 2, "kind": "game", "name": "G", "meta": null, "status": "active"}}).to_string(), 6.0);
    let ev = events(&c.drain());
    assert!(ev.contains(&Event::PeerConnected {
        mid: 2,
        path: PathId::Ws
    }));
    assert!(ev
        .iter()
        .any(|e| matches!(e, Event::MemberJoined { member } if member.mid == 2)));
    c.send(2, true, b"hi", 7.0);
    let frames: Vec<Vec<u8>> = c
        .drain()
        .into_iter()
        .filter_map(|a| {
            if let Action::SendBinary(b) = a {
                Some(b)
            } else {
                None
            }
        })
        .collect();
    assert!(frames
        .iter()
        .any(|f| f[0] == RELAY_FRAME && f[1..3] == [0, 2] && f.ends_with(b"hi")));
}

#[test]
fn incoming_relay_frames_become_messages() {
    let mut a = core(true);
    let mut b = core(true);
    for (c, you) in [(&mut a, 1u16), (&mut b, 2u16)] {
        online(c, "ws");
        let id = c.join("ABCDE", "game", "", Value::Null);
        c.drain();
        c.on_text(
            &json!({"t": "ok", "id": id, "room": room(you, &[1, 2])}).to_string(),
            0.0,
        );
        c.drain();
    }
    a.send(2, true, b"reliable", 1.0);
    a.send(2, false, b"fast", 1.0);
    for act in a.drain() {
        if let Action::SendBinary(f) = act {
            let mut rewritten = f.clone();
            rewritten[1..3].copy_from_slice(&1u16.to_be_bytes()); // the edge stamps the sender
            b.on_binary(&rewritten, 2.0);
        }
    }
    let got: Vec<(bool, Vec<u8>)> = events(&b.drain())
        .into_iter()
        .filter_map(|e| {
            if let Event::Message {
                reliable,
                bytes,
                from: 1,
            } = e
            {
                Some((reliable, bytes))
            } else {
                None
            }
        })
        .collect();
    assert!(got.contains(&(true, b"reliable".to_vec())));
    assert!(got.contains(&(false, b"fast".to_vec())));
}

#[test]
fn the_lower_member_offers_turn_and_direct() {
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1, 3])}).to_string(),
        0.0,
    );
    c.tick(1.0);
    let creates: Vec<(PathId, bool, bool, usize)> = c
        .drain()
        .into_iter()
        .filter_map(|a| {
            if let Action::CreatePc {
                path,
                offerer,
                relay_only,
                ice,
                ..
            } = a
            {
                Some((path, offerer, relay_only, ice.len()))
            } else {
                None
            }
        })
        .collect();
    assert!(creates.contains(&(PathId::Turn, true, true, 1)));
    // Direct gets STUN only.
    assert!(creates.contains(&(PathId::Direct, true, false, 1)));
}

#[test]
fn the_higher_member_waits_for_offers_and_answers_them() {
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        0.0,
    );
    c.tick(1.0);
    assert!(!c
        .drain()
        .iter()
        .any(|a| matches!(a, Action::CreatePc { .. })));
    c.on_text(&json!({"t": "signal", "from": 1, "data": {"pc": "turn", "gen": 1, "type": "offer", "sdp": "x"}}).to_string(), 2.0);
    let acts = c.drain();
    assert!(acts.iter().any(|a| matches!(
        a,
        Action::CreatePc {
            mid: 1,
            path: PathId::Turn,
            gen: 1,
            offerer: false,
            ..
        }
    )));
    assert!(acts.iter().any(|a| matches!(
        a,
        Action::PcSignal {
            mid: 1,
            path: PathId::Turn,
            gen: 1,
            ..
        }
    )));
    // A stale generation is ignored; a newer offer rebuilds.
    c.on_text(
        &json!({"t": "signal", "from": 1, "data": {"pc": "turn", "gen": 0, "type": "offer"}})
            .to_string(),
        3.0,
    );
    assert!(c.drain().is_empty());
    c.on_text(
        &json!({"t": "signal", "from": 1, "data": {"pc": "turn", "gen": 2, "type": "offer"}})
            .to_string(),
        4.0,
    );
    assert!(c.drain().iter().any(|a| matches!(
        a,
        Action::CreatePc {
            gen: 2,
            offerer: false,
            ..
        }
    )));
}

#[test]
fn a_failed_path_is_rebuilt_with_backoff_by_the_offerer() {
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1, 2])}).to_string(),
        0.0,
    );
    c.tick(1.0);
    c.drain();
    c.on_pc_open(2, PathId::Turn, 1, 100.0);
    c.on_pc_closed(2, PathId::Turn, 1, 200.0);
    c.tick(500.0);
    assert!(
        !c.drain().iter().any(|a| matches!(
            a,
            Action::CreatePc {
                path: PathId::Turn,
                ..
            }
        )),
        "waits out the backoff"
    );
    c.tick(1300.0);
    assert!(c.drain().iter().any(|a| matches!(
        a,
        Action::CreatePc {
            path: PathId::Turn,
            gen: 2,
            ..
        }
    )));
}

#[test]
fn a_dropped_socket_reconnects_and_resumes_the_room() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        0.0,
    );
    c.drain();
    let pending = c.update(json!({"joinable": false}));
    c.on_socket_closed(100.0);
    let ev = events(&c.drain());
    assert!(ev.contains(&Event::Offline));
    assert!(ev
        .iter()
        .any(|e| matches!(e, Event::Failed { req, .. } if *req == pending)));
    assert!(!c.link(1).unwrap().path(PathId::Ws).open);
    c.tick(100.0);
    assert!(
        matches!(c.drain()[..], [Action::Connect { .. }]),
        "first retry is immediate"
    );
    c.on_socket_closed(120.0);
    c.tick(200.0);
    assert!(c.drain().is_empty(), "second retry waits");
    c.tick(400.0);
    assert!(matches!(c.drain()[..], [Action::Connect { .. }]));
    c.on_socket_open(0.0);
    c.drain();
    c.on_text(&json!({"t": "welcome", "resumed": true, "relay": "ws", "ice_servers": [], "room": room(2, &[1, 2])}).to_string(), 450.0);
    assert!(events(&c.drain()).contains(&Event::Online {
        resumed: true,
        relay: "ws".into()
    }));
    assert_eq!(c.room().unwrap().you, 2);
    assert!(c.link(1).unwrap().path(PathId::Ws).open);
}

#[test]
fn a_fresh_session_after_a_long_drop_closes_the_room() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        0.0,
    );
    c.on_socket_closed(1.0);
    c.tick(2.0);
    c.on_socket_open(0.0);
    c.drain();
    c.on_text(
        r#"{"t":"welcome","resumed":false,"relay":"ws","ice_servers":[]}"#,
        3.0,
    );
    assert!(events(&c.drain()).contains(&Event::RoomClosed {
        reason: "expired".into()
    }));
    assert!(c.room().is_none());
}

#[test]
fn suspended_members_lose_the_ws_path_until_they_resume() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1, 2])}).to_string(),
        0.0,
    );
    c.on_text(r#"{"t":"member_suspended","mid":2}"#, 1.0);
    assert!(!c.link(2).unwrap().path(PathId::Ws).open);
    c.on_text(r#"{"t":"member_resumed","mid":2}"#, 2.0);
    assert!(c.link(2).unwrap().path(PathId::Ws).open);
    assert_eq!(c.room().unwrap().members[1].status, "active");
}

#[test]
fn a_silent_socket_is_pinged_then_replaced() {
    let mut c = core(true);
    online(&mut c, "ws");
    c.tick(1200.0);
    assert!(c.drain().contains(&Action::SendText("ping".into())));
    c.on_text("pong", 1300.0);
    c.tick(3000.0);
    assert!(c.online(), "a pong keeps it alive");
    c.tick(5000.0);
    let acts = c.drain();
    assert!(acts
        .iter()
        .any(|a| matches!(a, Action::Emit(Event::Offline))));
    assert!(
        acts.iter().any(|a| matches!(a, Action::Connect { .. })),
        "reconnects at once"
    );
}

#[test]
fn a_resume_restarts_unopened_paths_at_once() {
    // Offerer side: a failed path waiting out its backoff restarts on resume.
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1, 2])}).to_string(),
        0.0,
    );
    c.tick(1.0);
    c.drain();
    c.on_pc_closed(2, PathId::Turn, 1, 100.0);
    c.on_pc_closed(2, PathId::Turn, 1, 100.0);
    c.on_text(r#"{"t":"member_suspended","mid":2}"#, 200.0);
    c.on_text(r#"{"t":"member_resumed","mid":2}"#, 300.0);
    c.tick(310.0);
    assert!(
        c.drain().iter().any(|a| matches!(
            a,
            Action::CreatePc {
                path: PathId::Turn,
                ..
            }
        )),
        "no backoff wait after a resume"
    );

    // Answerer side: asks the offerer to restart.
    let mut g = core(false);
    online(&mut g, "turn");
    let id = g.join("ABCDE", "game", "", Value::Null);
    g.drain();
    g.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        0.0,
    );
    g.drain();
    g.on_text(r#"{"t":"member_resumed","mid":1}"#, 50.0);
    let asks: Vec<Value> = texts(&g.drain())
        .into_iter()
        .filter(|t| t["t"] == "signal" && t["data"]["type"] == "restart")
        .collect();
    assert_eq!(asks.len(), 2, "turn and direct");
}

#[test]
fn the_room_comes_before_its_peers() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        0.0,
    );
    let ev = events(&c.drain());
    let joined = ev
        .iter()
        .position(|e| matches!(e, Event::Joined { .. }))
        .unwrap();
    let peer = ev
        .iter()
        .position(|e| matches!(e, Event::PeerConnected { mid: 1, .. }))
        .unwrap();
    assert!(joined < peer, "{ev:?}");
    c.on_text(&json!({"t": "member_joined", "member": {"mid": 3, "kind": "game", "name": "", "meta": null, "status": "active"}}).to_string(), 1.0);
    let ev = events(&c.drain());
    let joined = ev
        .iter()
        .position(|e| matches!(e, Event::MemberJoined { .. }))
        .unwrap();
    let peer = ev
        .iter()
        .position(|e| matches!(e, Event::PeerConnected { mid: 3, .. }))
        .unwrap();
    assert!(joined < peer, "{ev:?}");
}

#[test]
fn queued_sends_go_out_as_one_bundled_relay_frame() {
    let mut a = core(true);
    online(&mut a, "ws");
    let id = a.join("ABCDE", "game", "", Value::Null);
    a.drain();
    a.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1, 2])}).to_string(),
        0.0,
    );
    a.drain();
    a.queue(2, false, b"one");
    a.queue(2, false, b"two");
    a.queue(2, true, b"three");
    a.flush(1.0);
    let frames: Vec<Vec<u8>> = a
        .drain()
        .into_iter()
        .filter_map(|x| {
            if let Action::SendBinary(f) = x {
                Some(f)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert_eq!(frames[0][0], RELAY_BUNDLE);
    // The receiver unpacks every datagram.
    let mut b = core(true);
    online(&mut b, "ws");
    let id = b.join("ABCDE", "game", "", Value::Null);
    b.drain();
    b.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        0.0,
    );
    b.drain();
    let mut f = frames[0].clone();
    f[1..3].copy_from_slice(&1u16.to_be_bytes());
    b.on_binary(&f, 2.0);
    let got: Vec<Vec<u8>> = events(&b.drain())
        .into_iter()
        .filter_map(|e| {
            if let Event::Message { bytes, .. } = e {
                Some(bytes)
            } else {
                None
            }
        })
        .collect();
    assert!(
        got.contains(&b"one".to_vec())
            && got.contains(&b"two".to_vec())
            && got.contains(&b"three".to_vec()),
        "{got:?}"
    );
}

#[test]
fn create_with_sends_room_settings_and_the_room_keeps_its_kinds() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.create_with(
        "g",
        "game",
        "Ann",
        Value::Null,
        json!({"max_members": 32, "kinds": {"game": {"joinable": false}}, "t": "nope"}),
    );
    let sent = texts(&c.drain());
    assert_eq!(
        sent[0]["t"], "create",
        "settings never override the message type"
    );
    assert_eq!(sent[0]["max_members"], 32);
    assert_eq!(sent[0]["kinds"], json!({"game": {"joinable": false}}));
    let mut r = room(1, &[1]);
    r["kinds"] = json!({"game": {"joinable": false}});
    c.on_text(&json!({"t": "ok", "id": id, "room": r}).to_string(), 1.0);
    assert_eq!(
        c.room().unwrap().kinds,
        Some(json!({"game": {"joinable": false}}))
    );
    // The old create still sends max_members.
    c.create("g", Some(4), "game", "", Value::Null);
    assert_eq!(texts(&c.drain())[0]["max_members"], 4);
}

#[test]
fn a_busy_refusal_says_how_long_to_wait_before_it_fails() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "phone", "", Value::Null);
    c.drain();
    c.on_text(&json!({"t": "err", "id": id, "code": "rate_limited", "message": "busy", "retry_after_ms": 12000}).to_string(), 1.0);
    let evs = events(&c.drain());
    assert_eq!(
        evs[0],
        Event::RateLimited {
            req: id,
            retry_after_ms: 12000
        }
    );
    assert!(
        matches!(&evs[1], Event::Failed { req, code, .. } if *req == id && code == "rate_limited")
    );
}

#[test]
fn members_rename_themselves() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(),
        1.0,
    );
    c.drain();
    let up = c.member_update(Some("Sam"), Some(json!({"color": "red"})));
    let sent = texts(&c.drain());
    assert_eq!(
        sent[0],
        json!({"t": "member_update", "id": up, "name": "Sam", "meta": {"color": "red"}})
    );
    c.on_text(&json!({"t": "ok", "id": up, "member": {"mid": 2, "kind": "game", "name": "Sam", "meta": {"color": "red"}, "status": "active"}}).to_string(), 2.0);
    assert_eq!(c.room().unwrap().members[1].name, "Sam");
    c.on_text(r#"{"t":"member_updated","member":{"mid":1,"kind":"game","name":"Host","meta":null,"status":"active"}}"#, 3.0);
    assert!(events(&c.drain())
        .iter()
        .any(|e| matches!(e, Event::MemberUpdated { member } if member.name == "Host")));
    assert_eq!(c.room().unwrap().members[0].name, "Host");
}

#[test]
fn the_host_keeps_its_reclaim_token_through_room_updates() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.create("g", None, "game", "", Value::Null);
    let mut r = room(1, &[1]);
    r["reclaim"] = json!("tok");
    c.on_text(&json!({"t": "ok", "id": id, "room": r}).to_string(), 1.0);
    assert_eq!(c.room().unwrap().reclaim.as_deref(), Some("tok"));
    c.on_text(
        &json!({"t": "room_updated", "room": room(1, &[1, 2])}).to_string(),
        2.0,
    );
    assert_eq!(c.room().unwrap().reclaim.as_deref(), Some("tok"));
    let up = c.update(json!({"joinable": false}));
    c.on_text(
        &json!({"t": "ok", "id": up, "room": room(1, &[1, 2])}).to_string(),
        3.0,
    );
    assert_eq!(c.room().unwrap().reclaim.as_deref(), Some("tok"));
}

#[test]
fn a_fresh_engine_on_a_resumed_session_learns_its_room_and_peers() {
    let mut c = core(true);
    c.tick(0.0);
    c.on_socket_open(0.0);
    c.drain();
    c.on_text(&json!({"t": "welcome", "resumed": true, "relay": "ws", "ice_servers": [], "room": room(3, &[1, 3])}).to_string(), 0.0);
    let evs = events(&c.drain());
    assert!(matches!(&evs[0], Event::Resumed { room } if room.you == 3));
    assert!(evs
        .iter()
        .any(|e| matches!(e, Event::Online { resumed: true, .. })));
    assert_eq!(c.me(), Some(3));
    assert!(c.link(1).is_some(), "a link to the host");
}

#[test]
fn hello_says_fresh_only_before_the_first_welcome() {
    let mut c = core(true);
    c.tick(0.0);
    c.on_socket_open(0.0);
    let h = texts(&c.drain());
    assert_eq!(h[0]["fresh"], true);
    assert_eq!(h[0]["caps"], json!(["reset"]), "the away grace is opt-in");
    c.on_text(
        r#"{"t":"welcome","resumed":false,"relay":"ws","ice_servers":[]}"#,
        1.0,
    );
    c.on_socket_closed(2.0);
    c.tick(10_000.0);
    c.on_socket_open(10_000.0);
    let again = texts(&c.drain());
    let h2 = again.iter().find(|t| t["t"] == "hello").unwrap();
    assert_eq!(h2["fresh"], false);
}

#[test]
fn a_member_back_from_a_new_engine_gets_a_fresh_link() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.create("g", None, "game", "", Value::Null);
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(1, &[1, 2])}).to_string(),
        1.0,
    );
    c.drain();
    // Reliable traffic moves the link's sequence on.
    for i in 0..5u8 {
        c.send(2, true, &[i], 2.0);
    }
    c.drain();
    assert!(c.link(2).unwrap().pending_reliable() > 0);
    c.on_text(r#"{"t":"member_suspended","mid":2}"#, 3.0);
    c.on_text(r#"{"t":"member_resumed","mid":2,"reset":true}"#, 4.0);
    assert_eq!(
        c.link(2).unwrap().pending_reliable(),
        0,
        "the old link and its unacked sends are gone"
    );
    assert!(events(&c.drain())
        .iter()
        .any(|e| matches!(e, Event::MemberResumed { mid: 2 })));
}

// ── Away grace (PROTOCOL.md "Away") ──

/// A room as an opted-in reader sees it, joined as `you`.
fn in_room(you: u16, mids: &[u16]) -> Core {
    let mut c = core(true);
    c.cfg.away = true;
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    let mut r = room(you, mids);
    r["away_grace_ms"] = json!(30_000);
    c.on_text(&json!({"t": "ok", "id": id, "room": r}).to_string(), 0.0);
    c.drain();
    c
}

#[test]
fn hello_opts_in_to_away_only_when_asked() {
    let mut c = core(true);
    c.cfg.away = true;
    c.tick(0.0);
    c.on_socket_open(0.0);
    assert_eq!(texts(&c.drain())[0]["caps"], json!(["reset", "away"]));
    // The web engine's JSON config may leave every optional field out; away is off then.
    let cfg: Config = serde_json::from_str(r#"{"edge":"https://e","key":"pk"}"#).unwrap();
    assert!(!cfg.away && !cfg.no_webrtc && cfg.conn.is_empty());
    let on: Config = serde_json::from_str(r#"{"edge":"https://e","key":"pk","away":true}"#).unwrap();
    assert!(on.away);
}

#[test]
fn away_and_back_without_opting_in_fail_and_send_nothing() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(&json!({"t": "ok", "id": id, "room": room(2, &[1, 2])}).to_string(), 0.0);
    c.drain();
    for back in [false, true] {
        let req = if back { c.back() } else { c.away() };
        let acts = c.drain();
        assert!(texts(&acts).is_empty(), "nothing goes to the edge");
        assert!(matches!(&events(&acts)[..], [Event::Failed { req: r, code, .. }] if *r == req && code == "not_enabled"));
    }
    assert!(c.away_hold().is_none() && c.room().is_some());
}

#[test]
fn the_rooms_away_grace_and_away_members_parse() {
    let c = in_room(1, &[1, 2]);
    assert_eq!(c.room().unwrap().away_grace_ms, Some(30_000));
}

#[test]
fn member_away_by_hint_then_back() {
    let mut c = in_room(1, &[1, 2]);
    c.on_text(
        r#"{"t":"member_away","mid":2,"reason":"hint","until":1761500030000,"grace_ms":30000}"#,
        1.0,
    );
    let evs = events(&c.drain());
    assert_eq!(
        evs,
        vec![Event::MemberAway { mid: 2, reason: "hint".into(), until: 1_761_500_030_000, grace_ms: 30_000 }]
    );
    let m = &c.room().unwrap().members[1];
    assert_eq!(m.status, "suspended");
    assert_eq!(m.away, Some(Away { reason: "hint".into(), until: 1_761_500_030_000 }));
    assert!(!c.link(2).unwrap().path(PathId::Ws).open, "the edge drops packets to it");
    // Sending to an away member is no error: reliable data waits for its return.
    c.send(2, true, b"hi", 2.0);
    let acts = c.drain();
    assert!(!events(&acts).iter().any(|e| matches!(e, Event::Error { .. } | Event::Failed { .. })));
    assert!(c.link(2).unwrap().pending_reliable() > 0);
    c.on_text(r#"{"t":"member_back","mid":2}"#, 3.0);
    assert_eq!(events(&c.drain()), vec![Event::MemberBack { mid: 2 }]);
    let m = &c.room().unwrap().members[1];
    assert_eq!((m.status.as_str(), m.away.as_ref()), ("active", None));
    assert!(c.link(2).unwrap().path(PathId::Ws).open, "its link is back");
}

#[test]
fn member_away_dropped_then_back_with_a_reset_restarts_the_link() {
    let mut c = in_room(1, &[1, 2]);
    for i in 0..5u8 {
        c.send(2, true, &[i], 1.0);
    }
    c.drain();
    c.on_text(r#"{"t":"member_away","mid":2,"reason":"dropped","until":5,"grace_ms":30000}"#, 2.0);
    assert!(matches!(
        &events(&c.drain())[..],
        [Event::MemberAway { mid: 2, reason, .. }] if reason == "dropped"
    ));
    assert!(c.link(2).unwrap().pending_reliable() > 0);
    c.on_text(r#"{"t":"member_back","mid":2,"reset":true}"#, 3.0);
    assert_eq!(c.link(2).unwrap().pending_reliable(), 0, "a fresh link");
    assert!(c.link(2).unwrap().path(PathId::Ws).open);
    assert!(events(&c.drain()).contains(&Event::MemberBack { mid: 2 }));
}

#[test]
fn member_away_then_left_on_timeout() {
    let mut c = in_room(1, &[1, 2]);
    c.on_text(r#"{"t":"member_away","mid":2,"reason":"hint","until":5,"grace_ms":5000}"#, 1.0);
    c.on_text(r#"{"t":"member_left","mid":2,"reason":"timeout"}"#, 2.0);
    let evs = events(&c.drain());
    assert_eq!(evs[1], Event::MemberLeft { mid: 2, reason: "timeout".into() });
    assert!(c.link(2).is_none());
    assert_eq!(c.room().unwrap().members.len(), 1);
}

#[test]
fn old_presence_frames_still_give_the_old_events() {
    // An edge from before the away grace (or a member we hear about the old way).
    let mut c = in_room(1, &[1, 2]);
    c.on_text(r#"{"t":"member_suspended","mid":2}"#, 1.0);
    c.on_text(r#"{"t":"member_resumed","mid":2}"#, 2.0);
    assert_eq!(
        events(&c.drain()),
        vec![Event::MemberSuspended { mid: 2 }, Event::MemberResumed { mid: 2 }]
    );
    // A member_away without grace_ms takes the room's grace.
    c.on_text(r#"{"t":"member_away","mid":2,"until":9}"#, 3.0);
    assert!(matches!(
        &events(&c.drain())[..],
        [Event::MemberAway { reason, grace_ms: 30_000, .. }] if reason == "dropped"
    ));
}

#[test]
fn away_and_back_requests_and_their_replies() {
    let mut c = in_room(2, &[1, 2]);
    let id = c.away();
    assert_eq!(texts(&c.drain()), vec![json!({"t": "away", "id": id})]);
    c.on_text(
        &json!({"t": "ok", "id": id, "reason": "hint", "until": 1_761_500_030_000u64, "grace_ms": 30_000}).to_string(),
        1.0,
    );
    assert_eq!(
        events(&c.drain()),
        vec![Event::AwayHeld { req: id, reason: "hint".into(), until: 1_761_500_030_000, grace_ms: 30_000 }]
    );
    assert_eq!(c.away_hold().map(|a| a.until), Some(1_761_500_030_000));
    let b = c.back();
    assert_eq!(texts(&c.drain()), vec![json!({"t": "back", "id": b})]);
    assert!(c.away_hold().is_none());
    c.on_text(&json!({"t": "ok", "id": b}).to_string(), 2.0);
    assert!(events(&c.drain()).is_empty(), "back's ok says nothing more");
    assert!(c.room().is_some());

    // Not in a room: the edge refuses.
    let mut d = core(true);
    d.cfg.away = true;
    online(&mut d, "ws");
    let id = d.away();
    d.drain();
    d.on_text(&json!({"t": "err", "id": id, "code": "not_in_room", "message": "not in a room"}).to_string(), 1.0);
    assert!(matches!(&events(&d.drain())[..], [Event::Failed { req, code, .. }] if *req == id && code == "not_in_room"));
    assert!(d.away_hold().is_none());
}

#[test]
fn away_offline_fails_harmlessly() {
    let mut c = core(true);
    c.cfg.away = true;
    let id = c.away();
    assert!(matches!(&events(&c.drain())[..], [Event::Failed { req, code, .. }] if *req == id && code == "offline"));
}

#[test]
fn away_by_hint_past_the_grace_closes_the_room_with_timeout() {
    let mut c = in_room(2, &[1, 2]);
    let id = c.away();
    c.on_text(&json!({"t": "ok", "id": id, "reason": "hint", "until": 5, "grace_ms": 5000}).to_string(), 1.0);
    c.drain();
    // The socket stayed up past `until`: the seat went.
    c.on_text(r#"{"t":"room_closed","reason":"timeout"}"#, 6000.0);
    assert_eq!(events(&c.drain()), vec![Event::RoomClosed { reason: "timeout".into() }]);
    assert!(c.room().is_none() && c.link(1).is_none() && c.away_hold().is_none());
}

#[test]
fn away_then_a_dropped_socket_resumes_into_the_same_seat() {
    let mut c = in_room(2, &[1, 2]);
    let id = c.away();
    c.on_text(&json!({"t": "ok", "id": id, "reason": "hint", "until": 99, "grace_ms": 30000}).to_string(), 1.0);
    c.on_socket_closed(2.0);
    c.tick(10.0);
    c.on_socket_open(10.0);
    c.drain();
    c.on_text(
        &json!({"t": "welcome", "resumed": true, "relay": "ws", "ice_servers": [], "room": room(2, &[1, 2])}).to_string(),
        11.0,
    );
    let evs = events(&c.drain());
    assert!(!evs.iter().any(|e| matches!(e, Event::RoomClosed { .. })));
    assert!(c.away_hold().is_none(), "the resume is back");
    assert_eq!(c.me(), Some(2));
    assert!(c.link(1).unwrap().path(PathId::Ws).open);
}

#[test]
fn events_serialize_with_snake_case_names() {
    let v = serde_json::to_value(Event::MemberAway { mid: 2, reason: "hint".into(), until: 1, grace_ms: 2 }).unwrap();
    assert_eq!(v, json!({"ev": "member_away", "mid": 2, "reason": "hint", "until": 1, "grace_ms": 2}));
    assert_eq!(serde_json::to_value(Event::MemberBack { mid: 2 }).unwrap(), json!({"ev": "member_back", "mid": 2}));
    assert_eq!(
        serde_json::to_value(Event::AwayHeld { req: 3, reason: "hint".into(), until: 1, grace_ms: 2 }).unwrap()["ev"],
        "away_held"
    );
}

// ── Topology (mesh / star) ──

/// A tiny edge: members 1..=n in one room (1 creates and hosts), signals
/// relayed between their cores, peer connections tracked from the actions.
struct Party {
    cores: BTreeMap<u16, Core>,
    /// Live peer connections: (owner, peer, path).
    pcs: BTreeSet<(u16, u16, PathId)>,
    /// Every pair that ever built a peer connection (owner, peer).
    ever: BTreeSet<(u16, u16)>,
    host: u16,
    now: f64,
}

fn member_json(mid: u16) -> Value {
    json!({"mid": mid, "kind": "game", "name": "", "meta": null, "status": "active"})
}

impl Party {
    fn new() -> Self {
        Party {
            cores: BTreeMap::new(),
            pcs: BTreeSet::new(),
            ever: BTreeSet::new(),
            host: 1,
            now: 0.0,
        }
    }
    fn mids(&self) -> Vec<u16> {
        self.cores.keys().copied().collect()
    }
    fn room_json(&self, you: u16) -> Value {
        let mut r = room(you, &self.mids());
        r["host"] = json!(self.host);
        r
    }
    /// The next member comes in, asking for `t` (`None`: plain create/join).
    fn add(&mut self, t: Option<Topology>) -> u16 {
        let mid = self.cores.len() as u16 + 1;
        let mut c = core(false);
        online(&mut c, "turn");
        let settings = t.map_or(Value::Null, |t| t.settings());
        let id = if mid == 1 {
            c.create_with("g", "game", "", Value::Null, settings)
        } else {
            c.join_with("ABCDE", "game", "", Value::Null, settings)
        };
        c.drain();
        self.cores.insert(mid, c);
        let r = self.room_json(mid);
        let now = self.now;
        self.cores
            .get_mut(&mid)
            .unwrap()
            .on_text(&json!({"t": "ok", "id": id, "room": r}).to_string(), now);
        let joined = json!({"t": "member_joined", "member": member_json(mid)}).to_string();
        for (other, c) in self.cores.iter_mut() {
            if *other != mid {
                c.on_text(&joined, now);
            }
        }
        self.pump();
        mid
    }
    /// Carry out every core's actions until nothing is left.
    fn pump(&mut self) {
        for _ in 0..50 {
            let mut work = false;
            let mut deliver: Vec<(u16, String)> = Vec::new();
            let mut offers: Vec<(u16, u16, PathId, u32)> = Vec::new();
            for (me, c) in self.cores.iter_mut() {
                for a in c.drain() {
                    work = true;
                    match a {
                        Action::SendText(t) => {
                            let Ok(v) = serde_json::from_str::<Value>(&t) else {
                                continue;
                            };
                            if v["t"] == "signal" {
                                let to = v["to"].as_u64().unwrap() as u16;
                                deliver.push((
                                    to,
                                    json!({"t": "signal", "from": me, "data": v["data"]})
                                        .to_string(),
                                ));
                            }
                        }
                        Action::CreatePc {
                            mid,
                            path,
                            gen,
                            offerer,
                            ..
                        } => {
                            self.pcs.insert((*me, mid, path));
                            self.ever.insert((*me, mid));
                            if offerer {
                                offers.push((*me, mid, path, gen));
                            }
                        }
                        Action::ClosePc { mid, path } => {
                            self.pcs.remove(&(*me, mid, path));
                        }
                        _ => {}
                    }
                }
            }
            // The offerer's driver makes an offer.
            for (me, mid, path, gen) in offers {
                self.cores.get_mut(&me).unwrap().on_pc_signal(
                    mid,
                    path,
                    gen,
                    json!({"type": "offer", "sdp": "x"}),
                );
            }
            let now = self.now;
            for (to, text) in deliver {
                if let Some(c) = self.cores.get_mut(&to) {
                    c.on_text(&text, now);
                }
            }
            if !work {
                return;
            }
        }
        panic!("the party never settled");
    }
    /// Ticks until `t` ms (past the topology wait), pumping.
    fn run_to(&mut self, t: f64) {
        while self.now < t {
            self.now += 100.0;
            let now = self.now;
            for c in self.cores.values_mut() {
                c.on_text("pong", now); // the edge keeps the sockets alive
                c.tick(now);
            }
            self.pump();
        }
    }
    /// Distinct member pairs with a live peer connection.
    fn pc_pairs(&self) -> BTreeSet<(u16, u16)> {
        self.pcs
            .iter()
            .map(|(a, b, _)| (*a.min(b), *a.max(b)))
            .collect()
    }
    fn links(&self, mid: u16) -> Vec<u16> {
        let c = &self.cores[&mid];
        self.mids().into_iter().filter(|m| c.linked(*m)).collect()
    }
    /// Pairs of non-hosts that ever built a peer connection.
    fn non_host_pcs_ever(&self) -> usize {
        self.ever
            .iter()
            .filter(|(a, b)| *a != self.host && *b != self.host)
            .count()
    }
}

/// Tankii's party: a host, six phones and a remote guest.
fn party(host: Option<Topology>, members: Option<Topology>) -> Party {
    let mut p = Party::new();
    p.add(host);
    for _ in 0..7 {
        p.add(members);
    }
    p.run_to(TOPOLOGY_WAIT_MS + 1_000.0);
    p
}

fn assert_star(p: &Party) {
    let n = p.cores.len();
    let host = p.host;
    let others: Vec<u16> = p.mids().into_iter().filter(|m| *m != host).collect();
    assert_eq!(p.links(host), others, "the host links to everyone");
    for m in &others {
        assert_eq!(p.links(*m), vec![host], "member {m} links to the host only");
        assert_eq!(p.cores[m].topology(), Topology::Star);
    }
    let pairs = p.pc_pairs();
    assert_eq!(pairs.len(), n - 1, "n - 1 links: {pairs:?}");
    assert!(pairs.iter().all(|(a, b)| *a == host || *b == host));
}

#[test]
fn a_mesh_links_every_pair() {
    let p = party(None, None);
    assert_eq!(p.pc_pairs().len(), 8 * 7 / 2);
    assert_eq!(p.cores[&5].topology(), Topology::Mesh);
    assert_eq!(p.links(5).len(), 7);
}

#[test]
fn a_star_has_n_minus_one_links_and_none_between_members() {
    let p = party(Some(Topology::Star), Some(Topology::Star));
    assert_star(&p);
    assert_eq!(p.non_host_pcs_ever(), 0);
    // Both paths (TURN and direct) per link, only to the host.
    assert_eq!(p.pcs.iter().filter(|(a, _, _)| *a == 4).count(), 2);
    assert_eq!(p.cores[&4].stats()["topology"], "star");
}

#[test]
fn members_follow_the_hosts_star_whatever_they_asked_for() {
    // The phones' controller page joins with plain `join` (mesh): the host's
    // announcement makes them a star before any member-to-member link starts.
    let p = party(Some(Topology::Star), None);
    assert_star(&p);
    assert_eq!(
        p.non_host_pcs_ever(),
        0,
        "no phone ever built a link to another phone"
    );
}

#[test]
fn members_asking_for_a_star_follow_a_mesh_host() {
    let p = party(None, Some(Topology::Star));
    assert_eq!(p.pc_pairs().len(), 8 * 7 / 2);
    assert!(p.cores.values().all(|c| c.topology() == Topology::Mesh));
}

#[test]
fn a_star_rebuilds_around_a_new_host() {
    let mut p = party(Some(Topology::Star), None);
    // Partii's edge closes a room whose host leaves (`host_left` /
    // `host_lost`); should a room ever name a new host, the star follows it.
    p.cores.remove(&1);
    p.host = 2;
    p.pcs.retain(|(a, b, _)| *a != 1 && *b != 1);
    let left = json!({"t": "member_left", "mid": 1, "reason": "left"}).to_string();
    let now = p.now;
    for c in p.cores.values_mut() {
        c.on_text(&left, now);
    }
    for mid in p.mids() {
        let r = p.room_json(mid);
        p.cores
            .get_mut(&mid)
            .unwrap()
            .on_text(&json!({"t": "room_updated", "room": r}).to_string(), now);
    }
    p.pump();
    p.run_to(now + 1_000.0);
    assert_star(&p);
}

#[test]
fn the_host_can_switch_a_running_room_to_a_star() {
    let mut p = party(None, None);
    assert_eq!(p.pc_pairs().len(), 28);
    p.cores
        .get_mut(&1)
        .unwrap()
        .update(Topology::Star.settings());
    p.pump();
    assert_star(&p);
}

#[test]
fn a_topology_on_the_rooms_view_wins_at_once() {
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    let mut r = room(2, &[1, 2, 3]);
    r["topology"] = json!("star");
    c.on_text(&json!({"t": "ok", "id": id, "room": r}).to_string(), 0.0);
    c.tick(10.0);
    assert!(c.linked(1) && !c.linked(3));
    assert!(!c
        .drain()
        .iter()
        .any(|a| matches!(a, Action::CreatePc { mid: 3, .. })));
}

#[test]
fn without_an_announcement_a_member_keeps_its_own_choice_after_the_wait() {
    // An old host announces nothing. A mesh member holds its member-to-member
    // offers for TOPOLOGY_WAIT_MS, then builds them; the host link starts at once.
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join("ABCDE", "game", "", Value::Null);
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2, 3])}).to_string(),
        0.0,
    );
    c.tick(10.0);
    let acts = c.drain();
    assert!(!acts
        .iter()
        .any(|a| matches!(a, Action::CreatePc { mid: 3, .. })));
    c.tick(TOPOLOGY_WAIT_MS + 10.0);
    assert!(c.drain().iter().any(|a| matches!(
        a,
        Action::CreatePc {
            mid: 3,
            offerer: true,
            ..
        }
    )));
    assert_eq!(c.topology(), Topology::Mesh);

    // A star member stays a star.
    let mut s = core(false);
    online(&mut s, "turn");
    let id = s.join_with("ABCDE", "game", "", Value::Null, Topology::Star.settings());
    s.drain();
    s.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2, 3])}).to_string(),
        0.0,
    );
    s.tick(TOPOLOGY_WAIT_MS + 10.0);
    assert!(s.linked(1) && !s.linked(3));
    assert_eq!(s.topology(), Topology::Star);
}

#[test]
fn a_star_member_ignores_offers_from_other_members() {
    let mut c = core(false);
    online(&mut c, "turn");
    let id = c.join_with("ABCDE", "game", "", Value::Null, Topology::Star.settings());
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(3, &[1, 2, 3])}).to_string(),
        0.0,
    );
    c.drain();
    c.on_text(
        &json!({"t": "signal", "from": 2, "data": {"pc": "turn", "gen": 1, "type": "offer"}})
            .to_string(),
        1.0,
    );
    assert!(!c.linked(2));
    assert!(!c
        .drain()
        .iter()
        .any(|a| matches!(a, Action::CreatePc { .. } | Action::PcSignal { .. })));
}

#[test]
fn in_a_star_members_send_to_the_host_and_are_told_when_they_cant() {
    let mut c = core(true);
    online(&mut c, "ws");
    let id = c.join_with("ABCDE", "game", "", Value::Null, Topology::Star.settings());
    c.drain();
    c.on_text(
        &json!({"t": "ok", "id": id, "room": room(2, &[1, 2, 3, 4])}).to_string(),
        0.0,
    );
    c.drain();
    let frames_to = |acts: &[Action]| -> Vec<u16> {
        acts.iter()
            .filter_map(|a| match a {
                Action::SendBinary(b) if b[0] == RELAY_FRAME || b[0] == RELAY_BUNDLE => {
                    Some(u16::from_be_bytes([b[1], b[2]]))
                }
                _ => None,
            })
            .collect()
    };
    // Broadcast reaches the host only.
    c.send(BROADCAST, false, b"all", 5.0);
    let acts = c.drain();
    let to: BTreeSet<u16> = frames_to(&acts).into_iter().collect();
    assert_eq!(to, BTreeSet::from([1]));
    // A member we have no link to: nothing sent, one clear error.
    c.send(3, true, b"hi", 6.0);
    let acts = c.drain();
    assert!(!frames_to(&acts).contains(&3));
    let errs: Vec<Event> = events(&acts)
        .into_iter()
        .filter(|e| matches!(e, Event::Error { .. }))
        .collect();
    assert_eq!(errs.len(), 1);
    assert!(
        matches!(&errs[0], Event::Error { message } if message.contains("star") && message.contains("member 1"))
    );
    c.send(3, true, b"again", 7.0);
    assert!(events(&c.drain()).is_empty(), "said once per member");
    // The host is fine.
    c.send(1, true, b"to host", 8.0);
    assert!(frames_to(&c.drain()).contains(&1));
}

// ── auth: signing in on a live connection ──

fn hello_token(c: &mut Core, now: f64) -> Value {
    c.on_socket_closed(now);
    c.tick(now + 10_000.0);
    c.on_socket_open(now + 10_000.0);
    let t = texts(&c.drain());
    let hello = t.iter().find(|v| v["t"] == "hello").expect("a hello");
    hello.get("player_token").cloned().unwrap_or(Value::Null)
}

#[test]
fn auth_sends_its_frame_with_a_request_id() {
    let mut c = in_room(2, &[1, 2]);
    let id = c.auth(Some("pt_new".into()));
    assert_eq!(texts(&c.drain()), vec![json!({"t": "auth", "id": id, "player_token": "pt_new"})]);
    let out = c.auth(None);
    assert_ne!(out, id);
    let sent = texts(&c.drain());
    assert_eq!(sent, vec![json!({"t": "auth", "id": out, "player_token": null})]);
    assert!(sent[0].get("player_token").is_some(), "sign-out sends an explicit null");
}

#[test]
fn auth_ok_keeps_the_token_for_every_later_hello() {
    let mut c = in_room(2, &[1, 2]);
    assert_eq!(c.player_token(), None);
    let id = c.auth(Some("pt_new".into()));
    c.drain();
    c.on_text(&json!({"t": "ok", "id": id, "signed_in": true, "player_id": "p_1", "changed": true}).to_string(), 1.0);
    assert_eq!(
        events(&c.drain()),
        vec![Event::Authed { req: id, signed_in: true, player_id: Some("p_1".into()), changed: true }]
    );
    assert_eq!(c.player_token(), Some("pt_new"));
    assert!(c.room().is_some(), "same room");
    // A dropped socket: the resume's hello carries the new token.
    assert_eq!(hello_token(&mut c, 2.0), json!("pt_new"));

    // A renewed token: changed false.
    c.on_text(
        &json!({"t": "welcome", "resumed": true, "relay": "ws", "ice_servers": [], "room": room(2, &[1, 2])}).to_string(),
        10_002.0,
    );
    c.drain();
    assert!(c.online() && c.room().is_some());
    let id = c.auth(Some("pt_renewed".into()));
    c.drain();
    c.on_text(&json!({"t": "ok", "id": id, "signed_in": true, "player_id": "p_1", "changed": false}).to_string(), 1.0);
    assert!(matches!(&events(&c.drain())[..], [Event::Authed { changed: false, .. }]));
    assert_eq!(c.player_token(), Some("pt_renewed"));

    // Sign out: later hellos send no token.
    let id = c.auth(None);
    c.drain();
    c.on_text(&json!({"t": "ok", "id": id, "signed_in": false, "changed": true}).to_string(), 1.0);
    assert_eq!(
        events(&c.drain()),
        vec![Event::Authed { req: id, signed_in: false, player_id: None, changed: true }]
    );
    assert_eq!(c.player_token(), None);
    assert_eq!(hello_token(&mut c, 3.0), Value::Null);
}

#[test]
fn auth_refusals_parse_and_keep_the_old_token() {
    for (code, want, retry) in [
        ("bad_token", AuthErrorCode::BadToken, None),
        ("player_mismatch", AuthErrorCode::PlayerMismatch, None),
        ("rate_limited", AuthErrorCode::RateLimited, Some(12_000u64)),
        ("something_new", AuthErrorCode::Other("something_new".into()), None),
    ] {
        let mut c = core(true);
        c.cfg.player_token = Some("pt_old".into());
        online(&mut c, "ws");
        let id = c.auth(Some("pt_other".into()));
        c.drain();
        let mut err = json!({"t": "err", "id": id, "code": code, "message": "no"});
        if let Some(ms) = retry {
            err["retry_after_ms"] = json!(ms);
        }
        c.on_text(&err.to_string(), 1.0);
        assert_eq!(
            events(&c.drain()),
            vec![Event::AuthFailed { req: id, code: want, message: "no".into(), retry_after_ms: retry }],
            "{code}: only AuthFailed (no Failed / RateLimited)"
        );
        assert_eq!(c.player_token(), Some("pt_old"), "{code}");
        assert_eq!(hello_token(&mut c, 2.0), json!("pt_old"), "{code}");
    }
}

#[test]
fn auth_offline_or_dropped_fails_offline_and_keeps_the_old_token() {
    let mut c = core(true);
    c.cfg.player_token = Some("pt_old".into());
    let id = c.auth(Some("pt_new".into()));
    let a = c.drain();
    assert!(texts(&a).is_empty(), "nothing sent while offline");
    assert!(matches!(&events(&a)[..], [Event::AuthFailed { req, code: AuthErrorCode::Offline, .. }] if *req == id));

    online(&mut c, "ws");
    let id = c.auth(Some("pt_new".into()));
    c.drain();
    c.on_socket_closed(1.0);
    assert!(events(&c.drain())
        .iter()
        .any(|e| matches!(e, Event::AuthFailed { req, code: AuthErrorCode::Offline, .. } if *req == id)));
    // A late ok for the dead request (it can't come on a new socket, but be safe) changes nothing.
    c.on_text(&json!({"t": "ok", "id": id, "signed_in": true, "player_id": "p_1", "changed": true}).to_string(), 2.0);
    assert_eq!(c.player_token(), Some("pt_old"));
    assert_eq!(hello_token(&mut c, 3.0), json!("pt_old"));
}

#[test]
fn auth_events_serialize_for_the_web() {
    assert_eq!(
        serde_json::to_value(Event::Authed { req: 7, signed_in: true, player_id: Some("p_1".into()), changed: true }).unwrap(),
        json!({"ev": "authed", "req": 7, "signed_in": true, "player_id": "p_1", "changed": true})
    );
    assert_eq!(
        serde_json::to_value(Event::AuthFailed {
            req: 7,
            code: AuthErrorCode::RateLimited,
            message: "busy".into(),
            retry_after_ms: Some(500)
        })
        .unwrap(),
        json!({"ev": "auth_failed", "req": 7, "code": "rate_limited", "message": "busy", "retry_after_ms": 500})
    );
}
