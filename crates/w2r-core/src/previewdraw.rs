//! Putting a map preview on screen (docs/create-screen.md, slice 3), the game half of
//! `map_preview.cpp`: reads the map file and the era's tileset art, hands them to
//! [`crate::mappreview`], uploads the pixels into one game texture and draws it with the game's
//! own image function. One preview is kept; it is rebuilt when the map or the tileset choice
//! changes. If the texture cannot be made, nothing is drawn (wc2r-plus falls back to coloured
//! rectangles; that path is not ported). x86 only.

use core::ffi::c_void;
use std::io::Read;
use std::sync::Mutex;

use crate::mappreview::{self, Preview, TILESET_FILES, Tileset};
use crate::pud;
use crate::screenkit::Rect;
use crate::screenkit::game::Kit;
use crate::symbols::Symbols;

/// The texture the game keeps for us is this big on both sides; the picture uses a corner of it.
const TEXTURE_DIM: i32 = 512;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const FORMAT_RGBA8: i32 = 0;
const FILTER_LINEAR: i32 = 1;
const BORDER: f32 = 4.0;
const BORDER_COLOUR: u32 = mappreview::rgba(0x18, 0x14, 0x0c);
const WHITE: u32 = 0xffff_ffff;
/// Largest map file and tileset art file read for a preview.
const MAX_MAP_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ART_BYTES: u64 = 16 * 1024 * 1024;

#[repr(C)]
struct GameTexture {
    w: i32,
    h: i32,
    gl_name: u32,
    format: i32,
    filter: i32,
}

#[repr(C)]
struct NkImage {
    handle: *mut c_void,
    w: u16,
    h: u16,
    region: [u16; 4],
}

type CreateTextureFn =
    unsafe extern "C" fn(*mut *mut GameTexture, i32, i32, *const c_void, i32, i32) -> *mut c_void;
type DrawImageFn = unsafe extern "C" fn(*mut c_void, f32, f32, f32, f32, *const NkImage, u32);
type SubImageFn = unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);

#[derive(Clone, Copy)]
pub struct PreviewGame {
    create_texture: CreateTextureFn,
    draw_image: DrawImageFn,
    /// The game's GL entry point slots: each holds a function pointer, null until the renderer
    /// is up.
    sub_image_slot: *const usize,
}

// SAFETY: addresses of the game's functions and globals, fixed for the session.
unsafe impl Send for PreviewGame {}
unsafe impl Sync for PreviewGame {}

impl PreviewGame {
    pub fn new(symbols: &Symbols) -> Result<PreviewGame, &'static str> {
        let at = |n: &'static str| symbols.address(n).ok_or(n);
        Ok(PreviewGame {
            create_texture: unsafe {
                core::mem::transmute::<usize, CreateTextureFn>(at("CreateGameTexture")?)
            },
            draw_image: unsafe { core::mem::transmute::<usize, DrawImageFn>(at("NkDrawImage")?) },
            sub_image_slot: at("GlTextureSubImage2DSlot")? as *const usize,
        })
    }
}

/// What to draw: a square box and the map file to show, with the tileset the user picked
/// (`None` for the map's own).
pub struct Request {
    pub area: Rect,
    pub path: String,
    pub tileset: Option<u16>,
    /// A heat image to lay over the picture: `heat::SIDE` x `SIDE` bytes indexed by map tile
    /// (`w2r_replay::heat::image`), drawn only where it is above zero.
    pub heat: Option<std::sync::Arc<Vec<u8>>>,
}

#[derive(Default)]
struct State {
    texture: Option<usize>,
    texture_failed: bool,
    key: Option<(String, Option<u16>)>,
    preview: Option<Preview>,
    uploaded: bool,
    tilesets: [Option<Tileset>; 4],
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn read_limited(path: &std::path::Path, limit: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut out)
        .ok()?;
    (!out.is_empty() && out.len() as u64 <= limit).then_some(out)
}

fn game_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default()
}

