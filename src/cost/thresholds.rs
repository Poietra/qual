//! Named emission-gate thresholds (DESIGN §7.3) as citable constants.
//!
//! Every gate is a [`Threshold`]: a named minimum with the rule ids it
//! gates. The single confirmation rule is
//! [`Threshold::confirmed_by`]: a value passes only when its **proven
//! lower bound** reaches the minimum. `Unknown`, `Symbol`, and
//! open-below intervals never pass (DESIGN §15 invariant 2: no
//! certain/high claim from an unknown), so a widened loop bound like
//! `[1, ∞)` cannot fake a large family.
//!
//! Rules cite the gate in evidence via [`Threshold::evidence`], which
//! serializes the gate name, minimum, and the value's real bounds.

use serde_json::{Value, json};

use crate::semantic::values::Num;

use super::estimator::num_bounds_json;

/// One named DESIGN §7.3 emission gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    /// Stable gate name (used verbatim in evidence JSON).
    pub name: &'static str,
    /// The §4.1 quantity the gate is measured in.
    pub quantity: &'static str,
    /// Inclusive minimum a proven lower bound must reach.
    pub minimum: f64,
    /// Rule ids gated by this threshold.
    pub rule_ids: &'static [&'static str],
}

/// `MLP207` / `MLP208` start at `N_family >= 32` (DESIGN §7.3).
pub const TRANSFORM_FAMILY_GATE: Threshold = Threshold {
    name: "transform-family-gate",
    quantity: "N_family",
    minimum: 32.0,
    rule_ids: &["MLP207", "MLP208"],
};

/// `MLP207` / `MLP208` alternatively start at an estimated curve
/// insertion of `>= 256` (DESIGN §7.3).
pub const TRANSFORM_CURVE_INSERTION_GATE: Threshold = Threshold {
    name: "transform-curve-insertion-gate",
    quantity: "C",
    minimum: 256.0,
    rule_ids: &["MLP207", "MLP208"],
};

/// `MLP211` fires on statically proven `>= 64 KiB` allocated per frame
/// (DESIGN §7.3; small coordinate vectors are excluded by construction).
pub const PER_FRAME_ALLOCATION_GATE: Threshold = Threshold {
    name: "per-frame-allocation-gate",
    quantity: "bytes",
    minimum: 64.0 * 1024.0,
    rule_ids: &["MLP211"],
};

/// `MLP202` / `MLP203` require a **confirmed** large family
/// (DESIGN §7.3 "large family / points が確定した場合だけ"). The minimum
/// mirrors the `N_family >= 32` transform gate and the §4.4
/// `known_size_weight` rank boundary.
pub const LARGE_FAMILY_GATE: Threshold = Threshold {
    name: "large-family-gate",
    quantity: "N_family",
    minimum: 32.0,
    rule_ids: &["MLP202", "MLP203"],
};

/// `MLP202` / `MLP211` "known large points": the §4.4
/// `known_size_weight` top rank (`>= 1024`).
pub const LARGE_POINTS_GATE: Threshold = Threshold {
    name: "large-points-gate",
    quantity: "P",
    minimum: 1024.0,
    rule_ids: &["MLP202", "MLP211"],
};

/// `MLP212` reports only a provably long full-screen translucent
/// animation. Five seconds matches the existing long-span policy used by
/// the path/lifetime rules while keeping short transitions idiomatic.
pub const FULL_SCREEN_TRANSLUCENT_SECONDS_GATE: Threshold = Threshold {
    name: "full-screen-translucent-seconds-gate",
    quantity: "seconds",
    minimum: 5.0,
    rule_ids: &["MLP212"],
};

/// `MLP213`'s calibrated Cairo Surface boundary: the retained evidence
/// measures the default 32 × 32 Surface (1,024 faces). Smaller surfaces
/// stay below the initial advisory gate.
pub const CAIRO_SURFACE_FACE_GATE: Threshold = Threshold {
    name: "cairo-surface-face-gate",
    quantity: "surface_faces",
    minimum: 1024.0,
    rule_ids: &["MLP213"],
};

/// Every named gate, in declaration order (for docs / introspection).
pub const ALL_GATES: [Threshold; 7] = [
    TRANSFORM_FAMILY_GATE,
    TRANSFORM_CURVE_INSERTION_GATE,
    PER_FRAME_ALLOCATION_GATE,
    LARGE_FAMILY_GATE,
    LARGE_POINTS_GATE,
    FULL_SCREEN_TRANSLUCENT_SECONDS_GATE,
    CAIRO_SURFACE_FACE_GATE,
];

impl Threshold {
    /// Whether `value` **provably** meets the gate: its known lower bound
    /// reaches the minimum. `Unknown` / `Symbol` / open-below values never
    /// confirm a gate.
    #[must_use]
    pub fn confirmed_by(&self, value: &Num) -> bool {
        value
            .lower_bound()
            .is_some_and(|lower| lower >= self.minimum)
    }

    /// Human-readable citation, e.g.
    /// `"N_family >= 32 (MLP207/MLP208 emission gate, DESIGN 7.3)"`.
    #[must_use]
    pub fn citation(&self) -> String {
        format!(
            "{} >= {} ({} emission gate, DESIGN 7.3)",
            self.quantity,
            self.minimum,
            self.rule_ids.join("/"),
        )
    }

    /// Machine-readable evidence of the gate check: the gate identity, the
    /// minimum, the value's real bounds (`null` when unknown — never a
    /// fabricated number), and the confirmation verdict.
    #[must_use]
    pub fn evidence(&self, value: &Num) -> Value {
        json!({
            "threshold": self.name,
            "quantity": self.quantity,
            "minimum": self.minimum,
            "value": num_bounds_json(value),
            "confirmed": self.confirmed_by(value),
        })
    }
}

/// The combined `MLP207` / `MLP208` begin-cost gate: confirmed large
/// family **or** confirmed large curve insertion (DESIGN §7.3).
#[must_use]
pub fn transform_begin_gate_met(family: &Num, curve_delta: &Num) -> bool {
    TRANSFORM_FAMILY_GATE.confirmed_by(family)
        || TRANSFORM_CURVE_INSERTION_GATE.confirmed_by(curve_delta)
}
