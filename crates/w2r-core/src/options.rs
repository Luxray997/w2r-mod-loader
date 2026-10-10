//! The game's options screen, taken over for the plugin list: the host hooks
//! `BuildOptionsMenuScreen` and draws a small "Plugins" button over the game's own screen; while
//! the user is on the plugin screen the game's screen is not built at all and ours is, with the
//! game's own menu widgets and skins. Layout numbers are in the game's 2160-high UI space and
//! follow wc2r-plus's settings screen (MIT). x86 only.

use core::ffi::{c_char, c_void};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO};

use crate::guard::guarded;
use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::symbols::Symbols;
use crate::ui::Widgets;

pub const SYMBOL: &str = "BuildOptionsMenuScreen";
const UI_HEIGHT: f32 = 2160.0;
const PANEL_TOP: f32 = 320.0;
const PANEL_HEIGHT: f32 = 1580.0;
const PANEL_WIDTH_PERCENT: f32 = 60.0;
const TITLE_TOP: f32 = 150.0;
const TITLE_HEIGHT: f32 = 120.0;
const BUTTON_W: f32 = 380.0;
const BUTTON_H: f32 = 100.0;
const ALIGN_TITLE: i32 = 0x12;
const ALIGN_ROW: i32 = 0x11;
const WINDOW_NO_SCROLLBAR: u32 = 0x20;
const GROUP_PLAIN: u32 = 0;
/// A group without scrollbars, as the create screen's frames.
const GROUP_CARD: u32 = 0x20;
/// The edit box's flags and the row spacing offset in the game's Nuklear context, as the other
/// screens use them (`screenkit.rs`, wc2r-plus `struct_offsets.h`).
const TEXT_EDIT_FLAGS: u32 = 0x260;
const CTX_SPACING_Y: usize = 0x2004;
/// The plugin screen's breathing room: a left margin (a fraction of the row, taken by an empty
/// first cell) and the gap between rows (the context's item spacing, in UI units).
const ROW_INSET: f32 = 0.02;
const CARD_INSET: f32 = 0.05;
const ROW_GAP: f32 = crate::ui::ROW_GAP;

/// Column ratios for rows of 1 to 8 widgets after the margin cell. The game's layout keeps the
/// pointer it is given and reads it as each widget of the row is placed, so these must outlive
/// the call (a stack array was freed by then and the next widget faulted).
const fn ratio_table(inset: f32) -> [[f32; 10]; 8] {
    let mut table = [[0.0; 10]; 8];
    let mut i = 0;
    while i < 8 {
        let cols = i + 1;
        table[i][0] = inset;
        let mut c = 1;
        while c <= cols {
            table[i][c] = (1.0 - 2.0 * inset) / cols as f32;
            c += 1;
        }
        table[i][cols + 1] = inset;
        i += 1;
    }
    table
}
static ROW_RATIOS: [[f32; 10]; 8] = ratio_table(ROW_INSET);
/// Inside a plugin card the frame's border is drawn over the content, so the margin is wider.
static CARD_RATIOS: [[f32; 10]; 8] = ratio_table(CARD_INSET);
/// After this long without the options screen being built, the next build is a fresh entry.
const REENTRY_GAP_MS: u64 = 250;

type BuildOptionsMenuScreenFn = unsafe extern "C" fn(i32);
type BeginNamedWindowFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, f32, f32, f32, f32, u32) -> i32;
type CtxFn = unsafe extern "C" fn(*mut c_void);
type UiWidthFn = unsafe extern "C" fn() -> f32;
type RectFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32);
type SpaceBeginFn = unsafe extern "C" fn(*mut c_void, i32, f32, i32);
type BeginGroupFn = unsafe extern "C" fn(*mut c_void, *const c_char, u32) -> i32;
type RowFn = unsafe extern "C" fn(*mut c_void, f32, i32);
type RowRatiosFn = unsafe extern "C" fn(*mut c_void, i32, f32, i32, *const f32);
type LabelFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32);
type MenuButtonFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> i8;
type CheckboxFn = unsafe extern "C" fn(*const c_char, *mut u32, u32) -> u8;
type SliderFn = unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, *mut u32) -> u8;
type TextEditFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_char, i32, *mut c_void) -> u32;
type SetSkinFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> u32;

