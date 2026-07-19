//! `MLR109`: a definite one-frame lag between two custom mobject updaters.
//!
//! Manim updates scene roots in list order. For leaf roots, each root's
//! updaters finish before the next root starts. The conservative subset
//! here proves that an earlier updater passes `driver.get_center()`
//! directly into a geometry write, while the later driver's updater writes
//! geometry from `dt` / another frame-varying read during a dynamic wait.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::cost::estimator::num_bounds_json;
use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayKind, UpdaterHost};
use crate::semantic::state::CallbackRef;
use crate::semantic::values::{Cardinality, KindSet, ObjectId, Presence, Truth};

use super::{build_diagnostic, site_range};

const MLR109: RuleMetadata = RuleMetadata {
    id: "MLR109",
    summary: "Updater ordering makes a one-frame dependency lag definite",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

pub(super) struct UpdaterReadBeforeWriter;

impl Rule for UpdaterReadBeforeWriter {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR109
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the cross-updater proof keeps frame, identity, order, and channel gates together"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let profiles = context.config().active_profile_names();
        let mut seen = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for wait in &scene.plays {
                if wait.kind != PlayKind::Wait
                    || wait.certainty != Presence::Present
                    || wait.dynamic_wait != Truth::Yes
                    || wait
                        .duration
                        .lower_bound()
                        .is_none_or(|seconds| seconds <= 0.0)
                    || wait
                        .repetitions
                        .lower_bound()
                        .is_none_or(|count| count < 1.0)
                {
                    continue;
                }
                let Some(snapshot) = scene.state_at(wait.site.file, wait.site.start) else {
                    continue;
                };
                let Some(scene_state) = snapshot.heap.scene(&scene.scene_id) else {
                    continue;
                };
                if scene_state.roots.order_known != Truth::Yes {
                    continue;
                }
                for reader in &scene.updaters {
                    let UpdaterHost::Mobject(reader_id) = &reader.host else {
                        continue;
                    };
                    if reader.certainty != Presence::Present
                        || !matches!(reader.fact.callback, CallbackRef::Lambda(_))
                        || reader_id.cardinality != Cardinality::Singleton
                    {
                        continue;
                    }
                    let reader_id = snapshot.heap.resolve(reader_id);
                    if active_leaf_root(snapshot, &reader_id, &reader.fact).is_none() {
                        continue;
                    }
                    let Some(reader_index) = root_index(snapshot, scene_state, &reader_id) else {
                        continue;
                    };
                    for read in reader
                        .body
                        .target_reads
                        .iter()
                        .filter(|read| read.certainty == Truth::Yes)
                    {
                        let Some(bound_writer) = snapshot.object_bindings.get(&read.binding) else {
                            continue;
                        };
                        let writer_id = snapshot.heap.resolve(bound_writer);
                        if writer_id == reader_id || writer_id.cardinality != Cardinality::Singleton
                        {
                            continue;
                        }
                        let Some(writer_index) = root_index(snapshot, scene_state, &writer_id)
                        else {
                            continue;
                        };
                        if reader_index >= writer_index {
                            continue;
                        }
                        for writer in &scene.updaters {
                            let UpdaterHost::Mobject(host) = &writer.host else {
                                continue;
                            };
                            if writer.certainty != Presence::Present
                                || !snapshot.heap.are_aliased(host, &writer_id)
                                || writer.body.mutates_target != Truth::Yes
                                || writer.body.channels_known != Truth::Yes
                                || !writer.body.write_channels.contains(&read.channel)
                                || (writer.body.uses_dt != Truth::Yes
                                    && writer.body.reads_frame_varying != Truth::Yes)
                            {
                                continue;
                            }
                            if active_leaf_root(snapshot, &writer_id, &writer.fact).is_none() {
                                continue;
                            }
                            let key = (
                                read.site.file,
                                read.site.start,
                                read.site.end,
                                writer.site.file,
                                writer.site.start,
                                writer.site.end,
                            );
                            if !seen.insert(key) {
                                continue;
                            }
                            let file = context.sources().file(read.site.file);
                            let mut evidence = BTreeMap::new();
                            evidence.insert("read_binding".to_owned(), json!(read.binding));
                            evidence.insert("read_method".to_owned(), json!(read.method));
                            evidence
                                .insert("channel".to_owned(), json!(format!("{:?}", read.channel)));
                            evidence.insert("reader_root_index".to_owned(), json!(reader_index));
                            evidence.insert("writer_root_index".to_owned(), json!(writer_index));
                            evidence.insert(
                                "dynamic_wait_seconds".to_owned(),
                                num_bounds_json(&wait.duration),
                            );
                            let mut diagnostic = build_diagnostic(
                                &MLR109,
                                file,
                                site_range(read.site),
                                Confidence::Medium,
                                format!(
                                    "This updater reads `{}.{}` before that object's frame-varying updater runs, so it observes the previous frame; add the writer to the Scene first or combine the dependent updates",
                                    read.binding, read.method,
                                ),
                                "Scene.update_mobjects walks top-level mobjects in scene order, and each leaf runs all of its own updaters before the next root. Here the dependent reader is earlier than the geometry writer during a proven dynamic wait, so every rendered frame reads the writer's state from the preceding frame. Reorder Scene.add arguments so the writer updates first, or perform both mutations in one updater.",
                                evidence,
                                profiles.clone(),
                                None,
                            );
                            let writer_file = context.sources().file(writer.site.file);
                            diagnostic.related_locations.push(RelatedLocation {
                                path: writer_file.relative_path().to_owned(),
                                span: writer_file.span_of_range(site_range(writer.site)),
                                message: "frame-varying geometry writer registered here".to_owned(),
                            });
                            let wait_file = context.sources().file(wait.site.file);
                            diagnostic.related_locations.push(RelatedLocation {
                                path: wait_file.relative_path().to_owned(),
                                span: wait_file.span_of_range(site_range(wait.site)),
                                message: "this dynamic wait renders the lagged updater order"
                                    .to_owned(),
                            });
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
        }
        diagnostics
    }
}

fn active_leaf_root<'a>(
    snapshot: &'a crate::semantic::interpreter::StateSnapshot,
    object: &ObjectId,
    updater: &crate::semantic::state::UpdaterFact,
) -> Option<&'a crate::semantic::state::MobjectState> {
    let state = snapshot.heap.object(object)?;
    let exact_manim_kind = matches!(
        &state.kind,
        KindSet::Known(candidates)
            if !candidates.is_empty()
                && candidates.iter().all(|candidate| candidate.starts_with("manim."))
    );
    (exact_manim_kind
        && state.scene_root_membership == Presence::Present
        && state.family_membership == Presence::Present
        && state.updating_suspended == Truth::No
        && state.children.is_empty()
        && state.parents.is_empty()
        && state.updaters.contains(updater))
    .then_some(state)
}

fn root_index(
    snapshot: &crate::semantic::interpreter::StateSnapshot,
    scene: &crate::semantic::state::SceneState,
    object: &ObjectId,
) -> Option<usize> {
    scene
        .roots
        .items
        .iter()
        .position(|root| snapshot.heap.are_aliased(root, object))
}
