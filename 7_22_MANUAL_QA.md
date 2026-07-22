# Manual QA — Animation-Plugin Program (Consolidated)

*2026-07-22. Consolidates the four per-scope guides from `.agentwork/qa/` (plugin-foundation,
plugin-api, gascii-anim, anim-export) into one hand-testing session. The originals remain in
`.agentwork/qa/` for per-scope context; this document supersedes them for actually running the pass.*

Everything below is what the 745-test automated suite **cannot** verify: visual parity, feel,
timing as perceived by a human, and how exported files behave in real external viewers. All
logic-level behavior (undo/redo exactness, registry/plugin-slot resolution, playback math, GIF
loop/duration encoding, cap rejection, save/load/export round-trips) is automated — if a scenario
here fails, it is almost certainly a real finding, not a known gap.

**Estimated time:** 45–60 minutes for the full pass.

---

## Setup

- [ ] Build and run normally: `cargo run -p gascii` (a release build is closer to real feel: `cargo run -p gascii --release`).
- [ ] Have a real GIF viewer or a modern web browser available (for Part 4).
- [ ] Optional but valuable: a stylus/touch device (kiosk touch targets, Brush pressure).
- [ ] Optional: a build at commit `c0a96a9` (pre-program) for side-by-side comparison — the
      automated suite already proves geometry/state equivalence, so this is a sanity check, not a hunt.

Run the parts in order — later parts build documents the earlier parts create.

---

## Part 1 — Chrome & options parity (foundation refactor)

The refactor's contract was zero visual/behavioral change. Two things only a human can confirm:

### 1.1 Toolbox cell order

| Step | Action | Expected |
|---|---|---|
| 1 | Windowed: look at the left sidebar's toolbox grid. | Pencil, Eraser, Brush / Text, Fill, Rectangle / Line, Selection, Eyedropper. |
| 2 | F11 → kiosk: look at the 4-wide tool grid. | Same relative order, Text absent (by design). |

- [ ] **Pass** — order reads as before (position is cosmetic only; persistence is by name, shortcuts capability-gated).

### 1.2 Brush options panel — windowed vs. kiosk parity

| Step | Action | Expected |
|---|---|---|
| 1 | Windowed: bind L to Brush, open OPTIONS. | Stepper, SHAPE row (no indent), BRUSH block: ramp segments, Fixed/Buildup on its own line, slider below. |
| 2 | Note your size/shape/ramp/mode values. | — |
| 3 | F11 → kiosk: open OPTIONS for the same binding. | Same values exactly; touch-scaled geometry (taller stepper, SHAPE indented, Fixed/Buildup shares the slider row); no missing or duplicated controls. |
| 4 | Change size via the kiosk stepper; exit fullscreen. | Windowed OPTIONS shows the change immediately — one shared state, not per-surface copies. |

- [ ] **Pass** — no missing controls, no clipping/overlap in either mode, state carries across modes.

---

## Part 2 — Brush through the plugin path

Brush's state, options UI, and digit-key shortcut moved into the `gascii-density-brush` plugin.
Contract: identical feel.

### 2.1 Brush end-to-end

| Step | Action | Expected |
|---|---|---|
| 1 | Bind Brush (`B`), pick a ramp, drag a stroke. | Glyphs follow the ramp light→dark, no stutter or glitch. |
| 2 | Press digits `1`–`9`, `0` with Brush bound. | Intensity updates to 10%–100% immediately; next stroke reflects it. |
| 3 | Click into the HEX color field, press a digit. | Digit goes to the field, NOT Brush intensity. |
| 4 | Bind Text to L, start typing, press a digit (Brush on R). | No intensity change — active Text session suppresses it. |
| 5 | *(Stylus only)* Enable "Pressure" in Brush options, vary pressure in a stroke. | Stamp size visibly follows pressure. |

- [ ] **Pass** — identical to pre-migration Brush behavior; flag any stutter or difference even if hard to pin down.

### 2.2 Nothing-else-changed sweep

| Step | Action | Expected |
|---|---|---|
| 1 | Cycle every other tool via shortcut and toolbox. | All behave normally. |
| 2 | Ctrl+S, close, reopen. | Brush binding + size/shape restore; ramp/intensity/pressure do NOT persist (never did). |
| 3 | Bind Brush to BOTH L and R; open OPTIONS. | The Brush block appears exactly once, not duplicated. |

- [ ] **Pass** — no non-Brush regressions; options dedup holds.

---

## Part 3 — Timeline, playback, onion skinning

The animation feature itself. Build up a working document here — Part 4 reuses it.

### 3.1 Single-frame contract (timeline appears/disappears)

| Step | Action | Expected |
|---|---|---|
| 1 | Fresh single-frame document: look at the bottom of the window. | No timeline panel; canvas fills the space exactly as pre-program. Only new affordance: Edit → "Add Frame". |
| 2 | Edit → "Add Frame". | Timeline appears: Play/Pause, Loop, counter ("1/2"), Add/Duplicate/Delete, ◀/▶, ±10ms duration stepper, Onion toggle, thumbnail strip. Canvas shrinks cleanly — no overlap with the status bar. |
| 3 | Delete back down to 1 frame (or undo). | Timeline disappears entirely; canvas reclaims the space. |

