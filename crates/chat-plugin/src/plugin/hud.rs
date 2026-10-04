//! The in-match HUD builder and the chat edit control, ported from wc2r-plus
//! `features/ui/in_match_hud.cpp`. The game's own HUD builder draws no chat block while the game
//! is paused, so wc2r-plus replaces `BuildInMatchHud` outright (the same pieces in the same
//! order, plus the chat block always) and lets the chat edit control accept typing while paused.
//! This is what makes the history visible, and typing possible, in a paused game.
//!
//! The replacement draws nothing the stock builder does not; if any symbol is missing the plugin
//! leaves the HUD alone (the history still shows while typing).

use core::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{lookup, rd};

const CHAT_EDIT_CONTROL: i32 = 6;
const EVENT_NUMBER: usize = 0x00;
const EVENT_SCAN: usize = 0x0c;
const EVENT_MODIFIERS: usize = 0x0d;
const EVENT_CHAR: usize = 0x0e;
const DLG_EDIT_TEXT: usize = 0x14;
const DLG_EDIT_CHANGED: usize = 0x36;
const DLG_EDIT_MAX_LEN: usize = 0xfe;
const LOBBY_SLOT_STRIDE: usize = 0x26;
const LOBBY_SLOT_NAME: usize = 0x0d;
const UNIT_TYPE_BYTE: usize = 0x27;
const SCAN_BACKSPACE: u8 = 0x0e;
const MOD_CTRL_ALT: u8 = 0x0c;

#[repr(C)]
#[derive(Clone, Copy)]
struct NkImage {
    handle: *mut c_void,
    w: u16,
    h: u16,
    region: [u16; 4],
}

impl NkImage {
    const fn empty() -> NkImage {
        NkImage {
            handle: core::ptr::null_mut(),
            w: 0,
            h: 0,
            region: [0; 4],
        }
    }
}

type OverrideNextRectFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32);
type BeginNamedWindowFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, f32, f32, f32, f32, u32) -> i32;
type CtxFn = unsafe extern "C" fn(*mut c_void);
type LayoutSpaceBeginFn = unsafe extern "C" fn(*mut c_void, i32, f32, i32);
type BeginNamedGroupFn = unsafe extern "C" fn(*mut c_void, *const c_char, u32) -> i32;
type BuildMapMessagesPanelFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32, i32);
type WriteChatPromptLineFn = unsafe extern "C" fn(*const c_char, *const c_char, *const c_char);
type DlgGetControlFn = unsafe extern "C" fn(*mut c_void, i32) -> *mut u8;
type LookupLocalizedFn = unsafe extern "C" fn(*const c_char) -> *const c_char;
type SetSkinFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> u32;
type SetFontFn = unsafe extern "thiscall" fn(*mut c_void, u32) -> u32;
type HudBeginFrameFn = unsafe extern "C" fn();
type UiExtentFn = unsafe extern "C" fn() -> f32;
type AtlasLookupFn = unsafe extern "thiscall" fn(*mut c_void, *mut NkImage, u32) -> u8;
type FramePieceSizeFn = unsafe extern "C" fn(*mut i32, *const NkImage, i32) -> *mut i32;
type DrawImageFn = unsafe extern "C" fn(*mut c_void, NkImage);
type DrawTiledFn = unsafe extern "C" fn(NkImage, i32, i32, i32, i32, i32);
type ResourceCellFn = unsafe extern "C" fn(f32, f32, f32, f32, i32, i32, i32, i32, i32);
type MenuButtonsFn = unsafe extern "C" fn(i32);
type MinimapTextureFn = unsafe extern "C" fn() -> *mut c_void;
type UnitHasCargoFn = unsafe extern "C" fn(*mut c_void) -> i32;
type PortraitFn = unsafe extern "C" fn(f32, f32, f32, f32, i32);
type UnitInfoFn = unsafe extern "C" fn(i32, i32, i32, i32, i32, i32);
type GroupInfoFn = unsafe extern "C" fn(i32, i32, i32, i32, i32);
type CommandButtonsFn = unsafe extern "C" fn(i32) -> *mut c_void;
type CommandTooltipFn = unsafe extern "C" fn(*mut c_void, i32, i32);
type ObjectivesLinesFn = unsafe extern "C" fn(*mut c_void, i32) -> i32;
type ObjectivesDrawFn = unsafe extern "C" fn(*mut c_void, i32, f32);
type DlgEditRefreshFn = unsafe extern "C" fn(*mut u8);
type EditControlEventProcFn = unsafe extern "C" fn(*mut u8, *mut u8) -> u32;

