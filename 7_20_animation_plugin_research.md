# Plugin Architecture & Animation Feature — Research and Findings

*2026-07-20 — pre-implementation exploration. Companion context: `CONTEXT.md`.*

Goal: add an animation feature to GASCII without growing the core application, by way of a
plugin architecture that lets features like this live outside the core crates.

---

## 1. Current architecture: extension-point audit

Two crates: `gascii-core` (headless, zero GUI deps — enforced by policy in `lib.rs`) and
`gascii` (eframe/egui shell).

### Plugin-friendly seams (already exist)

| Seam | Location | Why it helps |
|---|---|---|
| `Tool` trait | `gascii-core/src/tools/mod.rs:233` | Tools are UI-agnostic: consume resolved cell-space `ToolEvent`s, accumulate a `PendingCell` overlay, return an `Edit` on commit. Adding a tool = implementing a trait. Default-no-op hooks (`resync`, `accept_stamp`, `selection_overlay`, `caret`) already model optional capabilities. |
| `TOOLS` registry table | `gascii/src/app.rs:298` | Data-driven `ToolDef` table (`{kind, name, key, tip, make: fn() -> Box<dyn Tool>}`) — the single wire-up point for toolbox, shortcuts, sidebar rows, both mouse bindings. |
| Tool persistence by name | `gascii/src/prefs.rs:49` | Bound tools serialize as name strings looked up in `TOOLS` — new tools persist with no extra wiring. |
| `Edit` enum, `#[non_exhaustive]` | `gascii-core/src/edit.rs:29` | `History` is variant-agnostic and the sole writer of committed doc mutation. New undoable operations (frame ops) join as variants touching only the two apply/revert match arms. |
| `CanvasRenderer` trait | `gascii/src/canvas.rs:35` | Held as `Box<dyn CanvasRenderer>` on the app — swappable/wrappable rendering. This is where onion-skinning composites in. |
| Additive file format | `gascii-core/src/io/gascii_json.rs` | `version: u32` gate (reject newer), `#[serde(default)]` for additive fields, unknown fields tolerated. Schema can grow without a migration ladder. |
| `io::composite` | `gascii-core/src/io/mod.rs:12` | Single layer-flatten choke point every exporter builds on. |
| Dormant layer substrate | `gascii-core/src/model.rs:110` | `Document.layers` is a real `Vec<Layer>` with load/save/composite support and a `MAX_LAYERS = 256` cap — the app just hardcodes layer 0. Frames are structurally the same problem; the plumbing pattern exists. |

### Hostile spots (refactor cost centers, all app-side)

- **Scattered `ToolKind` matches** — per-tool behavior lives in ~8 helper fns in `app.rs`
  (`sized_slot`, `holds_session`, `tool_shows_hover`, `suppresses_tool_shortcuts`,
  `tool_shortcut_reachable`, `note_glyph_drawn`, `ALL_KINDS`) instead of on the tool/def.
- **Options UI is hardcoded and duplicated** — `sidebar.rs:173 binding_options` renders
  size/shape generically but special-cases `ToolKind::Brush`; `kiosk.rs:170` duplicates it at
  touch geometry. No per-tool `options_ui()` dispatch.
- **`ToolCtx.layer` hardcoded to 0** — `canvas.rs:268` and everywhere downstream. Any
  multi-layer or multi-frame targeting must thread an active-index through the draw path.
- **`GasciiApp` monolith** — `app.rs` is 3,663 lines; one ~140-line state struct owns
  everything. No sub-state modules to hang plugin state off of.
- **Eyedropper as `InertTool`** (`app.rs:269`) — evidence the `Tool` trait doesn't cover
  "tools that change app state, not the doc." Relevant for playback/timeline controls.

### Key file sizes (refactor gauge)

Core: `tools/mod.rs` 838 · `tools/select.rs` 721 · `io/gascii_json.rs` 664 · `edit.rs` 563 ·
`tools/text.rs` 516 · `model.rs` 395.
App: **`app.rs` 3663** · `canvas.rs` 1055 · `ui/sidebar.rs` 757 · `viewport.rs` 673 ·
`png_export.rs` 564 · `ui/kiosk.rs` 482 · `prefs.rs` 445.

---

## 2. Plugin architecture options (Rust, 2026)

### Option A — Static plugins: in-workspace crates behind Cargo features ✅ recommended

A plugin is a crate implementing host traits from a small `gascii-plugin-api` crate; the app
links it behind a feature flag and registers it at startup (the Bevy model).

- **Pros:** zero new deps in core; feature code fully quarantined in its own crate; no ABI
  hazards; full type safety; plugins can render real egui UI; `cargo build` just works;
  trivially testable.
- **Cons:** not runtime-loadable; third parties must build from source. (Acceptable — no
  third-party ecosystem is planned.)

### Option B — Dynamic libraries (`libloading` + `abi_stable`)

True runtime DLL loading. Rust has no stable ABI (layout can change between compiler *runs*),
so everything crossing the boundary needs `abi_stable`'s FFI-safe types — and **egui types
cannot cross it**, so plugins couldn't draw UI without a declarative-UI indirection layer.
Heavy, permanent maintenance tax; version lockstep with the host anyway. Rejected.

### Option C — WASM plugins (wasmtime / extism)

Excellent sandboxing and true third-party distribution, but plugins are headless by nature:
right for filters/generators/exporters ("dither this region", "export format X"), wrong for a
feature that owns a timeline panel, a playback clock, and renderer integration. **Deferred** —
a sensible *second tier* later for headless transform plugins, layered on the same registry.

### Deciding observation

Animation is a *deeply integrated* feature — document model, undo, file format, rendering, and
a major UI surface. Those are exactly what sandboxed plugin systems are worst at. Build the
static trait-based architecture now; design the API so a WASM tier can be added later.

