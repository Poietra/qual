//! Knowledge-profile generator and drift check (DESIGN §5.4, §11.2 layer 9).
//!
//! The non-ignored tests exercise the extractor on small inline Python
//! fixtures. The `#[ignore]`d tests are the layer-9 drift gate: they read
//! a Manim git checkout — `../manim`, or `MANIM_LINT_MANIM_ROOT` — (statically,
//! read-only) and check the shipped profiles against both provenances —
//! the clean upstream base commit (must be contradiction-free) and the
//! working tree carrying local fork changes (informational, except for
//! facts the clean base also disproves):
//!
//! ```sh
//! cargo test --test knowledge_drift -- --ignored
//! ```

use std::path::PathBuf;

use manim_lint::knowledge;
use manim_lint::knowledge::generator::{
    GeneratedCandidates, ReturnEvidence, diff, generate, generate_from_git_ref,
    generate_from_sources, generate_from_tar, sha256_hex, to_stable_json,
};
use manim_lint::knowledge::model::ProfileDocument;

/// The clean upstream base commit of the sibling checkout's fork lineage
/// (v0.20.1 lineage; `git describe` in that checkout names it
/// `v0.20.1-49-g4d25c031`). The shipped `upstream_0_20` digest is computed
/// over this tree; everything the working tree adds on top belongs to the
/// `local_0_20_1_4d25c031` overlay.
const UPSTREAM_BASE_COMMIT: &str = "4d25c031";

/// Shipped fork-overlay profile name.
const LOCAL_OVERLAY: &str = "local_0_20_1_4d25c031";

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

// --- tar-based generation (the `--manim-ref` path) -------------------------

/// One 512-byte ustar header. Only the fields the reader consumes are
/// filled; the checksum is left blank (the reader does not verify it).
fn tar_header(name: &str, size: usize, type_flag: u8) -> [u8; 512] {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    let size_octal = format!("{size:011o}\0");
    header[124..136].copy_from_slice(size_octal.as_bytes());
    header[156] = type_flag;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header
}

fn tar_entry(archive: &mut Vec<u8>, name: &str, data: &[u8], type_flag: u8) {
    archive.extend_from_slice(&tar_header(name, data.len(), type_flag));
    archive.extend_from_slice(data);
    let padding = data.len().next_multiple_of(512) - data.len();
    archive.extend(std::iter::repeat_n(0u8, padding));
}

#[test]
fn tar_stream_yields_the_same_candidates_as_loose_sources() {
    let init = "from .constants import *\n";
    let constants = "__all__ = [\"RIGHT\"]\nRIGHT = 1\n";
    let mut archive = Vec::new();
    // `git archive` opens with a pax global header carrying the commit id.
    tar_entry(
        archive.as_mut(),
        "pax_global_header",
        b"52 comment=4d25c031ffe71c602e20935afd54a96f33545a6e\n",
        b'g',
    );
    tar_entry(archive.as_mut(), "manim/", b"", b'5');
    tar_entry(archive.as_mut(), "manim/__init__.py", init.as_bytes(), b'0');
    tar_entry(
        archive.as_mut(),
        "manim/constants.py",
        constants.as_bytes(),
        b'0',
    );
    // Non-package and non-Python entries are ignored.
    tar_entry(archive.as_mut(), "README.md", b"docs\n", b'0');
    tar_entry(archive.as_mut(), "manim/py.typed", b"", b'0');
    archive.extend(std::iter::repeat_n(0u8, 1024)); // end-of-archive marker

    let from_tar = generate_from_tar("test@ref", &archive).expect("tar candidates");
    let from_sources = generate_from_sources(
        "manim",
        &[
            ("manim/__init__.py", init),
            ("manim/constants.py", constants),
        ],
    );
    assert_eq!(from_tar, from_sources);
    assert_eq!(from_tar.exports["RIGHT"], "manim.constants.RIGHT");
}

#[test]
fn tar_without_the_manim_package_is_an_error() {
    let mut archive = Vec::new();
    tar_entry(archive.as_mut(), "other/__init__.py", b"x = 1\n", b'0');
    archive.extend(std::iter::repeat_n(0u8, 1024));
    let error = generate_from_tar("test@ref", &archive).expect_err("must fail");
    assert!(error.to_string().contains("manim/__init__.py"), "{error}");
}

