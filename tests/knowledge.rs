//! Integration tests for the versioned Manim knowledge profile system
//! (DESIGN §5.4): loading, validation, overlay semantics, and the
//! star-import exports bridge.

use qual::knowledge::{
    self, AcceptedTarget, ForkBlocker, KnowledgeError, ProfileDocument, SceneMembershipEffect,
    SymbolKind, apply_overlay,
};

const UPSTREAM: &str = "upstream_0_20";
const LOCAL_OVERLAY: &str = "local_0_20_1_4d25c031";

#[test]
fn upstream_profile_loads_and_validates() {
    let profile = knowledge::load(UPSTREAM).expect("shipped profile must load");
    assert_eq!(profile.name, UPSTREAM);
    assert_eq!(profile.schema_version, 1);
    assert_eq!(profile.manim_version, ">=0.20,<0.21");
    assert!(profile.source_digest.starts_with("sha256:"));
    assert_eq!(profile.base_profile, None);
    assert!(!profile.symbols.is_empty());
    assert!(!profile.exports.is_empty());
}

#[test]
fn v0_20_alias_resolves_to_upstream() {
    let via_alias = knowledge::load("v0_20").expect("alias must load");
    assert_eq!(via_alias.name, UPSTREAM);
}

#[test]
fn list_available_contains_upstream_and_local_overlay() {
    let names = knowledge::list_available();
    assert!(names.contains(&UPSTREAM), "available: {names:?}");
    assert!(names.contains(&LOCAL_OVERLAY), "available: {names:?}");
}

#[test]
fn upstream_profile_carries_no_fork_only_symbols() {
    // The three fork-only `manim.constants` additions live in the overlay;
    // upstream provenance stays clean (drift-checked against the base
    // commit `4d25c031` via `git archive`).
    let profile = knowledge::load(UPSTREAM).expect("load");
    for name in ["CAIRO_ANTIALIAS_MODES", "VIDEO_ENCODERS", "X264_PRESETS"] {
        assert!(profile.resolve_export(name).is_none(), "{name}");
        assert!(
            profile.symbol(&format!("manim.constants.{name}")).is_none(),
            "{name}"
        );
    }
}

#[test]
fn local_fork_overlay_loads_end_to_end() {
    // `knowledge-profile = "local_0_20_1_4d25c031"` resolves the shipped
    // overlay against its upstream base: fork-only additions are present,
    // untouched upstream facts carry over.
    let profile = knowledge::load(LOCAL_OVERLAY).expect("shipped overlay must load");
    assert_eq!(profile.name, LOCAL_OVERLAY);
    assert_eq!(profile.base_profile.as_deref(), Some(UPSTREAM));

    let upstream = knowledge::load(UPSTREAM).expect("load base");
    assert_ne!(
        profile.source_digest, upstream.source_digest,
        "overlay digest covers the fork working tree, not the clean base"
    );

    for name in ["CAIRO_ANTIALIAS_MODES", "VIDEO_ENCODERS", "X264_PRESETS"] {
        let (id, entry) = profile.resolve_export(name).expect(name);
        assert_eq!(id, format!("manim.constants.{name}"));
        assert_eq!(entry.kind, SymbolKind::Constant, "{name}");
    }

    // Upstream facts are inherited unchanged.
    let create = profile
        .symbol("manim.animation.creation.Create")
        .expect("Create inherited from base");
    assert_eq!(
        create
            .effects
            .as_ref()
            .and_then(|effects| effects.introducer),
        Some(true)
    );
    assert!(profile.symbol("manim.scene.scene.Scene.add").is_some());
}

#[test]
fn unknown_profile_is_a_typed_error() {
    let error = knowledge::load("upstream_9_99").expect_err("unknown name must fail");
    match error {
        KnowledgeError::UnknownProfile { name, available } => {
            assert_eq!(name, "upstream_9_99");
            assert!(available.contains(UPSTREAM));
        }
        other => panic!("expected UnknownProfile, got: {other:?}"),
    }
}

#[test]
fn loading_twice_is_deterministic() {
    let first = knowledge::load(UPSTREAM).expect("first load");
    let second = knowledge::load(UPSTREAM).expect("second load");
    assert_eq!(first, second);
}

#[test]
fn create_maps_to_vmobject_introducer_animation() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (id, entry) = profile
        .resolve_export("Create")
        .expect("Create is exported");
    assert_eq!(id, "manim.animation.creation.Create");
    assert_eq!(entry.kind, SymbolKind::Animation);
    assert_eq!(entry.accepted_target, Some(AcceptedTarget::Vmobject));
    let effects = entry.effects.as_ref().expect("Create has curated effects");
    assert_eq!(effects.introducer, Some(true));
    assert_eq!(effects.remover, Some(false));
}

