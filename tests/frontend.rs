//! Frontend name-resolution tests (DESIGN §11.2 layer 2): explicit aliases,
//! star imports, relative imports, shadowing, inheritance chains, Scene
//! discovery, and import-style-independent qualified call facts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use qual::frontend::ManimSurface;
use qual::frontend::index::{
    ArgShape, AttributeRef, FrontendFacts, LiteralFact, ParamFact, ParamKind, QualifiedCall,
    ReceiverKind, analyze,
};
use qual::frontend::statements::{
    StatementRole, each_class_scope_statement, for_loop_with_body_statement, walk_certain_exprs,
};
use qual::source::{FileId, SourceManager};
use rustpython_parser::ast::{self, Ranged};

const SCENE: &str = "manim.scene.scene.Scene";
const SQUARE: &str = "manim.mobject.geometry.polygram.Square";
const FADE_IN: &str = "manim.animation.fading.FadeIn";
const MOBJECT: &str = "manim.mobject.mobject.Mobject";
const ANIMATION: &str = "manim.animation.animation.Animation";
const DOT: &str = "manim.mobject.geometry.arc.Dot";

fn surface() -> ManimSurface {
    let mut surface = ManimSurface::default();
    for (name, canonical) in [
        ("Scene", SCENE),
        ("Square", SQUARE),
        ("FadeIn", FADE_IN),
        ("Mobject", MOBJECT),
        ("Dot", DOT),
        ("Create", "manim.animation.creation.Create"),
        ("RIGHT", "manim.constants.RIGHT"),
    ] {
        surface
            .star_exports
            .insert(name.to_owned(), canonical.to_owned());
    }
    surface.scene_bases.insert(SCENE.to_owned());
    surface.mobject_bases.insert(MOBJECT.to_owned());
    surface.mobject_bases.insert(SQUARE.to_owned());
    surface.animation_bases.insert(ANIMATION.to_owned());
    surface.animation_bases.insert(FADE_IN.to_owned());
    surface
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/resolver")
}

fn load(files: &[&str]) -> (SourceManager, FrontendFacts) {
    let root = fixture_root();
    let mut sources = SourceManager::new(&root);
    for file in files {
        sources.load_file(&root.join(file));
    }
    for file in sources.files() {
        assert!(
            file.is_parsed(),
            "fixture must parse: {}",
            file.relative_path()
        );
    }
    let facts = analyze(&sources, &[".".to_owned()], &surface());
    (sources, facts)
}

fn file_id(sources: &SourceManager, relative: &str) -> FileId {
    sources
        .files()
        .iter()
        .find(|file| file.relative_path() == relative)
        .unwrap_or_else(|| panic!("fixture not loaded: {relative}"))
        .id()
}

/// All facts in `relative` whose callee source text equals `callee`.
fn calls_named<'a>(
    sources: &SourceManager,
    facts: &'a FrontendFacts,
    relative: &str,
    callee: &str,
) -> Vec<&'a QualifiedCall> {
    let id = file_id(sources, relative);
    let file = sources.file(id);
    facts
        .calls
        .calls_in_file(id)
        .filter(|call| file.slice(call.callee_range) == callee)
        .collect()
}

fn single_call<'a>(
    sources: &SourceManager,
    facts: &'a FrontendFacts,
    relative: &str,
    callee: &str,
) -> &'a QualifiedCall {
    let calls = calls_named(sources, facts, relative, callee);
    assert_eq!(calls.len(), 1, "expected exactly one `{callee}(...)` call");
    calls[0]
}

fn candidates(call: &QualifiedCall) -> BTreeSet<&str> {
    call.candidates.iter().map(String::as_str).collect()
}

#[test]
fn explicit_aliases_resolve_to_canonical_ids() {
    let (sources, facts) = load(&["alias_import.py"]);

    let sq = single_call(&sources, &facts, "alias_import.py", "Sq");
    assert_eq!(candidates(sq), BTreeSet::from([SQUARE]));
    assert_eq!(sq.receiver, ReceiverKind::Direct);

    let fade_in = single_call(&sources, &facts, "alias_import.py", "mn.FadeIn");
    assert_eq!(candidates(fade_in), BTreeSet::from([FADE_IN]));
    assert_eq!(fade_in.receiver, ReceiverKind::ModuleAlias);

    assert!(facts.index.is_scene_class("alias_import.Demo"));
}

#[test]
fn tracked_instance_receiver_resolves_method_candidates() {
    let (sources, facts) = load(&["alias_import.py"]);
    let shift = single_call(&sources, &facts, "alias_import.py", "sq.shift");
    assert_eq!(
        shift.receiver,
        ReceiverKind::KnownInstance(SQUARE.to_owned())
    );
    assert_eq!(
        candidates(&shift.clone()),
        BTreeSet::from(["manim.mobject.geometry.polygram.Square.shift"])
    );
}

