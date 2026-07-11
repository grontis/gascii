# Native Rust binary with egui/eframe

GASCII is a native desktop program, not a web app — user preference for standalone binaries, plus current enthusiasm for Rust. GUI is egui via eframe: immediate-mode suits a tool UI where the canvas is one custom widget and the chrome (palette, toolbar, swatches) is cheap to build. Rejected: iced (slower iteration on custom widgets), raw wgpu+winit (too much chrome to hand-build), Tauri (webview defeats the native goal).

Status: accepted (egui not yet validated hands-on; revisit only if the canvas widget proves infeasible).