### Proposed workspace shape

```
gascii-core        unchanged policy: headless, no new deps
gascii-plugin-api  NEW, tiny: host traits + registration types (deps: core + egui only)
gascii-anim        NEW: the animation plugin, feature-gated
gascii             app = plugin host; builds registries at startup
```

### Plugin API surface (only what animation needs — no speculative hooks)

- `register_tools(&mut ToolRegistry)` — feeds the `ToolDef` table, converted from a
  `const [ToolDef; 9]` to a runtime-built `Vec`.
- `panels()` — dockable UI surfaces (timeline strip = bottom panel; kiosk variant included).
- `wrap_renderer(Box<dyn CanvasRenderer>) -> Box<dyn CanvasRenderer>` — decorator chain for
  onion-skinning.
- `exporters()` — menu-registered export actions (GIF, spritesheet).
- `tick(&egui::Context)` — playback clock.
- Plugin-owned state lives on the plugin object, not on `GasciiApp`.

### Core-purity trade-off (flagged for decision)

A *purely* plugin-side animation (core untouched) forces frames to live as a parallel document
collection outside `Document` — undo, persistence, and tool targeting would all bypass the
choke points that keep this codebase sound. The clean line:

> **Frame *data* goes in core** (a small, dependency-free `Vec` of layer-stacks + timing
> metadata, mirroring how `layers` already works — ~200 lines of plain data model + new `Edit`
> variants, zero new crates). **All animation *behavior*** — timeline UI, playback, onion
> skinning, GIF export — **lives in the plugin.**

---

## 3. Animation feature design

### Prior art

| Tool | Takeaway |
|---|---|
| [durdraw](https://durdraw.org/) ([GitHub](https://github.com/cmang/durdraw)) | JSON `.dur` format: frames + per-frame delays. Closest format precedent. |
| [ASCII Motion](https://ascii-motion.xyz/) | Timeline controls, onion skinning, multi-format export — closest UX precedent. |
| [Moebius](https://github.com/blocktronics/moebius) | Modern ANSI/ASCII editor reference. |
| [REXPaint](https://www.gridsagegames.com/rexpaint/) | No animation — a gap GASCII would fill among native editors. |

### Model

- `Frame` = layer stack + optional per-frame duration override; document-level default fps and
  loop flag.
- All frames share one extent; resize applies across frames.
- Frame-count cap for the same untrusted-input reasons as `MAX_LAYERS`. Memory math: cells are
  cheap but 1024×1024 × many frames adds up — cap near 256 frames alongside the extent cap.

### Editing semantics

- Tools operate on the active frame unchanged (they already take `&Document`-shaped data —
  the `ToolCtx.layer` threading generalizes to frame+layer).
- One global undo history; `AddFrame` / `RemoveFrame` / `DuplicateFrame` / `ReorderFrame` /
  `SetTiming` as new `Edit` variants.

### UI

- Timeline bottom strip: frame thumbnails, add/duplicate/delete, drag-reorder, scrubbing,
  play/pause/loop, fps control.
- Kiosk/touch variant from day one (sidebar/kiosk duplication is a known pattern to design
  around, not repeat).
- Onion skinning: previous/next N frames tinted (red-behind / green-ahead convention) under
  the active frame, via the `CanvasRenderer` decorator.
- Playback: `ctx.request_repaint_after(frame_duration)` — egui-native, no threads.

### Persistence

- `.gascii` **version 2** with a `frames` array; v1 files load as a single frame.
- Single-frame documents keep saving as v1 → plain drawings stay compatible with older builds.

### Export

- Animated GIF (the already-shipped `image` crate encodes GIF).
- PNG spritesheet; per-frame text dump.
- Optional: `.dur` export for durdraw interop.

---

## 4. Phased plan

Each phase independently shippable and testable.

1. **Seam refactor (no behavior change).** Collapse scattered `ToolKind` matches into
   `ToolDef` capability fields / `Tool` trait methods; per-tool `options_ui()` dispatch so
   sidebar and kiosk render from one description; thread an active layer/frame index through
   the `ToolCtx.layer = 0` hardcode. Pays for itself even if plugins never happen.
2. **Plugin API + host.** Extract `gascii-plugin-api`; convert `TOOLS` to a runtime registry;
   add panel/renderer/exporter/tick registration. Prove it by porting one built-in (density
   brush) to register through the API.
3. **Frame substrate in core.** Model, `Edit` variants, format v2, load hardening,
   resize-across-frames.
4. **`gascii-anim` plugin.** Timeline panel (+ kiosk variant), playback, onion skinning.
5. **Exporters.** GIF, spritesheet, text frames, optional `.dur`.

### Open decisions

- [ ] Accept frame *data* in core (recommended) vs. purely plugin-side frames?
- [ ] Phase 1 refactor scope up front (recommended — cheapest moment) vs. minimal threading only?
- [ ] Frame cap value; whether per-frame duration overrides ship in v1 of the feature or fps-only.

---

## 5. References

- [NullDeref: Plugins in Rust — The Technologies](https://nullderef.com/blog/plugin-tech/)
- [NullDeref: Plugins in Rust — Reducing the Pain with Dependencies](https://nullderef.com/blog/plugin-abi-stable/)
- [abi_stable docs](https://docs.rs/abi_stable/)
- [Rust forum: linking issues in dynamic plugin architectures](https://users.rust-lang.org/t/linking-issues-when-designing-a-dynamic-plugin-based-architecture/136388)
- [durdraw](https://durdraw.org/) · [ASCII Motion](https://ascii-motion.xyz/) · [REXPaint](https://www.gridsagegames.com/rexpaint/) · [Moebius](https://github.com/blocktronics/moebius)
