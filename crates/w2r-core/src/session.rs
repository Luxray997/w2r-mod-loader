//! The host's main-thread state: the plugin registry, lobby presence, the handshake and the
//! user's `state.json` and `host.json`. Created on the first frame the game draws after
//! startup and driven by every `BeginNamedWindow` call from then on. Only the game's main
//! thread touches the session, which is why a `thread_local` holds it; messages that arrive
//! elsewhere wait in a queue for the next frame.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use w2r_abi::{
    W2R_EVT_LOBBY_ENTER, W2R_EVT_LOBBY_LEAVE, W2R_EVT_MATCH_END, W2R_EVT_MATCH_START,
    W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

use crate::chat::{self, Chat, NoChat};
use crate::dispatch::Registry;
use crate::entry;
use crate::frame;
use crate::guard::Fault;
use crate::handshake::{self, Handshake, Out};
use crate::host::{Game, Log, Services, State as SlotState};
use crate::hostcfg::{self, HostConfig};
use crate::lobby::{self, Event, Lobby};
use crate::lobbylayout::{self, ModMark};
use crate::lobbypolicy::{self, Locks};
use crate::manifest::Build;
#[cfg(target_arch = "x86")]
use crate::options;
use crate::peer::{self, NoTransport, PluginMessage, Presence, Roster, Transport};
use crate::plugins;
use crate::state::{self, State};
use crate::symbols::Symbols;
use crate::target::GAME_VERSION;
use crate::ui::{self, Action, HostRow, PluginRow, Status, Widgets};

struct Session {
    registry: Registry,
    lobby: Rc<RefCell<Lobby>>,
    /// As loaded at startup, plus the user's toggles (which take effect at the next launch).
    state: State,
    root: PathBuf,
    config: HostConfig,
    fingerprint: [u8; 4],
    /// The plugins in the fingerprint.
    simulation: Vec<String>,
    /// Plugin id to author, read once from the manifests for the plugin screen.
    authors: BTreeMap<String, String>,
    /// How many plugins `state.json` had enabled at startup, for the title screen's summary.
    enabled_at_start: usize,
    net: Rc<Net>,
    #[cfg(target_arch = "x86")]
    nk: Option<options::Nk>,
}

/// One line for the title screen: how many plugins are running and how many are not.
pub fn plugin_summary() -> Option<String> {
    SESSION.with(|cell| {
        let slot = cell.try_borrow().ok()?;
        let s = slot.as_ref()?;
        let disabled = (0..s.registry.len())
            .filter(|&i| matches!(s.registry.slot(i).state(), SlotState::Disabled { .. }))
            .count();
        let active = s.registry.len() - disabled;
        let failed = disabled + s.enabled_at_start.saturating_sub(s.registry.len());
        let noun = |n: usize| if n == 1 { "plugin" } else { "plugins" };
        Some(if failed == 0 {
            format!("{active} {} loaded", noun(active))
        } else {
            format!("{active} {} loaded, {failed} failed", noun(active))
        })
    })
}

/// The handshake and the game's net session. Separate from [`Session`] because the packet
/// hook may hand a message in while the session is borrowed (the game can poll packets from
/// inside a plugin callback); the handshake must still see it.
struct Net {
    handshake: RefCell<Handshake>,
    transport: Rc<dyn Transport>,
}

impl Net {
    fn idle() -> Net {
        Net {
            handshake: RefCell::new(Handshake::new(handshake::Local {
                strict: false,
                fingerprint: [0; 4],
            })),
            transport: Rc::new(NoTransport),
        }
    }

    /// Carries out what the handshake asked for. No handshake borrow is live here: the
    /// transport calls run game code under the guard.
    fn run(&self, outs: Vec<Out>) {
        for out in outs {
            let failed = match out {
                Out::Broadcast(m) => (!self.transport.broadcast(&m)).then_some("broadcast"),
                Out::SendTo(slot, m) => (!self.transport.send_to(slot, &m)).then_some("send"),
                Out::Kick(slot) => (!self.transport.kick(slot)).then_some("kick"),
                Out::Log(level, msg) => {
                    StartupLog.line(level, "handshake", &msg);
                    None
                }
            };
            if let Some(what) = failed {
                StartupLog.line(W2R_LOG_WARN, "handshake", &format!("{what} failed"));
            }
        }
    }
}

/// The roster `peer_send` and peer delivery consult (docs/abi.md).
struct HandshakeRoster(Rc<Net>);