/// The era's art, read once (a failure is remembered as an empty tileset).
fn tileset(state: &mut State, era: u16) -> &Tileset {
    let slot = &mut state.tilesets[(era & 3) as usize];
    slot.get_or_insert_with(|| {
        let (folder, stem) = TILESET_FILES[(era & 3) as usize];
        let base = game_dir()
            .join("Data")
            .join("Art")
            .join("bgs")
            .join(folder)
            .join(stem);
        let read = |ext: &str| read_limited(&base.with_extension(ext), MAX_ART_BYTES);
        match (read("cv4"), read("vx4"), read("vr4"), read("ppl")) {
            (Some(cv4), Some(vx4), Some(vr4), Some(ppl)) => {
                let ts = Tileset::build(&cv4, &vx4, &vr4, &ppl);
                crate::session::log_info("preview", &format!("tileset {stem} loaded"));
                ts
            }
            _ => {
                crate::session::log_error(
                    "preview",
                    &format!("tileset {stem}: art missing under {}", base.display()),
                );
                Tileset::default()
            }
        }
    })
}

/// Reads and renders one map; `None` (and a log line) when anything about it is wrong.
fn build(state: &mut State, path: &str, tileset_override: Option<u16>) -> Option<Preview> {
    let warn = |what: &str| {
        crate::session::log_info("preview", &format!("no preview for '{path}': {what}"));
    };
    if !mappreview::safe_map_path(path) {
        warn("the path is not allowed");
        return None;
    }
    let Some(bytes) = read_limited(std::path::Path::new(path), MAX_MAP_BYTES) else {
        warn("cannot read it (or it is over 4 MiB)");
        return None;
    };
    let pud = match pud::parse(&bytes) {
        Ok(p) => p,
        Err(fault) => {
            warn(&format!("not a usable PUD ({fault:?})"));
            return None;
        }
    };
    let era = tileset_override.unwrap_or(pud.era);
    let ts = tileset(state, era);
    if ts.is_empty() {
        return None;
    }
    Some(mappreview::render(&pud, ts, era, mappreview::TEX_SIZE))
}

fn ensure_texture(game: &PreviewGame, state: &mut State) -> Option<*mut GameTexture> {
    if let Some(t) = state.texture {
        return Some(t as *mut GameTexture);
    }
    if state.texture_failed {
        return None;
    }
    state.texture_failed = true;
    if unsafe { *game.sub_image_slot } == 0 {
        crate::session::log_error("preview", "GL entry points are not ready; no previews");
        return None;
    }
    let blank = vec![0u8; (TEXTURE_DIM * TEXTURE_DIM * 4) as usize];
    let mut tex: *mut GameTexture = core::ptr::null_mut();
    unsafe {
        (game.create_texture)(
            &mut tex,
            TEXTURE_DIM,
            TEXTURE_DIM,
            blank.as_ptr().cast(),
            FORMAT_RGBA8,
            FILTER_LINEAR,
        )
    };
    let ok = !tex.is_null()
        && unsafe { (*tex).gl_name != 0 && (*tex).w == TEXTURE_DIM && (*tex).h == TEXTURE_DIM };
    if !ok {
        crate::session::log_error("preview", "the game would not make a preview texture");
        return None;
    }
    state.texture_failed = false;
    state.texture = Some(tex as usize);
    Some(tex)
}

