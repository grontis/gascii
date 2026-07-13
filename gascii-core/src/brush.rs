//! Ramp: an ordered light→dark character sequence, plus the built-in ramps and the density-brush
//! intensity engine that consumes them.

#[derive(Clone, Debug)]
pub struct Ramp {
    pub name: &'static str,
    pub chars: Vec<char>,
}

pub fn builtin_ramps() -> Vec<Ramp> {
    vec![
        Ramp { name: "ASCII shading", chars: " .:-=+*#%@".chars().collect() },
        Ramp { name: "Block shades", chars: "░▒▓█".chars().collect() },
    ]
}

/// What an `IntensitySource` needs to compute a sample: where the stroke is, how long it's been
/// running, and the target cell's current position on the active ramp (`None` if the cell's glyph
/// isn't one of the ramp's characters).
#[derive(Clone, Copy, Debug)]
pub struct StrokeSample {
    pub position: (u16, u16),
    /// Seconds since the stroke began. `Fixed`/`Buildup` ignore it; it exists so the sample
    /// carries everything a time-sensitive source would need without changing this struct.
    pub timing: f32,
    pub current_ramp_index: Option<usize>,
    pub ramp_len: usize,
}

/// Pluggable intensity engine behind the density brush. The trait is shaped so a new source can
/// plug in without anything else depending on which source is active — in particular, nothing
/// outside a source may ever read pointer pressure.
pub trait IntensitySource {
    fn sample(&mut self, ctx: &StrokeSample) -> f32;
}

/// A constant, user-set intensity (slider + number-key shortcuts). Ignores `ctx` entirely.
#[derive(Clone, Copy, Debug)]
pub struct Fixed(pub f32);
impl IntensitySource for Fixed {
    fn sample(&mut self, _ctx: &StrokeSample) -> f32 {
        self.0.clamp(0.0, 1.0)
    }
}

/// Each pass over a cell advances it one ramp step from wherever it currently sits. A cell's
/// current ramp position is its glyph's index in the active ramp; a glyph that isn't one of the
/// ramp's characters has no current index, so its first pass lands on the ramp's own lightest step
/// (index 0). Blank (space) is not special-cased: if the active ramp's own first character happens
/// to be a space (the built-in "ASCII shading" ramp is), a Blank cell is already on-ramp at index
/// 0 and its first pass advances to index 1, same as any other on-ramp cell; only for a ramp with
/// no space character is Blank off-ramp and starts at index 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct Buildup;
impl IntensitySource for Buildup {
    fn sample(&mut self, ctx: &StrokeSample) -> f32 {
        if ctx.ramp_len <= 1 {
            return 1.0;
        }
        let next = ctx.current_ramp_index.map_or(0, |i| (i + 1).min(ctx.ramp_len - 1));
        next as f32 / (ctx.ramp_len - 1) as f32
    }
}

/// The brush's active intensity source, carried per-frame through `ToolCtx`. Both variants are
/// stateless (pure functions of `StrokeSample`), so this stays `Copy` — a future stateful source
/// would need a different carrier, noted here rather than designed speculatively.
#[derive(Clone, Copy, Debug)]
pub enum DensityMode {
    Fixed(Fixed),
    Buildup(Buildup),
}