#[test]
fn fade_out_is_a_remover_and_fade_in_an_introducer() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (_, fade_out) = profile.resolve_export("FadeOut").expect("FadeOut exported");
    let effects = fade_out.effects.as_ref().expect("effects");
    assert_eq!(effects.remover, Some(true));
    assert_eq!(effects.introducer, Some(false));

    let (_, fade_in) = profile.resolve_export("FadeIn").expect("FadeIn exported");
    let effects = fade_in.effects.as_ref().expect("effects");
    assert_eq!(effects.introducer, Some(true));
    assert_eq!(effects.remover, Some(false));
}

#[test]
fn opengl_only_meshes_carry_the_renderer_requirement_fact() {
    // The MLR123 curation: `Object3D` / `Mesh` (renderer/shader.py) and
    // the `OpenGLSurface` family (mobject/opengl/) are OpenGL-only scene
    // objects — Scene.add diverts Object3D into Scene.meshes only under
    // RendererType.OPENGL, and the Cairo camera's type_or_raise rejects
    // every OpenGLMobject-rooted class (scene.py, camera/camera.py).
    let profile = knowledge::load(UPSTREAM).expect("load");
    for (id, kind) in [
        ("manim.renderer.shader.Object3D", SymbolKind::Mobject),
        ("manim.renderer.shader.Mesh", SymbolKind::Mobject),
        ("manim.renderer.shader.FullScreenQuad", SymbolKind::Mobject),
        (
            "manim.mobject.opengl.opengl_surface.OpenGLSurface",
            SymbolKind::Mobject,
        ),
        (
            "manim.mobject.opengl.opengl_surface.OpenGLSurfaceGroup",
            SymbolKind::Mobject,
        ),
        (
            "manim.mobject.opengl.opengl_surface.OpenGLTexturedSurface",
            SymbolKind::Mobject,
        ),
        (
            "manim.mobject.opengl.opengl_three_dimensions.OpenGLSurfaceMesh",
            SymbolKind::Vmobject,
        ),
    ] {
        let entry = profile.symbol(id).unwrap_or_else(|| panic!("{id} curated"));
        assert_eq!(entry.kind, kind, "{id}");
        let compat = entry.renderer.as_ref().unwrap_or_else(|| panic!("{id}"));
        assert_eq!(compat.opengl_only_mesh, Some(true), "{id}");
        // The mesh fact deliberately does not set `cairo: false`: the
        // failure is display-time (MLR123's territory), so the generic
        // call-site rule MLR107 must stay silent on constructions.
        assert_eq!(compat.cairo, None, "{id}");
        assert_eq!(compat.opengl, None, "{id}");
    }
    // These classes are not star-exported from `manim` in 0.20
    // (manim/__init__.py omits the opengl_surface / shader modules).
    for name in ["Object3D", "Mesh", "OpenGLSurface", "OpenGLSurfaceMesh"] {
        assert!(profile.resolve_export(name).is_none(), "{name}");
    }
    // Cairo-capable 3D mobjects must never carry the mesh fact.
    let surface = profile
        .symbol("manim.mobject.three_d.three_dimensions.Surface")
        .expect("Surface curated");
    assert!(
        surface
            .renderer
            .as_ref()
            .and_then(|compat| compat.opengl_only_mesh)
            .is_none(),
        "Surface is Cairo-capable"
    );
}

#[test]
fn full_screen_rectangle_coverage_chain_is_curated() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (id, full_screen) = profile
        .resolve_export("FullScreenRectangle")
        .expect("FullScreenRectangle exported");
    assert_eq!(id, "manim.mobject.frame.FullScreenRectangle");
    assert_eq!(full_screen.kind, SymbolKind::Vmobject);
    assert_eq!(
        full_screen.bases,
        vec!["manim.mobject.frame.ScreenRectangle".to_owned()]
    );
    let (_, screen) = profile
        .resolve_export("ScreenRectangle")
        .expect("ScreenRectangle exported");
    assert_eq!(screen.kind, SymbolKind::Vmobject);
    assert_eq!(
        screen.bases,
        vec!["manim.mobject.geometry.polygram.Rectangle".to_owned()]
    );
}

#[test]
fn replacement_transform_declares_replacement() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (_, entry) = profile
        .resolve_export("ReplacementTransform")
        .expect("exported");
    let effects = entry.effects.as_ref().expect("effects");
    assert_eq!(effects.replacement, Some(true));
    // Plain Transform must NOT be a replacement.
    let (_, transform) = profile.resolve_export("Transform").expect("exported");
    let effects = transform.effects.as_ref().expect("effects");
    assert_eq!(effects.replacement, Some(false));
}

