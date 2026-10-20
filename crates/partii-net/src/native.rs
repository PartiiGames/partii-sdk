//! The native driver: carries out the core's actions with tokio (session
//! socket via tungstenite, peer connections via webrtc-rs) and feeds back
//! what happened. Games use the non-blocking `Engine` handle: commands go
//! in, events come out of `poll()` (Bevy-friendly; nothing blocks).
//!
//! webrtc-rs 0.21 relays over UDP TURN only. When UDP is blocked, a native
//! client plays over the ws relay (the last rung, always on 443).

use crate::core::{Action, Config, Core, Event, IceServer, TICK_MS};
use crate::link::PathId;
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceServer, RTCIceTransportPolicy, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription, SettingEngineBuilder,
};

enum Cmd {
    Create {
        game: String,
        kind: String,
        name: String,
        meta: Value,
        settings: Value,
        reply: async_channel::Sender<u32>,
    },
    Join {
        code: String,
        kind: String,
        name: String,
        meta: Value,
        settings: Value,
        reply: async_channel::Sender<u32>,
    },
    Leave,
    Update(Value),
    Kick(u16),
    MemberUpdate {
        name: Option<String>,
        meta: Option<Value>,
        reply: async_channel::Sender<u32>,
    },
    Send {
        to: u16,
        reliable: bool,
        bytes: Vec<u8>,
    },
    Report(Value),
    Stats(async_channel::Sender<Value>),
    Shutdown,
}

enum Io {
    SockOpen(u64),
    SockText(u64, String),
    SockBinary(u64, Vec<u8>),
    SockClosed(u64),
    PcSignal {
        mid: u16,
        path: PathId,
        gen: u32,
        signal: Value,
    },
    PcOpen {
        mid: u16,
        path: PathId,
        gen: u32,
    },
    PcClosed {
        mid: u16,
        path: PathId,
        gen: u32,
    },
    PcData {
        mid: u16,
        path: PathId,
        bytes: Vec<u8>,
    },
}

/// A live connection to partii. Cheap to poll every frame.
pub struct Engine {
    cmd: async_channel::Sender<Cmd>,
    events: async_channel::Receiver<Event>,
    /// Closed when the actor has finished (after a goodbye on shutdown).
    done: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    _rt: Arc<tokio::runtime::Runtime>,
}

