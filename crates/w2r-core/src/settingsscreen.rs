//! The options screen (docs/settings-screen.md): every setting the game, the host and the
//! plugins have, in one screen. Categories down the left, the selected category's settings on the
//! right under subcategory headings, a search page, hover descriptions, and Cancel / Apply /
//! Save and Exit along the bottom. A "Plugins" page lists the installed plugins.
//!
//! It replaces the game's options screen on the main menu (variant 0) and in the pause menu
//! (variant 1). The model is [`crate::settings`]; this file only draws it. Layout numbers are in
//! the game's 3840x2160 UI space. Ported from wc2r-plus `core/settings/settings_screen.cpp`
//! (MIT). x86 only.

use core::ffi::{CStr, c_char, c_void};
use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use w2r_abi::{W2R_LOG_ERROR, W2R_LOG_INFO, W2R_LOG_WARN};

use crate::hooks::{self, Owner};
use crate::host::Log;
use crate::pe::Image;
use crate::settings::{Editor, Kind, Native, Owners, SettingDef, Value};
use crate::symbols::Symbols;

// ---- Nuklear context, window and panel offsets (wc2r-plus target/struct_offsets.h) ----------
const CTX_MOUSE_X: usize = 0x1bc;
const CTX_MOUSE_Y: usize = 0x1c0;
const CTX_FONT: usize = 0x1e0;
const CTX_TEXT_COLOUR: usize = 0x20c;
/// style.window.spacing.y. Nuklear sizes a widget as `row.height - spacing.y` when it places it,
/// so the value must be the same when a row is declared and when its widget is drawn.
const CTX_SPACING_Y: usize = 0x2004;
/// style.window.scrollbar_size, .x here and .y at +4.
const CTX_SCROLLBAR_SIZE: usize = 0x2008;
const CTX_GROUP_PADDING_Y: usize = 0x201c;
const CTX_CURRENT_WINDOW: usize = 0x3d28;
const WINDOW_BUFFER: usize = 0x64;
const WINDOW_PANEL: usize = 0x8c;
const PANEL_BOUNDS_X: usize = 0x08;
const PANEL_BOUNDS_W: usize = 0x10;
/// A pointer to the group's scroll offset, not the offset itself.
const PANEL_OFFSET_Y: usize = 0x1c;
const PANEL_AT_Y: usize = 0x24;
const PANEL_CLIP_X: usize = 0x3c;
const PANEL_CLIP_Y: usize = 0x40;
const PANEL_CLIP_W: usize = 0x44;
const PANEL_CLIP_H: usize = 0x48;
const PANEL_ROW_HEIGHT: usize = 0x6c;
const USER_FONT_HEIGHT: usize = 0x04;
const SCROLLBAR_SHOW_BUTTONS: usize = 0xe8;

/// NK_EDIT_FIELD, the flags the game passes for its own search box.
const TEXT_EDIT_FLAGS: u32 = 0x260;
/// Nuklear text alignment: centered for titles, left and vertically centered for rows.
const ALIGN_TITLE: i32 = 0x12;
const ALIGN_ROW: i32 = 0x11;
const GROUP_NO_SCROLLBAR: u32 = 0x20;

const GAME_STATE_MAIN_MENU: i32 = 0x04;
/// Values of the pause menu's sub-screen selector. (0 means no pause menu and is never written.)
const PAUSE_SCREEN_ROOT: u32 = 1;
const PAUSE_SCREEN_OPTIONS: u32 = 7;

// ---- layout, in UI units ---------------------------------------------------------------------
const UI_HEIGHT: f32 = 2160.0;
const PANEL_TOP: f32 = 320.0;
const PANEL_HEIGHT: f32 = 1580.0;
const FOOTER_HEIGHT: f32 = 150.0;
const GAP: f32 = 20.0;
const PANEL_WIDTH_PERCENT: f32 = 80.0;
const CATEGORY_PANEL_PERCENT: f32 = 30.0;
const PANEL_PAD: f32 = 34.0;
const PANEL_PAD_TOP: f32 = 28.0;
const ROW_HEIGHT: f32 = 90.0;
const CATEGORY_ROW_HEIGHT: f32 = 120.0;
/// The gap between rows. The context's own spacing is 0, so this is set around our rows only.
const ROW_SPACING: f32 = 16.0;
/// A text box fills its whole row, so it gets a wider gap on both sides.
const TEXT_ROW_SPACING: f32 = 40.0;
/// Height of a text box row, the same as the game's own search box.
const SEARCH_ROW_HEIGHT: f32 = 120.0;
const TITLE_TOP: f32 = 150.0;
const TITLE_HEIGHT: f32 = 120.0;

const COMBO_ITEM_HEIGHT: f32 = 110.0;
const COMBO_POPUP_PAD: f32 = 40.0;
const COMBO_POPUP_MAX_HEIGHT: f32 = 900.0;

const TIP_MAX_WIDTH: f32 = 1100.0;
const TIP_PAD: f32 = 26.0;
const TIP_BORDER: f32 = 4.0;
const TIP_ROUNDING: f32 = 6.0;
const TIP_CURSOR_OFFSET: f32 = 44.0;
const TIP_MAX_LINES: i32 = 8;
const TIP_FALLBACK_LINE_HEIGHT: f32 = 44.0;

const fn rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    (a << 24) | (b << 16) | (g << 8) | r
}
const TIP_FILL: u32 = rgba(12, 10, 8, 244);
const TIP_BORDER_COLOUR: u32 = rgba(120, 96, 48, 255);
const TIP_TEXT_FALLBACK: u32 = rgba(255, 255, 255, 255);
/// The pause-menu variant's backdrop. The game's in-match window art has its own panel layout
/// drawn into it, which does not fit this screen, so a plain panel is drawn instead.
const BACKDROP: u32 = rgba(26, 20, 14, 246);
const BACKDROP_BORDER: u32 = rgba(120, 96, 48, 255);
const BACKDROP_PAD: f32 = 28.0;
const BACKDROP_BORDER_W: f32 = 5.0;

