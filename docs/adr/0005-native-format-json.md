# Native document format is versioned JSON (`.gascii`)

GASCII saves its own `.gascii` format: JSON with an explicit version field, serialized via serde. Chosen over adopting REXPaint `.xp` as the native format (gzipped binary, and it would cap the model at what .xp supports) and over a bespoke binary format (character grids are tiny; diffability and easy evolution are worth more than compactness). `.xp` remains an export/interop target only.
