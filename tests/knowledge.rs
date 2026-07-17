//! Integration tests for the versioned Manim knowledge profile system
//! (DESIGN §5.4): loading, validation, overlay semantics, and the
//! star-import exports bridge.

use manim_lint::knowledge::{
    self, AcceptedTarget, KnowledgeError, ProfileDocument, SceneMembershipEffect, SymbolKind,
    apply_overlay,
};

const UPSTREAM: &str = "upstream_0_20";

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
fn list_available_contains_upstream() {
    let names = knowledge::list_available();
    assert!(names.contains(&UPSTREAM), "available: {names:?}");
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
fn fixed_in_frame_removal_is_renderer_divergent() {
    let profile = knowledge::load(UPSTREAM).expect("load");
    let entry = profile
        .symbol("manim.scene.three_d_scene.ThreeDScene.remove_fixed_in_frame_mobjects")
        .expect("curated");
    let effects = entry.effects.as_ref().expect("effects");
    assert_eq!(effects.renderer_divergent_membership, Some(true));
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