// --- layer-9 drift gate (ignored: needs the sibling checkout) --------------

/// Path of the Manim checkout the drift gate reads.
///
/// `MANIM_LINT_MANIM_ROOT` overrides the default sibling location, so the gate
/// runs from any working copy and from CI without a machine-specific path.
fn sibling_checkout() -> PathBuf {
    let root = std::env::var_os("MANIM_LINT_MANIM_ROOT")
        .map_or_else(|| PathBuf::from("../manim"), PathBuf::from);
    assert!(
        root.join("manim").join("__init__.py").is_file(),
        "Manim checkout not found at {}; clone it there or set MANIM_LINT_MANIM_ROOT",
        root.display()
    );
    root
}

/// DESIGN §11.2 layer 9, upstream provenance: the shipped `upstream_0_20`
/// profile describes the **clean** upstream base commit — read via
/// `git archive` so the checkout's uncommitted fork changes never leak into
/// upstream facts. Everything must verify: no contradictions, no missing
/// symbols, and the profile digest must equal the clean-tree digest.
#[test]
#[ignore = "requires a Manim checkout at ../manim or MANIM_LINT_MANIM_ROOT"]
fn upstream_profile_matches_clean_base_commit() {
    let candidates = generate_from_git_ref(&sibling_checkout(), UPSTREAM_BASE_COMMIT)
        .expect("archive candidates from the clean base commit");
    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).expect("load upstream profile");
    let report = diff(&candidates, &profile);
    assert!(
        !report.has_contradictions(),
        "upstream profile contradicts the clean upstream source:\n{}",
        report.render_text()
    );
    assert!(
        report.missing.is_empty(),
        "curated upstream symbols missing from the clean upstream source:\n{}",
        report.render_text()
    );
    assert!(
        report.digest_match,
        "upstream_0_20 source_digest must be computed over the clean base tree \
         {UPSTREAM_BASE_COMMIT} (profile: {}, clean tree: {})",
        report.profile_digest, report.source_digest
    );
}

/// DESIGN §11.2 layer 9, fork provenance: the working tree carries local
/// fork changes, so upstream-profile drift against it is informational —
/// the gate fails only on facts the **clean base** also disproves (i.e.
/// facts that were never true upstream). The fork overlay itself must be
/// drift-free against the working tree it describes.
#[test]
#[ignore = "requires a Manim checkout at ../manim or MANIM_LINT_MANIM_ROOT"]
fn working_tree_drift_is_informational_for_fork_only_changes() {
    let root = sibling_checkout();
    let clean = generate_from_git_ref(&root, UPSTREAM_BASE_COMMIT)
        .expect("archive candidates from the clean base commit");
    let working = generate(&root).expect("generate candidates from the working tree");
    let upstream = knowledge::load(knowledge::DEFAULT_PROFILE).expect("load upstream profile");

    let clean_report = diff(&clean, &upstream);
    let working_report = diff(&working, &upstream);
    println!(
        "informational upstream-vs-working-tree drift:\n{}",
        working_report.render_text()
    );
    let never_true_upstream: Vec<String> = working_report
        .contradictions
        .iter()
        .filter(|finding| {
            clean_report
                .contradictions
                .iter()
                .any(|clean| clean.id == finding.id && clean.field == finding.field)
        })
        .map(|finding| format!("{} [{}]", finding.id, finding.field))
        .collect();
    assert!(
        never_true_upstream.is_empty(),
        "curated upstream facts contradicted by both the working tree and the clean \
         base (never true upstream): {never_true_upstream:?}\n{}",
        working_report.render_text()
    );

    let overlay = knowledge::load(LOCAL_OVERLAY).expect("load fork overlay profile");
    let overlay_report = diff(&working, &overlay);
    assert!(
        !overlay_report.has_contradictions(),
        "fork overlay contradicts the working tree it describes:\n{}",
        overlay_report.render_text()
    );
    assert!(
        overlay_report.missing.is_empty(),
        "fork-overlay symbols missing from the working tree:\n{}",
        overlay_report.render_text()
    );
}
