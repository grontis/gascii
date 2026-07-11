# Layers live in the data model from day one; UI deferred

A Document is an ordered stack of Layers even though v1 always has exactly one and ships no layer UI. Same insurance as per-cell color: the save format, undo system, and renderer are layer-aware from the start, so adding a layer panel later is a UI feature, not a model migration. A future reader seeing `Vec<Layer>` with a single element should not "simplify" it away.
