//! Updater-waste rules: `MLP215` (no-op updater), `MLP218`
//! (frame-invariant updater dynamicizes a wait), `MLP219` (long-lived
//! updater across many plays) — DESIGN §3.3, §7.3.

use std::collections::BTreeMap;

use rustpython_parser::ast::{self, Ranged};
use serde_json::{Value, json};

use crate::cost::estimator::{num_bounds_json, sum_frames_across_profiles, sum_seconds};
use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::render_order::{
    DisplayOrder, MovingReason, SuffixFact, inputs_at_play, moving_scope_at_play,
    moving_suffix_evidence,
};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::interpreter::{
    PlayFact, PlayKind, SceneLifecycle, UpdaterHost, UpdaterRegistration,
};
use crate::semantic::state::CallbackRef;
use crate::semantic::values::{Num, NumLit, ObjectId, Presence, Truth};
use crate::source::SourceFile;

use super::frame_scope::{
    camera_truth, frames_at_play, intrinsic_per_frame_kinds, related_site, renders_frames,
    site_range, wait_solely_dynamicized_by,
};
use super::support::{display_frames, display_seconds};

// ---------------------------------------------------------------------------
// MLP215: provably no-op updater.
// ---------------------------------------------------------------------------

/// Metadata for [`NoOpUpdaterCost`].
pub const MLP215: RuleMetadata = RuleMetadata {
    id: "MLP215",
    summary: "Provably no-op updater widens dynamic waits or the play moving scope",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// An updater whose body provably does nothing — no mutation of its
/// parameter, no write to any captured state, no unknown call — yet
/// costs per-frame work through one of two separate channels
/// (DESIGN §7.3 `MLP215`): (a) as a `dt`-parameter / Scene updater it
/// dynamicizes waits that would otherwise freeze; (b) as a family
/// updater it makes its host a moving member of every play's Cairo
/// moving scope. A one-argument no-op updater does **not** dynamicize a
/// plain wait (DESIGN §3.3) — only channel (b) can apply to it.
pub struct NoOpUpdaterCost;

/// Whether the registration's body is provably a no-op: it neither
/// mutates its parameter nor performs any effect outside pure reads and
/// local computation (`pure_affine_on_target == Yes` means the
/// disallowed-effect evidence is empty, which covers captured-state
/// writes and unmodeled calls), and no unknown call could hide one.
fn provably_no_op(registration: &UpdaterRegistration) -> bool {
    registration.body.mutates_target == Truth::No
        && registration.body.calls_unknown == Truth::No
        && registration.body.pure_affine_on_target == Truth::Yes
}

/// One play where the registration's host provably *leads* the moving
/// scope because of its updaters (quantified), or provably enters it
/// (qualitative).
struct ScopeLeg<'p> {
    play: &'p PlayFact,
    sentence: Option<String>,
}

/// The moving-scope legs of a mobject-host registration: plays rendering
/// frames where the host is a certainly-moving member with reason
/// `FamilyUpdater`. Quantified when the host is the first moving member
/// of a known order; qualitative (no numbers) otherwise.
fn scope_legs<'p>(
    scene: &'p SceneLifecycle,
    registration: &UpdaterRegistration,
) -> Vec<ScopeLeg<'p>> {
    let UpdaterHost::Mobject(host) = &registration.host else {
        return Vec::new();
    };
    let camera = camera_truth(scene.camera_kind);
    let mut legs = Vec::new();
    for play in &scene.plays {
        if play.kind != PlayKind::Play
            || play.certainty != Presence::Present
            || play.site.file != registration.site.file
            || play.site.start < registration.site.end
        {
            continue;
        }
        let Ok(inputs) = inputs_at_play(scene, play) else {
            continue;
        };
        let order = DisplayOrder::compute(&inputs);
        let scope = moving_scope_at_play(scene, play, camera);
        let Some(suffix) = SuffixFact::compute(&order, &inputs, &scope) else {
            // Unknown order: no positional claim is possible, and without
            // the member evidence no sound qualitative claim either.
            continue;
        };
        let Some(snapshot) = scene.state_at(play.site.file, play.site.end) else {
            continue;
        };
        let resolved = snapshot.heap.resolve(host);
        let Some(entry) = suffix.members_evidence.iter().find(|evidence| {
            evidence.id == resolved
                && evidence.reason == MovingReason::FamilyUpdater
                && evidence.certainty == Truth::Yes
        }) else {
            continue;
        };
        // Quantify only when the host is the exact first moving member
        // (the static suffix behind it is then attributable to the
        // updater); otherwise the claim stays qualitative.
        let leads = matches!(
            suffix.first_moving_index,
            Some(Num::Exact(NumLit::Int(first))) if i64::try_from(entry.index) == Ok(first)
        );
        let sentence = if leads {
            moving_suffix_evidence(&order, &suffix)
        } else {
            None
        };
        legs.push(ScopeLeg { play, sentence });
    }
    legs
}

