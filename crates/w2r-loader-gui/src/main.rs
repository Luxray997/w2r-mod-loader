#![windows_subsystem = "windows"]

fn main() -> eframe::Result {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([760.0, 520.0])
        .with_min_inner_size([520.0, 360.0])
        .with_icon(std::sync::Arc::new(w2r_loader_gui::theme::icon()));
    // Restore the last size and position; ignore a position that would be off every screen.
    if let Some([x, y, w, h]) = w2r_loader_gui::settings::Settings::load().window {
        viewport = viewport.with_inner_size([w.clamp(520, 4000) as f32, h.clamp(360, 3000) as f32]);
        if (-4000..8000).contains(&x) && (-100..4000).contains(&y) {
            viewport = viewport.with_position([x as f32, y as f32]);
        }
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "W2R Mod Loader",
        options,
        Box::new(|cc| {
            w2r_loader_gui::theme::apply(&cc.egui_ctx);
            Ok(Box::new(w2r_loader_gui::app::App::new()))
        }),
    )
}
