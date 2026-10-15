//! Look of the window: one dark palette with a gold accent, plus the small widgets (cards,
//! badges, pills) the screens share.

use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke, Vec2};

pub const ACCENT: Color32 = Color32::from_rgb(214, 170, 70);
pub const GOOD: Color32 = Color32::from_rgb(96, 190, 120);
pub const WARN: Color32 = Color32::from_rgb(230, 190, 70);
pub const BAD: Color32 = Color32::from_rgb(235, 110, 100);
pub const MUTED: Color32 = Color32::from_rgb(150, 154, 164);
const BG: Color32 = Color32::from_rgb(24, 26, 31);
const PANEL: Color32 = Color32::from_rgb(32, 35, 42);
const CARD: Color32 = Color32::from_rgb(39, 43, 52);
const HOVER: Color32 = Color32::from_rgb(55, 60, 72);

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let r = CornerRadius::same(6);
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(18, 19, 23);
    v.window_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0_f32, HOVER);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = ACCENT;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, HOVER);
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.inactive.weak_bg_fill = HOVER;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(62, 67, 80));
    v.widgets.hovered.bg_fill = HOVER;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(70, 76, 90);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.6);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    for (_, font) in style.text_styles.iter_mut() {
        font.size *= 1.1;
    }
    ctx.set_style(style);
}

/// A rounded box for one plugin or one settings group.
pub fn card() -> Frame {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, HOVER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
}

/// A small coloured label such as "Update available".
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    tag(
        ui,
        RichText::new(text).color(color).small(),
        color.gamma_multiply(0.22),
        Some(Stroke::new(1.0_f32, color.gamma_multiply(0.7))),
        CornerRadius::same(10),
        Vec2::new(8.0, 2.0),
    );
}

/// A neutral pill for a capability name.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    tag(
        ui,
        RichText::new(text).color(color).small().monospace(),
        Color32::from_rgb(28, 30, 36),
        None,
        CornerRadius::same(4),
        Vec2::new(6.0, 1.0),
    );
}

/// A label on a rounded background, sized to its text exactly (a Frame stretches to the full
/// height of a centred right-to-left row).
fn tag(
    ui: &mut egui::Ui,
    text: RichText,
    fill: Color32,
    stroke: Option<Stroke>,
    radius: CornerRadius,
    pad: Vec2,
) {
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let (rect, _) = ui.allocate_exact_size(galley.size() + pad * 2.0, egui::Sense::hover());
    ui.painter().rect_filled(rect, radius, fill);
    if let Some(s) = stroke {
        ui.painter()
            .rect_stroke(rect, radius, s, egui::StrokeKind::Inside);
    }
    ui.painter()
        .galley(rect.min + pad, galley, Color32::PLACEHOLDER);
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(MUTED)
}

/// The coloured circle with a plugin's first letter.
pub fn avatar(ui: &mut egui::Ui, name: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), egui::Sense::hover());
    let hue = name.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    let color: Color32 = egui::ecolor::Hsva::new(hue as f32 / 360.0, 0.45, 0.75, 1.0).into();
    ui.painter().circle_filled(rect.center(), 18.0, color.gamma_multiply(0.35));
    ui.painter().circle_stroke(rect.center(), 18.0, Stroke::new(1.5_f32, color));
    let initial = initials(name);
    let size = if initial.len() > 1 { 15.0 } else { 18.0 };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        egui::FontId::proportional(size),
        color,
    );
}

/// A card with a heading, full width, for one group of settings.
pub fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(heading(title, 18.0).color(ACCENT));
        ui.add_space(2.0);
        body(ui);
    });
    ui.add_space(6.0);
}

/// The window icon: a dark rounded square with a gold ring and dot, drawn here so no image file
/// ships with the exe.
pub fn icon() -> egui::IconData {
    const N: usize = 64;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5 - 32.0, y as f32 + 0.5 - 32.0);
            // Rounded square mask, corner radius 12.
            let (qx, qy) = (fx.abs() - 20.0, fy.abs() - 20.0);
            let sq = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - 12.0;
            if sq > 0.0 {
                continue;
            }
            let r = (fx * fx + fy * fy).sqrt();
            let c = if (15.0..=22.0).contains(&r) || r <= 7.0 {
                [214, 170, 70, 255]
            } else {
                [24, 26, 31, 255]
            };
            rgba[(y * N + x) * 4..][..4].copy_from_slice(&c);
        }
    }
    egui::IconData {
        rgba,
        width: N as u32,
        height: N as u32,
    }
}