/// The game functions the screen needs, resolved once. Plain pointers, so copies are cheap and
/// the session need not stay borrowed while the screen draws.
#[derive(Clone, Copy)]
pub struct Nk {
    ctx_slot: *mut *mut c_void,
    begin_window: BeginNamedWindowFn,
    end_window: CtxFn,
    ui_width: UiWidthFn,
    rect: RectFn,
    space_begin: SpaceBeginFn,
    space_end: CtxFn,
    begin_group: BeginGroupFn,
    end_group: CtxFn,
    row: RowFn,
    row_ratios: RowRatiosFn,
    spacing: CtxFn,
    label: LabelFn,
    button: MenuButtonFn,
    checkbox: CheckboxFn,
    slider: SliderFn,
    disable_begin: CtxFn,
    disable_end: CtxFn,
    text_edit: TextEditFn,
    text_edit_filter: *mut c_void,
    skin_text_edit: *const u32,
    set_skin: SetSkinFn,
    skin_manager: *mut c_void,
    skin_window: *const u32,
    skin_window_alt: *const u32,
    skin_alt_flag: *const u8,
    skin_content: *const u32,
    skin_card: *const u32,
}

macro_rules! sym {
    ($symbols:expr, $name:literal, $t:ty) => {
        unsafe { core::mem::transmute::<usize, $t>($symbols.address($name)?) }
    };
}

impl Nk {
    pub fn new(symbols: &Symbols) -> Option<Nk> {
        Some(Nk {
            ctx_slot: symbols.address("NkContextMenus")? as *mut *mut c_void,
            begin_window: sym!(symbols, "BeginNamedWindow", BeginNamedWindowFn),
            end_window: sym!(symbols, "EndCurrentWindow", CtxFn),
            ui_width: sym!(symbols, "GetUiSpaceWidth", UiWidthFn),
            rect: sym!(symbols, "OverrideNextRect", RectFn),
            space_begin: sym!(symbols, "LayoutSpaceBegin", SpaceBeginFn),
            space_end: sym!(symbols, "LayoutSpaceEnd", CtxFn),
            begin_group: sym!(symbols, "BeginNamedGroup", BeginGroupFn),
            end_group: sym!(symbols, "EndGroup", CtxFn),
            row: sym!(symbols, "LayoutRowDynamic", RowFn),
            row_ratios: sym!(symbols, "LayoutRowRatios", RowRatiosFn),
            spacing: sym!(symbols, "Spacing", CtxFn),
            label: sym!(symbols, "DrawLabel", LabelFn),
            button: sym!(symbols, "DrawMenuButton", MenuButtonFn),
            checkbox: sym!(symbols, "FlagCheckbox", CheckboxFn),
            slider: sym!(symbols, "SliderU32", SliderFn),
            disable_begin: sym!(symbols, "DisableBegin", CtxFn),
            disable_end: sym!(symbols, "DisableEnd", CtxFn),
            text_edit: sym!(symbols, "TextEdit", TextEditFn),
            text_edit_filter: symbols.address("TextEditFilter")? as *mut c_void,
            skin_text_edit: symbols.address("SkinTextEdit")? as *const u32,
            set_skin: sym!(symbols, "SetSkin", SetSkinFn),
            skin_manager: symbols.address("SkinManager")? as *mut c_void,
            skin_window: symbols.address("SkinWindow")? as *const u32,
            skin_window_alt: symbols.address("SkinWindowAlt")? as *const u32,
            skin_alt_flag: symbols.address("SkinAltFlag")? as *const u8,
            skin_content: symbols.address("SkinContentPanel")? as *const u32,
            skin_card: symbols.address("SkinLobbyCreationLeft")? as *const u32,
        })
    }

    pub(crate) fn ctx(&self) -> *mut c_void {
        unsafe { *self.ctx_slot }
    }

    pub(crate) fn push_skin(&self, id: u32) -> u32 {
        unsafe { (self.set_skin)(self.skin_manager, id) }
    }

    pub(crate) fn window_skin(&self) -> u32 {
        unsafe {
            if *self.skin_alt_flag != 0 {
                *self.skin_window_alt
            } else {
                *self.skin_window
            }
        }
    }
}

/// NUL-terminated copy for the game's C-string widgets.
fn cstr(text: &str) -> Vec<u8> {
    let mut v: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
    v.push(0);
    v
}

/// The game's menu widgets on the current context. Every call into the game runs under its
/// own guard: the plugin screen draws while the session is borrowed, and a fault caught only
/// by the hook's outer guard would abandon that borrow for good. A faulting widget reports
/// "nothing happened" and is logged by [`crate::session::host_fault`].
pub struct GameWidgets<'a> {
    nk: &'a Nk,
    ctx: *mut c_void,
    /// Inside a plugin card (wider margins).
    in_card: Cell<bool>,
}

impl<'a> GameWidgets<'a> {
    fn call<R: Default>(what: &str, f: impl FnOnce() -> R) -> R {
        match guarded(f) {
            Ok(r) => r,
            Err(fault) => {
                crate::session::host_fault(what, &fault);
                R::default()
            }
        }
    }
}

