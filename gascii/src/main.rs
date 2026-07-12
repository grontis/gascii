mod app;
mod canvas;
mod font_coverage;
mod fonts;
mod viewport;

fn main() -> eframe::Result {
    let t0 = std::time::Instant::now();
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "GASCII",
        options,
        Box::new(move |cc| Ok(Box::new(app::GasciiApp::new(cc, t0)))),
    )
}