struct Hud {
    override_rect: OverrideNextRectFn,
    begin_window: BeginNamedWindowFn,
    end_window: CtxFn,
    space_begin: LayoutSpaceBeginFn,
    space_end: CtxFn,
    begin_group: BeginNamedGroupFn,
    end_group: CtxFn,
    build_panel: BuildMapMessagesPanelFn,
    write_prompt: WriteChatPromptLineFn,
    dlg_get_control: DlgGetControlFn,
    lookup_localized: LookupLocalizedFn,
    set_skin: SetSkinFn,
    set_font: SetFontFn,
    begin_frame: HudBeginFrameFn,
    ui_width: UiExtentFn,
    ui_height: UiExtentFn,
    atlas_lookup: AtlasLookupFn,
    frame_piece_size: FramePieceSizeFn,
    draw_image: DrawImageFn,
    draw_tiled: DrawTiledFn,
    resource_cell: ResourceCellFn,
    food_cell: ResourceCellFn,
    menu_buttons: MenuButtonsFn,
    minimap_texture1: MinimapTextureFn,
    unit_has_cargo: UnitHasCargoFn,
    unit_info: UnitInfoFn,
    group_info: GroupInfoFn,
    command_buttons: CommandButtonsFn,
    command_tooltip: CommandTooltipFn,
    objectives_lines: ObjectivesLinesFn,
    objectives_draw: ObjectivesDrawFn,
    dlg_edit_refresh: DlgEditRefreshFn,
    // data
    ctx_slot: *mut *mut c_void,
    skin_manager_slot: *mut *mut c_void,
    ctx_view0: *const *mut c_void,
    ctx_view1: *const *mut c_void,
    skin_manager_view0: *mut c_void,
    skin_manager_view1: *mut c_void,
    skip_frame: *const u8,
    font: *const u32,
    skin_view0: *const u32,
    skin_view0_alt: *const u32,
    skin_view1: *const u32,
    skin_view1_alt: *const u32,
    skin_alt_flag: *const u8,
    frame_atlas_view0: *const *mut c_void,
    frame_atlas_view1: *const *mut c_void,
    selection_count: *const u8,
    selected_unit: *const *const u8,
    selection_other: *const *mut c_void,
    unit_type_flags: *const u32,
    gold: *const i32,
    lumber: *const i32,
    oil: *const i32,
    food_a: *const u16,
    food_b: *const u16,
    food_cap: *const u16,
    settings_flags: *const u32,
    pause_flags: *const u8,
    networked_match: *const u8,
    no_objectives: *const u8,
    chat_dialog: *const *mut c_void,
    chat_target_mode: *const u8,
    chat_target_slot: *const u8,
    local_player: *const u8,
    lobby_slots: *const u8,
    minimap_texture0: *const *mut c_void,
    portrait_by_type: *const Option<PortraitFn>,
}

// SAFETY: addresses of the game's functions and globals, fixed for the session.
unsafe impl Send for Hud {}
unsafe impl Sync for Hud {}

static HUD: OnceLock<Hud> = OnceLock::new();
pub static ORIG_EDIT_PROC: AtomicUsize = AtomicUsize::new(0);

macro_rules! f {
    ($name:literal, $t:ty) => {{
        let addr = lookup($name).ok_or($name)?;
        unsafe { core::mem::transmute::<usize, $t>(addr) }
    }};
}

