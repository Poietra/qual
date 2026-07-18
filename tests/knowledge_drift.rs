//! Knowledge-profile generator and drift check (DESIGN §5.4, §11.2 layer 9).
//!
//! The non-ignored tests exercise the extractor on small inline Python
//! fixtures. The `#[ignore]`d test is the layer-9 drift gate: it reads the
//! sibling Manim checkout at `/home/hosi/manim` (statically, read-only) and
//! asserts the shipped profile has zero category-(b) contradictions:
//!
//! ```sh
//! cargo test --test knowledge_drift -- --ignored
//! ```

use std::path::Path;

use manim_lint::knowledge;
use manim_lint::knowledge::generator::{
    GeneratedCandidates, ReturnEvidence, diff, generate, generate_from_sources, sha256_hex,
    to_stable_json,
};
use manim_lint::knowledge::model::ProfileDocument;

/// A miniature Manim-shaped package covering classes with bases across
/// modules, returns-`self` shapes, `__all__`, and a star-import closure.
fn fixture_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "manim/__init__.py",
            "from .animation.creation import *\n\
             from .mobject.mobject import *\n\
             from .constants import *\n\
             from .mobject.mobject import Mobject as Mob\n",
        ),
        (
            "manim/constants.py",
            "__all__ = [\"RIGHT\"]\nRIGHT = 1\nSECRET = 2\n",
        ),
        ("manim/mobject/__init__.py", ""),
        (
            "manim/mobject/mobject.py",
            "__all__ = [\"Mobject\"]\n\n\
             class Mobject:\n\
             \x20   def shift(self, *vectors):\n\
             \x20       return self\n\
             \x20   def copy(self):\n\
             \x20       return make_copy(self)\n\
             \x20   def get_x(self):\n\
             \x20       return 1\n\
             \x20   def begin(self):\n\
             \x20       pass\n",
        ),
        ("manim/animation/__init__.py", ""),
        (
            "manim/animation/animation.py",
            "__all__ = [\"Animation\"]\n\n\
             class Animation:\n\
             \x20   def begin(self):\n\
             \x20       pass\n",
        ),
        (
            "manim/animation/creation.py",
            "__all__ = [\"Create\"]\n\
             from .animation import Animation\n\n\
             class ShowPartial(Animation):\n\
             \x20   pass\n\n\
             class Create(ShowPartial):\n\
             \x20   def __init__(self, mobject, lag_ratio=1.0, **kwargs):\n\
             \x20       pass\n",
        ),
    ]
}

fn fixture() -> GeneratedCandidates {
    generate_from_sources("manim", &fixture_sources())
}

#[test]
fn class_bases_resolve_across_modules() {
    let candidates = fixture();
    let create = &candidates.symbols["manim.animation.creation.Create"];
    assert_eq!(create.bases, vec!["manim.animation.creation.ShowPartial"]);
    assert!(create.unresolved_bases.is_empty());
    let show_partial = &candidates.symbols["manim.animation.creation.ShowPartial"];
    assert_eq!(
        show_partial.bases,
        vec!["manim.animation.animation.Animation"]
    );
    // `__init__` is promoted to the class signature.
    let signature = create.signature.as_ref().expect("Create signature");
    let names: Vec<&str> = signature
        .params
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    assert_eq!(names, vec!["mobject", "lag_ratio"]);
    assert!(signature.params[0].required);
    assert!(!signature.params[1].required);
    assert!(signature.accepts_kwargs);
}

