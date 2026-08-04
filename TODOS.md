# 8-3-2026 todos

selection tool: we should implement a modify color feature. When an area is selected, we should provide an option for updating the color of it. 
My UX intuition points me to the idea that this should be an explicit button, rather than just changing the selected colors, as that could be bad if the user did not intend to change the color of the selection.

selection tool: selecting and then pressing ctrl + d duplicate selection and leave it selected and moveable, leaving the previously selected in place

~~The layers feature does not work as inteneded. If we create a second layer, the characters should be rendered on top of the characters of the bottom layer (showing both). 
Current implementation just replaces characters in the bottom layer. This essentially makes the currently implemented layer feature useless.~~
DONE 8-3: rendering switched to the stacked "acetate" model — each visible layer paints as its own complete image, bottom to top (canvas, animation playback, PNG export). Glyph ink from different layers now overlaps visibly in the same cell; only an opaque background blocks out what's beneath. Text export still flattens to one character per cell (top glyph wins) since .txt can't hold overlapping glyphs.

~~When a layer is hidden and we try to draw on it, we display an error message in the bottom, but this message never dissappears once it shows. We should display it for a set number of seconds (3 seconds default)~~
DONE 8-3: status-bar errors now expire 3s after being raised (all error sites, not just the hidden-layer one). Dialog-inline validation messages intentionally do not expire.

For the animation section, we only slightly enlarge the selected frame as UX for signaling which frame is being worked on. I say we also give a border to the selected.

Also for animation, for the frames, maybe we display a brief preview rather than just blank square

For fullscreen mode, we remove the Text tool icon from the tool selection, we should leave it in (supported if keyboard connected)

when duplicating a frame or adding  a new one, we should automatically switch to that frame being the frame selected.

if mouse is last active in the animation section, we should also support ctrl+d, delete key, copy paste keys.

We should implement drag and reorder for frames in the animation section

For animation frame duration, we allow for clicking +- 10ms, but we should also support typing in a specific value.

For animation section: Rather than a single button for play/pause, show classic pause, play, stop(resets to beginning), forward/backward(moves by one frame?)

The onion feature doesn't feel like a useful feature or good UX, should remove this.