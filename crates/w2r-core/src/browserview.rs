//! The map browser on the game (docs/create-screen.md, slice 2): [`GameSource`] reads and drives
//! the game's own map list, folder list and size dropdown, and [`View`] draws the "Select Map"
//! panel and publishes the choice. The rules live in [`crate::mapbrowser`]; layout numbers are
//! wc2r-plus `ui/map_browser.cpp`'s. x86 only.

use core::ffi::{CStr, c_char, c_void};

use crate::mapbrowser::{
    Browser, Choice, LiveEntry, MAX_SEARCH, SizeFilter, Source, contains_no_case, sanitize,
};
use crate::screenkit::game::{Kit, str_data};
use crate::screenkit::*;
use crate::symbols::Symbols;

const BROWSER_W_FRACTION: f32 = 0.80;
const BROWSER_H_FRACTION: f32 = 0.88;
const BROWSER_LEFT_FRACTION: f32 = 0.56;

/// A map-list entry in the game: stride and where its fields sit (wc2r-plus `struct_offsets.h`).
const ENTRY_STRIDE: usize = 0x68;
const ENTRY_PLAYERS: usize = 0x04;
const ENTRY_DIM: usize = 0x06;
const ENTRY_NAME: usize = 0x08;
const ENTRY_PATH: usize = 0x20;
const ENTRY_DESC: usize = 0x50;
/// The pending game setup's name field, which the game reads when the lobby is created.
const PENDING_SETUP_NAME: usize = 0x10;
const PENDING_SETUP_NAME_CAPACITY: usize = 0xdc;
/// A folder name in the game's folder list: a `std::string` each.
const STRING_STRIDE: usize = 0x18;
/// Sanity limits on what we read out of the game's vectors.
const MAX_LIVE_ENTRIES: usize = 20_000;
const MAX_FOLDERS: usize = 2_000;
const MAX_SIZE_OPTIONS: usize = 32;

type RefreshFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void);
type AssignFn = unsafe extern "thiscall" fn(*mut c_void, *const c_char, usize) -> *mut c_void;
type CopyEntryFn = unsafe extern "thiscall" fn(*mut c_void, *const c_void) -> *mut c_void;
type ValidateFn = unsafe extern "C" fn(*const c_char) -> i8;

/// The skins and font the panel is drawn with.
#[derive(Clone, Copy)]
pub struct Theme {
    pub right_frame: u32,
    pub left_frame: u32,
    pub text_edit: u32,
    pub heading_font: u32,
}

/// The game's lobby map list and its helpers. Plain pointers, so copies are cheap.
#[derive(Clone, Copy)]
pub struct GameSource {
    /// `LobbyMapListBegin`: the address of the list's `begin` pointer (the vector starts here).
    list_begin: *mut *mut u8,
    list_end: *const *mut u8,
    /// `LobbyFolderList`: a vector of folder-name strings.
    folder_list: *const *mut u8,
    rel_path: *mut c_void,
    folder_current: *mut c_void,
    size_current: *const u8,
    size_options: *const *const *const c_char,
    refresh: RefreshFn,
    assign: AssignFn,
}

// SAFETY: addresses of the game's functions and globals, fixed for the session.
unsafe impl Send for GameSource {}
unsafe impl Sync for GameSource {}

