//! The players the "Waiting for players" screen is waiting on (docs/waiting-for-players.md). After
//! about 2.5 s without a full set of turns the game stops and shows a window whose only text is
//! the localized string `mp_waiting_for_players`. The game does work out who it is waiting for
//! (`FUN_004f86e0`: seated humans in the match whose turn has not arrived) but writes the names
//! into a dialog object the Remastered screen no longer has, so nothing shows them. We repeat that
//! test and draw one row per player under the game's line, each with how long the match has been
//! waiting on them.

use core::ffi::CStr;

/// The localization key of the screen's text.
pub const KEY: &CStr = c"mp_waiting_for_players";

pub const SLOT_COUNT: usize = 8;
/// `TurnSlotFlags` bits: in the match, and the turn has been received.
pub const FLAG_IN_MATCH: u32 = 0x1_0000;
pub const FLAG_TURN_RECEIVED: u32 = 0x2_0000;
/// Longest name the game stores.
pub const NAME_MAX: usize = 0x19;
/// A gap between two draws longer than this means the screen went away and came back.
pub const GAP_MS: f64 = 1500.0;

/// One lobby seat as the roster holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    /// The seat's state byte: 0 is a human player.
    pub state: u8,
    pub net_slot: u32,
    /// `TurnSlotFlags[net_slot]`, or 0 when the net slot is out of range.
    pub turn_flags: u32,
    pub name: String,
}

/// The game's own test: a human who is in the match and whose turn has not arrived. Returns the
/// seat index and the name.
pub fn waiting(seats: &[Seat]) -> Vec<(usize, String)> {
    seats
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.state == 0
                && s.turn_flags & FLAG_TURN_RECEIVED == 0
                && s.turn_flags & FLAG_IN_MATCH != 0
                && !s.name.is_empty()
        })
        .map(|(i, s)| (i, s.name.clone()))
        .collect()
}

/// A name from the roster's fixed buffer: up to the first NUL, control characters dropped, so a
/// hostile name cannot put anything but text on the screen.
pub fn clean_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text = String::from_utf8_lossy(&raw[..end.min(NAME_MAX)]);
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Remembers since when each seat has been waited on.
#[derive(Clone, Debug, Default)]
pub struct Waits {
    since: [Option<f64>; SLOT_COUNT],
    last_draw: Option<f64>,
}

impl Waits {
    pub const fn new() -> Waits {
        Waits {
            since: [None; SLOT_COUNT],
            last_draw: None,
        }
    }

    /// Takes the seats being waited on at `now_ms` and returns each with the seconds it has been
    /// waited on. A seat that stops being waited on is forgotten, and so is everything when the
    /// screen has been away for a while.
    pub fn update(&mut self, now_ms: f64, waiting: &[(usize, String)]) -> Vec<(String, f64)> {
        if self.last_draw.is_none_or(|t| now_ms - t > GAP_MS) {
            self.since = [None; SLOT_COUNT];
        }
        self.last_draw = Some(now_ms);
        for (i, slot) in self.since.iter_mut().enumerate() {
            if !waiting.iter().any(|(s, _)| *s == i) {
                *slot = None;
            }
        }
        waiting
            .iter()
            .filter(|(i, _)| *i < SLOT_COUNT)
            .map(|(i, name)| {
                let since = *self.since[*i].get_or_insert(now_ms);
                (name.clone(), ((now_ms - since) / 1000.0).max(0.0))
            })
            .collect()
    }
}

/// "Ann - 0:07": the name and the wait as minutes and seconds, as the game writes its countdowns.
pub fn line(name: &str, seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{name} - {}:{:02}", total / 60, total % 60)
}

#[cfg(all(windows, target_arch = "x86"))]
pub mod game {
    //! Reads the roster, hooks the screen's label and adds the rows (x86 only).

    use core::ffi::{c_char, c_void};
    use std::ffi::CString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    use w2r_abi::W2R_LOG_ERROR;

    use super::{NAME_MAX, SLOT_COUNT, Seat, Waits, clean_name, line, waiting};
    use crate::guard::guarded;
    use crate::hooks::{self, Owner};
    use crate::host::Log;
    use crate::pe::Image;
    use crate::screenkit::game::Kit;
    use crate::symbols::Symbols;