impl Roster for HandshakeRoster {
    fn announced(&self, slot: u32) -> bool {
        self.0
            .handshake
            .try_borrow()
            .is_ok_and(|h| h.announced(slot))
    }
    fn any_announced(&self) -> bool {
        self.0
            .handshake
            .try_borrow()
            .is_ok_and(|h| h.any_announced())
    }
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// Set while `start` runs, so a nested frame does not start a second session.
    static STARTING: Cell<bool> = const { Cell::new(false) };
    /// The session's [`Net`], reachable without the session borrow.
    static NET: RefCell<Option<Rc<Net>>> = const { RefCell::new(None) };
}

/// A fault in host code under one of the host's own hooks (not a plugin's). Logged for the
/// first few and then sparsely: a hook runs every frame, and the same bug would repeat.
pub fn host_fault(site: &str, fault: &Fault) {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    if n <= 5 || n % 1000 == 0 {
        StartupLog.line(
            W2R_LOG_ERROR,
            "host",
            &format!("host fault in {site}: {fault} ({n} so far)"),
        );
    }
}

/// Routes to the startup log; the registry needs an `Rc<dyn Log>`.
pub(crate) struct StartupLog;

impl Log for StartupLog {
    fn line(&self, level: u32, plugin: &str, msg: &str) {
        if let Some(s) = entry::startup() {
            s.log.line(level, plugin, msg);
        }
    }
}

/// Lobby presence for the services, from the shared detector.
struct LobbyPresence(Rc<RefCell<Lobby>>);

impl Presence for LobbyPresence {
    fn in_lobby(&self) -> bool {
        self.0.borrow().in_lobby(chat::now_ms())
    }
}

fn on_main_thread() -> bool {
    let current = unsafe { GetCurrentThreadId() };
    current == frame::main_thread_id()
}

/// A nonce for one stay in a lobby: unique enough to tell our own broadcast from a peer's and
/// a rejoin from the last stay. Not a secret (docs/peer-protocol.md).
fn nonce() -> u32 {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut h = RandomState::new().build_hasher();
    h.write_u64(chat::now_ms());
    h.write_u32(std::process::id());
    let n = h.finish() as u32;
    if n == 0 { 1 } else { n }
}

/// The game is starting a match (`ResetMapMessageSlots`): tell the plugins that subscribed.
pub fn on_match_start() {
    if !on_main_thread() {
        return;
    }
    #[cfg(target_arch = "x86")]
    crate::matchend::rearm();
    #[cfg(target_arch = "x86")]
    crate::replayrec::rearm();
    #[cfg(target_arch = "x86")]
    crate::alliancesscreen::on_match_start();
    SESSION.with(|cell| {
        // Busy means a plugin callback is on the stack and drew something; the event is lost
        // rather than delivered into a half-finished callback.
        let Ok(slot) = cell.try_borrow() else {
            return;
        };
        if let Some(session) = slot.as_ref() {
            session
                .registry
                .dispatch(W2R_EVT_MATCH_START, core::ptr::null());
        }
    });
}

/// The local player's match is over (`CheckMatchEnd` wrote their result): tell the plugins that
/// subscribed.
pub fn on_match_end() {
    if !on_main_thread() {
        return;
    }
    SESSION.with(|cell| {
        let Ok(slot) = cell.try_borrow() else {
            return;
        };
        if let Some(session) = slot.as_ref() {
            session
                .registry
                .dispatch(W2R_EVT_MATCH_END, core::ptr::null());
        }
    });
}

