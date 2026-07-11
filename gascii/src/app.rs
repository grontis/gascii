use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui;
use gascii_core::{Cell, Document, Rgba};

use crate::canvas::{self, CanvasRenderer, NaiveRenderer};
use crate::fonts;
use crate::torture;
use crate::viewport::Viewport;

/// Rolling frame-time capture for the render-performance spike.
pub struct SpikeState {
    pub active: bool,
    frame_times_ms: VecDeque<f32>,
}

impl Default for SpikeState {
    fn default() -> Self {
        Self {
            active: false,
            frame_times_ms: VecDeque::with_capacity(Self::WINDOW),
        }
    }
}

impl SpikeState {
    const WINDOW: usize = 120;

    pub fn record(&mut self, elapsed: Duration) {
        if self.frame_times_ms.len() == Self::WINDOW {
            self.frame_times_ms.pop_front();
        }
        self.frame_times_ms.push_back(elapsed.as_secs_f32() * 1000.0);
    }

    pub fn avg_ms(&self) -> f32 {
        if self.frame_times_ms.is_empty() {
            return 0.0;
        }
        self.frame_times_ms.iter().sum::<f32>() / self.frame_times_ms.len() as f32
    }

    pub fn worst_ms(&self) -> f32 {
        self.frame_times_ms.iter().cloned().fold(0.0, f32::max)
    }

    /// `pct` in `[0, 100]`. Nearest-rank on a sorted copy of the current window.
    pub fn percentile_ms(&self, pct: f32) -> f32 {
        if self.frame_times_ms.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f32> = self.frame_times_ms.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).expect("frame times are never NaN"));
        let idx = ((pct / 100.0) * (sorted.len() - 1) as f32).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }
}

/// Small deterministic PRNG (xorshift64) — avoids pulling in the `rand` crate just to fill the
/// spike test-matrix docs with non-blank glyphs/colors (worst case for the naive text path).
struct Xorshift64(u64);
impl Xorshift64 {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 16) as u32
    }
}

const SPIKE_GLYPHS: &[char] = &['#', '@', '%', '*', '+', '-', '|', '░', '▒', '▓', '█'];

/// `--spike` auto-run matrix: a sanity row, the NFR-2 target row, and the culled-responsiveness
/// row. The fully-visible 1024x1024 stress case (~1M cells) is informational only and stays on
/// the interactive toolbar buttons, so automated runs aren't gated on its duration.
const SPIKE_AUTO_MATRIX: &[(&str, u16, u16, bool)] = &[
    ("80x25 (1:1)", 80, 25, false),
    ("200x100 (fit-to-window, NFR-2 target)", 200, 100, true),
    ("1024x1024 (1:1, culled)", 1024, 1024, false),
];
const SPIKE_AUTO_FRAME_BUDGET: usize = 90;
/// NFR-2: 60 fps at 200x100 fully visible.
const SPIKE_NFR2_TARGET_MS: f32 = 16.6;

fn random_doc(width: u16, height: u16) -> Document {
    let mut doc = Document::new(width, height);
    let mut rng = Xorshift64(0x1234_5678_9ABC_DEF1);
    for y in 0..height {
        for x in 0..width {
            let ch = SPIKE_GLYPHS[rng.next_u32() as usize % SPIKE_GLYPHS.len()];
            let fg = Rgba(rng.next_u32() as u8, rng.next_u32() as u8, rng.next_u32() as u8, 255);
            let bg = Rgba(rng.next_u32() as u8, rng.next_u32() as u8, rng.next_u32() as u8, 255);
            doc.set_cell(0, x, y, Cell { ch, fg, bg });
        }
    }
    doc
}