#[test]
fn register_font_is_exported_as_a_function() {
    // `register_font` is star-exported (`text_mobject.__all__`, re-exported
    // by `manim/__init__.py`); without this export `from manim import
    // register_font` never resolves and MLR117 misses the bare call.
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (id, entry) = profile
        .resolve_export("register_font")
        .expect("register_font is exported");
    assert_eq!(id, "manim.mobject.text.text_mobject.register_font");
    assert_eq!(entry.kind, SymbolKind::Function);
}

#[test]
fn override_animate_and_graph_overrides_are_curated() {
    let profile = qual::knowledge::load("upstream_0_20").unwrap();
    let (id, helper) = profile
        .resolve_export("override_animate")
        .expect("override_animate star export");
    assert_eq!(id, "manim.mobject.mobject.override_animate");
    assert_eq!(helper.kind, qual::knowledge::SymbolKind::Function);

    let graph = profile
        .resolve_export("Graph")
        .expect("Graph star export")
        .1;
    assert_eq!(graph.kind, qual::knowledge::SymbolKind::Vmobject);
    for method in [
        "add_vertices",
        "remove_vertices",
        "add_edges",
        "remove_edges",
    ] {
        let entry = profile
            .symbol(&format!("manim.mobject.graph.GenericGraph.{method}"))
            .unwrap_or_else(|| panic!("missing Graph override method {method}"));
        assert_eq!(
            entry
                .effects
                .as_ref()
                .and_then(|effects| effects.animate_override),
            Some(true),
            "{method} override fact",
        );
    }
}

#[test]
fn single_string_math_tex_is_exported() {
    // Star-exported via `tex_mobject.__all__`; backs the MLR103 / MLR115
    // constructor lists for explicit imports.
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (id, entry) = profile
        .resolve_export("SingleStringMathTex")
        .expect("SingleStringMathTex is exported");
    assert_eq!(id, "manim.mobject.text.tex_mobject.SingleStringMathTex");
    assert_eq!(entry.kind, SymbolKind::Vmobject);
}

#[test]
fn scene_add_has_membership_and_reorder_effect() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let entry = profile
        .symbol("manim.scene.scene.Scene.add")
        .expect("Scene.add curated");
    assert_eq!(entry.kind, SymbolKind::Method);
    assert_eq!(entry.returns_self, Some(true));
    let effects = entry.effects.as_ref().expect("effects");
    assert_eq!(effects.scene_membership, Some(SceneMembershipEffect::Add));
    assert_eq!(effects.reorders_existing_to_front, Some(true));
}

#[test]
fn scene_play_auto_adds_targets() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let entry = profile
        .symbol("manim.scene.scene.Scene.play")
        .expect("Scene.play curated");
    let effects = entry.effects.as_ref().expect("effects");
    assert_eq!(effects.auto_adds, Some(true));
    assert_eq!(effects.scene_membership, Some(SceneMembershipEffect::Play));
}

#[test]
fn fluent_mutators_return_self_and_copies_do_not() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    for method in [
        "manim.mobject.mobject.Mobject.shift",
        "manim.mobject.mobject.Mobject.rotate",
        "manim.mobject.mobject.Mobject.scale",
        "manim.mobject.mobject.Mobject.move_to",
        "manim.mobject.mobject.Mobject.become",
        "manim.mobject.mobject.Mobject.save_state",
        "manim.mobject.types.vectorized_mobject.VMobject.set_fill",
    ] {
        let entry = profile.symbol(method).expect(method);
        assert_eq!(entry.returns_self, Some(true), "{method}");
    }
    for method in [
        "manim.mobject.mobject.Mobject.copy",
        "manim.mobject.mobject.Mobject.generate_target",
        "manim.mobject.value_tracker.ValueTracker.get_value",
    ] {
        let entry = profile.symbol(method).expect(method);
        assert_eq!(entry.returns_self, Some(false), "{method}");
    }
}

#[test]
fn exports_all_resolve_to_curated_symbols() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    for (name, id) in &profile.exports {
        assert!(
            profile.symbols.contains_key(id),
            "export {name} -> {id} has no symbol entry"
        );
    }
    // Star-import constants resolve too.
    for constant in [
        "RIGHT", "LEFT", "UP", "DOWN", "ORIGIN", "OUT", "PI", "TAU", "DEGREES",
    ] {
        let (_, entry) = profile.resolve_export(constant).expect(constant);
        assert_eq!(entry.kind, SymbolKind::Constant, "{constant}");
    }
}