/// One `BeginNamedWindow` call on the main thread. `name` is the window the game is starting,
/// if it gave one.
pub fn on_window(name: Option<&str>) {
    #[cfg(target_arch = "x86")]
    {
        crate::matchend::note_frame();
        crate::replayplay::poll();
        crate::ingamekick::poll();
    }
    let now = chat::now_ms();
    ensure_started();
    // Queued messages first, at the top level only: a nested call (a plugin callback made the
    // game draw a window, which lands back here) sees the session borrowed and must not touch
    // it.
    if SESSION.with(|c| c.try_borrow().is_ok()) {
        drain_queue();
    }
    SESSION.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        let Some(session) = slot.as_mut() else {
            return;
        };
        let event = {
            let mut lobby = session.lobby.borrow_mut();
            match name {
                Some(n) => lobby.on_window(n, now),
                None => lobby.on_frame(now),
            }
        };
        match event {
            Some(Event::Enter) => {
                session.net.handshake.borrow_mut().enter(nonce());
                #[cfg(target_arch = "x86")]
                crate::matchend::on_lobby_enter();
                #[cfg(target_arch = "x86")]
                crate::replayplay::on_lobby_enter();
                session
                    .registry
                    .dispatch(W2R_EVT_LOBBY_ENTER, core::ptr::null());
            }
            Some(Event::Leave) => {
                session.net.handshake.borrow_mut().leave();
                session
                    .registry
                    .dispatch(W2R_EVT_LOBBY_LEAVE, core::ptr::null());
            }
            None => {}
        }
        if name == Some(lobby::WINDOW) {
            let net = session.net.clone();
            let chosen = Locks {
                teams: session.config.lock_team_changes,
                slots: session.config.lock_slot_swaps,
            };
            lobbypolicy::with(|p| p.frame(now, net.transport.is_host(), chosen));
            #[cfg(target_arch = "x86")]
            crate::replayplay::lobby_frame();
            while let Some(e) = lobbypolicy::with(|p| p.next_event()) {
                StartupLog.line(
                    W2R_LOG_INFO,
                    "lobbypolicy",
                    &format!(
                        "{} {}",
                        if e.teams {
                            "team changes"
                        } else {
                            "slot swaps"
                        },
                        if e.locked { "LOCKED" } else { "unlocked" }
                    ),
                );
                #[cfg(target_arch = "x86")]
                crate::lobbyscreen::push_notice(
                    lobbylayout::lock_notice(e.teams, e.locked).to_owned(),
                );
            }
            #[cfg(target_arch = "x86")]
            crate::mapdownload::frame(
                now,
                session.config.map_sharing,
                !session.config.auto_download,
            );
            if net.transport.is_host() && net.transport.connected() {
                lobbypolicy::frame_work();
            }
            if net.transport.is_host()
                && net.transport.connected()
                && lobbypolicy::with(|p| p.take_send(now))
            {
                let locks = lobbypolicy::with(|p| p.locks());
                if let Some(msg) = lobbypolicy::encode(locks, &lobbypolicy::host_password()) {
                    for slot in 1..peer::MAX_SLOTS {
                        let announced = net.handshake.try_borrow().is_ok_and(|h| h.announced(slot));
                        if announced && !net.transport.send_to(slot, &msg) {
                            StartupLog.line(W2R_LOG_WARN, "lobbypolicy", "send failed");
                        }
                    }
                }
            }
            let f = handshake::Frame {
                now,
                connected: net.transport.connected(),
                is_host: net.transport.is_host(),
                lobby_slots: net.transport.lobby_slots(),
            };
            let outs = match net.handshake.try_borrow_mut() {
                Ok(mut h) => h.frame(f, || net.transport.peers()),
                Err(_) => Vec::new(),
            };
            net.run(outs);
            #[cfg(target_arch = "x86")]
            {
                let everyone = net.handshake.try_borrow().is_ok_and(|h| h.everyone_has());
                crate::observer::frame(
                    everyone,
                    net.transport.is_host() && net.transport.connected(),
                );
            }
        }
    });
}

/// Builds the session on the first frame. It is built *outside* the cell's borrow: loading a
/// plugin runs its `DllMain` and `on_load`, either of which may draw a window and land back in
/// `on_window`, and a fault in there is caught by the frame hook's guard, which abandons this
/// frame; a live `RefMut` would then leave the cell borrowed for the rest of the session.
fn ensure_started() {
    let needed = SESSION.with(|c| c.try_borrow().map(|s| s.is_none()).unwrap_or(false));
    if !needed || STARTING.replace(true) {
        return;
    }
    let session = start();
    SESSION.with(|c| {
        if let Ok(mut slot) = c.try_borrow_mut() {
            *slot = Some(session);
        }
    });
    STARTING.set(false);
}

// ---- inbound messages ---------------------------------------------------------------------

/// A W2R message taken off the wire, owned so it can wait.
enum Inbound {
    Plugin {
        sender: u32,
        tag: [u8; 4],
        payload: Vec<u8>,
    },
    Handshake {
        sender: u32,
        body: Vec<u8>,
    },
}

/// Messages that could not be handled where they arrived (another thread, or the session was
/// busy) wait here for the next frame. Bounded: a flood drops the excess, counted.
const QUEUE_CAP: usize = 256;
static QUEUE: Mutex<VecDeque<Inbound>> = Mutex::new(VecDeque::new());
static QUEUE_DROPPED: AtomicU32 = AtomicU32::new(0);

