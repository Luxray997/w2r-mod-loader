//! Minimap pings (docs/minimap-ping.md): in a match, Alt+G and then a click (or Ctrl+Alt+click)
//! marks a spot on the minimap and on the map for your allies and for observers. The click is
//! swallowed, so it does not move the camera or give an order. A ping travels as one small
//! message of our own subsystem (`'G'`) straight to the teammates who run the mod, over the
//! game's reliable channel; players without the mod are never sent anything.
//!
//! The wire format, the tile arithmetic, the list of live pings, who may see a ping and the
//! sound are pure and tested here; the game side is x86 only.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::hostcfg::HostConfig;
use crate::peer;
use crate::screenkit::Rect;

/// The envelope's subsystem byte.
pub const SUBSYSTEM: u8 = b'G';
const PROTOCOL: u8 = 1;
/// How long a ping stays on screen.
pub const LIFETIME_MS: u64 = 4_000;
/// Two pings from one player closer together than this are one (the second is dropped).
pub const MIN_GAP_MS: u64 = 400;
/// How long Alt+G waits for the click.
pub const ARM_MS: u64 = 6_000;
/// The minimap picture is 128 pixels square whatever the map size.
pub const MINIMAP_PX: i32 = 128;
/// World pixels per tile.
pub const TILE_SHIFT: u32 = 5;

/// A whole ping message: the envelope, the protocol, the tile.
pub fn encode(x: u8, y: u8) -> [u8; 8] {
    [
        peer::TYPE,
        peer::MAGIC[0],
        peer::MAGIC[1],
        peer::MAGIC[2],
        SUBSYSTEM,
        PROTOCOL,
        x,
        y,
    ]
}

/// The tile in a ping's body (everything after the envelope). Longer bodies are accepted so a
/// later version can append fields; another protocol or a tile off the largest map is refused.
pub fn decode(body: &[u8]) -> Option<(u8, u8)> {
    match body {
        [PROTOCOL, x, y, ..] if *x < 128 && *y < 128 => Some((*x, *y)),
        _ => None,
    }
}

/// The tile under a point of the minimap picture. `rect` is the picture's click rectangle
/// `[x0, y0, x1, y1]` (inclusive, screen pixels); `side` the map's side in tiles.
pub fn minimap_tile(mx: i32, my: i32, rect: [i32; 4], side: i32) -> Option<(u8, u8)> {
    if !(1..=128).contains(&side) {
        return None;
    }
    let [x0, y0, x1, y1] = rect;
    if mx < x0 || mx > x1 || my < y0 || my > y1 {
        return None;
    }
    let tile = |p: i32, from: i32| ((p - from) * side / MINIMAP_PX).clamp(0, side - 1) as u8;
    Some((tile(mx, x0), tile(my, y0)))
}

/// The tile under a point of the map view: the game's own `(mouse - origin + scroll) >> 5`.
pub fn view_tile(
    mx: i32,
    my: i32,
    rect: [i32; 4],
    origin: (i32, i32),
    scroll: (i32, i32),
    side: i32,
) -> Option<(u8, u8)> {
    let [x0, y0, x1, y1] = rect;
    if mx < x0 || mx > x1 || my < y0 || my > y1 {
        return None;
    }
    let wx = mx - origin.0 + scroll.0;
    let wy = my - origin.1 + scroll.1;
    if wx < 0 || wy < 0 {
        return None;
    }
    let (tx, ty) = (wx >> TILE_SHIFT, wy >> TILE_SHIFT);
    (tx < side && ty < side).then_some((tx as u8, ty as u8))
}

/// Where a tile's centre is on the minimap picture, as an offset from its corner.
pub fn minimap_offset(tile: u8, side: i32) -> f32 {
    (f32::from(tile) + 0.5) * MINIMAP_PX as f32 / side.max(1) as f32
}

/// Where a tile's centre is on the screen.
pub fn view_point(tile: (u8, u8), origin: (i32, i32), scroll: (i32, i32)) -> (f32, f32) {
    let c = |t: u8, o: i32, s: i32| (o + (i32::from(t) << TILE_SHIFT) + (1 << (TILE_SHIFT - 1)) - s) as f32;
    (c(tile.0, origin.0, scroll.0), c(tile.1, origin.1, scroll.1))
}

/// Whether the local player may see a ping from `sender`. `stance` is the game's alliance entry
/// of the sender towards the local player; `observers` the bit mask of observing players.
/// Allies see each other's pings; observers see everyone's and send none, so they cannot
/// whisper to a team.
pub fn accepts(sender: u8, local: u8, stance: u8, observers: u8) -> bool {
    if sender >= 8 || local >= 8 || sender == local {
        return false;
    }
    if observers & (1 << sender) != 0 {
        return false;
    }
    observers & (1 << local) != 0 || stance != 0
}

/// One ping on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ping {
    pub player: u8,
    pub x: u8,
    pub y: u8,
    pub born_ms: u64,
}

/// The pings alive now: one per player (a new one replaces the old).
#[derive(Debug, Default)]
pub struct Pings {
    list: Vec<Ping>,
}

impl Pings {
    pub const fn new() -> Pings {
        Pings { list: Vec::new() }
    }