#[test]
fn color_constant_surface_is_exported() {
    // The full manim_colors star surface (89 names) plus the color core
    // types resolve so `from manim import RED, ManimColor` never falls
    // back to Unknown (generator backlog, machine-verified exports).
    let profile = knowledge::load(UPSTREAM).expect("load");
    for constant in [
        "RED",
        "BLUE",
        "GREEN",
        "YELLOW",
        "WHITE",
        "BLACK",
        "GRAY_A",
        "GREY_A",
        "TEAL_E",
        "LOGO_BLUE",
        "PURE_RED",
        "DARK_BROWN",
    ] {
        let (id, entry) = profile.resolve_export(constant).expect(constant);
        assert!(id.starts_with("manim.utils.color.manim_colors."), "{id}");
        assert_eq!(entry.kind, SymbolKind::Constant, "{constant}");
    }
    // Core color types are curated as inert constants (calling them never
    // yields a mobject), converters as functions.
    for name in ["ManimColor", "HSV", "RGBA", "ParsableManimColor"] {
        let (_, entry) = profile.resolve_export(name).expect(name);
        assert_eq!(entry.kind, SymbolKind::Constant, "{name}");
    }
    for name in [
        "average_color",
        "color_gradient",
        "interpolate_color",
        "rgb_to_color",
    ] {
        let (_, entry) = profile.resolve_export(name).expect(name);
        assert_eq!(entry.kind, SymbolKind::Function, "{name}");
    }
}

#[test]
fn constants_module_surface_is_complete() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    for constant in [
        "UL",
        "UR",
        "DL",
        "DR",
        "X_AXIS",
        "Y_AXIS",
        "Z_AXIS",
        "DEFAULT_FONT_SIZE",
        "DEFAULT_STROKE_WIDTH",
        "SMALL_BUFF",
        "LARGE_BUFF",
        "BOLD",
        "ITALIC",
        "QUALITIES",
        "CapStyleType",
        "LineJointType",
        "RendererType",
    ] {
        let (id, entry) = profile.resolve_export(constant).expect(constant);
        assert!(id.starts_with("manim.constants."), "{id}");
        assert_eq!(entry.kind, SymbolKind::Constant, "{constant}");
    }
}

#[test]
fn transform_star_exports_are_animations_with_verified_bases() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    for (name, base) in [
        ("FadeToColor", "manim.animation.transform.ApplyMethod"),
        ("ScaleInPlace", "manim.animation.transform.ApplyMethod"),
        ("ShrinkToCenter", "manim.animation.transform.ScaleInPlace"),
        ("ClockwiseTransform", "manim.animation.transform.Transform"),
        (
            "CounterclockwiseTransform",
            "manim.animation.transform.Transform",
        ),
        (
            "ApplyPointwiseFunction",
            "manim.animation.transform.ApplyMethod",
        ),
    ] {
        let (_, entry) = profile.resolve_export(name).expect(name);
        assert_eq!(entry.kind, SymbolKind::Animation, "{name}");
        assert_eq!(entry.bases, vec![base.to_owned()], "{name}");
    }
    // Chain-inheritance soundness: classes that swap or wrap their played
    // mobject keep their bases uncurated so Animation auto-add / Transform
    // cleanup facts are never inherited as certainties.
    for name in [
        "TransformFromCopy",
        "FadeTransform",
        "CyclicReplace",
        "Swap",
    ] {
        let (_, entry) = profile.resolve_export(name).expect(name);
        assert_eq!(entry.kind, SymbolKind::Animation, "{name}");
        assert!(entry.effects.is_none(), "{name} must not invent effects");
    }
    for name in ["TransformFromCopy", "FadeTransform", "CyclicReplace"] {
        let (_, entry) = profile.resolve_export(name).expect(name);
        assert!(entry.bases.is_empty(), "{name} bases stay uncurated");
    }
    // TransformAnimations is deliberately not curated: composition
    // modeling would join child introducer facts into a wrong certainty
    // (profiles/README.md).
    assert!(profile.resolve_export("TransformAnimations").is_none());
    assert!(
        profile
            .symbol("manim.animation.transform.TransformAnimations")
            .is_none()
    );
}

#[test]
fn bezier_helpers_are_exported_functions() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    for name in [
        "bezier",
        "bezier_remap",
        "interpolate",
        "inverse_interpolate",
        "integer_interpolate",
        "match_interpolate",
        "is_closed",
        "split_bezier",
        "subdivide_bezier",
    ] {
        let (id, entry) = profile.resolve_export(name).expect(name);
        assert!(id.starts_with("manim.utils.bezier."), "{id}");
        assert_eq!(entry.kind, SymbolKind::Function, "{name}");
    }
}