fn enqueue(item: Inbound) {
    static OFF_THREAD: std::sync::Once = std::sync::Once::new();
    if !on_main_thread() {
        OFF_THREAD.call_once(|| {
            StartupLog.line(
                W2R_LOG_INFO,
                "host",
                "peer messages arrive off the main thread; queued for the next frame",
            );
        });
    }
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= QUEUE_CAP {
        drop(q);
        let n = QUEUE_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 || n % 1000 == 0 {
            StartupLog.line(
                W2R_LOG_WARN,
                "host",
                &format!("inbound queue full ({QUEUE_CAP}); dropped a message ({n} so far)"),
            );
        }
        return;
    }
    q.push_back(item);
}

/// Hands one message on. False when the receiver is busy right now (the caller queues it).
/// Main thread only.
fn deliver_now(item: &Inbound) -> bool {
    match item {
        Inbound::Plugin {
            sender,
            tag,
            payload,
        } => SESSION.with(|cell| {
            let Ok(slot) = cell.try_borrow() else {
                return false;
            };
            if let Some(session) = slot.as_ref() {
                session
                    .registry
                    .dispatch_peer(*sender, PluginMessage { tag: *tag, payload });
            }
            true
        }),
        Inbound::Handshake { sender, body } => {
            let Some(net) = NET.with(|n| n.borrow().clone()) else {
                // No session yet: nobody is in a lobby, so there is nothing to answer.
                return true;
            };
            let outs = match net.handshake.try_borrow_mut() {
                Ok(mut h) => h.receive(chat::now_ms(), *sender, body),
                Err(_) => return false,
            };
            net.run(outs);
            true
        }
    }
}

/// Main thread, top level: delivers what waited. Bounded per frame so a flood from another
/// thread cannot keep this loop running.
fn drain_queue() {
    for _ in 0..QUEUE_CAP {
        let item = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        let Some(item) = item else {
            return;
        };
        if !deliver_now(&item) {
            QUEUE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_front(item);
            return;
        }
    }
}

/// A plugin peer message taken off the wire. Delivered now when possible, otherwise on the
/// next frame.
pub fn deliver_peer(sender: u32, msg: PluginMessage<'_>) {
    let item = Inbound::Plugin {
        sender,
        tag: msg.tag,
        payload: msg.payload.to_vec(),
    };
    if !on_main_thread() || !deliver_now(&item) {
        enqueue(item);
    }
}

/// A handshake message (`'H'`) taken off the wire; `body` is everything after the envelope.
pub fn deliver_handshake(sender: u32, body: &[u8]) {
    let item = Inbound::Handshake {
        sender,
        body: body.to_vec(),
    };
    if !on_main_thread() || !deliver_now(&item) {
        enqueue(item);
    }
}

// ---- UI -----------------------------------------------------------------------------------

/// Runs `f` with the game's UI functions, if the session has them.
#[cfg(target_arch = "x86")]
pub fn with_nk(f: impl FnOnce(&options::Nk)) {
    let nk = SESSION.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|s| s.as_ref().and_then(|s| s.nk))
    });
    if let Some(nk) = nk {
        f(&nk);
    }
}

/// What the lobby should tell the local player right now (docs/load-policy.md, "Policy"). Shown
/// on the plugin screen; the in-lobby banner is parked (docs/PLAN.md, Phase 3 step 1).
pub fn lobby_warnings() -> Vec<String> {
    NET.with(|n| {
        n.borrow()
            .as_ref()
            .and_then(|net| {
                net.handshake
                    .try_borrow()
                    .ok()
                    .map(|h| h.warnings(chat::now_ms()))
            })
            .unwrap_or_default()
    })
}

