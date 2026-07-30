# GASCII Architecture & Code Review — `animation-spike`

> **Resolution addendum (2026-07-30):** every finding below was addressed in a four-phase fix pass the day after the review; the suite grew from 819 to 888 tests, clippy stays at zero warnings, all uncommitted.
>
> - **Phase 1 (correctness/UX):** H1 (visible-rect culling + edit-id dirty gate, cache eviction), H2/H3 (clipboard/paste gated on widget focus), M1 (session-meta dirty snapshot), M2 (RasterAssets built once per export + glyph cache — export still runs on the UI thread; worker thread remains future work), M3 (structural frame edits validated at `History::apply`, graceful no-op), M4 (`resumed_after_suppression` tick signal resets the space-hold), M5 (Escape consumed only when it acts), M6 (AltGr rejection), L1/L3/L4/L6 + Ctrl+D/Ctrl+Plus/key-repeat fixes, `run_export` dedup, frame-header unification, debug-gated startup print, `slot_mut` deleted, decorator `PaintCtx`. Test gaps 1–5 and 7 closed.
> - **Phase 2 (plugin API round, per `.agentwork/architect/PLAN_plugin-api-round_2026-07-29.md`):** `ToolKind::Plugin(&'static str)`, derived stamp slots, name-keyed stamp prefs (dual-write legacy shape until merge), instance-free `PluginDescriptor` table, `PluginShortcut` declarations feeding the `?` overlay + a startup hard-error collision validator (test gap 6 now structural), `IconPath` in the API, `OptionsGeom`/`ToolCtxPatch` made plugin-neutral, source-stability policy documented. H4, M7–M10 resolved; `PanelOutcome` per-property fields kept by explicit decision.
> - **Phase 3:** `app.rs` (7,226 lines) → `app/` directory: mod.rs 1,004 + tests.rs 4,203 + tools_registry/session/dialogs/files/plugin_host + `ui/menu.rs`; dialog bools → `Option<OpenDialog>` (H5, M11).
> - **Phase 4:** `canvas::show` → 6-line orchestrator over `handle_canvas_input` / `route_owner_keys` / `paint_canvas` (M12).
>
> Still open by choice: worker-thread export, unbounded undo memory (documented tradeoff), playback single-step catch-up (L5, pinned deliberate), onion/playback layer-0 rendering (single-layer app).

**Date:** 2026-07-29
**Scope:** the two commits on `animation-spike` (`9010059` animation plugin, `04543f3` keyboard shortcut fixes) plus a general health review of the whole application.
**Method:** four parallel deep-dives (animation system + frame substrate, plugin architecture, keyboard shortcuts, general app health). Every finding below was verified against the actual code — file paths and line numbers refer to the `animation-spike` working tree at `04543f3`. Several findings about egui behavior were verified against the vendored `egui-0.35.0` / `egui-winit-0.35.0` sources, not assumed. Full workspace test suite ran green during review: **819 tests passed, 0 failed**; `cargo clippy --workspace --all-targets` produces **zero warnings**.

**No code was changed as part of this review.**

---

## Executive summary

The branch is in very good shape. The hard parts of the animation design — frame ops as pure `Edit`-producing functions, undo/redo carrying `active_frame_before/after`, positional frame addressing justified by a strictly-LIFO history, byte-identical v1 output for single-frame docs — are **correct, documented, and pinned by tests**. The plugin seam has clean dependency directions, a genuinely one-way mutation channel (`PanelOutcome` → host `apply_edit`), and per-app plugin instances. Workspace hygiene is exemplary: no `unsafe`, no TODO/FIXME markers, 23 total `expect`/`panic!` sites in non-test code (each with a documented invariant), uniform error routing through `last_error`, and atomic file writes.

Nothing found rises to Critical. The five High-severity findings:

| ID | Area | Finding |
|----|------|---------|
| H1 | Animation | Timeline strip recomposites + rehashes **every** frame of the document on **every** repaint |
| H2 | Shortcuts | Ctrl+X / Ctrl+C fire while a text field has keyboard focus — Cut silently deletes canvas selection contents behind a focused field (**regression introduced by `04543f3`**) |
| H3 | Shortcuts | Ctrl+V pastes into a focused text field **and** spawns a canvas float in the same keypress (pre-existing, not fixed by `04543f3`) |
| H4 | Plugin API | A third plugin contributing a tool requires coordinated host edits in three places (panicking `match` arms on tool names) |
| H5 | App structure | `gascii/src/app.rs` is a 6,655-line monolith with a 58-field `GasciiApp` struct holding ~9 distinct responsibilities |

H2 and H3 are the only findings that damage user data today; H1 is the only meaningful runtime performance problem; H4 and H5 are architecture debt that costs nothing until the next feature lands, at which point they cost a lot.

---

## Part 1 — Animation system