pub struct GasciiApp {
    pub(crate) doc: Document,
    pub(crate) viewport: Viewport,
    pub(crate) cursor: (u16, u16),
    pub(crate) hovered_cell: Option<(u16, u16)>,
    pub(crate) renderer: Box<dyn CanvasRenderer>,
    pub(crate) pending_fit: bool,
    show_torture: bool,
    pub(crate) spike: SpikeState,
    started: Instant,
    first_frame: bool,
    /// `--spike` CLI mode: auto-drives `SPIKE_AUTO_MATRIX` for a fixed frame budget per row,
    /// prints mean/p95/worst to stdout, then closes the window — no interaction required.
    spike_auto: bool,
    spike_matrix_index: usize,
    spike_frames_captured: usize,
    /// `(label, mean_ms, p95_ms, worst_ms)` per completed matrix row, used for the final
    /// decision line once the whole matrix has run.
    spike_results: Vec<(&'static str, f32, f32, f32)>,
}

impl GasciiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, started: Instant, spike_auto: bool) -> Self {
        fonts::install_canvas_font(&cc.egui_ctx);
        let mut app = Self {
            doc: Document::default_document(),
            viewport: Viewport::default(),
            cursor: (0, 0),
            hovered_cell: None,
            renderer: Box::new(NaiveRenderer),
            pending_fit: false,
            show_torture: false,
            spike: SpikeState::default(),
            started,
            first_frame: true,
            spike_auto,
            spike_matrix_index: 0,
            spike_frames_captured: 0,
            spike_results: Vec::with_capacity(SPIKE_AUTO_MATRIX.len()),
        };
        if spike_auto {
            let (label, width, height, fit) = SPIKE_AUTO_MATRIX[0];
            println!("[spike] starting matrix: {} rows, {SPIKE_AUTO_FRAME_BUDGET} frames each", SPIKE_AUTO_MATRIX.len());
            println!("[spike] row 1/{}: {label}", SPIKE_AUTO_MATRIX.len());
            app.start_spike(width, height, fit);
        }
        app
    }

    fn start_spike(&mut self, width: u16, height: u16, fit: bool) {
        self.doc = random_doc(width, height);
        self.spike = SpikeState {
            active: true,
            ..SpikeState::default()
        };
        if fit {
            self.pending_fit = true;
        } else {
            self.viewport = Viewport::default();
        }
    }

    fn stop_spike(&mut self) {
        self.doc = Document::default_document();
        self.spike = SpikeState::default();
        self.viewport = Viewport::default();
    }

    /// Advances the `--spike` auto-run by one captured frame; once a matrix row hits its frame
    /// budget, prints its stats, advances to the next row, and — after the last row — prints the
    /// gate decision and closes the window so the process exits cleanly on its own.
    fn drive_spike_auto(&mut self, ctx: &egui::Context) {
        self.spike_frames_captured += 1;
        if self.spike_frames_captured < SPIKE_AUTO_FRAME_BUDGET {
            return;
        }

        let (label, ..) = SPIKE_AUTO_MATRIX[self.spike_matrix_index];
        let mean_ms = self.spike.avg_ms();
        let p95_ms = self.spike.percentile_ms(95.0);
        let worst_ms = self.spike.worst_ms();
        println!(
            "[spike] result: {label} frames={} mean_ms={mean_ms:.3} p95_ms={p95_ms:.3} worst_ms={worst_ms:.3}",
            self.spike_frames_captured,
        );
        self.spike_results.push((label, mean_ms, p95_ms, worst_ms));

        let next_index = self.spike_matrix_index + 1;
        if next_index >= SPIKE_AUTO_MATRIX.len() {
            println!("[spike] matrix complete");
            self.print_spike_decision();
            self.spike_auto = false;
            self.stop_spike();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let (next_label, width, height, fit) = SPIKE_AUTO_MATRIX[next_index];
        println!("[spike] row {}/{}: {next_label}", next_index + 1, SPIKE_AUTO_MATRIX.len());
        self.start_spike(width, height, fit);
        self.spike_matrix_index = next_index;
        self.spike_frames_captured = 0;
    }

    /// Applies the NFR-2 gate thresholds to the 200x100 fit-to-window row and prints the
    /// resulting renderer decision.
    fn print_spike_decision(&self) {
        let Some(&(label, mean_ms, p95_ms, worst_ms)) = self
            .spike_results
            .iter()
            .find(|(label, ..)| label.starts_with("200x100"))
        else {
            println!("[spike] decision: inconclusive — 200x100 row did not complete");
            return;
        };

        let gated_ms = p95_ms.max(mean_ms);
        let decision = if gated_ms <= SPIKE_NFR2_TARGET_MS {
            "keep NaiveRenderer (comfortably <=16.6ms)"
        } else if gated_ms <= 33.0 {
            "keep NaiveRenderer, schedule galley-cache optimization (16.6-33ms, usable)"
        } else {
            "escalate to galley-cache renderer (>33ms, <30fps, unusable)"
        };
        println!(
            "[spike] decision basis: {label} mean_ms={mean_ms:.3} p95_ms={p95_ms:.3} worst_ms={worst_ms:.3} (target <= {SPIKE_NFR2_TARGET_MS}ms)"
        );
        println!("[spike] decision: {decision}");
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let label = if self.show_torture { "Canvas" } else { "Torture Sheet" };
            if ui.button(label).clicked() {
                self.show_torture = !self.show_torture;
            }
            if ui.button("Fit to Window").clicked() {
                self.pending_fit = true;
            }
            ui.separator();
            ui.label("Render spike:");
            if ui.button("80x25").clicked() {
                self.start_spike(80, 25, false);
            }
            if ui.button("200x100 (fit)").clicked() {
                self.start_spike(200, 100, true);
            }
            if ui.button("1024x1024 (1:1 culled)").clicked() {
                self.start_spike(1024, 1024, false);
            }
            if ui.button("1024x1024 (fit)").clicked() {
                self.start_spike(1024, 1024, true);
            }
            if ui.button("Stop Spike").clicked() {
                self.stop_spike();
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let coord = self
                .hovered_cell
                .map(|(x, y)| format!("{x},{y}"))
                .unwrap_or_else(|| "-".to_owned());
            ui.label(format!("cell: {coord}"));
            ui.separator();
            ui.label(format!("zoom: {:.0}%", self.viewport.scale() * 100.0));
            ui.separator();
            ui.label(format!("doc: {}x{}", self.doc.width, self.doc.height));
            if self.spike.active {
                ui.separator();
                ui.label(format!(
                    "frame avg: {:.2}ms worst: {:.2}ms",
                    self.spike.avg_ms(),
                    self.spike.worst_ms()
                ));
            }
        });
    }
}

impl eframe::App for GasciiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.first_frame {
            eprintln!("startup to first frame: {:?}", self.started.elapsed()); // NFR-5 check (< 1s)
            self.first_frame = false;
        }
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("palette").show(ui, |ui| {
            ui.label("Palette");
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if self.show_torture {
                torture::show(ui);
            } else {
                canvas::show(ui, self);
            }
        });

        if self.spike_auto {
            self.drive_spike_auto(ui.ctx());
        }
    }
}