const EDIT_BUFFER: usize = 256;
const SEARCH_BUFFER: usize = 128;
/// Pages listed after the categories.
const PLUGINS_CATEGORY: &str = "Plugins";
const SEARCH_CATEGORY: &str = "Search";
/// After this long without the screen being built, the next build is a fresh entry.
const REENTRY_GAP_MS: u64 = 250;

type BeginWindowFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, f32, f32, f32, f32, u32) -> i32;
type CtxFn = unsafe extern "C" fn(*mut c_void);
type UiWidthFn = unsafe extern "C" fn() -> f32;
type RectFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32);
type GroupFn = unsafe extern "C" fn(*mut c_void, *const c_char, u32) -> i32;
type SpaceBeginFn = unsafe extern "C" fn(*mut c_void, i32, f32, i32);
type RowFn = unsafe extern "C" fn(*mut c_void, f32, i32);
type LabelFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32);
type ButtonFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> i8;
type VoidFn = unsafe extern "C" fn();
type SliderFn = unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, *mut u32) -> bool;
type CheckboxFn = unsafe extern "C" fn(*const c_char, *mut u32, u32) -> bool;
type TextEditFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_char, i32, *mut c_void) -> u32;
type ComboBeginFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32, f32, f32) -> i32;
type ComboItemFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32) -> i32;
type DisplayCountFn = unsafe extern "C" fn() -> i32;
type FillRectFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32, f32, u32);
type CountLinesFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32, f32) -> i32;
type WrappedLabelFn = unsafe extern "C" fn(*mut c_void, *const c_char, u32);
type SetSkinFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> u32;
type LocalizedFn = unsafe extern "C" fn(*const c_char) -> *const c_char;
type TransitionFn = unsafe extern "C" fn(i32);
type ScrollbarFn = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    f32,
    f32,
    f32,
    f32,
    i32,
    f32,
    f32,
    f32,
    f32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> f32;

/// The game functions and globals the screen draws with, resolved once.
#[derive(Clone, Copy)]
pub struct Fns {
    ctx_slot: *const *mut c_void,
    begin_window: BeginWindowFn,
    end_window: CtxFn,
    ui_width: UiWidthFn,
    rect: RectFn,
    begin_group: GroupFn,
    end_group: CtxFn,
    space_begin: SpaceBeginFn,
    space_end: CtxFn,
    row: RowFn,
    label: LabelFn,
    button: ButtonFn,
    banner: VoidFn,
    slider: SliderFn,
    checkbox: CheckboxFn,
    text_edit: TextEditFn,
    text_edit_filter: *mut c_void,
    combo_begin: ComboBeginFn,
    combo_item: ComboItemFn,
    combo_close: CtxFn,
    combo_end: CtxFn,
    display_count: DisplayCountFn,
    skin_combo_closed: *const u32,
    skin_combo_closed_alt: *const u32,
    skin_combo_open: *const u32,
    skin_combo_open_alt: *const u32,
    fill_rect: FillRectFn,
    count_lines: CountLinesFn,
    wrapped_label: WrappedLabelFn,
    disable_begin: CtxFn,
    disable_end: CtxFn,
    set_skin: SetSkinFn,
    skin_manager: *mut c_void,
    skin_window: *const u32,
    skin_window_alt: *const u32,
    skin_alt_flag: *const u8,
    skin_content: *const u32,
    skin_text_edit: *const u32,
    localized: LocalizedFn,
    transition: TransitionFn,
    pause_screen: *mut u32,
}

// SAFETY: game function and data addresses, fixed for the session; used on the main thread only.
unsafe impl Send for Fns {}
unsafe impl Sync for Fns {}

macro_rules! sym {
    ($symbols:expr, $name:literal, $t:ty) => {
        unsafe { core::mem::transmute::<usize, $t>($symbols.address($name)?) }
    };
}

impl Fns {
    pub fn new(symbols: &Symbols) -> Option<Fns> {
        Some(Fns {
            ctx_slot: symbols.address("NkContextMenus")? as *const *mut c_void,
            begin_window: sym!(symbols, "BeginNamedWindow", BeginWindowFn),
            end_window: sym!(symbols, "EndCurrentWindow", CtxFn),
            ui_width: sym!(symbols, "GetUiSpaceWidth", UiWidthFn),
            rect: sym!(symbols, "OverrideNextRect", RectFn),
            begin_group: sym!(symbols, "BeginNamedGroup", GroupFn),
            end_group: sym!(symbols, "EndGroup", CtxFn),
            space_begin: sym!(symbols, "LayoutSpaceBegin", SpaceBeginFn),
            space_end: sym!(symbols, "LayoutSpaceEnd", CtxFn),
            row: sym!(symbols, "LayoutRowDynamic", RowFn),
            label: sym!(symbols, "DrawLabel", LabelFn),
            button: sym!(symbols, "DrawMenuButton", ButtonFn),
            banner: sym!(symbols, "DrawBanner", VoidFn),
            slider: sym!(symbols, "SliderU32", SliderFn),
            checkbox: sym!(symbols, "FlagCheckbox", CheckboxFn),
            text_edit: sym!(symbols, "TextEdit", TextEditFn),
            text_edit_filter: symbols.address("TextEditFilter")? as *mut c_void,
            combo_begin: sym!(symbols, "ComboBegin", ComboBeginFn),
            combo_item: sym!(symbols, "ComboItem", ComboItemFn),
            combo_close: sym!(symbols, "ComboClose", CtxFn),
            combo_end: sym!(symbols, "ComboEnd", CtxFn),
            display_count: sym!(symbols, "DisplayCount", DisplayCountFn),
            skin_combo_closed: symbols.address("SkinComboClosed")? as *const u32,
            skin_combo_closed_alt: symbols.address("SkinComboClosedAlt")? as *const u32,
            skin_combo_open: symbols.address("SkinComboOpen")? as *const u32,
            skin_combo_open_alt: symbols.address("SkinComboOpenAlt")? as *const u32,
            fill_rect: sym!(symbols, "NkFillRect", FillRectFn),
            count_lines: sym!(symbols, "CountWrappedLines", CountLinesFn),
            wrapped_label: sym!(symbols, "DrawWrappedLabel", WrappedLabelFn),
            disable_begin: sym!(symbols, "DisableBegin", CtxFn),
            disable_end: sym!(symbols, "DisableEnd", CtxFn),
            set_skin: sym!(symbols, "SetSkin", SetSkinFn),
            skin_manager: symbols.address("SkinManager")? as *mut c_void,
            skin_window: symbols.address("SkinWindow")? as *const u32,
            skin_window_alt: symbols.address("SkinWindowAlt")? as *const u32,
            skin_alt_flag: symbols.address("SkinAltFlag")? as *const u8,
            skin_content: symbols.address("SkinContentPanel")? as *const u32,
            skin_text_edit: symbols.address("SkinTextEdit")? as *const u32,
            localized: sym!(symbols, "LookupLocalized", LocalizedFn),
            transition: sym!(symbols, "TransitionGameState", TransitionFn),
            pause_screen: symbols.address("PauseMenuScreen")? as *mut u32,
        })
    }