/// Draws the plugin list with `w` and applies the user's toggles. Called from the options
/// screen hook on the main thread.
pub fn draw_plugin_screen(w: &dyn Widgets) -> Action {
    SESSION.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return Action::None;
        };
        let Some(session) = slot.as_mut() else {
            return Action::None;
        };
        let mut rows: Vec<PluginRow> = session
            .state
            .plugins
            .iter()
            .map(|p| {
                let (status, slow) = match session.registry.find(&p.id) {
                    Some(s) => match s.state() {
                        SlotState::Active => (Status::Active, s.is_slow()),
                        SlotState::Disabled { reason } => (Status::Disabled(reason), false),
                    },
                    None => (Status::NotLoaded, false),
                };
                PluginRow {
                    id: p.id.clone(),
                    version: state_version(p),
                    author: session.authors.get(&p.id).cloned(),
                    enabled: p.enabled,
                    status,
                    slow,
                }
            })
            .collect();
        let mut host = HostRow {
            fingerprint: session.fingerprint,
            simulation: session.simulation.clone(),
            strict: session.config.require_matching_plugins,
            lock_teams: session.config.lock_team_changes,
            lock_slots: session.config.lock_slot_swaps,
            create_screen: session.config.enhanced_create_screen,
            lobby_screen: session.config.enhanced_lobby_screen,
            map_sharing: session.config.map_sharing,
            auto_download: session.config.auto_download,
            record_replays: session.config.record_replays,
            auto_game_name: session.config.auto_game_name,
            share_match_data: crate::uploader::enabled(),
            lobby: lobby_warnings(),
        };
        let registry = &session.registry;
        let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let action = ui::draw_plugin_screen(w, &mut host, &mut rows, |w, i| {
            if let Some(s) = registry.find(&ids[i]) {
                ui::with_frame(w, |table| registry.call_on_ui(s, table));
            }
        });
        match action {
            Action::Toggle(i) => {
                let row = &rows[i];
                session.state.set_enabled(&row.id, row.enabled);
                let path = session.root.join("config").join("state.json");
                match state::write(&path, &session.state) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "{} {} for the next launch",
                            row.id,
                            if row.enabled { "enabled" } else { "disabled" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write state.json: {e}"),
                    ),
                }
            }
            Action::ToggleStrict => {
                session.config.require_matching_plugins = host.strict;
                match hostcfg::write(&session.root, &session.config) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "require_matching_plugins {}",
                            if host.strict { "on" } else { "off" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write {}: {e}", hostcfg::FILE),
                    ),
                }
                let net = session.net.clone();
                let outs = match net.handshake.try_borrow_mut() {
                    Ok(mut h) => h.set_strict(host.strict),
                    Err(_) => Vec::new(),
                };
                net.run(outs);
            }
            Action::ToggleLocks => {
                save_locks(session, host.lock_teams, host.lock_slots);
            }
            Action::ToggleMapSharing => {
                session.config.map_sharing = host.map_sharing;
                session.config.auto_download = host.auto_download;
                match hostcfg::write(&session.root, &session.config) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "map sharing {}, auto download {}",
                            if host.map_sharing { "on" } else { "off" },
                            if host.auto_download { "on" } else { "off" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write {}: {e}", hostcfg::FILE),
                    ),
                }
                if let Ok(mut h) = session.net.handshake.try_borrow_mut() {
                    h.set_map_sharing(host.map_sharing);
                }
            }
            Action::ToggleAutoGameName => {
                session.config.auto_game_name = host.auto_game_name;
                #[cfg(target_arch = "x86")]
                crate::createscreen::set_auto_game_name(host.auto_game_name);
                match hostcfg::write(&session.root, &session.config) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "auto_game_name {}",
                            if host.auto_game_name { "on" } else { "off" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write {}: {e}", hostcfg::FILE),
                    ),
                }
            }
            Action::ToggleSharing => {
                let line = match crate::uploader::set_enabled(&session.root, host.share_match_data)
                {
                    Ok(()) => format!(
                        "share_match_data {}",
                        if host.share_match_data { "on" } else { "off" }
                    ),
                    Err(e) => format!("cannot write sharing.json: {e}"),
                };
                StartupLog.line(W2R_LOG_INFO, "host", &line);
            }
            Action::ToggleRecording => {
                session.config.record_replays = host.record_replays;
                #[cfg(target_arch = "x86")]
                crate::replayrec::set_enabled(host.record_replays);
                match hostcfg::write(&session.root, &session.config) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "record_replays {}",
                            if host.record_replays { "on" } else { "off" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write {}: {e}", hostcfg::FILE),
                    ),
                }
            }
            Action::ToggleLobbyScreen => {
                session.config.enhanced_lobby_screen = host.lobby_screen;
                match hostcfg::write(&session.root, &session.config) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "enhanced_lobby_screen {}",
                            if host.lobby_screen { "on" } else { "off" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write {}: {e}", hostcfg::FILE),
                    ),
                }
            }
            Action::ToggleCreateScreen => {
                session.config.enhanced_create_screen = host.create_screen;
                match hostcfg::write(&session.root, &session.config) {
                    Ok(()) => StartupLog.line(
                        W2R_LOG_INFO,
                        "host",
                        &format!(
                            "enhanced_create_screen {}",
                            if host.create_screen { "on" } else { "off" }
                        ),
                    ),
                    Err(e) => StartupLog.line(
                        W2R_LOG_ERROR,
                        "host",
                        &format!("cannot write {}: {e}", hostcfg::FILE),
                    ),
                }
            }
            Action::None | Action::Back => {}
        }
        action
    })
}