impl Rule for NoOpUpdaterCost {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP215
    }

    #[allow(
        clippy::too_many_lines,
        reason = "two evidence legs with per-leg quantification discipline"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let intrinsic = intrinsic_per_frame_kinds(profile);
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for registration in &scene.updaters {
                if registration.certainty != Presence::Present || !provably_no_op(registration) {
                    continue;
                }
                // Leg (a): waits this registration solely dynamicizes.
                let waits: Vec<&PlayFact> = scene
                    .plays
                    .iter()
                    .filter(|play| {
                        wait_solely_dynamicized_by(scene, play, registration, &intrinsic)
                    })
                    .collect();
                // Leg (b): plays whose moving scope the host enters.
                let scopes = scope_legs(scene, registration);
                if waits.is_empty() && scopes.is_empty() {
                    continue;
                }
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                evidence.insert(
                    "body".to_owned(),
                    json!({
                        "mutates_target": "no",
                        "calls_unknown": "no",
                        "pure_reads_only": true,
                    }),
                );
                let mut related: Vec<RelatedLocation> = Vec::new();
                let mut parts: Vec<String> = Vec::new();
                if !waits.is_empty() {
                    let mut wait_json = Vec::new();
                    for play in &waits {
                        let frames = frames_at_play(context, play);
                        wait_json.push(json!({
                            "frames": num_bounds_json(&frames),
                        }));
                        related.push(related_site(
                            context,
                            play.site.file,
                            play.site.start,
                            play.site.end,
                            format!(
                                "this wait renders {frames} frames dynamically solely \
                                 because of the no-op updater",
                                frames = display_frames(&frames),
                            ),
                        ));
                    }
                    evidence.insert("dynamicized_waits".to_owned(), Value::Array(wait_json));
                    parts.push(format!(
                        "it is the only reason {count} wait(s) render dynamically \
                         instead of freezing",
                        count = waits.len(),
                    ));
                }
                if !scopes.is_empty() {
                    let mut scope_json = Vec::new();
                    for leg in &scopes {
                        scope_json.push(json!({
                            "quantified": leg.sentence.is_some(),
                        }));
                        let note = leg.sentence.as_ref().map_or_else(
                            || {
                                "the updater makes its host a moving member of this \
                                 play's Cairo scope"
                                    .to_owned()
                            },
                            |sentence| format!("the updater-bearing host {sentence}"),
                        );
                        related.push(related_site(
                            context,
                            leg.play.site.file,
                            leg.play.site.start,
                            leg.play.site.end,
                            note,
                        ));
                    }
                    evidence.insert("moving_scope_plays".to_owned(), Value::Array(scope_json));
                    parts.push(format!(
                        "it makes its host a moving member of {count} play(s)' \
                         Cairo moving scope",
                        count = scopes.len(),
                    ));
                }
                let file = context.sources().file(registration.site.file);
                diagnostics.push(Diagnostic {
                    rule_id: MLP215.id.to_owned(),
                    severity: MLP215.default_severity,
                    confidence: Confidence::High,
                    path: file.relative_path().to_owned(),
                    primary_span: file
                        .span_of_range(site_range(registration.site.start, registration.site.end)),
                    message: format!(
                        "This updater's body provably has no effect (no mutation, no \
                         unknown call), yet {parts}. Remove the updater, or give it \
                         its intended effect.",
                        parts = parts.join(", and "),
                    ),
                    explanation: Some(
                        "A registered updater costs per-frame work even when its body \
                         does nothing: a dt-parameter or Scene updater makes waits \
                         dynamic (DESIGN 3.3), and any family updater makes its host \
                         a moving member of Cairo's per-frame re-raster scope \
                         (scene.py get_moving_mobjects). A one-argument no-op updater \
                         does not dynamicize a plain wait, so only the moving-scope \
                         channel is ever attributed to one."
                            .to_owned(),
                    ),
                    related_locations: related,
                    evidence,
                    estimated_cost: None,
                    applicable_profiles: context.config().active_profile_names(),
                    fix: None,
                });
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP218: frame-invariant updater dynamicizes a plain wait.
// ---------------------------------------------------------------------------

/// Metadata for [`FrameInvariantDynamicWait`].
pub const MLP218: RuleMetadata = RuleMetadata {
    id: "MLP218",
    summary: "Provably frame-invariant updater is the only reason a wait renders dynamically",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// A wait made dynamic solely by an updater whose body provably produces
/// the same result every frame: `dt` is never read, no frame-varying
/// state is read, no unknown call exists, every effect is an absolute
/// setter on the parameter with parameter-free arguments — so every
/// rendered frame of the wait is identical and the full frame pipeline
/// runs for nothing (DESIGN §7.3 `MLP218`).
pub struct FrameInvariantDynamicWait;

/// Absolute-position / absolute-style setters: applying the same call
/// with the same arguments every frame reproduces the same state.
/// Relative accumulators (`shift`, `rotate`, `scale`, `flip`,
/// `stretch`) change state every frame and are deliberately absent.
fn absolute_setter(method: &str) -> bool {
    method.starts_with("set_")
        || matches!(
            method,
            "move_to" | "next_to" | "to_edge" | "to_corner" | "align_to" | "center"
        )
}

/// Whether `expr` mentions the name `param` anywhere.
fn mentions_name(expr: &ast::Expr, param: &str) -> bool {
    let mut found = false;
    visit_exprs(expr, &mut |inner| {
        if let ast::Expr::Name(name) = inner {
            if name.id.as_str() == param {
                found = true;
            }
        }
    });
    found
}

/// Pre-order walk over an expression tree. Deliberately exhaustive (no
/// wildcard arm): [`mentions_name`] must see *every* subexpression, or a
/// hidden parameter read could fake the frame-invariance proof.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive expression visitor: one arm per AST variant"
)]
fn visit_exprs(expr: &ast::Expr, visit: &mut impl FnMut(&ast::Expr)) {
    fn walk_all(exprs: &[ast::Expr], visit: &mut impl FnMut(&ast::Expr)) {
        for inner in exprs {
            visit_exprs(inner, visit);
        }
    }
    fn walk_generators(generators: &[ast::Comprehension], visit: &mut impl FnMut(&ast::Expr)) {
        for generator in generators {
            visit_exprs(&generator.target, visit);
            visit_exprs(&generator.iter, visit);
            for test in &generator.ifs {
                visit_exprs(test, visit);
            }
        }
    }
    visit(expr);
    match expr {
        ast::Expr::BoolOp(inner) => walk_all(&inner.values, visit),
        ast::Expr::NamedExpr(inner) => {
            visit_exprs(&inner.target, visit);
            visit_exprs(&inner.value, visit);
        }
        ast::Expr::BinOp(inner) => {
            visit_exprs(&inner.left, visit);
            visit_exprs(&inner.right, visit);
        }
        ast::Expr::UnaryOp(inner) => visit_exprs(&inner.operand, visit),
        ast::Expr::Lambda(inner) => {
            // A nested lambda may capture the parameter (shadowing is
            // ignored: over-reporting a mention only fails the proof).
            for arg in inner
                .args
                .posonlyargs
                .iter()
                .chain(&inner.args.args)
                .chain(&inner.args.kwonlyargs)
            {
                if let Some(default) = &arg.default {
                    visit_exprs(default, visit);
                }
            }
            visit_exprs(&inner.body, visit);
        }
        ast::Expr::IfExp(inner) => {
            visit_exprs(&inner.test, visit);
            visit_exprs(&inner.body, visit);
            visit_exprs(&inner.orelse, visit);
        }
        ast::Expr::Dict(inner) => {
            for key in inner.keys.iter().flatten() {
                visit_exprs(key, visit);
            }
            walk_all(&inner.values, visit);
        }
        ast::Expr::Set(inner) => walk_all(&inner.elts, visit),
        ast::Expr::ListComp(inner) => {
            visit_exprs(&inner.elt, visit);
            walk_generators(&inner.generators, visit);
        }
        ast::Expr::SetComp(inner) => {
            visit_exprs(&inner.elt, visit);
            walk_generators(&inner.generators, visit);
        }
        ast::Expr::DictComp(inner) => {
            visit_exprs(&inner.key, visit);
            visit_exprs(&inner.value, visit);
            walk_generators(&inner.generators, visit);
        }
        ast::Expr::GeneratorExp(inner) => {
            visit_exprs(&inner.elt, visit);
            walk_generators(&inner.generators, visit);
        }
        ast::Expr::Await(inner) => visit_exprs(&inner.value, visit),
        ast::Expr::Yield(inner) => {
            if let Some(value) = &inner.value {
                visit_exprs(value, visit);
            }
        }
        ast::Expr::YieldFrom(inner) => visit_exprs(&inner.value, visit),
        ast::Expr::Compare(inner) => {
            visit_exprs(&inner.left, visit);
            walk_all(&inner.comparators, visit);
        }
        ast::Expr::Call(inner) => {
            visit_exprs(&inner.func, visit);
            walk_all(&inner.args, visit);
            for keyword in &inner.keywords {
                visit_exprs(&keyword.value, visit);
            }
        }
        ast::Expr::FormattedValue(inner) => visit_exprs(&inner.value, visit),
        ast::Expr::JoinedStr(inner) => walk_all(&inner.values, visit),
        ast::Expr::Attribute(inner) => visit_exprs(&inner.value, visit),
        ast::Expr::Subscript(inner) => {
            visit_exprs(&inner.value, visit);
            visit_exprs(&inner.slice, visit);
        }
        ast::Expr::Starred(inner) => visit_exprs(&inner.value, visit),
        ast::Expr::List(inner) => walk_all(&inner.elts, visit),
        ast::Expr::Tuple(inner) => walk_all(&inner.elts, visit),
        ast::Expr::Slice(inner) => {
            for part in [&inner.lower, &inner.upper, &inner.step]
                .into_iter()
                .flatten()
            {
                visit_exprs(part, visit);
            }
        }
        ast::Expr::Name(_) | ast::Expr::Constant(_) => {}
    }
}

