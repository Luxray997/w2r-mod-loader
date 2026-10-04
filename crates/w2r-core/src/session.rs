//! The host's main-thread state: the plugin registry, lobby presence and the user's
//! `state.json`. Created on the first frame the game draws after startup and driven by every
//! `BeginNamedWindow` call from then on. Only the game's main thread touches it, which is why a
//! `thread_local` holds it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use w2r_abi::{
    W2R_EVT_LOBBY_ENTER, W2R_EVT_LOBBY_LEAVE, W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

use crate::chat::{self, Chat, NoChat};
use crate::dispatch::Registry;
use crate::entry;
use crate::frame;
use crate::host::{Game, Log, Services, State as SlotState};
use crate::lobby::{Event, Lobby};
use crate::manifest::Build;
#[cfg(target_arch = "x86")]
use crate::options;
use crate::peer::{self, NoPeers, NoTransport, PluginMessage, Presence, Transport};
use crate::plugins;
use crate::state::{self, State};
use crate::symbols::Symbols;
use crate::target::GAME_VERSION;
use crate::ui::{self, Action, Frame, PluginRow, Status, Widgets};

struct Session {
    registry: Registry,
    lobby: Rc<RefCell<Lobby>>,
    /// As loaded at startup, plus the user's toggles (which take effect at the next launch).
    state: State,
    root: PathBuf,
    #[cfg(target_arch = "x86")]
    nk: Option<options::Nk>,
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
                    enabled: p.enabled,
                    status,
                    slow,
                }
            })
            .collect();
        let registry = &session.registry;
        let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let action = ui::draw_plugin_screen(w, &mut rows, |w, i| {
            if let Some(s) = registry.find(&ids[i]) {
                let mut frame = Frame::new(w);
                let table = frame.table();
                registry.call_on_ui(s, &table);
            }
        });
        if let Action::Toggle(i) = action {
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
        action
    })
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

fn start() -> Session {
    let log: Rc<dyn Log> = Rc::new(StartupLog);
    let lobby = Rc::new(RefCell::new(Lobby::new()));
    let running = Build::parse(GAME_VERSION).expect("GAME_VERSION is well-formed");

    let Some(startup) = entry::startup() else {
        // Cannot happen: the frame hook exists only after startup; keep an empty session.
        return Session {
            registry: Registry::new(log),
            lobby,
            state: State::default(),
            root: PathBuf::new(),
            #[cfg(target_arch = "x86")]
            nk: None,
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
    #[cfg(target_arch = "x86")]
    let nk = {
        let nk = options::Nk::new(&symbols);
        match (&nk, &startup.image) {
            (Some(_), Some(image)) => {
                options::install(image, &symbols, &*log);
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
    let (registry, state) = plugins::load_all(&startup.root, running, log, game, services);
    Session {
        registry,
        lobby,
        state,
        root: startup.root.clone(),
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
