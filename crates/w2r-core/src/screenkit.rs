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

    // Offsets in the game's Nuklear context (wc2r-plus `struct_offsets.h`).
    const CTX_CURRENT_WINDOW: usize = 0x3d28;
    const CTX_TEXT_COLOUR: usize = 0x20c;
    const CTX_SPACING_Y: usize = 0x2004;
    const WINDOW_PANEL: usize = 0x8c;
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
    type SetSkinFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> u32;

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
                CStr::from_ptr(data).to_string_lossy().into_owned()
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
            Some(Kit {
                ctx_slot: symbols.address("NkContextMenus")? as *const *mut c_void,
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
            })
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