/// What the map-sharing code needs from the handshake and the game's network, without the
/// session itself (which may be borrowed).
#[cfg(target_arch = "x86")]
pub(crate) struct NetView(Rc<Net>);

#[cfg(target_arch = "x86")]
pub(crate) fn net_view() -> Option<NetView> {
    NET.with(|n| n.borrow().clone().map(NetView))
}

#[cfg(target_arch = "x86")]
impl NetView {
    pub(crate) fn is_host(&self) -> bool {
        self.0.transport.is_host()
    }

    pub(crate) fn connected(&self) -> bool {
        self.0.transport.connected()
    }

    /// The player announced the handshake: they run the mod.
    pub(crate) fn peer_runs_mod(&self, slot: u32) -> bool {
        self.0
            .handshake
            .try_borrow()
            .is_ok_and(|h| h.announced(slot))
    }

    pub(crate) fn peer_shares_maps(&self, slot: u32) -> bool {
        self.0
            .handshake
            .try_borrow()
            .is_ok_and(|h| h.peer_shares_maps(slot))
    }

    pub(crate) fn send_to(&self, slot: u32, bytes: &[u8]) -> bool {
        self.0.transport.send_to(slot, bytes)
    }

    pub(crate) fn kick(&self, slot: u32) -> bool {
        self.0.transport.kick(slot)
    }

    /// The lobby slot (0 to 7) a net slot sits in, when the game says.
    pub(crate) fn lobby_slot_of(&self, net_slot: u32) -> Option<u8> {
        self.0
            .transport
            .lobby_slots()
            .and_then(|s| s.get(net_slot as usize).copied().flatten())
    }
}

/// The host's current lock choice, `(teams, slots)`, for the create screen's buttons.
pub fn lock_choice() -> Option<(bool, bool)> {
    SESSION.with(|cell| {
        let slot = cell.try_borrow().ok()?;
        let s = slot.as_ref()?;
        Some((s.config.lock_team_changes, s.config.lock_slot_swaps))
    })
}

/// Stores the lock choice in `host.json` (applied when hosting, see `on_window`).
pub fn set_lock_choice(teams: bool, slots: bool) {
    SESSION.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut()
            && let Some(s) = slot.as_mut()
        {
            save_locks(s, teams, slots);
        }
    });
}

/// `(map_sharing, ask)` from `host.json`: whether map sharing is on, and whether a missing map
/// asks first (`auto_download` off).
pub fn map_sharing_settings() -> (bool, bool) {
    SESSION.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|s| {
                s.as_ref()
                    .map(|s| (s.config.map_sharing, !s.config.auto_download))
            })
            .unwrap_or((false, true))
    })
}

/// Whether any active plugin has registered a Create-screen callback.
pub fn create_ui_present() -> bool {
    SESSION.with(|cell| {
        cell.try_borrow().ok().is_some_and(|s| {
            s.as_ref()
                .is_some_and(|s| s.registry.slots().any(|slot| slot.create_ui().is_some()))
        })
    })
}

/// Calls every plugin's Create-screen callback with a table over `w`; inside them `picker`
/// serves `create_map_count` and `create_map_select`. Main thread, from the screen's draw.
pub fn draw_create_ui(w: &dyn Widgets, picker: &mut dyn crate::createpick::MapPicker) {
    SESSION.with(|cell| {
        let Ok(session) = cell.try_borrow() else {
            return;
        };
        let Some(session) = session.as_ref() else {
            return;
        };
        crate::createpick::with_picker(picker, || {
            for slot in session.registry.slots() {
                if slot.create_ui().is_some() {
                    ui::with_frame(w, |table| session.registry.call_create_ui(slot, table));
                }
            }
        });
    });
}

/// Whether the replacement Create Multiplayer Game screen is switched on (`host.json`).
pub fn enhanced_create_screen() -> bool {
    SESSION.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|s| s.as_ref().map(|s| s.config.enhanced_create_screen))
            .unwrap_or(false)
    })
}

/// Whether the replacement multiplayer lobby screen is switched on (`host.json`).
pub fn enhanced_lobby_screen() -> bool {
    SESSION.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|s| s.as_ref().map(|s| s.config.enhanced_lobby_screen))
            .unwrap_or(false)
    })
}

