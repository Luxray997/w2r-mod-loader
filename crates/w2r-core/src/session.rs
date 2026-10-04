//! The host's main-thread state: the plugin registry and lobby presence. Created on the first
//! frame the game draws after startup and driven by every `BeginNamedWindow` call from then on.
//! Only the game's main thread touches it, which is why a `thread_local` holds it.

use std::cell::RefCell;
use std::rc::Rc;

use w2r_abi::{W2R_EVT_LOBBY_ENTER, W2R_EVT_LOBBY_LEAVE, W2R_LOG_INFO};

use crate::chat::{self, Chat, NoChat};
use crate::dispatch::Registry;
use crate::entry;
use crate::host::{Game, Log};
use crate::lobby::{Event, Lobby};
use crate::manifest::Build;
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
    let chat: Rc<dyn Chat> = lobby_chat(&symbols, lobby.clone(), &*log);

    log.line(W2R_LOG_INFO, "host", "loading plugins on the main thread");
    let registry = plugins::load_all(&startup.root, running, log, game, chat);
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