#[test]
fn star_import_discovers_scene_and_resolves_direct_names() {
    let (sources, facts) = load(&["star_import.py"]);
    assert!(facts.index.is_scene_class("star_import.StarScene"));

    let fade_in = single_call(&sources, &facts, "star_import.py", "FadeIn");
    assert_eq!(candidates(fade_in), BTreeSet::from([FADE_IN]));
    assert_eq!(fade_in.receiver, ReceiverKind::Direct);

    let square = single_call(&sources, &facts, "star_import.py", "Square");
    assert_eq!(candidates(square), BTreeSet::from([SQUARE]));

    // The FadeIn argument references the nested Square call fact.
    let ArgShape::Call(index) = fade_in.arguments[0].shape else {
        panic!("FadeIn argument should be a call fact reference");
    };
    assert_eq!(&facts.calls.calls[index], square);
}

#[test]
fn self_play_resolves_inside_discovered_scene() {
    let (sources, facts) = load(&["star_import.py"]);
    let play = single_call(&sources, &facts, "star_import.py", "self.play");
    assert_eq!(play.receiver, ReceiverKind::SelfScene);
    assert_eq!(
        candidates(play),
        BTreeSet::from(["manim.scene.scene.Scene.play"])
    );
    assert_eq!(play.context.module, "star_import");
    assert_eq!(
        play.context.class_name.as_deref(),
        Some("star_import.StarScene")
    );
    assert_eq!(play.context.function.as_deref(), Some("construct"));
}

#[test]
fn relative_imports_and_inheritance_chains_discover_scenes() {
    let (sources, facts) = load(&[
        "my_scenes/__init__.py",
        "my_scenes/base.py",
        "my_scenes/scenes.py",
    ]);
    let scenes = &facts.index.scene_classes;
    assert!(scenes.contains("my_scenes.base.BaseScene"));
    assert!(
        scenes.contains("my_scenes.scenes.ChainScene"),
        "chain via project-local BaseScene"
    );
    assert!(
        scenes.contains("my_scenes.scenes.ModulePathScene"),
        "chain via `from . import base` + base.BaseScene"
    );

    let play = single_call(&sources, &facts, "my_scenes/scenes.py", "self.play");
    assert_eq!(play.receiver, ReceiverKind::SelfScene);
    assert_eq!(
        candidates(play),
        BTreeSet::from(["manim.scene.scene.Scene.play"])
    );

    // A project ancestor's own method wins over the canonical base.
    let helper = single_call(&sources, &facts, "my_scenes/scenes.py", "self.setup_helper");
    assert_eq!(helper.receiver, ReceiverKind::SelfScene);
    assert_eq!(
        candidates(helper),
        BTreeSet::from(["my_scenes.base.BaseScene.setup_helper"])
    );
}

#[test]
fn package_reexport_and_literal_all_are_respected() {
    let (_sources, facts) = load(&[
        "my_scenes/__init__.py",
        "my_scenes/base.py",
        "uses_package.py",
    ]);
    let package = &facts.index.modules["my_scenes"];
    assert!(package.exports.from_literal_all);
    assert_eq!(
        package.exports.names,
        BTreeSet::from(["BaseScene".to_owned()])
    );
    assert!(facts.index.is_scene_class("uses_package.PackScene"));
}

#[test]
fn project_star_import_reexports_scene_base() {
    let (_sources, facts) = load(&[
        "my_scenes/__init__.py",
        "my_scenes/base.py",
        "star_project.py",
    ]);
    assert!(facts.index.is_scene_class("star_project.ProjectStarScene"));
    let module = &facts.index.modules["star_project"];
    assert!(!module.has_unknown_star, "project star import is resolved");
}

#[test]
fn class_named_scene_is_never_treated_as_manim_scene() {
    let (_sources, facts) = load(&["not_a_scene.py"]);
    assert!(facts.index.scene_classes.is_empty());
    let my_scene = &facts.index.classes["not_a_scene.MyScene"];
    assert!(my_scene.kinds.is_empty());
    assert!(
        my_scene.bases_fully_resolved,
        "base resolved to the local class"
    );
    assert!(my_scene.reached_bases.is_empty());
}

#[test]
fn shadowing_follows_lexical_position_at_module_level() {
    let (sources, facts) = load(&["shadowing.py"]);
    let calls = calls_named(&sources, &facts, "shadowing.py", "x");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        candidates(calls[0]),
        BTreeSet::from([SQUARE]),
        "before rebind"
    );
    assert!(calls[1].candidates.is_empty(), "after `x = 5`");
}

