//! Ramp: an ordered light→dark character sequence, plus the built-in ramps.

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
}