#[test]
fn parametric_function_and_axes_plot_are_curated() {
    // MLP221 groundwork: both the class and the plot helper carry
    // machine-verified kinds, bases, and signature facts.
    let profile = knowledge::load(UPSTREAM).expect("load");
    let (id, entry) = profile
        .resolve_export("ParametricFunction")
        .expect("ParametricFunction exported");
    assert_eq!(id, "manim.mobject.graphing.functions.ParametricFunction");
    assert_eq!(entry.kind, SymbolKind::Vmobject);
    assert_eq!(
        entry.bases,
        vec!["manim.mobject.types.vectorized_mobject.VMobject".to_owned()]
    );
    let signature = entry.signature.as_ref().expect("signature curated");
    assert_eq!(
        signature.params.first().map(|p| p.name.as_str()),
        Some("function")
    );
    assert!(signature.params.first().is_some_and(|p| p.required));

    // Axes reaches CoordinateSystem so `axes.plot` resolves via the chain.
    let axes = profile
        .symbol("manim.mobject.graphing.coordinate_systems.Axes")
        .expect("Axes curated");
    assert!(
        axes.bases
            .contains(&"manim.mobject.graphing.coordinate_systems.CoordinateSystem".to_owned())
    );
    let plot = profile
        .symbol("manim.mobject.graphing.coordinate_systems.Axes.plot")
        .expect("Axes.plot curated");
    assert_eq!(plot.kind, SymbolKind::Method);
    assert_eq!(plot.returns_self, Some(false));
    let signature = plot.signature.as_ref().expect("signature curated");
    assert_eq!(
        signature.params.first().map(|p| p.name.as_str()),
        Some("function")
    );
}

#[test]
fn family_getters_and_insert_n_curves_are_curated_methods() {
    // MLP202/203 receiver-resolution scope: family/geometry getters are
    // positive no-mutation facts, insert_n_curves is a fluent mutator.
    let profile = knowledge::load(UPSTREAM).expect("load");
    for method in [
        "manim.mobject.mobject.Mobject.get_family",
        "manim.mobject.mobject.Mobject.family_members_with_points",
        "manim.mobject.mobject.Mobject.get_all_points",
        "manim.mobject.types.vectorized_mobject.VMobject.get_arc_length",
    ] {
        let entry = profile.symbol(method).expect(method);
        assert_eq!(entry.kind, SymbolKind::Method, "{method}");
        assert_eq!(entry.returns_self, Some(false), "{method}");
    }
    let insert = profile
        .symbol("manim.mobject.types.vectorized_mobject.VMobject.insert_n_curves")
        .expect("insert_n_curves curated");
    assert_eq!(insert.returns_self, Some(true));
    // align_data mutates its mobject *argument*, which curated method
    // dispatch would stop widening: it stays uncurated on purpose
    // (profiles/README.md).
    assert!(
        profile
            .symbol("manim.mobject.mobject.Mobject.align_data")
            .is_none()
    );
}

#[test]
fn fixed_in_frame_removal_is_renderer_divergent() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let entry = profile
        .symbol("manim.scene.three_d_scene.ThreeDScene.remove_fixed_in_frame_mobjects")
        .expect("curated");
    let effects = entry.effects.as_ref().expect("effects");
    assert_eq!(effects.renderer_divergent_membership, Some(true));
}

// --- fork fast-path capabilities (DESIGN §7.3) ------------------------------

#[test]
fn upstream_profile_declares_no_fork_capabilities() {
    // Fork-gated rule interpretation (MLP214 / MLP217 gating / MLP225 and
    // the cairo_fork_workers / cairo_static_layers fast-path semantics) must
    // stay inert under upstream_0_20: the upstream profile must not declare
    // any fork capability.
    let profile = knowledge::load(UPSTREAM).expect("load");
    assert!(profile.fork_capabilities().is_none());
    assert!(profile.tex_parallel_compile().is_none());
    assert!(profile.cairo_fork_gate().is_none());
    assert!(profile.cairo_static_layers().is_none());
    assert!(profile.cairo_bulk_interpolation().is_none());
    assert!(profile.svg_cache().is_none());
    assert!(profile.continuous_movie_stream().is_none());
}

#[test]
fn local_overlay_declares_tex_parallel_compile() {
    let profile = knowledge::load(LOCAL_OVERLAY).expect("load overlay");
    let tex = profile
        .tex_parallel_compile()
        .expect("tex_parallel_compile declared");
    assert_eq!(
        tex.entry_points,
        vec![
            "manim.mobject.text.tex_mobject.MathTex.precompile".to_owned(),
            "manim.utils.tex_file_writing.tex_to_svg_file_async".to_owned(),
        ]
    );
    assert_eq!(tex.same_key_coalesced, Some(true));
    assert_eq!(tex.cache_hit_short_circuits, Some(true));
    assert_eq!(tex.in_flight_blocks_cairo_fork, Some(true));

    // Never suggest APIs that do not exist in the selected profile: every
    // entry point is a curated symbol (also enforced by validation).
    for entry_point in &tex.entry_points {
        assert!(profile.symbol(entry_point).is_some(), "{entry_point}");
    }
    // Both live below `manim.*` module paths, not on the star surface.
    assert!(profile.resolve_export("tex_to_svg_file_async").is_none());
    assert!(profile.resolve_export("precompile").is_none());

    let precompile = profile
        .symbol("manim.mobject.text.tex_mobject.MathTex.precompile")
        .expect("curated");
    assert_eq!(precompile.kind, SymbolKind::Method);
    assert_eq!(precompile.returns_self, Some(false));
    let async_compile = profile
        .symbol("manim.utils.tex_file_writing.tex_to_svg_file_async")
        .expect("curated");
    assert_eq!(async_compile.kind, SymbolKind::Function);
}