#[test]
fn shadowing_follows_lexical_position_inside_functions() {
    let (sources, facts) = load(&["shadowing.py"]);
    let calls = calls_named(&sources, &facts, "shadowing.py", "y.shift");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].receiver,
        ReceiverKind::KnownInstance(SQUARE.to_owned())
    );
    assert_eq!(
        candidates(calls[0]),
        BTreeSet::from(["manim.mobject.geometry.polygram.Square.shift"])
    );
    assert_eq!(calls[1].receiver, ReceiverKind::Unknown, "after `y = 5`");
    assert!(calls[1].candidates.is_empty());
}

#[test]
fn unresolvable_star_import_widens_to_unknown_never_guesses() {
    let (sources, facts) = load(&["unknown_star.py"]);
    let module = &facts.index.modules["unknown_star"];
    assert!(module.has_unknown_star);
    assert!(!module.exports.complete);

    let whatever = single_call(&sources, &facts, "unknown_star.py", "Whatever");
    assert_eq!(whatever.receiver, ReceiverKind::Unknown);
    assert!(whatever.candidates.is_empty());

    let spin = single_call(&sources, &facts, "unknown_star.py", "thing.spin");
    assert_eq!(spin.receiver, ReceiverKind::Unknown);
    assert!(spin.candidates.is_empty());
}

#[test]
fn mobject_and_animation_subclasses_are_discovered() {
    let (_sources, facts) = load(&["mobjects.py"]);
    assert!(facts.index.mobject_classes.contains("mobjects.FancySquare"));
    assert!(facts.index.animation_classes.contains("mobjects.SlowFade"));
    assert!(facts.index.scene_classes.is_empty());
}

#[test]
fn same_facts_regardless_of_import_style() {
    let (mn_sources, mn_facts) = load(&["import_style_mn.py"]);
    let (star_sources, star_facts) = load(&["import_style_star.py"]);

    assert!(mn_facts.index.is_scene_class("import_style_mn.StyleScene"));
    assert!(
        star_facts
            .index
            .is_scene_class("import_style_star.StyleScene")
    );

    let mn_id = file_id(&mn_sources, "import_style_mn.py");
    let star_id = file_id(&star_sources, "import_style_star.py");
    let mn_calls: Vec<_> = mn_facts.calls.calls_in_file(mn_id).collect();
    let star_calls: Vec<_> = star_facts.calls.calls_in_file(star_id).collect();
    assert_eq!(mn_calls.len(), star_calls.len());
    for (mn_call, star_call) in mn_calls.iter().zip(&star_calls) {
        assert_eq!(
            mn_call.candidates, star_call.candidates,
            "diagnosis-relevant candidates must not depend on import style"
        );
        assert_eq!(mn_call.positional_count, star_call.positional_count);
        assert_eq!(mn_call.keyword_names, star_call.keyword_names);
        assert_eq!(mn_call.arguments.len(), star_call.arguments.len(),);
    }

    let play = single_call(&mn_sources, &mn_facts, "import_style_mn.py", "self.play");
    assert_eq!(play.receiver, ReceiverKind::SelfScene);
    assert_eq!(play.keyword_names, BTreeSet::from(["run_time".to_owned()]));
}

#[test]
fn argument_shapes_are_summarized() {
    let root = fixture_root();
    let mut sources = SourceManager::new(&root);
    sources.load_bytes(
        &root.join("inline_args.py"),
        b"from manim import Square, FadeIn\n\n\ndef build(scene):\n    scene.play(FadeIn(Square()), lambda: 1, 5, name=\"x\")\n",
    );
    let facts = analyze(&sources, &[".".to_owned()], &surface());

    let play = single_call(&sources, &facts, "inline_args.py", "scene.play");
    assert_eq!(
        play.receiver,
        ReceiverKind::Unknown,
        "opaque parameter receiver"
    );
    assert_eq!(play.positional_count, 3);
    assert_eq!(play.keyword_names, BTreeSet::from(["name".to_owned()]));
    let shapes: Vec<_> = play
        .arguments
        .iter()
        .map(|argument| argument.shape.clone())
        .collect();
    let ArgShape::Call(fade_index) = shapes[0] else {
        panic!("first argument is a call fact reference");
    };
    assert_eq!(
        facts.calls.calls[fade_index].candidates,
        BTreeSet::from([FADE_IN.to_owned()])
    );
    assert_eq!(shapes[1], ArgShape::Lambda);
    assert_eq!(shapes[2], ArgShape::Literal);
    assert_eq!(shapes[3], ArgShape::Literal);
    assert_eq!(play.arguments[3].keyword.as_deref(), Some("name"));

    // The nested Square call is referenced from the FadeIn fact.
    let ArgShape::Call(square_index) = facts.calls.calls[fade_index].arguments[0].shape else {
        panic!("FadeIn argument is a call fact reference");
    };
    assert_eq!(
        facts.calls.calls[square_index].candidates,
        BTreeSet::from([SQUARE.to_owned()])
    );
}