    fn ctx(&self) -> *mut c_void {
        unsafe { *self.ctx_slot }
    }

    fn alt(&self) -> bool {
        unsafe { *self.skin_alt_flag == 1 }
    }

    fn push_skin(&self, id: u32) -> u32 {
        unsafe { (self.set_skin)(self.skin_manager, id) }
    }

    /// The main menu's options window skin, used for both variants: it draws no window art, and
    /// text boxes render correctly under it.
    fn window_skin(&self) -> u32 {
        unsafe {
            if self.alt() {
                *self.skin_window_alt
            } else {
                *self.skin_window
            }
        }
    }

    fn combo_skin(&self, open: bool) -> u32 {
        unsafe {
            match (open, self.alt()) {
                (true, true) => *self.skin_combo_open_alt,
                (true, false) => *self.skin_combo_open,
                (false, true) => *self.skin_combo_closed_alt,
                (false, false) => *self.skin_combo_closed,
            }
        }
    }

    /// Translates a localization key; any other text comes back unchanged.
    fn localize(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let key = cstr(text);
        let shown = unsafe { (self.localized)(key.as_ptr().cast()) };
        if shown.is_null() {
            return text.to_owned();
        }
        unsafe { CStr::from_ptr(shown) }
            .to_string_lossy()
            .into_owned()
    }
}

/// NUL-terminated copy for the game's C-string widgets.
fn cstr(text: &str) -> Vec<u8> {
    let mut v: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
    v.push(0);
    v
}

fn read_f32(base: *mut c_void, offset: usize) -> f32 {
    unsafe { base.cast::<u8>().add(offset).cast::<f32>().read_unaligned() }
}

fn write_f32(base: *mut c_void, offset: usize, value: f32) {
    unsafe {
        base.cast::<u8>()
            .add(offset)
            .cast::<f32>()
            .write_unaligned(value)
    }
}

fn set_row_spacing(ctx: *mut c_void, spacing: f32) {
    write_f32(ctx, CTX_SPACING_Y, spacing);
}

fn current_window(ctx: *mut c_void) -> *mut c_void {
    unsafe {
        ctx.cast::<u8>()
            .add(CTX_CURRENT_WINDOW)
            .cast::<*mut c_void>()
            .read_unaligned()
    }
}

/// The current window's panel, or null outside a window.
fn current_panel(ctx: *mut c_void) -> *mut c_void {
    let win = current_window(ctx);
    if win.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        win.cast::<u8>()
            .add(WINDOW_PANEL)
            .cast::<*mut c_void>()
            .read_unaligned()
    }
}

fn panel_f32(ctx: *mut c_void, offset: usize) -> f32 {
    let panel = current_panel(ctx);
    if panel.is_null() {
        -1.0
    } else {
        read_f32(panel, offset)
    }
}

/// The top of the next row Nuklear will place, in layout space.
fn next_row_top(ctx: *mut c_void) -> f32 {
    panel_f32(ctx, PANEL_AT_Y) + panel_f32(ctx, PANEL_ROW_HEIGHT)
}

/// What the screen remembers between frames.
struct Screen {
    category: String,
    categories: Vec<String>,
    last_frame_ms: u64,
    search: [u8; SEARCH_BUFFER],
    /// Text box buffers by setting index, cleared on each entry.
    edits: Vec<(usize, [u8; EDIT_BUFFER])>,
    /// Whether each dropdown was open last frame: its header is drawn with a different skin.
    combo_open: Vec<u8>,
    /// Dropdowns open this frame. No tooltip is drawn while one is open.
    combos_this_frame: u32,
    tip: Option<Tip>,
}

/// The hovered setting's tooltip. Captured while its row is drawn inside the scrolling group, and
/// drawn after every group has ended so the group does not clip it.
struct Tip {
    text: String,
    mouse_x: f32,
    mouse_y: f32,
}

thread_local! {
    static SCREEN: RefCell<Screen> = const {
        RefCell::new(Screen {
            category: String::new(),
            categories: Vec::new(),
            last_frame_ms: 0,
            search: [0; SEARCH_BUFFER],
            edits: Vec::new(),
            combo_open: Vec::new(),
            combos_this_frame: 0,
            tip: None,
        })
    };
}

/// What the screen needs from the session.
pub trait ScreenHost {
    fn parts(&mut self) -> (&mut Editor, &mut dyn Native, &mut dyn Owners);
    fn draw_plugins(&mut self, ctx: *mut c_void);
}

static FNS: OnceLock<Fns> = OnceLock::new();
static SCROLLBAR_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static HIDE_ARROWS: AtomicBool = AtomicBool::new(true);
/// Set when the footer closes the screen; [`finish`] acts on it.
static CLOSE_PENDING: AtomicBool = AtomicBool::new(false);

/// Fix Overscroll: hides the arrow buttons on every vertical scrollbar. Takes effect at once.
pub fn set_hide_arrows(on: bool) {
    HIDE_ARROWS.store(on, Ordering::Release);
}