    /// Adds a ping; false when the same player pinged less than [`MIN_GAP_MS`] ago.
    pub fn add(&mut self, player: u8, x: u8, y: u8, now: u64) -> bool {
        if self
            .list
            .iter()
            .any(|p| p.player == player && now.saturating_sub(p.born_ms) < MIN_GAP_MS)
        {
            return false;
        }
        self.list.retain(|p| p.player != player);
        self.list.push(Ping { player, x, y, born_ms: now });
        true
    }

    /// The pings still alive at `now`; the rest are forgotten.
    pub fn live(&mut self, now: u64) -> &[Ping] {
        self.list.retain(|p| now.saturating_sub(p.born_ms) < LIFETIME_MS);
        &self.list
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }
}

/// How a ping looks `age_ms` after it appeared, like a sonar: two outlined circles, half a pulse
/// apart, each growing from the middle to its full size while it fades out, then starting over.
/// Each is its size as a fraction of the largest and its opacity; the whole ping fades out
/// over its last half.
pub fn sonar(age_ms: u64) -> [(f32, f32); 2] {
    const PULSE_MS: u64 = 1_200;
    let left = LIFETIME_MS.saturating_sub(age_ms) as f32 / LIFETIME_MS as f32;
    let fade = (left * 2.0).min(1.0);
    [0, PULSE_MS / 2].map(|offset| {
        let phase = ((age_ms + offset) % PULSE_MS) as f32 / PULSE_MS as f32;
        (0.08 + 0.92 * phase, (1.0 - phase) * fade)
    })
}

/// The dots of a circle of `diameter` centred on (`cx`, `cy`): `count` squares of side `thick`,
/// each given by its top-left corner (drawn rounded, they read as a ring).
pub fn ring_dots(cx: f32, cy: f32, diameter: f32, thick: f32, count: usize) -> Vec<Rect> {
    let r = (diameter - thick) / 2.0;
    (0..count)
        .map(|i| {
            let a = i as f32 / count as f32 * core::f32::consts::TAU;
            Rect::new(cx + r * a.cos() - thick / 2.0, cy + r * a.sin() - thick / 2.0, thick, thick)
        })
        .collect()
}

/// Alt+G armed the next click as a ping.
#[derive(Debug, Default)]
pub struct Arm {
    until: Option<u64>,
}

impl Arm {
    pub const fn new() -> Arm {
        Arm { until: None }
    }

    /// Alt+G: arms, or disarms when already armed.
    pub fn toggle(&mut self, now: u64) {
        self.until = if self.armed(now) { None } else { Some(now + ARM_MS) };
    }

    pub fn armed(&self, now: u64) -> bool {
        self.until.is_some_and(|t| now < t)
    }

    pub fn clear(&mut self) {
        self.until = None;
    }
}