#[test]
fn loop_carried_bindings_are_widened_but_straight_line_stays_precise() {
    let root = fixture_root();
    let mut sources = SourceManager::new(&root);
    sources.load_bytes(
        &root.join("inline_loop.py"),
        b"from manim import Square\n\n\ndef build():\n    for _ in range(3):\n        sq.shift()\n        sq = Square()\n        sq.scale()\n",
    );
    let facts = analyze(&sources, &[".".to_owned()], &surface());

    let shift = single_call(&sources, &facts, "inline_loop.py", "sq.shift");
    assert_eq!(
        shift.receiver,
        ReceiverKind::Unknown,
        "loop-carried use before assignment"
    );

    let scale = single_call(&sources, &facts, "inline_loop.py", "sq.scale");
    assert_eq!(
        scale.receiver,
        ReceiverKind::KnownInstance(SQUARE.to_owned()),
        "straight-line use after assignment in the same iteration"
    );
}

fn load_inline(name: &str, source: &str) -> (SourceManager, FrontendFacts) {
    let root = fixture_root();
    let mut sources = SourceManager::new(&root);
    sources.load_bytes(&root.join(name), source.as_bytes());
    for file in sources.files() {
        assert!(file.is_parsed(), "inline source must parse: {source}");
    }
    let facts = analyze(&sources, &[".".to_owned()], &surface());
    (sources, facts)
}

#[test]
fn bare_attribute_arguments_expose_receiver_and_candidates() {
    let (sources, facts) = load_inline(
        "attr_args.py",
        "from manim import *\n\n\nclass AttrScene(Scene):\n    def construct(self):\n        mob = Dot()\n        self.play(mob.shift, RIGHT)\n",
    );
    let play = single_call(&sources, &facts, "attr_args.py", "self.play");
    assert_eq!(play.receiver, ReceiverKind::SelfScene);

    assert_eq!(
        play.arguments[0].shape,
        ArgShape::Attribute(AttributeRef {
            name: "shift".to_owned(),
            receiver: ReceiverKind::KnownInstance(DOT.to_owned()),
            candidates: BTreeSet::from(["manim.mobject.geometry.arc.Dot.shift".to_owned()]),
        }),
        "bound-method-style argument records base classification"
    );
    assert_eq!(play.arguments[0].literal, None);

    assert_eq!(play.arguments[1].shape, ArgShape::Name);
    assert!(
        play.arguments[1].kind_candidates.is_empty(),
        "RIGHT is a constant, not a tracked instance"
    );
}

#[test]
fn literal_values_are_folded_and_summarized() {
    let (sources, facts) = load_inline(
        "literal_args.py",
        concat!(
            "def build(scene):\n",
            "    scene.play(\n",
            "        -1,\n",
            "        0.0,\n",
            "        r\"\\frac{a}{b}\",\n",
            "        f\"{scene}\",\n",
            "        \"a\" \"b\",\n",
            "        True,\n",
            "        None,\n",
            "        9223372036854775808,\n",
            "        run_time=-1.5,\n",
            "    )\n",
        ),
    );
    let play = single_call(&sources, &facts, "literal_args.py", "scene.play");
    let file = sources.file(file_id(&sources, "literal_args.py"));

    assert_eq!(play.arguments[0].literal, Some(LiteralFact::Int(-1)));
    assert_eq!(
        play.arguments[0].shape,
        ArgShape::Literal,
        "folded negative counts as a literal"
    );
    assert_eq!(play.arguments[1].literal, Some(LiteralFact::Float(0.0)));

    let Some(LiteralFact::Str {
        value,
        prefix,
        range,
    }) = &play.arguments[2].literal
    else {
        panic!("raw string argument must carry a Str fact");
    };
    assert_eq!(value, "\\frac{a}{b}");
    assert!(prefix.raw);
    assert!(!prefix.bytes);
    assert!(!prefix.formatted);
    assert_eq!(
        file.slice(*range),
        "r\"\\frac{a}{b}\"",
        "range covers the literal exactly, prefix and quotes included"
    );

    assert_eq!(
        play.arguments[3].literal, None,
        "f-strings never produce a Str literal value"
    );
    assert_eq!(play.arguments[3].shape, ArgShape::Literal);

    let Some(LiteralFact::Str { value, prefix, .. }) = &play.arguments[4].literal else {
        panic!("implicit concatenation of plain parts must carry a Str fact");
    };
    assert_eq!(value, "ab");
    assert!(!prefix.raw);

    assert_eq!(play.arguments[5].literal, Some(LiteralFact::Bool(true)));
    assert_eq!(play.arguments[6].literal, Some(LiteralFact::NoneLit));
    assert_eq!(
        play.arguments[7].literal, None,
        "integers that do not fit i64 carry no fact"
    );

    let run_time = play.keyword("run_time").expect("run_time keyword");
    assert_eq!(run_time.literal, Some(LiteralFact::Float(-1.5)));
}