impl GameSource {
    pub fn new(symbols: &Symbols) -> Result<GameSource, &'static str> {
        let at = |n: &'static str| symbols.address(n).ok_or(n);
        Ok(GameSource {
            list_begin: at("LobbyMapListBegin")? as *mut *mut u8,
            list_end: at("LobbyMapListEnd")? as *const *mut u8,
            folder_list: at("LobbyFolderList")? as *const *mut u8,
            rel_path: at("LobbyFolderRelPath")? as *mut c_void,
            folder_current: at("LobbyFolderCurrent")? as *mut c_void,
            size_current: at("LobbyMapSizeCurrent")? as *const u8,
            size_options: at("LobbyMapSizeOptions")? as *const *const *const c_char,
            refresh: unsafe {
                core::mem::transmute::<usize, RefreshFn>(at("RefreshMapListFromFolder")?)
            },
            assign: unsafe { core::mem::transmute::<usize, AssignFn>(at("StringAssign")?) },
        })
    }

    fn assign_str(&self, target: *mut c_void, text: &str) {
        let bytes: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
        unsafe { (self.assign)(target, bytes.as_ptr().cast(), bytes.len()) };
    }

    fn entry_ptrs(&self) -> Vec<*mut u8> {
        let (begin, end) = unsafe { (*self.list_begin, *self.list_end) };
        if begin.is_null() || end <= begin {
            return Vec::new();
        }
        let n = ((end as usize - begin as usize) / ENTRY_STRIDE).min(MAX_LIVE_ENTRIES);
        (0..n)
            .map(|i| unsafe { begin.add(i * ENTRY_STRIDE) })
            .collect()
    }

    /// The size dropdown's option keys and its current value.
    fn size_filter(&self) -> SizeFilter {
        let mut options = Vec::new();
        unsafe {
            // The option list is a vector of `const char*` (begin, end, cap).
            let begin = *self.size_options;
            let end = *self.size_options.add(1);
            if !begin.is_null() && end >= begin {
                let n = ((end as usize - begin as usize) / 4).min(MAX_SIZE_OPTIONS);
                for i in 0..n {
                    let p = *begin.add(i);
                    if !p.is_null() {
                        options.push(CStr::from_ptr(p).to_string_lossy().into_owned());
                    }
                }
            }
        }
        SizeFilter::new(&options, &unsafe { str_data(self.size_current) })
    }

    fn live_at(&self, e: *const u8) -> LiveEntry {
        unsafe {
            LiveEntry {
                name: sanitize(&str_data(e.add(ENTRY_NAME))),
                path: sanitize(&str_data(e.add(ENTRY_PATH))),
                desc: sanitize(&str_data(e.add(ENTRY_DESC))),
                players: *e.add(ENTRY_PLAYERS) as u32,
                dim: e.add(ENTRY_DIM).cast::<u16>().read_unaligned() as u32,
            }
        }
    }
}

impl Source for GameSource {
    fn rel_path(&self) -> String {
        unsafe { str_data(self.rel_path as *const u8) }
    }

    fn set_rel_path(&mut self, rel: &str) {
        self.assign_str(self.rel_path, rel);
        unsafe {
            (self.refresh)(
                self.list_begin.cast(),
                self.folder_list as *mut c_void,
                self.rel_path,
            )
        };
    }

    fn set_folder_label(&mut self, label: &str) {
        self.assign_str(self.folder_current, label);
    }

    fn live_entries(&self) -> Vec<LiveEntry> {
        self.entry_ptrs()
            .into_iter()
            .map(|e| self.live_at(e))
            .collect()
    }

    fn folder_names(&self) -> Vec<String> {
        let (begin, end) = unsafe { (*self.folder_list, *self.folder_list.add(1)) };
        if begin.is_null() || end <= begin {
            return Vec::new();
        }
        let n = ((end as usize - begin as usize) / STRING_STRIDE).min(MAX_FOLDERS);
        (0..n)
            .map(|i| sanitize(&unsafe { str_data(begin.add(i * STRING_STRIDE)) }))
            .filter(|n| !n.is_empty() && n != "..")
            .collect()
    }
}

/// Everything the browser needs beyond the list: the screen's selection and how to publish.
#[derive(Clone, Copy)]
pub struct View {
    pub source: GameSource,
    selected: *mut u8,
    pending_setup: *mut u8,
    size_combo_open: *mut u8,
    copy_entry: CopyEntryFn,
    validate: ValidateFn,
}

// SAFETY: as above.
unsafe impl Send for View {}
unsafe impl Sync for View {}

