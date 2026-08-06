/// One stroked sub-path of a tool icon, authored in a 16x16 viewBox — the host strokes it (stroke
/// width, scale, color are all host-owned; a plugin only supplies geometry). Mirrors the shape the
/// host's own built-in icons were already authored in before this type existed.
pub struct IconPath {
    pub pts: &'static [(f32, f32)],
    pub closed: bool,
    pub dashed: bool,
}

impl IconPath {
    pub const fn open(pts: &'static [(f32, f32)]) -> IconPath {
        IconPath {
            pts,
            closed: false,
            dashed: false,
        }
    }

    pub const fn closed(pts: &'static [(f32, f32)]) -> IconPath {
        IconPath {
            pts,
            closed: true,
            dashed: false,
        }
    }
}