/// The ping sound as a stereo WAV: two soft sine notes, about C5 and G5 (a fifth apart), that
/// swell and ring out in an echo-like pattern and drift from the middle to the left. The
/// loudness of each note in each ear over time is a table measured from a reference sound
/// (`pingtable`); the notes themselves are generated here.
pub fn blip_wav() -> Vec<u8> {
    use crate::pingtable as table;
    const RATE: u32 = 22_050;
    const LOW_HZ: f32 = 522.0;
    const HIGH_HZ: f32 = 799.0;
    /// The table's peak is a quarter of full scale; this brings the loudest moment to about half.
    const GAIN: f32 = 2.0;
    let tau = core::f32::consts::TAU;
    let points = table::LOW_LEFT.len();
    let total = (points - 1) * table::STEP_MS * RATE as usize / 1000;
    // The loudness at time `t` seconds: the table decoded and joined with straight lines.
    let level = |column: &[u8; 424], t: f32| -> f32 {
        let pos = t * 1000.0 / table::STEP_MS as f32;
        let i = (pos as usize).min(points - 2);
        let frac = (pos - i as f32).clamp(0.0, 1.0);
        let decode = |q: u8| (f32::from(q) / 255.0).powi(2) * table::PEAK;
        decode(column[i]) * (1.0 - frac) + decode(column[i + 1]) * frac
    };
    let mut samples: Vec<i16> = Vec::with_capacity(total * 2);
    for i in 0..total {
        let t = i as f32 / RATE as f32;
        let (low, high) = ((t * LOW_HZ * tau).sin(), (t * HIGH_HZ * tau).sin());
        // A short fade at the very end.
        let end = ((total - i) as f32 / (RATE as f32 * 0.02)).min(1.0);
        let ear = |low_column, high_column| {
            let v = (low * level(low_column, t) + high * level(high_column, t)) * GAIN * end;
            (v * f32::from(i16::MAX)).clamp(-32767.0, 32767.0) as i16
        };
        samples.push(ear(&table::LOW_LEFT, &table::HIGH_LEFT));
        samples.push(ear(&table::LOW_RIGHT, &table::HIGH_RIGHT));
    }
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

static ENABLED: AtomicBool = AtomicBool::new(true);
/// The net slots (bit n) that announced the handshake in the lobby just left.
static ROSTER: AtomicU8 = AtomicU8::new(0);

/// Takes the settings (at startup and whenever the user changes one; they apply at once).
pub fn configure(cfg: &HostConfig) {
    ENABLED.store(cfg.minimap_ping, Ordering::Release);
}

/// Remembers which net slots run the mod; called every lobby frame, because the handshake's
/// roster is cleared when the lobby is left and pings travel during the match.
pub fn note_roster(mask: u8) {
    ROSTER.store(mask, Ordering::Release);
}

/// The net slots (bit n) that announced the handshake in the lobby just left.
pub fn roster() -> u8 {
    ROSTER.load(Ordering::Acquire)
}

#[cfg(target_arch = "x86")]
pub use live::{before_hud, deliver, install, on_match_start, poll};

#[cfg(target_arch = "x86")]
mod live {
    use core::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    use super::*;
    use crate::chat::now_ms;
    use crate::guard::guarded;
    use crate::host::Log;
    use crate::hooks::{self, Owner};
    use crate::hudpanel::BeginFn;
    use crate::pe::Image;
    use crate::replayplay;
    use crate::screenkit::game::{Kit, rgba};
    use crate::screenkit::{ALIGN_CENTRE};
    use crate::symbols::Symbols;

    type SendFn = unsafe extern "C" fn(u32, *const u8, u32);

    /// The game state while a match is being played.
    const STATE_IN_MATCH: u16 = 3;
    /// `TurnSlotFlags` bit: in the match.
    const FLAG_IN_MATCH: u32 = 0x1_0000;
    const NO_SEAT: u32 = 8;
    /// The lobby slot table: stride, and where the team byte sits.
    const SLOT_STRIDE: usize = 0x26;
    const SLOT_TEAM: usize = 0x0a;
    const OBSERVER_TEAM: u8 = 9;
    const OWNER_HUMAN: u8 = 0;
    /// `DiplomacyStance` is indexed `[player * 16 + other]`.
    const STANCE_STRIDE: usize = 16;

    const VK_LBUTTON: i32 = 0x01;
    const VK_ESCAPE: i32 = 0x1B;
    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;
    const VK_G: i32 = 0x47;

    const WINDOW_FLAGS: u32 = 0x120;
    const HINT: u32 = rgba(255, 214, 64, 255);

    struct Game {
        state: usize,
        networked: usize,
        local_player: usize,
        player_by_net: usize,
        lobby_by_net: usize,
        turn_flags: usize,
        left_flags: usize,
        owner: usize,
        send: SendFn,
        mouse: usize,
        handler_slots: usize,
        no_op: usize,
        view_origin: usize,
        view_rect: usize,
        minimap_rect: usize,
        camera: usize,
        map_side: usize,
        stance: usize,
        lobby_slots: usize,
        colour_reorder: usize,
        colour_rgba: usize,
        kit: Kit,
    }

    // SAFETY: plain game function and data addresses, fixed for the session.
    unsafe impl Send for Game {}
    unsafe impl Sync for Game {}

    #[derive(Default)]
    struct State {
        pings: Pings,
        arm: Arm,
        observers: u8,
        prev_g: bool,
        prev_esc: bool,
        prev_button: bool,
        /// A ping was fired with this button press; keep swallowing until it is released.
        swallow_release: bool,
    }

    static GAME: OnceLock<Game> = OnceLock::new();
    static STATE: Mutex<Option<State>> = Mutex::new(None);
    static SOUND: OnceLock<Vec<u8>> = OnceLock::new();
    /// Clicks are being kept from the game right now.
    static SWALLOW: AtomicBool = AtomicBool::new(false);
    static PICK_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static LOGGED_PING: AtomicU64 = AtomicU64::new(0);

    fn log(level: u32, text: &str) {
        crate::session::StartupLog.line(level, "ping", text);
    }

    pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
        let get = |n: &str| symbols.address(n);
        let (
            Some(state),
            Some(networked),
            Some(local_player),
            Some(player_by_net),
            Some(lobby_by_net),
            Some(turn_flags),
            Some(left_flags),
            Some(owner),
            Some(send),
            Some(mouse),
            Some(handler_slots),
            Some(no_op),
            Some(view_origin),
            Some(view_rect),
            Some(minimap_rect),
            Some(camera),
            Some(map_side),
            Some(stance),
            Some(lobby_slots),
            Some(colour_reorder),
            Some(colour_rgba),
            Some(pick),
            Some(draw_image),
            Some(texture0),
            Some(texture1),
        ) = (
            get("GameStateCurrent"),
            get("NetworkedMatch"),
            get("LocalPlayer"),
            get("NetSlotPlayer"),
            get("LobbySlotByNetSlot"),
            get("TurnSlotFlags"),
            get("PeerLeftFlags"),
            get("PlayerOwner"),
            get("SendToNetSlot"),
            get("MouseCursorPos"),
            get("MouseHandlerSlots"),
            get("NoOpHandler"),
            get("ViewportOrigin"),
            get("ViewportRect"),
            get("MinimapClickRect"),
            get("CameraScrollX"),
            get("MapSide"),
            get("DiplomacyStance"),
            get("MpLobbySlots"),
            get("TeamColourReorder"),
            get("TeamColourRgba"),
            get("MousePickHandlers"),
            get("HudDrawImage"),
            get("HudMinimapTexture0"),
            get("HudMinimapTexture1"),
        )
        else {
            log.line(W2R_LOG_ERROR, "host", "minimap ping: symbols missing");
            return false;
        };
        let Some(kit) = Kit::new_in(symbols, "HudCtx") else {
            log.line(W2R_LOG_ERROR, "host", "minimap ping: screen-kit symbols missing");
            return false;
        };
        // SAFETY: `SendToNetSlot(netSlot, message, length)` (Ghidra, build 2818), as the host kick uses it.
        let send = unsafe { core::mem::transmute::<usize, SendFn>(send) };
        let _ = GAME.set(Game {
            state,
            networked,
            local_player,
            player_by_net,
            lobby_by_net,
            turn_flags,
            left_flags,
            owner,
            send,
            mouse,
            handler_slots,
            no_op,
            view_origin,
            view_rect,
            minimap_rect,
            camera,
            map_side,
            stance,
            lobby_slots,
            colour_reorder,
            colour_rgba,
            kit,
        });
        // The game picks the click handlers again on every mouse event, the click's own included,
        // so setting them once is not enough: put the no-op back right after each pick.
        // SAFETY: `pick_detour` matches the routine's cdecl, no-argument signature (Ghidra, build 2818).
        let hooked = unsafe {
            hooks::global().install(
                Owner::Host,
                image,
                pick,
                pick_detour as *mut c_void,
                "host:MousePickHandlers",
                |original| PICK_ORIGINAL.store(original as usize, Ordering::Release),
            )
        };
        if let Err(e) = hooked {
            log.line(W2R_LOG_ERROR, "host", &format!("minimap ping: could not hook the click handlers: {e:?}"));
            return false;
        }
        MINIMAP_TEXTURE0.store(texture0, Ordering::Release);
        MINIMAP_TEXTURE1.store(texture1, Ordering::Release);
        // SAFETY: `draw_image_detour` matches `HudDrawImage(ctx, NkImage)`, cdecl, as the chat plugin calls it.
        let image_hook = unsafe {
            hooks::global().install(
                Owner::Host,
                image,
                draw_image,
                draw_image_detour as *mut c_void,
                "host:HudDrawImage",
                |original| DRAW_IMAGE_ORIGINAL.store(original as usize, Ordering::Release),
            )
        };
        if let Err(e) = image_hook {
            log.line(W2R_LOG_ERROR, "host", &format!("minimap ping: could not hook the minimap draw: {e:?}"));
        }
        log.line(W2R_LOG_INFO, "host", "minimap ping ready (Alt+G, then click)");
        true
    }

    unsafe extern "C" fn pick_detour() {
        let original: unsafe extern "C" fn() =
            unsafe { core::mem::transmute(PICK_ORIGINAL.load(Ordering::Acquire)) };
        unsafe { original() };
        if SWALLOW.load(Ordering::Acquire)
            && let Some(g) = GAME.get()
        {
            let _ = guarded(|| g.swallow_clicks());
        }
    }

    impl Game {
        fn u8_at(&self, base: usize, index: usize) -> u8 {
            // SAFETY: small per-player tables in the game's data section; callers use `guarded`.
            unsafe { core::ptr::read_unaligned((base + index) as *const u8) }
        }

        fn u32_at(&self, base: usize, index: usize) -> u32 {
            // SAFETY: per-slot tables of at least 8 entries in the game's data section.
            unsafe { core::ptr::read_unaligned((base + index * 4) as *const u32) }
        }

        fn i16_at(&self, base: usize, index: usize) -> i32 {
            // SAFETY: small rectangles in the game's data section.
            i32::from(unsafe { core::ptr::read_unaligned((base + index * 2) as *const i16) })
        }

        fn i32_at(&self, base: usize, index: usize) -> i32 {
            // SAFETY: as above.
            unsafe { core::ptr::read_unaligned((base + index * 4) as *const i32) }
        }

        /// A live match (not a replay).
        fn in_match(&self) -> bool {
            if replayplay::is_active() {
                return false;
            }
            // SAFETY: the state is a u16 in the game's data section.
            unsafe { core::ptr::read_unaligned(self.state as *const u16) == STATE_IN_MATCH }
        }

        fn local(&self) -> u8 {
            self.u8_at(self.local_player, 0)
        }

        fn side(&self) -> i32 {
            i32::from(self.u8_at(self.map_side, 0))
        }

        fn mouse(&self) -> (i32, i32) {
            // SAFETY: two u16 in the game's data section.
            unsafe {
                (
                    i32::from(core::ptr::read_unaligned(self.mouse as *const u16)),
                    i32::from(core::ptr::read_unaligned((self.mouse + 2) as *const u16)),
                )
            }
        }

        fn rect(&self, base: usize) -> [i32; 4] {
            [
                self.i16_at(base, 0),
                self.i16_at(base, 1),
                self.i16_at(base, 2),
                self.i16_at(base, 3),
            ]
        }

        fn scroll(&self) -> (i32, i32) {
            (self.i32_at(self.camera, 0), self.i32_at(self.camera, 1))
        }

        fn origin(&self) -> (i32, i32) {
            (self.i32_at(self.view_origin, 0), self.i32_at(self.view_origin, 1))
        }

        /// The tile under the mouse, on the minimap or the map view.
        fn tile_at_mouse(&self) -> Option<(u8, u8)> {
            let (mx, my) = self.mouse();
            let side = self.side();
            minimap_tile(mx, my, self.rect(self.minimap_rect), side)
                .or_else(|| view_tile(mx, my, self.rect(self.view_rect), self.origin(), self.scroll(), side))
        }

        /// Points the game's click handlers at its own do-nothing routine, so a click does not
        /// order or select anything. The game picks the handlers again at the next mouse move.
        fn swallow_clicks(&self) {
            for i in 0..4 {
                // SAFETY: four handler pointers in the game's data section; the no-op is a
                // routine that takes nothing and returns.
                unsafe { core::ptr::write_unaligned((self.handler_slots + i * 4) as *mut usize, self.no_op) };
            }
        }

        /// The net slots a ping goes to: those that run the mod, are in the match and are
        /// people, apart from ourselves.
        fn recipients(&self) -> Vec<u32> {
            let roster = ROSTER.load(Ordering::Acquire);
            let me = self.local();
            (0..8usize)
                .filter(|&n| {
                    if roster & (1 << n) == 0 || self.u32_at(self.lobby_by_net, n) & 0xff >= NO_SEAT {
                        return false;
                    }
                    if self.u32_at(self.turn_flags, n) & FLAG_IN_MATCH == 0
                        || self.u32_at(self.left_flags, n) != 0
                    {
                        return false;
                    }
                    let player = (self.u32_at(self.player_by_net, n) & 0xff) as u8;
                    player < 8 && player != me && self.u8_at(self.owner, usize::from(player)) == OWNER_HUMAN
                })
                .map(|n| n as u32)
                .collect()
        }

        fn colour(&self, player: u8) -> (u8, u8, u8) {
            let index = usize::from(self.u8_at(self.colour_reorder, usize::from(player) % 8));
            if index >= 8 {
                return (150, 150, 150);
            }
            let f = |k: usize| {
                // SAFETY: the colour table is four floats per colour index.
                unsafe { core::ptr::read_unaligned((self.colour_rgba + (index * 4 + k) * 4) as *const f32) }
            };
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            (byte(f(0)), byte(f(1)), byte(f(2)))
        }
    }

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
        let mut guard = STATE.lock().ok()?;
        Some(f(guard.get_or_insert_with(State::default)))
    }

    fn play() {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        let wav = SOUND.get_or_init(blip_wav);
        // SAFETY: the WAV lives in a static for the rest of the process, as SND_ASYNC with
        // SND_MEMORY requires.
        unsafe {
            PlaySoundW(wav.as_ptr().cast(), core::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }

    /// A new match: nobody has pinged yet, and the lobby tells who observes.
    pub fn on_match_start() {
        let Some(g) = GAME.get() else { return };
        let observers = guarded(|| {
            (0..8usize)
                .filter(|&i| {
                    g.u8_at(g.lobby_slots, i * SLOT_STRIDE + SLOT_TEAM) == OBSERVER_TEAM
                        && g.u8_at(g.owner, i) == OWNER_HUMAN
                })
                .fold(0u8, |m, i| m | 1 << i)
        })
        .unwrap_or(0);
        with_state(|s| *s = State { observers, ..State::default() });
    }

    /// Once per game frame: Alt+G, the click, and keeping the click away from the game.
    pub fn poll() {
        let Some(g) = GAME.get() else { return };
        if let Err(fault) = guarded(|| poll_inner(g)) {
            crate::session::host_fault("minimap ping", &fault);
        }
    }

    fn held(vk: i32) -> bool {
        // SAFETY: a plain key-state query.
        (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
    }

    fn poll_inner(g: &Game) {
        let live = ENABLED.load(Ordering::Acquire) && g.in_match() && replayplay::game_has_focus();
        let now = now_ms();
        if !live {
            SWALLOW.store(false, Ordering::Release);
            with_state(|s| {
                s.arm.clear();
                s.prev_g = false;
                s.prev_esc = false;
                s.prev_button = false;
                s.swallow_release = false;
            });
            return;
        }
        let (alt, ctrl, key_g, esc, button) =
            (held(VK_MENU), held(VK_CONTROL), held(VK_G), held(VK_ESCAPE), held(VK_LBUTTON));
        let me = g.local();
        let cannot_ping = with_state(|s| s.observers & (1 << me) != 0).unwrap_or(true);
        if cannot_ping {
            SWALLOW.store(false, Ordering::Release);
            return;
        }
        let swallow = with_state(|s| {
            if key_g && !s.prev_g && alt && !ctrl {
                s.arm.toggle(now);
            }
            if esc && !s.prev_esc {
                s.arm.clear();
            }
            s.prev_g = key_g;
            s.prev_esc = esc;
            let armed = s.arm.armed(now);
            let combo = alt && ctrl;
            let swallow = armed || combo || s.swallow_release;
            let pressed = button && !s.prev_button;
            s.prev_button = button;
            if !button {
                s.swallow_release = false;
            }
            (swallow, pressed && (armed || combo), armed)
        });
        let Some((swallow, click, armed)) = swallow else { return };
        SWALLOW.store(swallow, Ordering::Release);
        if swallow {
            g.swallow_clicks();
        }
        if !click {
            return;
        }
        let Some((x, y)) = g.tile_at_mouse() else { return };
        with_state(|s| {
            if armed {
                s.arm.clear();
            }
            s.swallow_release = true;
        });
        fire(g, x, y, now);
    }

    /// The local player pings a tile: shown here at once, sent to the teammates who run the mod.
    fn fire(g: &Game, x: u8, y: u8, now: u64) {
        let me = g.local();
        if !with_state(|s| s.pings.add(me, x, y, now)).unwrap_or(false) {
            return;
        }
        play();
        let targets = if g.u8_at(g.networked, 0) != 0 { g.recipients() } else { Vec::new() };
        let msg = encode(x, y);
        let mut sent = 0;
        for slot in targets {
            // Only teammates need it, and the observers (who see everything).
            let player = (g.u32_at(g.player_by_net, slot as usize) & 0xff) as u8;
            let stance = g.u8_at(g.stance, usize::from(me) * STANCE_STRIDE + usize::from(player));
            let observer = with_state(|s| s.observers & (1 << player) != 0).unwrap_or(false);
            if stance == 0 && !observer {
                continue;
            }
            // SAFETY: the game's own send, a reliable message to one connected peer.
            unsafe { (g.send)(slot, msg.as_ptr(), msg.len() as u32) };
            sent += 1;
        }
        log(
            W2R_LOG_INFO,
            &format!(
                "ping at tile {x},{y} (map side {}, scroll {:?}, origin {:?}), sent to {sent}",
                g.side(),
                g.scroll(),
                g.origin()
            ),
        );
    }

    /// A ping arrived from net slot `sender` (called from the packet hook; `body` is what follows
    /// the envelope).
    pub fn deliver(sender: u32, body: &[u8]) {
        let Some(g) = GAME.get() else { return };
        if !ENABLED.load(Ordering::Acquire) || !g.in_match() || sender >= 8 {
            return;
        }
        let Some((x, y)) = decode(body) else { return };
        if i32::from(x) >= g.side() || i32::from(y) >= g.side() {
            return;
        }
        let player = (g.u32_at(g.player_by_net, sender as usize) & 0xff) as u8;
        let me = g.local();
        if player >= 8 || me >= 8 {
            return;
        }
        let stance = g.u8_at(g.stance, usize::from(player) * STANCE_STRIDE + usize::from(me));
        let shown = with_state(|s| {
            accepts(player, me, stance, s.observers) && s.pings.add(player, x, y, now_ms())
        })
        .unwrap_or(false);
        if shown {
            play();
        }
    }

    /// Draws the live pings on the minimap and on the map view, and the hint while armed. Called
    /// by the window hook just before the game starts its `game_hud` window.
    ///
    /// # Safety
    /// `ctx` is the context the game is about to begin `game_hud` in; `begin` is the game's own
    /// `BeginNamedWindow`.
    pub unsafe fn before_hud(ctx: *mut c_void, begin: BeginFn) {
        let Some(g) = GAME.get() else { return };
        if !ENABLED.load(Ordering::Acquire) {
            return;
        }
        let result = guarded(|| unsafe { draw(g, ctx, begin) });
        if let Err(fault) = result {
            crate::session::host_fault("ping overlay", &fault);
        }
    }

    /// One ping as sonar rings: outlined circles drawn as a ring of small round dots.
    fn paint_ping(g: &Game, p: &Ping, cx: f32, cy: f32, max: f32, thick: f32, now: u64) {
        let (r, gr, b) = g.colour(p.player);
        for (size, alpha) in sonar(now.saturating_sub(p.born_ms)) {
            if alpha < 0.03 {
                continue;
            }
            let col = rgba(r, gr, b, (alpha * 255.0) as u8);
            let diameter = max * size;
            let count = (core::f32::consts::PI * diameter / (thick * 0.7)).clamp(12.0, 120.0) as usize;
            for dot in ring_dots(cx, cy, diameter, thick, count) {
                g.kit.fill_round(dot, thick / 2.0, col);
            }
        }
    }

    type DrawImageFn = unsafe extern "C" fn(*mut c_void, NkImage);
    type TextureFn = unsafe extern "C" fn() -> *mut c_void;

    /// The game's image argument (`NkImage`), passed by value.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NkImage {
        handle: *mut c_void,
        w: u16,
        h: u16,
        region: [u16; 4],
    }

    static DRAW_IMAGE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    /// `HudMinimapTexture0` (a pointer variable) and `HudMinimapTexture1` (a getter): the two
    /// textures the HUD draws the minimap from.
    static MINIMAP_TEXTURE0: AtomicUsize = AtomicUsize::new(0);
    static MINIMAP_TEXTURE1: AtomicUsize = AtomicUsize::new(0);

    /// Draws the image, then, if it is the minimap, the pings on top of it: an overlay window
    /// opened before the HUD is covered by the minimap picture, so the markers have to be drawn
    /// inside the HUD's own window, right after the picture.
    unsafe extern "C" fn draw_image_detour(ctx: *mut c_void, img: NkImage) {
        let original: DrawImageFn =
            unsafe { core::mem::transmute(DRAW_IMAGE_ORIGINAL.load(Ordering::Acquire)) };
        unsafe { original(ctx, img) };
        if !ENABLED.load(Ordering::Acquire) || img.handle.is_null() {
            return;
        }
        let _ = guarded(|| unsafe { minimap_pings(img.handle) });
    }

    /// # Safety
    /// Main thread, while the HUD draws; the texture addresses are the game's.
    unsafe fn minimap_pings(handle: *mut c_void) {
        let Some(g) = GAME.get() else { return };
        let now = now_ms();
        // Cheap tests first: most images drawn are not the minimap and most frames have no ping.
        let Some(pings) = with_state(|s| s.pings.live(now).to_vec()) else { return };
        if pings.is_empty() || !g.in_match() {
            return;
        }
        let tex0 = unsafe { core::ptr::read_unaligned(MINIMAP_TEXTURE0.load(Ordering::Acquire) as *const usize) };
        let getter = MINIMAP_TEXTURE1.load(Ordering::Acquire);
        let tex1 = if getter == 0 {
            0
        } else {
            unsafe { core::mem::transmute::<usize, TextureFn>(getter)() as usize }
        };
        if handle as usize != tex0 && handle as usize != tex1 {
            return;
        }
        let m = g.rect(g.minimap_rect);
        let side = g.side().max(1);
        for p in &pings {
            let cx = m[0] as f32 + minimap_offset(p.x, side);
            let cy = m[1] as f32 + minimap_offset(p.y, side);
            paint_ping(g, p, cx, cy, 26.0, 2.0, now);
        }
    }

    unsafe fn draw(g: &Game, ctx: *mut c_void, begin: BeginFn) {
        if !g.in_match() {
            return;
        }
        let now = now_ms();
        let Some((pings, armed, observers)) = with_state(|s| (s.pings.live(now).to_vec(), s.arm.armed(now), s.observers))
        else {
            return;
        };
        if pings.is_empty() && !armed {
            return;
        }
        let _ = observers;

        let m = g.rect(g.minimap_rect);
        if let Some(newest) = pings.iter().map(|p| p.born_ms).max()
            && LOGGED_PING.swap(newest, Ordering::AcqRel) != newest
        {
            log(
                W2R_LOG_INFO,
                &format!(
                    "overlay: minimap rect {m:?}, view rect {:?}, {} ping(s)",
                    g.rect(g.view_rect),
                    pings.len()
                ),
            );
        }

        let v = g.rect(g.view_rect);
        if v[2] > v[0] && v[3] > v[1] {
            let (w, h) = ((v[2] - v[0] + 1) as f32, (v[3] - v[1] + 1) as f32);
            if unsafe { begin(ctx, c"ping_view".as_ptr(), v[0] as f32, v[1] as f32, w, h, WINDOW_FLAGS) } != 0 {
                g.kit.space_begin();
                let (origin, scroll) = (g.origin(), g.scroll());
                for p in &pings {
                    let (cx, cy) = view_point((p.x, p.y), origin, scroll);
                    if cx >= v[0] as f32 && cx <= v[2] as f32 && cy >= v[1] as f32 && cy <= v[3] as f32 {
                        paint_ping(g, p, cx, cy, 84.0, 3.0, now);
                    }
                }
                if armed {
                    g.kit.label_coloured_at(
                        Rect::new(w / 2.0 - 200.0, 22.0, 400.0, 16.0),
                        "Ping: click the map or the minimap (Esc cancels)",
                        ALIGN_CENTRE,
                        HINT,
                    );
                }
                g.kit.space_end();
                g.kit.end_window();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_round_trips_and_refuses_the_rest() {
        let msg = encode(12, 99);
        assert_eq!(&msg[..5], &[0xE7, b'W', b'2', b'R', b'G']);
        match peer::classify(&msg) {
            peer::Packet::Ours { subsystem, body } => {
                assert_eq!(subsystem, SUBSYSTEM);
                assert_eq!(decode(body), Some((12, 99)));
            }
            peer::Packet::NotOurs => panic!("ours"),
        }
        assert_eq!(decode(&[1, 5, 6, 7, 8]), Some((5, 6)), "extra bytes are for later versions");
        assert_eq!(decode(&[2, 5, 6]), None, "another protocol");
        assert_eq!(decode(&[1, 128, 0]), None, "off the largest map");
        assert_eq!(decode(&[1, 0, 200]), None);
        assert_eq!(decode(&[1, 0]), None);
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn the_minimap_picture_maps_to_tiles() {
        let rect = [24, 26, 151, 153];
        assert_eq!(minimap_tile(24, 26, rect, 128), Some((0, 0)));
        assert_eq!(minimap_tile(151, 153, rect, 128), Some((127, 127)));
        // A 32-tile map is drawn four pixels a tile.
        assert_eq!(minimap_tile(24 + 8, 26 + 4, rect, 32), Some((2, 1)));
        assert_eq!(minimap_tile(151, 153, rect, 32), Some((31, 31)));
        assert_eq!(minimap_tile(23, 30, rect, 32), None);
        assert_eq!(minimap_tile(30, 154, rect, 32), None);
        assert_eq!(minimap_tile(30, 30, rect, 0), None);
        assert_eq!(minimap_tile(30, 30, rect, 200), None);
    }

    #[test]
    fn the_map_view_maps_to_tiles_and_back() {
        let rect = [176, 16, 1263, 703];
        let origin = (176, 16);
        let scroll = (64, 96); // tile 2, 3 at the view's corner
        assert_eq!(view_tile(176, 16, rect, origin, scroll, 64), Some((2, 3)));
        assert_eq!(view_tile(176 + 32, 16 + 33, rect, origin, scroll, 64), Some((3, 4)));
        assert_eq!(view_tile(10, 100, rect, origin, scroll, 64), None, "in the sidebar");
        assert_eq!(view_tile(176, 16, rect, origin, (0, 0), 1), Some((0, 0)));
        // Past the map's far edge.
        assert_eq!(view_tile(1000, 400, rect, origin, (63 * 32, 0), 64), None);
        // The centre of a tile is where its ring is drawn.
        assert_eq!(view_point((2, 3), origin, scroll), (176.0 + 16.0, 16.0 + 16.0));
        assert_eq!(view_point((3, 3), origin, scroll), (176.0 + 48.0, 16.0 + 16.0));
    }

    #[test]
    fn minimap_offsets_stay_inside_the_picture() {
        assert_eq!(minimap_offset(0, 128), 0.5);
        assert_eq!(minimap_offset(0, 32), 2.0);
        assert_eq!(minimap_offset(31, 32), 126.0);
        assert!(minimap_offset(127, 128) < 128.0);
    }

    #[test]
    fn allies_and_observers_see_pings_enemies_do_not() {
        assert!(accepts(1, 0, 1, 0), "an ally");
        assert!(!accepts(1, 0, 0, 0), "an enemy");
        assert!(!accepts(0, 0, 1, 0), "yourself");
        assert!(!accepts(9, 0, 1, 0), "no such player");
        assert!(accepts(1, 5, 0, 1 << 5), "an observer sees everyone's");
        assert!(!accepts(5, 0, 1, 1 << 5), "an observer sends nothing to players");
        assert!(!accepts(5, 6, 1, (1 << 5) | (1 << 6)), "nor to other observers");
    }

    #[test]
    fn pings_live_for_a_while_one_per_player() {
        let mut p = Pings::new();
        assert!(p.add(1, 10, 10, 1_000));
        assert!(!p.add(1, 11, 11, 1_200), "too soon after the last");
        assert!(p.add(2, 20, 20, 1_200));
        assert_eq!(p.live(1_300).len(), 2);
        assert!(p.add(1, 12, 12, 1_500));
        let list = p.live(1_600).to_vec();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().find(|x| x.player == 1).unwrap().x, 12, "the new ping replaced the old");
        assert_eq!(p.live(1_200 + LIFETIME_MS).len(), 1, "player 2's is gone");
        assert!(p.live(1_500 + LIFETIME_MS).is_empty());
    }

    #[test]
    fn a_ring_grows_and_fades() {
        let [(s0, a0), (h0, _)] = sonar(0);
        let [(s1, a1), _] = sonar(700);
        assert!(s1 > s0 && a1 < a0, "grows and fades");
        assert!(h0 > s0, "the second ring is half a pulse ahead");
        let [(s2, _), _] = sonar(1_200);
        assert_eq!(s2, s0, "the pulse starts over");
        let [(_, late), (_, late2)] = sonar(LIFETIME_MS - 100);
        assert!(late < 0.1 && late2 < 0.1);
        let dots = ring_dots(50.0, 50.0, 20.0, 2.0, 4);
        assert_eq!(dots.len(), 4);
        // The first dot is on the right, the third on the left, both on the circle's edge.
        assert!((dots[0].x + 1.0 - 59.0).abs() < 0.01 && (dots[0].y + 1.0 - 50.0).abs() < 0.01);
        assert!((dots[2].x + 1.0 - 41.0).abs() < 0.01);
    }

    #[test]
    fn alt_g_arms_and_times_out() {
        let mut a = Arm::new();
        assert!(!a.armed(0));
        a.toggle(1_000);
        assert!(a.armed(1_000 + ARM_MS - 1));
        assert!(!a.armed(1_000 + ARM_MS));
        a.clear();
        a.toggle(2_000);
        a.toggle(2_100);
        assert!(!a.armed(2_200), "the second Alt+G cancels");
        a.toggle(3_000);
        a.clear();
        assert!(!a.armed(3_001));
    }

    #[test]
    fn the_blip_is_a_wav() {
        let wav = blip_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(wav.len(), 44 + data_len);
        let samples: Vec<i16> = wav[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let peak = samples.iter().map(|v| i32::from(*v).abs()).max().unwrap();
        println!("blip peak {peak}");
        assert!((8_000..30_000).contains(&peak), "audible and not clipping: {peak}");
        let tail = samples[samples.len() * 9 / 10..].iter().map(|v| i32::from(*v).abs()).max().unwrap();
        assert!(tail < peak / 20, "dies away");
    }
}