macro_rules! p {
    ($name:literal, $t:ty) => {
        lookup($name).ok_or($name)? as $t
    };
}

/// Resolves everything the HUD builder needs; the error names the first missing symbol.
pub fn resolve() -> Result<(), &'static str> {
    let hud = Hud {
        override_rect: f!("OverrideNextRect", OverrideNextRectFn),
        begin_window: f!("BeginNamedWindow", BeginNamedWindowFn),
        end_window: f!("EndCurrentWindow", CtxFn),
        space_begin: f!("LayoutSpaceBegin", LayoutSpaceBeginFn),
        space_end: f!("LayoutSpaceEnd", CtxFn),
        begin_group: f!("BeginNamedGroup", BeginNamedGroupFn),
        end_group: f!("EndGroup", CtxFn),
        build_panel: f!("BuildMapMessagesPanel", BuildMapMessagesPanelFn),
        write_prompt: f!("WriteChatPromptLine", WriteChatPromptLineFn),
        dlg_get_control: f!("DlgGetControl", DlgGetControlFn),
        lookup_localized: f!("LookupLocalized", LookupLocalizedFn),
        set_skin: f!("SetSkin", SetSkinFn),
        set_font: f!("SetFont", SetFontFn),
        begin_frame: f!("HudBeginFrame", HudBeginFrameFn),
        ui_width: f!("HudUiWidth", UiExtentFn),
        ui_height: f!("HudUiHeight", UiExtentFn),
        atlas_lookup: f!("HudAtlasLookup", AtlasLookupFn),
        frame_piece_size: f!("HudFramePieceSize", FramePieceSizeFn),
        draw_image: f!("HudDrawImage", DrawImageFn),
        draw_tiled: f!("HudDrawTiled", DrawTiledFn),
        resource_cell: f!("HudResourceCell", ResourceCellFn),
        food_cell: f!("HudFoodCell", ResourceCellFn),
        menu_buttons: f!("HudMenuButtons", MenuButtonsFn),
        minimap_texture1: f!("HudMinimapTexture1", MinimapTextureFn),
        unit_has_cargo: f!("HudUnitHasCargo", UnitHasCargoFn),
        unit_info: f!("HudUnitInfo", UnitInfoFn),
        group_info: f!("HudGroupInfo", GroupInfoFn),
        command_buttons: f!("HudCommandButtons", CommandButtonsFn),
        command_tooltip: f!("HudCommandTooltip", CommandTooltipFn),
        objectives_lines: f!("HudObjectivesLines", ObjectivesLinesFn),
        objectives_draw: f!("HudObjectivesDraw", ObjectivesDrawFn),
        dlg_edit_refresh: f!("DlgEditRefresh", DlgEditRefreshFn),
        ctx_slot: p!("HudCtx", *mut *mut c_void),
        skin_manager_slot: p!("HudSkinManager", *mut *mut c_void),
        ctx_view0: p!("HudCtxView0", *const *mut c_void),
        ctx_view1: p!("HudCtxView1", *const *mut c_void),
        skin_manager_view0: p!("HudSkinManagerView0", *mut c_void),
        skin_manager_view1: p!("HudSkinManagerView1", *mut c_void),
        skip_frame: p!("HudSkipFrame", *const u8),
        font: p!("HudFont", *const u32),
        skin_view0: p!("HudSkinView0", *const u32),
        skin_view0_alt: p!("HudSkinView0Alt", *const u32),
        skin_view1: p!("HudSkinView1", *const u32),
        skin_view1_alt: p!("HudSkinView1Alt", *const u32),
        skin_alt_flag: p!("SkinAltFlag", *const u8),
        frame_atlas_view0: p!("HudFrameAtlasView0", *const *mut c_void),
        frame_atlas_view1: p!("HudFrameAtlasView1", *const *mut c_void),
        selection_count: p!("HudSelectionCount", *const u8),
        selected_unit: p!("HudSelectedUnit", *const *const u8),
        selection_other: p!("HudSelectionOther", *const *mut c_void),
        unit_type_flags: p!("UnitTypeFlags", *const u32),
        gold: p!("HudGold", *const i32),
        lumber: p!("HudLumber", *const i32),
        oil: p!("HudOil", *const i32),
        food_a: p!("FoodA", *const u16),
        food_b: p!("FoodB", *const u16),
        food_cap: p!("FoodCap", *const u16),
        settings_flags: p!("GameSettingsFlags", *const u32),
        pause_flags: p!("MatchPauseFlags", *const u8),
        networked_match: p!("NetworkedMatch", *const u8),
        no_objectives: p!("HudNoObjectives", *const u8),
        chat_dialog: p!("ChatDialog", *const *mut c_void),
        chat_target_mode: p!("ChatTargetMode", *const u8),
        chat_target_slot: p!("ChatTargetSlot", *const u8),
        local_player: p!("LocalPlayer", *const u8),
        lobby_slots: p!("MpLobbySlots", *const u8),
        minimap_texture0: p!("HudMinimapTexture0", *const *mut c_void),
        portrait_by_type: p!("HudPortraitByType", *const Option<PortraitFn>),
    };
    let _ = HUD.set(hud);
    Ok(())
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