impl Widgets for GameWidgets<'_> {
    fn row(&self, height: f32, cols: i32) {
        if !(1..=8).contains(&cols) {
            return Self::call("LayoutRowDynamic", || unsafe {
                (self.nk.row)(self.ctx, height, cols)
            });
        }
        // An empty first cell is the left margin; the widgets share the rest.
        let table = if self.in_card.get() {
            &CARD_RATIOS
        } else {
            &ROW_RATIOS
        };
        let ratios = table[cols as usize - 1].as_ptr();
        Self::call("LayoutRowRatios", || unsafe {
            (self.nk.row_ratios)(self.ctx, 0, height, cols + 2, ratios);
            (self.nk.spacing)(self.ctx);
        })
    }
    fn label(&self, text: &str) {
        let t = cstr(text);
        Self::call("DrawLabel", || unsafe {
            (self.nk.label)(self.ctx, t.as_ptr().cast(), ALIGN_ROW)
        })
    }
    fn button(&self, text: &str) -> bool {
        let t = cstr(text);
        Self::call("DrawMenuButton", || unsafe {
            (self.nk.button)(self.ctx, t.as_ptr().cast()) != 0
        })
    }
    fn checkbox(&self, text: &str, value: &mut bool) -> bool {
        // A space keeps the text off the box.
        let t = cstr(&format!(" {text}"));
        let mut flags: u32 = *value as u32;
        let changed = Self::call("FlagCheckbox", || unsafe {
            (self.nk.checkbox)(t.as_ptr().cast(), &mut flags, 1) != 0
        });
        if changed {
            *value = flags & 1 != 0;
        }
        changed
    }
    fn slider(&self, text: &str, value: &mut i32, min: i32, max: i32) -> bool {
        // The game slider is unsigned; shift the range so min maps to 0.
        self.label(text);
        let mut v = (*value as i64 - min as i64) as u32;
        let mut lo = 0u32;
        let mut hi = (max as i64 - min as i64) as u32;
        let mut step = 1u32;
        let changed = Self::call("SliderU32", || unsafe {
            (self.nk.slider)(&mut v, &mut lo, &mut hi, &mut step) != 0
        });
        if changed {
            *value = (v as i64 + min as i64) as i32;
        }
        changed
    }
    fn text_edit(&self, text: &str, buf: &mut [u8]) -> bool {
        // wc2r-plus `TextEdit`: a label in the first cell, the game's edit box in the second,
        // with the text-edit skin pushed and the row spacing put back around the call.
        self.label(text);
        let before = buf.to_vec();
        let ctx = self.ctx;
        let max = buf.len() as i32;
        let typing = Self::call("TextEdit", || unsafe {
            let spacing_at = (ctx as *mut u8).add(CTX_SPACING_Y).cast::<f32>();
            let spacing = spacing_at.read_unaligned();
            let skin = self.nk.push_skin(*self.nk.skin_text_edit);
            spacing_at.write_unaligned(spacing);
            let state = (self.nk.text_edit)(
                ctx,
                TEXT_EDIT_FLAGS,
                buf.as_mut_ptr().cast(),
                max,
                self.nk.text_edit_filter,
            );
            self.nk.push_skin(skin);
            spacing_at.write_unaligned(spacing);
            state
        });
        let _ = typing;
        before != *buf
    }
    fn card_begin(&self, key: usize, height: f32) -> bool {
        self.row(height, 1);
        let name = cstr(&format!("w2rpm_plugin_card_{key}"));
        let previous = self.nk.push_skin(unsafe { *self.nk.skin_card });
        let opened = Self::call("BeginNamedGroup", || unsafe {
            (self.nk.begin_group)(self.ctx, name.as_ptr().cast(), GROUP_CARD) != 0
        });
        self.nk.push_skin(previous);
        if opened {
            self.in_card.set(true);
            // The frame's top border covers the first row: start below it.
            self.row(crate::ui::CARD_TOP_H, 1);
            self.label("");
        }
        opened
    }
    fn card_end(&self) {
        self.in_card.set(false);
        Self::call("EndGroup", || unsafe { (self.nk.end_group)(self.ctx) })
    }
    fn disable_begin(&self) {
        Self::call("DisableBegin", || unsafe {
            (self.nk.disable_begin)(self.ctx)
        })
    }
    fn disable_end(&self) {
        Self::call("DisableEnd", || unsafe { (self.nk.disable_end)(self.ctx) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Game,
    Plugins,
}

thread_local! {
    static MODE: Cell<Mode> = const { Cell::new(Mode::Game) };
    static LAST_BUILD_MS: Cell<u64> = const { Cell::new(0) };
}

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(target) = symbols.address(SYMBOL) else {
        log.line(
            W2R_LOG_ERROR,
            "host",
            &format!("{SYMBOL} is not in the symbol table"),
        );
        return false;
    };
    // SAFETY: `detour` matches BuildOptionsMenuScreen's cdecl signature from the symbol table.
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            detour as *mut c_void,
            &format!("host:{SYMBOL}"),
            |original| ORIGINAL.store(original as usize, Ordering::Release),
        )
    };
    match result {
        Ok(_) => {
            log.line(
                W2R_LOG_INFO,
                "host",
                &format!("hooked {SYMBOL} @ {target:#010x}"),
            );
            true
        }
        Err(e) => {
            log.line(
                W2R_LOG_ERROR,
                "host",
                &format!("could not hook {SYMBOL}: {e:?}"),
            );
            false
        }
    }
}

