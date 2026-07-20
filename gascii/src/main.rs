mod app;
mod canvas;
mod font_coverage;
mod fonts;
mod image_bg;
mod png_export;
mod prefs;
mod ui;
mod viewport;

fn main() -> eframe::Result {
    let t0 = std::time::Instant::now();
    let launch_fullscreen = std::env::args().any(|a| a == "--fullscreen");
    let options = eframe::NativeOptions {
        // eframe's default opens too small to be useful. The minimum is the sidebar plus enough
        // desk for the default 80×25 document to be worth looking at.
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([920.0, 600.0])
            // The app draws its own title bar, so the OS one is off. That also removes winit's
            // resize borders and drag region — `ui::titlebar` reimplements both.
            .with_decorations(false)
            .with_resizable(true)
            .with_title("GASCII"),
        ..Default::default()
    };
    eframe::run_native(
        "GASCII",
        options,
        Box::new(move |cc| Ok(Box::new(app::GasciiApp::new(cc, t0, launch_fullscreen)))),
    )
}