Covers `gascii-anim/`, `gascii/src/anim_export.rs`, `gascii/src/png_export.rs` (shared rasterizer), and the frame substrate in `gascii-core` (`frame_ops.rs`, `model.rs`, `edit.rs`, `io/gascii_json.rs`, `io/export_png.rs`).

### Strengths

- **The frame substrate architecture is sound.** Frame ops are pure `Edit`-producing functions (`gascii-core/src/frame_ops.rs`); `History` remains the sole mutation choke point; `active_frame_before/after` is baked into each structural `Edit` so undo/redo restores the cursor deterministically. The positional-frame-addressing safety argument (LIFO history) is written down in `edit.rs`'s module doc *and* pinned by tests (`history_is_a_single_strictly_lifo_stack_across_mixed_edit_kinds`; the full-stack test at `frame_substrate_integration.rs:101`). This is the hard part of the design, and it is correct.
- **`CellEdit` gained a `frame` field**, so tool strokes address an explicit frame independent of the frame cursor — the mid-stroke reorder/resync test (`frame_substrate_integration.rs:162`) exercises the nastiest interleaving directly.
- **Format versioning is careful.** Single-frame docs still emit byte-identical v1 (pinned to the exact key set, `frame_substrate_integration.rs:276`); the v2 loader validates frame count, per-frame layer count, and the joint `MAX_TOTAL_CELLS` budget *before* any allocation.
- **Plugin isolation is clean.** `gascii-anim` depends only on `egui` + `gascii-core`; all document mutation flows through `PanelOutcome`; the `Rc<RefCell<SharedState>>` split between plugin and `OnionRenderer` is single-threaded-appropriate, and all `RefCell` borrows are dropped before `inner.paint` (`decorator.rs:62,76-80`).
- **GIF export correctness:** streaming one-frame-at-a-time encode; the loop flag is verified against raw NETSCAPE2.0 bytes rather than the lossy decoder API; delays are rounded to GIF's centisecond grid with a 10 ms floor; `image` 0.25.10's encoder forces `DisposalMethod::Background` per frame (verified in the crate source), so transparent-background GIFs won't smear.

### H1 — Timeline strip recomposites and rehashes every frame on every repaint

`timeline.rs:216-217` (`body`) loops `for i in 0..doc.frame_count()` calling `ThumbnailCache::get_or_build` for *every* frame. egui's `ScrollArea` clips painting but does not skip this code for offscreen thumbnails — contradicting `thumbnail.rs`'s own doc comments ("only a frame actually scrolled into view ever calls `get_or_build`", lines 18-20, 33-34). Worse, `get_or_build` (`thumbnail.rs:36-40`) calls `composite_frame` — allocating a fresh `Vec<Vec<Cell>>` of the whole frame — **and** hashes every cell *before* consulting the cache; a cache hit only skips the texture upload.

Cost per repaint is O(frame_count × width × height × layers). Concrete scenario: a 256-frame 80×25 doc = 512K cells composited + hashed per repaint, recurring at playback rate (~every 100 ms) or on every pointer-move repaint while drawing.

**Fix direction:** check a cheap dirty signal (host edit-id, or hash only on invalidation) before compositing, and/or cull to the `ScrollArea`'s visible rect.

### M1 — `Loop` toggle bypasses dirty tracking → silent data loss on close

`drain_panel_outcomes` writes `self.doc.loop_playback` directly (`app.rs:1247-1252`), while `is_dirty()` is purely a `history.top_edit_id() != saved_marker` comparison (`app.rs:1163-1165`). `loop_playback` **is** serialized in the v2 envelope — it is real document state — but toggling it never dirties the document: the title-bar dot stays clean and the close-confirm dialog (`app.rs:2573`) never fires. Scenario: open a multi-frame file, uncheck Loop, close → the change is silently discarded with no prompt. (Being non-undoable is documented and fine; the dirty-tracking gap is the bug.)

### M2 — GIF/spritesheet export repeats expensive fixed work per frame, synchronously on the UI thread

`rasterize_composited` (`png_export.rs:87`) parses the entire canvas font via `fontdue::Font::from_bytes` on **every call**, and re-premultiplies + re-resizes the background image (`png_export.rs:95-115`) on every call. `export_gif`/`export_spritesheet` (`anim_export.rs:45-54, 76-81`) call it once per frame — a 100-frame export parses the same TTF 100 times and resizes the same background 100 times. Glyphs are also rasterized per cell with no per-(char, size) cache. All of this runs inside `run_export` on the UI thread (`app.rs:2418`), freezing the app for the duration.

**Fix direction:** hoist font parse + background resize out of the per-frame loop; add a glyph bitmap cache; consider a worker thread for export later.

### M3 — Host applies plugin-supplied `Edit`s unvalidated; structural frame edits panic on out-of-range indices

