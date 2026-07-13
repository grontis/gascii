# GASCII — Architecture & Requirements

**Status (2026-07-10):** specification agreed via design grilling session. Supersedes the brainstorm in `DESIGN.md` (kept as history). Decisions with real trade-offs are recorded as ADRs in `docs/adr/`; domain vocabulary lives in `CONTEXT.md` — terms capitalized here (Cell, Stroke, Blank, …) are defined there.

## 1. Product overview

GASCII is a native desktop editor for ASCII/ANSI art: a character-grid canvas drawn on with continuous pointer input (mouse/stylus/touch position), a curated character Palette, per-cell fg/bg color, and a density-brush system where drawing feels like a real art tool.

### Goals

- Drawing on a character grid that *feels* like an art tool — brushes, shading, buildup — not like typing.
- ANSI-art-tradition color model (per-cell fg+bg), truecolor internally.
- Grid integrity is sacred: nothing may ever break 1 cell = 1 column.
- Single standalone binary; no web stack.

### Non-goals (v1)

- Animation, layers UI (the *model* has layers; see ADR-0006), plugins/scripting, arbitrary font selection for the canvas, mobile.

## 2. Requirements

### 2.1 Document & data model

- **FR-1** A Document has fixed, explicit canvas dimensions (width × height in cells), resizable via an undoable resize operation — top-left anchored: growing pads with Blank at the bottom/right, shrinking crops from the bottom/right, and undo restores cropped content exactly. Default new-document size: **80×25**. No auto-grow, no infinite canvas.
- **FR-2** A Document contains an ordered stack of Layers; v1 always creates exactly one and exposes no layer UI (ADR-0006).
- **FR-3** Each Cell stores `{ch: char, fg: Rgba, bg: Rgba}` with 8-bit alpha (ADR-0002).
- **FR-4** The canonical empty cell is **Blank** = space glyph + fully transparent bg; there is no null cell state (ADR-0007). Erasing writes Blank; compositing and export trimming test blankness.
- **FR-5** Per-document **strict-ASCII mode**: non-ASCII palette Pages grey out; paste/import of non-ASCII warns.

### 2.2 Input & tools