#[test]
fn name_arguments_carry_instance_kind_candidates() {
    let (sources, facts) = load_inline(
        "kind_args.py",
        "from manim import Square\n\n\ndef build(scene, thing):\n    sq = Square()\n    scene.play(sq)\n    scene.play(thing)\n",
    );
    let plays = calls_named(&sources, &facts, "kind_args.py", "scene.play");
    assert_eq!(plays.len(), 2);

    assert_eq!(plays[0].arguments[0].shape, ArgShape::Name);
    assert_eq!(
        plays[0].arguments[0].kind_candidates,
        BTreeSet::from([SQUARE.to_owned()])
    );
    assert!(
        plays[1].arguments[0].kind_candidates.is_empty(),
        "an opaque parameter never guesses a kind"
    );
}

#[test]
fn lambda_arguments_expose_declared_signatures() {
    let (sources, facts) = load_inline(
        "lambda_args.py",
        "def build(scene):\n    scene.play(\n        lambda dt: dt,\n        lambda m, dt=0: m,\n        lambda *, k: 0,\n        lambda x, /, y: x,\n        lambda *args, **kw: 0,\n    )\n",
    );
    let play = single_call(&sources, &facts, "lambda_args.py", "scene.play");
    let signature = |index: usize| -> Vec<ParamFact> {
        play.arguments[index]
            .callable_signature
            .clone()
            .unwrap_or_else(|| panic!("argument {index} must carry a signature"))
            .params
    };
    let param = |name: &str, kind: ParamKind, has_default: bool| ParamFact {
        name: name.to_owned(),
        kind,
        has_default,
    };

    assert_eq!(play.arguments[0].shape, ArgShape::Lambda);
    assert_eq!(
        signature(0),
        vec![param("dt", ParamKind::PositionalOrKeyword, false)]
    );
    assert_eq!(
        signature(1),
        vec![
            param("m", ParamKind::PositionalOrKeyword, false),
            param("dt", ParamKind::PositionalOrKeyword, true),
        ]
    );
    assert_eq!(
        signature(2),
        vec![param("k", ParamKind::KeywordOnly, false)],
        "lambdas support keyword-only params after a bare star"
    );
    assert_eq!(
        signature(3),
        vec![
            param("x", ParamKind::PositionalOnly, false),
            param("y", ParamKind::PositionalOrKeyword, false),
        ]
    );
    assert_eq!(
        signature(4),
        vec![
            param("args", ParamKind::VarArgs, false),
            param("kw", ParamKind::KwArgs, false),
        ]
    );
}

#[test]
fn def_resolved_name_arguments_expose_signatures() {
    let (sources, facts) = load_inline(
        "sig_defs.py",
        concat!(
            "def updater(mob, dt=0):\n",
            "    return mob\n",
            "\n\n",
            "class Helper:\n",
            "    def tick(self, dt):\n",
            "        return dt\n",
            "\n\n",
            "def build(scene, missing):\n",
            "    scene.play(updater)\n",
            "    scene.play(missing)\n",
        ),
    );
    let plays = calls_named(&sources, &facts, "sig_defs.py", "scene.play");
    assert_eq!(plays.len(), 2);

    let signature = plays[0].arguments[0]
        .callable_signature
        .as_ref()
        .expect("module-level def resolves by name");
    assert_eq!(
        signature.params,
        vec![
            ParamFact {
                name: "mob".to_owned(),
                kind: ParamKind::PositionalOrKeyword,
                has_default: false,
            },
            ParamFact {
                name: "dt".to_owned(),
                kind: ParamKind::PositionalOrKeyword,
                has_default: true,
            },
        ]
    );
    assert_eq!(
        plays[1].arguments[0].callable_signature, None,
        "an unresolvable name never guesses a signature"
    );

    let method = facts
        .index
        .function_signature("sig_defs.Helper.tick")
        .expect("method signatures are indexed by qualified name");
    assert_eq!(
        method.params.first().map(|param| param.name.as_str()),
        Some("self"),
        "method signatures keep the written self parameter"
    );
    assert!(facts.index.function_signature("sig_defs.absent").is_none());
}

