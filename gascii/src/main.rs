mod app;
mod canvas;
mod fonts;
mod torture;
mod viewport;

fn main() -> eframe::Result {
    let t0 = std::time::Instant::now();
    // `--spike`: headless-friendly render-spike mode. Auto-runs the test matrix, prints
    // mean/p95/worst frame times to stdout, then closes the window and exits — no interaction.
    let spike_auto = std::env::args().any(|arg| arg == "--spike");
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "GASCII",
        options,
        Box::new(move |cc| Ok(Box::new(app::GasciiApp::new(cc, t0, spike_auto)))),
    )
}