unsafe extern "C" fn detour(variant: i32) {
    let original: BuildOptionsMenuScreenFn =
        unsafe { core::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let now = crate::chat::now_ms();
    let fresh = LAST_BUILD_MS.with(|t| now.saturating_sub(t.replace(now)) > REENTRY_GAP_MS);
    if fresh {
        MODE.with(|m| m.set(Mode::Game));
    }
    // Only the main-menu (0) and pause-menu (1) options screens get the button.
    let ours = (variant == 0 || variant == 1) && MODE.with(|m| m.get()) == Mode::Plugins;
    if !ours {
        unsafe { original(variant) };
        if variant == 0 || variant == 1 {
            if let Err(fault) = guarded(|| crate::session::with_nk(draw_button)) {
                crate::session::host_fault("options button", &fault);
            }
        }
        return;
    }
    if let Err(fault) = guarded(|| crate::session::with_nk(draw_screen)) {
        crate::session::host_fault("plugin screen", &fault);
        // Do not leave the user on a screen that cannot draw.
        MODE.with(|m| m.set(Mode::Game));
    }
}

/// The "Plugins" button in the top-right corner, over the game's own options screen.
fn draw_button(nk: &Nk) {
    let ctx = nk.ctx();
    if ctx.is_null() {
        return;
    }
    let width = unsafe { (nk.ui_width)() };
    let x = width - BUTTON_W - 40.0;
    let name = c"w2rpm_button";
    unsafe {
        if (nk.begin_window)(
            ctx,
            name.as_ptr(),
            x,
            40.0,
            BUTTON_W,
            BUTTON_H,
            WINDOW_NO_SCROLLBAR,
        ) != 0
        {
            (nk.row)(ctx, BUTTON_H - 20.0, 1);
            let w = GameWidgets {
                nk,
                ctx,
                in_card: Cell::new(false),
            };
            if w.button("Plugins") {
                MODE.with(|m| m.set(Mode::Plugins));
            }
        }
        (nk.end_window)(ctx);
    }
}

/// The plugin screen in place of the game's options screen.
fn draw_screen(nk: &Nk) {
    let ctx = nk.ctx();
    if ctx.is_null() {
        return;
    }
    let width = unsafe { (nk.ui_width)() }.floor();
    let panel_w = (width * PANEL_WIDTH_PERCENT / 100.0).floor();
    let panel_x = ((width - panel_w) / 2.0).floor();
    let prev_skin = nk.push_skin(nk.window_skin());
    let name = c"w2rpm_plugins";
    unsafe {
        if (nk.begin_window)(
            ctx,
            name.as_ptr(),
            0.0,
            0.0,
            width,
            UI_HEIGHT,
            WINDOW_NO_SCROLLBAR,
        ) == 0
        {
            (nk.end_window)(ctx);
            nk.push_skin(prev_skin);
            return;
        }
        (nk.space_begin)(ctx, 1, 0.0, 1000);
        (nk.rect)(ctx, panel_x, TITLE_TOP, panel_w, TITLE_HEIGHT);
        let title = c"w2rpm Plugins";
        (nk.label)(ctx, title.as_ptr(), ALIGN_TITLE);

        (nk.rect)(ctx, panel_x, PANEL_TOP, panel_w, PANEL_HEIGHT);
        let prev_content = nk.push_skin(*nk.skin_content);
        let group = c"w2rpm_plugins_list";
        let gap_at = (ctx as *mut u8).add(CTX_SPACING_Y).cast::<f32>();
        let gap_before = gap_at.read_unaligned();
        gap_at.write_unaligned(ROW_GAP);
        if (nk.begin_group)(ctx, group.as_ptr(), GROUP_PLAIN) != 0 {
            let w = GameWidgets {
                nk,
                ctx,
                in_card: Cell::new(false),
            };
            if crate::session::draw_plugin_screen(&w) == crate::ui::Action::Back {
                MODE.with(|m| m.set(Mode::Game));
            }
            (nk.end_group)(ctx);
        }
        gap_at.write_unaligned(gap_before);
        nk.push_skin(prev_content);
        (nk.space_end)(ctx);
        (nk.end_window)(ctx);
    }
    nk.push_skin(prev_skin);
}