- **FR-6** All drawing tools reduce pointer input to Strokes: press→release gestures producing a set of (cell, change) edits, committed atomically.
- **FR-7** Every tool has three **plane toggles** — write glyph? write fg? write bg? — filtering what each Stroke writes (ADR-0002). All three default to on, so a stroke fully replaces the cells it touches; toggling planes off is the opt-in for selective drawing.
- **FR-8** Tools in v1: pencil (stamp), eraser (stamps Blank through plane toggles), flood fill, rectangle, straight line, text mode, rectangular selection, and the density brush (its own tool, sharing the pencil's stroke pipeline).
- **FR-9** **Flood fill** fills the 4-connected region of cells *exactly matching* the clicked cell (glyph AND fg AND bg equal). Plane toggles govern only what is written, never the match.
- **FR-10** **Rectangle/line tools auto-join**: strokes crossing existing box-drawing characters resolve junctions by unioning arm directions (`─`+`│` crossing → `┼`, etc.). v1 ships the single-line box set; in strict-ASCII documents the tools draw `+ - |` with `+` junctions using the same join logic. The ASCII encoding is lossy by design: re-crossing an existing `+`/`-`/`|` does not recover its arms (a `+` has no single well-defined arm set), so the incoming stroke's arms win.
- **FR-11** **Text mode**: click a cell, type; typed characters are width-validated (FR-16); Enter moves to line start below, arrows navigate; typing stops at the right edge (no wrap), and Backspace at the anchor column is a no-op. A typing burst coalesces into one undo entry, committed at any session boundary — click-away, tool switch, undo, save/open/export, or window focus loss — never silently discarded.
- **FR-12** **Selection**: rectangular only. Move lifts cells (leaving Blank) into a floating stamp, dropped on click-away/Enter. Delete clears to Blank. Stamps transplant whole cells (glyph + both colors; plane toggles are not consulted), and the stamp's full rectangle replaces the destination on drop — Blank cells overwrite rather than reveal what was underneath, and the drag preview shows exactly what the drop will produce. A whole lift-move-drop gesture is one undo entry.
- **FR-13** **Clipboard**: copy puts plain text on the system clipboard; internal clipboard additionally preserves colors. Pasting external text goes through width validation and lands as a floating stamp. Pasted text is untrusted input: line and column counts are clamped to the document maxima before any allocation, and clamped or width-rejected characters are counted and surfaced as a warning, never silently discarded.

### 2.3 Palette & density brush

- **FR-14** The Palette is organized in curated Pages: **ASCII** (95 printable), **Box drawing** (U+2500 block), **Blocks/shades** (`░▒▓█ ▀▄▌▐`…). Future: Braille patterns. All curated glyphs are single-width (ADR-0003).
- **FR-15** **Ramps** are first-class palette objects: ordered light→dark char sequences (e.g. ` .:-=+*#%@`, `░▒▓█`). Built-ins ship in v1; user-defined ramps may follow.
- **FR-16** Any character entering a Document (palette, typing, paste, import) is validated single-width via `unicode-width`; double-width and combining characters are rejected (dropped with a visible warning), never stored (ADR-0003).
- **FR-17** The **density brush** exposes one intensity parameter (0.0–1.0) indexing the active Ramp. Intensity sources are pluggable (ADR-0004). v1 ships **Fixed** (slider + number-key shortcuts: `1`–`9` → 0.1–0.9, `0` → 1.0) and **Buildup** (each pass advances a cell one ramp step; a cell's ramp position is its glyph's index in the active ramp, and a glyph not on the ramp starts at the lightest step). Falloff, speed, and pressure are post-v1 sources; nothing else may depend on pressure.

### 2.4 Color

- **FR-18** Truecolor RGBA in the model; pickers offer constrained palettes (ANSI 16 and custom truecolor in v1; xterm-256 deferred post-v1) as a *picking* aid; quantization happens only at export (ADR-0002).
- **FR-19** Active text-color and background swatches (UI labels: "Text Color" / "Background"); pick-color-from-cell (eyedropper) sets both from the clicked cell.

### 2.5 History

- **FR-20** Single undo/redo history containing **every Document mutation** — strokes, resize, future layer ops. One Stroke = one entry; text bursts coalesce. App-level state (active tool, selected color, zoom) is never in history.

### 2.6 Viewport

- **FR-21** Zoom (Ctrl+scroll, discrete steps scaling the glyph raster) and pan (middle-drag / Space-drag), plus fit-to-window. Cell coordinates under the cursor shown in a status bar.

### 2.7 Files & export

- **FR-22** Native format `.gascii`: versioned JSON via serde, round-tripping the entire Document (dimensions, settings, all layers, all cell data) (ADR-0005). Unknown newer versions fail with a clear message. Loading is hardened against malformed or hostile files: invalid input always yields a specific error (never a crash), declared dimensions and layer counts are validated against the maxima (1024×1024 cells, 256 layers) before any allocation, and every loaded glyph passes the same width validation as typed input. Saves are atomic (write to a sibling temp file, then rename), so an interrupted save never corrupts an existing file.
- **FR-23** v1 exports: **plain text** (compositing flattened, Blank → space, trailing whitespace trimmed, also available as copy-to-clipboard) and **PNG** (rasterized at a chosen cell scale; straight-alpha output with Blank cells transparent, no baked-in editor background). PNG output dimensions are validated with overflow-safe math against a total-pixel cap before any buffer is allocated — a user-chosen scale is untrusted input like any other. Post-v1 ladder: ANSI escape text, HTML, REXPaint `.xp`.

### 2.8 Non-functional

- **NFR-1** **Portability rule:** no platform-specific APIs; cross-platform crates only (e.g. `rfd` for dialogs, `arboard` for clipboard). Must build on Windows/macOS/Linux, developed and tested on Windows; no CI until warranted.
- **NFR-2** Smooth interactive editing (60 fps target) at 200×100 cells; documents up to 1024×1024 must load and remain usable, with rendering optimizations budgeted, not speculative.
- **NFR-3** Canvas renders exclusively with bundled **Iosevka Fixed** (ADR-0008); glyph coverage of every curated palette range is enforced by automated tests against the embedded font file. UI chrome font is unconstrained.
- **NFR-4** All bundled assets OFL/permissively licensed; the binary is self-contained (no font/system dependencies).
- **NFR-5** Startup to interactive canvas < 1 s.

## 3. Architecture

### 3.1 Workspace layout

```
gascii/
├── Cargo.toml            # workspace
├── gascii-core/          # document model, tools, history, formats — ZERO gui deps
│   └── src/
│       ├── model.rs      # Rgba, Cell, Layer, Document, DocSettings
│       ├── edit.rs       # CellEdit, Edit, History
│       ├── tools/        # one module per tool; stroke pipeline, PlaneMask
│       ├── brush.rs      # IntensitySource trait, Fixed, Buildup; Ramp
│       ├── palette.rs    # Pages, curation, width validation
│       ├── join.rs       # box-drawing junction resolution (+ ASCII set)
│       └── io/           # gascii_json.rs, export_text.rs, export_png.rs
└── gascii/               # eframe app
    └── src/
        ├── app.rs        # eframe::App, toolbars, palette UI, swatches
        ├── canvas.rs     # custom egui widget: render + pointer→Stroke
        └── viewport.rs   # zoom/pan state, cell↔screen mapping
```

`gascii-core` is fully unit-testable headless (tools, fill, auto-join, undo, round-trip serialization are all pure functions over `Document`). The app crate owns egui types exclusively.

### 3.2 Core types (sketch)

```rust
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

pub struct Cell { pub ch: char, pub fg: Rgba, pub bg: Rgba }
impl Cell {
    pub const BLANK: Cell = Cell { ch: ' ', fg: Rgba(255,255,255,255), bg: Rgba(0,0,0,0) };
    pub fn is_blank(&self) -> bool { self.ch == ' ' && self.bg.3 == 0 }
}

pub struct Layer { cells: Vec<Cell> }              // row-major, w*h
pub struct Document {
    pub width: u16, pub height: u16,
    pub layers: Vec<Layer>,                        // v1: exactly one
    pub settings: DocSettings,                     // strict_ascii, ...
}

pub struct PlaneMask { pub glyph: bool, pub fg: bool, pub bg: bool }

pub struct CellEdit { pub layer: usize, pub x: u16, pub y: u16, pub before: Cell, pub after: Cell }
pub enum Edit { Cells(Vec<CellEdit>), Resize { before: DocExtent, after: DocExtent } }
pub struct History { undo: Vec<Edit>, redo: Vec<Edit> }
```

### 3.3 Stroke pipeline

```
pointer events ──▶ active Tool ──▶ pending edits (preview overlay, not yet in Document)
                                        │ pointer release / commit
                                        ▼
                     PlaneMask filter ──▶ Edit::Cells ──▶ Document + History
```

- Tools never mutate the Document directly; they accumulate pending `(cell, Cell)` pairs rendered as an overlay, committed atomically on release. Rectangle/line preview and floating selection stamps fall out of the same overlay mechanism.
- Pointer positions are interpolated between events (Bresenham over cell coords) so fast strokes don't skip cells.

### 3.4 Density brush

```rust
pub trait IntensitySource {
    fn sample(&mut self, ctx: &StrokeSample) -> f32;   // 0.0–1.0
}
```

`StrokeSample` carries position, timing, and the target cell's current ramp index — Buildup reads the current index and returns one step higher; Fixed ignores context. The brush maps intensity → ramp index → glyph, then stamps through the normal pipeline. Pressure, if ever added, is just another source (ADR-0004).

### 3.5 Rendering

Custom egui widget. v1 approach: per visible cell, paint bg rect then glyph via egui's text API from the bundled Iosevka Fixed. Optimization path (only if NFR-2 misses): cache per-row galleys, dirty-region tracking, then a glyph-atlas mesh. Layer compositing is top-down alpha-over per cell, done in core so exporters share it.

### 3.6 File format

`.gascii` = JSON: `{version, width, height, settings, layers: [...]}`. Cells are encoded structure-of-arrays per layer: glyphs as one string per row, colors as within-row run-length-encoded `#RRGGBBAA` hex (runs never cross row boundaries, so editing one row never ripples the encoding of others); the contract is FR-22 (full round-trip + mandatory version). Serde with `deny_unknown_fields` off, so older builds tolerate additive fields.

## 4. Build order (v1 milestones)

| # | Milestone | Contents |
|---|-----------|----------|
| M0 | Skeleton | Workspace scaffold; empty 80×25 grid rendering with Iosevka Fixed; blinking cell cursor; glyph torture-test sheet; zoom/pan |
| M1 | Draw | Pencil + eraser strokes with interpolation; palette Pages; fg/bg swatches; plane toggles; undo/redo |
| M2 | Persist | `.gascii` save/load; text mode; copy/export plain text; strict-ASCII toggle |
| M3 | Structure | Flood fill; rectangle + line with auto-join (+ ASCII fallback); selection, move, system clipboard |
| M4 | Feel | Ramps + Fixed & Buildup density brush; PNG export; resize dialog; polish pass |

Each milestone leaves a usable program; M0's grid-with-cursor is the surface everything else hangs off.

## 5. Open questions (deliberately unresolved)

- Layer UI design (model is ready; interaction design isn't started).
- Braille patterns page and "high-res" brush mode.
- Falloff and speed intensity sources — tuning curves TBD when built.
- ANSI/HTML/`.xp` export details (post-v1 ladder).
- Autosave / crash recovery policy.
- Maximum canvas dimensions cap (1024×1024 assumed; revisit with perf data).
- The meaning of the name "GASCII" — TBD by author :)