/// Resolves the screen's game functions and hooks `nk_do_scrollbarv` for Fix Overscroll. The
/// options screen hook is in `options.rs`.
pub fn install(image: &Image, symbols: &Symbols, log: &dyn Log) -> bool {
    let Some(fns) = Fns::new(symbols) else {
        log.line(
            W2R_LOG_ERROR,
            "settings",
            "screen symbols missing; stock options screen",
        );
        return false;
    };
    let _ = FNS.set(fns);
    let Some(target) = symbols.address("DoScrollbarV") else {
        return true;
    };
    // SAFETY: `scrollbar_detour` matches nk_do_scrollbarv's cdecl signature (symbol table).
    let result = unsafe {
        hooks::global().install(
            Owner::Host,
            image,
            target,
            scrollbar_detour as *mut c_void,
            "host:DoScrollbarV",
            |original| SCROLLBAR_ORIGINAL.store(original as usize, Ordering::Release),
        )
    };
    if let Err(e) = result {
        log.line(
            W2R_LOG_WARN,
            "settings",
            &format!("could not hook DoScrollbarV (Fix Overscroll off): {e:?}"),
        );
    }
    true
}

/// Whether [`install`] resolved the screen.
pub fn available() -> bool {
    FNS.get().is_some()
}

/// Applies Fix Overscroll. It is set here, where the scrollbar reads it, because the game
/// restores the style (and `show_buttons` with it) between beginning a group and drawing its
/// scrollbar.
unsafe extern "C" fn scrollbar_detour(
    state: *mut c_void,
    out: *mut c_void,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    has_scrolling: i32,
    offset: f32,
    target: f32,
    step: f32,
    button_pixel_inc: f32,
    style: *mut c_void,
    input: *mut c_void,
    font: *mut c_void,
) -> f32 {
    static SAVED: AtomicUsize = AtomicUsize::new(usize::MAX);
    if !style.is_null() {
        let show = unsafe { style.cast::<u8>().add(SCROLLBAR_SHOW_BUTTONS).cast::<i32>() };
        let current = unsafe { show.read_unaligned() };
        if SAVED.load(Ordering::Relaxed) == usize::MAX {
            SAVED.store(current as usize, Ordering::Relaxed);
        }
        let value = if HIDE_ARROWS.load(Ordering::Acquire) {
            0
        } else {
            SAVED.load(Ordering::Relaxed) as i32
        };
        unsafe { show.write_unaligned(value) };
    }
    let original: ScrollbarFn =
        unsafe { core::mem::transmute(SCROLLBAR_ORIGINAL.load(Ordering::Acquire)) };
    unsafe {
        original(
            state,
            out,
            x,
            y,
            w,
            h,
            has_scrolling,
            offset,
            target,
            step,
            button_pixel_inc,
            style,
            input,
            font,
        )
    }
}

/// Draws the screen for `variant` (0 main menu, 1 pause menu). False when the screen is not
/// available.
pub fn draw(variant: i32, host: &mut dyn ScreenHost) -> bool {
    let Some(fns) = FNS.get().copied() else {
        return false;
    };
    let ctx = fns.ctx();
    if ctx.is_null() {
        return true;
    }
    SCREEN.with(|cell| {
        let Ok(mut screen) = cell.try_borrow_mut() else {
            return;
        };
        if draw_screen(&fns, ctx, variant, &mut screen, host) {
            CLOSE_PENDING.store(true, Ordering::Release);
        }
    });
    true
}

/// Leaves the screen if the footer asked to. Called after the session is released, because
/// leaving runs game code that may re-enter the host's hooks.
pub fn finish(variant: i32) {
    if CLOSE_PENDING.swap(false, Ordering::AcqRel) {
        if let Some(fns) = FNS.get() {
            leave(fns, variant);
        }
    }
}

/// From the main menu, a game state transition. From the pause menu, its selector goes back to
/// the pause menu's root, but only if it still shows the options.
fn leave(fns: &Fns, variant: i32) {
    if variant == 1 {
        let selector = unsafe { fns.pause_screen.read() };
        if selector == PAUSE_SCREEN_OPTIONS {
            unsafe { fns.pause_screen.write(PAUSE_SCREEN_ROOT) };
            crate::session::host_line(W2R_LOG_INFO, "settings", "leaving to the pause menu root");
        } else {
            crate::session::host_line(
                W2R_LOG_WARN,
                "settings",
                &format!(
                    "NOT leaving: pause selector is {selector}, expected {PAUSE_SCREEN_OPTIONS}"
                ),
            );
        }
    } else {
        crate::session::host_line(W2R_LOG_INFO, "settings", "leaving to the main menu");
        unsafe { (fns.transition)(GAME_STATE_MAIN_MENU) };
    }
}