    const SLOT_STRIDE: usize = 0x26;
    const SLOT_NET: usize = 0x04;
    const SLOT_STATE: usize = 0x08;
    const SLOT_NAME: usize = 0x0d;
    /// The alignment the game draws the screen's text with: centred both ways.
    const ALIGN_CENTRE: i32 = 0x12;
    /// Row height of a player line, in font heights.
    const ROW_FONT_HEIGHTS: f32 = 1.6;

    type LabelFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32);

    struct Game {
        /// `MpLobbySlots`.
        slots: *const u8,
        /// `TurnSlotFlags`.
        turn_flags: *const u32,
        kit: Kit,
    }

    // SAFETY: plain game addresses and function pointers, fixed for the session; used from the
    // game's main thread.
    unsafe impl Send for Game {}
    unsafe impl Sync for Game {}

    static GAME: OnceLock<Game> = OnceLock::new();
    static ORIGINAL_LABEL: AtomicUsize = AtomicUsize::new(0);
    /// The pointer the game's lookup last returned for the waiting text.
    static WAIT_TEXT: AtomicUsize = AtomicUsize::new(0);
    static WAITS: Mutex<Waits> = Mutex::new(Waits::new());

    /// Resolves the roster and hooks `DrawLabel`; false (logged) when either fails, and the
    /// screen stays as the game draws it.
    pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
        let (Some(slots), Some(flags), Some(target), Some(kit)) = (
            symbols.address("MpLobbySlots"),
            symbols.address("TurnSlotFlags"),
            symbols.address("DrawLabel"),
            Kit::new(symbols),
        ) else {
            log.line(
                W2R_LOG_ERROR,
                "host",
                "waiting list: symbols missing; the Waiting screen stays as it is",
            );
            return false;
        };
        let _ = GAME.set(Game {
            slots: slots as *const u8,
            turn_flags: flags as *const u32,
            kit,
        });
        // SAFETY: `label_detour` matches DrawLabel's cdecl signature from the symbol table.
        let result = unsafe {
            hooks::global().install(
                Owner::Host,
                image,
                target,
                label_detour as *mut c_void,
                "host:DrawLabel (waiting list)",
                |original| ORIGINAL_LABEL.store(original as usize, Ordering::Release),
            )
        };
        if let Err(e) = result {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("waiting list: could not hook DrawLabel: {e:?}"),
            );
            return false;
        }
        true
    }

    /// Called by the localized-string lookup with what it returned for the waiting text, so the
    /// label that draws it can be told from every other label.
    pub fn note_text(text: *const c_char) {
        WAIT_TEXT.store(text as usize, Ordering::Release);
    }

    fn seats(g: &Game) -> Option<Vec<Seat>> {
        guarded(|| unsafe {
            (0..SLOT_COUNT)
                .map(|i| {
                    let base = g.slots.add(i * SLOT_STRIDE);
                    let net = base.add(SLOT_NET).cast::<u32>().read_unaligned();
                    let flags = if (net as usize) < SLOT_COUNT {
                        g.turn_flags.add(net as usize).read_unaligned()
                    } else {
                        0
                    };
                    let name = core::slice::from_raw_parts(base.add(SLOT_NAME), NAME_MAX);
                    Seat {
                        state: *base.add(SLOT_STATE),
                        net_slot: net,
                        turn_flags: flags,
                        name: clean_name(name),
                    }
                })
                .collect()
        })
        .ok()
    }

    unsafe extern "C" fn label_detour(ctx: *mut c_void, text: *const c_char, align: i32) {
        let original: LabelFn =
            unsafe { core::mem::transmute(ORIGINAL_LABEL.load(Ordering::Acquire)) };
        unsafe { original(ctx, text, align) };
        if !text.is_null()
            && align == ALIGN_CENTRE
            && text as usize == WAIT_TEXT.load(Ordering::Acquire)
        {
            // The game's line is drawn; the players go in rows under it.
            draw_rows(ctx, original);
        }
    }

    fn draw_rows(ctx: *mut c_void, label: LabelFn) {
        let Some(g) = GAME.get() else {
            return;
        };
        let Some(seats) = seats(g) else {
            return;
        };
        let rows = {
            let mut waits = WAITS.lock().unwrap_or_else(|e| e.into_inner());
            waits.update(crate::chat::now_ms() as f64, &waiting(&seats))
        };
        let height = g.kit.font_height() * ROW_FONT_HEIGHTS;
        for (name, seconds) in rows {
            let Ok(text) = CString::new(line(&name, seconds).replace('\0', "")) else {
                continue;
            };
            g.kit.row_dynamic(height, 1);
            unsafe { label(ctx, text.as_ptr(), ALIGN_CENTRE) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(state: u8, net: u32, flags: u32, name: &str) -> Seat {
        Seat {
            state,
            net_slot: net,
            turn_flags: flags,
            name: name.to_owned(),
        }
    }

    const WAITING: u32 = FLAG_IN_MATCH;
    const READY: u32 = FLAG_IN_MATCH | FLAG_TURN_RECEIVED;

    #[test]
    fn only_humans_in_the_match_without_their_turn_are_listed() {
        let seats = [
            seat(0, 0, READY, "Host"),
            seat(0, 1, WAITING, "Ann"),
            seat(3, 2, WAITING, "Empty"),
            seat(0, 3, 0, "Gone"),
            seat(0, 4, WAITING, "Bob"),
            seat(1, 5, WAITING, "Computer"),
            seat(0, 6, WAITING, ""),
        ];
        assert_eq!(
            waiting(&seats),
            vec![(1, "Ann".to_owned()), (4, "Bob".to_owned())]
        );
    }

    #[test]
    fn nobody_waiting_is_an_empty_list() {
        assert!(waiting(&[seat(0, 0, READY, "Host")]).is_empty());
        assert!(waiting(&[]).is_empty());
    }

    #[test]
    fn a_name_is_cut_at_the_nul_and_stripped_of_control_characters() {
        assert_eq!(clean_name(b"Ann\0garbage"), "Ann");
        assert_eq!(clean_name(b"A\x1b[31mn\nn\0"), "A[31mnn");
        assert_eq!(clean_name(b"  Bob  \0"), "Bob");
        assert_eq!(clean_name(b"\0"), "");
        let long = [b'x'; 40];
        assert_eq!(clean_name(&long).len(), NAME_MAX);
    }

    #[test]
    fn the_wait_is_minutes_and_two_digit_seconds() {
        assert_eq!(line("Ann", 0.0), "Ann - 0:00");
        assert_eq!(line("Ann", 7.9), "Ann - 0:07");
        assert_eq!(line("Ann", 65.0), "Ann - 1:05");
        assert_eq!(line("Ann", -3.0), "Ann - 0:00");
    }

    fn names(rows: &[(String, f64)]) -> Vec<(&str, u32)> {
        rows.iter().map(|(n, s)| (n.as_str(), *s as u32)).collect()
    }

    #[test]
    fn each_player_is_timed_from_when_they_first_held_things_up() {
        let mut w = Waits::new();
        let ann = vec![(1usize, "Ann".to_owned())];
        let both = vec![(1usize, "Ann".to_owned()), (4usize, "Bob".to_owned())];
        assert_eq!(names(&w.update(10_000.0, &ann)), vec![("Ann", 0)]);
        assert_eq!(names(&w.update(10_500.0, &ann)), vec![("Ann", 0)]);
        assert_eq!(names(&w.update(12_000.0, &ann)), vec![("Ann", 2)]);
        // Bob starts holding things up two seconds in; Ann's clock keeps running.
        assert_eq!(
            names(&w.update(12_500.0, &both)),
            vec![("Ann", 2), ("Bob", 0)]
        );
        assert_eq!(
            names(&w.update(14_000.0, &both)),
            vec![("Ann", 4), ("Bob", 1)]
        );
    }

    #[test]
    fn a_player_who_catches_up_is_forgotten_and_starts_again_if_they_stall_again() {
        let mut w = Waits::new();
        let ann = vec![(1usize, "Ann".to_owned())];
        let bob = vec![(4usize, "Bob".to_owned())];
        w.update(0.0, &ann);
        w.update(1_000.0, &ann);
        w.update(1_100.0, &bob);
        assert_eq!(names(&w.update(2_000.0, &ann)), vec![("Ann", 0)]);
    }

    #[test]
    fn the_clocks_restart_when_the_screen_has_been_away() {
        let mut w = Waits::new();
        let ann = vec![(1usize, "Ann".to_owned())];
        w.update(0.0, &ann);
        w.update(1_000.0, &ann);
        assert_eq!(names(&w.update(60_000.0, &ann)), vec![("Ann", 0)]);
    }
}
