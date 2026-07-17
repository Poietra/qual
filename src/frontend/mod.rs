//! Syntactic frontend: parsing facade, imports, names, index, and the
//! intra-procedural CFG (DESIGN §5.1 pipeline stages before the abstract
//! interpreter).

pub mod cfg;
pub mod imports;
pub mod index;
pub mod names;
pub mod parser;

use std::collections::{BTreeMap, BTreeSet};

/// The Manim public API surface the frontend resolves names against.
///
/// The frontend never reads the versioned knowledge profile directly; the
/// application layer populates this struct from the selected profile so the
/// resolver stays decoupled from `crate::knowledge`. Tests build small
/// instances by hand.
///
/// Every value is a canonical fully qualified symbol id, e.g.
/// `manim.scene.scene.Scene`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManimSurface {
    /// Names exported by `from manim import *`, mapped to their canonical
    /// symbol id (e.g. `Square` → `manim.mobject.geometry.polygram.Square`).
    /// Also used for explicit `from manim import X` and `manim.X` attribute
    /// resolution.
    pub star_exports: BTreeMap<String, String>,
    /// Canonical class ids that count as Manim `Scene` bases. A project class
    /// is discovered as a Scene only when its resolved base chain reaches one
    /// of these ids — never from the string `"Scene"` alone (DESIGN §5.3).
    pub scene_bases: BTreeSet<String>,
    /// Canonical class ids that count as Manim `Mobject` bases.
    pub mobject_bases: BTreeSet<String>,
    /// Canonical class ids that count as Manim `Animation` bases.
    pub animation_bases: BTreeSet<String>,
}