unsafe fn place(h: &Hud, ctx: *mut c_void, x: i32, y: i32, w: i32, hgt: i32) {
    unsafe { (h.override_rect)(ctx, x as f32, y as f32, w as f32, hgt as f32) };
}

unsafe fn piece(h: &Hud, atlas: *mut c_void, name: &str) -> Option<NkImage> {
    let mut img = NkImage::empty();
    (unsafe { (h.atlas_lookup)(atlas, &mut img, fnv1a(name)) } != 0).then_some(img)
}

unsafe fn piece_size(h: &Hud, img: &NkImage, view: i32) -> [i32; 2] {
    let mut wh = [0i32; 2];
    unsafe { (h.frame_piece_size)(wh.as_mut_ptr(), img, view) };
    wh
}

unsafe fn draw_frame_art(
    h: &Hud,
    ctx: *mut c_void,
    atlas: *mut c_void,
    view: i32,
    w: i32,
    hgt: i32,
) {
    unsafe {
        let (mut tl_w, mut tl_h, mut bl_w, mut tr_w, mut tr_h, mut br_w, mut br_h) =
            (0, 0, 0, 0, 0, 0, 0);
        let top_left = if *h.selection_count != 0 {
            "TopLeftAlt"
        } else {
            "TopLeft"
        };
        if let Some(img) = piece(h, atlas, top_left) {
            let wh = piece_size(h, &img, view);
            tl_w = wh[0];
            tl_h = wh[1];
            place(h, ctx, 0, 0, tl_w, tl_h);
            (h.draw_image)(ctx, img);
        }
        if let Some(img) = piece(h, atlas, "TileableLeft") {
            (h.draw_tiled)(img, 0, tl_h, hgt - tl_h, 0, view);
        }
        if let Some(img) = piece(h, atlas, "BottomLeft") {
            let wh = piece_size(h, &img, view);
            bl_w = wh[0];
            place(h, ctx, 0, hgt - wh[1], bl_w, wh[1]);
            (h.draw_image)(ctx, img);
        }
        if let Some(img) = piece(h, atlas, "TopRight") {
            let wh = piece_size(h, &img, view);
            tr_w = wh[0];
            tr_h = wh[1];
            place(h, ctx, w - tr_w, 0, tr_w, tr_h);
            (h.draw_image)(ctx, img);
        }
        if let Some(img) = piece(h, atlas, "BottomRight") {
            let wh = piece_size(h, &img, view);
            br_w = wh[0];
            br_h = wh[1];
            place(h, ctx, w - br_w, hgt - br_h, br_w, br_h);
            (h.draw_image)(ctx, img);
        }
        if let Some(img) = piece(h, atlas, "Right") {
            let wh = piece_size(h, &img, view);
            (h.draw_tiled)(img, w - wh[0], tr_h, hgt - br_h - tr_h, 0, view);
        }
        if let Some(img) = piece(h, atlas, "Top") {
            (h.draw_tiled)(img, tl_w, 0, w - tr_w - tl_w, 1, view);
        }
        if let Some(img) = piece(h, atlas, "Bottom") {
            let wh = piece_size(h, &img, view);
            (h.draw_tiled)(img, tl_w, hgt - wh[1], w - br_w - bl_w, 1, view);
        }
    }
}

