# Bundled fonts

| File | Role | Licence |
| --- | --- | --- |
| `IosevkaFixed-Regular.ttf` | Canvas cells. Needs full box-drawing + block-element coverage. | `LICENSE-Iosevka.txt` |
| `InstrumentSans-{Regular,Medium,SemiBold}.ttf` | UI chrome — labels and controls. | `OFL-InstrumentSans.txt` |
| `FragmentMono-Regular.ttf` | Content and measurement — glyphs, coordinates, sizes, hex values. | `OFL-FragmentMono.txt` |

## Regenerating the Instrument Sans cuts

Instrument Sans is published only as a variable font. egui rasterizes via `ab_glyph`, which renders
a variable font's default instance with no axis selection and no synthetic bold — so the three
weights the design uses have to be baked into three static files.

```sh
pip install fonttools
curl -sLO "https://raw.githubusercontent.com/google/fonts/main/ofl/instrumentsans/InstrumentSans%5Bwdth%2Cwght%5D.ttf"

for w in 400:Regular 500:Medium 600:SemiBold; do
  python -m fontTools.varLib.instancer "InstrumentSans[wdth,wght].ttf" \
    "wght=${w%%:*}" wdth=100 -o "InstrumentSans-${w##*:}.ttf"
done
```

The variable source is not checked in — these three cuts are the artefact. Verify a regeneration
with `cargo test -p gascii font_coverage`, which pins both the weights and the glyph coverage the
chrome relies on.
