# 8-3-2026 todos

~~selection tool: we should implement a modify color feature. When an area is selected, we should provide an option for updating the color of it. 
My UX intuition points me to the idea that this should be an explicit button, rather than just changing the selected colors, as that could be bad if the user did not intend to change the color of the selection.~~
DONE 8-3: "Recolor Selection" button in the sidebar's COLORS section (windowed + kiosk), enabled while a selection exists — one undoable edit. Glyph text color follows the FG well; backgrounds follow the BG well when it holds a color (a transparent BG well leaves backgrounds untouched rather than wiping them). Blank cells are never filled.

~~selection tool: selecting and then pressing ctrl + d duplicate selection and leave it selected and moveable, leaving the previously selected in place~~
DONE 8-3: Ctrl+D / Edit ▸ Duplicate Selection — copy lands as a moveable float one cell down-right, original untouched. Deselect moved to Escape (menu item hint updated).

~~The layers feature does not work as inteneded. If we create a second layer, the characters should be rendered on top of the characters of the bottom layer (showing both). 
Current implementation just replaces characters in the bottom layer. This essentially makes the currently implemented layer feature useless.~~
DONE 8-3: rendering switched to the stacked "acetate" model — each visible layer paints as its own complete image, bottom to top (canvas, animation playback, PNG export). Glyph ink from different layers now overlaps visibly in the same cell; only an opaque background blocks out what's beneath. Text export still flattens to one character per cell (top glyph wins) since .txt can't hold overlapping glyphs.

~~When a layer is hidden and we try to draw on it, we display an error message in the bottom, but this message never dissappears once it shows. We should display it for a set number of seconds (3 seconds default)~~
DONE 8-3: status-bar errors now expire 3s after being raised (all error sites, not just the hidden-layer one). Dialog-inline validation messages intentionally do not expire.

~~For the animation section, we only slightly enlarge the selected frame as UX for signaling which frame is being worked on. I say we also give a border to the selected.~~
DONE 8-4: (Note: no enlargement actually existed before — the old marker was just a slightly thicker gray stroke.) The selected frame's thumb now paints enlarged (+2px each side) with a 2px high-contrast border (the theme's inversion color — near-white in dark mode, near-black in light). Layout/scrolling geometry is unchanged; only the paint pops.

~~Also for animation, for the frames, maybe we display a brief preview rather than just blank square~~
DONE 8-5: thumbnails now show the actual art. Each cell's preview color blends the glyph's text color over the background by an ink-coverage weight (█▓▒░ get their real densities, punctuation reads faint, other characters a middle weight) — so glyph-only drawings appear as a tonal preview instead of a blank square. Thumbnail resolution doubled to 96×60 (matches the fullscreen thumb size exactly; windowed thumbs downscale smoothly).

~~For fullscreen mode, we remove the Text tool icon from the tool selection, we should leave it in (supported if keyboard connected)~~
DONE 8-5: Text now has a cell in the fullscreen tool grid, and the `T` shortcut works while fullscreen too — both were driven by the same registry flag, so they came back together. Binding badges show on the Text cell like any other tool. Follow-up per feedback: Eyedropper removed from the fullscreen grid instead, keeping it a tidy 4×2 (Alt+click's temporary sample still picks colors there without a tool switch; `I` is gated while fullscreen accordingly).

~~when duplicating a frame or adding  a new one, we should automatically switch to that frame being the frame selected.~~
DONE 8-4: adding or duplicating a frame now selects it, from every entry point (timeline Add/Duplicate buttons, Shift+D, Animation ▸ Add Frame). Implemented in core so undo also returns the cursor to the frame it left — same rule layers already used.

if mouse is last active in the animation section, we should also support ctrl+d, delete key, copy paste keys.

We should implement drag and reorder for frames in the animation section

~~For animation frame duration, we allow for clicking +- 10ms, but we should also support typing in a specific value.~~
DONE 8-5: both duration readouts (the active frame's and the DEFAULT) are now editable text fields between their ±10ms steppers. Type a value and press Enter (or click away) to commit; Escape cancels; values clamp to the same 10ms–max range the steppers use; non-numeric text just reverts. Typing into the per-frame field sets that frame's override, same as the steppers. Layout refined per feedback: the two clusters are labeled FRAME and DEFAULT with separators between them, and the override-clear "×" is now a labeled Reset button.

~~For animation section: Rather than a single button for play/pause, show classic pause, play, stop(resets to beginning), forward/backward(moves by one frame?)~~
DONE 8-5: full transport — ◀ (step back) / Play / Pause / Stop / ▶ (step forward), each enabled only when meaningful. Pause freezes on the frame playback was showing and moves the editing cursor there (Space-tap pause matches). Stop halts and rewinds to frame 1. Step buttons move the cursor one frame while not playing. The old ◀/▶ reorder buttons are relabeled "Move ◀"/"Move ▶" to avoid clashing with the step arrows, and the control bar is now two rows (transport + frame ops, then timing + onion). Follow-up 8-5: editing is disabled while playing — canvas strokes refuse with a status-bar message ("Playback is running — pause to edit"), undo/redo/cut/paste/duplicate/recolor/clear/resize and all timeline frame/timing controls are blocked or disabled, and the `,`/`.`/Shift+D shortcuts idle. Eyedropper sampling stays allowed (read-only).

The onion feature doesn't feel like a useful feature or good UX, should remove this.

~~(8-5 follow-up) design a way to give UI/UX for what frame is currently on the screen when playing — maybe set the currently played as selected~~
DONE 8-5: while playing, the frame strip's selection marker rides the playback frame (the frame actually on screen), the counter shows "▶ n/N", and the strip auto-scrolls to keep the playing frame in view. Clicking a thumb during playback scrubs playback to that frame (instead of invisibly moving the editing cursor). On Pause the editing cursor parks on the shown frame, so marker and real selection converge.

~~(8-5 follow-up) the animation pane should be toggleable to open easier — currently the only way to open it is Animation ▸ Add Frame~~
DONE 8-5 (reworked per feedback — toggle lives where the panel pops up, not in the menu; pane labeled ANIMATION): a slim "▲ ANIMATION" bar sits at the bottom edge whenever the pane is hidden; clicking it opens the full panel, and a "▼" button in the panel header collapses it back. Works at any frame count (hiding a multi-frame timeline is fine — the bar is always one click from reopening). Auto behavior unchanged until you choose: fresh single-frame documents start collapsed, multi-frame documents start open. Add Frame stays in the menu as a second path.