unsafe fn draw_resource_bar(h: &Hud, w: i32, view: i32) {
    unsafe {
        let mut x = w - 0xc0 - 0x12c;
        let flags = *h.settings_flags;
        if flags & 0x200_0000 != 0 {
            x = 0;
        } else if flags & 0x400_0000 == 0 {
            x /= 2;
        }
        let rx = x as f32;
        (h.resource_cell)(rx, 0.0, 300.0, 14.0, 0, 4, 0, *h.gold, view);
        (h.resource_cell)(rx, 0.0, 300.0, 14.0, 1, 4, 1, *h.lumber, view);
        (h.resource_cell)(rx, 0.0, 300.0, 14.0, 2, 4, 2, *h.oil, view);

        let p = *h.local_player as usize;
        let used = *h.food_a.add(p) as i32 - *h.food_b.add(p) as i32;
        let cap = (*h.food_cap.add(p)).min(200) as i32;
        (h.food_cell)(rx, 0.0, 300.0, 14.0, 3, 4, used, cap, view);
    }
}

unsafe fn draw_portbar(h: &Hud, view: i32) {
    unsafe {
        let count = *h.selection_count;
        let style = if *h.settings_flags & 0x1000_0000 != 0 || count > 9 {
            4
        } else {
            3
        };
        let other = *h.selection_other;
        let mut group = count > 1;
        if !group && !other.is_null() {
            let unit = *h.selected_unit;
            if !unit.is_null() {
                let ty = *unit.add(UNIT_TYPE_BYTE) as usize;
                group = (*h.unit_type_flags.add(ty) & 0x400) != 0 && (h.unit_has_cargo)(other) != 0;
            }
        }
        if group {
            (h.group_info)(5, 5, 3, style, view);
            return;
        }
        if count != 1 {
            return;
        }
        let unit = *h.selected_unit;
        if unit.is_null() {
            return;
        }
        let ty = *unit.add(UNIT_TYPE_BYTE) as usize;
        if let Some(portrait) = *h.portrait_by_type.add(ty) {
            portrait(5.0, 5.0, 166.0, 166.0, view);
        }
        (h.unit_info)(0, 5, 5, 3, style, view);
    }
}

unsafe fn draw_chat_block(
    h: &Hud,
    ctx: *mut c_void,
    bottom: i32,
    hovered: *mut c_void,
    view: i32,
    paused: bool,
) {
    unsafe {
        let mode = *h.chat_target_mode;
        let key: Option<&CStr> = match mode {
            1 | 6 => Some(c"message_generic"),
            2 => Some(c"message_to_all"),
            3 => Some(c"message_to_ally"),
            4 => Some(c"message_to_enemy"),
            5 => Some(c"message_to_player"),
            7 => Some(c"message_to_none"),
            _ => None,
        };
        match key {
            None => (h.write_prompt)(core::ptr::null(), core::ptr::null(), core::ptr::null()),
            Some(key) => {
                let dialog = *h.chat_dialog;
                let edit = if dialog.is_null() {
                    core::ptr::null_mut()
                } else {
                    (h.dlg_get_control)(dialog, CHAT_EDIT_CONTROL)
                };
                if edit.is_null() {
                    (h.write_prompt)(core::ptr::null(), core::ptr::null(), core::ptr::null());
                    (h.build_panel)(ctx, 182.0, 22.0, 294.0, (bottom - 0xc) as f32, 0x10);
                    return;
                }
                let text = rd::<*const c_char>(edit, DLG_EDIT_TEXT);
                let banner = (h.lookup_localized)(key.as_ptr());
                if mode == 5 {
                    let name = h
                        .lobby_slots
                        .add(*h.chat_target_slot as usize * LOBBY_SLOT_STRIDE + LOBBY_SLOT_NAME);
                    (h.write_prompt)(banner, name.cast(), text);
                } else {
                    (h.write_prompt)(banner, text, core::ptr::null());
                }
            }
        }
        (h.build_panel)(ctx, 182.0, 22.0, 294.0, (bottom - 0xc) as f32, 0x10);
        if !hovered.is_null() && !paused {
            (h.command_tooltip)(hovered, bottom, view);
        }
    }
}