impl Engine {
    /// Start connecting. Events (starting with `Online`) arrive via `poll`.
    pub fn start(cfg: Config) -> Self {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("partii")
                .enable_all()
                .build()
                .expect("partii: tokio runtime"),
        );
        let (cmd_tx, cmd_rx) = async_channel::unbounded();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            actor(cfg, cmd_rx, ev_tx).await;
            drop(done_tx);
        });
        Self {
            cmd: cmd_tx,
            events: ev_rx,
            done: std::sync::Mutex::new(done_rx),
            _rt: rt,
        }
    }

    /// A fresh random connection id (one per launch).
    pub fn new_conn_id() -> String {
        let mut b = [0u8; 18];
        getrandom::fill(&mut b).expect("random");
        b.iter()
            .map(|x| {
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
                    [(*x & 63) as usize] as char
            })
            .collect()
    }

    /// Every event since the last call.
    pub fn poll(&self) -> Vec<Event> {
        let mut v = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            v.push(e);
        }
        v
    }

    /// Wait for the next event (for tools and tests, not game loops).
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        let rx = self.events.clone();
        self._rt
            .block_on(async move { tokio::time::timeout(timeout, rx.recv()).await.ok()?.ok() })
    }

    pub fn create(
        &self,
        game: &str,
        max_members: Option<u32>,
        kind: &str,
        name: &str,
        meta: Value,
    ) -> u32 {
        self.create_with(
            game,
            kind,
            name,
            meta,
            serde_json::json!({"max_members": max_members}),
        )
    }
    /// `create` with room settings from the start, e.g. `{"max_members": 32, "kinds": {"game": {"joinable": false}}}`.
    pub fn create_with(
        &self,
        game: &str,
        kind: &str,
        name: &str,
        meta: Value,
        settings: Value,
    ) -> u32 {
        let (tx, rx) = async_channel::bounded(1);
        let _ = self.cmd.try_send(Cmd::Create {
            game: game.into(),
            kind: kind.into(),
            name: name.into(),
            meta,
            settings,
            reply: tx,
        });
        rx.recv_blocking().unwrap_or(0)
    }
    /// Change your own member name and/or meta; the others get `Event::MemberUpdated`.
    pub fn member_update(&self, name: Option<&str>, meta: Option<Value>) -> u32 {
        let (tx, rx) = async_channel::bounded(1);
        let _ = self.cmd.try_send(Cmd::MemberUpdate {
            name: name.map(Into::into),
            meta,
            reply: tx,
        });
        rx.recv_blocking().unwrap_or(0)
    }
    pub fn join(&self, code: &str, kind: &str, name: &str, meta: Value) -> u32 {
        self.join_with(code, kind, name, meta, Value::Null)
    }
    /// `join` with extra fields, e.g. `{"net_version": "a1b2c3d4e5f6"}`.
    pub fn join_with(
        &self,
        code: &str,
        kind: &str,
        name: &str,
        meta: Value,
        settings: Value,
    ) -> u32 {
        let (tx, rx) = async_channel::bounded(1);
        let _ = self.cmd.try_send(Cmd::Join {
            code: code.into(),
            kind: kind.into(),
            name: name.into(),
            meta,
            settings,
            reply: tx,
        });
        rx.recv_blocking().unwrap_or(0)
    }
    pub fn leave(&self) {
        let _ = self.cmd.try_send(Cmd::Leave);
    }
    pub fn update(&self, patch: Value) {
        let _ = self.cmd.try_send(Cmd::Update(patch));
    }
    pub fn kick(&self, mid: u16) {
        let _ = self.cmd.try_send(Cmd::Kick(mid));
    }
    /// Send to a member (`crate::core::BROADCAST` for everyone).
    pub fn send(&self, to: u16, reliable: bool, bytes: Vec<u8>) {
        let _ = self.cmd.try_send(Cmd::Send {
            to,
            reliable,
            bytes,
        });
    }
    pub fn report(&self, metrics: Value) {
        let _ = self.cmd.try_send(Cmd::Report(metrics));
    }
    /// Link state per member (path in use, RTT per path).
    pub fn stats(&self) -> Value {
        let (tx, rx) = async_channel::bounded(1);
        let _ = self.cmd.try_send(Cmd::Stats(tx));
        rx.recv_blocking().unwrap_or(Value::Null)
    }
}

impl Drop for Engine {
    /// Leaves the room (so others see it at once, not after the seat grace)
    /// and gives the actor a moment to say so before the runtime stops.
    fn drop(&mut self) {
        let _ = self.cmd.try_send(Cmd::Shutdown);
        if let Ok(done) = self.done.lock() {
            let _ = done.recv_timeout(Duration::from_millis(400));
        }
    }
}

fn trace() -> bool {
    std::env::var_os("PARTII_TRACE").is_some()
}

fn t_ms() -> u128 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis()
}

struct PcHandle {
    gen: u32,
    signals: async_channel::Sender<Value>,
    data: async_channel::Sender<Vec<u8>>,
}

