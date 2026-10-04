//! The host's main-thread state: the plugin registry and lobby presence. Created on the first
//! frame the game draws after startup and driven by every `BeginNamedWindow` call from then on.
//! Only the game's main thread touches it, which is why a `thread_local` holds it.

use std::cell::RefCell;
use std::rc::Rc;

use w2r_abi::{W2R_EVT_LOBBY_ENTER, W2R_EVT_LOBBY_LEAVE, W2R_LOG_INFO, W2R_LOG_WARN};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

use crate::chat::{self, Chat, NoChat};
use crate::dispatch::Registry;
use crate::entry;
use crate::frame;
use crate::host::{Game, Log, Services};
use crate::lobby::{Event, Lobby};
use crate::manifest::Build;
use crate::peer::{self, NoPeers, NoTransport, PluginMessage, Presence, Transport};
use crate::plugins;
use crate::symbols::Symbols;
use crate::target::GAME_VERSION;

struct Session {
    registry: Registry,
    lobby: Rc<RefCell<Lobby>>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Routes to the startup log; the registry needs an `Rc<dyn Log>`.
struct StartupLog;

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

/// One `BeginNamedWindow` call on the main thread. `name` is the window the game is starting,
/// if it gave one.
pub fn on_window(name: Option<&str>) {
    let now = chat::now_ms();
    SESSION.with(|cell| {
        // A plugin callback below may make the game draw a window, which lands back here;
        // that nested call must not touch the session.
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        let session = slot.get_or_insert_with(start);
        let event = {
            let mut lobby = session.lobby.borrow_mut();
            match name {
                Some(n) => lobby.on_window(n, now),
                None => lobby.on_frame(now),
            }
        };
        match event {
            Some(Event::Enter) => session
                .registry
                .dispatch(W2R_EVT_LOBBY_ENTER, core::ptr::null()),
            Some(Event::Leave) => session
                .registry
                .dispatch(W2R_EVT_LOBBY_LEAVE, core::ptr::null()),
            None => {}
        }
    });
}

/// A plugin peer message taken off the wire. Delivered only on the main thread, after the
/// session exists (a message before the first frame has nobody to receive it anyway).
pub fn deliver_peer(sender: u32, msg: PluginMessage<'_>) {
    static OFF_THREAD_LOGGED: std::sync::Once = std::sync::Once::new();
    if unsafe { GetCurrentThreadId() } != frame::main_thread_id() {
        OFF_THREAD_LOGGED.call_once(|| {
            StartupLog.line(
                W2R_LOG_WARN,
                "host",
                "peer messages arrive off the main thread; dropping them (needs a queue)",
            );
        });
        return;
    }
    SESSION.with(|cell| {
        let Ok(slot) = cell.try_borrow() else {
            return;
        };
        if let Some(session) = slot.as_ref() {
            session.registry.dispatch_peer(sender, msg);
        }
    });
}

fn start() -> Session {
    let log: Rc<dyn Log> = Rc::new(StartupLog);
    let lobby = Rc::new(RefCell::new(Lobby::new()));
    let running = Build::parse(GAME_VERSION).expect("GAME_VERSION is well-formed");

    let Some(startup) = entry::startup() else {
        // Cannot happen: the frame hook exists only after startup; keep an empty session.
        return Session {
            registry: Registry::new(log),
            lobby,
        };
    };
    let symbols = Symbols::new(startup.game_base);
    let game = startup.image.clone().map(|image| Game { image, symbols });
    let services = Services {
        chat: lobby_chat(&symbols, lobby.clone(), &*log),
        transport: transport(&symbols, &*log),
        // The handshake feature (Phase 3) will replace this; until then nobody is announced.
        roster: Rc::new(NoPeers),
        presence: Rc::new(LobbyPresence(lobby.clone())),
    };
    if let Some(image) = &startup.image {
        peer::hook::install(image, &symbols, &*log);
    }

    log.line(W2R_LOG_INFO, "host", "loading plugins on the main thread");
    let registry = plugins::load_all(&startup.root, running, log, game, services);
    Session { registry, lobby }
}

#[cfg(target_arch = "x86")]
fn lobby_chat(symbols: &Symbols, lobby: Rc<RefCell<Lobby>>, log: &dyn Log) -> Rc<dyn Chat> {
    match chat::game::LobbyChat::new(symbols, lobby) {
        Some(c) => Rc::new(c),
        None => {
            log.line(
                w2r_abi::W2R_LOG_ERROR,
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
                w2r_abi::W2R_LOG_ERROR,
                "host",
                "net session symbol missing; peer_send unavailable",
            );
            Rc::new(NoTransport)
        }
    }
}

#[cfg(not(target_arch = "x86"))]
fn transport(_: &Symbols, _: &dyn Log) -> Rc<dyn Transport> {
    Rc::new(NoTransport)
}
