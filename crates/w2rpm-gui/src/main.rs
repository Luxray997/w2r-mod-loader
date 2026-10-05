#![windows_subsystem = "windows"]

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([760.0, 520.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "w2rpm",
        options,
        Box::new(|_cc| Ok(Box::new(w2rpm_gui::app::App::new()))),
    )
}
