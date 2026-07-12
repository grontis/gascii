//! Box-drawing junction resolution: a stroke crossing existing box-drawing characters is resolved
//! by unioning which cardinal "arms" each glyph extends into, then mapping the union back to a
//! single glyph (or, in strict-ASCII documents, an ASCII `+ - |` fallback with the same union
//! logic). Pure and dependency-free — no `Document` types involved.

/// Bitset of the four cardinal directions a box-drawing glyph extends a line into.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ArmSet(u8);

impl ArmSet {
    pub const EMPTY: ArmSet = ArmSet(0);
    pub const N: ArmSet = ArmSet(1);
    pub const S: ArmSet = ArmSet(2);
    pub const E: ArmSet = ArmSet(4);
    pub const W: ArmSet = ArmSet(8);

    pub fn union(self, other: ArmSet) -> ArmSet {
        ArmSet(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn arm_count(self) -> u32 {
        self.0.count_ones()
    }
}

/// Arm directions of a single-line box-drawing glyph; `None` for any non-box character (space,
/// letters, block/shade glyphs, …).
pub fn arms_of(ch: char) -> Option<ArmSet> {
    Some(match ch {
        '─' => ArmSet::E.union(ArmSet::W),
        '│' => ArmSet::N.union(ArmSet::S),
        '┌' => ArmSet::S.union(ArmSet::E),
        '┐' => ArmSet::S.union(ArmSet::W),
        '└' => ArmSet::N.union(ArmSet::E),
        '┘' => ArmSet::N.union(ArmSet::W),
        '├' => ArmSet::N.union(ArmSet::S).union(ArmSet::E),
        '┤' => ArmSet::N.union(ArmSet::S).union(ArmSet::W),
        '┬' => ArmSet::S.union(ArmSet::E).union(ArmSet::W),
        '┴' => ArmSet::N.union(ArmSet::E).union(ArmSet::W),
        '┼' => ArmSet::N.union(ArmSet::S).union(ArmSet::E).union(ArmSet::W),
        _ => return None,
    })
}

/// Glyph for an arm set: the single-line box-drawing glyph, or (when `strict_ascii`) the ASCII
/// fallback — any junction of two or more arms other than a straight run becomes `+`, a pure
/// horizontal run (`E|W`) becomes `-`, a pure vertical run (`N|S`) becomes `|`. `None` for the
/// empty set or a lone arm: neither table has a glyph for a dangling single direction.
pub fn char_of(arms: ArmSet, strict_ascii: bool) -> Option<char> {
    let horizontal = ArmSet::E.union(ArmSet::W);
    let vertical = ArmSet::N.union(ArmSet::S);
    if strict_ascii {
        if arms.is_empty() || arms.arm_count() < 2 {
            return None;
        }
        return Some(if arms == horizontal {
            '-'
        } else if arms == vertical {
            '|'
        } else {
            '+'
        });
    }
    Some(match arms {
        a if a == horizontal => '─',
        a if a == vertical => '│',
        a if a == ArmSet::S.union(ArmSet::E) => '┌',
        a if a == ArmSet::S.union(ArmSet::W) => '┐',
        a if a == ArmSet::N.union(ArmSet::E) => '└',
        a if a == ArmSet::N.union(ArmSet::W) => '┘',
        a if a == vertical.union(ArmSet::E) => '├',
        a if a == vertical.union(ArmSet::W) => '┤',
        a if a == horizontal.union(ArmSet::S) => '┬',
        a if a == horizontal.union(ArmSet::N) => '┴',
        a if a == horizontal.union(vertical) => '┼',
        _ => return None, // EMPTY or a lone arm — no glyph in either table
    })
}

/// Unions `incoming` with whatever box arms `existing` already has — a non-box `existing` (space,
/// letter, block glyph) contributes no arms and is simply overwritten, not joined — then resolves
/// the union to a glyph. Falls back to `default_ch` when the union has no glyph (the empty or
/// 1-arm sets, which the rectangle/line tools never actually propose since they only ever union in
/// 2+-arm sets, but this keeps the function total for any caller).
pub fn join(existing: char, incoming: ArmSet, strict_ascii: bool, default_ch: char) -> char {
    let base = arms_of(existing).unwrap_or(ArmSet::EMPTY);
    char_of(base.union(incoming), strict_ascii).unwrap_or(default_ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX_GLYPHS: [(char, ArmSet); 11] = [
        ('─', ArmSet(4 | 8)),
        ('│', ArmSet(1 | 2)),
        ('┌', ArmSet(2 | 4)),
        ('┐', ArmSet(2 | 8)),
        ('└', ArmSet(1 | 4)),
        ('┘', ArmSet(1 | 8)),
        ('├', ArmSet(1 | 2 | 4)),
        ('┤', ArmSet(1 | 2 | 8)),
        ('┬', ArmSet(2 | 4 | 8)),
        ('┴', ArmSet(1 | 4 | 8)),
        ('┼', ArmSet(1 | 2 | 4 | 8)),
    ];

    #[test]
    fn every_box_glyph_round_trips_through_arms_of_and_char_of() {
        for &(ch, arms) in &BOX_GLYPHS {
            assert_eq!(arms_of(ch), Some(arms), "arms_of({ch:?}) mismatch");
            assert_eq!(char_of(arms, false), Some(ch), "char_of({arms:?}) mismatch");
        }
    }

    #[test]
    fn exhaustive_16_arm_combinations_map_to_the_expected_box_glyph_or_none() {
        for bits in 0u8..16 {
            let arms = ArmSet(bits);
            let expected = BOX_GLYPHS.iter().find(|&&(_, a)| a == arms).map(|&(ch, _)| ch);
            assert_eq!(char_of(arms, false), expected, "mismatch for bits={bits:04b}");
        }
    }

    #[test]
    fn exhaustive_16_arm_combinations_map_to_the_expected_ascii_fallback() {
        for bits in 0u8..16 {
            let arms = ArmSet(bits);
            let expected = match bits {
                0b1100 => Some('-'), // E|W
                0b0011 => Some('|'), // N|S
                0 | 0b0001 | 0b0010 | 0b0100 | 0b1000 => None, // empty or lone arm
                _ => Some('+'),
            };
            assert_eq!(char_of(arms, true), expected, "ascii mismatch for bits={bits:04b}");
        }
    }

    #[test]
    fn arms_of_returns_none_for_non_box_characters() {
        for ch in ['x', ' ', '#', '░', '@'] {
            assert_eq!(arms_of(ch), None, "{ch:?} must not be treated as a box glyph");
        }
    }

    #[test]
    fn char_of_returns_none_for_empty_and_lone_arm_sets() {
        assert_eq!(char_of(ArmSet::EMPTY, false), None);
        for arm in [ArmSet::N, ArmSet::S, ArmSet::E, ArmSet::W] {
            assert_eq!(char_of(arm, false), None, "{arm:?} must have no box glyph");
            assert_eq!(char_of(arm, true), None, "{arm:?} must have no ascii glyph");
        }
    }

    #[test]
    fn union_of_horizontal_and_vertical_crosses_to_a_full_junction() {
        let union = ArmSet::E.union(ArmSet::W).union(ArmSet::N).union(ArmSet::S);
        assert_eq!(char_of(union, false), Some('┼'));
        assert_eq!(char_of(union, true), Some('+'));
    }

    #[test]
    fn join_a_horizontal_run_crossing_an_existing_vertical_run_makes_a_cross() {
        // existing '│' (N|S), incoming E|W (a horizontal rectangle border crossing it)
        let ch = join('│', ArmSet::E.union(ArmSet::W), false, '#');
        assert_eq!(ch, '┼');
    }

    #[test]
    fn join_completing_a_tee_into_a_cross() {
        // existing '┤' (N|S|W), incoming E completes all four arms.
        let ch = join('┤', ArmSet::E, false, '#');
        assert_eq!(ch, '┼');
    }

    #[test]
    fn join_over_a_non_box_glyph_overwrites_it_using_only_the_incoming_arms() {
        let ch = join('x', ArmSet::E.union(ArmSet::W), false, '#');
        assert_eq!(ch, '─', "a non-box existing glyph contributes no arms of its own");
    }

    #[test]
    fn join_corner_unions_produce_the_expected_glyphs() {
        assert_eq!(join(' ', ArmSet::S.union(ArmSet::E), false, '#'), '┌');
        assert_eq!(join(' ', ArmSet::S.union(ArmSet::W), false, '#'), '┐');
        assert_eq!(join(' ', ArmSet::N.union(ArmSet::E), false, '#'), '└');
        assert_eq!(join(' ', ArmSet::N.union(ArmSet::W), false, '#'), '┘');
    }

    #[test]
    fn join_falls_back_to_default_ch_when_the_union_has_no_glyph() {
        // A lone incoming arm over a non-box existing glyph unions to a 1-arm set, which has no
        // glyph in either table — join must fall back rather than panic or silently drop the cell.
        let ch = join(' ', ArmSet::N, false, '#');
        assert_eq!(ch, '#');
        let ch = join(' ', ArmSet::N, true, '#');
        assert_eq!(ch, '#');
    }

    #[test]
    fn strict_ascii_fallback_table_matches_the_spec_examples() {
        // existing '│' (N|S, a real box glyph arms_of recognizes) crossed by an incoming
        // horizontal run unions to all four arms, which the ascii table maps to '+'.
        assert_eq!(join('│', ArmSet::E.union(ArmSet::W), true, '#'), '+');
        assert_eq!(join(' ', ArmSet::E.union(ArmSet::W), true, '#'), '-');
        assert_eq!(join(' ', ArmSet::N.union(ArmSet::S), true, '#'), '|');
    }

    /// ASCII fallback characters (`+ - |`) are not themselves recognized as existing box arms —
    /// `arms_of` only covers the 11 Unicode single-line glyphs (matching its documented contract).
    /// Crossing a previously ASCII-drawn junction therefore overwrites it with only the incoming
    /// arms' resolved glyph, rather than truly unioning with what it visually represents. This is
    /// a deliberate, narrow scope boundary: the lossy `+` encoding (any 2+-arm junction) has no
    /// single well-defined arm set to recover from the character alone.
    #[test]
    fn ascii_fallback_characters_are_not_recognized_as_existing_box_arms() {
        assert_eq!(arms_of('+'), None);
        assert_eq!(arms_of('-'), None);
        assert_eq!(arms_of('|'), None);
    }

    #[test]
    fn arm_set_union_is_commutative_and_idempotent() {
        let a = ArmSet::N.union(ArmSet::E);
        let b = ArmSet::E.union(ArmSet::N);
        assert_eq!(a, b);
        assert_eq!(a.union(a), a);
    }

    #[test]
    fn empty_arm_set_is_empty() {
        assert!(ArmSet::EMPTY.is_empty());
        assert!(!ArmSet::N.is_empty());
    }
}
