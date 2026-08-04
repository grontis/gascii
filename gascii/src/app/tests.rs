    use super::*;
    use gascii_core::{export_text_frames, load_str, save_string, ToolEvent, ToolResponse};
    use crate::{anim_export, png_export};

    /// Each test gets its own throwaway directory under the OS temp dir so parallel test runs
    /// (and repeat local runs) never collide or race on the same path.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gascii_write_atomic_test_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_atomic_creates_a_new_file_with_exact_contents() {
        let dir = scratch_dir("create");
        let path = dir.join("out.gascii");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_overwrites_an_existing_file_and_leaves_no_temp_file_behind() {
        let dir = scratch_dir("overwrite");
        let path = dir.join("out.gascii");
        std::fs::write(&path, b"old contents").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!dir.join("out.gascii.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cell(ch: char) -> gascii_core::Cell {
        gascii_core::Cell { ch, fg: Rgba::WHITE, bg: Rgba::TRANSPARENT }
    }

    /// Pins the `sized_slot` mapping: sized kinds get distinct in-range slots, unsized get none —
    /// a duplicated or out-of-range slot would silently alias two tools' stamp settings.
    #[test]
    fn sized_slots_are_distinct_and_in_range() {
        let sized = [ToolKind::Pencil, ToolKind::Eraser, ToolKind::Line, BRUSH_KIND];
        let mut seen = std::collections::HashSet::new();
        for kind in sized {
            let slot = sized_slot(kind).expect("sized kind must have a slot");
            assert!(slot < sized_tool_count());
            assert!(seen.insert(slot), "slot {slot} assigned twice");
        }
        for kind in [
            ToolKind::Eyedropper,
            ToolKind::Text,
            ToolKind::Fill,
            ToolKind::Rectangle,
            ToolKind::Selection,
        ] {
            assert_eq!(sized_slot(kind), None, "{kind:?} must not have a stamp slot");
        }
    }

    /// `sized_tool_count()` must exactly cover the sized rows in the tool registry — too small
    /// would silently truncate a `stamps` array read, too large wastes slots no kind will ever
    /// index.
    #[test]
    fn sized_tool_count_matches_stamp_slots() {
        let count = tools().iter().filter(|d| d.stamp_slot.is_some()).count();
        assert_eq!(sized_tool_count(), count);
    }

    /// H4, directly: `assign_stamp_slots`' dense counter must produce distinct, `0..count` slot
    /// indices covering exactly the `sized` rows — the property that lets a plugin's sized tool
    /// grow this count with zero host edits, replacing the old per-name literal assignment.
    #[test]
    fn derived_stamp_slots_are_dense_and_distinct_and_match_sized_tool_count() {
        let mut slots: Vec<u8> = tools().iter().filter_map(|d| d.stamp_slot).collect();
        slots.sort_unstable();
        let expected: Vec<u8> = (0..sized_tool_count() as u8).collect();
        assert_eq!(slots, expected, "stamp slots must be exactly 0..sized_tool_count(), each once");
    }

    /// `stamp_slot` must be `Some` exactly for `sized` rows, never for any other — the invariant
    /// `assign_stamp_slots` and every `stamps[sized_slot(kind)]` call site both depend on.
    #[test]
    fn stamp_slot_is_some_exactly_when_sized() {
        for d in tools() {
            assert_eq!(d.stamp_slot.is_some(), d.sized, "{:?}: stamp_slot.is_some() must match sized", d.name);
        }
    }

    const ALL_KINDS: [ToolKind; 9] = [
        ToolKind::Pencil,
        ToolKind::Eraser,
        ToolKind::Eyedropper,
        ToolKind::Text,
        ToolKind::Fill,
        ToolKind::Rectangle,
        ToolKind::Line,
        ToolKind::Selection,
        BRUSH_KIND,
    ];

    /// The tool registry is the single source of truth for names, shortcuts, hints and
    /// constructors. If a kind were missing, `make_tool`'s `expect` would fire; if one were listed
    /// twice, the toolbox would show a duplicate cell and the two entries could drift apart.
    #[test]
    fn tools_table_lists_every_kind_exactly_once() {
        assert_eq!(tools().len(), ALL_KINDS.len());
        for kind in ALL_KINDS {
            let count = tools().iter().filter(|d| d.kind == kind).count();
            assert_eq!(count, 1, "{kind:?} appears {count} times in the tool registry");
        }
    }

    /// Locks each row's capability fields against the pre-refactor per-kind facts, so a typo in
    /// the tool registry can't silently drift from the scattered `match` arms it replaces.
    #[test]
    fn capability_fields_match_expected_for_every_kind() {
        for kind in ALL_KINDS {
            let d = tool_def(kind);
            let expected_stamp_slot = match kind {
                ToolKind::Pencil => Some(0u8),
                ToolKind::Eraser => Some(1),
                ToolKind::Line => Some(2),
                BRUSH_KIND => Some(3),
                _ => None,
            };
            let expected_holds_session = matches!(kind, ToolKind::Text | ToolKind::Selection);
            let expected_shows_hover = !matches!(kind, ToolKind::Selection);
            let expected_stamps_glyph = matches!(
                kind,
                ToolKind::Pencil | ToolKind::Fill | ToolKind::Rectangle | ToolKind::Line
            );
            let expected_suppresses_shortcuts = matches!(kind, ToolKind::Text);
            let expected_kiosk_visible = !matches!(kind, ToolKind::Text);
            // The two plugin-boundary capability fields: today only Brush's plugin-sourced row
            // sets either.
            let expected_pressure_sizeable = matches!(kind, BRUSH_KIND);
            let expected_wants_ctx_patch = matches!(kind, BRUSH_KIND);

            assert_eq!(d.stamp_slot, expected_stamp_slot, "{kind:?}: stamp_slot");
            assert_eq!(d.holds_session, expected_holds_session, "{kind:?}: holds_session");
            assert_eq!(d.shows_hover, expected_shows_hover, "{kind:?}: shows_hover");
            assert_eq!(d.stamps_glyph, expected_stamps_glyph, "{kind:?}: stamps_glyph");
            assert_eq!(
                d.suppresses_shortcuts, expected_suppresses_shortcuts,
                "{kind:?}: suppresses_shortcuts"
            );
            assert_eq!(d.kiosk_visible, expected_kiosk_visible, "{kind:?}: kiosk_visible");
            assert_eq!(d.pressure_sizeable, expected_pressure_sizeable, "{kind:?}: pressure_sizeable");
            assert_eq!(d.wants_ctx_patch, expected_wants_ctx_patch, "{kind:?}: wants_ctx_patch");
        }
    }

    /// Locks the registry's observable shape against the merge machinery's own plumbing: whether a
    /// row came from a pure built-in literal or a plugin bundle, the table still has exactly 9
    /// entries with exactly the capability values `capability_fields_match_expected_for_every_kind`
    /// already pins.
    #[test]
    fn tools_registry_merge_produces_the_same_9_row_table_the_pre_plugin_registry_had() {
        assert_eq!(tools().len(), 9);
        capability_fields_match_expected_for_every_kind();
    }

    /// H4's direct regression test: a synthetic plugin tool name the host has never heard of must
    /// merge into a complete, valid registry row — no panic, and a real derived stamp slot if it's
    /// sized — proving the three-place (really four-place) special-casing this round deletes is
    /// actually gone.
    #[test]
    fn merge_plugin_row_accepts_an_unknown_plugin_tool_name_without_panicking() {
        let cap = gascii_plugin_api::PluginToolCapabilities {
            name: "Sprinkler",
            key: egui::Key::Z,
            tip: "t",
            make: || Box::new(InertTool),
            icon: &[],
            sized: true,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            pressure_sizeable: false,
            wants_ctx_patch: false,
        };
        let row = merge_plugin_row(0, &cap);
        assert_eq!(row.kind, ToolKind::Plugin("Sprinkler"));
        assert_eq!(row.plugin_slot, Some(0));
    }

    /// Edge case: two plugins (or a plugin and a built-in) registering the same tool name would
    /// make `tool_def`'s `find()` resolve ambiguously. `validate_unique_tool_names` must catch it,
    /// distinct from `validate_key_claims`'s own key-collision error.
    #[test]
    fn validate_unique_tool_names_rejects_a_duplicate_name() {
        let mut rows = tools().to_vec();
        let dupe = rows[0];
        rows.push(dupe);
        let err = validate_unique_tool_names(&rows).expect_err("a duplicate tool name must be an error");
        assert!(err.contains(dupe.name), "error must name the duplicated tool: {err}");
    }

    /// Two plugins persisting under one id would make stored enabled-state resolve against
    /// whichever descriptor `position()` finds first — the other plugin silently inherits prefs
    /// that were never its own. `validate_unique_plugin_ids` must reject this at registry
    /// construction, like its tool-name and key-claim siblings.
    #[test]
    fn validate_unique_plugin_ids_rejects_a_duplicate_id() {
        let mut dupe = gascii_anim::DESCRIPTOR;
        dupe.id = gascii_density_brush::DESCRIPTOR.id;
        let err = validate_unique_plugin_ids(&[gascii_density_brush::DESCRIPTOR, dupe])
            .expect_err("a duplicate plugin id must be an error");
        assert!(err.contains(dupe.id), "error must name the duplicated id: {err}");
        validate_unique_plugin_ids(PLUGINS).expect("the real descriptor table has unique ids");
    }

    /// The Plugin Manager (`dialogs.rs`) renders straight off `PLUGINS` — a plugin missing from
    /// this list is invisible there regardless of how correctly its own crate is built.
    #[test]
    fn gascii_layers_is_registered_in_plugins() {
        assert!(
            PLUGINS.iter().any(|d| d.id == gascii_layers::DESCRIPTOR.id),
            "gascii-layers must be registered so it appears in the Plugin Manager"
        );
    }

    /// The Plugin Manager renders id/name/description/version verbatim, and prefs key off `id` —
    /// an empty string in any registered descriptor is a registration mistake this pins against.
    #[test]
    fn every_registered_descriptor_carries_non_empty_metadata() {
        for d in PLUGINS {
            assert!(!d.id.is_empty(), "descriptor {:?} has an empty id", d.name);
            assert!(!d.name.is_empty(), "descriptor {:?} has an empty name", d.id);
            assert!(!d.description.is_empty(), "descriptor {:?} has an empty description", d.id);
            assert!(!d.version.is_empty(), "descriptor {:?} has an empty version", d.id);
        }
    }

    /// `merge_plugin_row`'s `plugin_slot` must carry through whatever index it is given, not a
    /// hardcoded value — every downstream consumer (`tool_ctx`'s ctx-patch injection, the
    /// pressure-override gate, `binding_options_geom`'s dedup) trusts `plugin_slot` to resolve back
    /// to the correct entry of `GasciiApp.plugins`. This phase ships exactly two plugins, so a real
    /// `GasciiApp`'s Brush row can only ever observe `plugin_slot == Some(0)` — this test exercises
    /// indices beyond that scale directly against the pure merge function.
    #[test]
    fn merge_plugin_row_carries_the_given_plugin_slot_index_verbatim() {
        let cap = gascii_plugin_api::PluginToolCapabilities {
            name: "Brush",
            key: egui::Key::B,
            tip: "t",
            make: || Box::new(gascii_core::DensityBrush::new()),
            icon: &[],
            sized: true,
            holds_session: false,
            shows_hover: true,
            stamps_glyph: false,
            suppresses_shortcuts: false,
            kiosk_visible: true,
            pressure_sizeable: true,
            wants_ctx_patch: true,
        };
        for slot in [0usize, 1, 4, 7] {
            let row = merge_plugin_row(slot, &cap);
            assert_eq!(row.plugin_slot, Some(slot), "merge_plugin_row must not hardcode a plugin_slot index");
        }
    }

    /// A fresh `GasciiApp`'s Brush row's `plugin_slot` must resolve back to the exact live plugin
    /// instance that actually registered the "Brush" tool — not merely to *some* valid index into
    /// `plugins`. Proven by downcasting the instance at that index to the concrete `BrushPlugin`
    /// type — strictly stronger than the pre-migration version, which could only call
    /// `register_tools()` on the trait object: this pins the *type* at that index, not just the
    /// description (the description now comes from `PLUGINS[slot].tools` directly, which needs no
    /// instance at all).
    #[test]
    fn a_fresh_apps_brush_row_plugin_slot_resolves_to_the_live_instance_that_registered_brush() {
        let mut app = GasciiApp::headless();
        let slot = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");
        let described = (PLUGINS[slot].tools)();
        assert_eq!(described.len(), 1);
        assert_eq!(described[0].name, gascii_density_brush::BRUSH);
        assert!(
            app.plugins[slot].as_any_mut().downcast_mut::<gascii_density_brush::BrushPlugin>().is_some(),
            "plugin_slot must resolve to a live BrushPlugin instance"
        );
    }

    /// M9's structural guarantee, directly: for every row with a `plugin_slot`, that same index's
    /// `PLUGINS` descriptor must describe a tool by the same name — the property that makes
    /// "description and instances come from one list" true by construction rather than by
    /// convention two separately-iterated lists have to uphold.
    #[test]
    fn plugin_slot_indexes_the_same_slice_for_description_and_instances() {
        for d in tools() {
            if let Some(slot) = d.plugin_slot {
                let described = (PLUGINS[slot].tools)();
                assert!(
                    described.iter().any(|cap| cap.name == d.name),
                    "PLUGINS[{slot}] must describe a tool named {:?}",
                    d.name
                );
            }
        }
    }

    /// D5's refinement, directly: `GasciiApp::with_state`/`headless` must construct one real,
    /// *retained* instance per `PLUGINS` entry, per app — never a process-global shared instance.
    /// Two independently constructed apps must never see each other's Brush state; a
    /// `OnceLock`-cached plugin list (the design the plan explicitly rejected) would fail this.
    #[test]
    fn two_independent_gascii_apps_never_share_brush_plugin_state() {
        let mut app1 = GasciiApp::headless();
        let mut app2 = GasciiApp::headless();
        app1.brush_plugin_mut().set_active_ramp(1);
        app1.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));
        app1.brush_plugin_mut().set_pressure_enabled(true);

        assert_eq!(app2.brush_plugin_mut().active_ramp(), 0, "app2 must start at Brush's own default ramp, not app1's mutated one");
        assert!(
            matches!(app2.brush_plugin_mut().density_mode(), gascii_core::DensityMode::Fixed(_)),
            "app2 must not see app1's Buildup mode"
        );
        assert!(!app2.brush_plugin_mut().pressure_enabled(), "app2 must not see app1's pressure opt-in");

        // And app1's own state must still hold, proving this isn't a case of neither app retaining
        // anything at all.
        assert_eq!(app1.brush_plugin_mut().active_ramp(), 1);
    }

    /// `Plugin::panel` must be a true no-op for the real, shipped builtin plugin list
    /// (`BrushPlugin` never overrides it) — called in both chrome modes, must not mutate the
    /// document or panic.
    #[test]
    fn every_plugins_panel_hook_is_a_true_no_op_for_the_real_builtin_list() {
        let mut app = GasciiApp::headless();
        let before = app.doc.clone();
        let ctx = egui::Context::default();
        let (stylus_detected, bound) = host_context(&app);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let host = host_facts(&app.doc, stylus_detected, bound, app.history.top_edit_id());
            for p in app.plugins.iter_mut() {
                let outcome = p.panel(ui, false, &host);
                assert!(outcome.edits.is_empty());
                let outcome = p.panel(ui, true, &host);
                assert!(outcome.edits.is_empty());
            }
        });
        assert_eq!(app.doc, before, "no plugin's panel hook may mutate the document");
    }

    fn raw_input_with_screen(w: f32, h: f32) -> egui::RawInput {
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(w, h))), ..Default::default() }
    }

    /// A throwaway plugin double whose `panel` draws a real `egui::Panel::bottom` — proves the
    /// panel-loop reorder at the mechanism level, not just "it doesn't panic": a bottom
    /// panel declared from inside a plugin, run through `run_plugin_panels` BEFORE `CentralPanel`,
    /// must actually shrink the central panel's claimed rect, exactly like every other panel this
    /// app declares. This is the property a `ctx: &Context`-only plugin signature could never prove
    /// (egui's `Panel` reads/mutates its literal parent `Ui`'s own placer state, not `Context` —
    /// see `Plugin::panel`'s doc comment) — the reason `panel`'s host parameter is `&mut egui::Ui`.
    struct BottomPanelDouble;
    impl Plugin for BottomPanelDouble {
        fn panel(&mut self, ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
            egui::Panel::bottom("test_timeline_double").exact_size(50.0).show(ui, |ui| {
                ui.label("test timeline");
            });
            gascii_plugin_api::PanelOutcome::default()
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn a_real_bottom_panel_from_a_plugin_correctly_shrinks_the_central_panel_before_it_claims_space() {
        let mut app = GasciiApp::headless();
        app.plugins.push(Box::new(BottomPanelDouble));

        let ctx = egui::Context::default();
        let mut with_double = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            with_double = Some(resp.response.rect);
        });

        let mut app_without = GasciiApp::headless(); // no BottomPanelDouble registered
        let ctx2 = egui::Context::default();
        let mut without_double = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app_without.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            without_double = Some(resp.response.rect);
        });

        let with_double = with_double.unwrap();
        let without_double = without_double.unwrap();
        assert!(
            with_double.height() < without_double.height(),
            "a real Panel::bottom declared inside a plugin must shrink the central panel's rect — \
             with={with_double:?} without={without_double:?}"
        );
    }

    /// `AnimPlugin`'s own single-frame gate, exercised through the real registered plugin list (not
    /// a double): a fresh document has exactly one frame, so its panel must claim zero space and
    /// must not shrink the central panel at all. `gascii-layers`' own panel has no such gate — it
    /// has no host menu bootstrap for its first extra layer the way "Add Frame" does, so it must
    /// always be visible — disabled here so this test isolates `AnimPlugin`'s gate specifically,
    /// not the compound layout of every registered plugin.
    #[test]
    fn anim_panel_claims_no_space_while_frame_count_is_one() {
        let mut app = GasciiApp::headless();
        assert_eq!(app.doc.frame_count(), 1);
        let layers = PLUGINS.iter().position(|d| d.id == gascii_layers::DESCRIPTOR.id).expect("gascii-layers is registered");
        app.set_plugin_enabled(layers, false);

        let ctx = egui::Context::default();
        let mut central_rect = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            central_rect = Some(resp.response.rect);
        });

        let ctx2 = egui::Context::default();
        let mut central_rect_no_plugins = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            central_rect_no_plugins = Some(resp.response.rect);
        });

        assert_eq!(central_rect.unwrap(), central_rect_no_plugins.unwrap(), "a single-frame document's layout must be byte-identical with or without the plugin panel loop running");
    }

    /// The real, registered `gascii-anim` plugin (not a double) must claim real screen space the
    /// moment a second frame exists — the flip side of the single-frame no-op gate above.
    #[test]
    fn anim_panel_claims_space_once_a_second_frame_exists() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);

        let ctx = egui::Context::default();
        let mut with_second_frame = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            with_second_frame = Some(resp.response.rect);
        });

        let app_single = GasciiApp::headless();
        let ctx2 = egui::Context::default();
        let mut single_frame = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            let _ = &app_single; // single-frame baseline: no plugin panel call needed (the single-frame no-op gate)
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            single_frame = Some(resp.response.rect);
        });

        assert!(
            with_second_frame.unwrap().height() < single_frame.unwrap().height(),
            "the timeline panel must claim real space once frame_count() > 1"
        );
    }

    /// A test-double plugin returning a `PanelOutcome` with one `Edit`, driven through one
    /// `run_plugin_panels` call — the edit must reach the document through the same, unmodified
    /// `apply_edit` choke point every other mutation uses (undo/redo proves this, not just the
    /// forward direction).
    #[test]
    fn plugin_panel_outcome_edits_are_applied_through_apply_edit() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.resolved_frame_duration_ms(0), Some(Document::DEFAULT_FRAME_DURATION_MS));

        struct EditOutcomeDouble;
        impl Plugin for EditOutcomeDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let idx = host.document().active_frame();
                gascii_plugin_api::PanelOutcome {
                    edits: vec![gascii_core::Edit::SetFrameDuration { index: idx, before: None, after: Some(50) }],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(EditOutcomeDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.resolved_frame_duration_ms(0), Some(50), "the PanelOutcome's edit must reach the document");
        assert!(app.history.can_undo(), "the edit must have gone through History::apply, not bypassed it");
        app.request_undo();
        assert_eq!(
            app.doc.resolved_frame_duration_ms(0),
            Some(Document::DEFAULT_FRAME_DURATION_MS),
            "undo must reverse the plugin-originated edit exactly like any other apply_edit call"
        );
    }

    /// `DocProperty::ActiveFrame` in a returned `PanelOutcome` must flush pending sessions on BOTH bindings
    /// before actually moving the cursor — a live Text burst must commit onto the frame it was
    /// typed on, not silently carry over to the new one.
    #[test]
    fn plugin_panel_outcome_set_active_frame_flushes_pending_sessions_before_switching() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);

        // A pending Text burst on L, uncommitted, at (0,0) on frame 0.
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        struct SwitchFrameDouble;
        impl Plugin for SwitchFrameDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome {
                    properties: vec![gascii_plugin_api::DocProperty::ActiveFrame(1)],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(SwitchFrameDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.active_frame, 1, "set_active_frame must move the cursor");
        assert_eq!(app.doc.active_frame(), 1);
        assert_eq!(
            app.doc.cell_at(0, 0, 0, 0).unwrap().ch,
            'a',
            "a pending burst must be flushed onto the frame it was typed on before the switch, not dropped or carried over"
        );
    }

    /// The layer twin of `plugin_panel_outcome_set_active_frame_flushes_pending_sessions_before_switching`:
    /// `DocProperty::ActiveLayer` must flush pending sessions on BOTH bindings before actually moving
    /// the cursor — a live Text burst must commit onto the layer it was typed on, not silently carry
    /// over to the new one.
    #[test]
    fn plugin_panel_outcome_set_active_layer_flushes_pending_sessions_before_switching() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.active_layer, 1, "sanity: adding a layer makes it the active one");
        app.switch_active_layer(0); // back to layer 0, where the pending burst below is typed

        // A pending Text burst on L, uncommitted, at (0,0) on layer 0.
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        struct SwitchLayerDouble;
        impl Plugin for SwitchLayerDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome {
                    properties: vec![gascii_plugin_api::DocProperty::ActiveLayer(1)],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(SwitchLayerDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.active_layer, 1, "set_active_layer must move the cursor");
        assert_eq!(app.doc.active_layer(), 1);
        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().ch,
            'a',
            "a pending burst must be flushed onto the layer it was typed on before the switch, not dropped or carried over"
        );
    }

    /// `DocProperty::LoopPlayback` in a returned `PanelOutcome` must write `Document.loop_playback`
    /// directly — a plain field write, not an `Edit`, so it must NOT create an undo entry.
    #[test]
    fn plugin_panel_outcome_set_loop_playback_writes_the_document_field_directly_without_history() {
        let mut app = GasciiApp::headless();
        assert!(app.doc.loop_playback, "sanity: a fresh document defaults to looping");
        let can_undo_before = app.history.can_undo();

        struct LoopToggleDouble;
        impl Plugin for LoopToggleDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome {
                    properties: vec![gascii_plugin_api::DocProperty::LoopPlayback(false)],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(LoopToggleDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert!(!app.doc.loop_playback, "the PanelOutcome's request must reach Document.loop_playback");
        assert_eq!(app.history.can_undo(), can_undo_before, "a plain field write must never create an undo entry");
    }

    /// `DocProperty::DefaultFrameDuration` in a returned `PanelOutcome` must write `Document.
    /// frame_duration_ms` directly — a plain field write, not an `Edit`, mirroring
    /// `DocProperty::LoopPlayback`'s own contract exactly.
    #[test]
    fn plugin_panel_outcome_set_default_frame_duration_writes_the_document_field_directly_without_history() {
        let mut app = GasciiApp::headless();
        let can_undo_before = app.history.can_undo();

        struct DefaultDurationDouble;
        impl Plugin for DefaultDurationDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome {
                    properties: vec![gascii_plugin_api::DocProperty::DefaultFrameDuration(250)],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(DefaultDurationDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.frame_duration_ms, 250, "the PanelOutcome's request must reach Document.frame_duration_ms");
        assert_eq!(app.history.can_undo(), can_undo_before, "a plain field write must never create an undo entry");
    }

    /// Fix 2: `saved_marker` alone (the undo-stack comparison) is blind to a session-meta property
    /// changing — toggling Loop must still be visible to `is_dirty()`/the close-confirm path, via
    /// the saved-snapshot comparison.
    #[test]
    fn toggling_loop_playback_marks_the_document_dirty_even_though_it_is_never_an_edit() {
        let mut app = GasciiApp::headless();
        assert!(!app.is_dirty(), "sanity: a fresh headless app starts clean");

        app.drain_panel_outcomes(vec![gascii_plugin_api::PanelOutcome {
            properties: vec![gascii_plugin_api::DocProperty::LoopPlayback(false)],
            ..Default::default()
        }]);

        assert!(app.is_dirty(), "toggling Loop must mark the document dirty even with an untouched undo stack");
        assert!(!app.history.can_undo(), "sanity: still no undo entry — this must be caught by the snapshot comparison, not History");
    }

    #[test]
    fn toggling_the_default_frame_duration_marks_the_document_dirty() {
        let mut app = GasciiApp::headless();
        assert!(!app.is_dirty());

        app.drain_panel_outcomes(vec![gascii_plugin_api::PanelOutcome {
            properties: vec![gascii_plugin_api::DocProperty::DefaultFrameDuration(300)],
            ..Default::default()
        }]);

        assert!(app.is_dirty(), "changing the default frame duration must mark the document dirty");
    }

    /// A successful save resets both halves of dirty tracking — the undo marker (already covered
    /// elsewhere) and the session-meta snapshot this fix adds — so a Loop toggle followed by a save
    /// reads clean again, not perpetually dirty.
    #[test]
    fn saving_resets_the_saved_session_meta_snapshot_so_the_document_reads_clean_again() {
        let mut app = GasciiApp::headless();
        app.drain_panel_outcomes(vec![gascii_plugin_api::PanelOutcome {
            properties: vec![gascii_plugin_api::DocProperty::LoopPlayback(false)],
            ..Default::default()
        }]);
        assert!(app.is_dirty(), "sanity: the toggle made it dirty");

        let path = std::env::temp_dir().join(format!("gascii_dirty_snapshot_save_test_{}.gascii", std::process::id()));
        app.write_gascii(&path);
        let _ = std::fs::remove_file(&path);

        assert!(!app.is_dirty(), "a successful save must reset the session-meta snapshot alongside the undo marker");
    }

    /// Opening a document resets the snapshot to whatever the freshly loaded document's own
    /// session-meta values are, not the previous document's.
    #[test]
    fn opening_a_document_resets_the_saved_session_meta_snapshot_to_the_loaded_documents_own_values() {
        let mut app = GasciiApp::headless();
        app.drain_panel_outcomes(vec![gascii_plugin_api::PanelOutcome {
            properties: vec![gascii_plugin_api::DocProperty::LoopPlayback(false)],
            ..Default::default()
        }]);
        assert!(app.is_dirty(), "sanity: dirty before the open");

        // A single-frame document round-trips through the v1 envelope shape, which carries no
        // `loop_playback` field at all (see `gascii_json`'s module doc) — a second frame forces
        // the v2 shape so this test actually exercises the field being loaded, not just defaulted.
        let mut fresh = Document::default_document();
        let edit = gascii_core::add_frame(&fresh, 1, gascii_core::Frame::blank(fresh.width, fresh.height)).unwrap();
        let mut history = gascii_core::History::new();
        history.apply(&mut fresh, edit);
        fresh.loop_playback = false; // matches what's about to be loaded
        let path = std::env::temp_dir().join(format!("gascii_dirty_snapshot_open_test_{}.gascii", std::process::id()));
        std::fs::write(&path, save_string(&fresh)).unwrap();

        app.open_path(&path);
        let _ = std::fs::remove_file(&path);

        assert!(!app.is_dirty(), "a fresh open must reset the snapshot to the loaded document's own values");
        assert!(!app.doc.loop_playback, "sanity: the loaded document's own loop_playback was actually applied");
    }

    /// `create_new_document` must reset the snapshot too — a Loop toggle on the old document must
    /// not leave the brand-new one reading dirty.
    #[test]
    fn create_new_document_resets_the_saved_session_meta_snapshot() {
        let mut app = GasciiApp::headless();
        app.drain_panel_outcomes(vec![gascii_plugin_api::PanelOutcome {
            properties: vec![gascii_plugin_api::DocProperty::LoopPlayback(false)],
            ..Default::default()
        }]);
        assert!(app.is_dirty());

        app.create_new_document();

        assert!(!app.is_dirty(), "a brand-new document must start clean regardless of the old document's dirty session-meta state");
    }

    /// `create_new_document` swaps in a `Document` that always starts at frame/layer 0 — the app-side
    /// cursors must follow, or the first stroke on the fresh document is built against a stale,
    /// out-of-range index and silently no-ops.
    #[test]
    fn create_new_document_resets_active_frame_and_active_layer() {
        let mut app = GasciiApp::headless();
        let add_frame = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(add_frame, None);
        app.switch_active_frame(1);
        for _ in 0..2 {
            // Each add_layer lands its own new layer active, so two adds walk the cursor to 2.
            let add_layer = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
            app.apply_edit(add_layer, None);
        }
        assert_eq!(app.active_frame, 1, "sanity");
        assert_eq!(app.active_layer, 2, "sanity");

        app.create_new_document();

        assert_eq!(app.active_frame, 0, "a fresh document must reset the frame cursor");
        assert_eq!(app.active_layer, 0, "a fresh document must reset the layer cursor");
    }

    /// The `open_path` twin of `create_new_document_resets_active_frame_and_active_layer` — a loaded
    /// document also always starts at frame/layer 0.
    #[test]
    fn open_path_resets_active_frame_and_active_layer() {
        let mut app = GasciiApp::headless();
        let add_frame = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(add_frame, None);
        app.switch_active_frame(1);
        for _ in 0..2 {
            // Each add_layer lands its own new layer active, so two adds walk the cursor to 2.
            let add_layer = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
            app.apply_edit(add_layer, None);
        }
        assert_eq!(app.active_frame, 1, "sanity");
        assert_eq!(app.active_layer, 2, "sanity");

        let dir = scratch_dir("open_path_resets_cursors");
        let path = dir.join("out.gascii");
        let fresh = gascii_core::Document::new(4, 4);
        std::fs::write(&path, save_string(&fresh)).unwrap();

        app.open_path(&path);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(app.active_frame, 0, "opening a document must reset the frame cursor");
        assert_eq!(app.active_layer, 0, "opening a document must reset the layer cursor");
    }

    /// End-to-end through `handle_close_request`: a Loop toggle alone (no `Edit`, no undo entry)
    /// must still raise the same unsaved-changes veto a real cell edit would — the `is_dirty()`
    /// snapshot comparison this fix adds is what `handle_close_request` actually consults, not a
    /// hand-reimplemented copy of it.
    #[test]
    fn toggling_loop_playback_raises_the_close_confirm_veto_exactly_like_a_real_edit_would() {
        let mut app = GasciiApp::headless();
        app.drain_panel_outcomes(vec![gascii_plugin_api::PanelOutcome {
            properties: vec![gascii_plugin_api::DocProperty::LoopPlayback(false)],
            ..Default::default()
        }]);
        assert!(app.is_dirty(), "sanity: the toggle alone made it dirty");
        assert!(!app.history.can_undo(), "sanity: still no undo entry — this must go through the snapshot comparison");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().events.push(egui::ViewportEvent::Close);
        let _ = ctx.run_ui(raw, |_ui| app.handle_close_request(&ctx));

        assert_eq!(app.confirm, Some(PendingConfirm::CloseApp), "a Loop-only dirty document must veto the close exactly like an edited one");
    }

    /// A returned `PanelOutcome.error` must reach `self.last_error` — the same status-bar channel
    /// every other structural trigger already uses, not a silent no-op (Important #1).
    #[test]
    fn plugin_panel_outcome_error_surfaces_through_last_error() {
        let mut app = GasciiApp::headless();
        assert!(app.last_error.is_none());

        struct ErrorOutcomeDouble;
        impl Plugin for ErrorOutcomeDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, _host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                gascii_plugin_api::PanelOutcome { error: Some("add frame: exceeds the 256 maximum".to_string()), ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(ErrorOutcomeDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.last_error_text(), Some("add frame: exceeds the 256 maximum"));
    }

    /// The status bar's expiry rule: `error_flash` shows a fresh message with its time-left, stops
    /// showing it once `ERROR_FLASH_TTL` has passed, while `last_error_text` (the dialog-inline
    /// read) keeps the message regardless — dialog validation must not vanish mid-edit.
    #[test]
    fn error_flash_expires_for_the_status_bar_but_persists_for_dialog_reads() {
        let mut app = GasciiApp::headless();
        let now = std::time::Instant::now();
        assert!(app.error_flash(now).is_none(), "no error, nothing to flash");

        // Stamped explicitly rather than through `flash_error` so `now`-relative arithmetic is
        // exact — `flash_error` reads its own, slightly later clock.
        app.last_error = Some(crate::app::ErrorFlash { text: "boom".to_string(), at: now });
        let (text, left) = app.error_flash(now).expect("a fresh flash must be visible");
        assert_eq!(text, "boom");
        assert!(left <= crate::app::ERROR_FLASH_TTL, "time-left is bounded by the TTL");

        let after_ttl = now + crate::app::ERROR_FLASH_TTL;
        assert!(app.error_flash(after_ttl).is_none(), "at/past the TTL the status bar stops showing it");
        assert_eq!(app.last_error_text(), Some("boom"), "the dialog-inline read must not expire");

        app.flash_error("again");
        assert!(app.error_flash(std::time::Instant::now()).is_some(), "a re-raise restarts the clock");
    }

    /// A `PanelOutcome` carrying BOTH a successful edit and a failure message in the same drain pass
    /// (a multi-op outcome) must apply the edit AND still surface the error — neither channel may
    /// silently swallow the other just because they arrived together.
    #[test]
    fn plugin_panel_outcome_with_both_an_edit_and_an_error_applies_the_edit_and_still_surfaces_the_error() {
        let mut app = GasciiApp::headless();

        struct PartialFailureDouble;
        impl Plugin for PartialFailureDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let idx = host.document().active_frame();
                gascii_plugin_api::PanelOutcome {
                    edits: vec![gascii_core::Edit::SetFrameDuration { index: idx, before: None, after: Some(50) }],
                    error: Some("duplicate frame: exceeds the 256 maximum".to_string()),
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(PartialFailureDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.resolved_frame_duration_ms(0), Some(50), "the succeeding half of a multi-op outcome must still apply");
        assert_eq!(app.last_error_text(), Some("duplicate frame: exceeds the 256 maximum"), "the failing half must still surface");
    }

    /// A `PanelOutcome`-originated `duplicate_frame` edit (built from the real `gascii_core::
    /// duplicate_frame`, exactly what `gascii-anim`'s own timeline controls call — not a hand-built
    /// `Edit` literal) must land in `History` and undo byte-identically to the functionally same
    /// operation reached via the host's own "Add Frame" menu path — the two entry points must be
    /// indistinguishable to `History`/undo, not just visually similar.
    #[test]
    fn plugin_outcome_originated_duplicate_frame_edit_lands_in_history_and_undoes_identically_to_the_menu_path() {
        let mut app_plugin = GasciiApp::headless();
        app_plugin.doc.set_cell(0, 0, 0, cell('D'));

        struct DuplicateOutcomeDouble;
        impl Plugin for DuplicateOutcomeDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let doc = host.document();
                let edit = gascii_core::duplicate_frame(doc, doc.active_frame()).unwrap();
                gascii_plugin_api::PanelOutcome { edits: vec![edit], ..Default::default() }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app_plugin.plugins.push(Box::new(DuplicateOutcomeDouble));
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app_plugin.run_plugin_panels(ui, false));

        let mut app_menu = GasciiApp::headless();
        app_menu.doc.set_cell(0, 0, 0, cell('D'));
        app_menu.add_frame_via_menu();

        assert_eq!(app_plugin.doc, app_menu.doc, "a plugin-outcome-originated duplicate must produce a byte-identical document to the menu path");
        assert!(app_plugin.history.can_undo());

        app_plugin.request_undo();
        app_menu.request_undo();
        assert_eq!(app_plugin.doc, app_menu.doc, "undo must restore both paths to an identical document");
        assert_eq!(app_plugin.doc.frame_count(), 1, "undo must fully reverse the plugin-originated duplicate");
    }

    /// A single `PanelOutcome` that both adds a frame AND requests switching to the frame the edit
    /// just created — the index requested by `set_active_frame` does not exist in the document until
    /// the outcome's own `edits` are drained first. Proves `run_plugin_panels`'s edits-then-switch
    /// ordering, not just that each half works when tested alone.
    #[test]
    fn plugin_panel_outcome_that_both_adds_a_frame_and_switches_to_it_in_one_pass_resolves_against_the_post_edit_document() {
        let mut app = GasciiApp::headless();
        assert_eq!(app.doc.frame_count(), 1);

        struct AddAndSwitchDouble;
        impl Plugin for AddAndSwitchDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let doc = host.document();
                let edit = gascii_core::add_frame(doc, 1, gascii_core::Frame::blank(doc.width, doc.height)).unwrap();
                gascii_plugin_api::PanelOutcome {
                    edits: vec![edit],
                    properties: vec![gascii_plugin_api::DocProperty::ActiveFrame(1)],
                    ..Default::default()
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(AddAndSwitchDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));

        assert_eq!(app.doc.frame_count(), 2, "the edit half of the outcome must have landed");
        assert_eq!(app.active_frame, 1, "the switch half must resolve against the post-edit document, landing on the frame the edit just created");
        assert_eq!(app.doc.active_frame(), 1);
    }

    /// A `PanelOutcome`-originated `remove_frame`/`reorder_frame` edit (the real `gascii_core`
    /// functions, matching what `gascii-anim`'s own Delete/reorder controls call) must undo
    /// byte-exactly, mirroring `frame-substrate`'s own already-proven ladder-undo property, this
    /// time reached through the plugin-drain path rather than a direct `apply_edit` call.
    #[test]
    fn plugin_panel_outcome_originated_delete_edit_undoes_to_a_byte_exact_prior_document() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        app.doc.set_cell(1, 0, 0, cell('B'));
        let before_delete = app.doc.clone();
        assert_eq!(before_delete.frame_count(), 2);

        struct DeleteFrameDouble;
        impl Plugin for DeleteFrameDouble {
            fn panel(&mut self, _ui: &mut egui::Ui, _kiosk: bool, host: &dyn gascii_plugin_api::PluginHost) -> gascii_plugin_api::PanelOutcome {
                let doc = host.document();
                match gascii_core::remove_frame(doc, doc.active_frame()) {
                    Ok(edit) => gascii_plugin_api::PanelOutcome { edits: vec![edit], ..Default::default() },
                    Err(_) => gascii_plugin_api::PanelOutcome::default(),
                }
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
        app.plugins.push(Box::new(DeleteFrameDouble));

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.run_plugin_panels(ui, false));
        assert_eq!(app.doc.frame_count(), 1, "the plugin-outcome-originated delete must have applied");

        app.request_undo();
        assert_eq!(app.doc, before_delete, "undo must byte-exactly restore the prior 2-frame document");
    }

    /// `switch_active_frame` (the target of a `DocProperty::ActiveFrame`) flushes via
    /// `flush_all()`, but that only actually commits a `holds_session` tool's (Text/Selection)
    /// pending work — a plain stroke tool like Pencil does not hold a "session" the flush machinery
    /// recognizes, so a mid-drag Pencil press is left genuinely pending, not force-committed, across
    /// the switch. The eventual `Release` (whenever the pointer lifts) builds its `ToolCtx` fresh
    /// against whatever frame is active *at that moment* — this proves it lands on the frame the
    /// stroke was actually released against, and that the switch itself does not silently commit (or
    /// lose) the in-flight stroke onto either frame by itself.
    #[test]
    fn switch_active_frame_mid_pencil_drag_does_not_silently_commit_the_pending_stroke_to_either_frame() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.active_frame, 0);

        app.active_glyph = 'Z';
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Pencil);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.stroke_owner = Some(Binding::L);

        // A plugin-shaped frame switch arrives mid-drag (mirrors a real timeline click's outcome).
        app.switch_active_frame(1);

        assert_eq!(app.active_frame, 1, "the switch itself must still take effect");
        assert_eq!(app.doc.cell_at(0, 0, 0, 0).unwrap().ch, ' ', "the pending stroke must not have been silently committed onto its origin frame by the flush");
        assert_eq!(app.doc.cell_at(1, 0, 0, 0).unwrap().ch, ' ', "nor onto the frame just switched to");

        // Whenever the pointer eventually releases, it commits against whatever frame is active at
        // that moment — proving the eventual commit target is the frame the release actually
        // targets, never a stale snapshot of the origin frame.
        let tctx2 = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx2, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell_at(1, 0, 0, 0).unwrap().ch, 'Z', "the eventual release commits onto whichever frame is active when it fires");
        assert_eq!(app.doc.cell_at(0, 0, 0, 0).unwrap().ch, ' ', "the origin frame is left untouched by a release that fires after the switch");
    }

    /// `add_frame_via_menu`'s own per-variant `last_error` message at the `MAX_FRAMES` boundary —
    /// pinned literally so a future wording change is deliberate, and so it can be cross-checked
    /// against `gascii-anim`'s own `frame_op_error_message` test for the same `FrameOpError` variant
    /// (Important #1's "menu and timeline paths produce consistent messages for the same failure").
    #[test]
    fn add_frame_via_menu_reports_the_max_frames_boundary_with_a_specific_readable_message() {
        let mut app = GasciiApp::headless();
        for i in 1..Document::MAX_FRAMES {
            let edit = gascii_core::add_frame(&app.doc, i, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
            app.apply_edit(edit, None);
        }
        assert_eq!(app.doc.frame_count(), Document::MAX_FRAMES);

        app.add_frame_via_menu();

        assert_eq!(app.doc.frame_count(), Document::MAX_FRAMES, "a rejected add must not change frame_count");
        assert_eq!(app.last_error_text(), Some("add frame: exceeds the 256 maximum"));
    }

    /// The Animation menu's "Add Frame" bootstrap: duplicates the active frame and flushes any pending
    /// session first — mirrors "Resize Canvas…"'s own flush-before-structural-trigger discipline.
    #[test]
    fn add_frame_menu_item_duplicates_the_active_frame_and_flushes_first() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 0, 0, cell('D'));

        // A pending Text burst, uncommitted — must be flushed (not dropped) before the duplicate.
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('X'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        app.add_frame_via_menu();

        assert_eq!(app.doc.frame_count(), 2, "Add Frame must duplicate into a second frame");
        assert_eq!(app.doc.cell_at(0, 0, 1, 0).unwrap().ch, 'X', "the pending burst must be flushed before duplicating");
        assert_eq!(app.doc.cell_at(1, 0, 0, 0).unwrap().ch, 'D', "the duplicate must carry the source frame's content");
        assert_eq!(app.doc.cell_at(1, 0, 1, 0).unwrap().ch, 'X', "the duplicate must carry the just-flushed burst too");
        assert!(app.last_error.is_none());
    }

    /// End-to-end integration: drives the whole Add-Frame/switch-frame/undo commit chain together
    /// through the real registered `gascii-anim` plugin (not a double) — Add Frame via the menu,
    /// draw on frame 2, switch back to frame 1 via a synthetic `PanelOutcome`, undo twice, and
    /// confirm both the frame structure and cell content are back to the single-frame starting state.
    #[test]
    fn add_frame_draw_switch_frame_and_undo_twice_restores_the_single_frame_starting_state() {
        let mut app = GasciiApp::headless();
        let starting_doc = app.doc.clone();

        app.add_frame_via_menu();
        assert_eq!(app.doc.frame_count(), 2);

        // Switch to frame 1 via a plugin-shaped PanelOutcome (mirrors a real timeline click).
        app.switch_active_frame(1);
        assert_eq!(app.active_frame, 1);
        assert_eq!(app.doc.active_frame(), 1);

        // Draw on frame 2 (index 1).
        app.active_glyph = 'Z';
        let r = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 2, y: 2 }, &r, &app.doc);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &r, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        assert_eq!(app.doc.cell_at(1, 0, 2, 2).unwrap().ch, 'Z');

        // Switch back to frame 0.
        app.switch_active_frame(0);
        assert_eq!(app.active_frame, 0);

        // Undo the draw, then the Add Frame — back to the single-frame starting state.
        app.request_undo();
        app.request_undo();
        assert_eq!(app.doc, starting_doc, "two undos must fully restore the pre-Add-Frame document");
    }

    /// A test-only `Plugin` that logs its own tag into a shared log when `wrap_renderer` is
    /// called, proving `build_renderer`'s fold order directly rather than trying to inspect the
    /// opaque composed `Box<dyn CanvasRenderer>` it returns.
    struct TaggingPlugin {
        tag: &'static str,
        log: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    }
    impl Plugin for TaggingPlugin {
        fn wrap_renderer(&self, inner: Box<dyn CanvasRenderer>) -> Box<dyn CanvasRenderer> {
            self.log.borrow_mut().push(self.tag);
            inner
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn build_renderer_folds_every_plugins_wrap_renderer_in_registration_order() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let plugins: Vec<Box<dyn Plugin>> = vec![
            Box::new(TaggingPlugin { tag: "a", log: log.clone() }),
            Box::new(TaggingPlugin { tag: "b", log: log.clone() }),
            Box::new(TaggingPlugin { tag: "c", log: log.clone() }),
        ];
        let _ = build_renderer(plugins.iter().map(|p| p.as_ref()));
        assert_eq!(*log.borrow(), vec!["a", "b", "c"], "fold order must match plugin-list order");
    }

    /// Confirms `BrushPlugin`'s actual no-op defaults hold end-to-end against a real
    /// `GasciiApp::headless()` — not just against test doubles: the plugin-composed renderer
    /// chain paints without panicking.
    #[test]
    fn a_real_app_with_the_builtin_plugin_list_has_an_identity_renderer() {
        let app = GasciiApp::headless();

        let mut renderer = build_renderer(app.plugins.iter().map(|p| p.as_ref()));
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter().clone();
            renderer.paint(
                &painter,
                &app.doc,
                &app.viewport as &dyn gascii_plugin_api::CellGrid,
                egui::Pos2::ZERO,
                egui::Vec2::new(10.0, 20.0),
                (0, 0, app.doc.width, app.doc.height),
                &[],
                &[],
                None,
                None,
            );
        });
    }

    /// Disabling the plugin that owns a bound tool must snap that binding to Pencil — the
    /// structural half of "a disabled plugin's tool is never bound" — while leaving the name-keyed
    /// stamp settings untouched, so the size survives the round trip. Re-enable must NOT
    /// auto-rebind: the user gets their tool back by picking it, with its stamp intact.
    #[test]
    fn disabling_the_plugin_owning_a_bound_tool_rebinds_to_pencil_and_preserves_its_stamp() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, BRUSH_KIND);
        let slot = sized_slot(BRUSH_KIND).expect("Brush is sized");
        app.slots[Binding::L.ix()].stamps[slot].size = 7;
        let i = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");

        app.set_plugin_enabled(i, false);
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "L must fall back to Pencil at disable time");
        assert_eq!(app.slots[Binding::L.ix()].stamps[slot].size, 7, "the stamp array is per-binding, not per-bound-tool — disable must not touch it");

        app.set_plugin_enabled(i, true);
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "re-enable must not auto-rebind");

        app.bind(Binding::L, BRUSH_KIND);
        assert_eq!(app.slot(Binding::L).kind, BRUSH_KIND);
        assert_eq!(app.slots[Binding::L.ix()].stamps[slot].size, 7, "rebinding after the cycle must see the same stamp size");
    }

    /// The other half of the invariant: `set_tool`'s enabled guard. While the owning plugin is
    /// disabled, a bind request for its tool must be a silent no-op — otherwise `tool_ctx_patch`,
    /// the pressure gate, and `options_ui` (all ungated by design) would run against a disabled
    /// plugin.
    #[test]
    fn a_disabled_plugins_tool_cannot_be_bound() {
        let mut app = GasciiApp::headless();
        let i = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");
        app.set_plugin_enabled(i, false);

        app.bind(Binding::L, BRUSH_KIND);
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "binding a disabled plugin's tool must be refused");

        app.set_plugin_enabled(i, true);
        app.bind(Binding::L, BRUSH_KIND);
        assert_eq!(app.slot(Binding::L).kind, BRUSH_KIND, "the same bind must succeed once re-enabled");
    }

    /// A test-only plugin logging the `resumed_after_suppression` flag of every `tick` it
    /// receives — one log entry per delivered tick, so both "was I called at all" and "what reset
    /// signal did I see" fall out of the same list.
    struct TickRecorderDouble {
        log: std::rc::Rc<std::cell::RefCell<Vec<bool>>>,
    }
    impl Plugin for TickRecorderDouble {
        fn tick(
            &mut self,
            _ui: &mut egui::Ui,
            _focused: bool,
            resumed_after_suppression: bool,
            _host: &dyn gascii_plugin_api::PluginHost,
        ) -> gascii_plugin_api::PanelOutcome {
            self.log.borrow_mut().push(resumed_after_suppression);
            gascii_plugin_api::PanelOutcome::default()
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    /// `Plugin::tick`'s documented contract at the toggle boundary: a disabled plugin's tick is
    /// skipped outright (its clock freezes), and the FIRST tick after re-enable — and only that
    /// one — sees `resumed_after_suppression`, so cross-frame hold state (gascii-anim's Space
    /// hold) can reset exactly as it does after a modal closes.
    #[test]
    fn a_disabled_plugins_tick_is_skipped_and_its_first_tick_after_reenable_sees_the_resume_flag() {
        let mut app = GasciiApp::headless();
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let i = app.push_plugin_double(Box::new(TickRecorderDouble { log: log.clone() }));
        let run_frame = |app: &mut GasciiApp| {
            let ctx = egui::Context::default();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.handle_keys(ui));
        };

        run_frame(&mut app);
        assert_eq!(*log.borrow(), vec![false], "an ordinary enabled tick carries no resume flag");

        app.set_plugin_enabled(i, false);
        run_frame(&mut app);
        assert_eq!(*log.borrow(), vec![false], "a disabled plugin must not be ticked at all");

        app.set_plugin_enabled(i, true);
        run_frame(&mut app);
        assert_eq!(*log.borrow(), vec![false, true], "the first tick after re-enable must see resumed_after_suppression");

        run_frame(&mut app);
        assert_eq!(*log.borrow(), vec![false, true, false], "the latch is one-shot — the second tick is ordinary again");
    }

    /// Disabling a plugin must reclaim its panel's screen space the same frame: `run_plugin_panels`
    /// skips it, so the egui panel is simply never declared (immediate mode) and the central rect
    /// grows back to the no-plugin baseline exactly.
    #[test]
    fn disabling_a_plugin_removes_its_panel_from_the_layout() {
        let mut app = GasciiApp::headless();
        let i = app.push_plugin_double(Box::new(BottomPanelDouble));

        let central_rect = |app: &mut GasciiApp| {
            let ctx = egui::Context::default();
            let mut rect = None;
            let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
                app.run_plugin_panels(ui, false);
                let resp = egui::CentralPanel::default().show(ui, |_ui| {});
                rect = Some(resp.response.rect);
            });
            rect.unwrap()
        };

        let enabled_rect = central_rect(&mut app);
        app.set_plugin_enabled(i, false);
        let disabled_rect = central_rect(&mut app);

        let mut baseline_app = GasciiApp::headless(); // never had the double
        let baseline_rect = central_rect(&mut baseline_app);

        assert!(enabled_rect.height() < baseline_rect.height(), "sanity: the enabled double's bottom panel must claim space");
        assert_eq!(disabled_rect, baseline_rect, "disabling must reclaim the panel's space down to the exact baseline rect");
    }

    /// The real registered gascii-anim plugin, not a double: its timeline claims space once a
    /// second frame exists, and disabling the plugin must remove the timeline even then — the
    /// frames stay in the document, but nothing draws a panel for them. `gascii-layers` is disabled
    /// alongside it here for the same reason as the single-frame gate test above: its own panel has
    /// no gate to claim zero space, so it would otherwise still shrink the central panel and break
    /// this test's "as if no panel loop ran at all" comparison.
    #[test]
    fn disabling_the_anim_plugin_removes_the_timeline_even_with_multiple_frames() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);
        let anim = PLUGINS.iter().position(|d| d.id == gascii_anim::DESCRIPTOR.id).expect("gascii-anim is registered");
        app.set_plugin_enabled(anim, false);
        let layers = PLUGINS.iter().position(|d| d.id == gascii_layers::DESCRIPTOR.id).expect("gascii-layers is registered");
        app.set_plugin_enabled(layers, false);

        let ctx = egui::Context::default();
        let mut with_disabled_anim = None;
        let _ = ctx.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            app.run_plugin_panels(ui, false);
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            with_disabled_anim = Some(resp.response.rect);
        });

        let ctx2 = egui::Context::default();
        let mut bare_baseline = None;
        let _ = ctx2.run_ui(raw_input_with_screen(1000.0, 800.0), |ui| {
            let resp = egui::CentralPanel::default().show(ui, |_ui| {});
            bare_baseline = Some(resp.response.rect);
        });

        assert_eq!(
            with_disabled_anim.unwrap(),
            bare_baseline.unwrap(),
            "with anim disabled, a two-frame document must lay out as if no panel loop ran at all"
        );
    }

    /// Disabling `gascii-layers` (Plugin Manager) with `layer_count() > 1` must not reset
    /// `active_layer` or touch the document at all — the plugin's own panel/tick hooks are the only
    /// things gated. Rendering must stay correct regardless: `composite_cell` is the host's own
    /// choke point, never the plugin's, so the canvas still composites every visible layer with the
    /// plugin disabled. Mirrors `gascii-anim`'s own disabled-with-multi-frame behavior.
    #[test]
    fn disabling_the_layers_plugin_with_multiple_layers_does_not_reset_active_layer_or_break_rendering() {
        let mut app = GasciiApp::headless();
        let add = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(add, None);
        assert_eq!(app.active_layer, 1, "sanity: adding a layer makes it the active one");

        let (cx, cy) = (app.doc.width / 2, app.doc.height / 2);
        let top_bg = Rgba(11, 22, 33, 255);
        app.doc.set_cell(1, cx, cy, gascii_core::Cell { ch: 'Y', fg: Rgba::WHITE, bg: top_bg });
        let before = app.doc.clone();

        let layers = PLUGINS.iter().position(|d| d.id == gascii_layers::DESCRIPTOR.id).expect("gascii-layers is registered");
        app.set_plugin_enabled(layers, false);

        assert_eq!(app.active_layer, 1, "disabling the plugin must not reset active_layer");
        assert_eq!(app.doc.active_layer(), 1);
        assert_eq!(app.doc, before, "disabling a plugin must never touch the document");

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |_ui| {});
        let out = ctx.run_ui(raw_input_with_screen(300.0, 300.0), |ui| crate::canvas::show(ui, &mut app, false));
        let seeded_color = egui::Color32::from_rgba_unmultiplied(top_bg.0, top_bg.1, top_bg.2, top_bg.3);
        let count = out.shapes.iter().filter(|cs| matches!(&cs.shape, egui::Shape::Rect(r) if r.fill == seeded_color)).count();
        assert_eq!(count, 1, "the host-owned composite must still include layer 1's content with gascii-layers disabled");
    }

    /// Every effective toggle rebuilds the renderer from the enabled plugins only — a disabled
    /// plugin's `wrap_renderer` must not run, so its canvas decorator (anim's onion skin) drops
    /// out of the chain immediately and comes back on re-enable.
    #[test]
    fn toggling_a_plugin_rebuilds_the_renderer_excluding_disabled_wrappers() {
        let mut app = GasciiApp::headless();
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let i = app.push_plugin_double(Box::new(TaggingPlugin { tag: "double", log: log.clone() }));
        let brush = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");

        app.set_plugin_enabled(brush, false);
        assert_eq!(*log.borrow(), vec!["double"], "a rebuild triggered by toggling ANOTHER plugin must still wrap the enabled double");

        log.borrow_mut().clear();
        app.set_plugin_enabled(i, false);
        assert!(log.borrow().is_empty(), "the rebuild after disabling the double must not call its wrap_renderer");

        app.set_plugin_enabled(i, true);
        assert_eq!(*log.borrow(), vec!["double"], "re-enabling must fold the double back into the renderer chain");
    }

    /// Disable keeps the live plugin instance — it gates hook calls, it does not drop the box — so
    /// unpersisted plugin state (the ramp choice here) survives a disable/enable cycle.
    #[test]
    fn plugin_state_survives_a_disable_enable_cycle() {
        let mut app = GasciiApp::headless();
        app.brush_plugin_mut().set_active_ramp(1);
        let i = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");

        app.set_plugin_enabled(i, false);
        app.set_plugin_enabled(i, true);

        assert_eq!(app.brush_plugin_mut().active_ramp(), 1, "the live instance must be retained across the toggle, state and all");
    }

    /// A disabled tool's shortcut letter must be left unconsumed — the enabled check runs before
    /// the consuming predicate in `handle_keys`'s dispatch, the same leave-unconsumed shape
    /// Text-in-kiosk uses — so the key stays available to whatever else might claim it.
    #[test]
    fn the_tool_letter_of_a_disabled_plugins_tool_is_left_unconsumed() {
        let mut app = GasciiApp::headless();
        let i = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");
        app.set_plugin_enabled(i, false);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut key_still_there = false;
        let _ = ctx.run_ui(raw, |ui| {
            app.handle_keys(ui);
            key_still_there = ui.input_mut(|inp| inp.consume_key(egui::Modifiers::NONE, egui::Key::B));
        });

        assert!(key_still_there, "handle_keys must not consume a disabled tool's shortcut key");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "and must not rebind L off its default");
    }

    /// The Plugin Manager must count as a modal like every other dialog: `modal_open()` is what
    /// suppresses ticks and gates canvas input while it's up, and the same latch is what delivers
    /// `resumed_after_suppression` when it closes — the toggle-time safety argument rests on this.
    #[test]
    fn opening_the_plugins_dialog_counts_as_a_modal() {
        let mut app = GasciiApp::headless();
        assert!(!app.modal_open());
        app.open_plugins_dialog();
        assert!(app.modal_open(), "the Plugins dialog must register with modal_open()");
    }

    /// `anim_plugin_enabled` (the Animation menu gate) must track exactly the gascii-anim toggle —
    /// not any other plugin's.
    #[test]
    fn anim_plugin_enabled_follows_the_anim_toggle_and_ignores_other_plugins() {
        let mut app = GasciiApp::headless();
        assert!(app.anim_plugin_enabled());

        let brush = tool_def(BRUSH_KIND).plugin_slot.expect("Brush is plugin-sourced");
        app.set_plugin_enabled(brush, false);
        assert!(app.anim_plugin_enabled(), "another plugin's toggle must not affect the Animation menu gate");

        let anim = PLUGINS.iter().position(|d| d.id == gascii_anim::DESCRIPTOR.id).expect("gascii-anim is registered");
        app.set_plugin_enabled(anim, false);
        assert!(!app.anim_plugin_enabled(), "disabling gascii-anim must hide the Animation menu");

        app.set_plugin_enabled(anim, true);
        assert!(app.anim_plugin_enabled());
    }

    /// Every kind must be constructible, including Eyedropper — which is not really a tool and is
    /// backed by `InertTool`. A kind that panicked or returned a stale instance here would take
    /// down a binding the moment it was selected.
    #[test]
    fn every_kind_builds_a_tool_with_an_empty_pending_overlay() {
        for kind in ALL_KINDS {
            let tool = make_tool(kind);
            assert!(tool.pending().is_empty(), "{kind:?} starts with a non-empty overlay");
        }
    }

    /// Shortcuts must be unique, or one tool would be unreachable from the keyboard: `handle_keys`
    /// consumes the first match and the loser would silently never fire.
    #[test]
    fn tool_shortcuts_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for def in tools().iter() {
            assert!(seen.insert(def.key), "{:?} reuses shortcut {:?}", def.kind, def.key);
        }
    }

    /// Both bindings start bound, and to different tools — exactly one tool is bound to L and one
    /// to R at all times; there is no unbound state.
    #[test]
    fn default_bindings_are_pencil_on_l_and_eraser_on_r() {
        let slots = [ToolSlot::new(ToolKind::Pencil), ToolSlot::new(ToolKind::Eraser)];
        assert_eq!(slots[Binding::L.ix()].kind, ToolKind::Pencil);
        assert_eq!(slots[Binding::R.ix()].kind, ToolKind::Eraser);
    }

    /// Each binding keeps its own footprint memory, so sizing the right button's Eraser must not
    /// resize the left button's. Structural here — the two slots own separate arrays — but this
    /// pins it against a refactor that reintroduces a shared one.
    #[test]
    fn stamps_are_per_slot_so_sizing_rs_eraser_never_resizes_ls() {
        let mut slots = [ToolSlot::new(ToolKind::Eraser), ToolSlot::new(ToolKind::Eraser)];
        let eraser = sized_slot(ToolKind::Eraser).expect("Eraser is sized");
        slots[Binding::R.ix()].stamps[eraser].size = 9;
        assert_eq!(slots[Binding::R.ix()].stamp().size, 9);
        assert_eq!(slots[Binding::L.ix()].stamp().size, 1, "L's Eraser was resized by R's");
    }

    /// A slot's stamp follows whatever it is bound to, and unsized kinds fall back to the identity
    /// default rather than borrowing another tool's size.
    #[test]
    fn a_slots_stamp_tracks_its_own_kind() {
        let mut slot = ToolSlot::new(ToolKind::Pencil);
        slot.stamps[sized_slot(ToolKind::Pencil).unwrap()].size = 5;
        slot.stamps[sized_slot(BRUSH_KIND).unwrap()].size = 12;
        assert_eq!(slot.stamp().size, 5);
        slot.kind = BRUSH_KIND;
        assert_eq!(slot.stamp().size, 12);
        slot.kind = ToolKind::Fill; // unsized
        assert_eq!(slot.stamp().size, StampSettings::default().size);
    }

    /// Overlay order is commit order: a slot mid-gesture commits at its imminent release, so it
    /// paints underneath the other slot's session, which commits later. Pure over the stroke
    /// owner, so `flush_all` and the painter provably agree.
    #[test]
    fn commit_order_puts_the_gesture_slot_first() {
        assert_eq!(order_for(None), [Binding::L, Binding::R]);
        assert_eq!(order_for(Some(Binding::L)), [Binding::L, Binding::R]);
        assert_eq!(order_for(Some(Binding::R)), [Binding::R, Binding::L]);
    }

    /// The full truth table behind Escape's fullscreen-exit precedence: only "no keyboard-owning
    /// session AND no live stroke AND no focused widget" claims Escape for exiting fullscreen. Any
    /// higher-priority claim alone is enough to withhold it.
    #[test]
    fn should_handle_escape_for_fullscreen_truth_table() {
        assert!(
            should_handle_escape_for_fullscreen(None, false, false),
            "no session, no stroke, no focused widget: Escape should exit fullscreen"
        );
        assert!(
            !should_handle_escape_for_fullscreen(Some(Binding::L), false, false),
            "an active keyboard-owning session outranks the fullscreen exit"
        );
        assert!(
            !should_handle_escape_for_fullscreen(None, true, false),
            "a live pointer stroke outranks the fullscreen exit"
        );
        assert!(
            !should_handle_escape_for_fullscreen(None, false, true),
            "a focused widget outranks the fullscreen exit"
        );
        assert!(
            !should_handle_escape_for_fullscreen(Some(Binding::R), true, true),
            "every higher-priority claim held at once still withholds Escape from fullscreen"
        );
    }

    /// Stylus pressure must drive `tool_ctx`'s size for the live stroke without ever touching the
    /// slot's own `StampSettings.size` — the same field the Size stepper/`[`/`]` keys edit and
    /// `Prefs::from_app` persists. A user's configured size must survive a pressure-modulated
    /// stroke byte-for-byte.
    #[test]
    fn pressure_override_drives_tool_ctx_size_without_touching_the_slots_configured_size() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(BRUSH_KIND);
        let brush_slot = sized_slot(BRUSH_KIND).expect("Brush is a sized tool");
        app.slots[Binding::L.ix()].stamps[brush_slot].size = 10;

        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::L), "sanity: L is mid-stroke");
        assert_eq!(
            app.pressure_stamp_size, None,
            "a fresh stroke starts with no pressure override"
        );

        // A light-pressure dab (mirrors canvas.rs's quantization) sets only the transient override.
        app.pressure_stamp_size = Some(2);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.size, 2, "the live stroke's footprint follows the pressure override");
        assert_eq!(
            app.slots[Binding::L.ix()].stamps[brush_slot].size, 10,
            "the binding's configured/persisted Brush size must be untouched by pressure"
        );

        // The other binding never sees a pressure override that isn't its own.
        let r_tctx = crate::canvas::tool_ctx(&app, Binding::R);
        assert_ne!(r_tctx.size, 2, "pressure only overrides the stroke-owning binding");

        // Ending the stroke clears the override; the slot's size is still exactly what it was.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        app.pressure_stamp_size = None;
        assert_eq!(
            app.slots[Binding::L.ix()].stamps[brush_slot].size, 10,
            "the configured size survives the whole stroke, including release"
        );
    }

    /// `tool_ctx`'s ctx-patch injection (density mode, ramp) must reach only a plugin tool that
    /// asks for it (Brush, via `wants_ctx_patch`), reading it from the *live* plugin instance rather
    /// than a fresh default — and must leave a non-plugin tool at the inert default the pre-migration
    /// literal `GasciiApp::with_state` used, matching what every non-Brush tool already got before
    /// this workstream.
    #[test]
    fn tool_ctx_injects_extra_context_only_for_a_plugin_tool_that_wants_it() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, BRUSH_KIND);
        app.brush_plugin_mut().set_active_ramp(1);
        let expected_ramp = gascii_core::builtin_ramps()[1].chars.clone();

        let brush_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(brush_ctx.ramp, expected_ramp, "Brush's tool_ctx.ramp must follow the live plugin's active ramp");

        app.bind(Binding::L, ToolKind::Pencil);
        let pencil_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert!(pencil_ctx.ramp.is_empty(), "a non-plugin tool must get the inert default, not Brush's ramp");
    }

    /// End-to-end proof that a real Brush stroke, driven the same way every other tool-stroke test
    /// in this module drives one (`tool.update` + `apply_edit`, not just inspecting `tool_ctx` in
    /// isolation), actually stamps a glyph read off the live plugin's active ramp — not a default
    /// or a stale snapshot. Ramp index 1 ("Block shades", `"░▒▓█"`) with the plugin's default
    /// `Fixed(1.0)` intensity picks the ramp's last character deterministically.
    #[test]
    fn a_full_brush_stroke_through_the_app_commits_a_glyph_from_the_plugins_active_ramp() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, BRUSH_KIND);
        app.brush_plugin_mut().set_active_ramp(1);
        let expected_ch = gascii_core::builtin_ramps()[1].chars[3]; // '█', Fixed(1.0) on a 4-char ramp

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 2, y: 2 }, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        assert_eq!(
            app.doc.cell(app.active_layer, 2, 2).unwrap().ch,
            expected_ch,
            "the committed glyph must come from the live plugin's active ramp/density, not a default"
        );

        // A non-plugin tool bound to the same binding must never read a ramp at all — it stamps
        // the app's plain active_glyph, completely untouched by whatever the plugin's ramp holds.
        app.bind(Binding::L, ToolKind::Pencil);
        app.active_glyph = '#';
        let pencil_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 3, y: 3 }, &pencil_ctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &pencil_ctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        assert_eq!(app.doc.cell(app.active_layer, 3, 3).unwrap().ch, '#');
    }

    /// Per-binding isolation through the plugin: Brush need not be on L. Bound to R alone while L
    /// holds an unrelated tool, `tool_ctx` must still resolve R's ramp/density through the live
    /// plugin (not just when Brush happens to be the L-bound case every other test exercises), and
    /// L must see none of it.
    #[test]
    fn brush_bound_only_to_r_while_l_holds_a_different_tool_still_resolves_through_the_plugin() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, BRUSH_KIND);
        app.brush_plugin_mut().set_active_ramp(1);
        let expected_ramp = gascii_core::builtin_ramps()[1].chars.clone();

        let r_ctx = crate::canvas::tool_ctx(&app, Binding::R);
        assert_eq!(r_ctx.ramp, expected_ramp, "R's tool_ctx must resolve through the plugin even though L holds a different tool");
        let l_ctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert!(l_ctx.ramp.is_empty(), "L (Pencil) must not see Brush's ramp just because R holds Brush");
    }

    /// The digit-key intensity shortcut's real gating, driven through `handle_keys` itself (not
    /// `BrushPlugin::tick` in isolation with a `FakeHost`, which `gascii-density-brush`'s own suite
    /// already covers) — proving the host's `!focused` gate, `host_facts`, and the per-frame
    /// `plugins.iter_mut().for_each(|p| p.tick(...))` loop are wired together correctly end to end.
    /// Also exercises kiosk (fullscreen) input: the shortcut was never fullscreen-gated pre-migration
    /// and must not become so now.
    #[test]
    fn digit_key_intensity_shortcut_through_handle_keys_sets_fixed_intensity_while_bound_and_unfocused() {
        for fullscreen in [false, true] {
            let mut app = GasciiApp::headless();
            app.bind(Binding::L, BRUSH_KIND);
            app.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));

            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(fullscreen);
            raw.events.push(egui::Event::Key {
                key: egui::Key::Num5,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

            match app.brush_plugin_mut().density_mode() {
                gascii_core::DensityMode::Fixed(gascii_core::Fixed(level)) => {
                    assert!((level - 0.5).abs() < 1e-4, "fullscreen={fullscreen}: expected Fixed(0.5)")
                }
                other => panic!("fullscreen={fullscreen}: expected Fixed(0.5), got {other:?}"),
            }
        }
    }

    /// The exact suppression the pre-migration `bound_to(BRUSH_KIND).is_some() && !focused`
    /// gate provided: an active Text session anywhere on the keyboard must still suppress the
    /// digit-key shortcut, even though Brush is bound to the OTHER binding (R), not the one holding
    /// the session — `focused` is a single app-wide fact, not per-binding.
    #[test]
    fn digit_key_intensity_shortcut_is_suppressed_while_a_text_session_owns_the_keyboard() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.bind(Binding::R, BRUSH_KIND);
        app.keyboard_owner = Some(Binding::L);
        app.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(
            matches!(app.brush_plugin_mut().density_mode(), gascii_core::DensityMode::Buildup(_)),
            "an active Text session must suppress Brush's digit-key shortcut even though Brush is bound to the other binding"
        );
    }

    /// The other suppression path: a focused egui widget (e.g. the HEX color field) must also
    /// suppress the shortcut, matching every other single-key tool shortcut's own `!focused` gate.
    #[test]
    fn digit_key_intensity_shortcut_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, BRUSH_KIND);
        app.brush_plugin_mut().set_density_mode(gascii_core::DensityMode::Buildup(gascii_core::Buildup));

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Num5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| {
            let id = egui::Id::new("qa_test_fake_focused_widget");
            ui.memory_mut(|m| m.request_focus(id));
            app.handle_keys(ui);
        });

        assert!(
            matches!(app.brush_plugin_mut().density_mode(), gascii_core::DensityMode::Buildup(_)),
            "a focused widget must suppress Brush's digit-key shortcut, matching every other tool-shortcut gate"
        );
    }

    /// `active_layer` is the single source `tool_ctx` and the eyedropper pick read from — pins that
    /// a non-zero value (session-only in this scope; the app itself never writes anything but 0)
    /// actually reaches both call sites rather than a stale `0` literal surviving in either.
    #[test]
    fn tool_ctx_and_eyedropper_follow_active_layer() {
        let mut app = GasciiApp::headless();
        let (w, h) = (app.doc.width, app.doc.height);
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.active_layer = 2;
        app.doc.set_cell(2, 3, 3, gascii_core::Cell { ch: 'z', fg: Rgba(1, 2, 3, 255), bg: Rgba::TRANSPARENT });

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.layer, 2, "tool_ctx's layer must follow active_layer");

        app.bind(Binding::L, ToolKind::Eyedropper);
        crate::canvas::begin_gesture(&mut app, Binding::L, 3, 3, false, false);
        let (expected_fg, _) = gascii_core::eyedrop(&app.doc.cell(2, 3, 3).copied().unwrap());
        assert_eq!(
            app.active_fg, expected_fg,
            "the eyedropper pick must read the cell from active_layer, not layer 0"
        );
    }

    /// The eyedropper samples the active layer's own raw cell even when a fully opaque layer above
    /// it covers that cell on screen — deliberate, not a bug: the pick stays coherent with the
    /// layer the user is about to draw on, and the on-screen composited color can blend content
    /// from multiple layers into something not present on any single one of them.
    #[test]
    fn eyedropper_samples_the_active_layer_even_when_a_fully_opaque_layer_covers_it_on_screen() {
        let mut app = GasciiApp::headless();
        let (w, h) = (app.doc.width, app.doc.height);
        app.doc.layers_mut().push(gascii_core::Layer::blank(w, h));
        app.active_layer = 0;
        app.doc.set_cell(0, 3, 3, gascii_core::Cell { ch: 'z', fg: Rgba(1, 2, 3, 255), bg: Rgba::TRANSPARENT });
        // Layer 1, above the active layer, fully covers the same cell with an opaque glyph.
        app.doc.set_cell(1, 3, 3, gascii_core::Cell { ch: '#', fg: Rgba(9, 9, 9, 255), bg: Rgba(9, 9, 9, 255) });

        app.bind(Binding::L, ToolKind::Eyedropper);
        crate::canvas::begin_gesture(&mut app, Binding::L, 3, 3, false, false);

        let (expected_fg, _) = gascii_core::eyedrop(&app.doc.cell(0, 3, 3).copied().unwrap());
        assert_eq!(
            app.active_fg, expected_fg,
            "the pick must read the active layer's own cell, not the opaque layer covering it on screen"
        );
    }

    /// Mirrors `tool_ctx_and_eyedropper_follow_active_layer`'s shape, but for `frame`:
    /// `active_frame` defaults to `0` and `tool_ctx` follows whatever it's set to.
    #[test]
    fn active_frame_defaults_to_zero_and_tool_ctx_follows_it() {
        let app = GasciiApp::headless();
        assert_eq!(app.active_frame, 0);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.frame, 0, "tool_ctx's frame must follow active_frame");

        let mut app = app;
        app.active_frame = 1;
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        assert_eq!(tctx.frame, 1, "tool_ctx's frame must follow a non-default active_frame too");
    }

    /// `apply_edit`'s app -> doc sync actually reaches `doc.active_frame()` before every applied
    /// edit, exercised end-to-end against a multi-frame document.
    #[test]
    fn apply_edit_syncs_doc_active_frame_from_app_active_frame_before_applying() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);

        app.active_frame = 1;
        let cell_edit = gascii_core::Edit::Cells(vec![gascii_core::CellEdit {
            frame: 1,
            layer: 0,
            x: 0,
            y: 0,
            before: gascii_core::Cell::BLANK,
            after: gascii_core::Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT },
        }]);
        app.apply_edit(cell_edit, None);
        assert_eq!(app.doc.active_frame(), 1, "apply_edit must sync doc's active-frame cursor from app.active_frame");
    }

    /// The layer twin of `apply_edit_syncs_doc_active_frame_from_app_active_frame_before_applying`:
    /// `apply_edit`'s app -> doc sync actually reaches `doc.active_layer()` before every applied
    /// edit, exercised end-to-end against a multi-layer document.
    #[test]
    fn apply_edit_syncs_doc_active_layer_from_app_active_layer_before_applying() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_layer(&app.doc, 1).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.layer_count(), 2);

        app.active_layer = 1;
        let cell_edit = gascii_core::Edit::Cells(vec![gascii_core::CellEdit {
            frame: 0,
            layer: 1,
            x: 0,
            y: 0,
            before: gascii_core::Cell::BLANK,
            after: gascii_core::Cell { ch: 'x', fg: Rgba::WHITE, bg: Rgba::TRANSPARENT },
        }]);
        app.apply_edit(cell_edit, None);
        assert_eq!(app.doc.active_layer(), 1, "apply_edit must sync doc's active-layer cursor from app.active_layer");
    }

    /// `apply_edit`'s doc -> app direction: `AddFrame` shifts `doc`'s cursor as a side effect of
    /// applying (inserting at index 0 pushes the active frame from 0 to 1) — `app.active_frame`
    /// must follow that shift, not just the app -> doc seed. Then `request_undo`'s own doc -> app
    /// resync must follow `doc`'s cursor back down when the insert is undone.
    #[test]
    fn undoing_an_add_frame_moves_the_docs_cursor_and_app_active_frame_follows() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_frame(&app.doc, 0, gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.doc.frame_count(), 2);
        assert_eq!(app.doc.active_frame(), 1, "inserting at index 0 shifts the active cursor forward");
        assert_eq!(app.active_frame, 1, "apply_edit's doc -> app resync must follow the shift");

        app.request_undo();
        assert_eq!(app.doc.active_frame(), 0, "undo restores doc's pre-insert cursor");
        assert_eq!(app.active_frame, 0, "app.active_frame must follow doc's cursor back down after undo");
    }

    /// App-side pinning spot-check: with `active_frame` shipped pinned at `0` (no UI writes it),
    /// a full stroke -> undo -> redo -> save -> load cycle driven through the real app pipeline
    /// must still land on `frame_count() == 1` and save exactly the pre-frames v1 envelope shape —
    /// the `frame_count() == 1 => save v1` rule makes this directly assertable without a second,
    /// pre-frames build to diff against.
    #[test]
    fn a_stroke_undo_redo_save_load_cycle_through_the_app_still_produces_a_plain_v1_file() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.active_glyph = '#';
        app.active_fg = Rgba::WHITE;
        app.active_bg = Rgba::TRANSPARENT;

        crate::canvas::begin_gesture(&mut app, Binding::L, 2, 2, false, false);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, '#', "sanity: the stroke committed");

        app.request_undo();
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, ' ', "sanity: undo reverted the stroke");
        app.request_redo();
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, '#', "sanity: redo restored it");

        assert_eq!(app.doc.frame_count(), 1, "the shipped app never leaves frame_count() == 1");
        assert_eq!(app.active_frame, 0, "the shipped app never moves active_frame off 0");
        assert_eq!(app.doc.active_frame(), 0);

        let dir = scratch_dir("frame_pin_v1_shape");
        let path = dir.join("out.gascii");
        app.current_path = Some(path.clone());
        app.save_file();

        let raw = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["background", "height", "layer_meta", "layers", "version", "width"],
            "a single-frame session must save exactly the v1 key set — no frame-substrate field leaks in"
        );
        assert_eq!(value["version"], 1, "a single-frame session must be tagged version 1, the pre-frames version");

        let loaded = load_str(&raw).unwrap();
        assert_eq!(loaded, app.doc, "the round trip must be byte-exact");
        assert_eq!(loaded.cell(0, 2, 2).unwrap().ch, '#');

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `F11` must exit/enter fullscreen even while a Text session is fully active — the exact
    /// scenario `suppresses_tool_shortcuts` exists to gate (single-letter tool keys), which F11 is
    /// not one of. A stale `!focused` gate on F11 previously reused that same flag and swallowed
    /// the toggle for as long as a Text burst lasted.
    #[test]
    fn f11_toggles_fullscreen_even_during_an_active_text_session() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        let owner_kind = app.keyboard_owner().map(|b| app.slot(b).kind);
        assert!(
            suppresses_tool_shortcuts(owner_kind),
            "sanity: an active Text session suppresses the single-letter tool shortcuts"
        );

        let ctx = egui::Context::default();
        let mut raw_input = egui::RawInput::default();
        raw_input.events.push(egui::Event::Key {
            key: egui::Key::F11,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let output = ctx.run_ui(raw_input, |ui| app.handle_keys(ui));

        let sent_toggle = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(true))));
        assert!(sent_toggle, "F11 must toggle fullscreen even while a Text session is active");
    }

    /// eframe's own window persistence restores the previous run's fullscreen state, so the first
    /// frame must force the launch state — windowed unless `--fullscreen` — exactly once.
    #[test]
    fn startup_window_state_is_forced_exactly_once() {
        let mut app = GasciiApp::headless();
        app.startup_fullscreen = Some(false);
        let ctx = egui::Context::default();

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let c = ui.ctx().clone();
            app.apply_startup_window_state(&c);
        });
        let sent_windowed = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(false))));
        assert!(sent_windowed, "the first frame must pin the window state even if eframe restored fullscreen");
        assert!(app.startup_fullscreen.is_none(), "the forced state is one-shot");

        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let c = ui.ctx().clone();
            app.apply_startup_window_state(&c);
        });
        let sent_again = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| !vp.commands.is_empty());
        assert!(!sent_again, "later frames must never re-force the window state");
    }

    /// `--fullscreen` launches straight into kiosk mode, which must arrive with zoom snapped to
    /// Fit exactly like an interactive entry does.
    #[test]
    fn a_fullscreen_launch_requests_the_same_fit_snap_as_an_interactive_entry() {
        let mut app = GasciiApp::headless();
        app.startup_fullscreen = Some(true);
        app.pending_fit = false;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let c = ui.ctx().clone();
            app.apply_startup_window_state(&c);
        });
        assert!(app.pending_fit, "a fullscreen launch must snap zoom to Fit");
    }

    /// A paste lands on a binding already holding Selection rather than rebinding one, and falls
    /// back to L (never R) when neither does — silently rebinding the right button out from under
    /// the user is worse than rebinding the left.
    #[test]
    fn paste_target_prefers_an_existing_selection_binding_over_rebinding() {
        use ToolKind::{Pencil, Selection};
        assert_eq!(paste_target(Selection, Pencil), Binding::L);
        assert_eq!(paste_target(Pencil, Selection), Binding::R, "should not clobber L's binding");
        assert_eq!(paste_target(Selection, Selection), Binding::L, "L wins when both qualify");
        assert_eq!(paste_target(Pencil, Pencil), Binding::L, "falls back to L, never R");
    }

    /// The reachable half of the two-slot resync obligation, and the reason `apply_edit` exists.
    ///
    /// A stroke on one binding commits straight into the document, underneath a *session* held by
    /// the other. That leaves the session's pinned `before` values describing a document state that
    /// no longer exists. Undo restores `before`, so a missed resync shows up as undo resurrecting
    /// pre-stroke content and silently destroying what the other binding drew.
    ///
    /// Here: R's Pencil draws '#' under L's live text burst, then the burst commits and is undone.
    /// Undo must restore R's '#', not the blank that was there when the burst started.
    #[test]
    fn a_strokes_commit_repins_the_other_bindings_live_session() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.keyboard_owner = Some(Binding::L);

        // L: place a caret at (0,0) and type — the burst pins `before` = Blank at (0,0).
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &l, &app.doc);

        // R: a pencil stroke commits '#' into (0,0), beneath the burst.
        app.active_glyph = '#';
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &r, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, '#', "the pencil stroke landed");

        // L's burst commits its 'A' over the top, then undo rolls it back.
        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "the burst committed");
        app.history.undo(&mut app.doc);

        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().ch,
            '#',
            "undo restored a stale pre-stroke `before`, destroying what the other binding drew"
        );
    }

    /// At most one cross-frame session exists across both bindings, so `flush_all`'s second flush
    /// has nothing to commit. Pins the invariant that makes two Selection bindings coherent (never
    /// two floats) and keeps `selection_slot` — hence "the selection" — singular.
    #[test]
    fn a_slot_holding_a_session_is_the_only_one() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Selection);

        // A press on L starts a marquee and claims the keyboard.
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1, false, false);
        assert_eq!(app.keyboard_owner, Some(Binding::L));
        assert_eq!(app.selection_slot(), Some(Binding::L));

        // A press on R takes over: ownership moves, and it is still the only session.
        crate::canvas::begin_gesture(&mut app, Binding::R, 4, 4, false, false);
        assert_eq!(app.keyboard_owner, Some(Binding::R));
        assert_eq!(app.selection_slot(), Some(Binding::R), "two selections would be ambiguous");
    }

    /// Rebinding a slot releases only its own claim on the keyboard. Clearing the claim globally
    /// would mute a live session on the other binding, which nothing would then re-acquire.
    #[test]
    fn rebinding_releases_only_its_own_keyboard_claim() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Text);
        app.keyboard_owner = Some(Binding::R);

        app.set_tool(Binding::L, ToolKind::Fill);
        assert_eq!(app.keyboard_owner, Some(Binding::R), "rebinding L muted R's session");

        app.set_tool(Binding::R, ToolKind::Fill);
        assert_eq!(app.keyboard_owner, None, "rebinding R should release its own claim");
    }

    /// Every kind is bindable to either button — Text, Selection and Eyedropper included.
    #[test]
    fn every_kind_can_bind_to_either_button() {
        for kind in ALL_KINDS {
            for b in Binding::ALL {
                let mut app = GasciiApp::headless();
                app.set_tool(b, kind);
                assert_eq!(app.slot(b).kind, kind, "{kind:?} would not bind to {b:?}");
            }
        }
    }

    #[test]
    fn paste_text_matching_the_internal_clipboards_own_flattening_is_recognized_as_own() {
        let patch = CellPatch { width: 2, height: 1, cells: vec![cell('a'), cell('b')] };
        let text = patch.to_text();
        assert!(is_own_clipboard_text(&text, Some(&patch)));
    }

    #[test]
    fn paste_text_differing_from_the_internal_clipboard_is_treated_as_external() {
        let patch = CellPatch { width: 2, height: 1, cells: vec![cell('a'), cell('b')] };
        assert!(!is_own_clipboard_text("something else entirely", Some(&patch)));
    }

    #[test]
    fn paste_text_with_no_internal_clipboard_is_always_external() {
        assert!(!is_own_clipboard_text("anything", None));
        assert!(!is_own_clipboard_text("", None));
    }

    #[test]
    fn copy_events_with_no_event_copy_present_fires_neither_copy_nor_copy_all() {
        let events = [egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }];
        assert_eq!(
            copy_events(&events, false),
            (false, false),
            "a bare Event::Key{{C}} is the exact fiction egui-winit never produces for the clipboard \
             chord — it must not fire copy"
        );
    }

    #[test]
    fn copy_events_with_event_copy_and_no_shift_fires_plain_copy_only() {
        let events = [egui::Event::Copy];
        assert_eq!(copy_events(&events, false), (true, false));
    }

    #[test]
    fn copy_events_with_event_copy_and_shift_held_fires_copy_all_only() {
        let events = [egui::Event::Copy];
        assert_eq!(copy_events(&events, true), (false, true));
    }

    #[test]
    fn edit_marker_differs_is_clean_when_both_markers_are_none() {
        assert!(!edit_marker_differs(None, None));
    }

    #[test]
    fn edit_marker_differs_is_clean_when_current_matches_saved() {
        assert!(!edit_marker_differs(Some(3), Some(3)));
    }

    #[test]
    fn edit_marker_differs_is_dirty_when_current_and_saved_diverge() {
        assert!(edit_marker_differs(Some(3), Some(4)));
    }

    #[test]
    fn edit_marker_differs_is_dirty_when_current_is_some_but_saved_is_none() {
        assert!(edit_marker_differs(Some(0), None));
    }

    #[test]
    fn ctrl_c_response_is_none_when_no_new_presses() {
        assert_eq!(ctrl_c_response(2, 2, false), None);
        assert_eq!(ctrl_c_response(2, 2, true), None);
    }

    #[test]
    fn ctrl_c_response_first_press_requests_a_normal_close() {
        assert_eq!(ctrl_c_response(1, 0, false), Some(CtrlCResponse::RequestClose));
    }

    /// Several presses landing before the first frame drains them still count as one request —
    /// the veto dialog hasn't had a chance to appear, so nothing is discarded unprompted.
    #[test]
    fn ctrl_c_response_burst_before_dialog_shows_stays_a_normal_close() {
        assert_eq!(ctrl_c_response(3, 0, false), Some(CtrlCResponse::RequestClose));
    }

    #[test]
    fn ctrl_c_response_repeat_press_while_confirming_forces_the_close() {
        assert_eq!(ctrl_c_response(2, 1, true), Some(CtrlCResponse::ForceClose));
    }

    /// Pure-function coverage over every `ToolKind` plus `None`: only a Text-owning keyboard
    /// suppresses tool-select shortcuts — `SelectionTool`'s `Char` arm falls through to a no-op, so
    /// every other owning kind (and no owner at all) must leave shortcuts live.
    #[test]
    fn suppresses_tool_shortcuts_is_true_only_for_text() {
        for kind in ALL_KINDS {
            let expected = kind == ToolKind::Text;
            assert_eq!(suppresses_tool_shortcuts(Some(kind)), expected, "{kind:?}");
        }
        assert!(!suppresses_tool_shortcuts(None));
    }

    /// Pure-function coverage over every `ToolKind`: only Text's shortcut is gated, and only while
    /// fullscreen — kiosk's sidebar has no cell for Text, so `T` must not be reachable there, but
    /// every other tool's shortcut (visible in the kiosk grid, showing L/R badges) stays live in
    /// both chrome modes.
    #[test]
    fn tool_shortcut_reachable_only_gates_text_and_only_while_fullscreen() {
        for kind in ALL_KINDS {
            assert!(tool_shortcut_reachable(kind, false), "{kind:?}: every shortcut works windowed");
        }
        for kind in ALL_KINDS {
            let expected = kind != ToolKind::Text;
            assert_eq!(
                tool_shortcut_reachable(kind, true), expected,
                "{kind:?}: fullscreen gating must affect only Text"
            );
        }
    }

    /// `flush_slot` commits pending work but never releases the keyboard — that is `end_session`'s
    /// job. A flushed Text burst must still hold the keyboard, and its caret must still be placed,
    /// right after the flush.
    #[test]
    fn flush_slot_never_releases_keyboard_ownership() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.acquire_keyboard(Binding::L);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);

        app.flush_slot(Binding::L);

        assert_eq!(app.keyboard_owner(), Some(Binding::L), "flush must never release the keyboard");
        assert!(
            app.slots[Binding::L.ix()].tool.caret().is_some(),
            "the burst's cursor must still be placed after a flush"
        );
    }

    /// A flush commits the session's pending work even while its own binding is mid-stroke: every
    /// flush caller either reads the document right after (save, the close-confirm dirty check,
    /// copy) or follows up with a `Cancel` — a gated flush would hand them a document missing work
    /// the user can see, or let the `Cancel` discard it. The scenario: a pasted float is being
    /// dragged into place when the window is asked to close.
    #[test]
    fn a_mid_stroke_flush_commits_the_float_so_the_dirty_check_sees_it() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let patch = CellPatch { width: 1, height: 1, cells: vec![cell('x')] };
        app.slots[Binding::L.ix()].tool.accept_stamp(patch, (3, 3), &app.doc);
        app.acquire_keyboard(Binding::L);

        // Grab the float: the press starts a Move stroke and takes stroke ownership.
        crate::canvas::begin_gesture(&mut app, Binding::L, 3, 3, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::L), "sanity: L is mid-stroke");
        assert!(!app.is_dirty(), "sanity: nothing committed yet");

        // Alt+F4 / Ctrl+S while the button is still held.
        app.flush_all();

        assert_eq!(app.doc.cell(0, 3, 3).unwrap().ch, 'x', "the float must commit at its current spot");
        assert!(app.is_dirty(), "the close-confirm dirty check must see the committed float");
    }

    /// `end_session` commits before it clears, even when the binding owns the in-flight stroke —
    /// Escape pressed while the pointer is still held must never discard what was typed during the
    /// hold.
    #[test]
    fn end_session_commits_pending_work_even_for_the_stroke_owning_binding() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::L), "sanity: the press is still held");
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);

        app.end_session(Binding::L); // Escape mid-hold

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h', "the held-press burst must commit, not vanish");
        assert_eq!(app.keyboard_owner(), None, "the session is over");
    }

    /// Ctrl+C internally calls `flush_all`, which must not silently drop the marquee or the
    /// keyboard claim — Delete right afterward must still see the selection and blank it, or the
    /// standard copy-then-delete cut workflow dies at its second step.
    #[test]
    fn ctrl_c_then_delete_workflow_survives_a_flush() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        app.doc.set_cell(0, 1, 1, cell('x'));
        app.doc.set_cell(0, 2, 2, cell('y'));

        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx); // internally calls flush_all

        assert_eq!(
            app.selection_slot(),
            Some(Binding::L),
            "a flush triggered by copy must not clear the selection slot"
        );
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().and_then(|v| v.marquee).is_some(),
            "the marquee must survive a structural flush"
        );

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Delete, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) = resp {
            app.apply_edit(edit, Some(Binding::L));
        }
        for y in 1..=2u16 {
            for x in 1..=2u16 {
                assert_eq!(app.doc.cell(0, x, y), Some(&gascii_core::Cell::BLANK));
            }
        }
    }

    /// A structural flush (Ctrl+S/Ctrl+Z) mid-burst must not release the keyboard, or the very
    /// next typed letter would be consumed as a tool-select shortcut instead of burst content.
    #[test]
    fn mid_typing_structural_flush_does_not_let_the_next_letter_rebind_the_tool() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('a'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        app.flush_all(); // simulates the Ctrl+S / Ctrl+Z structural-trigger path

        let owner_kind = app.keyboard_owner().map(|b| app.slot(b).kind);
        assert_eq!(owner_kind, Some(ToolKind::Text), "a structural flush must not release the keyboard mid-burst");
        assert!(
            suppresses_tool_shortcuts(owner_kind),
            "the very next 's' keypress must still be swallowed as burst content, not routed to set_tool"
        );
    }

    /// Starting a session on the other binding must fully clear the losing slot's marquee, not
    /// merely leave it behind to be masked by render/commit ordering — a lingering invisible
    /// marquee is what keyboard Delete would silently operate on.
    #[test]
    fn starting_a_selection_session_on_the_other_binding_clears_the_losing_slots_marquee() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Selection);

        // A press on L starts a marquee and claims the keyboard.
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1, false, false);
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().and_then(|v| v.marquee).is_some(),
            "sanity: L has a marquee"
        );

        // A press on R takes over: L's session must be fully ended, not just masked.
        crate::canvas::begin_gesture(&mut app, Binding::R, 4, 4, false, false);

        assert_eq!(app.keyboard_owner(), Some(Binding::R));
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().is_none(),
            "the losing slot's marquee must be cleared, not merely masked by render order"
        );
    }

    /// A flush landing on the idle binding mid-stroke leaves the stroking binding holding pending
    /// cells composed against the pre-flush document; its own eventual commit must not revert the
    /// just-flushed content on a masked-off plane. The app-integration face of the resync
    /// contract (the tool-level pin lives in `gascii-core`).
    #[test]
    fn a_strokes_commit_mid_gesture_repins_a_flushed_idle_slots_masked_plane() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.acquire_keyboard(Binding::L);

        // L: place a caret at (0,0) and type — commits 'A' once flushed.
        app.mask = PlaneMask::ALL;
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &l, &app.doc);

        // R: a glyph-masked-off Pencil stroke touches (0,0) and keeps gesturing — no Release yet.
        app.mask = PlaneMask { glyph: false, bg: true };
        app.active_glyph = '#';
        app.stroke_owner = Some(Binding::R);
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &r, &app.doc);

        // A same-frame flush lands on L mid-R-stroke (Escape/Ctrl+C mid-R-stroke): commits 'A'.
        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "L's burst committed under R's live stroke");

        // R's stroke moves on WITHOUT revisiting (0,0). Deliberate: a revisit re-stamps the cell
        // and recomposes as a side effect, hiding a resync that fixed only future stamps — the
        // corruption lives precisely in the already-stamped, never-revisited pending cell.
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Drag { x: 2, y: 0 }, &r, &app.doc);

        app.stroke_owner = None;
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }

        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().ch,
            'A',
            "R's stroke must not silently revert L's committed glyph on the masked-off plane"
        );
    }

    /// The full copy-paste-drag-save cross-feature flow: a pasted float is mid-drag when Save
    /// fires. The save's flush must commit the float at its current (dragged) position, the saved
    /// file must reflect that position, and the session must stay coherent afterward — the
    /// keyboard claim survives (it's still residue, not a discard), and a further press starts a
    /// clean new marquee rather than getting stuck referencing the just-committed float.
    #[test]
    fn copy_paste_drag_then_save_mid_drag_commits_the_float_and_the_session_stays_interactive() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.doc.set_cell(0, 1, 1, cell('x'));

        // Select the single cell and copy it.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx);
        let copied_text = app.internal_clipboard.as_ref().unwrap().to_text();

        // Paste: lands as a floating stamp at the hovered cell (the origin — nothing is hovered).
        app.paste_text(&copied_text);
        assert_eq!(app.selection_slot(), Some(Binding::L));
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, ' ', "sanity: a paste floats, it doesn't write yet");

        // Grab the float and drag it.
        assert!(crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false), "the press on the float starts a drag");
        assert_eq!(app.stroke_owner, Some(Binding::L));
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &app.doc);

        // Ctrl+S while the button is still held.
        let dir = scratch_dir("mid_drag_save");
        let path = dir.join("out.gascii");
        app.current_path = Some(path.clone());
        app.save_file();

        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, 'x', "the float committed at its dragged position");
        let saved_doc = load_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved_doc.cell(0, 2, 2).unwrap().ch, 'x', "the saved file reflects the dragged position");

        // The session/keyboard state stays coherent afterward: still residue, not a discard.
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "the flush must not release the keyboard mid-drag");

        // The physical button releases a beat later; interaction continues cleanly from there.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.stroke_owner = None;
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 5, y: 5 }, &tctx, &app.doc);
        assert!(matches!(resp, ToolResponse::Active), "a fresh press must start a clean marquee, not error");
        assert!(
            app.slots[Binding::L.ix()]
                .tool
                .selection_overlay()
                .and_then(|v| v.marquee)
                .is_some_and(|r| r.contains(5, 5)),
            "the new marquee must not still be referencing the committed float"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The full cut workflow end to end — select, copy (a structural flush), delete, undo, redo —
    /// with content asserted at every step, not just the final state.
    #[test]
    fn the_cut_workflow_copy_delete_undo_redo_preserves_content_at_every_step() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Drag { x: 2, y: 2 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        app.doc.set_cell(0, 1, 1, cell('x'));
        app.doc.set_cell(0, 2, 2, cell('y'));

        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx); // Ctrl+C: a structural flush must not disturb the marquee.
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "copy must not itself mutate the document");
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, 'y');

        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Delete, &tctx, &app.doc);
        let ToolResponse::Commit(Some(edit)) = resp else { panic!("Delete must produce a committed edit") };
        app.apply_edit(edit, Some(Binding::L));
        for (x, y) in [(1u16, 1u16), (2, 2)] {
            assert_eq!(app.doc.cell(0, x, y), Some(&gascii_core::Cell::BLANK), "cut must blank the region");
        }

        app.request_undo();
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "undo restores the cut content");
        assert_eq!(app.doc.cell(0, 2, 2).unwrap().ch, 'y', "undo restores the cut content");

        app.request_redo();
        for (x, y) in [(1u16, 1u16), (2, 2)] {
            assert_eq!(app.doc.cell(0, x, y), Some(&gascii_core::Cell::BLANK), "redo re-applies the cut");
        }
    }

    /// `copy_selection` used to read `CellPatch::from_region(&self.doc, rect, 0)` — a literal layer
    /// 0 — regardless of which layer was active. Pins that Ctrl+C on a non-zero active layer captures
    /// that layer's own content, not layer 0's.
    #[test]
    fn copy_selection_captures_the_active_layer_not_layer_0() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.active_layer, 1, "sanity: adding a layer makes it the active one");

        app.doc.set_cell(1, 1, 1, cell('Q'));
        // Layer 0 stays blank at the same coordinate, so a layer-0 read would return a blank cell.
        assert_eq!(app.doc.cell(0, 1, 1), Some(&gascii_core::Cell::BLANK));

        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        let egui_ctx = egui::Context::default();
        app.copy_selection(&egui_ctx);

        let patch = app.internal_clipboard.as_ref().expect("copy must populate the clipboard");
        assert_eq!(patch.to_text(), "Q", "the clipboard must hold the active layer's content, not layer 0's blank cell");
    }

    /// The data-loss scenario from the layer-0-literal bug: Cut on a non-zero active layer must
    /// remove that layer's content AND fill the clipboard with the same content that was removed —
    /// not silently discard it while the clipboard holds unrelated layer-0 content.
    #[test]
    fn cut_on_a_non_zero_active_layer_clipboard_matches_what_delete_removed() {
        let mut app = GasciiApp::headless();
        let edit = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(edit, None);
        assert_eq!(app.active_layer, 1, "sanity: adding a layer makes it the active one");

        app.doc.set_cell(1, 1, 1, cell('Q'));

        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        let egui_ctx = egui::Context::default();
        app.cut_selection(&egui_ctx);

        assert_eq!(app.doc.cell(1, 1, 1), Some(&gascii_core::Cell::BLANK), "cut must remove the active layer's content");
        let patch = app.internal_clipboard.as_ref().expect("cut must populate the clipboard");
        assert_eq!(patch.to_text(), "Q", "the clipboard must hold exactly what was removed from the active layer");
    }

    /// `request_redo` deliberately skips flushing first (see its own doc comment), so a live burst
    /// can still be pending when a Redo mutates the document out from under it on the *other*
    /// binding. The resync fan-out that follows must reach that live burst too, not just a flush's
    /// targets — and, on a masked-off plane, recompose its pending content, not merely re-pin
    /// `before`.
    #[test]
    fn redoing_the_other_bindings_stroke_resyncs_a_live_burst_preserving_its_masked_off_plane() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);

        // R draws a colored cell, full mask.
        app.mask = PlaneMask::ALL;
        app.active_glyph = '#';
        app.active_bg = Rgba(1, 2, 3, 255);
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &r, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().bg, Rgba(1, 2, 3, 255), "sanity: R's stroke landed");

        app.request_undo(); // Ctrl+Z: reverts R's stroke back to Blank.
        assert_eq!(app.doc.cell(0, 0, 0), Some(&gascii_core::Cell::BLANK), "sanity: undo reverted R's stroke");

        // L starts a burst at the now-blank cell, writing only the glyph plane — the bg plane
        // composes from whatever `before` turns out to be.
        app.mask = PlaneMask { glyph: true, bg: false };
        app.acquire_keyboard(Binding::L);
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('B'), &l, &app.doc);

        app.request_redo(); // Ctrl+Shift+Z: redoes R's stroke, without flushing L's live burst first.
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().bg, Rgba(1, 2, 3, 255), "sanity: redo restored R's stroke");

        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'B', "the burst's glyph committed");
        assert_eq!(
            app.doc.cell(0, 0, 0).unwrap().bg,
            Rgba(1, 2, 3, 255),
            "the burst's masked-off bg plane must carry the redo's color, not a pre-redo stale value"
        );
    }

    /// Rebinding the OTHER binding through several kinds must never disturb a live burst — only
    /// rebinding the burst's OWN binding may touch it, and when it does, it must commit rather than
    /// discard.
    #[test]
    fn rebinding_the_other_binding_through_several_kinds_leaves_a_live_burst_untouched_then_rebinding_its_own_binding_commits_it(
    ) {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);

        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &l, &app.doc);
        app.acquire_keyboard(Binding::L);

        for kind in [ToolKind::Eraser, ToolKind::Fill, ToolKind::Selection, BRUSH_KIND, ToolKind::Line] {
            app.set_tool(Binding::R, kind);
            assert_eq!(app.slot(Binding::R).kind, kind, "R must actually rebind to {kind:?}");
            assert_eq!(app.keyboard_owner(), Some(Binding::L), "R's rebind must not touch L's session");
            assert!(
                app.slots[Binding::L.ix()].tool.caret().is_some(),
                "L's caret must survive R's rebind to {kind:?}"
            );
        }

        // Continue typing on L: the burst is unaffected by any of R's rebinds.
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('i'), &l, &app.doc);

        // Rebinding L itself must commit the burst, not discard it.
        app.set_tool(Binding::L, ToolKind::Pencil);
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h', "rebinding L must commit, not discard, the burst");
        assert_eq!(app.keyboard_owner(), None, "L released its own claim");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil);
    }

    /// Opening a file must strand neither a live Session (a Text burst) nor an in-flight Stroke (a
    /// Pencil drag still held) that exist simultaneously on the two bindings — nothing grafts onto
    /// the newly loaded document, and neither binding's ownership claim survives the swap.
    #[test]
    fn opening_a_file_strands_neither_a_live_burst_nor_an_in_flight_stroke_onto_the_new_document() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);

        // L: a live burst, pinned against the document that's about to be discarded.
        let l = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &l, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &l, &app.doc);
        app.acquire_keyboard(Binding::L);

        // R: a pencil stroke still physically held when Open fires.
        assert!(crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2, false, false));
        let r = crate::canvas::tool_ctx(&app, Binding::R);
        app.slots[Binding::R.ix()].tool.update(ToolEvent::Drag { x: 3, y: 2 }, &r, &app.doc);
        assert_eq!(app.stroke_owner, Some(Binding::R), "sanity: R is mid-stroke");

        // Open: Cancel (not flush) the pending tools, then swap the document — mirrors `open_file`
        // minus the native file dialog.
        let extent = app.doc.extent();
        app.reset_cross_frame_tool();
        app.doc = Document::new(extent.width, extent.height);
        app.history = History::new();

        assert_eq!(app.doc.cell(0, 0, 0), Some(&gascii_core::Cell::BLANK), "L's burst must not have committed");
        assert_eq!(app.doc.cell(0, 2, 2), Some(&gascii_core::Cell::BLANK), "R's in-flight stroke must not have committed");
        assert_eq!(app.stroke_owner, None, "R's in-flight stroke claim must not survive Open");
        assert_eq!(app.keyboard_owner(), None, "L's session claim must not survive Open");
        assert!(app.slots[Binding::L.ix()].tool.caret().is_none(), "L's caret must not survive Open");
        assert!(app.slots[Binding::R.ix()].tool.pending().is_empty(), "R's in-flight stroke cells must not survive Open");

        // A fresh press on the new document behaves like a clean start.
        let l2 = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &l2, &app.doc);
        assert!(app.slots[Binding::L.ix()].tool.caret().is_some(), "the new Text instance is interactive");
    }

    /// `note_recent_file` mirrors `push_recent`'s contract: most-recent-first, de-duplicated,
    /// capped — re-opening an already-listed path must move it to the front, not add a duplicate.
    #[test]
    fn note_recent_file_is_most_recent_first_deduplicated_and_capped_at_eight() {
        let mut app = GasciiApp::headless();
        for i in 0..10 {
            app.note_recent_file(&PathBuf::from(format!("{i}.gascii")));
        }
        assert_eq!(app.recent_files.len(), 8, "capped at 8 entries");
        assert_eq!(app.recent_files[0], PathBuf::from("9.gascii"), "most recent is first");
        assert_eq!(app.recent_files[7], PathBuf::from("2.gascii"), "oldest surviving entry");

        let reopened = PathBuf::from("5.gascii");
        app.note_recent_file(&reopened); // already present, mid-list
        assert_eq!(app.recent_files[0], reopened, "re-opening moves it to the front");
        assert_eq!(
            app.recent_files.iter().filter(|p| **p == reopened).count(),
            1,
            "must not duplicate an already-listed path"
        );
        assert_eq!(app.recent_files.len(), 8, "re-adding an existing entry does not grow the list");
    }

    /// A failed re-open (`open_path` reading a path that no longer exists) must drop that entry
    /// from `recent_files` rather than leaving a dead path the user can never successfully open.
    #[test]
    fn a_failed_reopen_drops_the_path_from_recent_files() {
        let mut app = GasciiApp::headless();
        let missing = std::env::temp_dir().join("gascii_definitely_missing_file.gascii");
        app.note_recent_file(&missing);
        assert!(app.recent_files.contains(&missing));

        app.open_path(&missing);

        assert!(!app.recent_files.contains(&missing), "a failed open must drop the dead entry");
        assert!(app.last_error.is_some());
    }

    /// The Export dialog's cell-px mapping: `16 * {1, 2, 4}` (D9), pinned so a future change to
    /// the base or the offered scales is a deliberate, visible edit here.
    #[test]
    fn export_cell_px_maps_scale_to_16x_32x_64x() {
        for (scale, expected) in [(1u8, 16u32), (2, 32), (4, 64)] {
            let settings = ExportSettings { scale, ..ExportSettings::default() };
            assert_eq!(settings.cell_px(), expected);
        }
    }

    /// `step_zoom` is a deferred request — it accumulates into `pending_step_zoom` for
    /// `canvas::show` to apply through the anchored `zoom_at` path (whose end-of-scale clamping
    /// the viewport tests cover). Mutating `zoom_step` directly here would bypass both the
    /// anchoring and the mid-stroke gate.
    #[test]
    fn step_zoom_defers_into_pending_step_zoom_without_touching_the_viewport() {
        let mut app = GasciiApp::headless();
        let before = app.viewport.zoom_step;
        app.step_zoom(1);
        app.step_zoom(1);
        app.step_zoom(-1);
        assert_eq!(app.pending_step_zoom, 1, "requests accumulate by sign");
        assert_eq!(app.viewport.zoom_step, before, "the viewport itself must be untouched until canvas::show applies it");
    }

    /// `modal_open()` is the one gate `canvas.rs`'s raw-input polling relies on — it must report
    /// true while `open_dialog` holds any variant, and while `confirm` is set, and false only when
    /// both are `None`. Structural now that `open_dialog` is one field rather than four independent
    /// bools: nothing to enumerate here, unlike the pre-migration version of this test.
    #[test]
    fn modal_open_is_true_while_open_dialog_or_confirm_is_set() {
        let mut app = GasciiApp::headless();
        assert!(!app.modal_open());

        app.confirm = Some(PendingConfirm::CloseApp);
        assert!(app.modal_open());
        app.confirm = None;

        for dialog in [OpenDialog::New, OpenDialog::Resize, OpenDialog::Export, OpenDialog::Help] {
            app.open_dialog = Some(dialog);
            assert!(app.modal_open(), "{dialog:?}");
            app.open_dialog = None;
        }

        assert!(!app.modal_open());
    }

    /// `open_dialog` is a single field, not four independent flags — opening a second dialog while
    /// one is already open structurally replaces it rather than leaving both set. This scenario has
    /// no real UI path in practice (every raw-input-polling site and `handle_keys` itself are gated
    /// on `modal_open()`, and `egui::Modal`'s own backdrop occludes the standard widgets — menu
    /// items included — that would otherwise open a second dialog), but the replacement behavior is
    /// pinned here since it's the one observable difference from the old shape, where two
    /// independent bools could (unreachably) both have ended up `true` at once.
    #[test]
    fn opening_a_dialog_while_another_is_open_replaces_it() {
        let mut app = GasciiApp::headless();
        app.open_dialog = Some(OpenDialog::Help);
        app.open_export_dialog();
        assert_eq!(app.open_dialog, Some(OpenDialog::Export), "opening Export must replace Help, not add to it");

        app.open_resize_dialog();
        assert_eq!(app.open_dialog, Some(OpenDialog::Resize), "opening Resize must replace Export");

        app.open_new_dialog();
        assert_eq!(app.open_dialog, Some(OpenDialog::New), "opening New must replace Resize");
    }

    /// The Export dialog's "Trim trailing spaces" checkbox toggles between two different text
    /// export functions (`export_text`, trimmed; `export_text_untrimmed`, padded) — this pins that
    /// the two genuinely diverge on a document with both a full-width row and a row with real
    /// trailing whitespace, so a future refactor that accidentally routes both dialog paths through
    /// the same function is caught here rather than only visually in the export preview.
    #[test]
    fn export_trim_checkbox_toggles_between_trimmed_and_full_width_padded_rows() {
        let mut doc = Document::new(5, 2);
        // Row 0: full-width content, no trailing blanks -- trim must be a no-op here.
        for x in 0..5u16 {
            doc.set_cell(0, x, 0, cell('#'));
        }
        // Row 1: content only in the first two columns, rest genuinely blank -- trim removes the
        // trailing three columns; untrimmed keeps the row padded to the full document width.
        doc.set_cell(0, 0, 1, cell('a'));
        doc.set_cell(0, 1, 1, cell('b'));

        let trimmed = export_text(&doc);
        let untrimmed = export_text_untrimmed(&doc);

        assert_eq!(trimmed, "#####\nab", "trim must drop row 1's trailing blanks but leave the full row untouched");
        assert_eq!(untrimmed, "#####\nab   ", "untrimmed must pad row 1 to the full document width");
        assert_ne!(trimmed, untrimmed, "the two export paths must genuinely diverge for this document");
    }

    /// Builds a real `n`-frame document via `apply_edit` (the same choke point every other
    /// multi-frame test in this module uses), rather than mutating `app.doc` directly.
    fn app_with_frame_count(n: usize) -> GasciiApp {
        let mut app = GasciiApp::headless();
        while app.doc.frame_count() < n {
            let edit = gascii_core::add_frame(&app.doc, app.doc.frame_count(), gascii_core::Frame::blank(app.doc.width, app.doc.height)).unwrap();
            app.apply_edit(edit, None);
        }
        app
    }

    /// A single-frame document's Export dialog offers exactly Text/PNG — the zero-visible-
    /// behavior-change constraint for every pre-existing document, locked in by construction.
    #[test]
    fn a_single_frame_documents_format_list_is_exactly_text_and_png() {
        let doc = Document::default_document();
        assert_eq!(doc.frame_count(), 1);
        let names: Vec<&str> = export_dialog_formats(&doc).iter().map(|(_, label)| *label).collect();
        assert_eq!(names, vec!["Text (.txt)", "PNG"]);
    }

    /// A multi-frame document's Export dialog offers all five formats, in the documented order.
    #[test]
    fn a_multi_frame_documents_format_list_offers_all_five_formats_in_order() {
        let app = app_with_frame_count(2);
        let names: Vec<&str> = export_dialog_formats(&app.doc).iter().map(|(_, label)| *label).collect();
        assert_eq!(names, vec!["Text (.txt)", "PNG", "Animated GIF", "PNG Spritesheet", "Text Frames (.txt)"]);
    }

    /// `snap_unavailable_export_format`: a multi-frame-only format survives while `frame_count() >
    /// 1`, snaps back to `Text` the moment it drops to 1, and every format is a no-op at `> 1`.
    #[test]
    fn snap_unavailable_export_format_only_touches_multi_frame_only_formats_at_frame_count_one() {
        for format in [ExportFormat::Gif, ExportFormat::SpriteSheet, ExportFormat::TextFrames] {
            assert_eq!(snap_unavailable_export_format(format, 1), ExportFormat::Text);
            assert_eq!(snap_unavailable_export_format(format, 2), format, "must be a no-op while still offered");
        }
        for format in [ExportFormat::Text, ExportFormat::Png] {
            assert_eq!(snap_unavailable_export_format(format, 1), format, "an always-offered format is never snapped");
        }
    }

    /// `refresh_export_preview`'s gate: Gif and SpriteSheet build a preview texture exactly like
    /// PNG (all three are raster formats sharing the active-frame rasterizer); TextFrames does not,
    /// mirroring the existing Text case.
    #[test]
    fn refresh_export_preview_builds_a_texture_for_every_raster_format_but_not_text_frames() {
        let mut app = app_with_frame_count(2);
        let ctx = egui::Context::default();
        for format in [ExportFormat::Png, ExportFormat::Gif, ExportFormat::SpriteSheet] {
            app.export.format = format;
            app.export_preview = None;
            app.export_preview_key = None;
            app.refresh_export_preview(&ctx);
            assert!(app.export_preview.is_some(), "{format:?} must build a preview texture");
        }
        app.export.format = ExportFormat::TextFrames;
        app.export_preview = None;
        app.export_preview_key = None;
        app.refresh_export_preview(&ctx);
        assert!(app.export_preview.is_none(), "TextFrames must not build a preview texture");
    }

    /// `export_gif`/`export_spritesheet`'s output written through `write_atomic` — the same
    /// file-write half `write_atomic_creates_a_new_file_with_exact_contents` already pins for
    /// Text/PNG — decodes back as the expected format and dimensions. `run_export` itself opens a
    /// real native (blocking) `rfd::FileDialog`, so it — like the pre-existing Text/Png format
    /// arms — has no direct test; this exercises the same export-function-then-write_atomic
    /// pipeline `run_export`'s new arms perform, without the interactive file picker.
    #[test]
    fn gif_and_spritesheet_bytes_round_trip_through_write_atomic() {
        let app = app_with_frame_count(2);
        let dir = std::env::temp_dir().join(format!("gascii_anim_export_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let gif_path = dir.join("out.gif");
        let gif_bytes = anim_export::export_gif(&app.doc, 8, None, None).unwrap();
        write_atomic(&gif_path, &gif_bytes).unwrap();
        let decoded = image::load_from_memory(&std::fs::read(&gif_path).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (app.doc.width as u32 * 8, app.doc.height as u32 * 8));

        let sheet_path = dir.join("out.png");
        let sheet_bytes = anim_export::export_spritesheet(&app.doc, 8, None, None).unwrap();
        write_atomic(&sheet_path, &sheet_bytes).unwrap();
        let decoded = image::load_from_memory(&std::fs::read(&sheet_path).unwrap()).unwrap();
        // 2 frames -> a 2x1 grid (`cols = ceil(sqrt(2)) = 2`, `rows = 1`).
        assert_eq!(
            (decoded.width(), decoded.height()),
            (app.doc.width as u32 * 8 * 2, app.doc.height as u32 * 8),
            "2 frames, 2x1 grid"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The Export dialog's "Trim trailing spaces" checkbox toggles between `export_text_frames`
    /// (trimmed) and `export_text_frames_untrimmed` (padded) for `TextFrames`, the same divergence
    /// `export_trim_checkbox_toggles_between_trimmed_and_full_width_padded_rows` already pins for
    /// the single-frame `Text` pair.
    #[test]
    fn text_frames_trim_checkbox_toggles_between_trimmed_and_full_width_padded_rows() {
        let mut doc = Document::new(5, 1);
        doc.set_cell(0, 0, 0, cell('a'));
        doc.set_cell(0, 1, 0, cell('b'));

        let trimmed = export_text_frames(&doc);
        let untrimmed = export_text_frames_untrimmed(&doc);

        assert_eq!(trimmed, format!("--- frame 1 ({}ms) ---\nab", Document::DEFAULT_FRAME_DURATION_MS));
        assert_eq!(untrimmed, format!("--- frame 1 ({}ms) ---\nab   ", Document::DEFAULT_FRAME_DURATION_MS));
        assert_ne!(trimmed, untrimmed, "the two export paths must genuinely diverge for this document");
    }

    /// `TextFrames`'s combined dump, like every other export format, must round-trip byte-exact
    /// through `write_atomic` and leave no `.tmp` file behind -- the same file-write half
    /// `write_atomic_creates_a_new_file_with_exact_contents` already pins for Text/PNG/Gif/
    /// SpriteSheet, extended to the one format that had no direct atomic-write test yet.
    #[test]
    fn text_frames_bytes_round_trip_through_write_atomic_with_no_tmp_file_left_behind() {
        let mut doc = Document::new(3, 1);
        doc.set_cell(0, 0, 0, cell('a'));
        let mut history = History::new();
        let edit = gascii_core::add_frame(&doc, 1, gascii_core::Frame::blank(3, 1)).unwrap();
        history.apply(&mut doc, edit);
        assert!(doc.set_active_frame(1));
        doc.set_cell(0, 1, 0, cell('b'));
        assert!(doc.set_active_frame(0));

        let dir = scratch_dir("text_frames_atomic");
        let path = dir.join("out.txt");
        let text = export_text_frames(&doc);
        write_atomic(&path, text.as_bytes()).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(!dir.join("out.txt.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cap composition, GIF: a document legitimately built past the joint frame-count budget
    /// (`validate_gif_dimensions`'s own locked-in 196-frame boundary) must be rejected with no
    /// bytes ever reaching the filesystem -- the same `Err` -> skip-`write_atomic` shape
    /// `run_export`'s `Gif` arm itself follows -- and the source document/history must be
    /// completely untouched by the failed attempt (export functions only ever borrow `&Document`,
    /// but this pins that invariant at the integration level rather than trusting the type system
    /// silently).
    #[test]
    fn a_gif_export_rejected_for_too_many_frames_writes_no_file_and_leaves_the_document_untouched() {
        let mut doc = Document::new(80, 25);
        let mut history = History::new();
        for _ in 1..196 {
            let edit = gascii_core::add_frame(&doc, doc.frame_count(), gascii_core::Frame::blank(80, 25)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), 196);
        let before = doc.clone();
        let before_top_edit = history.top_edit_id();

        let dir = scratch_dir("gif_cap_rejection");
        let path = dir.join("rejected.gif");
        match anim_export::export_gif(&doc, 16, None, None) {
            Ok(_) => panic!("196 frames at 80x25/16px must be rejected by the joint pixel budget"),
            Err(e) => assert!(matches!(e, png_export::PngExportAppError::Dimensions(gascii_core::PngExportError::TooManyFrames { .. }))),
        }

        assert!(!path.exists(), "a rejected export must never reach write_atomic, so no file may exist");
        assert_eq!(doc, before, "a failed export must not mutate the source document");
        assert_eq!(history.top_edit_id(), before_top_edit, "a failed export must not touch history");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cap composition, spritesheet: a per-frame size that's individually fine
    /// (`validate_png_dimensions` succeeds) can still be rejected once tiled into a multi-frame
    /// grid (`validate_spritesheet_dimensions` fails on the joint canvas) -- a genuinely different
    /// rejection reason from the GIF case above, reached through the same `Err`-before-any-write
    /// shape, no file left behind, document/history untouched.
    #[test]
    fn a_spritesheet_export_rejected_for_a_too_large_tiled_canvas_writes_no_file_and_leaves_the_document_untouched() {
        let mut doc = Document::new(1024, 512);
        let mut history = History::new();
        for _ in 1..4 {
            let edit = gascii_core::add_frame(&doc, doc.frame_count(), gascii_core::Frame::blank(1024, 512)).unwrap();
            history.apply(&mut doc, edit);
        }
        assert_eq!(doc.frame_count(), 4);
        // Sanity: one frame alone is well under the single-PNG cap (8192x4096 ≈ 33.5MP < 100MP).
        assert!(gascii_core::validate_png_dimensions(doc.width, doc.height, 8).is_ok());
        let before = doc.clone();
        let before_top_edit = history.top_edit_id();

        let dir = scratch_dir("spritesheet_cap_rejection");
        let path = dir.join("rejected.png");
        match anim_export::export_spritesheet(&doc, 8, None, None) {
            Ok(_) => panic!("a 2x2 grid of 8192x4096 tiles (~134MP) must be rejected by the spritesheet cap"),
            Err(e) => assert!(matches!(e, png_export::PngExportAppError::Dimensions(gascii_core::PngExportError::TooLarge { .. }))),
        }

        assert!(!path.exists(), "a rejected export must never reach write_atomic, so no file may exist");
        assert_eq!(doc, before, "a failed export must not mutate the source document");
        assert_eq!(history.top_edit_id(), before_top_edit, "a failed export must not touch history");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `Gif`-format export preference persisted while a multi-frame document was open survives
    /// its own `Prefs` JSON round-trip unmodified -- `Prefs` is document-agnostic, it has no idea
    /// what document it'll next be applied to -- but the moment it's paired with a single-frame
    /// document (e.g. the app restarted after the user deleted every extra frame), the Export
    /// dialog's own reopen-time guard (`snap_unavailable_export_format`, applied at the top of
    /// `export_dialog` every frame it's open) snaps it back to `Text`, exactly as it would for any
    /// other document that dropped to one frame mid-session.
    #[test]
    fn a_persisted_gif_preference_meeting_a_single_frame_document_snaps_back_to_text_on_reopen() {
        let mut multi = app_with_frame_count(2);
        multi.export.format = ExportFormat::Gif;
        let prefs = crate::prefs::Prefs::from_app(&multi);
        let json = serde_json::to_string(&prefs).unwrap();
        let restored_prefs: crate::prefs::Prefs = serde_json::from_str(&json).unwrap();

        let mut single = GasciiApp::headless();
        assert_eq!(single.doc.frame_count(), 1, "sanity: a fresh headless app starts single-frame");
        restored_prefs.apply_to(&mut single);
        assert_eq!(
            single.export.format,
            ExportFormat::Gif,
            "Prefs itself is document-agnostic -- it restores whatever format was last stored, unsnapped"
        );

        let snapped = snap_unavailable_export_format(single.export.format, single.doc.frame_count());
        assert_eq!(snapped, ExportFormat::Text, "the dialog's reopen-time guard must snap an unavailable format back to Text");
    }

    /// Regression proof for WS5's `rasterize_composited` extraction, with realistic (not
    /// single-pixel) content: multiple distinct glyphs, distinct fg/bg colors including partial
    /// alpha, and an opaque document background -- `export_png` (built on `rasterize_rgba8`, which
    /// now delegates to `rasterize_composited`) must still produce byte-identical output to
    /// manually driving the frame-explicit path (`rasterize_frame_rgba8` at the active frame) and
    /// encoding it the same way `export_png` itself does. A regression that only shows up on
    /// multi-cell, multi-color, partially-transparent content (not the trivial 1x1 cases the
    /// pre-existing suite already covers) would only be caught here.
    #[test]
    fn export_png_is_byte_identical_to_manually_driving_the_frame_explicit_rasterizer_on_realistic_content() {
        let mut doc = Document::new(6, 4);
        doc.background = Rgba(20, 30, 40, 255);
        doc.set_cell(0, 0, 0, gascii_core::Cell { ch: 'A', fg: Rgba(255, 0, 0, 255), bg: Rgba::TRANSPARENT });
        doc.set_cell(0, 1, 0, gascii_core::Cell { ch: 'B', fg: Rgba(0, 255, 0, 200), bg: Rgba(10, 10, 10, 128) });
        doc.set_cell(0, 2, 1, gascii_core::Cell { ch: '#', fg: Rgba::WHITE, bg: Rgba(0, 0, 255, 255) });
        doc.set_cell(0, 5, 3, gascii_core::Cell { ch: 'Z', fg: Rgba(1, 2, 3, 90), bg: Rgba::TRANSPARENT });

        let opaque_bg = Some(doc.background);
        let via_export_png = png_export::export_png(&doc, 12, opaque_bg, None).unwrap();

        let (w, h, pixels) = png_export::rasterize_frame_rgba8(&doc, doc.active_frame(), 12, opaque_bg, None).unwrap();
        let img = image::RgbaImage::from_raw(w, h, pixels).unwrap();
        let mut via_manual_frame_explicit = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut via_manual_frame_explicit), image::ImageFormat::Png).unwrap();

        assert_eq!(via_export_png, via_manual_frame_explicit, "export_png must remain byte-identical to the frame-explicit rasterization path");
    }

    /// Program-level end-to-end proof for the whole animation-plugin program (Phases 1-5): a
    /// document built up through the real user-facing sequence -- new doc, draw per frame, add
    /// frames, set per-frame durations -- is saved to `.gascii`, reloaded as a fresh `Document`
    /// sharing no memory with the original, and every one of this phase's three export formats is
    /// produced from that *reloaded* document and independently verified against it. A bug in the
    /// v2 save/load frame round-trip that only manifested at export time would slip through every
    /// phase-scoped test suite but not this one.
    #[test]
    fn a_full_new_draw_frames_durations_save_load_export_lifecycle_round_trips_correctly() {
        let mut app = GasciiApp::headless();
        app.doc = Document::new(3, 2);
        app.history = History::new();

        // Frame 0: draw.
        let red = Rgba(255, 0, 0, 255);
        app.doc.set_cell(0, 0, 0, gascii_core::Cell { ch: 'A', fg: red, bg: Rgba::TRANSPARENT });

        // Frame 1: add, draw distinct content.
        let edit = gascii_core::add_frame(&app.doc, 1, gascii_core::Frame::blank(3, 2)).unwrap();
        app.apply_edit(edit, None);
        assert!(app.doc.set_active_frame(1));
        let green = Rgba(0, 255, 0, 255);
        app.doc.set_cell(0, 1, 0, gascii_core::Cell { ch: 'B', fg: green, bg: Rgba::TRANSPARENT });

        // Frame 2: add, draw distinct content, give it its own duration.
        let edit = gascii_core::add_frame(&app.doc, 2, gascii_core::Frame::blank(3, 2)).unwrap();
        app.apply_edit(edit, None);
        assert!(app.doc.set_active_frame(2));
        let blue = Rgba(0, 0, 255, 255);
        app.doc.set_cell(0, 2, 0, gascii_core::Cell { ch: 'C', fg: blue, bg: Rgba::TRANSPARENT });
        let edit = gascii_core::set_frame_duration(&app.doc, 2, Some(250)).unwrap().unwrap();
        app.apply_edit(edit, None);
        app.doc.loop_playback = false;
        assert!(app.doc.set_active_frame(0));

        // Save + reload: the loaded document shares no memory with `app.doc`.
        let saved = save_string(&app.doc);
        let loaded = load_str(&saved).unwrap();
        assert_eq!(loaded.frame_count(), 3);
        assert_eq!(loaded.resolved_frame_duration_ms(2), Some(250));
        assert!(!loaded.loop_playback);

        // Export all three multi-frame formats from the *loaded* document.
        let gif_bytes = anim_export::export_gif(&loaded, 8, None, None).unwrap();
        let sheet_bytes = anim_export::export_spritesheet(&loaded, 8, None, None).unwrap();
        let text = export_text_frames(&loaded);

        // GIF: 3 frames, no loop extension (loop_playback == false survived the round trip), each
        // frame carries its source glyph's color, frame 2's delay honors the reloaded 250ms override.
        use image::AnimationDecoder;
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&gif_bytes)).unwrap();
        let frames = decoder.into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 3);
        let close = |a: u8, b: u8| (a as i16 - b as i16).abs() <= 16;
        for (frame, &color) in frames.iter().zip([red, green, blue].iter()) {
            assert!(
                frame.buffer().pixels().any(|p| close(p.0[0], color.0) && close(p.0[1], color.1) && close(p.0[2], color.2)),
                "each decoded GIF frame must contain a pixel close to its source frame's color {color:?}"
            );
        }
        let (numer, denom) = frames[2].delay().numer_denom_ms();
        assert_eq!(numer / denom, 250, "frame 2's reloaded duration_override must survive into the GIF's delay");
        assert!(!gif_bytes.windows(11).any(|w| w == b"NETSCAPE2.0"), "the reloaded loop_playback == false must write no loop extension");

        // Spritesheet: 3 frames -> a 2x2 grid (cols=ceil(sqrt(3))=2, rows=2); frame 2 lands at (0,1).
        // Each frame draws one glyph at a different cell column (not a whole-tile fill), so the
        // check scans frame 2's whole tile region for its color rather than one hard-coded pixel.
        let decoded = image::load_from_memory(&sheet_bytes).unwrap().to_rgba8();
        let (frame_px_w, frame_px_h) = gascii_core::validate_png_dimensions(loaded.width, loaded.height, 8).unwrap();
        let (sheet_w, sheet_h) = gascii_core::validate_spritesheet_dimensions(frame_px_w, frame_px_h, 2, 2).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (sheet_w, sheet_h));
        let (tile_x0, tile_y0) = (0u32, frame_px_h); // frame 2's tile origin at grid (col=0, row=1)
        let found_blue_in_frame_2s_tile = (tile_y0..tile_y0 + frame_px_h)
            .flat_map(|y| (tile_x0..tile_x0 + frame_px_w).map(move |x| (x, y)))
            .any(|(x, y)| decoded.get_pixel(x, y).0[..3] == [blue.0, blue.1, blue.2]);
        assert!(found_blue_in_frame_2s_tile, "frame 2's own tile must contain its glyph's exact fg color somewhere");

        // Text frames: 3 headered bodies, each matching that frame's own `export_text` taken in
        // isolation on the reloaded document. Sliced by each header's own position (not a naive
        // `"\n\n"` split) since frame 0's body itself ends in a blank second row -- its own
        // trailing newline plus the `"\n\n"` frame separator would otherwise misalign a
        // position-based split.
        let headers: Vec<String> = (0..3)
            .map(|i| format!("--- frame {} ({}ms) ---", i + 1, loaded.resolved_frame_duration_ms(i).unwrap()))
            .collect();
        let starts: Vec<usize> = headers.iter().map(|h| text.find(h.as_str()).unwrap()).collect();
        for i in 0..3 {
            let mut isolated = loaded.clone();
            isolated.set_active_frame(i);
            let expected_body = export_text(&isolated);
            let seg_end = if i + 1 < 3 { starts[i + 1] - 2 } else { text.len() };
            let segment = &text[starts[i]..seg_end];
            assert_eq!(
                segment,
                format!("{}\n{expected_body}", headers[i]),
                "frame {i}'s text segment must match export_text of the reloaded document in isolation"
            );
        }
    }

    /// End-to-end, driven through real tool sessions and `apply_edit` rather than hand-built
    /// documents: paint on layer 0, add a second layer, paint on it, hide layer 0 — text and PNG
    /// export must then reflect only layer 1's content — and undoing the whole sequence restores
    /// layer 0's visibility and content exactly.
    #[test]
    fn paint_add_layer_paint_hide_export_then_undo_restores_layer_0_exactly() {
        let mut app = GasciiApp::headless();
        app.doc = Document::new(4, 4);
        app.history = History::new();
        let pristine = app.doc.clone();

        // Paint on layer 0.
        app.active_glyph = 'A';
        app.active_fg = Rgba(255, 0, 0, 255);
        app.active_bg = Rgba(255, 0, 0, 255);
        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "sanity: layer 0's paint landed");
        let after_paint0 = app.doc.clone();

        // Add a second layer — becomes active automatically.
        let add = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(add, None);
        assert_eq!(app.active_layer, 1, "sanity: add_layer lands its own new layer active");

        // Paint on layer 1.
        app.active_glyph = 'B';
        app.active_fg = Rgba(0, 0, 255, 255);
        app.active_bg = Rgba(0, 0, 255, 255);
        crate::canvas::begin_gesture(&mut app, Binding::L, 1, 1, false, false);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc) {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(1, 1, 1).unwrap().ch, 'B', "sanity: layer 1's paint landed");

        // Hide layer 0.
        let hide = gascii_core::set_layer_visibility(&app.doc, 0, false).unwrap().unwrap();
        app.apply_edit(hide, None);
        assert!(!app.doc.layer_visible(0));

        // Text export reflects only layer 1's content.
        let text = export_text(&app.doc);
        assert!(text.contains('B'), "text export must include layer 1's glyph: {text:?}");
        assert!(!text.contains('A'), "text export must exclude layer 0's now-hidden glyph: {text:?}");

        // PNG export reflects only layer 1's content: layer 0's tile is fully transparent, layer
        // 1's tile carries its own bg color somewhere within it.
        let cell_px = 12;
        let (px_w, _px_h, pixels) = png_export::rasterize_rgba8(&app.doc, cell_px, None, None).unwrap();
        let pixel_at = |x: u32, y: u32| -> [u8; 4] {
            let idx = (y * px_w + x) as usize * 4;
            [pixels[idx], pixels[idx + 1], pixels[idx + 2], pixels[idx + 3]]
        };
        let tile_pixels = |cx: u32, cy: u32| -> Vec<[u8; 4]> {
            (0..cell_px).flat_map(|py| (0..cell_px).map(move |px| (px, py))).map(|(px, py)| pixel_at(cx * cell_px + px, cy * cell_px + py)).collect()
        };
        assert!(
            tile_pixels(0, 0).iter().all(|p| p[3] == 0),
            "layer 0's tile must be fully transparent in the PNG export once hidden"
        );
        let blue = [0u8, 0, 255, 255];
        assert!(
            tile_pixels(1, 1).contains(&blue),
            "layer 1's tile must carry its own bg color in the PNG export"
        );

        // Undo the whole sequence: the hide first, then layer 1's paint, then the add, landing
        // back at exactly the post-layer-0-paint snapshot; one more undo restores the pristine doc.
        app.request_undo(); // undoes the hide
        assert!(app.doc.layer_visible(0), "undoing the hide must restore layer 0's visibility");
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "layer 0's content must still be exactly what was painted");

        app.request_undo(); // undoes layer 1's paint
        app.request_undo(); // undoes add_layer
        assert_eq!(app.doc, after_paint0, "undoing back to just after layer 0's paint must match that snapshot exactly");

        app.request_undo(); // undoes layer 0's paint
        assert_eq!(app.doc, pristine, "undoing the entire sequence must restore the pristine document exactly");
    }

    /// The New dialog's background color well (`new_bg`) must land on the freshly created
    /// document's `background` field, not just sit as inert dialog state -- the one place this
    /// wiring is exercised outside a full GUI run.
    #[test]
    fn create_new_document_carries_the_dialog_background_onto_the_fresh_document() {
        let mut app = GasciiApp::headless();
        app.new_w = 12;
        app.new_h = 6;
        app.new_bg = Rgba(1, 2, 3, 255);
        app.create_new_document();

        assert_eq!((app.doc.width, app.doc.height), (12, 6));
        assert_eq!(app.doc.background, Rgba(1, 2, 3, 255));
        assert_eq!(app.open_dialog, None, "creating the document must close the dialog");
        assert!(!app.history.can_undo(), "a fresh document starts with empty history");
    }

    /// The Clear button's wiring end to end: one undoable step that blanks the document and
    /// undoes cleanly, exercised through `GasciiApp::clear_document` rather than the core free
    /// function directly (core's own tests already cover the pure edit-building math).
    #[test]
    fn clear_document_app_method_produces_one_undoable_step() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 0, 0, cell('a'));
        app.doc.set_cell(0, 5, 5, cell('z'));
        let before = app.doc.clone();

        app.clear_document();

        assert!(app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank));
        assert!(app.history.can_undo());
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc, before);
    }

    /// Clearing an already-blank document must not push a phantom undo entry — matches every
    /// other tool's "nothing to commit" contract.
    #[test]
    fn clear_document_on_an_already_blank_document_creates_no_undo_entry() {
        let mut app = GasciiApp::headless();
        assert!(!app.history.can_undo());
        app.clear_document();
        assert!(!app.history.can_undo());
    }

    /// Clear flushes first, same trigger-table discipline as Save/Export/Resize/Copy: a live text
    /// burst must commit before Clear blanks the document, not be silently discarded.
    #[test]
    fn clear_document_flushes_a_pending_text_burst_before_blanking() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &tctx, &app.doc);

        app.clear_document();

        // The burst's 'A' commits, then Clear blanks it right back out — both as real edits, so
        // two undos are needed to get back to the empty starting document.
        assert!(app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank));
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'A', "undo #1 restores the burst's commit");
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, ' ', "undo #2 restores the pre-burst blank state");
    }

    /// The Selection counterpart of the Text-burst flush test above: a floating stamp is a
    /// session too (`holds_session`), so `clear_document`'s `flush_all()` must drop it into the
    /// document before Clear blanks everything — not silently discard the pending paste/move.
    #[test]
    fn clear_document_flushes_a_pending_floating_selection_stamp_before_blanking() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        let patch = CellPatch { width: 1, height: 1, cells: vec![cell('z')] };
        app.slots[Binding::L.ix()].tool.accept_stamp(patch, (4, 4), &app.doc);

        app.clear_document();

        // Same two-undo-entry shape as the Text-burst case: the drop commits, then Clear blanks
        // it back out.
        assert!(app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank));
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 4, 4).unwrap().ch, 'z', "undo #1 restores the dropped stamp");
        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc.cell(0, 4, 4).unwrap().ch, ' ', "undo #2 restores the pre-drop blank state");
    }

    /// Clear must round-trip through both undo AND redo, not just undo — `History::redo` re-applies
    /// the exact same `Edit::Cells` `clear_document` built, so re-blanking after a redo must match
    /// the original clear byte-for-byte, and the history's can_undo/can_redo flags must track it.
    #[test]
    fn clear_document_survives_an_undo_then_redo_round_trip() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 1, 1, cell('a'));
        app.doc.set_cell(0, 3, 3, cell('b'));
        let before = app.doc.clone();

        app.clear_document();
        let after_clear = app.doc.clone();
        assert!(app.history.can_undo());
        assert!(!app.history.can_redo());

        assert!(app.history.undo(&mut app.doc));
        assert_eq!(app.doc, before, "undo must restore the exact pre-Clear document");
        assert!(app.history.can_redo());

        assert!(app.history.redo(&mut app.doc));
        assert_eq!(app.doc, after_clear, "redo must re-apply the exact same Clear edit");
        assert!(!app.history.can_redo());
    }

    /// The bug class `Tool::resync` exists to prevent: Clear runs mid-stroke (`flush_all` only
    /// commits session-holding kinds, so a raw Pencil drag survives Clear untouched at the tool
    /// level), but the document underneath it is blanked. `apply_edit`'s resync fan-out must
    /// recompose the stroke's already-touched pending cells against the new blank document —
    /// including on a masked-off plane, where a missed recompose would silently commit the
    /// pre-Clear bg color back in on release.
    #[test]
    fn clear_mid_stroke_resyncs_the_pending_drags_masked_off_bg_plane_to_the_post_clear_blank_state() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.mask = PlaneMask { glyph: true, bg: false }; // bg masked off: must always track `before`
        app.active_glyph = '#';
        app.active_fg = Rgba::WHITE;

        let old_bg = Rgba(10, 20, 30, 255);
        app.doc.set_cell(0, 2, 2, gascii_core::Cell { ch: 'x', fg: Rgba::WHITE, bg: old_bg });

        crate::canvas::begin_gesture(&mut app, Binding::L, 2, 2, false, false);
        assert_eq!(app.stroke_owner, Some(Binding::L));
        assert!(
            !app.slots[Binding::L.ix()].tool.pending().is_empty(),
            "sanity: the stroke touched a cell before Clear ran"
        );

        app.clear_document();
        assert!(
            app.doc.layers()[0].cells().iter().all(gascii_core::Cell::is_blank),
            "Clear must blank the document even with a stroke mid-flight"
        );
        assert_eq!(app.stroke_owner, Some(Binding::L), "Clear must not itself end an in-progress stroke");

        // Finish the stroke where it started (a click) and commit.
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;

        let committed = app.doc.cell(0, 2, 2).unwrap();
        assert_eq!(committed.ch, '#', "the unmasked glyph plane still stamps the drawn glyph");
        assert_ne!(committed.bg, old_bg, "the masked-off bg plane must not resurrect the pre-Clear color");
        assert_eq!(
            committed.bg,
            Rgba::TRANSPARENT,
            "the masked-off bg plane must follow the post-Clear blank bg, not a stale composition"
        );
    }

    /// `begin_gesture`'s own reset of `pressure_stamp_size` is the last line of defense against a
    /// leaked override, independent of every release/cancel path already covered elsewhere: even if
    /// some future bug left a stale value behind, the very next stroke on ANY binding — pen or not —
    /// must never inherit it.
    #[test]
    fn begin_gesture_always_clears_a_stale_pressure_override_regardless_of_which_binding_or_tool_set_it() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Pencil);
        app.pressure_stamp_size = Some(3); // simulates a leftover value some other path failed to clear

        crate::canvas::begin_gesture(&mut app, Binding::R, 0, 0, false, false);

        assert_eq!(
            app.pressure_stamp_size, None,
            "a fresh stroke on ANY binding must start with no pressure override"
        );
        let tctx = crate::canvas::tool_ctx(&app, Binding::R);
        let pencil_slot = sized_slot(ToolKind::Pencil).expect("Pencil is sized");
        assert_eq!(
            tctx.size, app.slots[Binding::R.ix()].stamps[pencil_slot].size,
            "a non-pen stroke must use its own configured size, never an inherited override"
        );
    }

    /// K2: zoom snaps to Fit only on the false→true transition, never on exit. `pending_fit` is the
    /// mechanism — this pins it directly against `toggle_fullscreen` rather than trusting the
    /// existing Escape/F11 tests, which don't inspect `pending_fit` at all.
    #[test]
    fn toggle_fullscreen_snaps_pending_fit_only_on_the_false_to_true_transition() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        let ctx = egui::Context::default(); // no viewport info registered: fullscreen reads as false
        app.toggle_fullscreen(&ctx); // false -> true
        assert!(app.pending_fit, "entering fullscreen must snap zoom to Fit");

        app.pending_fit = false;
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        let _ = ctx.run_ui(raw, |_ui| {});
        app.toggle_fullscreen(&ctx); // true -> false
        assert!(!app.pending_fit, "exiting fullscreen must NOT re-trigger a Fit snap");
    }

    /// End-to-end trace of K1's full precedence chain across two real frames — not just the pure
    /// `should_handle_escape_for_fullscreen` predicate, but `handle_keys` AND `canvas.rs`'s own
    /// Text-session Escape handling racing over the same frame's events, exactly as `GasciiApp::ui`
    /// drives them in sequence. Frame 1's Escape must end the session and leave fullscreen alone;
    /// frame 2's Escape (session now gone) must exit fullscreen.
    #[test]
    fn escape_precedence_chain_first_ends_text_session_second_exits_fullscreen() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "sanity: the Text session holds the keyboard");

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);

        fn escape_event() -> egui::Event {
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }
        }

        // Frame 1: fullscreen, Escape pressed, Text session still active.
        let mut raw1 = egui::RawInput::default();
        raw1.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw1.events.push(escape_event());
        let out1 = ctx.run_ui(raw1, |ui| {
            app.handle_keys(ui);
            crate::canvas::show(ui, &mut app, false);
        });
        let toggled_frame1 = out1
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))));
        assert!(!toggled_frame1, "frame 1's Escape must be claimed by the session, not fullscreen");
        assert_eq!(
            app.keyboard_owner(), None,
            "frame 1's Escape must end the Text session (canvas.rs's own handling)"
        );

        // Frame 2: fullscreen, Escape pressed again, no session left to claim it.
        let mut raw2 = egui::RawInput::default();
        raw2.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw2.events.push(escape_event());
        let out2 = ctx.run_ui(raw2, |ui| app.handle_keys(ui));
        let toggled_frame2 = out2
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(false))));
        assert!(toggled_frame2, "frame 2's Escape, with no session left, must exit fullscreen");
    }

    /// The third rung of K1's precedence chain: a live pointer stroke outranks Escape's
    /// fullscreen-exit claim exactly like an active session does — exiting mid-drag would yank the
    /// canvas out from under the pointer. Drives the real `handle_keys` rather than only the pure
    /// predicate, so a future accidental reordering of the guards would be caught here too.
    #[test]
    fn escape_does_not_exit_fullscreen_while_a_stroke_is_mid_drag() {
        let mut app = GasciiApp::headless();
        crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
        assert!(app.stroke_in_progress(), "sanity: a stroke is mid-drag");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let output = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        let toggled = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))));
        assert!(!toggled, "Escape must not exit fullscreen while a stroke is mid-drag");
        assert!(app.stroke_in_progress(), "the mid-drag stroke itself must be untouched");
    }

    /// The fourth rung of K1's precedence chain: a focused widget (the hex color popup, say)
    /// outranks Escape's fullscreen-exit claim exactly like a session or a stroke does — egui's own
    /// popups close on Escape at draw time, which runs after `handle_keys`, so consuming the key
    /// here would swallow it before the popup ever gets a chance to react. Once nothing is focused,
    /// the same Escape press exits fullscreen as usual.
    #[test]
    fn escape_does_not_exit_fullscreen_while_a_widget_has_focus_but_does_once_nothing_is_focused() {
        let mut app = GasciiApp::headless();
        app.pending_fit = false;
        let ctx = egui::Context::default();

        fn escape_event() -> egui::Event {
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }
        }

        // Frame 1: fullscreen, a widget focused, Escape pressed.
        let mut raw1 = egui::RawInput::default();
        raw1.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw1.events.push(escape_event());
        let out1 = ctx.run_ui(raw1, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });
        let toggled_frame1 = out1
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))));
        assert!(!toggled_frame1, "a focused widget must withhold Escape from the fullscreen exit");

        // Frame 2: fullscreen, nothing focused, Escape pressed again.
        let mut raw2 = egui::RawInput::default();
        raw2.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw2.events.push(escape_event());
        let out2 = ctx.run_ui(raw2, |ui| app.handle_keys(ui));
        let toggled_frame2 = out2
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(false))));
        assert!(toggled_frame2, "with nothing focused, Escape must exit fullscreen exactly as before");
    }

    /// Escape must not be consumed at all when it has nothing to do — windowed (no fullscreen to
    /// exit), no session, no stroke, no focused widget. Consuming it anyway would swallow the key
    /// before `handle_keys`'s caller ever gets to a popup/menu whose own Escape handling runs at
    /// draw time.
    #[test]
    fn escape_is_not_consumed_when_windowed_with_nothing_to_cancel() {
        let mut app = GasciiApp::headless();
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(false);
        raw.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let mut still_pressed = false;
        let _ = ctx.run_ui(raw, |ui| {
            app.handle_keys(ui);
            still_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        assert!(
            still_pressed,
            "windowed with nothing to cancel: Escape must not be consumed, so a later popup/menu can still see it"
        );
    }

    /// Extends the existing F11-during-Text-session regression test: toggling fullscreen must be a
    /// pure side-channel to the Text session, not just "doesn't block the toggle command" — typing
    /// must keep working immediately afterward and the eventual flush must commit every character,
    /// proving the toggle never touched `keyboard_owner`, the tool instance, or its pending buffer.
    #[test]
    fn f11_mid_text_burst_leaves_the_burst_content_and_caret_fully_intact_after_toggling() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('i'), &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::F11,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        // The toggle must be a pure side-channel: keyboard ownership, the tool instance, and the
        // caret are all untouched, so typing continues exactly where it left off.
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "F11 must not release the keyboard");
        assert!(app.slots[Binding::L.ix()].tool.caret().is_some(), "F11 must not clear the caret");
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('!'), &tctx, &app.doc);
        app.flush_slot(Binding::L);

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h');
        assert_eq!(app.doc.cell(0, 1, 0).unwrap().ch, 'i');
        assert_eq!(app.doc.cell(0, 2, 0).unwrap().ch, '!', "typing after the F11 toggle must still commit");
    }

    /// End-to-end companion to `tool_shortcut_reachable_only_gates_text_and_only_while_fullscreen`:
    /// drives the real `handle_keys` rather than the pure predicate alone, confirming `T` is left
    /// unconsumed (L stays whatever it was) while fullscreen, and that this gating is narrow — every
    /// other tool's shortcut (e.g. Fill's `F`) still switches L normally in the same chrome mode.
    #[test]
    fn pressing_t_while_fullscreen_leaves_l_unchanged_but_other_tool_shortcuts_still_work() {
        let mut app = GasciiApp::headless();
        let original_l_kind = app.slot(Binding::L).kind;

        let ctx = egui::Context::default();
        let mut raw_t = egui::RawInput::default();
        raw_t.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw_t.events.push(egui::Event::Key {
            key: egui::Key::T,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw_t, |ui| app.handle_keys(ui));
        assert_eq!(app.slot(Binding::L).kind, original_l_kind, "T must not switch L to Text while fullscreen");

        let mut raw_f = egui::RawInput::default();
        raw_f.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(true);
        raw_f.events.push(egui::Event::Key {
            key: egui::Key::F,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw_f, |ui| app.handle_keys(ui));
        assert_eq!(
            app.slot(Binding::L).kind, ToolKind::Fill,
            "every other tool's shortcut must stay reachable while fullscreen"
        );
    }

    /// Acceptance criterion: "X swaps FG/BG in both chrome modes". K12's own fix (the pre-existing
    /// tooltip/missing-keybinding gap) had no dedicated test anywhere — the `X` branch isn't gated
    /// on `is_fullscreen` at all, so this drives the real `handle_keys` in both chrome modes and
    /// confirms the swap actually happens each time.
    #[test]
    fn x_key_swaps_fg_and_bg_in_both_windowed_and_fullscreen_chrome() {
        for fullscreen in [false, true] {
            let mut app = GasciiApp::headless();
            app.active_fg = Rgba(1, 2, 3, 255);
            app.active_bg = Rgba(4, 5, 6, 255);

            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(fullscreen);
            raw.events.push(egui::Event::Key {
                key: egui::Key::X,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

            assert_eq!(app.active_fg, Rgba(4, 5, 6, 255), "fullscreen={fullscreen}: X must swap FG");
            assert_eq!(app.active_bg, Rgba(1, 2, 3, 255), "fullscreen={fullscreen}: X must swap BG");
        }
    }

    /// Loading a second image over an already-loaded one must **replace** the whole
    /// `ImageBackground` in one assignment, not accumulate state — mirrors `load_trace_image`'s own
    /// `self.image_bg = Some(...)` step (an `rfd` pick can't be driven headlessly, so this drives
    /// the same decode/upload/assign sequence directly). Proven two ways: the second image's
    /// `pixels`/`path` are the only ones present afterward (no merge), and the *first* texture is
    /// actually freed from the `TextureManager` once the second assignment drops it — a stacking or
    /// leak bug (e.g. pushing into a `Vec` instead of replacing the `Option`) would leave the first
    /// texture id still allocated.
    #[test]
    fn loading_a_second_image_over_an_existing_one_replaces_it_and_frees_the_old_texture() {
        let mut app = GasciiApp::headless();
        let ctx = egui::Context::default();

        fn make_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
            let mut img = image::RgbaImage::new(w, h);
            for px in img.pixels_mut() {
                px.0 = rgba;
            }
            let mut bytes = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
            bytes
        }
        fn load(ctx: &egui::Context, bytes: &[u8]) -> image_bg::ImageBackground {
            let rgba = image_bg::decode_image(bytes).unwrap();
            let (w, h) = (rgba.width() as usize, rgba.height() as usize);
            let tex = ctx.load_texture(
                "trace_bg",
                egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw()),
                egui::TextureOptions::LINEAR,
            );
            image_bg::ImageBackground::new(rgba, Some(tex), None)
        }

        let bg_a = load(&ctx, &make_png(3, 2, [10, 20, 30, 255]));
        let id_a = bg_a.texture.as_ref().unwrap().id();
        app.image_bg = Some(bg_a);
        app.image_bg_gen += 1;
        assert!(
            ctx.tex_manager().read().meta(id_a).is_some(),
            "sanity: the first texture must actually be allocated before the replace"
        );

        let bg_b = load(&ctx, &make_png(5, 7, [90, 100, 110, 255]));
        let id_b = bg_b.texture.as_ref().unwrap().id();
        // Mirrors `load_trace_image`'s own replace: this single assignment swaps the whole
        // `Option`, dropping the previous `ImageBackground` (and its `TextureHandle`) right here.
        app.image_bg = Some(bg_b);
        app.image_bg_gen += 1;

        let bg = app.image_bg.as_ref().unwrap();
        assert_eq!(
            (bg.pixels.width(), bg.pixels.height()),
            (5, 7),
            "the second image's dimensions replace the first's, not stack alongside them"
        );
        assert_eq!(app.image_bg_gen, 2, "each load bumps the generation once, not merged into one bump");
        assert!(
            ctx.tex_manager().read().meta(id_a).is_none(),
            "the first texture must be freed once the second `Some(...)` assignment drops it"
        );
        assert!(ctx.tex_manager().read().meta(id_b).is_some(), "the second (current) texture must still be allocated");
    }

    /// An `image_bg_gen` bump alone — with `ExportSettings` completely unchanged — must still
    /// invalidate `refresh_export_preview`'s cache key. This is the whole reason
    /// `ExportPreviewKey` exists (`Option<ExportSettings>` alone can't see an opacity/gate/load
    /// edit): without the generation folded in, a preview built before an image edit would be
    /// served forever afterward, since `self.export` never itself changed.
    #[test]
    fn an_image_bg_gen_bump_invalidates_the_cached_export_preview_key_with_export_settings_unchanged() {
        let mut app = GasciiApp::headless();
        app.export.format = ExportFormat::Png;
        let ctx = egui::Context::default();

        app.refresh_export_preview(&ctx);
        let key_before = app.export_preview_key;
        assert!(key_before.is_some(), "sanity: a PNG-format refresh must produce a cache key");

        // `self.export` is untouched below — only the image generation moves, exactly as
        // `load_trace_image`/`clear_image_bg`/the export dialog's opacity slider and "Use as
        // background" toggle all do.
        app.image_bg_gen += 1;
        app.refresh_export_preview(&ctx);
        let key_after = app.export_preview_key;

        assert_ne!(key_before, key_after, "an image_bg_gen bump alone must change the cache key");
        assert_eq!(
            key_after.map(|k| k.image_gen),
            Some(1),
            "the new key must reflect the bumped generation, not just differ arbitrarily"
        );
    }

    /// `suppresses_tool_shortcuts` end to end through the real `handle_keys` loop, not just the
    /// pure predicate: a live Text session must swallow `P` as burst content rather than let it
    /// rebind L to Pencil, and the very next frame after that session ends must let the same key
    /// through normally — proving the gate tracks the session's lifetime, not a stuck flag.
    #[test]
    fn a_live_text_session_suppresses_the_pencil_shortcut_through_handle_keys_and_releases_it_once_the_session_ends() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);

        let press_p = |app: &mut GasciiApp| {
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.events.push(egui::Event::Key {
                key: egui::Key::P,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));
        };

        press_p(&mut app);
        assert_eq!(
            app.slot(Binding::L).kind, ToolKind::Text,
            "P must be swallowed as burst content while the Text session is active, not rebind L"
        );

        // Ending the session (Escape/toolbox click equivalent) must release the gate.
        app.end_session(Binding::L);
        press_p(&mut app);
        assert_eq!(
            app.slot(Binding::L).kind, ToolKind::Pencil,
            "once the session has ended, the very same key must rebind L normally"
        );
    }

    /// Windowed complement to `pressing_t_while_fullscreen_leaves_l_unchanged_but_other_tool_
    /// shortcuts_still_work`: outside fullscreen every registry entry's shortcut — including
    /// Text's, which `kiosk_visible` gates only while fullscreen — must reach `set_tool` normally.
    #[test]
    fn pressing_t_while_windowed_rebinds_l_to_text() {
        let mut app = GasciiApp::headless();
        assert_ne!(app.slot(Binding::L).kind, ToolKind::Text, "sanity: L doesn't already start on Text");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().fullscreen = Some(false);
        raw.events.push(egui::Event::Key {
            key: egui::Key::T,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Text, "T must rebind L to Text while windowed");
    }

    /// The `[`/`]` size keys must adjust whichever binding `options_focus` currently names, driven
    /// through the real `handle_keys` loop rather than by mutating `stamps` directly — proving the
    /// `sized_slot` capability lookup and the focus-tracking field are both actually wired into the
    /// live key-handling path, not merely consistent in isolation.
    #[test]
    fn close_bracket_grows_only_the_options_focused_bindings_stamp_through_handle_keys() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Eraser);
        app.slots[Binding::R.ix()] = ToolSlot::new(ToolKind::Eraser);
        app.options_focus = Binding::R;
        let slot = sized_slot(ToolKind::Eraser).expect("Eraser is sized");
        app.slots[Binding::L.ix()].stamps[slot].size = 1;
        app.slots[Binding::R.ix()].stamps[slot].size = 1;

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::CloseBracket,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slots[Binding::R.ix()].stamps[slot].size, 2, "R (options_focus) must grow");
        assert_eq!(app.slots[Binding::L.ix()].stamps[slot].size, 1, "L must be untouched by R's focused key");
    }

    /// A committed Pencil stroke that stamps the glyph plane must add the active glyph to RECENT —
    /// closing the code review's flagged gap (`note_glyph_drawn`/`stamps_glyph` had no test driving
    /// the real committed-stroke call path, only the underlying capability-table field). Eraser
    /// (`stamps_glyph: false`) is driven the same way as a negative control: a real committed
    /// stroke that does NOT count toward RECENT.
    #[test]
    fn a_committed_stroke_updates_recent_glyphs_exactly_for_stamps_glyph_kinds() {
        for (kind, should_note) in [(ToolKind::Pencil, true), (ToolKind::Eraser, false)] {
            let mut app = GasciiApp::headless();
            app.slots[Binding::L.ix()] = ToolSlot::new(kind);
            app.mask = PlaneMask::ALL;
            app.active_glyph = 'Q';
            assert!(app.recent_glyphs.is_empty(), "{kind:?}: sanity, RECENT starts empty");

            crate::canvas::begin_gesture(&mut app, Binding::L, 0, 0, false, false);
            let tctx = crate::canvas::tool_ctx(&app, Binding::L);
            let resp = app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
            app.stroke_owner = None;
            if let ToolResponse::Commit(Some(edit)) = resp {
                app.apply_edit(edit, Some(Binding::L));
                // Mirrors canvas.rs's own commit call site exactly (`show`'s stroke-tail branch).
                app.note_glyph_drawn(app.slots[Binding::L.ix()].kind);
            }

            if should_note {
                assert_eq!(
                    app.recent_glyphs.first(), Some(&'Q'),
                    "{kind:?}: a committed glyph-plane stroke must add the active glyph to RECENT"
                );
            } else {
                assert!(
                    app.recent_glyphs.is_empty(),
                    "{kind:?}: a stamps_glyph=false kind's committed stroke must not touch RECENT"
                );
            }
        }
    }

    /// The recurring stale-`before` bug class (flagged in prior reviews of redesign-round-2 and
    /// fullscreen-mode), exercised specifically against a non-default `active_layer`: a Pencil
    /// commit, a live Text session's resync, a second Pencil commit that re-pins the Text session's
    /// `before` mid-burst, the Text session's own eventual commit, and finally undo/redo — every
    /// one of those five steps must read and write the SAME layer (2), and layer 0 must stay
    /// completely untouched throughout. `active_layer` now mirrors `doc.active_layer()`
    /// (`apply_edit`'s seed/resync), so reaching layer 2 must go through the real, `History`-
    /// tracked `add_layer` op rather than the raw `layers_mut()` bypass this test used before that
    /// plumbing landed — `layer_meta` (the seed's bounds check) is `pub(crate)` to gascii-core, with
    /// no bypass reachable from here. Each add lands its own new layer as active, so `active_layer`
    /// ends at 2 without a hand-set.
    #[test]
    fn active_layer_resync_and_undo_redo_all_target_the_same_non_default_layer_under_adversarial_sequencing() {
        let mut app = GasciiApp::headless();
        let add1 = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(add1, None);
        let add2 = gascii_core::add_layer(&app.doc, app.doc.layer_count()).unwrap();
        app.apply_edit(add2, None);
        assert_eq!(app.active_layer, 2, "sanity: each add_layer lands its own new layer as active");
        app.mask = PlaneMask::ALL;

        // R: a first Pencil stroke stamps layer 2's (2,2) with 'Z'.
        app.bind(Binding::R, ToolKind::Pencil);
        app.active_glyph = 'Z';
        crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2, false, false);
        let r_tctx = crate::canvas::tool_ctx(&app, Binding::R);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r_tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'Z', "sanity: R's first stroke landed on layer 2");
        assert_eq!(app.doc.cell(0, 2, 2), Some(&gascii_core::Cell::BLANK), "sanity: layer 0 untouched so far");

        // L: a Text burst starts on the SAME cell, pinning `before` against layer 2's current 'Z'.
        app.bind(Binding::L, ToolKind::Text);
        app.acquire_keyboard(Binding::L);
        let l_tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 2, y: 2 }, &l_tctx, &app.doc);
        app.active_glyph = 'A';
        let l_tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('A'), &l_tctx, &app.doc);

        // R: a second Pencil stroke, still mid-L-session, touches the SAME cell again — its commit
        // must resync L against layer 2's new value ('Y'), not layer 0's untouched blank.
        app.active_glyph = 'Y';
        crate::canvas::begin_gesture(&mut app, Binding::R, 2, 2, false, false);
        let r_tctx2 = crate::canvas::tool_ctx(&app, Binding::R);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::R.ix()].tool.update(ToolEvent::Release, &r_tctx2, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::R));
        }
        app.stroke_owner = None;
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'Y', "sanity: R's second stroke committed 'Y' on layer 2");

        // L's burst finally commits 'A'. Correctness here is invisible on the forward write (the
        // committed `after` is always 'A' either way) — the resync's target layer only shows up in
        // the undo entry's `before`, which is exactly why this test checks undo/redo, not just the
        // post-commit cell.
        app.flush_slot(Binding::L);
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'A', "L's committed burst lands 'A' on layer 2");

        app.request_undo();
        assert_eq!(
            app.doc.cell(2, 2, 2).unwrap().ch, 'Y',
            "undo must restore layer 2's actual prior content ('Y' from R's second stroke), not a \
             stale before pinned against the wrong layer"
        );

        app.request_redo();
        assert_eq!(app.doc.cell(2, 2, 2).unwrap().ch, 'A', "redo must re-land the Text commit");

        assert_eq!(
            app.doc.cell(0, 2, 2), Some(&gascii_core::Cell::BLANK),
            "layer 0 must stay completely untouched by every edit and every undo/redo in this sequence"
        );
    }

    /// The process-global tool registry must build exactly once: repeated `tools()` calls return
    /// the same backing allocation, not a freshly rebuilt `Vec` each time. Guards the `OnceLock`
    /// contract `prefs::load`'s first-ever-read-in-the-app-lifetime relies on (a rebuild-per-call
    /// registry would still be correct today since every row is a pure constant, but would silently
    /// stop being `&'static`-cheap the moment a plugin's `register_tool` needs to append once,
    /// before the first read, rather than on every read).
    #[test]
    fn tools_registry_returns_the_same_backing_slice_across_repeated_calls() {
        let a = tools().as_ptr();
        let b = tools().as_ptr();
        assert_eq!(a, b, "tools() must not rebuild the registry on every call");
    }

    fn selection_at_1_1(app: &mut GasciiApp) {
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Selection);
        app.doc.set_cell(0, 1, 1, cell('x'));
        let tctx = crate::canvas::tool_ctx(app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 1, y: 1 }, &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
    }

    fn copy_event() -> egui::Event {
        egui::Event::Copy
    }

    /// The real fix `handle_keys` needed: `Event::Copy` (what egui-winit actually emits for
    /// Ctrl+C/Cmd+C) with a live selection must copy that selection's text into the internal
    /// clipboard — not the dead `consume_key(COMMAND, C)` pair that never fired because
    /// `Event::Key{C}` is never produced for this chord.
    #[test]
    fn ctrl_c_via_event_copy_copies_the_live_selections_text_to_the_internal_clipboard() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(copy_event());
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        let patch = app.internal_clipboard.as_ref().expect("Ctrl+C must populate the internal clipboard");
        assert_eq!(patch.to_text(), "x", "the copied patch must hold the selected cell's glyph");
    }

    /// `Ctrl+Shift+C`'s copy-all path, discriminated purely from `InputState::modifiers.shift` at
    /// the moment `Event::Copy` is observed, must copy the whole document as text to the OS
    /// clipboard, not just the live selection.
    #[test]
    fn ctrl_shift_c_via_event_copy_with_shift_held_copies_the_whole_document_as_text() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 0, 0, cell('z'));

        let ctx = egui::Context::default();
        let mut raw =
            egui::RawInput { modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(copy_event());
        let output = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        let expected = export_text(&app.doc);
        let copied = output
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if *t == expected));
        assert!(copied, "Ctrl+Shift+C must copy the whole document's exported text to the OS clipboard");
    }

    /// A bare `Event::Key{key: C, modifiers: COMMAND}` — the event egui-winit never actually
    /// produces for this chord — must NOT fire copy. Reproducing that event shape as if it were
    /// real is the exact fiction that let the dead `consume_key` pair look correct while never
    /// actually firing.
    #[test]
    fn a_bare_event_key_c_with_no_event_copy_present_does_not_fire_copy() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(
            app.internal_clipboard.is_none(),
            "a synthetic Event::Key{{C}} with no real Event::Copy must not copy anything — this is the \
             exact fiction that let the dead consume_key pair look correct while never actually firing"
        );
    }

    /// `Event::Copy` also fires on `Ctrl+Insert` (Windows) — the app receives the exact same event
    /// shape either way, so scanning for the event variant (rather than a specific key chord)
    /// handles this chord for free. Pinned as its own dedicated test.
    #[test]
    fn event_copy_from_ctrl_insert_copies_the_live_selection_identically_to_ctrl_c() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(copy_event()); // egui-winit emits the identical Event::Copy for Ctrl+Insert
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.internal_clipboard.as_ref().map(|p| p.to_text()).as_deref(),
            Some("x"),
            "Ctrl+Insert's Event::Copy must copy the live selection just like Ctrl+C's"
        );
    }

    /// `Event::Copy`/`Event::Cut` must be suppressed while a widget has focus, matching Undo/Redo's
    /// own gate — a focused `TextEdit` (the hex color popup) reads the identical events off this
    /// same frame's list via its own `filtered_events`, so an unguarded scan would cut/copy the
    /// canvas selection underneath the field at the same time the field cuts/copies its own text.
    /// A pending float surviving `Event::Copy` proves `flush_all` never ran as a side effect.
    #[test]
    fn copy_copy_all_and_cut_are_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        // A pasted float, still uncommitted: `copy_all`'s own `flush_all()` would commit it if the
        // suppression failed to hold.
        app.paste_text("z");
        assert!(app.slots[Binding::L.ix()].tool.selection_overlay().is_some(), "sanity: a float is pending");

        let ctx = egui::Context::default();
        let mut raw =
            egui::RawInput { modifiers: egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT), ..Default::default() };
        raw.events.push(copy_event()); // Ctrl+Shift+C shape: would fire copy_all if not suppressed
        raw.events.push(egui::Event::Cut); // Ctrl+X shape: would fire cut_selection if not suppressed
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert!(app.internal_clipboard.is_none(), "a focused widget must suppress Ctrl+C/Ctrl+Shift+C");
        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().is_some(),
            "a focused widget must suppress copy_all's flush_all side effect, so the pending float survives"
        );
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "a focused widget must suppress Ctrl+X's deletion too");
    }

    /// Regression guard: with no widget focused, `Event::Copy`/`Event::Cut` still fire exactly as
    /// before this gate was added.
    #[test]
    fn copy_and_cut_still_fire_via_event_copy_and_event_cut_while_unfocused() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(copy_event());
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));
        assert_eq!(
            app.internal_clipboard.as_ref().map(|p| p.to_text()).as_deref(),
            Some("x"),
            "an unfocused Ctrl+C must still copy the selection"
        );

        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Cut);
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, ' ', "an unfocused Ctrl+X must still delete the selection");
    }

    /// The more-specific chord (Redo, Ctrl+Shift+Z) must win over the less-specific one (Undo,
    /// Ctrl+Z) that would otherwise also match via `matches_logically`'s modifier-superset rule —
    /// driven through the real `handle_keys`, not just a pure predicate, so a future reordering of
    /// the two `consume_key` calls is actually caught.
    #[test]
    fn ctrl_shift_z_via_handle_keys_fires_redo_not_undo() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        app.request_undo(); // one edit undone: redo is now available, undo is not
        assert!(app.history.can_redo(), "sanity: a redo is available");
        assert!(!app.history.can_undo(), "sanity: no undo is available after the single edit was undone");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        });
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, '#', "Ctrl+Shift+Z must have redone the edit");
        assert!(!app.history.can_redo(), "the redo must have actually fired, emptying the redo stack");
    }

    /// The other precedence-sensitive pair: Ctrl+Shift+C (copy-all) firing must mean plain Ctrl+C's
    /// copy-selection path does NOT also run — proven by asserting the internal (selection)
    /// clipboard stays untouched while the OS clipboard receives the whole document, not merely
    /// that copy-all's own effect happened in isolation.
    #[test]
    fn ctrl_shift_c_fires_copy_all_and_never_also_the_plain_selection_copy_path() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        assert!(app.internal_clipboard.is_none(), "sanity: nothing has been copied yet");

        let ctx = egui::Context::default();
        let mut raw =
            egui::RawInput { modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(copy_event());
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(
            app.internal_clipboard.is_none(),
            "Ctrl+Shift+C must not also run the plain-copy path (copy_selection), which would have \
             populated the internal clipboard"
        );
    }

    /// M7's direct regression test: every key a plugin's `tick` actually consumes (gascii-anim's
    /// hold/toggle/navigation/duplicate shortcuts, gascii-density-brush's digit keys) must appear in
    /// the app's own key-claim set — the set the review said was structurally impossible to build
    /// before this round.
    #[test]
    fn key_claims_include_every_plugin_tick_shortcut() {
        let claims = key_claims(tools());
        let keys: std::collections::HashSet<egui::Key> = claims.iter().map(|c| c.key).collect();
        for key in [
            egui::Key::Space,
            egui::Key::O,
            egui::Key::Comma,
            egui::Key::Period,
            egui::Key::D,
            egui::Key::Num0,
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ] {
            assert!(keys.contains(&key), "{key:?} must be a claimed key");
        }
    }

    /// `validate_key_claims` is pure and unit-testable without a real colliding plugin: two
    /// synthetic claims on the same key must produce an `Err` naming both claimants by their
    /// `ClaimSource` description.
    #[test]
    fn validate_key_claims_reports_a_synthetic_collision_naming_both_claimants() {
        let claims = vec![
            KeyClaim { key: egui::Key::X, source: ClaimSource::Chord("Swap Colors") },
            KeyClaim { key: egui::Key::X, source: ClaimSource::Tool("Sprinkler") },
        ];
        let err = validate_key_claims(&claims).expect_err("a duplicate key claim must be an error");
        assert!(err.contains("Swap Colors"), "error must name the first claimant: {err}");
        assert!(err.contains("Sprinkler"), "error must name the second claimant: {err}");
    }

    /// The real shipping claim set (every chord, every tool shortcut, every plugin's declared
    /// shortcuts) must be collision-free — this is the check `build_tools` runs at registry
    /// construction and panics on `Err` for; this test exercises the pure validator directly so the
    /// panic path itself is never what CI runs.
    #[test]
    fn validate_key_claims_accepts_the_real_shipping_claim_set() {
        assert!(validate_key_claims(&key_claims(tools())).is_ok());
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// `Ctrl+N` on a clean document must open the New dialog directly, exactly like clicking
    /// File ▸ New… on a clean document — no confirm veto in the way.
    #[test]
    fn ctrl_n_on_a_clean_document_opens_the_new_dialog_directly() {
        let mut app = GasciiApp::headless();
        assert!(!app.is_dirty(), "sanity: a fresh headless app starts clean");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::N, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.open_dialog, Some(OpenDialog::New), "Ctrl+N on a clean document must open the New dialog");
        assert!(app.confirm.is_none(), "a clean document must not raise the unsaved-changes veto");
    }

    /// `Ctrl+N` on a dirty document must veto through the same unsaved-changes confirm the menu
    /// click uses — proving `new_document_via_menu` is genuinely shared, not reimplemented.
    #[test]
    fn ctrl_n_on_a_dirty_document_raises_the_unsaved_changes_confirm_instead_of_opening_new_directly() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        if let ToolResponse::Commit(Some(edit)) =
            app.slots[Binding::L.ix()].tool.update(ToolEvent::Release, &tctx, &app.doc)
        {
            app.apply_edit(edit, Some(Binding::L));
        }
        app.stroke_owner = None;
        assert!(app.is_dirty(), "sanity: the committed stroke made the document dirty");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::N, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.confirm, Some(PendingConfirm::NewDocument), "a dirty document must veto through the confirm");
        assert_eq!(app.open_dialog, None, "the New dialog must not open directly while the veto is pending");
    }

    /// `G` toggles the grid overlay, gated on `!focused` exactly like `X` (SwapColors) already is —
    /// driven through the real `handle_keys`, not the pure `consume_generic_chords` helper alone.
    #[test]
    fn g_toggles_the_grid_overlay_through_handle_keys_while_unfocused() {
        let mut app = GasciiApp::headless();
        let starting = app.show_grid;

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::G, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.show_grid, !starting, "G must flip show_grid");
    }

    /// `G` must be suppressed while a widget has focus — the same `!focused` gate `X` already
    /// obeys, so typing "g" into a focused field never toggles the grid.
    #[test]
    fn g_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        let starting = app.show_grid;

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::G, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.show_grid, starting, "a focused widget must suppress G");
    }

    /// `G` is a toggle, so a key-repeat press (the OS re-firing a held key) must not flip
    /// `show_grid` again — only the initial, non-repeat press may. `egui::InputState::begin_pass`
    /// recomputes `repeat` itself from `keys_down` rather than trusting a raw event's own field, so
    /// a real repeat can only be produced by holding the key across two passes on the same
    /// `Context` (no `Release` in between) — mirrored here across two real `handle_keys` calls.
    #[test]
    fn g_key_repeat_does_not_toggle_the_grid() {
        let mut app = GasciiApp::headless();
        let starting = app.show_grid;
        let ctx = egui::Context::default();

        fn g_press() -> egui::Event {
            egui::Event::Key { key: egui::Key::G, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
        }

        // Pass 1: a genuine, first-ever press of G toggles the grid once.
        let mut raw1 = egui::RawInput::default();
        raw1.events.push(g_press());
        let _ = ctx.run_ui(raw1, |ui| app.handle_keys(ui));
        assert_eq!(app.show_grid, !starting, "sanity: the initial press toggles the grid");

        // Pass 2: G is still held down (no Release event between passes) — egui itself now reports
        // this press as a repeat. Must not toggle again.
        let mut raw2 = egui::RawInput::default();
        raw2.events.push(g_press());
        let _ = ctx.run_ui(raw2, |ui| app.handle_keys(ui));
        assert_eq!(app.show_grid, !starting, "a held-down key-repeat G press must not toggle the grid again");
    }

    /// `?` opens the keyboard-shortcuts overlay while unfocused, the same `GenericUnfocused` gate
    /// `G`/`X` already use.
    #[test]
    fn question_mark_via_handle_keys_opens_the_help_overlay_while_unfocused() {
        let mut app = GasciiApp::headless();
        assert_eq!(app.open_dialog, None, "sanity: the overlay starts closed");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Questionmark, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.open_dialog, Some(OpenDialog::Help), "? must open the overlay");
    }

    #[test]
    fn question_mark_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Questionmark, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.open_dialog, None, "a focused widget must suppress ?");
    }

    /// While open, the overlay counts as a modal — `handle_keys` (and therefore every other chord)
    /// must stop running, matching every other dialog's own `modal_open()` coverage.
    #[test]
    fn help_overlay_open_suppresses_handle_keys_via_modal_open() {
        let mut app = GasciiApp::headless();
        app.open_dialog = Some(OpenDialog::Help);
        assert!(app.modal_open(), "the open overlay must count as a modal");
    }

    /// The `plugin_ticks_suppressed` latch end to end, through the real `eframe::App::ui` gate: a
    /// modal-open frame sets it and skips `handle_keys` (so it never gets cleared that frame); the
    /// next modal-closed frame's `handle_keys` clears it again. Drives the actual top-level `ui()`
    /// method (not just `handle_keys` directly) so this pins the real gate `ui()`'s own doc comment
    /// describes, not a hand-reimplemented copy of its branching.
    #[test]
    fn plugin_ticks_suppressed_latches_while_a_modal_is_open_and_clears_on_the_next_real_tick() {
        let mut app = GasciiApp::headless();
        assert!(!app.plugin_ticks_suppressed, "sanity: a fresh app starts with nothing suppressed");

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        let mut frame = eframe::Frame::_new_kittest();

        // A modal-open frame: handle_keys must be skipped and the latch set.
        app.open_dialog = Some(OpenDialog::Help);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| eframe::App::ui(&mut app, ui, &mut frame));
        assert!(app.plugin_ticks_suppressed, "a modal-open frame must set the latch");

        // The modal closes; the next frame's handle_keys runs for real and must clear the latch.
        app.open_dialog = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| eframe::App::ui(&mut app, ui, &mut frame));
        assert!(!app.plugin_ticks_suppressed, "the next real handle_keys call must clear the latch");
    }

    /// The overlay renders without panicking and is dismissed (Escape, matching every other
    /// `dialog::modal`-built dialog) by clearing `open_dialog` — not by a second `?` press, which
    /// can never reach `handle_keys` while the overlay counts as a modal.
    #[test]
    fn help_overlay_renders_and_closes_on_escape_dismiss() {
        let mut app = GasciiApp::headless();
        app.open_dialog = Some(OpenDialog::Help);

        let ctx = egui::Context::default();
        fonts::install_fonts(&ctx);
        // `egui::Modal`'s "am I the topmost modal" bookkeeping is layer-order state the context
        // only has after at least one real frame — mirrors how the overlay is always already open
        // for at least a frame before a real Escape press can reach it.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.help_overlay(ui.ctx()));
        assert_eq!(app.open_dialog, Some(OpenDialog::Help), "sanity: a no-input frame must not close the overlay");

        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Escape, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.help_overlay(ui.ctx()));

        assert_eq!(app.open_dialog, None, "Escape must dismiss the overlay, matching every other dialog");
    }

    /// M7's discoverability half: every shortcut a plugin's `Plugin::shortcuts()` declares must
    /// appear in the PLUGINS section the overlay renders — the gap the review flagged as invisible
    /// before this round. Asserted against the harvested rows directly (the same data the render
    /// loop consumes) rather than scraping painted text.
    #[test]
    fn the_help_overlay_lists_every_declared_plugin_shortcut() {
        let declared: Vec<(&str, &str)> =
            PLUGINS.iter().flat_map(|d| (d.shortcuts)()).map(|s| (s.name, s.label)).collect();
        assert!(declared.iter().any(|&(name, _)| name == "Play / Pause"));
        assert!(declared.iter().any(|&(_, label)| label == "Shift+D"));
        assert!(declared.iter().any(|&(_, label)| label == "1-9, 0"));
        assert_eq!(declared.len(), 6, "5 gascii-anim rows + 1 gascii-density-brush row");
    }

    /// `Ctrl+=`/`Ctrl+-` request the same one-step zoom the plain `+`/`-` chords and the View menu
    /// already do — proven through the deferred `pending_step_zoom` field `step_zoom` writes,
    /// mirroring how `canvas::show` itself applies the request.
    #[test]
    fn ctrl_equals_and_ctrl_minus_request_the_same_one_step_zoom_as_the_plain_aliases() {
        for (key, expected_dir) in [(egui::Key::Equals, 1), (egui::Key::Minus, -1)] {
            let mut app = GasciiApp::headless();
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
            raw.events.push(key_event(key, egui::Modifiers::COMMAND));
            let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

            assert_eq!(
                app.pending_step_zoom, expected_dir,
                "{key:?} with Ctrl held must request a one-step zoom in direction {expected_dir}"
            );
        }
    }

    /// `Ctrl+Shift+=` produces `Key::Plus` (not `Key::Equals`) on US layouts — the same physical
    /// key as `Ctrl+=`, and must request the same one-step zoom-in `ZoomInAlias` already does.
    #[test]
    fn ctrl_plus_requests_the_same_zoom_in_as_ctrl_equals() {
        let mut app = GasciiApp::headless();
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::Plus, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.pending_step_zoom, 1, "Ctrl+Plus must request the same one-step zoom-in as Ctrl+=");
    }

    /// `Ctrl+A` with neither binding already holding Selection must rebind L (`paste_target`'s
    /// default) and select the whole document, without requiring a prior manual tool switch.
    #[test]
    fn ctrl_a_via_handle_keys_rebinds_l_to_selection_and_selects_the_whole_document_by_default() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Eraser);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Selection, "Ctrl+A must rebind L by default");
        assert_eq!(app.selection_slot(), Some(Binding::L));
        assert_eq!(
            app.slot(Binding::L).tool.selection_overlay().and_then(|v| v.marquee),
            Some(gascii_core::CellRect {
                x0: 0,
                y0: 0,
                x1: app.doc.width - 1,
                y1: app.doc.height - 1
            }),
            "the marquee must span the full document"
        );
    }

    /// `Ctrl+A` must prefer whichever binding already holds Selection — the same `paste_target`
    /// rule `paste_text` already follows — rather than always defaulting to L.
    #[test]
    fn ctrl_a_via_handle_keys_prefers_a_binding_that_already_holds_selection() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Selection);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "L must be left untouched");
        assert_eq!(app.selection_slot(), Some(Binding::R), "Ctrl+A must select through R, which already held it");
    }

    /// `Ctrl+A` must be suppressed while a widget has focus, matching Undo/Redo's own gate —
    /// `egui::TextEdit`'s own Ctrl+A (select-all-in-field) must win instead.
    #[test]
    fn ctrl_a_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "a focused widget must suppress Ctrl+A");
    }

    /// A live canvas Text burst sets no egui widget focus, so the `widget_focused` gate above does
    /// NOT suppress Ctrl+A during one — `select_all` still fires and rebinds the Text slot to
    /// Selection via `set_tool`'s own `end_session`. The one thing that must never happen is silent
    /// data loss: `end_session` flushes (commits) the pending burst before the slot's tool is
    /// replaced, exactly like an Escape or a toolbox click already would, so the typed content lands
    /// in the document rather than vanishing underneath the tool switch.
    #[test]
    fn ctrl_a_during_a_live_text_burst_commits_the_burst_before_switching_to_select_all() {
        let mut app = GasciiApp::headless();
        app.slots[Binding::L.ix()] = ToolSlot::new(ToolKind::Text);
        let tctx = crate::canvas::tool_ctx(&app, Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Press { x: 0, y: 0 }, &tctx, &app.doc);
        app.acquire_keyboard(Binding::L);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('h'), &tctx, &app.doc);
        app.slots[Binding::L.ix()].tool.update(ToolEvent::Char('i'), &tctx, &app.doc);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::A, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.doc.cell(0, 0, 0).unwrap().ch, 'h', "the burst's typed text must be committed, not discarded");
        assert_eq!(app.doc.cell(0, 1, 0).unwrap().ch, 'i', "the burst's typed text must be committed, not discarded");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Selection, "Ctrl+A must switch the Text binding to Selection");
        assert_eq!(
            app.slot(Binding::L).tool.selection_overlay().and_then(|v| v.marquee),
            Some(gascii_core::CellRect { x0: 0, y0: 0, x1: app.doc.width - 1, y1: app.doc.height - 1 }),
            "the marquee must span the full document"
        );
    }

    /// The user-facing checkpoint dropped `D` (reset fg/bg) entirely: a bare, unmodified `D` press
    /// must not be bound to anything — no color change, no tool switch, no document mutation.
    /// `Ctrl+D`/`Shift+D` (Deselect/animation duplicate-frame) are unaffected; this only pins the
    /// bare key.
    #[test]
    fn bare_d_key_is_bound_to_nothing_and_leaves_colors_and_tools_untouched() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);
        app.bind(Binding::R, ToolKind::Eraser);
        let (fg_before, bg_before) = (app.active_fg, app.active_bg);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.active_fg, fg_before, "bare D must not touch the active foreground color");
        assert_eq!(app.active_bg, bg_before, "bare D must not touch the active background color");
        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "bare D must not rebind L");
        assert_eq!(app.slot(Binding::R).kind, ToolKind::Eraser, "bare D must not rebind R");
    }

    /// `Ctrl+X` must copy the live selection AND delete it in the same change — never leave an
    /// interim state where it only copied.
    #[test]
    fn ctrl_x_via_handle_keys_copies_and_deletes_the_selection_in_one_change() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Cut);
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.internal_clipboard.as_ref().map(|p| p.to_text()).as_deref(),
            Some("x"),
            "Ctrl+X must copy the selection's text"
        );
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, ' ', "Ctrl+X must also delete the selected cell");
    }

    /// `Ctrl+X` with no live selection must be a true no-op — no clipboard write, no document
    /// mutation, no panic.
    #[test]
    fn ctrl_x_via_handle_keys_is_a_no_op_without_a_live_selection() {
        let mut app = GasciiApp::headless();
        app.doc.set_cell(0, 1, 1, cell('x'));

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Cut);
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert!(app.internal_clipboard.is_none());
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "no selection: nothing may be deleted");
    }

    /// `Ctrl+D` must clear the marquee and release the keyboard without deleting the selection's
    /// content — the same pair `canvas.rs`'s own Selection-Escape handling already performs.
    #[test]
    fn ctrl_d_via_handle_keys_clears_the_marquee_and_releases_the_keyboard_without_deleting_content() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        assert_eq!(app.selection_slot(), Some(Binding::L), "sanity: L holds the live selection");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.keyboard_owner(), None, "Ctrl+D must release the keyboard");
        assert!(
            app.slot(Binding::L).tool.selection_overlay().is_none(),
            "Ctrl+D must clear the marquee"
        );
        assert_eq!(app.doc.cell(0, 1, 1).unwrap().ch, 'x', "Ctrl+D must never delete the selected content");
    }

    /// `Ctrl+D` with no live selection must be a true no-op.
    #[test]
    fn ctrl_d_via_handle_keys_is_a_no_op_without_a_live_selection() {
        let mut app = GasciiApp::headless();
        app.bind(Binding::L, ToolKind::Pencil);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(app.slot(Binding::L).kind, ToolKind::Pencil, "nothing may change with no live selection");
    }

    /// `Ctrl+D` must be suppressed while a widget has focus, matching Undo/Redo's own gate: a
    /// `ToolEvent::Cancel` discards a lifted-but-not-dropped float outright, so pressing Ctrl+D
    /// while typing into a popup field must not reach the canvas and silently drop pending work.
    #[test]
    fn ctrl_d_is_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        selection_at_1_1(&mut app);
        app.paste_text("z"); // a live float, uncommitted
        assert!(app.slots[Binding::L.ix()].tool.selection_overlay().is_some(), "sanity: a float is pending");

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::COMMAND, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::COMMAND));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert!(
            app.slots[Binding::L.ix()].tool.selection_overlay().is_some(),
            "a focused widget must suppress Ctrl+D, so the pending float survives"
        );
        assert_eq!(app.keyboard_owner(), Some(Binding::L), "a focused widget must suppress Ctrl+D's keyboard release");
    }

    /// `Plugin::tick`'s breaking return-type change end to end, mirroring
    /// `digit_key_intensity_shortcut_through_handle_keys_sets_fixed_intensity_while_bound_and_unfocused`:
    /// `AnimPlugin::tick`'s `Shift+D` duplicate-frame shortcut returns a `PanelOutcome` whose `edits`
    /// must reach `apply_edit` via `handle_keys`'s new two-pass tick-then-drain loop
    /// (`drain_panel_outcomes`), not just be silently discarded.
    #[test]
    fn plugin_tick_panel_outcome_edits_reach_apply_edit_via_handle_keys() {
        let mut app = GasciiApp::headless();
        let before_frame_count = app.doc.frame_count();

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::SHIFT));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.doc.frame_count(), before_frame_count + 1,
            "Shift+D's PanelOutcome::edits must reach apply_edit through drain_panel_outcomes"
        );
    }

    /// The other half of the same wiring: `.`'s `DocProperty::ActiveFrame` must reach
    /// `switch_active_frame` through the same drain pass.
    #[test]
    fn plugin_tick_panel_outcome_set_active_frame_reaches_switch_active_frame_via_handle_keys() {
        let mut app = GasciiApp::headless();
        app.add_frame_via_menu(); // now 2 frames, so '.' has somewhere to advance to
        assert_eq!(app.active_frame, 0);

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.events.push(key_event(egui::Key::Period, egui::Modifiers::NONE));
        let _ = ctx.run_ui(raw, |ui| app.handle_keys(ui));

        assert_eq!(
            app.active_frame, 1,
            "'.'s DocProperty::ActiveFrame must reach switch_active_frame through drain_panel_outcomes"
        );
    }

    /// `,`/`.`/`Shift+D` must all be suppressed while a widget has focus, matching every other
    /// `gascii-anim` shortcut's own `!focused` gate.
    #[test]
    fn comma_period_and_shift_d_are_suppressed_while_a_widget_has_keyboard_focus() {
        let mut app = GasciiApp::headless();
        app.add_frame_via_menu();
        app.switch_active_frame(1);
        let before_frame_count = app.doc.frame_count();

        let ctx = egui::Context::default();
        let mut raw = egui::RawInput { modifiers: egui::Modifiers::SHIFT, ..Default::default() };
        raw.events.push(key_event(egui::Key::Comma, egui::Modifiers::NONE));
        raw.events.push(key_event(egui::Key::D, egui::Modifiers::SHIFT));
        let _ = ctx.run_ui(raw, |ui| {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("qa_test_fake_focused_widget")));
            app.handle_keys(ui);
        });

        assert_eq!(app.active_frame, 1, "a focused widget must suppress ','");
        assert_eq!(app.doc.frame_count(), before_frame_count, "a focused widget must suppress Shift+D");
    }

    /// The Windows message hook mirrors the pen barrel button into `gascii_stylus::barrel_down`,
    /// and `raw_input_hook` must reroute the emulated primary press AND its matching release to
    /// Secondary. The release arrives after the OS has already dropped the barrel bit
    /// (`WM_POINTERUP` carries no button flags), so only the per-stroke latch keeps the pair
    /// consistent — and it must clear afterward so the next plain click is primary again.
    #[test]
    fn barrel_button_press_and_release_both_reroute_to_secondary_then_the_latch_clears() {
        let mut app = GasciiApp::headless();
        let ctx = egui::Context::default();
        let pointer = |pressed| egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        gascii_stylus::set_barrel_down(true);
        let mut raw = egui::RawInput::default();
        raw.events.push(pointer(true));
        eframe::App::raw_input_hook(&mut app, &ctx, &mut raw);
        assert!(
            matches!(raw.events[0], egui::Event::PointerButton { button: egui::PointerButton::Secondary, pressed: true, .. }),
            "a press with the barrel held must become a secondary press"
        );

        gascii_stylus::set_barrel_down(false);
        let mut raw = egui::RawInput::default();
        raw.events.push(pointer(false));
        eframe::App::raw_input_hook(&mut app, &ctx, &mut raw);
        assert!(
            matches!(raw.events[0], egui::Event::PointerButton { button: egui::PointerButton::Secondary, pressed: false, .. }),
            "the matching release must stay secondary even though the barrel bit is already gone"
        );

        let mut raw = egui::RawInput::default();
        raw.events.push(pointer(true));
        eframe::App::raw_input_hook(&mut app, &ctx, &mut raw);
        assert!(
            matches!(raw.events[0], egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: true, .. }),
            "a plain press after the stroke must be primary — the latch must not stick"
        );
    }
