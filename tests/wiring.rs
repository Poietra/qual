//! Wave 1 integration tests: the check pipeline loads the configured
//! knowledge profile (DESIGN §8.2), derives the frontend's Manim surface
//! from it (DESIGN §5.3/§5.4), and attaches frontend facts end to end.

use std::fmt::Write as _;
use std::path::Path;

use manim_lint::application::{check, manim_surface};
use manim_lint::cli::CheckArgs;
use manim_lint::frontend::index::analyze;
use manim_lint::knowledge;
use manim_lint::reporting::OutputFormat;
use manim_lint::source::SourceManager;

const SCENE_FILE: &str = "\
from manim import *


class Intro(Scene):
    def construct(self):
        square = Square()
        self.play(Create(square))
";

fn write_project(root: &Path, knowledge_profile: Option<&str>) {
    let mut pyproject = String::from("[tool.manim-lint]\n");
    if let Some(name) = knowledge_profile {
        let _ = writeln!(pyproject, "knowledge-profile = \"{name}\"");
    }
    std::fs::write(root.join("pyproject.toml"), pyproject).unwrap();
    std::fs::write(root.join("intro.py"), SCENE_FILE).unwrap();
}

fn args_for(root: &Path) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: Some(OutputFormat::Concise),
        ..CheckArgs::default()
    }
}

#[test]
fn unknown_knowledge_profile_is_a_config_error() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), Some("no_such_profile"));
    let error = check(&args_for(project.path())).unwrap_err();
    assert!(
        error.to_string().contains("no_such_profile"),
        "error should name the unknown profile: {error}"
    );
}

#[test]
fn default_canonical_and_alias_profile_names_all_check_cleanly() {
    for profile in [None, Some("upstream_0_20"), Some("v0_20")] {
        let project = tempfile::tempdir().unwrap();
        write_project(project.path(), profile);
        let report = check(&args_for(project.path())).unwrap();
        assert!(
            report.diagnostics.is_empty(),
            "clean scene must stay clean under profile {profile:?}: {:?}",
            report.diagnostics
        );
    }
}

#[test]
fn surface_is_derived_from_profile_symbol_kinds() {
    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).unwrap();
    let surface = manim_surface(&profile);

    assert_eq!(
        surface.star_exports.get("Create").map(String::as_str),
        Some("manim.animation.creation.Create")
    );
    assert!(surface.scene_bases.contains("manim.scene.scene.Scene"));
    // VMobject-kind symbols count as Mobject bases.
    assert!(
        surface
            .mobject_bases
            .contains("manim.mobject.types.vectorized_mobject.VMobject")
    );
    assert!(
        surface
            .animation_bases
            .contains("manim.animation.animation.Animation")
    );
    // Non-class symbol kinds never become bases.
    for base in surface
        .scene_bases
        .iter()
        .chain(&surface.mobject_bases)
        .chain(&surface.animation_bases)
    {
        let entry = profile.symbol(base).expect("base ids come from symbols");
        assert!(
            !matches!(
                entry.kind,
                knowledge::SymbolKind::Function
                    | knowledge::SymbolKind::Method
                    | knowledge::SymbolKind::Constant
                    | knowledge::SymbolKind::Camera
            ),
            "{base} must be a class kind"
        );
    }
}

#[test]
fn profile_derived_surface_discovers_scenes_and_resolves_calls() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), None);

    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).unwrap();
    let surface = manim_surface(&profile);
    let mut sources = SourceManager::new(project.path());
    sources.load_file(&project.path().join("intro.py"));
    let facts = analyze(&sources, &[".".to_owned()], &surface);

    assert!(facts.index.scene_classes.contains("intro.Intro"));
    let play_calls: Vec<_> = facts
        .calls
        .calls_with_candidate("manim.scene.scene.Scene.play")
        .collect();
    assert_eq!(play_calls.len(), 1, "self.play resolves via the profile");
    let create_calls: Vec<_> = facts
        .calls
        .calls_with_candidate("manim.animation.creation.Create")
        .collect();
    assert_eq!(create_calls.len(), 1, "Create resolves via star exports");
}