`apply_forward`/`apply_backward` (`edit.rs:82, 86, 91-92, 113`) use `doc.frames.insert(*index, ..)`, `doc.frames.remove(*index)`, and `doc.frames[*index]` directly — all panic on an out-of-range index — while `Edit::Cells` deliberately no-ops gracefully via `set_cell_at` (an asymmetry pinned by `apply_and_undo_do_not_validate...`). Since `PanelOutcome.edits` is the public plugin API (`drain_panel_outcomes` → `apply_edit` → `History::apply`, `app.rs:1241-1243`), any buggy plugin can crash the host with `AddFrame { index: len+1, .. }`. Theoretical within the current in-workspace plugin set, but it is the one place the plugin boundary trusts blindly.

**Fix direction:** bounds-validate structural edits at the `History::apply` seam (no-op + `debug_assert`, matching `Cells`' philosophy).

### M4 — Stale Space-hold across a modal fires a spurious play/pause toggle

`handle_keys` — and therefore `AnimPlugin::tick` — only runs while `!modal_open()` (`app.rs:2761-2762`). The plugin resets its hold state on OS focus loss (`plugin.rs:110-118`) but has no equivalent for modal suppression. Scenario: hold Space (hold latches), press Ctrl+E so the export dialog opens, release Space while the dialog is open (no tick runs), close the dialog → next tick sees `active=true, saw=false, space_down=false` and `resolve_space_hold` fires `toggle_playback` for a hold the user completed inside the modal. This is exactly the bug class the `was_focused` edge-reset was added for (`plugin.rs:104-109`); the modal axis needs the same treatment.

### Low findings

- **L1 — `step_duration` can panic via a file-sourced duration.** `timeline.rs:99`: `(current as i32 + delta_ms).max(10) as u32`. The v2 loader accepts any `u32` for `duration_override` unbounded. A hand-edited file with a duration near `i32::MAX` panics on the "+10 ms" click (overflow-checks are on in release); values above `i32::MAX` wrap negative and silently collapse to 10 ms. Do the math in `i64` or clamp at load.
- **L2 — `ThumbnailCache` never evicts** (`thumbnail.rs:37-39`). After deleting frames, trailing GPU textures live until the doc grows again or the session ends. Bounded (≤256 small textures) but easy to trim.
- **L3 — Onion-skin paint order inverts distance prominence.** `paint_onion` (`decorator.rs:106-116`) paints neighbors in increasing distance, so a frame 3 steps away paints *over* the frame 1 step away where they overlap; tint is constant rather than fading. Iterate farthest→nearest and/or fade alpha by distance.
- **L4 — Loader inefficiencies.** `load_str` parses the full JSON twice (version probe, then envelope); `load_v2` allocates a full blank layer per frame then immediately clears it — ~12 MB churn per frame at max extents. Harmless at typical sizes.
- **L5 — Playback catch-up advances at most one frame per tick** (`plugin.rs:191-203`), pinned as deliberate by test. After a stall, the animation fast-forwards at repaint rate instead of jumping; wall-clock fidelity drifts under load. Noting only.
- **L6 — Two one-way doors in the duration model.** `step_duration` always writes `Some(..)` — once touched, a frame can never fall back to the document default; and `frame_duration_ms` itself has no UI control anywhere, so it is reachable only by hand-editing the file. The data model explicitly supports both reversals.

### Nits

- Stale module doc: `timeline.rs:7-12` claims the panel "never offers a control to change" `loop_playback` — the Loop checkbox at `timeline.rs:129-132` now does, via `PanelOutcome::set_loop_playback`.
- `paint_frame_cells`/`paint_tinted_frame` read layer 0 only (`decorator.rs:93,127`) while every exporter composites all layers. Consistent with the single-layer app today, but playback will silently diverge from export the day multi-layer files render.
- `thumbnail.rs` `content_hash` excludes `doc.background`, which `effective_rgb` blends in — irrelevant while background is immutable after creation; worth a comment.

---

## Part 2 — Plugin architecture

Covers `gascii-plugin-api/`, `gascii-density-brush/` (first consumer), and host integration in `gascii/src/app.rs`.

### Overall shape

A compile-time, in-repo plugin architecture. The host owns a fixed factory list (`plugin_factories()`, `app.rs:583-588`), merges plugin tool bundles into its `ToolDef` registry (`build_tools`, `app.rs:545-563`), retains one live instance per factory per app, folds `wrap_renderer` into the canvas renderer, and drives three hooks per frame: `options_ui`, `tick`, and `panel`. Mutation is one-way: plugins return `PanelOutcome` requests; the host applies them.

**Dependency directions are clean and verified:** `gascii-core` has zero GUI deps; `gascii-plugin-api` depends on core + `egui` (no eframe); both plugin crates depend on core + api + egui only; the binary depends on everything. Core never depends on the plugin API. No cycles.

### Strengths

- **Request/apply mutation split** keeps all document mutation authority in the host, each edit its own undo entry, plugin errors routed into the same `last_error` channel — tested end-to-end (`app.rs:3242-3527`).
- **Borrow ergonomics solved correctly and explicitly.** `HostFacts<'_>` (`app.rs:670-694`) is built from `&self.doc` plus owned facts, keeping the borrow disjoint from `&mut self.plugins`; the two-pass draw-then-drain shape avoids the double-borrow without host-side `RefCell`. The doc comments explain the exact compiler constraint — good learning material.
- **Per-app plugin instances, never process-global** — pinned by `two_independent_gascii_apps_never_share_brush_plugin_state` (`app.rs:3081`).
- **`panel` taking the host's live root `&mut Ui`** (not `&Context`) is a subtle, correct egui decision — pinned by a test (`app.rs:3149`) so an egui upgrade that changes placer semantics fails loudly.
- Cross-boundary test coverage is exceptional (cross-crate downcast, byte-identical single-frame layout with/without the anim panel, fold-order test for the decorator chain).

### H4 — A third plugin's tool requires coordinated host edits in three places

Tool identity is host-assigned via per-tool `match` arms that **panic** on unknown names:

1. The `ToolKind::Brush` variant in the host's own enum — `app.rs:250`.
2. `kind_for_plugin_tool` — `app.rs:601-606`: `match name { gascii_density_brush::BRUSH => ToolKind::Brush, _ => panic!(...) }`.
3. `stamp_slot_for_plugin_tool` — `app.rs:612-617`: same shape, panics for any sized tool without a hand-reserved slot.

A third plugin contributing one sized tool must edit the host in three coordinated locations before `tools()` stops panicking at startup. The rationale (persistence stability — `prefs.rs` persists stamps positionally by `stamp_slot`) is real and documented, but prefs already persist *tool bindings by name* (`app.rs:645`); persisting stamps by name as well would dissolve both match functions, leaving `ToolKind` for built-ins only. As shipped, `Plugin::register_tools` is *description*, not *registration* — the host holds a veto list. (`gascii-anim` avoided all of this only because it registers zero tools.)

### M8 — The "generic" API accretes plugin-specific members

Three places where one plugin's needs are baked into shared API types (API growth is O(plugin features)):

1. **`OptionsGeom.wrap_brush_mode` / `brush_slider_h`** (`gascii-plugin-api/src/options.rs:14-17`) — a plugin-neutral layout struct with fields named for one plugin's widgets.
2. **`Plugin::extra_tool_ctx(&self, name) -> Option<(DensityMode, Vec<char>)>`** (`plugin.rs:105-107`) — the return type is exactly the density brush's context; a future tool needing different context forces a breaking tuple change or a second bespoke method. An opaque/extensible context object would scale.
3. **`PanelOutcome.set_loop_playback`** (`panel.rs:17-22`) — one named field per non-`Edit` document property a plugin wants to write; predicts a new field + `drain_panel_outcomes` arm per future plugin.

Not broken — honest names are a virtue at two consumers — but the API's shape is currently "the union of its two consumers' requirements," not a stable abstraction.

### M9 — Dual-construction registry contract (fragile-by-convention, well-mitigated)

`build_tools` (`app.rs:545-550`) constructs **throwaway** plugin instances to harvest `register_tools()` into the **process-global** `TOOL_REGISTRY: OnceLock` (`app.rs:642`), while app construction builds the **retained** instances (`app.rs:972`). `ToolDef.plugin_slot` is only valid because both sites iterate `plugin_factories()` in the same order — held by convention plus tests. Residual risks: `register_tools(&self)` takes an instance, so registration output *could* depend on constructor-time state and silently diverge between the two instances; and the registry being process-global while plugins are per-app is enforced only implicitly by the `&'static str` fields in `PluginToolCapabilities`. Cleaner: harvest from the retained instances at app construction, or make registration an associated function.

### M10 — The plugins' domain logic actually lives in `gascii-core`

The density brush's entire tool implementation, `DensityMode`, `Ramp`, and the `ToolCtx.density`/`ramp` fields are in core (`gascii-core/src/tools/density_brush.rs`); `gascii-anim`'s substrate (`frame_ops`, durations, `loop_playback`) is likewise core. The plugin crates hold only UI, session state, and shortcuts. Defensible (core hosts all `Tool` impls), but the plugin boundary is thinner than the crate layout suggests: a genuinely new tool plugin will typically need a core PR too, and core's `ToolCtx` already carries brush-specific fields every other tool ignores (acknowledged at `canvas.rs:306-308`). Worth deciding explicitly whether plugin-crate `Tool` impls are permitted; nothing currently demonstrates it.

### Low findings

- **Key-collision guard is debug-only.** `build_tools`'s reserved-chord check is a `debug_assert!` (`app.rs:559-562`); a colliding plugin key in release ships silently, permanently killing one of the two bindings. Runs once at startup — should be a hard error the day any out-of-repo plugin exists. Both uniqueness guards are tests, not runtime checks.
- **`tick` capability breadth.** `tick(&mut self, ui: &mut Ui, ...)` hands plugins the same root `Ui` as `panel`, so a tick can draw widgets or consume arbitrary input during the shortcut phase. Both shipped plugins behave; the signature just can't express "input and scheduling only."
- **Render override without input override.** During playback, `OnionRenderer` shows the playback frame, but the host can't know the view is overridden (`SharedState` is `pub(crate)`), so canvas input still edits the invisible active frame. Documented as expected in `7_22_MANUAL_QA.md` (Part 3.3 step 7) — a deliberate scope cut, but a structural one-way gap worth remembering if playback becomes more prominent.
- **`add_frame_via_menu`** (`app.rs:1334-1366`) exists solely because plugins have no menu-contribution hook; the comment names `gascii-anim` directly. A `menu_items()` hook is the obvious future extension point.
- **No versioning story — and none needed yet, but say so.** Plugins are workspace crates statically linked; `unsafe_code = "forbid"` forecloses `cdylib` plugins anyway. The trait already absorbed one breaking change via monorepo recompile. A sentence in `gascii-plugin-api`'s crate doc ("source-stable only; breaking changes absorbed by the workspace") would make the policy explicit.

### Nits

- `stamp_slot_for_plugin_tool` returns `Option<u8>` but can never return `None` (it panics first, and the caller gates on `cap.sized`). Return `u8`.
- `extra_tool_ctx` clones the ramp `Vec<char>` on every `tool_ctx` build during a Brush drag (`canvas.rs:312`). Acknowledged in comments; ~4-16 chars, noise — `&[char]` or `Rc<[char]>` would remove it.
- Two plugins both returning `set_active_frame` in one frame: last-in-plugin-order silently wins (`app.rs:1240-1246`). Harmless, undocumented as an ordering contract.
- Tool *name* is the cross-boundary identity (`HostFacts::is_bound`, `app.rs:681-683`); a name collision between plugins is caught only indirectly.
- The `Rc<RefCell<SharedState>>` split is well-commented and borrow-disciplined, but it is the one place a future edit could introduce a `RefCell` panic at paint time; no regression test paints while `panel` is mid-borrow.

---

## Part 3 — Keyboard shortcuts

Covers `gascii/src/chords.rs` (new, 441 lines), `handle_keys` in `app.rs`, canvas session-key routing, plugin tick shortcuts, and the `9010059..04543f3` diff. egui claims verified against vendored `egui-0.35.0` / `egui-winit-0.35.0` sources.

### Strengths

- **The egui facts the design leans on are correct.** `Modifiers::matches_logically` really does ignore extra Shift/Alt, so the Redo-before-Undo and SaveAs-before-Save consumption ordering (`app.rs:1579-1587`, `chords.rs:105-119`) is necessary and right — pinned by a real regression test (`chords.rs:420-440`, which asserts the *full* fired vector; the strongest test in the file).
- **The headline fix is genuine.** egui-winit intercepts Ctrl+C/X/V before they ever become `Event::Key` (`egui-winit-0.35.0/src/lib.rs:1000-1016`), so the old `consume_key(COMMAND, C)` code deleted in `04543f3` was dead. The new `Event::Copy`/`Event::Cut` scan (`app.rs:1596-1601`) is the correct mechanism, well tested including the Ctrl+Insert path.
- **One label source of truth:** every menu `shortcut_text` and the `?` overlay read through `chord_label`/`chord_rows` — labels cannot drift from bindings for host chords.
- **One dispatch path.** Effectively a single host dispatch — `handle_keys`, once per frame under one `!modal_open()` gate — plus two coordinated satellites (canvas session-key routing, gated on `widget_focused` and `keyboard_owner`; plugin `tick` at the tail of `handle_keys`). Every `consume_key` in `handle_keys` corresponds to a CHORDS row; each `HandWritten` row's stated reason checks out. No multi-key chord state machine exists (all "chords" are single keystrokes), so the stale-pending/timeout bug class cannot occur. Mac handling is correct throughout (`Modifiers::COMMAND`; Cmd+Shift+Z and Cmd+Y both redo).
- `resolve_space_hold` (`gascii-anim/src/plugin.rs:47-69`) is a clean pure state machine with an explicit OS-focus-loss reset and edge-case tests.
- ~40 shortcut tests in `app.rs` plus 7 in `chords.rs`, including suppression-while-focused, the escape precedence chain, and F11 mid-text-burst.

### H2 — Ctrl+X / Ctrl+C fire while a widget has keyboard focus; Cut mutates the document behind a focused text field *(regression from `04543f3`)*

`handle_keys` gates Undo/Redo/Select-All on `widget_focused` (`app.rs:1579-1588`), but the `copy`/`copy_all`/`cut` flags are computed and dispatched **unconditionally** (`app.rs:1600-1601`, `1696-1704`). `cut_selection` (`app.rs:1445-1455`) deletes the selection region from the document.

Repro: drag a marquee with the Selection tool (it keeps `keyboard_owner`), open the color-picker popup, focus the hex field (`ui/sidebar.rs:455` — a popup, outside `modal_open()`), select the hex text, press Ctrl+X. egui emits `Event::Cut`; the scan comment ("Scanned (not consumed): nothing else in this app reads these events", `app.rs:1598`) is wrong — egui's `TextEdit` reads the same events via `filtered_events`. The field cuts its text **and** `cut_selection` silently deletes the canvas selection contents in the same keypress. Ctrl+C in the same state overwrites the clipboard with the canvas selection and calls `flush_all()` (committing any pending float) as a side effect.

**Fix:** gate `copy`/`copy_all`/`cut` on `!widget_focused`, exactly as Undo/Redo already are.

### H3 — Ctrl+V pastes into a focused text field AND spawns a canvas float *(pre-existing)*

`canvas.rs:700-711` collects every `Event::Paste` with no `widget_focused` gate (the session-key block directly above it *is* gated, `canvas.rs:622-623`). Pasting a hex color into the focused hex field also calls `paste_text` (`app.rs:1494-1529`), which rebinds a slot to Selection, ends the other session, steals `keyboard_owner`, and drops the pasted text as a float on the canvas. The adjacent "no double-handling" comment (`canvas.rs:697-699`) only considers the Text/Selection blocks, not `TextEdit`. Pre-existing on `main`, but `04543f3` rebuilt the neighboring clipboard logic without closing it. Same fix shape as H2.

### M5 — Escape is consumed even when it does nothing, starving egui popups/menus of their close key

`app.rs:1666-1672`: when no session/stroke is live, Escape is consumed via `consume_key` *unconditionally* — the `is_fullscreen` check gates only the action, not the consumption. `handle_keys` runs at the top of `ui()` before any panel draws, and egui popups close on Escape at draw time (`egui-0.35.0/src/containers/popup.rs:611`) — the event is already gone, so **Escape never closes the color-picker popup or an open menu**. (Widget *unfocus* still works — egui handles that from raw input before the app runs; consequence: in fullscreen, Escape pressed to leave the hex field also exits fullscreen in the same press, since `should_handle_escape_for_fullscreen` (`app.rs:186-188`) ignores widget focus.) Fix: only consume when `is_fullscreen` (and consider `!widget_focused`). Pre-existing on `main`.

### M6 — AltGr on Windows international layouts triggers Ctrl-chords

`matches_logically` ignores extra Alt, and Windows reports AltGr as Ctrl+Alt. So `GenericAlways` rows fire on AltGr combos: on a Polish layout, AltGr+O (typing "ó") matches `(COMMAND, Key::O)` and opens the file dialog; AltGr+S matches Save. egui-winit already suppresses the `Event::Text` for ctrl-modified keys, so the user gets a surprise command instead of a character. Mitigation: reject events with `alt` set when matching COMMAND-only patterns — a small wrapper in `consume_generic_chords` (`chords.rs:247-256`) covers all rows at once.

### M7 — `reserved_global_keys` only partially closes the gap it exists for; plugin shortcuts are invisible in the `?` overlay

`chords.rs:268-273` special-cases `Space` (gascii-anim's play/pause hold, invisible to the chord table because it uses `key_down`) — but every *other* plugin-tick shortcut has the same problem and was not added: gascii-anim's `O`, `,`, `.` (`plugin.rs:141,150`), the density brush's digit keys (`gascii-density-brush/src/plugin.rs:181-188`), and the host's hand-written `=` zoom alias (`app.rs:1717`). A future plugin registering tool key `O`/`,`/`.`/`=` sails past the `build_tools` debug assert and silently kills the existing shortcut (tool lookup consumes before plugin ticks run). The doc comment on the Space exception (`chords.rs:263-267`) describes exactly this failure mode while leaving five keys exposed to it.

Related discoverability gap: the help overlay lists tools + host chords only (`app.rs:2008-2017`); gascii-anim's Space/`O`/`,`/`.`/Shift+D and the brush's `1`-`9` appear nowhere in any UI — the newest shortcuts (this branch's whole point) are the least discoverable. **One structural fix covers both:** a plugin-API shortcut declaration (`(key, label)` list) that feeds `reserved_global_keys`, the collision assert, and the `?` overlay.

### Low findings

- **Ctrl+D can discard a pending float while a field is focused.** `ChordId::Deselect` is `GenericAlways` (`chords.rs:214-219`); `deselect` sends `ToolEvent::Cancel`, which discards a lifted-but-undropped float (`app.rs:1474-1479`). Same gating fix as H2 — a destructive chord belongs in the focus-gated class.
- **"Ctrl++" doesn't zoom; only literal Ctrl+= does.** On US layouts Ctrl+Shift+= produces `Key::Plus`, which no COMMAND row matches; the modifier-less handler has the `Plus || Equals` fallback (`app.rs:1716-1717`) but the Ctrl alias doesn't. Self-consistent with its label, just asymmetric.

### Nit — key-repeat on toggles

`consume_key` includes repeats, so holding `G`, `X`, or F11 flickers grid/colors/fullscreen. Holding `?` is safe only by accident (the overlay is modal, so `handle_keys` stops running once it opens).

### Commit `04543f3` verdict

All seven things it set out to fix are verified correct: the dead Ctrl+C path replaced with `Event::Copy`/`Cut` scanning; chord labels centralized; missing standard chords added (Ctrl+N/O/Shift+S/A/D/X, Ctrl+=/−, G, ?, help overlay); `Plugin::tick` now returns `PanelOutcome` and `handle_keys` drains it (without this, gascii-anim's `,`/`.`/Shift+D could never reach the document); Enter-to-confirm in dialogs; the anim space-hold focus-loss reset; the plugin-vs-chord collision assert. **One regression introduced:** the ungated Cut dispatch (H2). Test gap: nothing at app level pins copy/cut/paste behavior *while a widget has focus* — precisely where both High bugs live.

---

## Part 4 — General application health

### Metrics

| Metric | Value |
|---|---|
| Workspace tests | **819 passed, 0 failed** (gascii 336, gascii-core 335 unit + ~97 integration, gascii-anim 51) |
| Clippy (`--workspace --all-targets`, forced rebuild) | **0 warnings** |
| Non-test `unwrap`/`expect`/`panic!`/`unreachable!` | 23 total, all `expect` with invariant messages |
| `#[allow(...)]` | 9 total: 2 `dead_code` (both documented), 7 `too_many_arguments` |
| TODO/FIXME/HACK markers | 0 |
| Largest files | `app.rs` 6,655; `canvas.rs` 1,531; `gascii_json.rs` 1,000; `tools/mod.rs` 898 |

### H5 — `gascii/src/app.rs` is a 6,655-line monolith

Breakdown: **2,878 lines of production code + 3,777 lines of `#[cfg(test)] mod tests`** (57% of the file is its own test module, starting at line 2879). The branch grew it by ~5,570 changed lines. The `GasciiApp` struct (`app.rs:703-874`) has **58 fields**, 13 of them dialog state.

Responsibilities co-resident (with approximate line ranges): pure policy helpers (46-260); binding/slot/stamp types (261-409); tool registry + plugin merge (410-675); plugin host glue (scattered); session/flush/undo machinery (~1177-1563); keyboard dispatch (1564-1768); menu bar (1790-1953); five modal dialogs + export orchestration (~640 lines); file IO + lifecycle (~200 lines); the `eframe::App` impl (2744-2872 — actually well-organized).

Natural decomposition seams, each already having a crisp internal boundary:

- `tools_registry.rs` — `ToolDef`, `build_tools`, plugin-row merge (~415 lines, self-contained)
- `plugin_host.rs` — `HostFacts` + panel/drain plumbing (already has a documented borrow contract)
- `session.rs` — flush/end_session/apply_edit choke points (the doc comments already describe this as one subsystem)
- `ui/menu.rs` + `ui/dialogs/` — one file per dialog, each owning its state as a struct
- `files.rs` — open/save/atomic-write
- **Moving the test module out would halve the file by itself** — the single highest-leverage, lowest-risk cut.

Mitigating: code *quality* inside is high (exhaustive invariant docs, pure functions extracted for testability). This is a navigability/merge-cost problem, not a correctness one — but it is the right moment to pay it down, before the next feature lands on top.

### M11 — Dialog state as N independent bools + a manually-maintained `modal_open()`

`app.rs:1042-1048`: `modal_open()` is a hand-enumerated OR of five flags, and its doc comment warns "every modal flag must be named here" — because `canvas.rs` polls raw pointer state and bypasses egui occlusion, a forgotten flag means clicks land *through* a modal onto the canvas. A regression test exists but must itself be manually extended. Replacing the four bools with a single `Option<OpenDialog>` enum (as `confirm: Option<PendingConfirm>` already models) makes the invariant structural, eliminates the impossible two-dialogs-open state, and shrinks the struct. Low-risk since `open_/close_export_dialog` etc. already mediate access.

### M12 — `canvas::show` is a ~520-line function

`canvas.rs:402-921` sequences fit/refit, zoom, pan, gesture routing, stroke driving, Text/Selection keyboard dispatch (623-695), paste, stylus pressure, focus-loss recovery, then five painting phases. The phases are cleanly ordered, but the keyboard-owner routing (~70-line nested match) and the input-precedence block are separable stages: `route_owner_keys(...)`, `handle_canvas_input(...)`, `paint_canvas(...)`. Note the honest "Known gap" comment at 515-517 (stroke stuck if the OS swallows a mouse-up without a focus change) — the focus-loss handler at 745-760 covers alt-tab, so the residual gap is narrow.

### Low findings

- **`run_export` repeats the dialog→bytes→write→report pattern five times** (`app.rs:2375-2464`), with the three raster arms repeating identical `opaque_bg`/`bg_image` construction. A helper taking `(filter, exts, impl FnOnce() -> Result<Vec<u8>, String>)` collapses ~90 lines to ~40 and makes a sixth format one closure.
- **Frame-header format string duplicated across crates.** `app.rs:92-106` reproduces the `"--- frame {} ({dur}ms) ---"` header and separator of `gascii-core/src/io/export_text.rs:26-35` verbatim. Documented as deliberate, but the literal existing twice means a header change silently desynchronizes trimmed vs untrimmed output. Otherwise the export stack has **no** duplication — core owns validation, `png_export.rs` owns all rasterization through one shared function, `anim_export.rs` streams through it. That layering is a genuine strength.
- **Accepted-risk items to keep on the radar** (all documented in-code with sound reasoning): unbounded undo memory (`edit.rs:133-141`; ~1M `CellEdit`s for a full max-size fill); synchronous export on the UI thread (bounded to a seconds-scale freeze — see M2); animation freezing under modals (deliberate — see M4 for the one real bug it causes).

### Nits

- `app.rs:2746`: `eprintln!("startup to first frame: …")` ships in release builds. Gate on `cfg(debug_assertions)` or remove.
- `slot_mut` (`app.rs:1062`) is `#[allow(dead_code)]` with a comment telling callers not to use it — consider deleting it outright.
- 4 of the 7 `too_many_arguments` allows are in `gascii-anim/src/decorator.rs` — the renderer-decorator signature wants a params struct.

### Core-crate and supporting-file health (strong, briefly)

- `model.rs`: `frames`/`active_frame` stay `pub(crate)` with the mutation-choke-point rule documented; untrusted-input caps are derived, not magic, and shared with the loader.
- `edit.rs`: textbook command-pattern undo/redo; the new frame variants preserved the design rather than bolting on; `top_edit_id` as the dirty-marker is elegant.
- `tools/mod.rs`: the `Tool` trait survived heavy modification coherently; the `resync` contract (240-247) is the load-bearing piece and is well specified; default-no-op hooks serving single implementors are a mild fat-trait smell, each documented.
- `viewport.rs`, `prefs.rs`, `ui/`: all clean. Kiosk pre-empted windowed-vs-kiosk duplication by delegating per-tool options layout to `sidebar::binding_options_geom`.
- Root `Cargo.toml`: workspace dependency table, minimal `image` features, workspace lints, `unsafe_code = "forbid"`, release `overflow-checks = true` with rationale — exemplary hygiene.

---

## Test-coverage gaps worth closing

1. **App-level: clipboard chords while a widget has focus** — precisely where H2/H3 live; nothing pins this today.
2. **`add_frame` with `at > frame_count()`** — the documented "caller bug" behavior isn't pinned; if M3 is fixed, pin the new no-op, else pin the panic with `#[should_panic]`.
3. **Duration overrides under structural ops** — no test that `duplicate_frame` carries `duration_override`, or that `SetFrameDuration` stays correct across an interleaved reorder + undo.
4. **Loader bound on `duration_override`** — a hardening test would force the L1 decision.
5. **Transparent multi-frame GIF** — nothing asserts no frame-to-frame smear with `opaque_bg: None`; today this passes only because `image` hardcodes `DisposalMethod::Background`, which an upgrade could silently change. The suite already pins one `image` behavior this way; this deserves the same treatment.
6. **`reserved_global_keys` vs plugin tick keys** — currently untestable because the data isn't there (M7); the API-level shortcut declaration would make it testable.
7. **AltGr false-positive test** for M6.

---

## Recommended priorities

**Fix before merging this branch** (small, user-facing, two are regressions/data-loss):

1. H2 — gate `copy`/`copy_all`/`cut` on `!widget_focused` (one-line class of fix; add the missing focused-widget clipboard tests).
2. H3 — gate the paste collection in `canvas.rs` the same way.
3. M1 — route the Loop toggle through dirty tracking (or fold it into the save-marker comparison).
4. M5 — stop consuming Escape when it performs no action.

**Fix soon** (correctness/robustness, low urgency):

5. H1 — thumbnail dirty-check before composite+hash; visible-rect culling.
6. M3 — bounds-validate structural edits at the `History::apply` seam.
7. M4 — reset the space-hold state machine across modal suppression.
8. M6 — reject Alt-modified events for COMMAND-only chords.

**Architecture, next quiet moment** (before the next feature lands on `app.rs`):

9. H5 — split `app.rs`: move the test module out first (halves the file, zero risk), then extract the tool registry and dialogs; convert dialog bools to `Option<OpenDialog>` (M11) during the dialog extraction.
10. H4 + M7 + M8 — one coherent plugin-API round: name-keyed stamp persistence (dissolves the three-place tool special-casing), a shortcut-declaration hook (feeds the `?` overlay, `reserved_global_keys`, and the collision check), and revisit `extra_tool_ctx`/`OptionsGeom` shapes. Best done together, and only when a third plugin is actually contemplated.
11. M2 — hoist per-frame fixed work out of the export loop (worth doing whenever export next gets touched; a worker thread can wait).

Everything in Low/Nit is opportunistic — none of it blocks anything.