/// For the lobby screen's marks: the lobby seat of the host (net slot 0), and which lobby seats
/// hold a player running the mod (announced in the handshake, or ourselves). Empty off a lobby.
/// The third value is the version text each such player announced (ours for ourselves).
pub fn lobby_roles() -> (Option<u8>, [ModMark; 8], [String; 8]) {
    SESSION.with(|cell| {
        let none = || (None, [ModMark::None; 8], Default::default());
        let Ok(slot) = cell.try_borrow() else {
            return none();
        };
        let Some(s) = slot.as_ref() else {
            return none();
        };
        let (Some(table), Ok(h)) = (s.net.transport.lobby_slots(), s.net.handshake.try_borrow())
        else {
            return none();
        };
        if !h.in_lobby() {
            return (table[0], [ModMark::None; 8], Default::default());
        }
        let marks = lobbylayout::mod_slots(&table, |n| h.peer_matches(n), h.self_slot());
        let mut versions: [String; 8] = Default::default();
        for (net, lobby) in table.iter().enumerate() {
            let Some(l) = lobby.filter(|&l| (l as usize) < 8) else {
                continue;
            };
            versions[l as usize] = if h.self_slot() == Some(net as u32) {
                handshake::VERSION_TEXT.to_owned()
            } else {
                h.peer(net as u32).map(|p| p.version.clone()).unwrap_or_default()
            };
        }
        (table[0], marks, versions)
    })
}

pub fn log_error(source: &str, msg: &str) {
    StartupLog.line(W2R_LOG_ERROR, source, msg);
}

/// A log line from another host module.
pub fn log_info(source: &str, msg: &str) {
    StartupLog.line(W2R_LOG_INFO, source, msg);
}

fn save_locks(session: &mut Session, teams: bool, slots: bool) {
    session.config.lock_team_changes = teams;
    session.config.lock_slot_swaps = slots;
    match hostcfg::write(&session.root, &session.config) {
        Ok(()) => StartupLog.line(
            W2R_LOG_INFO,
            "host",
            &format!(
                "lobby locks: team changes {}, slot swaps {}",
                if teams { "locked" } else { "open" },
                if slots { "locked" } else { "open" }
            ),
        ),
        Err(e) => StartupLog.line(
            W2R_LOG_ERROR,
            "host",
            &format!("cannot write {}: {e}", hostcfg::FILE),
        ),
    }
}

fn state_version(p: &state::PluginState) -> String {
    let v = &p.version;
    format!(
        "{}.{}.{}{}",
        v.major,
        v.minor,
        v.patch,
        if v.pre.is_empty() { "" } else { "-pre" }
    )
}

// ---- startup ------------------------------------------------------------------------------

