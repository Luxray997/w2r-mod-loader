//! The drawing kit for the host's own screens (docs/create-screen.md): rectangle placement over
//! the game's immediate-mode UI, ported from wc2r-plus `features/ui/screen_kit.cpp`. The
//! geometry is pure and tested everywhere; the part that calls into the game is x86 only
//! ([`game`]). Layout numbers are in the game's 2160-high UI space.

pub const SIDE_MARGIN: f32 = 500.0;
pub const CONTENT_TOP: f32 = 360.0;
pub const FOOTER_H: f32 = 135.0;
pub const FOOTER_BOTTOM_GAP: f32 = 230.0;
pub const FRAME_PAD: f32 = 100.0;
pub const ROW_H: f32 = 120.0;
pub const CAPTION_H: f32 = 90.0;
pub const NAME_H: f32 = 120.0;
pub const GAP: f32 = 30.0;
pub const FRAME_BORDER_ABOVE_BOTTOM: f32 = 50.0;

pub const ALIGN_CENTRE: i32 = 0x12;
pub const ALIGN_LEFT: i32 = 0x11;
pub const ALIGN_RIGHT: i32 = 0x14;
pub const ALIGN_CAPTION: i32 = 0x21;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
}

/// Truncates toward zero, as the C++ `static_cast<int>` does.
pub fn fl(v: f32) -> f32 {
    v.trunc()
}

/// The big panel behind the screen's content.
pub fn lobby_frame(window: Rect) -> Rect {
    let footer_y = window.h - FOOTER_BOTTOM_GAP;
    let bottom = fl(footer_y + FOOTER_H * 0.5 + FRAME_BORDER_ABOVE_BOTTOM);
    Rect::new(
        SIDE_MARGIN,
        CONTENT_TOP,
        window.w - SIDE_MARGIN * 2.0,
        bottom - CONTENT_TOP,
    )
}

pub fn footer_button_width(window: Rect) -> f32 {
    fl((window.w - 1040.0) / 3.0)
}

/// The square map preview inside the map column, under the name.
pub fn preview_box(column: Rect) -> Rect {
    let available = column.h - NAME_H - GAP - ROW_H - GAP;
    let side = fl(available.min(column.w).max(0.0));
    Rect::new(
        fl(column.x + (column.w - side) * 0.5),
        column.y + NAME_H + GAP,
        side,
        side,
    )
}

#[cfg(all(windows, target_arch = "x86"))]
pub mod game {
    //! The game-facing kit. `Kit` is a bag of resolved pointers, `Copy`, built once.

    use core::ffi::{CStr, c_char, c_void};

    use super::*;
    use crate::symbols::Symbols;