/// Up to two letters for the avatar: the first letters of the first two words, after dropping the
/// `w2r` prefix nearly every plugin shares ("w2r-random-map" gives "RM", "W2R Chat" gives "C").
fn initials(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let rest = match words.first() {
        Some(&"w2r") if words.len() > 1 => &words[1..],
        _ => &words[..],
    };
    let s: String = rest
        .iter()
        .take(2)
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if s.is_empty() { "?".to_owned() } else { s }
}

#[cfg(test)]
mod tests {
    use super::initials;

    #[test]
    fn avatar_letters_skip_the_w2r_prefix() {
        assert_eq!(initials("w2r-random-map"), "RM");
        assert_eq!(initials("W2R Chat"), "C");
        assert_eq!(initials("w2r"), "W");
        assert_eq!(initials("fancy"), "F");
        assert_eq!(initials(""), "?");
    }
}

static QUADRATA_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const QUADRATA: &str = "quadrata";

/// Loads Blizzard's UI font (Friz Quadrata, `Data\Font\BLQ55.ttf`) from the game folder for
/// headings. It is ParaType's licensed font, so it is read from the user's own install and never
/// shipped; without it headings keep the default font. Returns whether it was loaded; pass that to
/// `use_heading_font` on the next frame.
pub fn load_game_font(ctx: &egui::Context, game_dir: Option<&std::path::Path>) -> bool {
    let bytes = game_dir
        .map(|d| d.join("Data").join("Font").join("BLQ55.ttf"))
        .and_then(|p| std::fs::read(p).ok())
        .filter(|b| b.len() < 4 << 20);
    let mut fonts = egui::FontDefinitions::default();
    if let Some((bytes, scale)) = system_body_font(game_dir) {
        fonts.font_data.insert(
            "body".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes).tweak(egui::FontTweak {
                scale,
                ..Default::default()
            })),
        );
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "body".to_owned());
        }
    }
    let loaded = bytes.is_some();
    if let Some(b) = bytes {
        fonts
            .font_data
            .insert(QUADRATA.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(b)));
        // The game's font only has Latin letters; everything else falls back to the defaults.
        let mut chain = vec![QUADRATA.to_owned()];
        chain.extend(
            fonts
                .families
                .get(&egui::FontFamily::Proportional)
                .cloned()
                .unwrap_or_default(),
        );
        fonts
            .families
            .insert(egui::FontFamily::Name(QUADRATA.into()), chain);
    }
    ctx.set_fonts(fonts);
    loaded
}

/// Heading text: Friz Quadrata when loaded, else the strong default font.
pub fn heading(text: impl Into<String>, size: f32) -> RichText {
    let t = RichText::new(text.into())
        .size(size)
        .color(Color32::from_rgb(236, 237, 242));
    if QUADRATA_LOADED.load(std::sync::atomic::Ordering::Relaxed) {
        t.family(egui::FontFamily::Name(QUADRATA.into()))
    } else {
        t.strong()
    }
}

/// Turns the heading font on or off. Call it on the frame after `load_game_font`: egui only
/// binds new fonts when a frame starts, and using the family before that panics.
pub fn use_heading_font(on: bool) {
    QUADRATA_LOADED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The body font, read from an installed font file (nothing is bundled): Noto Sans from the
/// game's `Data\Font` when the game folder is known, else Segoe UI from the Windows fonts folder.
/// `W2RPM_GUI_FONT` forces one: a file name in the Windows fonts folder (`corbel.ttf`), or one of
/// the game's with a `game:` prefix (`game:NotoSans-Regular.ttf`). Anything missing leaves egui's
/// default font.
fn system_body_font(game_dir: Option<&std::path::Path>) -> Option<(Vec<u8>, f32)> {
    match std::env::var("W2RPM_GUI_FONT") {
        Ok(spec) => read_font(&spec, game_dir),
        Err(_) => read_font("game:NotoSans-Regular.ttf", game_dir)
            .or_else(|| read_font("segoeui.ttf", game_dir)),
    }
}

fn read_font(spec: &str, game_dir: Option<&std::path::Path>) -> Option<(Vec<u8>, f32)> {
    let (from_game, name) = match spec.strip_prefix("game:") {
        Some(n) => (true, n),
        None => (false, spec),
    };
    if !name
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
    {
        return None;
    }
    let dir = if from_game {
        game_dir?.join("Data").join("Font")
    } else {
        let windir = std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into());
        std::path::Path::new(&windir).join("Fonts")
    };
    let bytes = std::fs::read(dir.join(name)).ok()?;
    if bytes.len() > 16 << 20 {
        return None;
    }
    let scale = match name.to_ascii_lowercase().as_str() {
        "corbel.ttf" => 1.1,
        "candara.ttf" => 1.08,
        "calibri.ttf" => 1.1,
        _ => 1.0,
    };
    Some((bytes, scale))
}