/// Verifies the strict absolute-setter shape of a callback body
/// expression: every occurrence of the parameter is the receiver of an
/// allowlisted absolute setter whose arguments never mention the
/// parameter; lambdas / nested callables anywhere fail the proof.
fn expr_frame_invariant(expr: &ast::Expr, param: &str) -> bool {
    match expr {
        ast::Expr::Call(call) => {
            if let ast::Expr::Attribute(attribute) = call.func.as_ref() {
                if let ast::Expr::Name(base) = attribute.value.as_ref() {
                    if base.id.as_str() == param {
                        if !absolute_setter(attribute.attr.as_str()) {
                            return false;
                        }
                        return call
                            .args
                            .iter()
                            .chain(call.keywords.iter().map(|keyword| &keyword.value))
                            .all(|argument| {
                                !mentions_name(argument, param)
                                    && expr_frame_invariant(argument, param)
                            });
                    }
                }
            }
            expr_frame_invariant(&call.func, param)
                && call
                    .args
                    .iter()
                    .chain(call.keywords.iter().map(|keyword| &keyword.value))
                    .all(|argument| expr_frame_invariant(argument, param))
        }
        ast::Expr::Lambda(_) => false,
        ast::Expr::Name(name) => name.id.as_str() != param,
        ast::Expr::BoolOp(inner) => inner
            .values
            .iter()
            .all(|value| expr_frame_invariant(value, param)),
        ast::Expr::BinOp(inner) => {
            expr_frame_invariant(&inner.left, param) && expr_frame_invariant(&inner.right, param)
        }
        ast::Expr::UnaryOp(inner) => expr_frame_invariant(&inner.operand, param),
        ast::Expr::IfExp(inner) => {
            expr_frame_invariant(&inner.test, param)
                && expr_frame_invariant(&inner.body, param)
                && expr_frame_invariant(&inner.orelse, param)
        }
        ast::Expr::Attribute(inner) => expr_frame_invariant(&inner.value, param),
        ast::Expr::Subscript(inner) => {
            expr_frame_invariant(&inner.value, param) && expr_frame_invariant(&inner.slice, param)
        }
        ast::Expr::Tuple(inner) => inner
            .elts
            .iter()
            .all(|element| expr_frame_invariant(element, param)),
        ast::Expr::List(inner) => inner
            .elts
            .iter()
            .all(|element| expr_frame_invariant(element, param)),
        ast::Expr::Constant(_) => true,
        ast::Expr::Starred(inner) => expr_frame_invariant(&inner.value, param),
        // Anything else (comprehensions, yields, walrus, f-strings over
        // the parameter, ...) is out of the verified shape.
        other => !mentions_name(other, param),
    }
}