async fn actor(
    cfg: Config,
    cmds: async_channel::Receiver<Cmd>,
    events: async_channel::Sender<Event>,
) {
    let start = Instant::now();
    let now = || start.elapsed().as_secs_f64() * 1000.0;
    let mut core = Core::new(cfg);
    let (io_tx, io_rx) = async_channel::unbounded::<Io>();
    let mut sock_out: Option<async_channel::Sender<Message>> = None;
    let mut sock_epoch = 0u64;
    let mut pcs: HashMap<(u16, PathId), PcHandle> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS as u64));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut quitting = false;
    loop {
        // Biased: commands, then inbound data, then the timer. After a
        // freeze (a backgrounded app) the socket holds frames that must be
        // read before the keepalive check decides the socket is dead.
        tokio::select! {
            biased;
            c = cmds.recv() => match c {
                Ok(Cmd::Create { game, kind, name, meta, settings, reply }) => { let _ = reply.try_send(core.create_with(&game, &kind, &name, meta, settings)); }
                Ok(Cmd::Join { code, kind, name, meta, settings, reply }) => { let _ = reply.try_send(core.join_with(&code, &kind, &name, meta, settings)); }
                Ok(Cmd::Leave) => { core.leave(); }
                Ok(Cmd::Update(p)) => { core.update(p); }
                Ok(Cmd::Kick(m)) => { core.kick(m); }
                Ok(Cmd::MemberUpdate { name, meta, reply }) => { let _ = reply.try_send(core.member_update(name.as_deref(), meta)); }
                Ok(Cmd::Send { to, reliable, bytes }) => {
                    // Take every send already waiting, then flush once:
                    // bundled frames cost the relay one request, not one each.
                    core.queue(to, reliable, &bytes);
                    let mut more = 0;
                    while more < 256 {
                        match cmds.try_recv() {
                            Ok(Cmd::Send { to, reliable, bytes }) => core.queue(to, reliable, &bytes),
                            Ok(other) => {
                                core.flush(now());
                                if !apply_other(&mut core, other) {
                                    return;
                                }
                            }
                            Err(_) => break,
                        }
                        more += 1;
                    }
                    core.flush(now());
                }
                Ok(Cmd::Report(m)) => core.report(m),
                Ok(Cmd::Stats(reply)) => { let _ = reply.try_send(core.stats()); }
                Ok(Cmd::Shutdown) | Err(_) => {
                    if core.room().is_none() || sock_out.is_none() {
                        break;
                    }
                    core.leave();
                    quitting = true;
                }
            },
            io = io_rx.recv() => {
                let Ok(io) = io else { break };
                let t = now();
                if trace() {
                    match &io {
                        Io::PcOpen { mid, path, gen } => eprintln!("[partii {}ms] pc open {mid}/{} g{gen}", t_ms(), path.name()),
                        Io::PcClosed { mid, path, gen } => eprintln!("[partii {}ms] pc closed {mid}/{} g{gen}", t_ms(), path.name()),
                        Io::PcSignal { mid, path, gen, signal } => eprintln!("[partii {}ms] pc signal out {mid}/{} g{gen} {}", t_ms(), path.name(), signal["type"]),
                        _ => {}
                    }
                }
                match io {
                    Io::SockOpen(e) if e == sock_epoch => core.on_socket_open(t),
                    Io::SockText(e, s) if e == sock_epoch => core.on_text(&s, t),
                    Io::SockBinary(e, b) if e == sock_epoch => core.on_binary(&b, t),
                    Io::SockClosed(e) if e == sock_epoch => { sock_out = None; core.on_socket_closed(t); }
                    Io::PcSignal { mid, path, gen, signal } => core.on_pc_signal(mid, path, gen, signal),
                    Io::PcOpen { mid, path, gen } => core.on_pc_open(mid, path, gen, t),
                    Io::PcClosed { mid, path, gen } => core.on_pc_closed(mid, path, gen, t),
                    Io::PcData { mid, path, bytes } => core.on_pc_data(mid, path, &bytes, t),
                    _ => {}
                }
            }
            _ = tick.tick() => core.tick(now()),
        }
        for a in core.drain() {
            match a {
                Action::Connect { url } => {
                    sock_epoch += 1;
                    let (tx, rx) = async_channel::unbounded();
                    sock_out = Some(tx);
                    tokio::spawn(socket(url, sock_epoch, rx, io_tx.clone()));
                }
                Action::SendText(t) => {
                    if let Some(s) = &sock_out {
                        let _ = s.try_send(Message::text(t));
                    }
                }
                Action::SendBinary(b) => {
                    if let Some(s) = &sock_out {
                        let _ = s.try_send(Message::binary(b));
                    }
                }
                Action::CreatePc {
                    mid,
                    path,
                    gen,
                    offerer,
                    ice,
                    relay_only,
                } => {
                    if trace() {
                        eprintln!("[partii {}ms] create pc {mid}/{} g{gen} offerer={offerer} relay_only={relay_only} ice={}", t_ms(), path.name(), ice.len());
                    }
                    let (sig_tx, sig_rx) = async_channel::unbounded();
                    let (data_tx, data_rx) = async_channel::unbounded();
                    pcs.insert(
                        (mid, path),
                        PcHandle {
                            gen,
                            signals: sig_tx,
                            data: data_tx,
                        },
                    );
                    tokio::spawn(peer(
                        mid,
                        path,
                        gen,
                        offerer,
                        ice,
                        relay_only,
                        sig_rx,
                        data_rx,
                        io_tx.clone(),
                    ));
                }
                Action::PcSignal {
                    mid,
                    path,
                    gen,
                    signal,
                } => {
                    if trace() {
                        eprintln!(
                            "[partii {}ms] pc signal in {mid}/{} g{gen} {} known={:?}",
                            t_ms(),
                            path.name(),
                            signal["type"],
                            pcs.get(&(mid, path)).map(|h| h.gen)
                        );
                    }
                    if let Some(h) = pcs.get(&(mid, path)).filter(|h| h.gen == gen) {
                        let _ = h.signals.try_send(signal);
                    }
                }
                Action::ClosePc { mid, path } => {
                    pcs.remove(&(mid, path)); // dropping the channels ends the task
                }
                Action::PcSend { mid, path, bytes } => {
                    if let Some(h) = pcs.get(&(mid, path)) {
                        let _ = h.data.try_send(bytes);
                    }
                }
                Action::Emit(e) => {
                    let _ = events.try_send(e);
                }
            }
        }
        if quitting {
            // Let the socket writer send the goodbye.
            tokio::time::sleep(Duration::from_millis(150)).await;
            break;
        }
    }
}