/// Replaces `BuildInMatchHud`: the stock HUD, always with the chat block (wc2r-plus
/// `OnBuildInMatchHud`). Nothing here calls the original.
unsafe fn build_hud(h: &Hud, which_view: i8) {
    unsafe {
        let view = if which_view != 0 { 1 } else { 0 };
        let ctx = if view == 1 {
            *h.ctx_view1
        } else {
            *h.ctx_view0
        };
        let manager = if view == 1 {
            h.skin_manager_view1
        } else {
            h.skin_manager_view0
        };
        *h.ctx_slot = ctx;
        *h.skin_manager_slot = manager;
        (h.begin_frame)();
        if *h.skip_frame != 0 {
            return;
        }

        let alt = *h.skin_alt_flag != 0;
        let skin = match (view, alt) {
            (1, true) => *h.skin_view1_alt,
            (1, false) => *h.skin_view1,
            (_, true) => *h.skin_view0_alt,
            (_, false) => *h.skin_view0,
        };
        let prev_font = (h.set_font)(manager, *h.font);
        let prev_skin = (h.set_skin)(manager, skin);

        let w = (h.ui_width)() as i32;
        let hgt = (h.ui_height)() as i32;
        if (h.begin_window)(
            ctx,
            c"game_hud".as_ptr(),
            0.0,
            0.0,
            w as f32,
            hgt as f32,
            0x120,
        ) != 0
        {
            (h.space_begin)(ctx, 1, 0.0, 1000);

            let atlas = if view == 1 {
                *h.frame_atlas_view1
            } else {
                *h.frame_atlas_view0
            };
            if !atlas.is_null() {
                draw_frame_art(h, ctx, atlas, view, w, hgt);
            }

            let w2 = (h.ui_width)() as i32;
            place(h, ctx, 0xb0, 0, w2 - 0xc0, 0x10);
            if (h.begin_group)(ctx, c"resourcebar".as_ptr(), 0x20) != 0 {
                (h.space_begin)(ctx, 1, 0.0, 1000);
                draw_resource_bar(h, w2, view);
                (h.space_end)(ctx);
                (h.end_group)(ctx);
            }
            place(h, ctx, 0xb0, hgt - 0x10, w2 - 0xc0, 0x10);
            if (h.begin_group)(ctx, c"bottom".as_ptr(), 0x20) != 0 {
                (h.end_group)(ctx);
            }
            place(h, ctx, w2 - 0x10, 0, 0x10, hgt);
            if (h.begin_group)(ctx, c"right".as_ptr(), 0x20) != 0 {
                (h.end_group)(ctx);
            }

            place(h, ctx, 0, 0, 0xbc, 0xa0);
            if (h.begin_group)(ctx, c"minimap".as_ptr(), 0x20) != 0 {
                (h.space_begin)(ctx, 1, 0.0, 1000);
                (h.menu_buttons)(view);
                (h.override_rect)(ctx, 24.0, 26.0, 128.0, 128.0);
                let mut minimap = NkImage::empty();
                minimap.handle = if view == 1 {
                    (h.minimap_texture1)()
                } else {
                    *h.minimap_texture0
                };
                (h.draw_image)(ctx, minimap);
                (h.space_end)(ctx);
                (h.end_group)(ctx);
            }

            place(h, ctx, 0, 0xa0, 0xbc, 0xb0);
            if (h.begin_group)(ctx, c"portbar".as_ptr(), 0x20) != 0 {
                (h.space_begin)(ctx, 1, 0.0, 1000);
                draw_portbar(h, view);
                (h.space_end)(ctx);
                (h.end_group)(ctx);
            }

            let mut hovered: *mut c_void = core::ptr::null_mut();
            place(h, ctx, 0, 0x150, 0xbc, 0x90);
            if (h.begin_group)(ctx, c"statbar".as_ptr(), 0x20) != 0 {
                (h.space_begin)(ctx, 1, 0.0, 1000);
                hovered = (h.command_buttons)(view);
                (h.space_end)(ctx);
                (h.end_group)(ctx);
            }

            let paused = *h.pause_flags != 0;
            draw_chat_block(h, ctx, hgt - 0x20, hovered, view, paused);

            if *h.networked_match == 0 && *h.no_objectives == 0 && *h.settings_flags & 0x20 == 0 {
                let lines = (h.objectives_lines)(ctx, 0xe6);
                place(h, ctx, 0xb8, 0x18, 0xe6, lines << 4);
                if (h.begin_group)(ctx, c"objectives".as_ptr(), 0x20) != 0 {
                    (h.objectives_draw)(ctx, 0xe6, 16.0);
                    (h.end_group)(ctx);
                }
            }

            (h.space_end)(ctx);
            (h.end_window)(ctx);
        }

        (h.set_skin)(manager, prev_skin);
        (h.set_font)(manager, prev_font);
    }
}

