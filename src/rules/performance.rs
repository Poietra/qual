//! Performance / cost-multiplicity rules (`MLP2xx`, DESIGN §7.3).
//!
//! Phase 3 first tranche over `CostFacts` and `LifecycleFacts`:
//! `MLP201`, `MLP204`, `MLP205`, `MLP206`, `MLP220`, `MLP226`.
//! Cardinality tranche over the wave-5 size facts:
//! `MLP202`, `MLP203`, `MLP207`, `MLP208`, `MLP211`, `MLP216`, `MLP224`.
//!
//! Deliberately unimplemented catalog ids (a reserved rule never
//! fake-fires, DESIGN §12):
//!
//! - `MLP217` needs a knowledge-profile declaration of process-global SVG
//!   cache semantics that `upstream_0_20` does not carry;
//! - `MLP218` needs a frame-invariance proof of updater bodies (no `dt`
//!   use, no frame-varying reads, idempotence) that no fact layer provides;
//! - `MLP221` needs `ParametricFunction` / `Axes.plot` curation that
//!   `upstream_0_20` does not carry;
//! - `MLP227` needs the literal value of `always_update_mobjects`, which
//!   the interpreter deliberately widens to Unknown;
//! - `MLP214` / `MLP225` are local-fork-overlay rules.

mod allocation;
mod hot_construction;
mod hot_receiver;
mod redraw_geometry;
mod scene_mutation;
pub(crate) mod support;
mod timing;
mod traced_path;
mod transform;

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
        Box::new(allocation::HotLargeAllocation),
        Box::new(redraw_geometry::StableGeometryRedraw),
        Box::new(traced_path::UnboundedTracedPath),
        Box::new(hot_receiver::HotPointFromProportion),
        Box::new(hot_construction::FrameVaryingResourceKey),
    ]
}
