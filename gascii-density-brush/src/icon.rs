use gascii_plugin_api::IconPath;

// M13 2c-3 1-6 4-7.5 6.5l2 2C10 9 13 6 14 3z  +  M5 9c-1.5.5-2 2-2 4 2 0 3.5-.5 4-2
// `static`, not `const`: a `const` slice gets re-promoted to a fresh anonymous `'static` allocation
// at every use site, which would make a `std::ptr::eq` identity check (the one this crate's own
// icon test and `gascii::ui::icons`' "brush icon comes from the plugin crate" test both rely on)
// false even though the *contents* are identical. A `static` has exactly one address, always.
pub static BRUSH_ICON: &[IconPath] = &[
    IconPath::closed(&[
        (13.0, 2.0),
        (10.0, 3.0),
        (7.0, 5.5),
        (5.5, 8.5),
        (7.5, 10.5),
        (10.0, 9.0),
        (13.0, 6.0),
        (14.0, 3.0),
    ]),
    IconPath::open(&[(5.0, 9.0), (3.5, 9.5), (3.0, 11.0), (3.0, 13.0), (5.0, 13.0), (6.5, 12.5), (7.0, 11.0)]),
];
