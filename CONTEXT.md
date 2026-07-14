# GASCII

A native ASCII/ANSI art editor: a character-grid canvas drawn on with continuous pointer input, using curated character palettes, per-cell color, and a density-brush system.

## Language

### Canvas & cells

**Document**:
One saved GASCII artwork: fixed canvas dimensions and an ordered stack of Layers.
_Avoid_: File, project

**Layer**:
One full-canvas sheet of Cells within a Document. Layers composite top-down via alpha. v1 documents have exactly one.
_Avoid_: Plane (that's a Cell component), level

**Canvas**:
The 2D grid of Cells that makes up a document's drawing surface, with explicit width×height set per Document (resizable, not growable).
_Avoid_: Grid (reserve for the geometric lattice), page, sheet

**Blank**:
The canonical empty Cell: space glyph with fully transparent background. There is no separate null state — erasing writes Blank; compositing and export trimming test for it.
_Avoid_: Empty, null, void

**Cell**:
One grid position holding a single code point plus foreground and background colors. 1 cell = 1 character = 1 terminal column.
_Avoid_: Pixel, tile, character (that's the glyph it holds)

**Plane**:
A writable component of a Cell that a tool can independently toggle: the glyph (drawn together with its text color — the two are inseparable) or the background. Every tool can choose which planes it writes.
_Avoid_: Channel, layer (a Layer is a full canvas sheet, if/when added)

### Input & tools

**Stroke**:
One continuous pointer gesture from press to release, reduced to a set of (cell, change) edits. The universal primitive all drawing tools produce; also the unit of undo.
_Avoid_: Drag, gesture, path

**Tool**:
A mode that translates pointer/keyboard input into Strokes (pencil, eraser, fill, rectangle, line, text, selection).

**Auto-join**:
Junction resolution for the rectangle/line tools: where strokes cross box-drawing characters, the union of arm directions picks the right glyph (`├ ┬ ┼`).

**Floating stamp**:
Cells lifted by a selection move or produced by paste, hovering above the canvas until dropped and committed as one Stroke.
_Avoid_: Selection buffer, ghost

### Palette & density

**Palette**:
The curated set of characters currently offered for drawing, organized into Pages.
_Avoid_: Charset, character map

**Page**:
One themed group within the Palette (ASCII, Box drawing, Blocks, …). Curation guarantees single-width glyphs.
_Avoid_: Tab, category

**Ramp**:
An ordered character sequence from light to dark (e.g. ` .:-=+*#%@`), indexed by Intensity. A first-class palette object.
_Avoid_: Gradient (reserve for color), scale

**Intensity**:
The 0.0–1.0 parameter of the density brush that selects a character from the active Ramp. Sources are pluggable (fixed, buildup, falloff, speed, pressure).
_Avoid_: Pressure (one possible source, not the parameter), darkness

**Buildup**:
The intensity source where each pass of the brush over a cell advances it one Ramp step, like layering graphite.