#[test]
fn local_overlay_declares_cairo_fork_gate_with_monotonic_disable() {
    let profile = knowledge::load(LOCAL_OVERLAY).expect("load overlay");
    let gate = profile.cairo_fork_gate().expect("cairo_fork_gate declared");
    assert_eq!(gate.config_key, "cairo_fork_workers");
    // workers 0..1 is "unrequested", not a blocker (DESIGN §7.3).
    assert_eq!(gate.min_workers, 2);
    assert_eq!(gate.linux_only, Some(true));
    // Once the first written play opens the parent encoder, later eligible
    // plays cannot fork: OutputState must model renderer-wide monotonic
    // disabling, never per-play independence.
    assert_eq!(gate.monotonic_disable, Some(true));
    for blocker in [
        ForkBlocker::ParentEncoderOpened,
        ForkBlocker::SceneUpdaters,
        ForkBlocker::ForegroundMobjects,
        ForkBlocker::SoundAdded,
        ForkBlocker::SaveSections,
        ForkBlocker::TransparentOutput,
        ForkBlocker::UnsupportedAnimationType,
        ForkBlocker::CustomRateFunc,
        ForkBlocker::NonStraightPathFunc,
        ForkBlocker::InFlightTexWorkers,
        ForkBlocker::NonLibx264Encoder,
        ForkBlocker::Meshes,
    ] {
        assert!(
            gate.blockers.contains(&blocker),
            "missing fork blocker {}",
            blocker.as_str()
        );
    }
    // The allowlist is exact-type versioned knowledge, not name matching.
    for id in [
        "manim.animation.animation.Wait",
        "manim.animation.transform.Transform",
        "manim.animation.transform._MethodAnimation",
        "manim.animation.fading.FadeOut",
    ] {
        assert!(
            gate.animation_allowlist.contains(&id.to_owned()),
            "missing allowlisted animation {id}"
        );
    }
    assert!(
        gate.composition_allowlist
            .contains(&"manim.animation.composition.AnimationGroup".to_owned())
    );
}

#[test]
fn local_overlay_declares_static_layer_and_packed_interpolation_gates() {
    let profile = knowledge::load(LOCAL_OVERLAY).expect("load overlay");

    let layers = profile
        .cairo_static_layers()
        .expect("cairo_static_layers declared");
    assert_eq!(layers.config_key, "cairo_static_layers");
    assert_eq!(layers.min_play_frames, 3);
    assert_eq!(layers.retains_trailing_static_runs, Some(true));
    assert!(layers.blockers.contains(&ForkBlocker::SceneUpdaters));
    assert!(layers.blockers.contains(&ForkBlocker::NonOpaqueBackground));

    let packed = profile
        .cairo_bulk_interpolation()
        .expect("cairo_bulk_interpolation declared");
    // _CAIRO_BULK_MIN_FRAMES / _CAIRO_BULK_MIN_FAMILY_COUNT /
    // _CAIRO_BULK_MIN_AMORTIZATION in the fork's cairo_renderer.py.
    assert_eq!(packed.min_frames, 12);
    assert_eq!(packed.min_family_count, 4);
    assert_eq!(packed.min_amortization, 192);
    assert_eq!(
        packed.animation_allowlist,
        vec![
            "manim.animation.transform.Transform".to_owned(),
            "manim.animation.transform._MethodAnimation".to_owned(),
        ]
    );
    for blocker in [
        ForkBlocker::UpdaterBearingFamily,
        ForkBlocker::UnsupportedAnimationType,
        ForkBlocker::CustomRateFunc,
        ForkBlocker::NonStraightPathFunc,
        ForkBlocker::NonzeroLagRatio,
    ] {
        assert!(
            packed.blockers.contains(&blocker),
            "missing packed-path blocker {}",
            blocker.as_str()
        );
    }
}

