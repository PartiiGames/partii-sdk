//! The browser driver: the same core as natively, with the browser's own
//! WebSocket and RTCPeerConnection doing the I/O. Rust games compiled to wasm
//! use `Engine`, which has the native `Engine`'s API, so one code path serves
//! both. JavaScript uses `WebEngine` (wasm-bindgen): `poll()` returns the
//! events since the last call as a JSON array. Browsers relay over TURN on
//! every transport, TLS/443 included.

use crate::core::{Action, Config, Core, Event, IceServer, TICK_MS};
use crate::link::PathId;
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    BinaryType, MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent,
    RtcDataChannelInit, RtcIceCandidateInit, RtcIceTransportPolicy, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit, WebSocket,
};

struct Pc {
    gen: u32,
    pc: RtcPeerConnection,
    dc: Option<RtcDataChannel>,
    remote_set: bool,
    pending: Vec<Value>,
}

struct Inner {
    core: Core,
    ws: Option<WebSocket>,
    ws_epoch: u32,
    pcs: HashMap<(u16, PathId), Pc>,
    events: Vec<Event>,
}

type Shared = Rc<RefCell<Inner>>;

fn now() -> f64 {
    js_sys::Date::now()
}

fn path_name(p: PathId) -> &'static str {
    p.name()
}

/// Run the core's actions (and any they lead to).
fn perform(sh: &Shared) {
    loop {
        let actions = sh.borrow_mut().core.drain();
        if actions.is_empty() {
            return;
        }
        for a in actions {
            match a {
                Action::Connect { url } => connect(sh, url),
                Action::SendText(t) => {
                    if let Some(ws) = &sh.borrow().ws {
                        let _ = ws.send_with_str(&t);
                    }
                }
                Action::SendBinary(b) => {
                    if let Some(ws) = &sh.borrow().ws {
                        let _ = ws.send_with_u8_array(&b);
                    }
                }
                Action::CreatePc {
                    mid,
                    path,
                    gen,
                    offerer,
                    ice,
                    relay_only,
                } => create_pc(sh, mid, path, gen, offerer, ice, relay_only),
                Action::PcSignal {
                    mid,
                    path,
                    gen,
                    signal,
                } => pc_signal(sh, mid, path, gen, signal),
                Action::ClosePc { mid, path } => {
                    if let Some(p) = sh.borrow_mut().pcs.remove(&(mid, path)) {
                        p.pc.close();
                    }
                }
                Action::PcSend { mid, path, bytes } => {
                    if let Some(dc) = sh
                        .borrow()
                        .pcs
                        .get(&(mid, path))
                        .and_then(|p| p.dc.as_ref())
                    {
                        if dc.ready_state() == web_sys::RtcDataChannelState::Open
                            && dc.buffered_amount() < 512 * 1024
                        {
                            let _ = dc.send_with_u8_array(&bytes);
                        }
                    }
                }
                Action::Emit(e) => sh.borrow_mut().events.push(e),
            }
        }
    }
}