/// Draws the request's preview on the window being built. Call after the screen's layout space
/// has ended: the picture goes straight onto the window's canvas, over the widgets. False when
/// nothing was drawn (no map, no art, no texture).
pub fn draw(game: &PreviewGame, kit: &Kit, req: &Request) -> bool {
    if req.area.w <= 0.0 || req.path.is_empty() {
        return false;
    }
    let canvas = kit.canvas();
    if canvas.is_null() {
        return false;
    }
    with_state(|st| {
        let key = (req.path.clone(), req.tileset);
        if st.key.as_ref() != Some(&key) {
            st.preview = build(st, &req.path, req.tileset);
            st.key = Some(key);
            st.uploaded = false;
        }
        if st.preview.is_none() {
            return false;
        }
        let Some(tex) = ensure_texture(game, st) else {
            return false;
        };
        let p = st.preview.as_ref().expect("checked");
        if !st.uploaded {
            let upload = unsafe { *game.sub_image_slot };
            if upload == 0 {
                return false;
            }
            let upload: SubImageFn = unsafe { core::mem::transmute(upload) };
            unsafe {
                upload(
                    (*tex).gl_name,
                    0,
                    0,
                    0,
                    p.width as i32,
                    p.height as i32,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    p.texels.as_ptr().cast(),
                );
            }
            st.uploaded = true;
        }
        // Fit the picture inside the square, keeping its shape, centred.
        let size = req.area.w;
        let (mut w, mut h) = (size, size);
        if p.width >= p.height {
            h = size * p.height as f32 / p.width as f32;
        } else {
            w = size * p.width as f32 / p.height as f32;
        }
        let x = req.area.x + (size - w) * 0.5;
        let y = req.area.y + (size - h) * 0.5;
        let b = BORDER;
        kit.fill_rect(Rect::new(x - b, y - b, w + 2.0 * b, b), BORDER_COLOUR);
        kit.fill_rect(Rect::new(x - b, y + h, w + 2.0 * b, b), BORDER_COLOUR);
        kit.fill_rect(Rect::new(x - b, y, b, h), BORDER_COLOUR);
        kit.fill_rect(Rect::new(x + w, y, b, h), BORDER_COLOUR);
        let image = NkImage {
            handle: tex.cast(),
            w: TEXTURE_DIM as u16,
            h: TEXTURE_DIM as u16,
            region: [0, 0, p.width as u16, p.height as u16],
        };
        unsafe { (game.draw_image)(canvas, x, y, w, h, &image, WHITE) };
        if let Some(heat) = &req.heat {
            overlay_heat(kit, heat, (p.map_w as usize, p.map_h as usize), Rect::new(x, y, w, h));
        }
        true
    })
}

/// Lays a heat image over the picture, one square per map tile that has any.
fn overlay_heat(kit: &Kit, heat: &[u8], map: (usize, usize), area: Rect) {
    use w2r_replay::heat::{SIDE, ramp};
    let (mw, mh) = (map.0.min(SIDE), map.1.min(SIDE));
    if mw == 0 || mh == 0 || heat.len() < SIDE * SIDE {
        return;
    }
    let (cw, ch) = (area.w / mw as f32, area.h / mh as f32);
    for ty in 0..mh {
        for tx in 0..mw {
            let v = heat[ty * SIDE + tx];
            if v == 0 {
                continue;
            }
            let [r, g, b, a] = ramp(v);
            kit.fill_rect(
                Rect::new(area.x + tx as f32 * cw, area.y + ty as f32 * ch, cw, ch),
                crate::screenkit::game::rgba(r, g, b, a),
            );
        }
    }
}

#[derive(Default)]
struct IconState {
    texture: Option<usize>,
    failed: bool,
}

static ICON: Mutex<IconState> = Mutex::new(IconState {
    texture: None,
    failed: false,
});

/// Draws a small square RGBA picture (`size` by `size`) into `area`, from a texture of its own
/// made on first use. One picture is kept (the lobby's host crown). False when the texture
/// cannot be made; the caller then draws something else.
pub fn draw_icon(game: &PreviewGame, kit: &Kit, area: Rect, rgba: &[u8], size: i32) -> bool {
    if rgba.len() != (size * size * 4) as usize {
        return false;
    }
    let canvas = kit.canvas();
    if canvas.is_null() {
        return false;
    }
    let mut st = ICON.lock().unwrap_or_else(|e| e.into_inner());
    if st.texture.is_none() && !st.failed {
        st.failed = true;
        let mut tex: *mut GameTexture = core::ptr::null_mut();
        unsafe {
            (game.create_texture)(
                &mut tex,
                size,
                size,
                rgba.as_ptr().cast(),
                FORMAT_RGBA8,
                FILTER_LINEAR,
            )
        };
        if !tex.is_null() && unsafe { (*tex).gl_name != 0 && (*tex).w == size && (*tex).h == size }
        {
            st.texture = Some(tex as usize);
            st.failed = false;
        } else {
            crate::session::log_error("lobbyscreen", "the game would not make the crown texture");
        }
    }
    let Some(tex) = st.texture else {
        return false;
    };
    let image = NkImage {
        handle: tex as *mut c_void,
        w: size as u16,
        h: size as u16,
        region: [0, 0, size as u16, size as u16],
    };
    unsafe { (game.draw_image)(canvas, area.x, area.y, area.w, area.h, &image, WHITE) };
    true
}