#[test]
fn returns_self_evidence_is_structural_only() {
    let candidates = fixture();
    let mobject = &candidates.symbols["manim.mobject.mobject.Mobject"];
    let shift = &mobject.methods["shift"];
    assert_eq!(shift.returns_self, Some(true));
    assert_eq!(shift.return_evidence, ReturnEvidence::AllSelf);
    assert!(shift.signature.as_ref().is_some_and(|s| s.accepts_var_args));
    // A call could still return `self` transitively: no fact is claimed.
    let copy = &mobject.methods["copy"];
    assert_eq!(copy.returns_self, None);
    assert_eq!(copy.return_evidence, ReturnEvidence::Mixed);
    // Constant-only returns are a positive not-self fact.
    let get_x = &mobject.methods["get_x"];
    assert_eq!(get_x.returns_self, Some(false));
    assert_eq!(get_x.return_evidence, ReturnEvidence::NeverSelf);
    // No returns at all: no `returns_self` mirror field.
    let begin = &mobject.methods["begin"];
    assert_eq!(begin.returns_self, None);
    assert_eq!(begin.return_evidence, ReturnEvidence::NoReturns);
}

#[test]
fn static_dunder_all_and_members_are_extracted() {
    let candidates = fixture();
    assert_eq!(
        candidates.module_all["manim.constants"],
        vec!["RIGHT".to_owned()]
    );
    // `SECRET` is public but not in `__all__`: a member, not a symbol.
    assert!(candidates.symbols.contains_key("manim.constants.RIGHT"));
    assert!(!candidates.symbols.contains_key("manim.constants.SECRET"));
    assert!(candidates.module_members["manim.constants"].contains("SECRET"));
    assert!(candidates.dynamic_all_modules.is_empty());
}

#[test]
fn dynamic_dunder_all_is_flagged() {
    let candidates = generate_from_sources(
        "manim",
        &[(
            "manim/__init__.py",
            "__all__ = [\"a\"]\n__all__.append(\"b\")\na = 1\nb = 2\n",
        )],
    );
    assert!(candidates.dynamic_all_modules.contains("manim"));
    assert!(!candidates.exports_complete);
    // The static part is still a usable lower bound.
    assert_eq!(candidates.exports["a"], "manim.a");
}

#[test]
fn star_export_closure_follows_chains_and_aliases() {
    let candidates = fixture();
    assert!(candidates.exports_complete);
    assert_eq!(
        candidates.exports["Create"],
        "manim.animation.creation.Create"
    );
    assert_eq!(
        candidates.exports["Mobject"],
        "manim.mobject.mobject.Mobject"
    );
    assert_eq!(candidates.exports["RIGHT"], "manim.constants.RIGHT");
    // Aliased explicit re-export keeps the defining symbol id.
    assert_eq!(candidates.exports["Mob"], "manim.mobject.mobject.Mobject");
    // `__all__` gates the star surface: ShowPartial and SECRET stay out.
    assert!(!candidates.exports.contains_key("ShowPartial"));
    assert!(!candidates.exports.contains_key("SECRET"));
}

#[test]
fn unresolvable_star_import_marks_closure_incomplete() {
    let candidates = generate_from_sources(
        "manim",
        &[("manim/__init__.py", "from external_pkg import *\nx = 1\n")],
    );
    assert!(!candidates.exports_complete);
    assert_eq!(candidates.exports["x"], "manim.x");
}

#[test]
fn generated_output_is_byte_stable() {
    let first = to_stable_json(&fixture()).expect("serialize");
    let second = to_stable_json(&fixture()).expect("serialize");
    assert_eq!(first, second);
    assert!(first.ends_with('\n'));
    // Candidate entries are marked generated and never carry curated-only
    // semantic fields.
    assert!(first.contains("\"generated\": true"));
    assert!(!first.contains("\"effects\""));
    assert!(!first.contains("\"introducer\""));
}

#[test]
fn source_digest_follows_the_manifest_recipe() {
    let candidates = generate_from_sources("manim", &[("manim/__init__.py", "x = 1\n")]);
    let manifest = format!("{}  manim/__init__.py\n", sha256_hex(b"x = 1\n"));
    assert_eq!(
        candidates.source_digest,
        format!("sha256:{}", sha256_hex(manifest.as_bytes()))
    );
}