#[test]
fn star_argument_splats_set_flags_and_accessors() {
    let (sources, facts) = load_inline(
        "star_args.py",
        "def build(scene, anims, opts):\n    scene.play(*anims)\n    scene.play(**opts)\n    scene.play(1, 2, run_time=3)\n",
    );
    let plays = calls_named(&sources, &facts, "star_args.py", "scene.play");
    assert_eq!(plays.len(), 3);

    assert!(plays[0].has_star_args);
    assert!(!plays[0].has_star_star_kwargs);
    assert!(!plays[1].has_star_args);
    assert!(plays[1].has_star_star_kwargs);

    let plain = plays[2];
    assert!(!plain.has_star_args && !plain.has_star_star_kwargs);
    assert_eq!(
        plain.positional(0).and_then(|arg| arg.literal.clone()),
        Some(LiteralFact::Int(1))
    );
    assert_eq!(
        plain.positional(1).and_then(|arg| arg.literal.clone()),
        Some(LiteralFact::Int(2))
    );
    assert!(
        plain.positional(2).is_none(),
        "keyword arguments are not positional"
    );
    assert_eq!(
        plain
            .keyword("run_time")
            .and_then(|arg| arg.literal.clone()),
        Some(LiteralFact::Int(3))
    );
    assert!(plain.keyword("rate_func").is_none());
}

#[test]
fn conditional_rebinding_joins_to_unknown() {
    let root = fixture_root();
    let mut sources = SourceManager::new(&root);
    sources.load_bytes(
        &root.join("inline_branch.py"),
        b"from manim import Square, Create\n\ndef build(flag):\n    x = Square\n    if flag:\n        x = Create\n    x()\n",
    );
    let facts = analyze(&sources, &[".".to_owned()], &surface());
    let call = single_call(&sources, &facts, "inline_branch.py", "x");
    assert_eq!(call.receiver, ReceiverKind::Unknown);
    assert!(
        call.candidates.is_empty(),
        "conflicting branch bindings never guess"
    );
}

#[test]
fn tuple_and_list_displays_yield_element_literals() {
    let (sources, facts) = load_inline(
        "literal_sequences.py",
        concat!(
            "def build(axes, x):\n",
            "    axes.plot(\n",
            "        (0, 1, 0.01),\n",
            "        [1, -2],\n",
            "        (1, f\"{x}\"),\n",
            "        (1, (2, 3)),\n",
            "        (1, *x),\n",
            "    )\n",
        ),
    );
    let plot = single_call(&sources, &facts, "literal_sequences.py", "axes.plot");

    // Literal t_range-style tuple (MLP221).
    assert_eq!(
        plot.arguments[0].literal,
        Some(LiteralFact::Tuple(vec![
            LiteralFact::Int(0),
            LiteralFact::Int(1),
            LiteralFact::Float(0.01),
        ]))
    );
    assert_eq!(plot.arguments[0].shape, ArgShape::Literal);

    // List displays fold folded-negative numbers too.
    assert_eq!(
        plot.arguments[1].literal,
        Some(LiteralFact::List(vec![
            LiteralFact::Int(1),
            LiteralFact::Int(-2),
        ]))
    );

    // A single non-literal element voids the whole display fact.
    assert_eq!(
        plot.arguments[2].literal, None,
        "an f-string element voids the tuple fact"
    );

    // Nesting is allowed.
    assert_eq!(
        plot.arguments[3].literal,
        Some(LiteralFact::Tuple(vec![
            LiteralFact::Int(1),
            LiteralFact::Tuple(vec![LiteralFact::Int(2), LiteralFact::Int(3)]),
        ]))
    );

    // Starred elements make the element count unknown: no fact.
    assert_eq!(
        plot.arguments[4].literal, None,
        "starred elements void the fact"
    );
}

// ---------------------------------------------------------------------------
// Statement facts (frontend::statements::StatementFacts).
// ---------------------------------------------------------------------------

/// The (line, role) of every `Square(...)` call in the roles fixture.
#[test]
fn statement_facts_classify_call_roles_including_nested_statements() {
    let (sources, facts) = load(&["statement_roles.py"]);
    let id = file_id(&sources, "statement_roles.py");
    let file = sources.file(id);

    let mut roles: Vec<(usize, StatementRole)> = facts
        .calls
        .calls_in_file(id)
        .filter(|call| file.slice(call.callee_range) == "Square")
        .map(|call| {
            let fact = facts
                .calls
                .statements
                .for_call(call)
                .expect("every call has a statement fact");
            let line = file.span_of_range(call.call_range).start.line;
            (line, fact.role.clone())
        })
        .collect();
    roles.sort_by_key(|(line, _)| *line);

    assert_eq!(
        roles,
        vec![
            (8, StatementRole::BareExpression),
            (
                10,
                StatementRole::AssignmentRhs {
                    target: Some("sq".to_owned()),
                },
            ),
            // Chained assignment: no single plain-name target.
            (12, StatementRole::AssignmentRhs { target: None }),
            (16, StatementRole::ReturnValue),
            (24, StatementRole::WithContext),
            // The `if` test is not a bare expression statement.
            (27, StatementRole::Other),
            // The call inside the `if` body anchors to its own (nested)
            // statement, not the enclosing `if`.
            (28, StatementRole::BareExpression),
            (
                30,
                StatementRole::AssignmentRhs {
                    target: Some("total".to_owned()),
                },
            ),
            // The nested constructor argument is not the assignment RHS.
            (31, StatementRole::Other),
        ]
    );
}