- [ ] **Pass** — no flash/glitch on appear/disappear; no overlap ever.

### 3.2 Timeline editing feel

| Step | Action | Expected |
|---|---|---|
| 1 | Draw something distinct on frame 1; click "Duplicate". | New frame right after the active one, thumbnail shows the drawing, counter increments. Note whether Duplicate switches you to the new frame or stays — record which (unpinned by tests). |
| 2 | Click a thumbnail 2–3 frames away. | Canvas switches immediately; clicked thumbnail gets the highlight border. |
| 3 | Reorder the active frame with ◀/▶. | Strip order updates; the counter follows the frame, not the position. |
| 4 | Set different durations on a couple of frames (±10ms). | Label updates live; floors at 10ms (try hammering −10ms). |
| 5 | Ctrl+Z repeatedly through all the above. | One click = one undo step, exact reverse order, no batching or skips. |

- [ ] **Pass** — first-click response on every control, clean undo granularity, no thumbnail corruption.

### 3.3 Playback feel and timing

Build 4–5 frames with visibly different content and durations (e.g. 100/300/100/500ms).

| Step | Action | Expected |
|---|---|---|
| 1 | Click Play. | Frames advance at their own speeds — the 500ms frame visibly holds longest. No stutter. |
| 2 | Loop checked: play past the end. | Wraps to frame 1 smoothly, keeps going. |
| 3 | Loop unchecked: reach the end. | Stops on the last frame; button reads "Play" again. |
| 4 | While playing: Pause, then click another thumbnail. | Canvas jumps there instantly — normal cursor navigation, independent of playback (by design). |
| 5 | While playing: minimize/restore or alt-tab away and back. | Resumes without a jarring multi-frame catch-up skip (a narrow accepted edge — flag if it's worse than "minor"). |
| 6 | While playing: open any modal (New, Resize, Export). | Playback freezes while the modal is open; resumes smoothly on close. |
| 7 | While playing: draw a stroke. | Canvas shows the *playing* frames while you draw (expected); the stroke commits to the frame you're *editing* — pause and verify the content landed there. |
| 8 | Ctrl+S during playback. | Saves normally, playback uninterrupted. |

- [ ] **Pass** — timing visibly respects durations; loop/stop as configured; no freeze-that-stays; drawing mid-playback commits correctly.

### 3.4 Onion skinning

| Step | Action | Expected |
|---|---|---|
| 1 | 3+ distinct frames, enable Onion at default depth. | Prev frame tinted red-ish, next tinted green-ish, both under the active frame's own content. |
| 2 | Step prev/next depth up past 8. | Stops at 8; the stepper visually disables and **genuinely stops responding** — ⚠ this specific control has zero automated coverage; confirm it explicitly. |
| 3 | Go to frame 1, then the last frame, onion on. | Fewer tints near edges, no glitch, no error. |
| 4 | Onion on → click Play. | Tint vanishes during playback, returns when paused. |

- [ ] **Pass** — tints distinguishable from real content and each other; the depth-8 cap actually stops responding.

### 3.5 Kiosk parity

| Step | Action | Expected |
|---|---|---|
| 1 | 2+ frames, F11. | Timeline re-renders at touch geometry — same full control set, nothing missing. |
| 2 | Exercise every timeline control in kiosk. | Identical behavior to windowed, just larger. |
| 3 | *(Touch device)* Tap each control directly. | First-tap registration on every target. |
| 4 | F11 back to windowed. | Active frame, Play/Loop/Onion state all carry over — no reset. |

- [ ] **Pass** — full capability parity; comfortable touch targets.

### 3.6 Thumbnail fidelity

| Step | Action | Expected |
|---|---|---|
| 1 | 4–5 frames with different dominant colors. | Thumbnails distinguishable at a glance without clicking through. |
| 2 | Edit a frame; look at its thumbnail without switching. | Updates promptly, no manual refresh needed. |
| 3 | Resize the document large (e.g. 200×100); recheck. | Thumbnails keep their fixed on-screen size — no distortion or strip overflow. |

- [ ] **Pass** — useful at a glance, prompt updates, size-stable.

---

## Part 4 — Animation export

Reuse Part 3's document (3–5 distinct, colorful frames with varied durations).

### 4.1 Export dialog format gating (single-frame contract)

| Step | Action | Expected |
|---|---|---|
| 1 | Single-frame doc → Ctrl+Shift+E. | Exactly two formats: Text, PNG — identical to pre-program. |
| 2 | Add a frame; reopen Export. | Five formats: Text, PNG, Animated GIF, PNG Spritesheet, Text Frames. |
| 3 | Delete back to 1 frame; reopen. | Two formats again; if an animation format was selected, the selection snapped to Text (never blank/broken). |

- [ ] **Pass** — no leftover formats, no broken selection.

### 4.2 Animated GIF in a real viewer

| Step | Action | Expected |
|---|---|---|
| 1 | Distinct durations set (e.g. 100/500/100/300ms), Loop checked. Export → Animated GIF, Scale 1x, transparent bg. | File written, no error. |
| 2 | Open the `.gif` in a browser/viewer. | Animates all frames in order, loops continuously, the 500ms frame visibly holds longer — timing looks distinct, not uniform. |
| 3 | Re-export with Loop unchecked. | Plays through ONCE and stays on the last frame — watch it reach the end and stop. |
| 4 | Try Scale 2x/4x. | Crisper output; dialog readout `{w}×{h} px · {scale}×` matches; only expected GIF quantization artifacts. |
| 5 | Uncheck "Transparent background". | Document background color fills the GIF instead of transparency. |

- [ ] **Pass** — real-viewer animation, timing, and loop behavior all correct (the one thing tests can't see).

### 4.3 PNG spritesheet usability

| Step | Action | Expected |
|---|---|---|
| 1 | Export → PNG Spritesheet; open in an image viewer. | Roughly-square grid (4 frames → 2×2; 5 → 3×2 with one empty tile), reading order left-to-right top-to-bottom, no cropping/offset within tiles. |
| 2 | Compare tiles against timeline thumbnails. | Tile order = frame order exactly (frame 1 top-left). |
| 3 | Try a 2-frame document. | A 2×1 side-by-side strip — sensible, not surprising. |

- [ ] **Pass** — a human (or sprite slicer) could find "frame N" without guessing.

### 4.4 Text Frames readability

| Step | Action | Expected |
|---|---|---|
| 1 | Export → Text Frames; open in a text editor. | One file; each frame preceded by `--- frame N (Dms) ---`, blank line between frames. |
| 2 | Toggle "Trim trailing spaces" off, re-export, diff. | Untrimmed pads rows to full width; content otherwise identical. |
| 3 | Check header durations. | Match the timeline's values exactly. |

- [ ] **Pass** — readable, unambiguous boundaries, accurate headers.

### 4.5 Dialog UX with five formats

| Step | Action | Expected |
|---|---|---|
| 1 | Click through all five formats. | Each format's own option set appears (Scale/Transparent/Bg-image for the raster three; Trim for the text two); no lingering controls from the previous format. |
| 2 | Watch the preview across Png/Gif/SpriteSheet. | Shows the active frame's static raster for all three — deliberate simplification, should read as intentional. |
| 3 | Text Frames preview. | Frame 1's first lines, not blank. |
| 4 | Narrow the window with the dialog open. | Five-segment control doesn't overflow or clip. |

- [ ] **Pass** — nothing cramped or confusing; labels self-explanatory.

### 4.6 Encode-time feel and the cap

| Step | Action | Expected |
|---|---|---|
| 1 | Large doc (200×100+), 5–10 frames, GIF at 2x–4x. | A brief synchronous freeze is expected and accepted; it resolves in a few seconds — never feels stuck. |
| 2 | Push past the cap (more frames / higher scale until rejected). | Clear error text in the dialog — not silence, not a crash, not a hang. |

- [ ] **Pass** — bounded latency, communicated rejection.

---

## Exploratory (untargeted, all parts)

- Undo/redo "feel" anywhere in the app — the timeline is the first plugin surface that mutates the
  document; anything off is worth flagging even without a matching scenario.
- Rapid-click Add/Duplicate near the 256-frame ceiling → error appears in the status bar, buttons
  never silently no-op.
- Very narrow window with the timeline open → graceful degradation (scrollable strip), no overlap.
- Export dialog: load a background image, export GIF/spritesheet → background composites into
  *every* frame. Export all three formats back-to-back without closing → no stale state between switches.
- With the Export dialog open, try to change frame count without closing it → should be unreachable
  (modal gates the shortcuts); confirm genuinely unreachable rather than reachable-but-broken.
- Import a GASCII GIF/spritesheet into another tool if available — real interop beyond "opens in a viewer."

## Known / accepted (do not file)

- Onion-skin and playback glyphs render in a generic monospace, not Iosevka Fixed — known, tracked.
- Playback stopping snaps the canvas to the editing cursor's frame — deliberate.
- Playback freezes under modals and resumes without skipping — deliberate.
- GIF 256-color quantization — inherent format limit.
- No mouse drag-to-reorder in the thumbnail strip (◀/▶ only), no fps-override or spritesheet column
  controls, no animated dialog preview, no `.dur` export — all deliberately out of scope this program.

---

## Sign-off

| Part | Result | Notes |
|---|---|---|
| 1 — Chrome & options parity | ☐ pass / ☐ fail | |
| 2 — Brush via plugin | ☐ pass / ☐ fail | |
| 3 — Timeline / playback / onion | ☐ pass / ☐ fail | |
| 4 — Export | ☐ pass / ☐ fail | |

Findings go to the usual place — describe what you saw and which step; the per-scope guides in
`.agentwork/qa/` map each scenario back to its QA report if deeper context is needed.