fn draw_screen(
    fns: &Fns,
    ctx: *mut c_void,
    variant: i32,
    screen: &mut Screen,
    host: &mut dyn ScreenHost,
) -> bool {
    let now = crate::chat::now_ms();
    let fresh = now.saturating_sub(screen.last_frame_ms) > REENTRY_GAP_MS;
    screen.last_frame_ms = now;
    if fresh {
        let (editor, native, owners) = host.parts();
        refresh_monitor_list(fns, editor);
        editor.enter(native, owners);
        screen.edits.clear();
        screen.search = [0; SEARCH_BUFFER];
        screen.categories = editor.registry.categories();
        screen.categories.push(PLUGINS_CATEGORY.to_owned());
        screen.categories.push(SEARCH_CATEGORY.to_owned());
        screen.category = screen.categories[0].clone();
    }
    screen.tip = None;
    screen.combos_this_frame = 0;

    // Floored, with every edge derived from these, so neighbouring panels share exact edges.
    // Fractional edges leave a hairline gap between panels.
    let ui_w = unsafe { (fns.ui_width)() }.floor();
    let panel_w = (ui_w * PANEL_WIDTH_PERCENT * 0.01).floor();
    let panel_x = ((ui_w - panel_w) * 0.5).floor();
    let left_w = (panel_w * CATEGORY_PANEL_PERCENT * 0.01).floor();
    let right_x = panel_x + left_w + GAP;
    let right_w = panel_w - left_w - GAP;
    let content_h = PANEL_HEIGHT - FOOTER_HEIGHT - GAP;
    let footer_y = PANEL_TOP + content_h + GAP;

    let prev_skin = fns.push_skin(fns.window_skin());
    let name = c"w2r_settings";
    if unsafe {
        (fns.begin_window)(
            ctx,
            name.as_ptr(),
            0.0,
            0.0,
            ui_w,
            UI_HEIGHT,
            GROUP_NO_SCROLLBAR,
        )
    } == 0
    {
        unsafe { (fns.end_window)(ctx) };
        fns.push_skin(prev_skin);
        return false;
    }
    unsafe { (fns.space_begin)(ctx, 1, 0.0, 1000) };

    // Commands draw in the order they are pushed, so the backdrop goes first.
    if variant == 1 {
        draw_backdrop(fns, ctx, panel_x, panel_w, footer_y);
    }
    // The stock options screen draws the banner on the main menu only.
    if variant == 0 {
        unsafe { (fns.banner)() };
    }
    rect(fns, ctx, panel_x, TITLE_TOP, panel_w, TITLE_HEIGHT);
    label(fns, ctx, &fns.localize("option_title"), ALIGN_TITLE);

    let fits = category_list_fits(ctx, screen.categories.len(), content_h);
    if begin_padded_panel(
        fns,
        ctx,
        c"w2r_settings_categories",
        c"w2r_settings_categories_inner",
        [panel_x, PANEL_TOP, left_w, content_h],
        0,
        fits,
    ) {
        draw_category_panel(fns, ctx, screen);
        end_padded_panel(fns, ctx);
    }

    let searching = screen.category.eq_ignore_ascii_case(SEARCH_CATEGORY);
    let plugins = screen.category.eq_ignore_ascii_case(PLUGINS_CATEGORY);
    let mut content_top = PANEL_TOP;
    let mut content_space = content_h;
    // The search box is placed directly in the window, not in a group: a text box inside a group
    // that does not take input can never get focus.
    if searching {
        rect(
            fns,
            ctx,
            right_x + PANEL_PAD,
            content_top + PANEL_PAD_TOP,
            right_w - PANEL_PAD * 2.0,
            SEARCH_ROW_HEIGHT,
        );
        let prev = fns.push_skin(unsafe { *fns.skin_text_edit });
        unsafe {
            (fns.text_edit)(
                ctx,
                TEXT_EDIT_FLAGS,
                screen.search.as_mut_ptr().cast(),
                SEARCH_BUFFER as i32,
                fns.text_edit_filter,
            )
        };
        fns.push_skin(prev);
        let strip = SEARCH_ROW_HEIGHT + PANEL_PAD_TOP + GAP;
        content_top += strip + GAP;
        content_space -= strip + GAP;
    }

    if begin_padded_panel(
        fns,
        ctx,
        c"w2r_settings_content",
        c"w2r_settings_content_inner",
        [right_x, content_top, right_w, content_space],
        0,
        false,
    ) {
        let saved = read_f32(ctx, CTX_SPACING_Y);
        set_row_spacing(ctx, ROW_SPACING);
        if plugins {
            host.draw_plugins(ctx);
        } else {
            let (editor, native, _) = host.parts();
            if searching {
                draw_search_pane(fns, ctx, screen, editor, native);
            } else {
                draw_settings_pane(fns, ctx, screen, editor, native);
            }
        }
        set_row_spacing(ctx, saved);
        end_padded_panel(fns, ctx);
    }

    let mut close = false;
    rect(fns, ctx, panel_x, footer_y, panel_w, FOOTER_HEIGHT);
    if unsafe { (fns.begin_group)(ctx, c"w2r_settings_footer".as_ptr(), GROUP_NO_SCROLLBAR) } != 0 {
        let (editor, native, owners) = host.parts();
        close = draw_footer(fns, ctx, FOOTER_HEIGHT - GAP, editor, native, owners);
        unsafe { (fns.end_group)(ctx) };
    }

    // Last, so it is drawn over everything.
    draw_tooltip(fns, ctx, ui_w, screen);

    unsafe {
        (fns.space_end)(ctx);
        (fns.end_window)(ctx);
    }
    fns.push_skin(prev_skin);
    close
}

/// Builds the monitor dropdown from the game's display count. Done on each entry, because the
/// count is not known yet at startup.
fn refresh_monitor_list(fns: &Fns, editor: &mut Editor) {
    let Some(i) = editor
        .registry
        .find(&crate::settings::Owner::Game, "display_monitor")
    else {
        return;
    };
    let count = unsafe { (fns.display_count)() };
    // Keep the slider when the count is unavailable or implausible.
    if !(1..=16).contains(&count) {
        return;
    }
    let names = (1..=count).map(|n| format!("Display {n}")).collect();
    editor.registry.set_choices(i, names);
}

fn rect(fns: &Fns, ctx: *mut c_void, x: f32, y: f32, w: f32, h: f32) {
    unsafe { (fns.rect)(ctx, x, y, w, h) };
}

fn row(fns: &Fns, ctx: *mut c_void, height: f32, cols: i32) {
    unsafe { (fns.row)(ctx, height, cols) };
}

fn label(fns: &Fns, ctx: *mut c_void, text: &str, align: i32) {
    let t = cstr(text);
    unsafe { (fns.label)(ctx, t.as_ptr().cast(), align) };
}

fn button(fns: &Fns, ctx: *mut c_void, text: &str) -> bool {
    let t = cstr(text);
    unsafe { (fns.button)(ctx, t.as_ptr().cast()) != 0 }
}

/// Opens a group with the content-panel frame.
fn begin_panel_group(fns: &Fns, ctx: *mut c_void, name: &CStr, flags: u32) -> bool {
    let prev = fns.push_skin(unsafe { *fns.skin_content });
    let opened = unsafe { (fns.begin_group)(ctx, name.as_ptr(), flags) } != 0;
    fns.push_skin(prev);
    opened
}