/// The detour for `BuildInMatchHud`. There is no original to fall back on once drawing started,
/// so a panic here ends the frame's HUD (the next frame draws it again).
pub unsafe extern "C" fn build_in_match_hud(which_view: i8) {
    let Some(h) = HUD.get() else { return };
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe { build_hud(h, which_view) }));
}

unsafe fn is_typed_character(evt: *const u8) -> bool {
    unsafe {
        let number = rd::<u16>(evt, EVENT_NUMBER);
        (number == 0 || number == 1)
            && rd::<u8>(evt, EVENT_MODIFIERS) & MOD_CTRL_ALT == 0
            && rd::<u8>(evt, EVENT_SCAN) != SCAN_BACKSPACE
            && rd::<u8>(evt, EVENT_CHAR) != 0
    }
}

unsafe fn is_chat_edit(h: &Hud, control: *const u8) -> bool {
    unsafe {
        let dialog = *h.chat_dialog;
        !dialog.is_null() && control == (h.dlg_get_control)(dialog, CHAT_EDIT_CONTROL).cast_const()
    }
}

/// While the game is paused the stock edit control refuses typing; the character is appended by
/// hand, exactly as wc2r-plus does (`OnEditControlEvent`).
pub unsafe extern "C" fn edit_control_event(control: *mut u8, evt: *mut u8) -> u32 {
    let original: EditControlEventProcFn =
        unsafe { core::mem::transmute(ORIG_EDIT_PROC.load(Ordering::Acquire)) };
    let accepted = unsafe { original(control, evt) };
    let Some(h) = HUD.get() else { return accepted };
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        if accepted != 0
            && !control.is_null()
            && !evt.is_null()
            && *h.pause_flags == 1
            && is_typed_character(evt)
            && is_chat_edit(h, control)
        {
            let text = rd::<*mut u8>(control, DLG_EDIT_TEXT);
            if !text.is_null() {
                let len = CStr::from_ptr(text.cast()).to_bytes().len();
                if len < DLG_EDIT_MAX_LEN {
                    *text.add(len) = rd::<u8>(evt, EVENT_CHAR);
                    *text.add(len + 1) = 0;
                    *control.add(DLG_EDIT_CHANGED) = 1;
                    (h.dlg_edit_refresh)(control);
                }
            }
        }
    }));
    accepted
}