#[test]
fn local_overlay_declares_svg_cache_and_continuous_stream_semantics() {
    let profile = knowledge::load(LOCAL_OVERLAY).expect("load overlay");

    // MLP217's gate: the profile must declare the process-global cache.
    let svg_cache = profile.svg_cache().expect("svg_cache declared");
    assert_eq!(svg_cache.process_global, Some(true));
    assert_eq!(svg_cache.unbounded, Some(true));
    assert_eq!(svg_cache.copies_on_hit, Some(true));
    assert_eq!(
        svg_cache.keyed_by,
        vec![
            "class_name".to_owned(),
            "svg_default".to_owned(),
            "path_string_config".to_owned(),
            "file_name".to_owned(),
            "config.renderer".to_owned(),
        ]
    );

    // MLP210's OutputState gate: per-play partial stream boundaries vanish
    // only under the continuous stream's own conditions.
    let stream = profile
        .continuous_movie_stream()
        .expect("continuous_movie_stream declared");
    assert_eq!(stream.merges_partial_movie_files, Some(true));
    assert_eq!(stream.requires_disable_caching, Some(true));
    assert!(stream.blockers.contains(&ForkBlocker::CachingEnabled));
    assert!(stream.blockers.contains(&ForkBlocker::SoundAdded));

    // The fork's new config surface is declared for config display.
    let capabilities = profile.fork_capabilities().expect("declared");
    let names: Vec<&str> = capabilities
        .config_keys
        .iter()
        .map(|key| key.name.as_str())
        .collect();
    for name in [
        "cairo_fork_workers",
        "cairo_static_layers",
        "cairo_antialias",
        "video_encoder",
        "x264_preset",
    ] {
        assert!(names.contains(&name), "missing config key {name}");
    }
}

#[test]
fn fork_blocker_names_match_their_json_encoding() {
    // Cost reports render `ForkBlocker::as_str`; it must stay identical to
    // the serde snake_case wire form the profiles are written in.
    for blocker in [
        ForkBlocker::ParentEncoderOpened,
        ForkBlocker::NonLibx264Encoder,
        ForkBlocker::SceneUpdaters,
        ForkBlocker::UntrustedAnimationLifecycle,
        ForkBlocker::InFlightTexWorkers,
        ForkBlocker::FrozenStaticWait,
        ForkBlocker::NonzeroLagRatio,
        ForkBlocker::OverlappingAnimationFamilies,
        ForkBlocker::CachingEnabled,
    ] {
        let encoded = serde_json::to_value(blocker).expect("serialize");
        assert_eq!(encoded, serde_json::Value::from(blocker.as_str()));
        let decoded: ForkBlocker = serde_json::from_value(encoded).expect("round-trip");
        assert_eq!(decoded, blocker);
    }
}

#[test]
fn tex_capability_entry_points_must_be_curated_symbols() {
    // Regression: a declared parallel-TeX capability whose entry point is
    // not a curated symbol would let MLP214 advise an API the profile
    // cannot prove exists — validation rejects it.
    let base = knowledge::load(UPSTREAM).expect("load base");
    let json = overlay_json(
        &base.source_digest,
        r#""fork_capabilities": {
      "tex_parallel_compile": {
        "entry_points": ["manim.ghost.Ghost.precompile"]
      }
    }"#,
    );
    let overlay = ProfileDocument::from_json("overlay", &json).expect("parses");
    let error = apply_overlay(&base, &overlay).expect_err("unknown entry point must fail");
    match error {
        KnowledgeError::Invalid { message, .. } => {
            assert!(
                message.contains("manim.ghost.Ghost.precompile"),
                "{message}"
            );
        }
        other => panic!("expected Invalid, got: {other:?}"),
    }

    let json = overlay_json(
        &base.source_digest,
        r#""fork_capabilities": {
      "tex_parallel_compile": {
        "entry_points": []
      }
    }"#,
    );
    let overlay = ProfileDocument::from_json("overlay", &json).expect("parses");
    let error = apply_overlay(&base, &overlay).expect_err("empty entry_points must fail");
    assert!(matches!(error, KnowledgeError::Invalid { .. }), "{error:?}");
}

#[test]
fn overlay_without_capabilities_inherits_the_base_block() {
    // An absent overlay block inherits the base's (None for upstream); a
    // present block replaces the base's wholesale.
    let base = knowledge::load(UPSTREAM).expect("load base");
    let json = overlay_json(&base.source_digest, r#""symbols": {}"#);
    let overlay = ProfileDocument::from_json("overlay", &json).expect("parses");
    let resolved = apply_overlay(&base, &overlay).expect("resolves");
    assert!(resolved.fork_capabilities().is_none());
}

// --- overlay semantics -----------------------------------------------------

fn overlay_json(base_digest: &str, body: &str) -> String {
    format!(
        r#"{{
  "schema_version": 1,
  "name": "local_0_20_1_4d25c031",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:{}",
  "base_profile": {{"name": "upstream_0_20", "source_digest": "{}"}},
  {}
}}"#,
        "ab".repeat(32),
        base_digest,
        body
    )
}