    /// Packs a colour the way the game does: `0xAABBGGRR`.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
    }

    // Offsets in the game's Nuklear context (wc2r-plus `struct_offsets.h`).
    const CTX_CURRENT_WINDOW: usize = 0x3d28;
    const CTX_TEXT_COLOUR: usize = 0x20c;
    const CTX_SPACING_Y: usize = 0x2004;
    const WINDOW_PANEL: usize = 0x8c;
    const WINDOW_BUFFER: usize = 0x64;
    const PANEL_BOUNDS_X: usize = 0x08;
    const PANEL_OFFSET_Y: usize = 0x1c;
    const PANEL_AT_Y: usize = 0x24;
    const PANEL_ROW_HEIGHT: usize = 0x6c;
    /// Row icons in the map list (wc2r-plus `map_browser.cpp`).
    const ICON_SIZE: f32 = 46.0;
    const ICON_INSET: f32 = 26.0 + ICON_SIZE * 0.2;
    const FOLDER_COLOUR: u32 = rgba(226, 184, 68, 255);
    const MAP_COLOUR: u32 = rgba(96, 186, 108, 255);
    const ICON_HOLE: u32 = rgba(18, 14, 10, 220);
    const PLACEHOLDER_COLOUR: u32 = rgba(150, 142, 128, 200);
    const PANEL_CLIP: usize = 0x3c;
    const GROUP_FLAGS: u32 = 0x20;
    const TEXT_EDIT_FLAGS: u32 = 0x260;
    /// MSVC `std::string`: size at +0x10, capacity at +0x14, inline up to 15 bytes.
    const STR_CAPACITY: usize = 0x14;
    const STR_SSO_MAX: u32 = 15;

    type RectFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32);
    type LabelFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32);
    type WrappedLabelFn = unsafe extern "C" fn(*mut c_void, *const c_char, u32);
    type ButtonFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> i8;
    type GroupFn = unsafe extern "C" fn(*mut c_void, *const c_char, u32) -> i32;
    type CtxFn = unsafe extern "C" fn(*mut c_void);
    type SpaceBeginFn = unsafe extern "C" fn(*mut c_void, i32, f32, i32);
    type TextEditFn = unsafe extern "C" fn(*mut c_void, u32, *mut c_char, i32, *mut c_void) -> u32;
    type DropdownFn =
        unsafe extern "C" fn(*const c_char, *mut c_void, *const c_void, *mut u8, f32) -> i8;
    type SliderFn = unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, *mut u32) -> u8;
    type BannerFn = unsafe extern "C" fn();
    type LocalizedFn = unsafe extern "C" fn(*const c_char) -> *const c_char;
    type FormatFn = unsafe extern "C" fn(*const c_char, ...) -> *const c_char;
    type ButtonLabelFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> i32;
    type RowFn = unsafe extern "C" fn(*mut c_void, f32, i32);
    type FillRectFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32, f32, u32);
    type SlotDropdownFn = unsafe extern "C" fn(
        i8,
        i8,
        *const c_char,
        *mut c_void,
        *mut c_void,
        *mut i8,
        f32,
        i8,
    ) -> i8;
    type SetSkinFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> u32;
    type RowRatiosFn = unsafe extern "C" fn(*mut c_void, i32, f32, i32, *const f32);
    type CountLinesFn = unsafe extern "C" fn(*mut c_void, *const c_char, i32, f32) -> i32;
    type KeyPressedFn = unsafe extern "C" fn(*mut c_void, i32) -> i32;
    type WidgetSpaceFn = unsafe extern "C" fn(*mut f32, *mut c_void, *mut c_void, i32);
    type ClockFn = unsafe extern "C" fn() -> i64;

    // More of the Nuklear context and panel (wc2r-plus `struct_offsets.h`).
    const CTX_SEQ: usize = 0x3d34;
    const CTX_FONT: usize = 0x1e0;
    const CTX_MOUSE_X: usize = 0x1bc;
    const CTX_MOUSE_Y: usize = 0x1c0;
    const FONT_HEIGHT: usize = 0x04;
    const PANEL_ROW_INDEX: usize = 0x68;
    const PANEL_ROW_ITEM_OFFSET: usize = 0x84;

    /// NUL-terminated copy for the game's C-string widgets.
    pub fn cstr(text: &str) -> Vec<u8> {
        let mut v: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
        v.push(0);
        v
    }

    /// The text of a MSVC `std::string` the game owns.
    ///
    /// # Safety
    /// `s` must point at a live `std::string` (or be null).
    pub unsafe fn str_data(s: *const u8) -> String {
        if s.is_null() {
            return String::new();
        }
        unsafe {
            let cap = s.add(STR_CAPACITY).cast::<u32>().read_unaligned();
            let data = if cap > STR_SSO_MAX {
                s.cast::<*const c_char>().read_unaligned()
            } else {
                s.cast::<c_char>()
            };
            if data.is_null() {
                String::new()
            } else {
                crate::gamepath::decode(CStr::from_ptr(data).to_bytes())
            }
        }
    }

    #[derive(Clone, Copy)]
    pub struct Kit {
        ctx_slot: *const *mut c_void,
        rect: RectFn,
        label: LabelFn,
        wrapped_label: WrappedLabelFn,
        button: ButtonFn,
        begin_group: GroupFn,
        end_group: CtxFn,
        space_begin: SpaceBeginFn,
        space_end: CtxFn,
        text_edit: TextEditFn,
        text_edit_filter: *mut c_void,
        dropdown: DropdownFn,
        slider: SliderFn,
        banner: BannerFn,
        localized: LocalizedFn,
        format: FormatFn,
        disable_begin: CtxFn,
        disable_end: CtxFn,
        set_skin: SetSkinFn,
        set_font: SetSkinFn,
        style_manager: *mut c_void,
        button_label: ButtonLabelFn,
        row: RowFn,
        fill_rect: FillRectFn,
        slot_dropdown: SlotDropdownFn,
        row_ratios: RowRatiosFn,
        count_lines: CountLinesFn,
        key_pressed: KeyPressedFn,
        widget_space: WidgetSpaceFn,
        spacing: CtxFn,
        end_window: CtxFn,
        clock: ClockFn,
    }

    // SAFETY: plain game function and data addresses, fixed for the session.
    unsafe impl Send for Kit {}
    unsafe impl Sync for Kit {}

    macro_rules! sym {
        ($symbols:expr, $name:literal, $t:ty) => {
            unsafe { core::mem::transmute::<usize, $t>($symbols.address($name)?) }
        };
    }

    impl Kit {
        pub fn new(symbols: &Symbols) -> Option<Kit> {
            Kit::new_in(symbols, "NkContextMenus")
        }

        /// A kit that draws into another Nuklear context (`ctx_symbol` is the global holding it),
        /// such as the in-match HUD's (`HudCtx`).
        pub fn new_in(symbols: &Symbols, ctx_symbol: &str) -> Option<Kit> {
            Some(Kit {
                ctx_slot: symbols.address(ctx_symbol)? as *const *mut c_void,
                rect: sym!(symbols, "OverrideNextRect", RectFn),
                label: sym!(symbols, "DrawLabel", LabelFn),
                wrapped_label: sym!(symbols, "DrawWrappedLabel", WrappedLabelFn),
                button: sym!(symbols, "DrawMenuButton", ButtonFn),
                begin_group: sym!(symbols, "BeginNamedGroup", GroupFn),
                end_group: sym!(symbols, "EndGroup", CtxFn),
                space_begin: sym!(symbols, "LayoutSpaceBegin", SpaceBeginFn),
                space_end: sym!(symbols, "LayoutSpaceEnd", CtxFn),
                text_edit: sym!(symbols, "TextEdit", TextEditFn),
                text_edit_filter: symbols.address("TextEditFilter")? as *mut c_void,
                dropdown: sym!(symbols, "DrawLobbyDropdown", DropdownFn),
                slider: sym!(symbols, "SliderU32", SliderFn),
                banner: sym!(symbols, "DrawBanner", BannerFn),
                localized: sym!(symbols, "LookupLocalized", LocalizedFn),
                format: sym!(symbols, "FormatLocalized", FormatFn),
                disable_begin: sym!(symbols, "DisableBegin", CtxFn),
                disable_end: sym!(symbols, "DisableEnd", CtxFn),
                set_skin: sym!(symbols, "SetSkin", SetSkinFn),
                set_font: sym!(symbols, "SetFont", SetSkinFn),
                style_manager: symbols.address("SkinManager")? as *mut c_void,
                button_label: sym!(symbols, "ButtonLabel", ButtonLabelFn),
                row: sym!(symbols, "LayoutRowDynamic", RowFn),
                fill_rect: sym!(symbols, "NkFillRect", FillRectFn),
                slot_dropdown: sym!(symbols, "LobbySlotDropdown", SlotDropdownFn),
                row_ratios: sym!(symbols, "LayoutRowRatios", RowRatiosFn),
                count_lines: sym!(symbols, "CountWrappedLines", CountLinesFn),
                key_pressed: sym!(symbols, "InputKeyPressed", KeyPressedFn),
                widget_space: sym!(symbols, "NkLayoutWidgetSpace", WidgetSpaceFn),
                spacing: sym!(symbols, "Spacing", CtxFn),
                end_window: sym!(symbols, "EndCurrentWindow", CtxFn),
                clock: sym!(symbols, "MillisecondClock", ClockFn),
            })
        }

        /// A row of `ratios.len()` columns, each a fraction of the row, `height` tall.
        pub fn row_ratios(&self, height: f32, ratios: &[f32]) {
            unsafe {
                (self.row_ratios)(self.ctx(), 0, height, ratios.len() as i32, ratios.as_ptr())
            };
        }

        /// A row of `cols` equal columns.
        pub fn row_dynamic(&self, height: f32, cols: i32) {
            unsafe { (self.row)(self.ctx(), height, cols) };
        }

        /// Skips one cell of the current row.
        pub fn spacing(&self) {
            unsafe { (self.spacing)(self.ctx()) };
        }

        /// Ends the window the game began; call after the screen's own content is done.
        pub fn end_window(&self) {
            unsafe { (self.end_window)(self.ctx()) };
        }

        /// How many lines the game's wrapped label would take for `text` at `wrap_width`.
        pub fn count_lines(&self, text: &str, wrap_width: f32) -> i32 {
            let t = cstr(text);
            unsafe {
                (self.count_lines)(
                    self.ctx(),
                    t.as_ptr().cast(),
                    (t.len() - 1) as i32,
                    wrap_width,
                )
            }
        }

        /// A wrapped label in the current layout cell, in `colour`.
        pub fn wrapped_label(&self, text: &str, colour: u32) {
            let t = cstr(text);
            unsafe { (self.wrapped_label)(self.ctx(), t.as_ptr().cast(), colour) };
        }

        /// The colour the game is drawing text in right now.
        pub fn current_text_colour(&self) -> u32 {
            self.text_colour()
        }

        pub fn key_pressed(&self, key: i32) -> bool {
            unsafe { (self.key_pressed)(self.ctx(), key) != 0 }
        }

        /// The game's own millisecond clock.
        pub fn clock_ms(&self) -> i64 {
            unsafe { (self.clock)() }
        }

        /// Greys out everything until [`Kit::disable_end`].
        pub fn disable_begin(&self) {
            unsafe { (self.disable_begin)(self.ctx()) };
        }

        pub fn disable_end(&self) {
            unsafe { (self.disable_end)(self.ctx()) };
        }

        /// The mouse position in window coordinates.
        pub fn mouse(&self) -> (f32, f32) {
            let ctx = self.ctx() as *const u8;
            if ctx.is_null() {
                return (-1.0, -1.0);
            }
            unsafe {
                (
                    ctx.add(CTX_MOUSE_X).cast::<f32>().read_unaligned(),
                    ctx.add(CTX_MOUSE_Y).cast::<f32>().read_unaligned(),
                )
            }
        }

        /// The context's sequence counter; it jumps by more than one when something clears the
        /// frame's drawing mid-frame.
        pub fn ctx_seq(&self) -> u32 {
            let ctx = self.ctx() as *const u8;
            if ctx.is_null() {
                return 0;
            }
            unsafe { ctx.add(CTX_SEQ).cast::<u32>().read_unaligned() }
        }

        /// The height of the font the game is drawing with.
        pub fn font_height(&self) -> f32 {
            let ctx = self.ctx() as *const u8;
            if ctx.is_null() {
                return CAPTION_H;
            }
            unsafe {
                let font = ctx.add(CTX_FONT).cast::<*const u8>().read_unaligned();
                if font.is_null() {
                    CAPTION_H
                } else {
                    font.add(FONT_HEIGHT).cast::<f32>().read_unaligned()
                }
            }
        }

        /// The vertical scroll of the current group.
        pub fn scroll_offset_y(&self) -> f32 {
            let p = self.panel();
            if p.is_null() {
                return 0.0;
            }
            unsafe {
                let at = p.add(PANEL_OFFSET_Y).cast::<*const u32>().read_unaligned();
                if at.is_null() { 0.0 } else { *at as f32 }
            }
        }

        pub fn set_scroll_offset_y(&self, value: f32) {
            let p = self.panel();
            if p.is_null() {
                return;
            }
            unsafe {
                let at = p.add(PANEL_OFFSET_Y).cast::<*mut u32>().read_unaligned();
                if !at.is_null() {
                    *at = if value > 0.0 { value as u32 } else { 0 };
                }
            }
        }

        /// The rectangle of the first cell of the row last laid out in the current group
        /// (wc2r-plus `LastRowFirstCell`).
        pub fn last_row_first_cell(&self) -> Option<Rect> {
            let p = self.panel() as *mut u8;
            if p.is_null() {
                return None;
            }
            unsafe {
                let index = p.add(PANEL_ROW_INDEX).cast::<i32>().read_unaligned();
                let offset = p.add(PANEL_ROW_ITEM_OFFSET).cast::<f32>().read_unaligned();
                if index <= 0 {
                    return None;
                }
                p.add(PANEL_ROW_INDEX).cast::<i32>().write_unaligned(0);
                p.add(PANEL_ROW_ITEM_OFFSET)
                    .cast::<f32>()
                    .write_unaligned(0.0);
                let mut b = [0f32; 4];
                (self.widget_space)(b.as_mut_ptr(), self.ctx(), core::ptr::null_mut(), 0);
                p.add(PANEL_ROW_INDEX).cast::<i32>().write_unaligned(index);
                p.add(PANEL_ROW_ITEM_OFFSET)
                    .cast::<f32>()
                    .write_unaligned(offset);
                (b[2] > 0.0 && b[3] > 0.0).then_some(Rect::new(b[0], b[1], b[2], b[3]))
            }
        }

        /// A group at `r` with explicit window `flags` (0x20: no scrollbar).
        pub fn begin_group_flags(&self, r: Rect, name: &CStr, flags: u32) -> bool {
            self.set_rect(r);
            unsafe { (self.begin_group)(self.ctx(), name.as_ptr(), flags) != 0 }
        }

        pub fn ctx(&self) -> *mut c_void {
            unsafe { *self.ctx_slot }
        }

        /// The rectangle of the window being built, from its panel's clip.
        pub fn window_rect(&self) -> Option<Rect> {
            let ctx = self.ctx() as *const u8;
            if ctx.is_null() {
                return None;
            }
            unsafe {
                let win = ctx
                    .add(CTX_CURRENT_WINDOW)
                    .cast::<*const u8>()
                    .read_unaligned();
                if win.is_null() {
                    return None;
                }
                let panel = win.add(WINDOW_PANEL).cast::<*const u8>().read_unaligned();
                if panel.is_null() {
                    return None;
                }
                let c = panel.add(PANEL_CLIP).cast::<[f32; 4]>().read_unaligned();
                (c[2] > 100.0 && c[3] > 100.0).then_some(Rect::new(c[0], c[1], c[2], c[3]))
            }
        }

        fn panel(&self) -> *const u8 {
            let ctx = self.ctx() as *const u8;
            if ctx.is_null() {
                return core::ptr::null();
            }
            unsafe {
                let win = ctx
                    .add(CTX_CURRENT_WINDOW)
                    .cast::<*const u8>()
                    .read_unaligned();
                if win.is_null() {
                    core::ptr::null()
                } else {
                    win.add(WINDOW_PANEL).cast::<*const u8>().read_unaligned()
                }
            }
        }

        pub fn canvas(&self) -> *mut c_void {
            let ctx = self.ctx() as *const u8;
            if ctx.is_null() {
                return core::ptr::null_mut();
            }
            unsafe {
                let win = ctx
                    .add(CTX_CURRENT_WINDOW)
                    .cast::<*const u8>()
                    .read_unaligned();
                if win.is_null() {
                    core::ptr::null_mut()
                } else {
                    win.add(WINDOW_BUFFER) as *mut c_void
                }
            }
        }

        fn text_colour(&self) -> u32 {
            unsafe {
                (self.ctx() as *const u8)
                    .add(CTX_TEXT_COLOUR)
                    .cast::<u32>()
                    .read_unaligned()
            }
        }

        fn set_text_colour(&self, v: u32) {
            unsafe {
                (self.ctx() as *mut u8)
                    .add(CTX_TEXT_COLOUR)
                    .cast::<u32>()
                    .write_unaligned(v)
            };
        }

        #[allow(clippy::too_many_arguments)]
        fn fill(&self, canvas: *mut c_void, x: f32, y: f32, w: f32, h: f32, round: f32, col: u32) {
            if !canvas.is_null() {
                unsafe { (self.fill_rect)(canvas, x, y, w, h, round, col) };
            }
        }

        /// A filled rounded rectangle (`round` is the corner radius; half the side makes a circle).
        pub fn fill_round(&self, r: Rect, round: f32, colour: u32) {
            self.fill(self.canvas(), r.x, r.y, r.w, r.h, round, colour);
        }

        /// A filled rectangle on the window being built.
        pub fn fill_rect(&self, r: Rect, colour: u32) {
            self.fill(self.canvas(), r.x, r.y, r.w, r.h, 0.0, colour);
        }

        /// A scrolling group at `r`; true when it opened (then draw rows and call `end_list`).
        pub fn begin_list(&self, r: Rect, name: &CStr) -> bool {
            self.set_rect(r);
            unsafe { (self.begin_group)(self.ctx(), name.as_ptr(), 0) != 0 }
        }

        pub fn end_list(&self) {
            unsafe { (self.end_group)(self.ctx()) };
        }

        /// The top of the next row in the current group's layout (wc2r-plus `NextRowTop`).
        pub fn next_row_top(&self) -> f32 {
            let p = self.panel();
            if p.is_null() {
                return 0.0;
            }
            unsafe {
                p.add(PANEL_AT_Y).cast::<f32>().read_unaligned()
                    + p.add(PANEL_ROW_HEIGHT).cast::<f32>().read_unaligned()
            }
        }

        /// One full-width list row with a button; true when clicked.
        pub fn list_row_button(&self, label: &str) -> bool {
            let ctx = self.ctx();
            unsafe { (self.row)(ctx, ROW_H, 1) };
            let t = cstr(label);
            unsafe { (self.button_label)(ctx, t.as_ptr().cast()) != 0 }
        }

        /// The folder or map icon at the left of the row that began at `row_top`.
        pub fn draw_row_icon(&self, row_top: f32, folder: bool) {
            let p = self.panel();
            let canvas = self.canvas();
            if p.is_null() || canvas.is_null() {
                return;
            }
            let (bounds_x, scroll) = unsafe {
                let sp = p.add(PANEL_OFFSET_Y).cast::<*const u32>().read_unaligned();
                (
                    p.add(PANEL_BOUNDS_X).cast::<f32>().read_unaligned(),
                    if sp.is_null() { 0.0 } else { *sp as f32 },
                )
            };
            let x = bounds_x + ICON_INSET;
            let y = row_top - scroll + (ROW_H - ICON_SIZE) * 0.5;
            if folder {
                self.fill(
                    canvas,
                    x,
                    y,
                    ICON_SIZE * 0.55,
                    ICON_SIZE * 0.24,
                    2.0,
                    FOLDER_COLOUR,
                );
                self.fill(
                    canvas,
                    x,
                    y + ICON_SIZE * 0.24,
                    ICON_SIZE,
                    ICON_SIZE * 0.76,
                    2.0,
                    FOLDER_COLOUR,
                );
            } else {
                self.fill(canvas, x, y, ICON_SIZE, ICON_SIZE, 2.0, MAP_COLOUR);
                self.fill(
                    canvas,
                    x + ICON_SIZE * 0.28,
                    y + ICON_SIZE * 0.28,
                    ICON_SIZE * 0.44,
                    ICON_SIZE * 0.44,
                    1.0,
                    ICON_HOLE,
                );
            }
        }

        /// A text field with a dim placeholder while it is empty and unfocused.
        ///
        /// # Safety
        /// `buf` must be a writable buffer of at least `max_len` bytes holding a C string.
        pub unsafe fn filter_field_at(
            &self,
            r: Rect,
            buf: *mut u8,
            max_len: i32,
            placeholder: &str,
            skin: Option<u32>,
        ) -> bool {
            let focused = unsafe { self.text_edit_at(r, buf, max_len, skin) };
            if !focused && unsafe { *buf } == 0 {
                let previous = self.text_colour();
                self.set_text_colour(PLACEHOLDER_COLOUR);
                self.label_at(
                    Rect::new(r.x + GAP, r.y, r.w - GAP, r.h),
                    placeholder,
                    ALIGN_LEFT,
                );
                self.set_text_colour(previous);
            }
            focused
        }

        /// A read-only box showing `text`: the game's slot dropdown, disabled, over a fake
        /// `std::string` (wc2r-plus `LockedBoxAt`). `id` must not be a `slotTypeN` cell.
        pub fn locked_box_at(&self, r: Rect, text: &str, id: &CStr) {
            let bytes: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
            let mut heap = bytes.clone();
            heap.push(0);
            let mut view = [0u8; 0x18];
            let len = bytes.len();
            if len <= STR_SSO_MAX as usize {
                view[..len].copy_from_slice(&bytes);
                view[STR_CAPACITY..STR_CAPACITY + 4].copy_from_slice(&STR_SSO_MAX.to_le_bytes());
            } else {
                view[..4].copy_from_slice(&(heap.as_ptr() as usize as u32).to_le_bytes());
                view[STR_CAPACITY..STR_CAPACITY + 4].copy_from_slice(&(len as u32).to_le_bytes());
            }
            view[0x10..0x14].copy_from_slice(&(len as u32).to_le_bytes());
            self.set_rect(r);
            let mut unused: i8 = 0;
            unsafe {
                (self.slot_dropdown)(
                    1,
                    0,
                    id.as_ptr(),
                    view.as_mut_ptr().cast(),
                    core::ptr::null_mut(),
                    &mut unused,
                    r.h,
                    0,
                );
            }
        }

        /// The "up" arrow, in rectangles, over a button.
        pub fn draw_up_arrow(&self, button: Rect, greyed: bool) {
            let canvas = self.canvas();
            let mut colour = self.text_colour();
            if greyed {
                colour = (colour & 0x00ff_ffff) | (0x60 << 24);
            }
            let steps = 9usize;
            let side = button.w.min(button.h) * 0.46;
            let cx = button.x + button.w * 0.5;
            let top = button.y + (button.h - side) * 0.5;
            let step_h = side / steps as f32;
            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let head_w = side * t;
                let shaft_w = side * 0.34;
                let w = if i < steps * 2 / 3 { head_w } else { shaft_w };
                if w <= 0.0 {
                    continue;
                }
                self.fill(
                    canvas,
                    cx - w * 0.5,
                    top + step_h * i as f32,
                    w,
                    step_h + 1.0,
                    0.0,
                    colour,
                );
            }
        }

        /// The reload icon in rectangles (the texture version comes with the preview work).
        pub fn draw_reload_icon(&self, button: Rect, greyed: bool) {
            let canvas = self.canvas();
            let side = button.w.min(button.h) * 0.58;
            let bx = fl(button.x + (button.w - side) * 0.5);
            let by = fl(button.y + (button.h - side) * 0.5 - 2.5);
            let mut colour = self.text_colour();
            if greyed {
                colour = (colour & 0x00ff_ffff) | (0x60 << 24);
            }
            let cx = bx + side * 0.5;
            let cy = by + side * 0.5;
            let radius = side * 0.5;
            let thick = (side * 0.17).max(2.0);
            const TAU: f32 = 6.283_185_5;
            const GAP_TURNS: f32 = 0.13;
            const SEGMENTS: usize = 30;
            for i in 0..=SEGMENTS {
                let turn = GAP_TURNS + (1.0 - GAP_TURNS) * i as f32 / SEGMENTS as f32;
                self.fill(
                    canvas,
                    cx + radius * (turn * TAU).sin() - thick * 0.5,
                    cy - radius * (turn * TAU).cos() - thick * 0.5,
                    thick,
                    thick,
                    0.0,
                    colour,
                );
            }
            let head_len = thick * 2.4;
            let head_half = thick * 1.5;
            const HEAD_STEPS: usize = 7;
            for i in 0..HEAD_STEPS {
                let half = head_half * (1.0 - i as f32 / HEAD_STEPS as f32);
                let col_w = head_len / HEAD_STEPS as f32;
                self.fill(
                    canvas,
                    cx - head_len * 0.45 + col_w * i as f32,
                    cy - radius - half,
                    col_w + 1.0,
                    half * 2.0,
                    0.0,
                    colour,
                );
            }
        }

        pub fn push_skin(&self, id: u32) -> u32 {
            unsafe { (self.set_skin)(self.style_manager, id) }
        }

        pub fn pop_skin(&self, previous: u32) {
            unsafe { (self.set_skin)(self.style_manager, previous) };
        }

        pub fn push_font(&self, id: u32) -> u32 {
            unsafe { (self.set_font)(self.style_manager, id) }
        }

        pub fn pop_font(&self, previous: u32) {
            unsafe { (self.set_font)(self.style_manager, previous) };
        }

        /// Opens the screen's layout space; pair with [`Kit::space_end`].
        pub fn space_begin(&self) {
            unsafe { (self.space_begin)(self.ctx(), 1, 0.0, 1000) };
        }

        pub fn space_end(&self) {
            unsafe { (self.space_end)(self.ctx()) };
        }

        pub fn set_rect(&self, r: Rect) {
            unsafe { (self.rect)(self.ctx(), r.x, r.y, r.w, r.h) };
        }

        pub fn banner(&self) {
            unsafe { (self.banner)() };
        }

        pub fn label(&self, text: &str, align: i32) {
            let t = cstr(text);
            unsafe { (self.label)(self.ctx(), t.as_ptr().cast(), align) };
        }

        pub fn label_at(&self, r: Rect, text: &str, align: i32) {
            self.set_rect(r);
            self.label(text, align);
        }

        /// A label in the heading font when `font` is given.
        pub fn heading_at(&self, r: Rect, text: &str, align: i32, font: Option<u32>) {
            let previous = font.map(|f| self.push_font(f));
            self.label_at(r, text, align);
            if let Some(p) = previous {
                self.pop_font(p);
            }
        }

        pub fn wrapped_label_at(&self, r: Rect, text: &str) {
            if text.is_empty() {
                return;
            }
            self.set_rect(r);
            let t = cstr(text);
            let colour = unsafe {
                (self.ctx() as *const u8)
                    .add(CTX_TEXT_COLOUR)
                    .cast::<u32>()
                    .read_unaligned()
            };
            unsafe { (self.wrapped_label)(self.ctx(), t.as_ptr().cast(), colour) };
        }

        /// A button at `r`; a greyed one is drawn disabled and never reports a click.
        pub fn button_at(&self, r: Rect, text: &str, greyed: bool) -> bool {
            let ctx = self.ctx();
            if greyed {
                unsafe { (self.disable_begin)(ctx) };
            }
            self.set_rect(r);
            let t = cstr(text);
            let clicked = unsafe { (self.button)(ctx, t.as_ptr().cast()) } != 0;
            if greyed {
                unsafe { (self.disable_end)(ctx) };
            }
            clicked && !greyed
        }

        /// A framed group at `r`, drawn under `skin` (wc2r-plus `PanelFrame`).
        pub fn panel_frame(&self, r: Rect, name: &CStr, skin: Option<u32>) {
            let previous = skin.map(|s| self.push_skin(s));
            self.set_rect(r);
            let opened = unsafe { (self.begin_group)(self.ctx(), name.as_ptr(), GROUP_FLAGS) };
            if let Some(p) = previous {
                self.pop_skin(p);
            }
            if opened != 0 {
                unsafe { (self.end_group)(self.ctx()) };
            }
        }

        /// A text field editing `buf` (`max_len` bytes including the NUL). True when focused.
        ///
        /// # Safety
        /// `buf` must be a writable buffer of at least `max_len` bytes.
        pub unsafe fn text_edit_at(
            &self,
            r: Rect,
            buf: *mut u8,
            max_len: i32,
            skin: Option<u32>,
        ) -> bool {
            let ctx = self.ctx();
            let spacing = unsafe {
                (ctx as *const u8)
                    .add(CTX_SPACING_Y)
                    .cast::<f32>()
                    .read_unaligned()
            };
            let set_spacing = |v: f32| unsafe {
                (ctx as *mut u8)
                    .add(CTX_SPACING_Y)
                    .cast::<f32>()
                    .write_unaligned(v)
            };
            self.set_rect(r);
            let previous = skin.map(|s| self.push_skin(s));
            set_spacing(spacing);
            let state = unsafe {
                (self.text_edit)(
                    ctx,
                    TEXT_EDIT_FLAGS,
                    buf.cast(),
                    max_len,
                    self.text_edit_filter,
                )
            };
            if let Some(p) = previous {
                self.pop_skin(p);
            }
            set_spacing(spacing);
            state & 1 != 0
        }

        /// One of the lobby option dropdowns (a `std::string` current value over an option list).
        ///
        /// # Safety
        /// The pointers are the game's own lobby option objects.
        pub unsafe fn dropdown(
            &self,
            id: &CStr,
            current: *mut c_void,
            options: *const c_void,
            open: *mut u8,
        ) -> bool {
            unsafe { (self.dropdown)(id.as_ptr(), current, options, open, ROW_H) != 0 }
        }

        /// The game's unsigned slider over `[min, max]` step 1.
        ///
        /// # Safety
        /// `value` must be a writable `u32` of the game's.
        pub unsafe fn slider(&self, value: *mut u32, min: u32, max: u32) -> bool {
            let (mut lo, mut hi, mut step) = (min, max, 1u32);
            unsafe { (self.slider)(value, &mut lo, &mut hi, &mut step) != 0 }
        }

        /// The game's localized text for `key`, or the key itself when it has none.
        pub fn localized(&self, key: &CStr) -> String {
            unsafe {
                let p = (self.localized)(key.as_ptr());
                if p.is_null() {
                    key.to_string_lossy().into_owned()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            }
        }

        /// `FormatLocalized(key, arg)` for a key with one `%s`-style argument.
        pub fn format_localized(&self, key: &CStr, arg: &str) -> String {
            let a = cstr(arg);
            unsafe {
                let p = (self.format)(key.as_ptr(), a.as_ptr());
                if p.is_null() {
                    key.to_string_lossy().into_owned()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Rect = Rect::new(0.0, 0.0, 3840.0, 2160.0);

    #[test]
    fn the_frame_leaves_the_side_margins_and_ends_below_the_footer_row() {
        let f = lobby_frame(WINDOW);
        assert_eq!(f.x, 500.0);
        assert_eq!(f.y, 360.0);
        assert_eq!(f.w, 3840.0 - 1000.0);
        // footer at 2160 - 230 = 1930; its middle is 1997.5; the border sits 50 below that.
        assert_eq!(f.y + f.h, 2047.0);
    }

    #[test]
    fn footer_buttons_take_a_third_of_what_the_margins_leave() {
        assert_eq!(footer_button_width(WINDOW), 933.0);
        assert_eq!(footer_button_width(Rect::new(0.0, 0.0, 1040.0, 100.0)), 0.0);
    }

    #[test]
    fn the_preview_is_a_centred_square_that_fits_the_column() {
        let wide = Rect::new(100.0, 400.0, 900.0, 1300.0);
        let b = preview_box(wide);
        assert_eq!(b.w, b.h);
        assert_eq!(b.w, 900.0, "the width is the limit");
        assert_eq!(b.x, 100.0);
        assert_eq!(b.y, 400.0 + NAME_H + GAP);
        let tall = Rect::new(0.0, 0.0, 2000.0, 700.0);
        let b = preview_box(tall);
        assert_eq!(
            b.w,
            700.0 - NAME_H - GAP - ROW_H - GAP,
            "the height is the limit"
        );
        assert_eq!(b.x, fl((2000.0 - b.w) * 0.5));
        let tiny = preview_box(Rect::new(0.0, 0.0, 500.0, 100.0));
        assert_eq!(tiny.w, 0.0, "never negative");
    }
}