impl View {
    pub fn new(symbols: &Symbols) -> Result<View, &'static str> {
        let at = |n: &'static str| symbols.address(n).ok_or(n);
        Ok(View {
            source: GameSource::new(symbols)?,
            selected: at("LobbyCreationSelected")? as *mut u8,
            pending_setup: at("PendingGameSetup")? as *mut u8,
            size_combo_open: at("LobbyComboOpenMapSize")? as *mut u8,
            copy_entry: unsafe {
                core::mem::transmute::<usize, CopyEntryFn>(at("CopyMapListEntry")?)
            },
            validate: unsafe { core::mem::transmute::<usize, ValidateFn>(at("ValidateMapFile")?) },
        })
    }

    /// The map the game currently has selected, as a choice.
    pub fn selected_choice(&self) -> Choice {
        let e = self.selected as *const u8;
        let live = self.source.live_at(e);
        Choice {
            name: live.name,
            path: live.path,
            desc: live.desc,
            rel_dir: self.source.rel_path(),
            players: live.players,
            dim: live.dim,
        }
    }

    /// Makes entry `index` of the live list the game's selected map (wc2r-plus `PublishChoice`),
    /// and records its name in the pending setup when the file validates.
    fn publish(&self, index: usize) -> bool {
        let entries = self.source.entry_ptrs();
        let Some(&entry) = entries.get(index) else {
            return false;
        };
        unsafe {
            (self.copy_entry)(self.selected.cast(), entry.cast());
            let path = self.source.live_at(self.selected).path;
            let Ok(c_path) = std::ffi::CString::new(path) else {
                return true;
            };
            if (self.validate)(c_path.as_ptr()) != 0 {
                let name = str_data(self.selected.add(ENTRY_NAME));
                let bytes = name.as_bytes();
                let n = bytes.len().min(PENDING_SETUP_NAME_CAPACITY - 1);
                let dst = self.pending_setup.add(PENDING_SETUP_NAME);
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
                *dst.add(n) = 0;
            }
        }
        true
    }

    fn list_row(&self, k: &Kit, label: &str, folder: bool) -> bool {
        let top = k.next_row_top();
        let clicked = k.list_row_button(label);
        k.draw_row_icon(top, folder);
        clicked
    }

    unsafe fn draw_list(&self, k: &Kit, b: &mut Browser, r: Rect) {
        if !k.begin_list(r, c"select_map_list") {
            return;
        }
        let mut src = self.source;
        let filter = src.size_filter();
        if b.searching() {
            let needle = b.search().to_owned();
            let mut picked = None;
            for m in b.indexed() {
                if filter.passes(m.dim)
                    && contains_no_case(&m.name, &needle)
                    && self.list_row(k, &m.name, false)
                {
                    picked = Some(m.clone());
                }
            }
            if let Some(m) = picked {
                b.choose_indexed(&m);
            }
            k.end_list();
            return;
        }
        for folder in src.folder_names() {
            if self.list_row(k, &folder, true) {
                b.descend(&mut src, &folder);
                // The list changed under us; the rest of the frame would read stale rows.
                k.end_list();
                return;
            }
        }
        let rel = src.rel_path();
        let mut picked = None;
        for e in src.live_entries() {
            if filter.passes(e.dim) && self.list_row(k, &e.name, false) {
                picked = Some(e);
            }
        }
        if let Some(e) = picked {
            b.choose_live(&e, &rel);
        }
        k.end_list();
    }

    /// Draws the "Select Map" panel. `window` is the screen's window. A confirmed choice is
    /// published and the browser closed.
    ///
    /// # Safety
    /// Must run inside the screen's layout space, on the game's main thread.
    pub unsafe fn draw(&self, k: &Kit, b: &mut Browser, window: Rect, theme: Theme) {
        let mut src = self.source;
        b.observe_root(&src.rel_path());

        let browser_w = fl(window.w * BROWSER_W_FRACTION);
        let browser_h = fl(window.h * BROWSER_H_FRACTION);
        let frame = Rect::new(
            fl((window.w - browser_w) * 0.5),
            fl((window.h - browser_h) * 0.5),
            browser_w,
            browser_h,
        );
        k.panel_frame(frame, c"select_map", Some(theme.right_frame));

        let x0 = frame.x + FRAME_PAD;
        let inner_right = frame.x + browser_w - FRAME_PAD;
        let inner_w = inner_right - x0;
        k.heading_at(
            Rect::new(x0, frame.y + FRAME_PAD, inner_w, NAME_H),
            "Select Map",
            ALIGN_CENTRE,
            Some(theme.heading_font),
        );

        let body_y = frame.y + FRAME_PAD + NAME_H + GAP;
        let body_bottom = frame.y + browser_h - FRAME_PAD;
        let left_w = fl(inner_w * BROWSER_LEFT_FRACTION);
        let right_x = x0 + left_w + GAP * 2.0;
        let right_w = inner_right - right_x;

        k.set_rect(Rect::new(x0, body_y, left_w, ROW_H));
        unsafe {
            k.dropdown(
                c"mapsize",
                self.source.size_current as *mut c_void,
                self.source.size_options as *const c_void,
                self.size_combo_open,
            )
        };

        let search_y = body_y + ROW_H + GAP;
        let mut buf = [0u8; MAX_SEARCH + 1];
        let text = b.search().as_bytes();
        buf[..text.len().min(MAX_SEARCH)].copy_from_slice(&text[..text.len().min(MAX_SEARCH)]);
        unsafe {
            k.filter_field_at(
                Rect::new(x0, search_y, left_w, ROW_H),
                buf.as_mut_ptr(),
                buf.len() as i32,
                "Search all maps",
                Some(theme.text_edit),
            )
        };
        let typed = buf.iter().position(|&c| c == 0).unwrap_or(MAX_SEARCH);
        let typed = String::from_utf8_lossy(&buf[..typed]).into_owned();
        if typed != b.search() {
            *b.search_mut() = typed;
        }

        let dir_y = search_y + ROW_H + GAP;
        let up_w = ROW_H;
        let reload_w = ROW_H;
        let reload = Rect::new(x0, dir_y, reload_w, ROW_H);
        if k.button_at(reload, "", false) {
            b.reload(&mut src);
        }
        k.draw_reload_icon(reload, false);

        let dir_x = x0 + reload_w + GAP;
        let dir_w = left_w - reload_w - up_w - GAP * 2.0;
        k.locked_box_at(
            Rect::new(dir_x, dir_y, dir_w, ROW_H),
            &b.directory_text(&src.rel_path()),
            c"map_browser_path",
        );

        let up = Rect::new(x0 + left_w - up_w, dir_y, up_w, ROW_H);
        let up_greyed = b.searching() || b.at_root(&src.rel_path());
        if k.button_at(up, "", up_greyed) {
            b.go_up(&mut src);
        }
        k.draw_up_arrow(up, up_greyed);

        let list_y = dir_y + ROW_H + GAP;
        if b.searching() {
            b.build_index(&mut src);
        }
        unsafe { self.draw_list(k, b, Rect::new(x0, list_y, left_w, body_bottom - list_y)) };

        let buttons_h = ROW_H;
        let buttons_y = body_bottom - buttons_h;
        k.panel_frame(
            Rect::new(right_x, body_y, right_w, buttons_y - body_y),
            c"select_map_details",
            Some(theme.left_frame),
        );
        let dx = right_x + FRAME_PAD;
        let dw = right_w - FRAME_PAD * 2.0;
        let choice = b.choice().cloned();
        k.heading_at(
            Rect::new(dx, body_y + FRAME_PAD, dw, NAME_H),
            choice.as_ref().map_or("", |c| c.name.as_str()),
            ALIGN_CENTRE,
            Some(theme.heading_font),
        );
        let preview_y = body_y + FRAME_PAD + NAME_H + GAP;
        let preview_side = (dw.min(buttons_y - preview_y - FRAME_PAD - CAPTION_H * 5.0)).max(0.0);
        if let Some(c) = &choice {
            let mut y = preview_y + preview_side + GAP;
            k.label_at(
                Rect::new(dx, y, dw, CAPTION_H),
                &format!("{} x {}", c.dim, c.dim),
                ALIGN_LEFT,
            );
            y += CAPTION_H;
            k.label_at(
                Rect::new(dx, y, dw, CAPTION_H),
                &k.format_localized(c"customscenario_maxplayers", &c.players.to_string()),
                ALIGN_LEFT,
            );
            y += CAPTION_H;
            if !c.desc.is_empty() && c.desc != c.name && y + CAPTION_H < buttons_y {
                k.wrapped_label_at(Rect::new(dx, y, dw, buttons_y - y - FRAME_PAD), &c.desc);
            }
        }

        let cancel_w = fl(right_w * 0.5);
        if k.button_at(
            Rect::new(right_x, buttons_y, cancel_w, buttons_h),
            "Cancel",
            false,
        ) {
            b.close();
        }
        if k.button_at(
            Rect::new(right_x + cancel_w, buttons_y, right_w - cancel_w, buttons_h),
            "Confirm",
            choice.is_none(),
        ) {
            if let Some(i) = b.resolve_choice(&mut src) {
                self.publish(i);
            }
            b.close();
        }
    }
}