fn connect(sh: &Shared, url: String) {
    let Ok(ws) = WebSocket::new(&url) else {
        sh.borrow_mut().core.on_socket_closed(now());
        return;
    };
    ws.set_binary_type(BinaryType::Arraybuffer);
    let epoch = {
        let mut i = sh.borrow_mut();
        i.ws_epoch += 1;
        if let Some(old) = i.ws.take() {
            let _ = old.close();
        }
        i.ws_epoch
    };
    let live = move |sh: &Shared| sh.borrow().ws_epoch == epoch;
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut()>::new(move || {
            if live(&s) {
                s.borrow_mut().core.on_socket_open(now());
                perform(&s);
            }
        });
        ws.set_onopen(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if !live(&s) {
                return;
            }
            let d = e.data();
            if let Some(t) = d.as_string() {
                s.borrow_mut().core.on_text(&t, now());
            } else if let Ok(buf) = d.dyn_into::<js_sys::ArrayBuffer>() {
                s.borrow_mut()
                    .core
                    .on_binary(&Uint8Array::new(&buf).to_vec(), now());
            }
            perform(&s);
        });
        ws.set_onmessage(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut()>::new(move || {
            if live(&s) {
                s.borrow_mut().ws = None;
                s.borrow_mut().core.on_socket_closed(now());
                perform(&s);
            }
        });
        ws.set_onclose(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    sh.borrow_mut().ws = Some(ws);
}

fn rtc_config(ice: &[IceServer], relay_only: bool) -> RtcConfiguration {
    let cfg = RtcConfiguration::new();
    let servers = Array::new();
    for s in ice {
        let o = Object::new();
        let urls = Array::new();
        for u in &s.urls {
            urls.push(&JsValue::from_str(u));
        }
        let _ = Reflect::set(&o, &"urls".into(), &urls);
        if let Some(u) = &s.username {
            let _ = Reflect::set(&o, &"username".into(), &JsValue::from_str(u));
        }
        if let Some(c) = &s.credential {
            let _ = Reflect::set(&o, &"credential".into(), &JsValue::from_str(c));
        }
        servers.push(&o);
    }
    cfg.set_ice_servers(&servers);
    if relay_only {
        cfg.set_ice_transport_policy(RtcIceTransportPolicy::Relay);
    }
    cfg
}

fn wire_channel(sh: &Shared, mid: u16, path: PathId, gen: u32, dc: &RtcDataChannel) {
    dc.set_binary_type(web_sys::RtcDataChannelType::Arraybuffer);
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut()>::new(move || {
            s.borrow_mut().core.on_pc_open(mid, path, gen, now());
            perform(&s);
        });
        dc.set_onopen(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                s.borrow_mut()
                    .core
                    .on_pc_data(mid, path, &Uint8Array::new(&buf).to_vec(), now());
                perform(&s);
            }
        });
        dc.set_onmessage(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut()>::new(move || {
            s.borrow_mut().core.on_pc_closed(mid, path, gen, now());
            perform(&s);
        });
        dc.set_onclose(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
}

fn create_pc(
    sh: &Shared,
    mid: u16,
    path: PathId,
    gen: u32,
    offerer: bool,
    ice: Vec<IceServer>,
    relay_only: bool,
) {
    let Ok(pc) = RtcPeerConnection::new_with_configuration(&rtc_config(&ice, relay_only)) else {
        sh.borrow_mut().core.on_pc_closed(mid, path, gen, now());
        return;
    };
    {
        let s = sh.clone();
        let f = Closure::<dyn FnMut(RtcPeerConnectionIceEvent)>::new(
            move |e: RtcPeerConnectionIceEvent| {
                if let Some(c) = e.candidate() {
                    let cand = json!({"candidate": c.candidate(), "sdpMid": c.sdp_mid(), "sdpMLineIndex": c.sdp_m_line_index()});
                    s.borrow_mut().core.on_pc_signal(
                        mid,
                        path,
                        gen,
                        json!({"type": "cand", "cand": cand}),
                    );
                    perform(&s);
                }
            },
        );
        pc.set_onicecandidate(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    {
        let s = sh.clone();
        let pcc = pc.clone();
        let f = Closure::<dyn FnMut()>::new(move || {
            use web_sys::RtcPeerConnectionState as St;
            if matches!(pcc.connection_state(), St::Failed | St::Closed) {
                s.borrow_mut().core.on_pc_closed(mid, path, gen, now());
                perform(&s);
            }
        });
        pc.set_onconnectionstatechange(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    let mut dc = None;
    if offerer {
        let init = RtcDataChannelInit::new();
        init.set_ordered(false);
        init.set_max_retransmits(0);
        let ch = pc.create_data_channel_with_data_channel_dict("d", &init);
        wire_channel(sh, mid, path, gen, &ch);
        dc = Some(ch);
    } else {
        let s = sh.clone();
        let f = Closure::<dyn FnMut(RtcDataChannelEvent)>::new(move |e: RtcDataChannelEvent| {
            let ch = e.channel();
            wire_channel(&s, mid, path, gen, &ch);
            if let Some(p) = s.borrow_mut().pcs.get_mut(&(mid, path)) {
                p.dc = Some(ch.clone());
            }
            if ch.ready_state() == web_sys::RtcDataChannelState::Open {
                s.borrow_mut().core.on_pc_open(mid, path, gen, now());
                perform(&s);
            }
        });
        pc.set_ondatachannel(Some(f.as_ref().unchecked_ref()));
        f.forget();
    }
    sh.borrow_mut().pcs.insert(
        (mid, path),
        Pc {
            gen,
            pc: pc.clone(),
            dc,
            remote_set: false,
            pending: Vec::new(),
        },
    );
    if offerer {
        let s = sh.clone();
        spawn_local(async move {
            let Ok(offer) = JsFuture::from(pc.create_offer()).await else {
                return;
            };
            let sdp = Reflect::get(&offer, &"sdp".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            let desc = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
            desc.set_sdp(&sdp);
            if JsFuture::from(pc.set_local_description(&desc))
                .await
                .is_err()
            {
                return;
            }
            s.borrow_mut()
                .core
                .on_pc_signal(mid, path, gen, json!({"type": "offer", "sdp": sdp}));
            perform(&s);
        });
    }
}

fn add_candidate(pc: &RtcPeerConnection, c: &Value) {
    let init = RtcIceCandidateInit::new(c["candidate"].as_str().unwrap_or(""));
    if let Some(m) = c["sdpMid"].as_str() {
        init.set_sdp_mid(Some(m));
    }
    if let Some(i) = c["sdpMLineIndex"].as_u64() {
        init.set_sdp_m_line_index(Some(i as u16));
    }
    let _ = pc.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&init));
}

fn pc_signal(sh: &Shared, mid: u16, path: PathId, gen: u32, signal: Value) {
    let Some(pc) = sh
        .borrow()
        .pcs
        .get(&(mid, path))
        .filter(|p| p.gen == gen)
        .map(|p| p.pc.clone())
    else {
        return;
    };
    match signal["type"].as_str() {
        Some("cand") => {
            let mut i = sh.borrow_mut();
            let Some(p) = i.pcs.get_mut(&(mid, path)) else {
                return;
            };
            if p.remote_set {
                add_candidate(&pc, &signal["cand"]);
            } else {
                p.pending.push(signal["cand"].clone());
            }
        }
        Some(kind @ ("offer" | "answer")) => {
            let s = sh.clone();
            let is_offer = kind == "offer";
            let sdp = signal["sdp"].as_str().unwrap_or("").to_string();
            spawn_local(async move {
                let desc = RtcSessionDescriptionInit::new(if is_offer {
                    RtcSdpType::Offer
                } else {
                    RtcSdpType::Answer
                });
                desc.set_sdp(&sdp);
                if JsFuture::from(pc.set_remote_description(&desc))
                    .await
                    .is_err()
                {
                    return;
                }
                let pending = {
                    let mut i = s.borrow_mut();
                    match i.pcs.get_mut(&(mid, path)) {
                        Some(p) => {
                            p.remote_set = true;
                            std::mem::take(&mut p.pending)
                        }
                        None => return,
                    }
                };
                for c in &pending {
                    add_candidate(&pc, c);
                }
                if is_offer {
                    let Ok(answer) = JsFuture::from(pc.create_answer()).await else {
                        return;
                    };
                    let sdp = Reflect::get(&answer, &"sdp".into())
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_default();
                    let d = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                    d.set_sdp(&sdp);
                    if JsFuture::from(pc.set_local_description(&d)).await.is_err() {
                        return;
                    }
                    s.borrow_mut().core.on_pc_signal(
                        mid,
                        path,
                        gen,
                        json!({"type": "answer", "sdp": sdp}),
                    );
                    perform(&s);
                }
            });
        }
        _ => {}
    }
}

/// The engine for Rust games built for the web: the native `Engine`'s API on
/// the browser's event loop (the browser runs it; nothing blocks).
pub struct Engine {
    sh: Shared,
}

impl Engine {
    /// Start connecting. Events (starting with `Online`) arrive via `poll`.
    pub fn start(mut cfg: Config) -> Self {
        if cfg.conn.is_empty() {
            cfg.conn = Self::new_conn_id();
        }
        let sh: Shared = Rc::new(RefCell::new(Inner {
            core: Core::new(cfg),
            ws: None,
            ws_epoch: 0,
            pcs: HashMap::new(),
            events: Vec::new(),
        }));
        let s = sh.clone();
        let tick = Closure::<dyn FnMut()>::new(move || {
            s.borrow_mut().core.tick(now());
            perform(&s);
        });
        if let Some(win) = web_sys::window() {
            let _ = win.set_interval_with_callback_and_timeout_and_arguments_0(
                tick.as_ref().unchecked_ref(),
                TICK_MS as i32,
            );
        }
        tick.forget();
        Engine { sh }
    }

    /// A fresh random connection id (one per page load).
    pub fn new_conn_id() -> String {
        let mut b = [0u8; 18];
        let _ = getrandom::fill(&mut b);
        b.iter()
            .map(|x| {
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
                    [(*x & 63) as usize] as char
            })
            .collect()
    }

    /// Every event since the last call.
    pub fn poll(&self) -> Vec<Event> {
        std::mem::take(&mut self.sh.borrow_mut().events)
    }

    fn act<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        let r = f(&mut self.sh.borrow_mut().core);
        perform(&self.sh);
        r
    }

    pub fn create_with(
        &self,
        game: &str,
        kind: &str,
        name: &str,
        meta: Value,
        settings: Value,
    ) -> u32 {
        self.act(|c| c.create_with(game, kind, name, meta, settings))
    }
    pub fn member_update(&self, name: Option<&str>, meta: Option<Value>) -> u32 {
        self.act(|c| c.member_update(name, meta))
    }
    pub fn create(
        &self,
        game: &str,
        max_members: Option<u32>,
        kind: &str,
        name: &str,
        meta: Value,
    ) -> u32 {
        self.act(|c| c.create(game, max_members, kind, name, meta))
    }
    pub fn join(&self, code: &str, kind: &str, name: &str, meta: Value) -> u32 {
        self.act(|c| c.join(code, kind, name, meta))
    }
    pub fn join_with(
        &self,
        code: &str,
        kind: &str,
        name: &str,
        meta: Value,
        settings: Value,
    ) -> u32 {
        self.act(|c| c.join_with(code, kind, name, meta, settings))
    }
    pub fn leave(&self) {
        self.act(|c| {
            c.leave();
        })
    }
    pub fn update(&self, patch: Value) {
        self.act(|c| {
            c.update(patch);
        })
    }
    pub fn kick(&self, mid: u16) {
        self.act(|c| {
            c.kick(mid);
        })
    }
    /// Send to a member (`crate::core::BROADCAST` for everyone).
    pub fn send(&self, to: u16, reliable: bool, bytes: Vec<u8>) {
        self.act(|c| c.send(to, reliable, &bytes, now()))
    }
    pub fn report(&self, metrics: Value) {
        self.act(|c| c.report(metrics))
    }
    pub fn online(&self) -> bool {
        self.sh.borrow().core.online()
    }
    /// Link state per member (path in use, RTT per path).
    pub fn stats(&self) -> Value {
        self.sh.borrow().core.stats()
    }
}

/// The engine for web pages (JavaScript). One per page.
#[wasm_bindgen]
pub struct WebEngine {
    e: Engine,
}

#[wasm_bindgen]
impl WebEngine {
    /// `config`: `{edge, key, conn?, client?, player_token?, no_webrtc?}` (JSON).
    #[wasm_bindgen(constructor)]
    pub fn new(config: &str) -> Result<WebEngine, JsValue> {
        let cfg: Config =
            serde_json::from_str(config).map_err(|e| JsValue::from_str(&format!("config: {e}")))?;
        web_sys::window().ok_or("no window")?;
        Ok(WebEngine {
            e: Engine::start(cfg),
        })
    }

    /// Events since the last call, as a JSON array.
    pub fn poll(&self) -> String {
        serde_json::to_string(&self.e.poll()).unwrap_or_else(|_| "[]".into())
    }

    pub fn create(&self, game: &str, max_members: u32, kind: &str, name: &str, meta: &str) -> u32 {
        self.e.create(
            game,
            Some(max_members),
            kind,
            name,
            serde_json::from_str(meta).unwrap_or(Value::Null),
        )
    }

    pub fn join(&self, code: &str, kind: &str, name: &str, meta: &str) -> u32 {
        self.e.join(
            code,
            kind,
            name,
            serde_json::from_str(meta).unwrap_or(Value::Null),
        )
    }

    /// `join` with extra fields as JSON, e.g. `{"net_version":"a1b2c3d4e5f6"}`.
    #[wasm_bindgen(js_name = joinWith)]
    pub fn join_with(&self, code: &str, kind: &str, name: &str, meta: &str, settings: &str) -> u32 {
        self.e.join_with(
            code,
            kind,
            name,
            serde_json::from_str(meta).unwrap_or(Value::Null),
            serde_json::from_str(settings).unwrap_or(Value::Null),
        )
    }

    /// `create` with room settings as JSON, e.g. `{"max_members":32,"kinds":{"game":{"joinable":false}}}`.
    #[wasm_bindgen(js_name = createWith)]
    pub fn create_with(
        &self,
        game: &str,
        kind: &str,
        name: &str,
        meta: &str,
        settings: &str,
    ) -> u32 {
        self.e.create_with(
            game,
            kind,
            name,
            serde_json::from_str(meta).unwrap_or(Value::Null),
            serde_json::from_str(settings).unwrap_or(Value::Null),
        )
    }

    /// Host only: change the room (`{"joinable":false}`, `{"kinds":{...}}`, `{"meta":...}`).
    pub fn update(&self, patch: &str) {
        self.e
            .update(serde_json::from_str(patch).unwrap_or(Value::Null))
    }

    /// Host only.
    pub fn kick(&self, mid: u16) {
        self.e.kick(mid)
    }

    /// Change your own name and/or meta (JSON `{"name":..,"meta":..}`); the others get `member_updated`.
    #[wasm_bindgen(js_name = memberUpdate)]
    pub fn member_update(&self, patch: &str) -> u32 {
        let v: Value = serde_json::from_str(patch).unwrap_or(Value::Null);
        self.e
            .member_update(v["name"].as_str(), v.get("meta").cloned())
    }

    pub fn leave(&self) {
        self.e.leave()
    }

    /// Send bytes to a member (0xFFFF = everyone).
    pub fn send(&self, to: u16, reliable: bool, bytes: &[u8]) {
        self.e.send(to, reliable, bytes.to_vec())
    }

    pub fn online(&self) -> bool {
        self.e.online()
    }

    /// Link state per member, as JSON.
    pub fn stats(&self) -> String {
        self.e.stats().to_string()
    }

    #[wasm_bindgen(js_name = pathName)]
    pub fn path_name_of(id: u8) -> String {
        path_name(match id {
            1 => PathId::Turn,
            2 => PathId::Direct,
            _ => PathId::Ws,
        })
        .into()
    }
}