/// A non-send command met while draining sends; false = shut down.
fn apply_other(core: &mut Core, c: Cmd) -> bool {
    match c {
        Cmd::Create {
            game,
            kind,
            name,
            meta,
            settings,
            reply,
        } => {
            let _ = reply.try_send(core.create_with(&game, &kind, &name, meta, settings));
        }
        Cmd::Join {
            code,
            kind,
            name,
            meta,
            settings,
            reply,
        } => {
            let _ = reply.try_send(core.join_with(&code, &kind, &name, meta, settings));
        }
        Cmd::Leave => {
            core.leave();
        }
        Cmd::Update(p) => {
            core.update(p);
        }
        Cmd::Kick(m) => {
            core.kick(m);
        }
        Cmd::MemberUpdate { name, meta, reply } => {
            let _ = reply.try_send(core.member_update(name.as_deref(), meta));
        }
        Cmd::Report(m) => core.report(m),
        Cmd::Stats(reply) => {
            let _ = reply.try_send(core.stats());
        }
        Cmd::Send { .. } => {}
        Cmd::Shutdown => return false,
    }
    true
}

async fn socket(
    url: String,
    epoch: u64,
    out: async_channel::Receiver<Message>,
    io: async_channel::Sender<Io>,
) {
    let conn = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(&url),
    )
    .await;
    let Ok(Ok((ws, _))) = conn else {
        let _ = io.send(Io::SockClosed(epoch)).await;
        return;
    };
    let (mut sink, mut stream) = ws.split();
    let _ = io.send(Io::SockOpen(epoch)).await;
    let writer = async {
        while let Ok(m) = out.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    };
    let reader = async {
        while let Some(Ok(m)) = stream.next().await {
            let sent = match m {
                Message::Text(t) => io.send(Io::SockText(epoch, t.to_string())).await,
                Message::Binary(b) => io.send(Io::SockBinary(epoch, b.to_vec())).await,
                Message::Close(_) => break,
                _ => Ok(()),
            };
            if sent.is_err() {
                break;
            }
        }
    };
    tokio::select! { _ = writer => {}, _ = reader => {} }
    let _ = io.send(Io::SockClosed(epoch)).await;
}

