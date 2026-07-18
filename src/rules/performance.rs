//! Performance / cost-multiplicity rules (`MLP2xx`, DESIGN §7.3).
//!
//! Phase 3 first tranche over `CostFacts` and `LifecycleFacts`:
//! `MLP201`, `MLP204`, `MLP205`, `MLP206`, `MLP220`, `MLP226`.
//! Cardinality tranche over the wave-5 size facts:
//! `MLP202`, `MLP203`, `MLP207`, `MLP208`, `MLP211`, `MLP216`, `MLP224`.
//! Timing / display-order tranche over the wave-5 fact layers
//! (`z_index` display order, updater-body dataflow, ownership intervals,
//! `always_update_mobjects` tracking): `MLP209`, `MLP210`, `MLP215`,
//! `MLP218`, `MLP219`, `MLP221`, `MLP222`, `MLP227`.
//! Fork-overlay tranche over the curated `fork_capabilities` block of the
//! local knowledge profile (inert under `upstream_0_20`, whose accessors
//! return `None`): `MLP214`, `MLP217`.
//!
//! Deliberately unimplemented catalog ids (a reserved rule never
//! fake-fires, DESIGN §12):
//!
//! - `MLP225` is the cost-report-only local-fork-overlay rule.

mod allocation;
mod display_order;
pub(crate) mod frame_scope;
mod hot_construction;
mod hot_receiver;
mod loop_plays;
mod redraw_geometry;
mod sampling;
mod scene_mutation;
pub(crate) mod support;
mod svg_cache;
mod tex_precompile;
mod timing;
mod traced_path;
mod transform;
mod updater_waste;
mod wait_updates;

use crate::rules::base::Rule;

/// Every implemented performance rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a performance rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(hot_construction::HotExpensiveConstruction),
        Box::new(hot_receiver::HotFamilyCopy),
        Box::new(hot_receiver::HotFamilyWalk),
        Box::new(scene_mutation::HotSceneGraphGrowth),
        Box::new(timing::StaticUnfrozenWait),
        Box::new(timing::SubFrameDuration),
        Box::new(transform::MismatchedTransformBegin),
        Box::new(transform::TextFamilyTransform),
        Box::new(display_order::FrontLoadedStaticSuffix),
        Box::new(loop_plays::FixedCountLoopPlays),
        Box::new(allocation::HotLargeAllocation),
        Box::new(updater_waste::NoOpUpdaterCost),
        Box::new(redraw_geometry::StableGeometryRedraw),
        Box::new(updater_waste::FrameInvariantDynamicWait),
        Box::new(updater_waste::LongLivedUpdater),
        Box::new(traced_path::UnboundedTracedPath),
        Box::new(sampling::ExcessiveSampleCount),
        Box::new(display_order::ImageInMovingSuffix),
        Box::new(hot_receiver::HotPointFromProportion),
        Box::new(hot_construction::FrameVaryingResourceKey),
        Box::new(wait_updates::ForcedAlwaysUpdateWait),
        // Fork-overlay tranche (inert unless the selected knowledge
        // profile declares the matching `fork_capabilities` block):
        Box::new(tex_precompile::SerialTexCompileKeys),
        Box::new(svg_cache::FrameVaryingSvgCacheGrowth),
    ]
}