#[test]
fn statement_facts_span_whole_multi_line_statements() {
    let (sources, facts) = load(&["statement_roles.py"]);
    let id = file_id(&sources, "statement_roles.py");
    let file = sources.file(id);

    // The nested call on line 31 sits inside the assignment statement
    // spanning lines 30-32.
    let nested = facts
        .calls
        .calls_in_file(id)
        .find(|call| file.span_of_range(call.call_range).start.line == 31)
        .expect("nested constructor call");
    let fact = facts.calls.statements.for_call(nested).expect("fact");
    let span = file.span_of_range(fact.statement_range);
    assert_eq!(span.start.line, 30);
    assert_eq!(span.end.line, 32);

    // Decorator calls anchor to the decorated `def` statement (whose AST
    // range starts at the `def` keyword, after the decorator list).
    let decorator = single_call(&sources, &facts, "statement_roles.py", "wrap");
    let fact = facts.calls.statements.for_call(decorator).expect("fact");
    assert_eq!(fact.role, StatementRole::Decorator);
    let span = file.span_of_range(fact.statement_range);
    assert_eq!(span.start.line, 20);
    assert_eq!(span.end.line, 21);
}

// ---------------------------------------------------------------------------
// Binding facts (frontend::statements::BindingFacts).
// ---------------------------------------------------------------------------

#[test]
fn binding_facts_resolve_canonical_paths_through_aliases() {
    let (sources, facts) = load(&["binding_facts.py"]);
    let id = file_id(&sources, "binding_facts.py");
    let bindings = facts.calls.bindings.file(id).expect("parsed file");

    // `import numpy as np` + `np.random.seed` → canonical dotted path.
    let parts: Vec<String> = ["np", "random", "seed"]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    assert_eq!(
        bindings.resolve_parts(&parts).as_deref(),
        Some("numpy.random.seed")
    );

    // `from math import inf as INF` → `math.inf` under the alias.
    assert_eq!(bindings.import_target("INF"), Some("math.inf"));
    assert_eq!(
        bindings.resolve_parts(&["INF".to_owned()]).as_deref(),
        Some("math.inf")
    );

    assert!(bindings.has_star_from("manim"));
    assert!(!bindings.has_star_other_than("manim"));
}

#[test]
fn binding_facts_poison_rebound_names_everywhere() {
    let (sources, facts) = load(&["binding_facts.py"]);
    let id = file_id(&sources, "binding_facts.py");
    let bindings = facts.calls.bindings.file(id).expect("parsed file");

    // `nan = 3.0` poisons the `from math import nan` binding.
    assert!(bindings.is_poisoned("nan"));
    assert!(bindings.is_statement_assigned("nan"));
    assert_eq!(bindings.import_target("nan"), None);

    // A def parameter named `time` poisons the `import time` binding.
    assert!(bindings.is_poisoned("time"));
    assert_eq!(bindings.import_target("time"), None);
    assert_eq!(bindings.resolve_parts(&["time".to_owned()]), None);

    // A lambda parameter poisons the imported `urlopen`.
    assert!(bindings.is_poisoned("urlopen"));
    assert_eq!(bindings.import_target("urlopen"), None);

    // Walrus targets are assignment bindings.
    assert!(bindings.is_poisoned("scale"));
    assert!(bindings.is_statement_assigned("scale"));

    // Untouched names stay trusted / bare.
    assert_eq!(bindings.import_target("np"), Some("numpy"));
    assert!(bindings.is_unshadowed_bare("float"));
    assert!(!bindings.is_unshadowed_bare("nan"));

    // Statement-level binding counts: `nan` is bound by its import and by
    // the assignment; `INF` only by its import.
    assert_eq!(bindings.binding_statement_count("nan"), 2);
    assert_eq!(bindings.binding_statement_count("INF"), 1);
    assert_eq!(bindings.binding_statement_count("np"), 1);
}

// ---------------------------------------------------------------------------
// Callee facts (dotted chains and call-chained methods).
// ---------------------------------------------------------------------------

#[test]
fn callee_facts_expose_dotted_chains_and_chained_methods() {
    let (sources, facts) = load(&["binding_facts.py"]);

    let seed = single_call(&sources, &facts, "binding_facts.py", "np.random.seed");
    assert_eq!(
        seed.callee_dotted.as_deref(),
        Some(&["np".to_owned(), "random".to_owned(), "seed".to_owned()][..])
    );
    assert_eq!(seed.callee_chained, None);

    // `open("data.txt").read_text()`: the outer call records the chained
    // method and the inner call's fact index.
    let open_call = single_call(&sources, &facts, "binding_facts.py", "open");
    let chained = facts
        .calls
        .calls_in_file(file_id(&sources, "binding_facts.py"))
        .find_map(|call| call.callee_chained.as_ref())
        .expect("chained callee fact");
    assert_eq!(chained.method, "read_text");
    assert_eq!(&facts.calls.calls[chained.inner], open_call);
    assert_eq!(
        open_call.callee_dotted.as_deref(),
        Some(&["open".to_owned()][..])
    );
}