/// Resolves the registration's callback body and verifies the strict
/// frame-invariant shape. Only single-expression lambdas and
/// single-statement `def` bodies (one expression statement or one
/// `return`) are verified; anything larger is not proven.
fn callback_frame_invariant(file: &SourceFile, registration: &UpdaterRegistration) -> bool {
    let Some(module) = file.ast() else {
        return false;
    };
    match &registration.fact.callback {
        CallbackRef::Lambda(site) => {
            let range = site_range(site.start, site.end);
            let mut verified = false;
            for stmt in &module.body {
                find_lambda(stmt, range, &mut |lambda| {
                    let param = first_positional_param(&lambda.args);
                    let Some(param) = param else {
                        return;
                    };
                    verified = expr_frame_invariant(&lambda.body, &param);
                });
            }
            verified
        }
        CallbackRef::Named(qualified) => {
            let name = qualified.rsplit('.').next().unwrap_or(qualified);
            let mut matches: Vec<&ast::StmtFunctionDef> = Vec::new();
            for stmt in &module.body {
                collect_defs(stmt, name, &mut matches);
            }
            let [def] = matches.as_slice() else {
                return false;
            };
            let Some(param) = first_positional_param(&def.args) else {
                return false;
            };
            let body_expr = match def.body.as_slice() {
                [ast::Stmt::Expr(stmt)] => &stmt.value,
                [ast::Stmt::Return(stmt)] => match &stmt.value {
                    Some(value) => value,
                    None => return false,
                },
                _ => return false,
            };
            expr_frame_invariant(body_expr, &param)
        }
        CallbackRef::Unknown => false,
    }
}