/// A framed panel with a content group inset inside it, so the content does not touch the frame.
///
/// Nuklear reserves a scrollbar's width in a group even when no scrollbar is drawn.
/// `suppress_scrollbar_width` removes it; pass it only when the content fits without scrolling.
fn begin_padded_panel(
    fns: &Fns,
    ctx: *mut c_void,
    frame: &CStr,
    content: &CStr,
    [x, y, w, h]: [f32; 4],
    content_flags: u32,
    suppress_scrollbar_width: bool,
) -> bool {
    let top = PANEL_PAD + PANEL_PAD_TOP;
    rect(fns, ctx, x, y, w, h);
    if !begin_panel_group(fns, ctx, frame, GROUP_NO_SCROLLBAR) {
        return false;
    }
    unsafe { (fns.space_begin)(ctx, 1, 0.0, 1000) };
    rect(
        fns,
        ctx,
        PANEL_PAD,
        top,
        w - PANEL_PAD * 2.0,
        h - top - PANEL_PAD,
    );
    let saved = read_f32(ctx, CTX_SCROLLBAR_SIZE);
    if suppress_scrollbar_width {
        write_f32(ctx, CTX_SCROLLBAR_SIZE, 0.0);
    }
    let opened = unsafe { (fns.begin_group)(ctx, content.as_ptr(), content_flags) } != 0;
    write_f32(ctx, CTX_SCROLLBAR_SIZE, saved);
    if !opened {
        unsafe {
            (fns.space_end)(ctx);
            (fns.end_group)(ctx);
        }
        return false;
    }
    true
}

fn end_padded_panel(fns: &Fns, ctx: *mut c_void) {
    unsafe {
        (fns.end_group)(ctx);
        (fns.space_end)(ctx);
        (fns.end_group)(ctx);
    }
}

/// Whether the category buttons fit their panel without scrolling.
fn category_list_fits(ctx: *mut c_void, count: usize, h: f32) -> bool {
    if count == 0 {
        return true;
    }
    let spacing = read_f32(ctx, CTX_SPACING_Y);
    let padding = read_f32(ctx, CTX_GROUP_PADDING_Y);
    let needed = count as f32 * CATEGORY_ROW_HEIGHT + (count - 1) as f32 * spacing;
    let available = h - (PANEL_PAD + PANEL_PAD_TOP) - PANEL_PAD - padding * 2.0;
    needed <= available
}

fn draw_category_panel(fns: &Fns, ctx: *mut c_void, screen: &mut Screen) {
    row(fns, ctx, CATEGORY_ROW_HEIGHT, 1);
    let mut picked = None;
    for c in &screen.categories {
        // The current category's button is disabled.
        let current = c.eq_ignore_ascii_case(&screen.category);
        if current {
            unsafe { (fns.disable_begin)(ctx) };
        }
        if button(fns, ctx, c) && !current {
            picked = Some(c.clone());
        }
        if current {
            unsafe { (fns.disable_end)(ctx) };
        }
    }
    if let Some(c) = picked {
        screen.category = c;
    }
}

fn is_text_row(d: &SettingDef) -> bool {
    d.kind == Kind::Str
}

/// The selected category's settings. Spacing is set before every row, because pushing a skin
/// resets it. A row's spacing is the gap below it, so a text box gets a wider gap above it by
/// widening the row before it.
fn draw_settings_pane(
    fns: &Fns,
    ctx: *mut c_void,
    screen: &mut Screen,
    editor: &mut Editor,
    native: &mut dyn Native,
) {
    let groups = editor.registry.groups(&screen.category);
    for (name, items) in groups {
        let first_text = items
            .first()
            .is_some_and(|&i| is_text_row(editor.registry.def(i)));
        set_row_spacing(
            ctx,
            if first_text {
                TEXT_ROW_SPACING
            } else {
                ROW_SPACING
            },
        );
        row(fns, ctx, ROW_HEIGHT, 1);
        label(fns, ctx, &name, ALIGN_TITLE);
        for (k, &i) in items.iter().enumerate() {
            let this_text = is_text_row(editor.registry.def(i));
            let next_text = items
                .get(k + 1)
                .is_some_and(|&n| is_text_row(editor.registry.def(n)));
            set_row_spacing(
                ctx,
                if this_text || next_text {
                    TEXT_ROW_SPACING
                } else {
                    ROW_SPACING
                },
            );
            let top = next_row_top(ctx);
            draw_setting_row(fns, ctx, screen, editor, native, i, false, true);
            capture_tooltip(ctx, screen, editor.registry.def(i), top, next_row_top(ctx));
        }
    }
}

/// Search results across every category. Text settings are read-only here: Nuklear identifies a
/// text box by its position in the frame, so results changing under the search box would take its
/// focus.
fn draw_search_pane(
    fns: &Fns,
    ctx: *mut c_void,
    screen: &mut Screen,
    editor: &mut Editor,
    native: &mut dyn Native,
) {
    let query = buffer_text(&screen.search);
    let shown = |d: &SettingDef| fns.localize(&d.label);
    let hits = editor.registry.search(&query, &shown);
    for &i in &hits {
        set_row_spacing(ctx, ROW_SPACING);
        let top = next_row_top(ctx);
        draw_setting_row(fns, ctx, screen, editor, native, i, true, false);
        capture_tooltip(ctx, screen, editor.registry.def(i), top, next_row_top(ctx));
    }
    if hits.is_empty() {
        row(fns, ctx, ROW_HEIGHT, 1);
        let text = if query.is_empty() {
            "Type to search all settings."
        } else {
            "No settings match."
        };
        label(fns, ctx, text, ALIGN_ROW);
    }
}