// ---------------------------------------------------------------------------
// Canonical traversal helpers (frontend::statements).
// ---------------------------------------------------------------------------

/// The class-scope walk enters direct methods and their nested functions
/// (with the enclosing function depth), but never a nested `class` body:
/// its `self` is a different instance.
#[test]
fn class_scope_walk_enters_methods_but_never_nested_class_bodies() {
    let (sources, _) = load_inline(
        "class_scope.py",
        "\
class Outer:
    def method(self):
        marker_method = 1
        def inner(dt):
            marker_inner = 1
    class Nested:
        def hidden(self):
            marker_hidden = 1
",
    );
    let id = file_id(&sources, "class_scope.py");
    let module = sources.file(id).ast().expect("parsed");
    let ast::Stmt::ClassDef(outer) = &module.body[0] else {
        panic!("expected a class statement");
    };

    let mut defs: Vec<(String, u32)> = Vec::new();
    let mut assigned: Vec<String> = Vec::new();
    each_class_scope_statement(&outer.body, &mut |stmt, depth| match stmt {
        ast::Stmt::FunctionDef(def) => defs.push((def.name.to_string(), depth)),
        ast::Stmt::ClassDef(def) => defs.push((def.name.to_string(), depth)),
        ast::Stmt::Assign(assign) => {
            for target in &assign.targets {
                if let ast::Expr::Name(name) = target {
                    assigned.push(name.id.to_string());
                }
            }
        }
        _ => {}
    });

    // `method` and the nested `Nested` class statement sit at class-body
    // depth 0; `inner` is nested inside a method (depth 1); `hidden`
    // belongs to the nested class's scope and is never visited.
    assert_eq!(
        defs,
        vec![
            ("method".to_owned(), 0),
            ("inner".to_owned(), 1),
            ("Nested".to_owned(), 0),
        ]
    );
    assert_eq!(
        assigned,
        vec!["marker_method".to_owned(), "marker_inner".to_owned()]
    );
}

/// The certainty-annotated walk visits only subexpressions evaluated on
/// every execution of the root: short-circuit tails, conditional
/// branches, lambda bodies, and comprehension parts are skipped.
#[test]
fn certain_walk_skips_deferred_and_conditional_subtrees() {
    let (sources, _) = load_inline(
        "certain.py",
        "certain() and tail()\nfirst() if test() else orelse()\n(lambda: deferred())\n[body() for x in iterable()]\n",
    );
    let id = file_id(&sources, "certain.py");
    let module = sources.file(id).ast().expect("parsed");

    let mut called: Vec<String> = Vec::new();
    for stmt in &module.body {
        let ast::Stmt::Expr(expr) = stmt else {
            panic!("expression statements only");
        };
        walk_certain_exprs(&expr.value, &mut |certain| {
            if let ast::Expr::Call(call) = certain {
                if let ast::Expr::Name(name) = call.func.as_ref() {
                    called.push(name.id.to_string());
                }
            }
        });
    }

    // `tail` (short-circuit), `first` / `orelse` (branches), `deferred`
    // (lambda body), and `body` / `iterable` (comprehension) are all
    // uncertain; only `certain` and the conditional's `test` remain.
    assert_eq!(called, vec!["certain".to_owned(), "test".to_owned()]);
}

/// `for_loop_with_body_statement` resolves the loop whose *direct* body
/// owns a statement; `orelse` suites and nested positions do not count.
#[test]
fn for_loop_lookup_matches_direct_body_statements_only() {
    let (sources, facts) = load_inline(
        "loops.py",
        "\
for i in range(4):
    direct = Square()
else:
    in_orelse = Square()

standalone = Square()
",
    );
    let id = file_id(&sources, "loops.py");
    let file = sources.file(id);

    let statement_of = |line: usize| {
        facts
            .calls
            .calls_in_file(id)
            .find(|call| file.span_of_range(call.call_range).start.line == line)
            .and_then(|call| facts.calls.statements.for_call(call))
            .map(|fact| fact.statement_range)
            .expect("statement fact")
    };

    let in_body = for_loop_with_body_statement(file, statement_of(2)).expect("loop found");
    assert_eq!(file.slice(in_body.iter.range()), "range(4)");
    assert!(for_loop_with_body_statement(file, statement_of(4)).is_none());
    assert!(for_loop_with_body_statement(file, statement_of(6)).is_none());
}