fn first_positional_param(args: &ast::Arguments) -> Option<String> {
    args.posonlyargs
        .iter()
        .chain(&args.args)
        .next()
        .map(|arg| arg.def.arg.to_string())
}

/// Finds the lambda spanning exactly `range` anywhere under `stmt`.
fn find_lambda(
    stmt: &ast::Stmt,
    range: rustpython_parser::text_size::TextRange,
    visit: &mut impl FnMut(&ast::ExprLambda),
) {
    visit_stmt_exprs(stmt, &mut |expr| {
        if let ast::Expr::Lambda(lambda) = expr {
            if lambda.range() == range {
                visit(lambda);
            }
        }
    });
}

/// Collects `def` statements named `name` anywhere under `stmt`.
fn collect_defs<'a>(stmt: &'a ast::Stmt, name: &str, out: &mut Vec<&'a ast::StmtFunctionDef>) {
    if let ast::Stmt::FunctionDef(def) = stmt {
        if def.name.as_str() == name {
            out.push(def);
        }
    }
    for child in stmt_children(stmt) {
        collect_defs(child, name, out);
    }
}

/// Direct child statements of a statement.
fn stmt_children(stmt: &ast::Stmt) -> Vec<&ast::Stmt> {
    match stmt {
        ast::Stmt::FunctionDef(inner) => inner.body.iter().collect(),
        ast::Stmt::AsyncFunctionDef(inner) => inner.body.iter().collect(),
        ast::Stmt::ClassDef(inner) => inner.body.iter().collect(),
        ast::Stmt::For(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::AsyncFor(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::While(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::If(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::With(inner) => inner.body.iter().collect(),
        ast::Stmt::AsyncWith(inner) => inner.body.iter().collect(),
        ast::Stmt::Try(inner) => inner
            .body
            .iter()
            .chain(inner.handlers.iter().flat_map(|handler| {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                handler.body.iter()
            }))
            .chain(&inner.orelse)
            .chain(&inner.finalbody)
            .collect(),
        ast::Stmt::Match(inner) => inner
            .cases
            .iter()
            .flat_map(|case| case.body.iter())
            .collect(),
        _ => Vec::new(),
    }
}

/// Walks every expression under a statement (statements and nested
/// statements included).
fn visit_stmt_exprs(stmt: &ast::Stmt, visit: &mut impl FnMut(&ast::Expr)) {
    match stmt {
        ast::Stmt::Expr(inner) => visit_exprs(&inner.value, visit),
        ast::Stmt::Return(inner) => {
            if let Some(value) = &inner.value {
                visit_exprs(value, visit);
            }
        }
        ast::Stmt::Assign(inner) => {
            visit_exprs(&inner.value, visit);
            inner.targets.iter().for_each(|t| visit_exprs(t, visit));
        }
        ast::Stmt::AugAssign(inner) => {
            visit_exprs(&inner.value, visit);
            visit_exprs(&inner.target, visit);
        }
        ast::Stmt::AnnAssign(inner) => {
            if let Some(value) = &inner.value {
                visit_exprs(value, visit);
            }
            visit_exprs(&inner.target, visit);
        }
        ast::Stmt::For(inner) => {
            visit_exprs(&inner.iter, visit);
            visit_exprs(&inner.target, visit);
        }
        ast::Stmt::While(inner) => visit_exprs(&inner.test, visit),
        ast::Stmt::If(inner) => visit_exprs(&inner.test, visit),
        ast::Stmt::With(inner) => {
            for item in &inner.items {
                visit_exprs(&item.context_expr, visit);
            }
        }
        ast::Stmt::AsyncWith(inner) => {
            for item in &inner.items {
                visit_exprs(&item.context_expr, visit);
            }
        }
        ast::Stmt::Raise(inner) => {
            for part in [&inner.exc, &inner.cause].into_iter().flatten() {
                visit_exprs(part, visit);
            }
        }
        ast::Stmt::Assert(inner) => {
            visit_exprs(&inner.test, visit);
            if let Some(message) = &inner.msg {
                visit_exprs(message, visit);
            }
        }
        _ => {}
    }
    for child in stmt_children(stmt) {
        visit_stmt_exprs(child, visit);
    }
}

impl Rule for FrameInvariantDynamicWait {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP218
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let intrinsic = intrinsic_per_frame_kinds(profile);
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for registration in &scene.updaters {
                if registration.certainty != Presence::Present {
                    continue;
                }
                let body = registration.body;
                // A no-op body (mutates_target == No) is MLP215's defect;
                // this rule wants a real but frame-invariant write.
                if body.uses_dt != Truth::No
                    || body.reads_frame_varying != Truth::No
                    || body.calls_unknown != Truth::No
                    || body.pure_affine_on_target != Truth::Yes
                    || body.mutates_target != Truth::Yes
                {
                    continue;
                }
                let source = context.sources().file(registration.site.file);
                if !callback_frame_invariant(source, registration) {
                    continue;
                }
                for play in &scene.plays {
                    if !wait_solely_dynamicized_by(scene, play, registration, &intrinsic) {
                        continue;
                    }
                    let frames = frames_at_play(context, play);
                    let file = context.sources().file(play.site.file);
                    let mut evidence = BTreeMap::new();
                    evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                    evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                    evidence.insert(
                        "body".to_owned(),
                        json!({
                            "uses_dt": "no",
                            "reads_frame_varying": "no",
                            "calls_unknown": "no",
                            "effects": "absolute setters with parameter-free arguments",
                        }),
                    );
                    diagnostics.push(Diagnostic {
                        rule_id: MLP218.id.to_owned(),
                        severity: MLP218.default_severity,
                        confidence: Confidence::High,
                        path: file.relative_path().to_owned(),
                        primary_span: file
                            .span_of_range(site_range(play.site.start, play.site.end)),
                        message: format!(
                            "This wait renders {frames} frames dynamically only because \
                             of a provably frame-invariant updater: the callback never \
                             reads `dt` or frame-varying state and only applies absolute \
                             setters, so every rendered frame is identical. Apply the \
                             final state once (or drop the unused `dt` parameter) and \
                             let the wait freeze.",
                            frames = display_frames(&frames),
                        ),
                        explanation: Some(
                            "A dt-parameter updater makes plain waits dynamic \
                             (DESIGN 3.3) even when the body ignores dt. A body that \
                             reads no frame-varying state and performs only absolute \
                             setter calls with parameter-free arguments produces the \
                             same state every frame, so the full per-frame pipeline \
                             (update, interpolate, raster) repeats an identical image \
                             for the whole wait."
                                .to_owned(),
                        ),
                        related_locations: vec![related_site(
                            context,
                            registration.site.file,
                            registration.site.start,
                            registration.site.end,
                            "the frame-invariant updater registered here".to_owned(),
                        )],
                        evidence,
                        estimated_cost: None,
                        applicable_profiles: context.config().active_profile_names(),
                        fix: None,
                    });
                }
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP219: updater alive across many subsequent plays.
// ---------------------------------------------------------------------------

/// Metadata for [`LongLivedUpdater`].
pub const MLP219: RuleMetadata = RuleMetadata {
    id: "MLP219",
    summary: "Updater's estimated lifetime spans many subsequent plays",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// Minimum provable live span (seconds of rendering plays with the host
/// in the family and the updater still registered) before `MLP219`
/// fires.
pub const MLP219_MIN_LIVE_SECONDS: f64 = 5.0;

/// Minimum number of covered plays before `MLP219` fires ("many
/// subsequent plays", DESIGN §7.3).
pub const MLP219_MIN_COVERED_PLAYS: usize = 3;

/// A mobject updater that provably stays registered while its host lives
/// in the scene family across many subsequent plays: every covered frame
/// pays the updater invocation and keeps the host in the moving scope
/// (DESIGN §7.3 `MLP219`). The rule never claims the updater is unused —
/// it reports the estimated live frames only (binding §7.3 gate).
pub struct LongLivedUpdater;

/// The first byte after which the registration's updater set is mutated
/// again on the host (a `remove_updater` / `clear_updaters` or an
/// unknown updater write): the provable lifetime ends there.
fn lifetime_cut(scene: &SceneLifecycle, registration: &UpdaterRegistration) -> Option<u32> {
    let UpdaterHost::Mobject(host) = &registration.host else {
        return None;
    };
    let final_heap = &scene.final_heap;
    let host = final_heap.resolve(host);
    scene
        .events
        .iter()
        .filter(|traced| {
            traced.site.file == registration.site.file && traced.site.start >= registration.site.end
        })
        .find_map(|traced| match &traced.event {
            Event::Mutate(mutate)
                if mutate.kind == MutationKind::Updaters
                    && final_heap.resolve(&mutate.target) == host =>
            {
                Some(traced.site.start)
            }
            _ => None,
        })
}

/// Live-span accumulation over the plays the updater provably covers.
struct LiveSpan<'p> {
    plays: Vec<&'p PlayFact>,
    seconds: Num,
}

fn live_span<'p>(
    scene: &'p SceneLifecycle,
    registration: &UpdaterRegistration,
    host: &ObjectId,
) -> Option<LiveSpan<'p>> {
    let cut = lifetime_cut(scene, registration);
    let mut plays = Vec::new();
    for play in &scene.plays {
        if play.site.file != registration.site.file
            || play.site.start < registration.site.end
            || play.certainty != Presence::Present
            || !renders_frames(play)
        {
            continue;
        }
        if cut.is_some_and(|cut| play.site.start >= cut) {
            break;
        }
        let (_, family) = scene.membership_at(host, play.site.file, play.site.start)?;
        if family != Presence::Present {
            continue;
        }
        // Plays that animate the host suspend its updaters for their
        // duration (DESIGN §3.2): they do not count as live frames.
        let targets_host = play.animations.iter().any(|animation| {
            animation.state.as_ref().is_some_and(|state| {
                state
                    .targets
                    .iter()
                    .any(|target| target.may_be_same(host) != Truth::No)
            })
        });
        if targets_host {
            continue;
        }
        plays.push(play);
    }
    let seconds = sum_seconds(plays.iter().map(|play| &play.duration));
    (!plays.is_empty()).then_some(LiveSpan { plays, seconds })
}

impl Rule for LongLivedUpdater {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP219
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for registration in &scene.updaters {
                if registration.certainty != Presence::Present {
                    continue;
                }
                let UpdaterHost::Mobject(host) = &registration.host else {
                    continue;
                };
                let host = scene.final_heap.resolve(host);
                let Some(span) = live_span(scene, registration, &host) else {
                    continue;
                };
                if span.plays.len() < MLP219_MIN_COVERED_PLAYS {
                    continue;
                }
                let Some(seconds_lower) = span.seconds.lower_bound() else {
                    continue;
                };
                if seconds_lower < MLP219_MIN_LIVE_SECONDS {
                    continue;
                }
                // Per-play frame grids: ceil each play's duration, then
                // sum (never ceil the summed seconds).
                let frames = sum_frames_across_profiles(
                    span.plays.iter().map(|play| &play.duration),
                    context.active_profiles(),
                );
                let file = context.sources().file(registration.site.file);
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                evidence.insert("covered_plays".to_owned(), json!(span.plays.len()));
                evidence.insert("live_seconds".to_owned(), num_bounds_json(&span.seconds));
                evidence.insert("live_frames".to_owned(), num_bounds_json(&frames));
                evidence.insert(
                    "gates".to_owned(),
                    json!({
                        "min_live_seconds": MLP219_MIN_LIVE_SECONDS,
                        "min_covered_plays": MLP219_MIN_COVERED_PLAYS,
                    }),
                );
                let related = span
                    .plays
                    .iter()
                    .map(|play| {
                        related_site(
                            context,
                            play.site.file,
                            play.site.start,
                            play.site.end,
                            "the updater runs every frame of this play".to_owned(),
                        )
                    })
                    .collect();
                diagnostics.push(Diagnostic {
                    rule_id: MLP219.id.to_owned(),
                    severity: MLP219.default_severity,
                    confidence: Confidence::Medium,
                    path: file.relative_path().to_owned(),
                    primary_span: file
                        .span_of_range(site_range(registration.site.start, registration.site.end)),
                    message: format!(
                        "This updater stays registered for an estimated {seconds} \
                         ({frames} frames) across {plays} subsequent plays. If the \
                         effect is only needed early, remove the updater after the \
                         last play that needs it; if the whole span is intended, this \
                         is the expected cost.",
                        seconds = display_seconds(&span.seconds),
                        frames = display_frames(&frames),
                        plays = span.plays.len(),
                    ),
                    explanation: Some(
                        "A registered updater is invoked every rendered frame while \
                         its host is in the scene family, and it keeps the host in \
                         Cairo's moving scope (DESIGN 3.3 / 3.4). Static analysis \
                         cannot prove when the effect stops being needed, so this \
                         reports the estimated live span only — not that the updater \
                         is unused."
                            .to_owned(),
                    ),
                    related_locations: related,
                    evidence,
                    estimated_cost: None,
                    applicable_profiles: context.config().active_profile_names(),
                    fix: None,
                });
            }
        }
        diagnostics
    }
}