#[test]
fn overlay_replaces_whole_entries_and_deletes_by_key() {
    let base = knowledge::load(UPSTREAM).expect("load base");
    let json = overlay_json(
        &base.source_digest,
        r#""symbols": {
      "manim.animation.creation.Create": {"kind": "animation", "accepted_target": "VMobject"}
    },
    "deleted_symbols": ["manim.animation.rotation.Rotate"],
    "deleted_exports": ["Rotate"]"#,
    );
    let overlay = ProfileDocument::from_json("overlay", &json).expect("overlay parses");
    let resolved = apply_overlay(&base, &overlay).expect("overlay resolves");

    assert_eq!(resolved.name, "local_0_20_1_4d25c031");
    assert_eq!(resolved.base_profile.as_deref(), Some(UPSTREAM));

    // Whole-entry replacement: the overlay entry had no effects, so the
    // base introducer fact must be gone (no deep merge).
    let create = resolved
        .symbol("manim.animation.creation.Create")
        .expect("Create still present");
    assert!(create.effects.is_none(), "no recursive deep merge");
    assert_eq!(create.accepted_target, Some(AcceptedTarget::Vmobject));

    // Deletion by key removes the symbol and its export.
    assert!(resolved.symbol("manim.animation.rotation.Rotate").is_none());
    assert!(resolved.resolve_export("Rotate").is_none());

    // Untouched entries carry over from the base.
    assert!(resolved.symbol("manim.scene.scene.Scene.add").is_some());
}

#[test]
fn overlay_digest_mismatch_is_rejected() {
    let base = knowledge::load(UPSTREAM).expect("load base");
    let wrong_digest = format!("sha256:{}", "00".repeat(32));
    let json = overlay_json(&wrong_digest, r#""symbols": {}"#);
    let overlay = ProfileDocument::from_json("overlay", &json).expect("parses");
    let error = apply_overlay(&base, &overlay).expect_err("digest mismatch must fail");
    match error {
        KnowledgeError::Invalid { profile, message } => {
            assert_eq!(profile, "local_0_20_1_4d25c031");
            assert!(message.contains("source_digest"), "{message}");
        }
        other => panic!("expected Invalid, got: {other:?}"),
    }
}

#[test]
fn overlay_deleting_unknown_symbol_is_rejected() {
    let base = knowledge::load(UPSTREAM).expect("load base");
    let json = overlay_json(
        &base.source_digest,
        r#""deleted_symbols": ["manim.does.not.Exist"]"#,
    );
    let overlay = ProfileDocument::from_json("overlay", &json).expect("parses");
    let error = apply_overlay(&base, &overlay).expect_err("unknown delete must fail");
    assert!(matches!(error, KnowledgeError::Invalid { .. }), "{error:?}");
}

#[test]
fn overlay_wrong_base_name_is_rejected() {
    let base = knowledge::load(UPSTREAM).expect("load base");
    let json = format!(
        r#"{{
  "schema_version": 1,
  "name": "local_x",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:{}",
  "base_profile": {{"name": "some_other_base", "source_digest": "{}"}}
}}"#,
        "cd".repeat(32),
        base.source_digest
    );
    let overlay = ProfileDocument::from_json("overlay", &json).expect("parses");
    let error = apply_overlay(&base, &overlay).expect_err("wrong base name must fail");
    assert!(matches!(error, KnowledgeError::Invalid { .. }), "{error:?}");
}

#[test]
fn base_document_may_not_use_deleted_symbols() {
    let json = format!(
        r#"{{
  "schema_version": 1,
  "name": "broken",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:{}",
  "deleted_symbols": ["manim.x.Y"]
}}"#,
        "ef".repeat(32)
    );
    let error = ProfileDocument::from_json("broken", &json).expect_err("must fail");
    assert!(matches!(error, KnowledgeError::Invalid { .. }), "{error:?}");
}

#[test]
fn unsupported_schema_version_is_rejected() {
    let json = format!(
        r#"{{
  "schema_version": 99,
  "name": "future",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:{}"
}}"#,
        "ef".repeat(32)
    );
    let error = ProfileDocument::from_json("future", &json).expect_err("must fail");
    assert!(matches!(error, KnowledgeError::Invalid { .. }), "{error:?}");
}

#[test]
fn export_to_unknown_symbol_is_rejected() {
    let json = format!(
        r#"{{
  "schema_version": 1,
  "name": "dangling",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:{}",
  "exports": {{"Ghost": "manim.ghost.Ghost"}}
}}"#,
        "ef".repeat(32)
    );
    let document = ProfileDocument::from_json("dangling", &json).expect("parses");
    let error = document
        .into_resolved()
        .expect_err("dangling export must fail");
    assert!(matches!(error, KnowledgeError::Invalid { .. }), "{error:?}");
}