enum PcEv {
    Ice(RTCIceCandidateInit),
    State(RTCPeerConnectionState),
    Channel(Arc<dyn DataChannel>),
}

struct Handler(async_channel::Sender<PcEv>);

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if let Ok(c) = event.candidate.to_json() {
            let _ = self.0.try_send(PcEv::Ice(c));
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.0.try_send(PcEv::State(state));
    }
    async fn on_data_channel(&self, dc: Arc<dyn DataChannel>) {
        let _ = self.0.try_send(PcEv::Channel(dc));
    }
}

#[allow(clippy::too_many_arguments)]
async fn peer(
    mid: u16,
    path: PathId,
    gen: u32,
    offerer: bool,
    ice: Vec<IceServer>,
    relay_only: bool,
    signals: async_channel::Receiver<Value>,
    data: async_channel::Receiver<Vec<u8>>,
    io: async_channel::Sender<Io>,
) {
    let closed = |io: &async_channel::Sender<Io>| {
        let _ = io.try_send(Io::PcClosed { mid, path, gen });
    };
    let servers: Vec<RTCIceServer> = ice
        .iter()
        .map(|s| RTCIceServer {
            urls: s.urls.clone(),
            username: s.username.clone().unwrap_or_default(),
            credential: s.credential.clone().unwrap_or_default(),
        })
        .collect();
    let policy = if relay_only {
        RTCIceTransportPolicy::Relay
    } else {
        RTCIceTransportPolicy::All
    };
    let config = RTCConfigurationBuilder::new()
        .with_ice_servers(servers)
        .with_ice_transport_policy(policy)
        .build();
    let (ev_tx, ev_rx) = async_channel::unbounded();
    // Each connection has one job (relay-only, or never-relay), so there is
    // nothing better to wait for: accept the first working pair at once.
    // Health is the link's call (pings), so ICE's own timers stay short.
    let settings = SettingEngineBuilder::default()
        .with_host_acceptance_min_wait(Some(Duration::ZERO))
        .with_srflx_acceptance_min_wait(Some(Duration::ZERO))
        .with_prflx_acceptance_min_wait(Some(Duration::ZERO))
        .with_relay_acceptance_min_wait(Some(Duration::ZERO))
        .with_ice_timeouts(
            Some(Duration::from_secs(3)),
            Some(Duration::from_secs(8)),
            Some(Duration::from_secs(1)),
        )
        // Check pairs every 50 ms (not 200) so a lost check costs little.
        .with_ice_connection_attempts(Some(Duration::from_millis(50)), Some(24))
        .build();
    let pc = match PeerConnectionBuilder::new()
        .with_configuration(config)
        .with_setting_engine(settings)
        .with_handler(Arc::new(Handler(ev_tx)))
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .with_data_channel_send_buffer_limit(256 * 1024)
        .build()
        .await
    {
        Ok(pc) => pc,
        Err(e) => {
            log::warn!("partii {mid}/{}: peer connection: {e}", path.name());
            return closed(&io);
        }
    };
    let signal = |v: Value| {
        let _ = io.try_send(Io::PcSignal {
            mid,
            path,
            gen,
            signal: v,
        });
    };
    let (dc_tx, dc_rx) = async_channel::unbounded::<Option<DataChannelEvent>>();
    let pump = |dc: Arc<dyn DataChannel>, tx: async_channel::Sender<Option<DataChannelEvent>>| {
        tokio::spawn(async move {
            while let Some(ev) = dc.poll().await {
                if tx.send(Some(ev)).await.is_err() {
                    return;
                }
            }
            let _ = tx.send(None).await;
        });
    };
    let mut channel: Option<Arc<dyn DataChannel>> = None;
    if offerer {
        let init = RTCDataChannelInit {
            ordered: false,
            max_retransmits: Some(0),
            ..Default::default()
        };
        match pc.create_data_channel("d", Some(init)).await {
            Ok(dc) => {
                pump(dc.clone(), dc_tx.clone());
                channel = Some(dc);
            }
            Err(e) => {
                log::warn!("partii {mid}/{}: data channel: {e}", path.name());
                return closed(&io);
            }
        }
        match pc.create_offer(None).await {
            Ok(offer) => {
                let sdp = offer.sdp.clone();
                if pc.set_local_description(offer).await.is_err() {
                    return closed(&io);
                }
                signal(json!({"type": "offer", "sdp": sdp}));
            }
            Err(_) => return closed(&io),
        }
    }
    let mut have_remote = false;
    let mut pending: Vec<RTCIceCandidateInit> = Vec::new();
    let mut open = false;
    let mut unhealthy_since: Option<Instant> = None;
    let mut check = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            s = signals.recv() => {
                let Ok(s) = s else { break };
                match s["type"].as_str() {
                    Some("offer") if !offerer => {
                        let Some(sdp) = s["sdp"].as_str() else { continue };
                        let Ok(desc) = RTCSessionDescription::offer(sdp.to_string()) else { continue };
                        if pc.set_remote_description(desc).await.is_err() { break; }
                        have_remote = true;
                        for c in pending.drain(..) { let _ = pc.add_ice_candidate(c).await; }
                        let Ok(answer) = pc.create_answer(None).await else { break };
                        let sdp = answer.sdp.clone();
                        if pc.set_local_description(answer).await.is_err() { break; }
                        signal(json!({"type": "answer", "sdp": sdp}));
                    }
                    Some("answer") if offerer => {
                        let Some(sdp) = s["sdp"].as_str() else { continue };
                        let Ok(desc) = RTCSessionDescription::answer(sdp.to_string()) else { continue };
                        if pc.set_remote_description(desc).await.is_err() { break; }
                        have_remote = true;
                        for c in pending.drain(..) { let _ = pc.add_ice_candidate(c).await; }
                    }
                    Some("cand") => {
                        let Ok(c) = serde_json::from_value::<RTCIceCandidateInit>(s["cand"].clone()) else { continue };
                        if c.candidate.is_empty() { continue; }
                        if have_remote { let _ = pc.add_ice_candidate(c).await; } else { pending.push(c); }
                    }
                    _ => {}
                }
            }
            d = data.recv() => {
                let Ok(d) = d else { break };
                if let Some(dc) = &channel {
                    let _ = dc.try_send(BytesMut::from(&d[..])).await;
                }
            }
            e = ev_rx.recv() => match e {
                Ok(PcEv::Ice(c)) => signal(json!({"type": "cand", "cand": {
                    "candidate": c.candidate, "sdpMid": c.sdp_mid, "sdpMLineIndex": c.sdp_mline_index, "usernameFragment": c.username_fragment
                }})),
                Ok(PcEv::State(st)) => match st {
                    RTCPeerConnectionState::Connected => unhealthy_since = None,
                    RTCPeerConnectionState::Disconnected => { unhealthy_since.get_or_insert_with(Instant::now); }
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => break,
                    _ => {}
                },
                Ok(PcEv::Channel(dc)) => {
                    // A channel the other side made arrives already open.
                    pump(dc.clone(), dc_tx.clone());
                    channel = Some(dc);
                    if !open {
                        open = true;
                        let _ = io.try_send(Io::PcOpen { mid, path, gen });
                    }
                }
                Err(_) => break,
            },
            ev = dc_rx.recv() => match ev {
                Ok(Some(DataChannelEvent::OnOpen)) => {
                    if !open {
                        open = true;
                        let _ = io.try_send(Io::PcOpen { mid, path, gen });
                    }
                }
                Ok(Some(DataChannelEvent::OnMessage(m))) => {
                    let _ = io.try_send(Io::PcData { mid, path, bytes: m.data.to_vec() });
                }
                Ok(Some(DataChannelEvent::OnClose)) | Ok(None) => if open { break },
                _ => {}
            },
            _ = check.tick() => {
                // A path that stays "disconnected" is dead; the link has
                // already moved off it (it stopped answering pings).
                if unhealthy_since.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) { break; }
            }
        }
    }
    let _ = pc.close().await;
    closed(&io);
}