/// Maps a 0.0..=1.0 intensity to a ramp index: nearest, clamped into range. `ramp_len == 0`
/// returns 0 so callers never need to special-case an empty ramp.
pub fn intensity_to_index(intensity: f32, ramp_len: usize) -> usize {
    if ramp_len == 0 {
        return 0;
    }
    ((intensity.clamp(0.0, 1.0) * (ramp_len - 1) as f32).round() as usize).min(ramp_len - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::validate_width;

    #[test]
    fn every_ramp_char_passes_validate_width() {
        for ramp in builtin_ramps() {
            for &ch in &ramp.chars {
                assert!(
                    validate_width(ch).is_ok(),
                    "ramp {:?} contains an invalid-width char: {ch:?}",
                    ramp.name
                );
            }
        }
    }

    #[test]
    fn ramps_are_non_empty_and_ordered_as_specified() {
        let ramps = builtin_ramps();
        let ascii = ramps.iter().find(|r| r.name == "ASCII shading").unwrap();
        assert_eq!(ascii.chars, " .:-=+*#%@".chars().collect::<Vec<char>>());

        let blocks = ramps.iter().find(|r| r.name == "Block shades").unwrap();
        assert_eq!(blocks.chars, "░▒▓█".chars().collect::<Vec<char>>());

        for ramp in &ramps {
            assert!(!ramp.chars.is_empty());
        }
    }

    fn sample(current_ramp_index: Option<usize>, ramp_len: usize) -> StrokeSample {
        StrokeSample { position: (0, 0), timing: 0.0, current_ramp_index, ramp_len }
    }

    #[test]
    fn fixed_ignores_varying_stroke_sample_fields() {
        let mut fixed = Fixed(0.5);
        assert_eq!(fixed.sample(&sample(None, 10)), 0.5);
        assert_eq!(fixed.sample(&sample(Some(3), 10)), 0.5);
        assert_eq!(fixed.sample(&sample(Some(9), 1)), 0.5);
    }

    #[test]
    fn fixed_clamps_out_of_range_levels() {
        assert_eq!(Fixed(1.5).sample(&sample(None, 10)), 1.0);
        assert_eq!(Fixed(-0.5).sample(&sample(None, 10)), 0.0);
    }

    #[test]
    fn buildup_steps_from_off_ramp_through_clamped_top() {
        let mut buildup = Buildup;
        let ramp_len = 5;
        // Off-ramp (no current index): lands on step 0.
        assert_eq!(intensity_to_index(buildup.sample(&sample(None, ramp_len)), ramp_len), 0);
        // Each pass advances exactly one step.
        assert_eq!(intensity_to_index(buildup.sample(&sample(Some(0), ramp_len)), ramp_len), 1);
        assert_eq!(intensity_to_index(buildup.sample(&sample(Some(1), ramp_len)), ramp_len), 2);
        assert_eq!(intensity_to_index(buildup.sample(&sample(Some(3), ramp_len)), ramp_len), 4);
        // Clamps at the top rather than wrapping or overflowing.
        assert_eq!(intensity_to_index(buildup.sample(&sample(Some(4), ramp_len)), ramp_len), 4);
        assert_eq!(intensity_to_index(buildup.sample(&sample(Some(99), ramp_len)), ramp_len), 4);
    }

    #[test]
    fn buildup_degenerates_safely_for_zero_or_one_length_ramps() {
        let mut buildup = Buildup;
        assert_eq!(buildup.sample(&sample(None, 1)), 1.0);
        assert_eq!(buildup.sample(&sample(Some(0), 1)), 1.0);
        assert_eq!(buildup.sample(&sample(None, 0)), 1.0);
    }

    #[test]
    fn intensity_to_index_boundary_rounding() {
        assert_eq!(intensity_to_index(0.0, 10), 0);
        assert_eq!(intensity_to_index(1.0, 10), 9);
        // Exact half-step rounds to nearest (banker's-rounding-free: f32::round rounds half away
        // from zero, so 4.5 -> 5).
        assert_eq!(intensity_to_index(0.5, 10), 5);
    }

    #[test]
    fn intensity_to_index_zero_or_one_length_ramp_does_not_panic() {
        assert_eq!(intensity_to_index(0.5, 0), 0);
        assert_eq!(intensity_to_index(0.0, 1), 0);
        assert_eq!(intensity_to_index(1.0, 1), 0);
    }

    #[test]
    fn intensity_to_index_clamps_out_of_range_intensity() {
        assert_eq!(intensity_to_index(-1.0, 10), 0);
        assert_eq!(intensity_to_index(2.0, 10), 9);
    }
}
