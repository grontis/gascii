/// Extra `gascii_core::ToolCtx` fields a plugin-owned tool wants while it is bound — a named,
/// defaultable replacement for a positional `(DensityMode, Vec<char>)` tuple. `canvas::tool_ctx`
/// applies this over the context's own defaults, field by field, so adding a future field is
/// additive (`..Default::default()`) rather than a breaking tuple-shape change.
///
/// Fields stay core-typed deliberately (see `gascii-plugin-api`'s crate-doc policy on `ToolCtx`
/// extras) rather than an opaque `&dyn Any` — every `Tool` impl would otherwise need a downcast for
/// data that already has a concrete, public core type.
#[derive(Default, Clone)]
pub struct ToolCtxPatch {
    pub density: Option<gascii_core::DensityMode>,
    pub ramp: Option<Vec<char>>,
}