fn buffer_text(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// One setting: a caption and its control. `show_category` prefixes the category, for search
/// results.
#[allow(clippy::too_many_arguments)]
fn draw_setting_row(
    fns: &Fns,
    ctx: *mut c_void,
    screen: &mut Screen,
    editor: &mut Editor,
    native: &mut dyn Native,
    i: usize,
    show_category: bool,
    allow_text_edit: bool,
) {
    let d = editor.registry.def(i).clone();
    let shown = fns.localize(&d.label);
    let caption = if show_category {
        format!("{} / {shown}", d.category)
    } else {
        shown
    };
    let inactive = editor.registry.inactive(i, native);
    if inactive {
        unsafe { (fns.disable_begin)(ctx) };
    }
    let value = editor.get(i, native);
    match d.kind {
        Kind::Choice if !d.choices.is_empty() => {
            row(fns, ctx, ROW_HEIGHT, 2);
            label(fns, ctx, &caption, ALIGN_ROW);
            let current = d.choice_index(value.int());
            let panel = current_panel(ctx);
            let combo_w = if panel.is_null() {
                600.0
            } else {
                read_f32(panel, PANEL_BOUNDS_W) * 0.5
            };
            let combo_h = (d.choices.len() as f32 * COMBO_ITEM_HEIGHT + COMBO_POPUP_PAD)
                .min(COMBO_POPUP_MAX_HEIGHT);
            let header = fns.localize(&d.choices[current]);
            if combo_begin(fns, ctx, screen, i, &header, combo_w, combo_h) {
                // No spacing between the popup's items. Set before the first row.
                let pane_spacing = read_f32(ctx, CTX_SPACING_Y);
                set_row_spacing(ctx, 0.0);
                for (o, choice) in d.choices.iter().enumerate() {
                    row(fns, ctx, COMBO_ITEM_HEIGHT, 1);
                    let t = cstr(&fns.localize(choice));
                    if unsafe { (fns.combo_item)(ctx, t.as_ptr().cast(), ALIGN_ROW) } != 0 {
                        editor.set(i, Value::Int(d.choice_value(o)), native);
                        unsafe { (fns.combo_close)(ctx) };
                    }
                }
                unsafe { (fns.combo_end)(ctx) };
                set_row_spacing(ctx, pane_spacing);
            }
        }
        Kind::Choice | Kind::Bool => {
            row(fns, ctx, ROW_HEIGHT, 1);
            let t = cstr(&caption);
            let mut flags: u32 = u32::from(value.int() != 0);
            if unsafe { (fns.checkbox)(t.as_ptr().cast(), &mut flags, 1) } {
                editor.set(i, Value::Int(i64::from(flags & 1 != 0)), native);
            }
        }
        Kind::Int => {
            let slider = d.is_slider();
            row(fns, ctx, ROW_HEIGHT, if slider { 2 } else { 1 });
            label(fns, ctx, &format!("{caption}: {}", value.int()), ALIGN_ROW);
            if slider {
                // The game's slider is unsigned: shift the range so min maps to 0.
                let mut v = (value.int() - d.min) as u32;
                let mut lo = 0u32;
                let mut hi = (d.max - d.min) as u32;
                let mut step = 1u32;
                if unsafe { (fns.slider)(&mut v, &mut lo, &mut hi, &mut step) } {
                    editor.set(i, Value::Int(i64::from(v) + d.min), native);
                }
            }
        }
        Kind::Str if !allow_text_edit => {
            row(fns, ctx, ROW_HEIGHT, 1);
            label(fns, ctx, &format!("{caption}: {}", value.str()), ALIGN_ROW);
        }
        Kind::Str => {
            row(fns, ctx, SEARCH_ROW_HEIGHT, 2);
            label(fns, ctx, &caption, ALIGN_ROW);
            let slot = match screen.edits.iter().position(|(k, _)| *k == i) {
                Some(p) => p,
                None => {
                    let mut buf = [0u8; EDIT_BUFFER];
                    let bytes = value.str().as_bytes();
                    let n = bytes.len().min(EDIT_BUFFER - 1);
                    buf[..n].copy_from_slice(&bytes[..n]);
                    screen.edits.push((i, buf));
                    screen.edits.len() - 1
                }
            };
            let typing = text_edit(fns, ctx, &mut screen.edits[slot].1);
            // Stored only once the box loses focus, so the value never holds half-typed text.
            let text = buffer_text(&screen.edits[slot].1);
            if !typing && text != value.str() {
                editor.set(i, Value::Str(text), native);
            }
        }
    }
    if inactive {
        unsafe { (fns.disable_end)(ctx) };
    }
}

/// The game's text box over `buf`. Pushing the edit skin resets the row spacing, which would
/// change the box's height, so the spacing is restored after the push and the pop. True while
/// the box has focus.
fn text_edit(fns: &Fns, ctx: *mut c_void, buf: &mut [u8]) -> bool {
    let spacing = read_f32(ctx, CTX_SPACING_Y);
    let prev = fns.push_skin(unsafe { *fns.skin_text_edit });
    set_row_spacing(ctx, spacing);
    let state = unsafe {
        (fns.text_edit)(
            ctx,
            TEXT_EDIT_FLAGS,
            buf.as_mut_ptr().cast(),
            buf.len() as i32,
            fns.text_edit_filter,
        )
    };
    fns.push_skin(prev);
    set_row_spacing(ctx, spacing);
    state & 1 != 0
}

/// Opens a dropdown header. Calls the combo function directly rather than the game's wrapper,
/// so the row spacing can be restored after the skin push.
#[allow(clippy::too_many_arguments)]
fn combo_begin(
    fns: &Fns,
    ctx: *mut c_void,
    screen: &mut Screen,
    i: usize,
    text: &str,
    w: f32,
    h: f32,
) -> bool {
    if screen.combo_open.len() <= i {
        screen.combo_open.resize(i + 1, 0);
    }
    let spacing = read_f32(ctx, CTX_SPACING_Y);
    let prev = fns.push_skin(fns.combo_skin(screen.combo_open[i] != 0));
    set_row_spacing(ctx, spacing);
    let t = cstr(text);
    let opened = unsafe { (fns.combo_begin)(ctx, t.as_ptr().cast(), ALIGN_ROW, w, h) } != 0;
    fns.push_skin(prev);
    set_row_spacing(ctx, spacing);
    screen.combo_open[i] = u8::from(opened);
    if opened {
        screen.combos_this_frame += 1;
    }
    opened
}

/// Records the tooltip if the mouse is over the setting just drawn, between `y0` and `y1` in
/// layout space. The group's scroll offset is subtracted to compare them with the mouse.
fn capture_tooltip(ctx: *mut c_void, screen: &mut Screen, d: &SettingDef, y0: f32, y1: f32) {
    let text = d.tooltip();
    if text.is_empty() {
        return;
    }
    let panel = current_panel(ctx);
    if panel.is_null() {
        return;
    }
    let offset_ptr = unsafe {
        panel
            .cast::<u8>()
            .add(PANEL_OFFSET_Y)
            .cast::<*const u32>()
            .read_unaligned()
    };
    let scroll = if offset_ptr.is_null() {
        0.0
    } else {
        (unsafe { offset_ptr.read_unaligned() }) as f32
    };
    let (y0, y1) = (y0 - scroll, y1 - scroll);
    let mx = read_f32(ctx, CTX_MOUSE_X);
    let my = read_f32(ctx, CTX_MOUSE_Y);
    let x = read_f32(panel, PANEL_BOUNDS_X);
    let w = read_f32(panel, PANEL_BOUNDS_W);
    if mx < x || mx >= x + w || my < y0 || my >= y1 {
        return;
    }
    // Ignore the part of a row that is scrolled out of view.
    let (cx, cy) = (read_f32(panel, PANEL_CLIP_X), read_f32(panel, PANEL_CLIP_Y));
    let (cw, ch) = (read_f32(panel, PANEL_CLIP_W), read_f32(panel, PANEL_CLIP_H));
    if mx < cx || mx >= cx + cw || my < cy || my >= cy + ch {
        return;
    }
    screen.tip = Some(Tip {
        text,
        mouse_x: mx,
        mouse_y: my,
    });
}

fn font_line_height(ctx: *mut c_void) -> f32 {
    let font = unsafe {
        ctx.cast::<u8>()
            .add(CTX_FONT)
            .cast::<*const u8>()
            .read_unaligned()
    };
    if font.is_null() {
        return TIP_FALLBACK_LINE_HEIGHT;
    }
    let h = unsafe { font.add(USER_FONT_HEIGHT).cast::<f32>().read_unaligned() };
    if h > 1.0 && h < 500.0 {
        h
    } else {
        TIP_FALLBACK_LINE_HEIGHT
    }
}

/// Draws the captured tooltip: a bordered box sized with the game's line counter, and the
/// wrapped text.
fn draw_tooltip(fns: &Fns, ctx: *mut c_void, ui_w: f32, screen: &Screen) {
    let Some(tip) = &screen.tip else { return };
    // An open dropdown's popup would cover it.
    if screen.combos_this_frame > 0 {
        return;
    }
    let win = current_window(ctx);
    if win.is_null() {
        return;
    }
    let canvas = unsafe { win.cast::<u8>().add(WINDOW_BUFFER).cast::<c_void>() };
    let text = cstr(&tip.text);
    let line_h = font_line_height(ctx);
    let wrap_w = TIP_MAX_WIDTH - TIP_PAD * 2.0;
    let lines =
        unsafe { (fns.count_lines)(ctx, text.as_ptr().cast(), (text.len() - 1) as i32, wrap_w) }
            .clamp(1, TIP_MAX_LINES);
    let tip_w = TIP_MAX_WIDTH;
    let tip_h = lines as f32 * line_h + TIP_PAD * 2.0;
    // Below and right of the cursor, moved back on screen if it would not fit.
    let mut x = tip.mouse_x + TIP_CURSOR_OFFSET;
    let mut y = tip.mouse_y + TIP_CURSOR_OFFSET;
    if x + tip_w > ui_w {
        x = tip.mouse_x - TIP_CURSOR_OFFSET - tip_w;
    }
    if y + tip_h > UI_HEIGHT {
        y = tip.mouse_y - TIP_CURSOR_OFFSET - tip_h;
    }
    let (x, y) = (x.max(0.0), y.max(0.0));
    unsafe {
        (fns.fill_rect)(canvas, x, y, tip_w, tip_h, TIP_ROUNDING, TIP_BORDER_COLOUR);
        (fns.fill_rect)(
            canvas,
            x + TIP_BORDER,
            y + TIP_BORDER,
            tip_w - TIP_BORDER * 2.0,
            tip_h - TIP_BORDER * 2.0,
            TIP_ROUNDING,
            TIP_FILL,
        );
    }
    let mut colour = unsafe {
        ctx.cast::<u8>()
            .add(CTX_TEXT_COLOUR)
            .cast::<u32>()
            .read_unaligned()
    };
    if colour >> 24 == 0 {
        colour = TIP_TEXT_FALLBACK;
    }
    rect(
        fns,
        ctx,
        x + TIP_PAD,
        y + TIP_PAD,
        tip_w - TIP_PAD * 2.0,
        tip_h - TIP_PAD * 2.0,
    );
    unsafe { (fns.wrapped_label)(ctx, text.as_ptr().cast(), colour) };
}

/// The pause-menu variant's backdrop, from the title down through the footer.
fn draw_backdrop(fns: &Fns, ctx: *mut c_void, panel_x: f32, panel_w: f32, footer_y: f32) {
    let win = current_window(ctx);
    if win.is_null() {
        return;
    }
    let canvas = unsafe { win.cast::<u8>().add(WINDOW_BUFFER).cast::<c_void>() };
    let x = panel_x - BACKDROP_PAD;
    let y = TITLE_TOP - BACKDROP_PAD;
    let w = panel_w + BACKDROP_PAD * 2.0;
    let h = (footer_y + FOOTER_HEIGHT + BACKDROP_PAD) - y;
    unsafe {
        (fns.fill_rect)(canvas, x, y, w, h, 0.0, BACKDROP_BORDER);
        (fns.fill_rect)(
            canvas,
            x + BACKDROP_BORDER_W,
            y + BACKDROP_BORDER_W,
            w - BACKDROP_BORDER_W * 2.0,
            h - BACKDROP_BORDER_W * 2.0,
            0.0,
            BACKDROP,
        );
    }
}

/// Cancel, Apply, Save and Exit. True when the screen should close.
fn draw_footer(
    fns: &Fns,
    ctx: *mut c_void,
    height: f32,
    editor: &mut Editor,
    native: &mut dyn Native,
    owners: &mut dyn Owners,
) -> bool {
    row(fns, ctx, height, 3);
    if button(fns, ctx, "Cancel") {
        editor.cancel(native, owners);
        return true;
    }
    // Apply is disabled until something changes.
    let dirty = editor.dirty();
    if !dirty {
        unsafe { (fns.disable_begin)(ctx) };
    }
    if button(fns, ctx, "Apply") && dirty {
        editor.apply(native, owners);
    }
    if !dirty {
        unsafe { (fns.disable_end)(ctx) };
    }
    if button(fns, ctx, "Save and Exit") {
        editor.apply(native, owners);
        return true;
    }
    false
}