fn start() -> Session {
    let log: Rc<dyn Log> = Rc::new(StartupLog);
    let lobby = Rc::new(RefCell::new(Lobby::new()));
    let running = Build::parse(GAME_VERSION).expect("GAME_VERSION is well-formed");

    let Some(startup) = entry::startup() else {
        // Cannot happen: the frame hook exists only after startup; keep an empty session.
        let net = Rc::new(Net::idle());
        NET.with(|n| *n.borrow_mut() = Some(net.clone()));
        return Session {
            registry: Registry::new(log),
            lobby,
            state: State::default(),
            root: PathBuf::new(),
            config: HostConfig::default(),
            fingerprint: [0; 4],
            simulation: Vec::new(),
            authors: BTreeMap::new(),
            enabled_at_start: 0,
            net,
            #[cfg(target_arch = "x86")]
            nk: None,
        };
    };
    let (config, warning) = hostcfg::read(&startup.root);
    if let Some(w) = warning {
        log.line(W2R_LOG_WARN, "host", &w);
    }
    if config.require_matching_plugins {
        log.line(
            W2R_LOG_INFO,
            "host",
            "require_matching_plugins is on: when hosting, mismatched players are removed",
        );
    }
    let symbols = Symbols::new(startup.game_base);
    let game = startup.image.clone().map(|image| Game { image, symbols });
    let transport = transport(&symbols, &*log);
    let net = Rc::new(Net {
        handshake: RefCell::new(Handshake::new(handshake::Local {
            strict: config.require_matching_plugins,
            fingerprint: [0; 4],
        })),
        transport: transport.clone(),
    });
    net.handshake
        .borrow_mut()
        .set_map_sharing(config.map_sharing);
    NET.with(|n| *n.borrow_mut() = Some(net.clone()));
    let services = Services {
        chat: lobby_chat(&symbols, lobby.clone(), &*log),
        transport,
        roster: Rc::new(HandshakeRoster(net.clone())),
        presence: Rc::new(LobbyPresence(lobby.clone())),
        root: None,
    };
    if let Some(image) = &startup.image {
        peer::hook::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        lobbypolicy::hook::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::mapdownload::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::matchstart::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::matchend::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::matchend::install_run_game(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::replayrec::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::replayplay::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::chatrec::install(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::hudpanel::install(&symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::ingamekick::install(&symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::ingamekick::install_alliances(image, &symbols, &*log);
        #[cfg(target_arch = "x86")]
        crate::alliancesscreen::install(&symbols, &*log);
    }
    #[cfg(target_arch = "x86")]
    let nk = {
        let nk = options::Nk::new(&symbols);
        match (&nk, &startup.image) {
            (Some(_), Some(image)) => {
                options::install(image, &symbols, &*log);
                options::install_main_menu(image, &symbols, &*log);
                crate::lobbycreate::install(image, &symbols, &*log);
                crate::joinpassword::install(image, &symbols, &*log);
                crate::gameinfopatch::install(image, symbols.base(), &*log);
                crate::createscreen::install(&symbols, &*log);
                crate::lobbyscreen::install(image, &symbols, &*log);
            }
            _ => log.line(
                W2R_LOG_ERROR,
                "host",
                "UI symbols missing; no plugin screen",
            ),
        }
        nk
    };

    log.line(W2R_LOG_INFO, "host", "loading plugins on the main thread");
    #[cfg(target_arch = "x86")]
    let host_log = log.clone();
    let loaded = plugins::load_all(&startup.root, running, log, game, services);
    #[cfg(target_arch = "x86")]
    if loaded.simulation.iter().any(|id| id == "w2r-observer") {
        crate::lobbyscreen::add_observer_team_option(&symbols, &*host_log);
    }
    net.handshake
        .borrow_mut()
        .set_fingerprint(loaded.fingerprint);
    #[cfg(target_arch = "x86")]
    {
        crate::uploader::init(&startup.root);
        crate::lobbyratings::init(&startup.root);
        crate::matchend::init_reporter(
            &startup.root,
            loaded.fingerprint,
            crate::handshake::VERSION_TEXT,
            &*host_log,
        );
        crate::replayrec::init(
            &startup.root,
            loaded.fingerprint,
            crate::handshake::VERSION_TEXT,
            &*host_log,
        );
        crate::replayrec::set_enabled(config.record_replays);
        crate::createscreen::set_auto_game_name(config.auto_game_name);
        crate::replayplay::init(&startup.root, loaded.fingerprint);
    }
    let authors = plugins::authors(&startup.root, &loaded.state);
    let enabled_at_start = loaded.state.plugins.iter().filter(|p| p.enabled).count();
    Session {
        registry: loaded.registry,
        lobby,
        state: loaded.state,
        root: startup.root.clone(),
        config,
        fingerprint: loaded.fingerprint,
        simulation: loaded.simulation,
        authors,
        enabled_at_start,
        net,
        #[cfg(target_arch = "x86")]
        nk,
    }
}

#[cfg(target_arch = "x86")]
fn lobby_chat(symbols: &Symbols, lobby: Rc<RefCell<Lobby>>, log: &dyn Log) -> Rc<dyn Chat> {
    match chat::game::LobbyChat::new(symbols, lobby) {
        Some(c) => Rc::new(c),
        None => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                "lobby chat symbols missing; chat_send unavailable",
            );
            Rc::new(NoChat)
        }
    }
}

#[cfg(not(target_arch = "x86"))]
fn lobby_chat(_: &Symbols, _: Rc<RefCell<Lobby>>, _: &dyn Log) -> Rc<dyn Chat> {
    Rc::new(NoChat)
}

#[cfg(target_arch = "x86")]
fn transport(symbols: &Symbols, log: &dyn Log) -> Rc<dyn Transport> {
    match peer::game::GameTransport::new(symbols) {
        Some(t) => Rc::new(t),
        None => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                "net session symbols missing; peer_send and the handshake are unavailable",
            );
            Rc::new(NoTransport)
        }
    }
}

#[cfg(not(target_arch = "x86"))]
fn transport(_: &Symbols, _: &dyn Log) -> Rc<dyn Transport> {
    Rc::new(NoTransport)
}