#[test]
fn sha256_matches_fips_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// A curated profile with one correct entry per checked field plus one
/// deliberate error of each drift category.
fn synthetic_profile() -> manim_lint::knowledge::model::KnowledgeProfile {
    let json = r#"{
  "schema_version": 1,
  "name": "test_profile",
  "manim_version": ">=0.20,<0.21",
  "source_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
  "base_profile": null,
  "symbols": {
    "manim.animation.creation.Create": {
      "kind": "animation",
      "bases": ["manim.animation.transform.Transform"]
    },
    "manim.animation.creation.Create.begin": {
      "kind": "method"
    },
    "manim.animation.creation.Vanish": {
      "kind": "animation"
    },
    "manim.animation.fading.Fade": {
      "kind": "animation"
    },
    "manim.mobject.mobject.Mobject.copy": {
      "kind": "method",
      "returns_self": false
    },
    "manim.mobject.mobject.Mobject.get_x": {
      "kind": "method",
      "returns_self": true
    },
    "manim.mobject.mobject.Mobject.shift": {
      "kind": "method",
      "returns_self": true
    }
  },
  "exports": {
    "Create": "manim.animation.creation.Create",
    "Fade": "manim.animation.fading.Fade"
  }
}"#;
    ProfileDocument::from_json("test_profile", json)
        .expect("parse synthetic profile")
        .into_resolved()
        .expect("resolve synthetic profile")
}

#[test]
fn diff_reports_missing_contradictions_and_gaps() {
    let candidates = fixture();
    let report = diff(&candidates, &synthetic_profile());

    // (a) curated symbols missing from source.
    let missing: Vec<&str> = report
        .missing
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(missing.contains(&"manim.animation.creation.Vanish"));
    assert!(missing.contains(&"manim.animation.fading.Fade"));
    // `Create.begin` resolves through the base chain (Animation defines it).
    assert!(!missing.contains(&"manim.animation.creation.Create.begin"));

    // (b) contradictions.
    assert!(report.has_contradictions());
    let mut fields: Vec<(&str, &str)> = report
        .contradictions
        .iter()
        .map(|entry| (entry.id.as_str(), entry.field.as_str()))
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        vec![
            ("Fade", "exports"),
            ("manim.animation.creation.Create", "bases"),
            ("manim.mobject.mobject.Mobject.get_x", "returns_self"),
        ]
    );

    // Verified facts produce no findings: shift returns_self=true matches,
    // copy returns_self=false is unprovable either way (mixed evidence).
    assert!(
        !report
            .contradictions
            .iter()
            .any(|entry| entry.id.contains("shift") || entry.id.contains("copy"))
    );

    // (c) coverage gaps: exported names absent from the profile, per module.
    assert_eq!(report.coverage_gap_total, 3);
    assert_eq!(
        report.coverage_gaps["manim.mobject.mobject"],
        vec!["Mob".to_owned(), "Mobject".to_owned()]
    );
    assert_eq!(
        report.coverage_gaps["manim.constants"],
        vec!["RIGHT".to_owned()]
    );

    // The text rendering is deterministic and carries the counts.
    let text = report.render_text();
    assert_eq!(text, report.render_text());
    assert!(text.contains("(b) contradictions: 3"));
}

/// DESIGN §11.2 layer 9: the shipped profile must not contradict the Manim
/// source it describes. Ignored by default because it needs the sibling
/// checkout at `/home/hosi/manim`.
#[test]
#[ignore = "requires the sibling Manim checkout at /home/hosi/manim"]
fn shipped_profile_has_no_contradictions_against_sibling_checkout() {
    let root = Path::new("/home/hosi/manim");
    assert!(
        root.join("manim").join("__init__.py").is_file(),
        "sibling Manim checkout not found at /home/hosi/manim"
    );
    let candidates = generate(root).expect("generate candidates from sibling checkout");
    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).expect("load shipped profile");
    let report = diff(&candidates, &profile);
    assert!(
        !report.has_contradictions(),
        "shipped profile contradicts the Manim source:\n{}",
        report.render_text()
    );
    assert!(
        report.missing.is_empty(),
        "curated symbols missing from the Manim source:\n{}",
        report.render_text()
    );
}
