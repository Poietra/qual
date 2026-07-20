//! Abstract value lattices and object identity (DESIGN §4.1, §5.5).
//!
//! Three-valued lattices (`Presence`, `Truth`, `Visibility`), the symbolic
//! numeric domain [`Num`], allocation-site object identity, and copy
//! provenance. Two invariants from DESIGN §4.1 and §15 are enforced here:
//!
//! - [`Num::Unknown`] never collapses to an exact `0` or `1` through any
//!   arithmetic or lattice operation (unknowns are never underestimated).
//! - Two [`Cardinality::Many`] object IDs are never assumed identical.

use std::collections::BTreeSet;

use rustpython_parser::text_size::TextRange;
use serde::{Deserialize, Serialize};

use crate::source::FileId;

/// Maximum call-string depth kept in a [`CallContextId`].
pub const MAX_CALL_CONTEXT_DEPTH: usize = 2;

/// Maximum number of kind candidates kept before widening to
/// [`KindSet::Unknown`].
pub const MAX_KIND_CANDIDATES: usize = 8;

/// Whether an object is in a collection (e.g. Scene membership).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Presence {
    /// Definitely not a member on every path.
    Absent,
    /// Definitely a member on every path.
    Present,
    /// Member on some paths only, or not provable either way.
    Maybe,
}

/// Three-valued boolean fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Truth {
    /// Definitely false on every path.
    No,
    /// Definitely true on every path.
    Yes,
    /// True on some paths only, or not provable either way.
    Maybe,
}

impl From<bool> for Truth {
    fn from(value: bool) -> Self {
        if value { Self::Yes } else { Self::No }
    }
}

/// Whether an object contributes visible pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Visibility {
    /// Definitely rendered visible.
    Visible,
    /// Definitely not visible.
    Invisible,
    /// Visible on some paths only, or not provable either way.
    Maybe,
}

macro_rules! three_valued_lattice {
    ($ty:ident, $top:ident) => {
        impl $ty {
            /// Least upper bound: agreement is kept, disagreement becomes
            #[doc = concat!("[`", stringify!($ty), "::", stringify!($top), "`].")]
            #[must_use]
            pub fn join(self, other: Self) -> Self {
                if self == other { self } else { Self::$top }
            }

            /// Whether this fact is definite (not the
            #[doc = concat!("[`", stringify!($ty), "::", stringify!($top), "`] top element).")]
            #[must_use]
            pub fn is_definite(self) -> bool {
                self != Self::$top
            }
        }
    };
}

three_valued_lattice!(Presence, Maybe);
three_valued_lattice!(Truth, Maybe);
three_valued_lattice!(Visibility, Maybe);

impl Presence {
    /// Whether membership is possible (present or maybe).
    #[must_use]
    pub fn may_be_present(self) -> bool {
        self != Self::Absent
    }
}

impl Truth {
    /// Whether the fact may hold (yes or maybe).
    #[must_use]
    pub fn may_be_true(self) -> bool {
        self != Self::No
    }
}

impl Visibility {
    /// Whether the object may be visible (visible or maybe).
    #[must_use]
    pub fn may_be_visible(self) -> bool {
        self != Self::Invisible
    }
}

/// An exactly known numeric literal.
///
/// Integers are kept exact as `i64` instead of being converted to `f64`, so
/// counts such as loop bounds and family sizes never lose precision.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NumLit {
    /// An exact integer.
    Int(i64),
    /// An exact float.
    Float(f64),
}

impl NumLit {
    /// The value as `f64` (large integers may lose precision, which is
    /// acceptable for bound comparisons only).
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounds comparison only; exactness is preserved in NumLit::Int"
    )]
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(value) => value as f64,
            Self::Float(value) => value,
        }
    }

    /// The larger of the two values, preserving exact representation.
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Self::Int(a.max(b)),
            _ => {
                if self.as_f64() >= other.as_f64() {
                    self
                } else {
                    other
                }
            }
        }
    }
}

impl std::ops::Add for NumLit {
    type Output = Self;

    /// Exact addition; integer overflow falls back to float.
    fn add(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_add(b)
                .map_or_else(|| Self::Float(self.as_f64() + other.as_f64()), Self::Int),
            _ => Self::Float(self.as_f64() + other.as_f64()),
        }
    }
}

impl std::ops::Mul for NumLit {
    type Output = Self;

    /// Exact multiplication; integer overflow falls back to float.
    fn mul(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_mul(b)
                .map_or_else(|| Self::Float(self.as_f64() * other.as_f64()), Self::Int),
            _ => Self::Float(self.as_f64() * other.as_f64()),
        }
    }
}

/// Symbolic numeric abstract value (DESIGN §4.1).
///
/// A value is `Exact(n)`, `Interval(lo, hi)` (with `None` meaning an open
/// bound), `Symbol(name)`, or `Unknown`. Unknown values are never assumed to
/// be `1` or `0`: any arithmetic involving `Symbol` or `Unknown` yields
/// `Unknown` rather than a fabricated exact value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Num {
    /// An exactly known value.
    Exact(NumLit),
    /// A closed or half-open interval; `None` is an open (infinite) bound.
    Interval {
        /// Inclusive lower bound; `None` means unbounded below.
        lo: Option<f64>,
        /// Inclusive upper bound; `None` means unbounded above.
        hi: Option<f64>,
    },
    /// A named symbolic quantity, e.g. `frames` or `family`.
    Symbol(String),
    /// No information. Never treated as `1` or `0`.
    Unknown,
}

fn add_bounds(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        _ => None,
    }
}

impl Num {
    /// An exact integer value.
    #[must_use]
    pub const fn int(value: i64) -> Self {
        Self::Exact(NumLit::Int(value))
    }

    /// An exact float value.
    #[must_use]
    pub const fn float(value: f64) -> Self {
        Self::Exact(NumLit::Float(value))
    }

    /// Numeric bounds when the value is exact or an interval; `None` for
    /// `Symbol` / `Unknown`.
    #[must_use]
    pub fn bounds(&self) -> Option<(Option<f64>, Option<f64>)> {
        match self {
            Self::Exact(lit) => {
                let value = lit.as_f64();
                Some((Some(value), Some(value)))
            }
            Self::Interval { lo, hi } => Some((*lo, *hi)),
            Self::Symbol(_) | Self::Unknown => None,
        }
    }

    /// Known inclusive lower bound, if any.
    #[must_use]
    pub fn lower_bound(&self) -> Option<f64> {
        self.bounds().and_then(|(lo, _)| lo)
    }

    /// Known inclusive upper bound, if any.
    #[must_use]
    pub fn upper_bound(&self) -> Option<f64> {
        self.bounds().and_then(|(_, hi)| hi)
    }

    /// Whether nothing at all is known about the value.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Abstract addition. Any `Symbol` / `Unknown` operand yields `Unknown`.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        if let (Self::Exact(a), Self::Exact(b)) = (self, other) {
            return Self::Exact(*a + *b);
        }
        match (self.bounds(), other.bounds()) {
            (Some((a_lo, a_hi)), Some((b_lo, b_hi))) => Self::Interval {
                lo: add_bounds(a_lo, b_lo),
                hi: add_bounds(a_hi, b_hi),
            },
            _ => Self::Unknown,
        }
    }

    /// Abstract multiplication. Any `Symbol` / `Unknown` operand yields
    /// `Unknown`; in particular `Unknown × Exact(0)` stays `Unknown` so an
    /// unknown factor is never silently dropped.
    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        if let (Self::Exact(a), Self::Exact(b)) = (self, other) {
            return Self::Exact(*a * *b);
        }
        let (Some((a_lo, a_hi)), Some((b_lo, b_hi))) = (self.bounds(), other.bounds()) else {
            return Self::Unknown;
        };
        if let (Some(a_lo), Some(a_hi), Some(b_lo), Some(b_hi)) = (a_lo, a_hi, b_lo, b_hi) {
            let products = [a_lo * b_lo, a_lo * b_hi, a_hi * b_lo, a_hi * b_hi];
            let lo = products.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = products.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            return Self::Interval {
                lo: Some(lo),
                hi: Some(hi),
            };
        }
        // With an open bound the product is only bounded when both factors
        // are known non-negative.
        match (a_lo, b_lo) {
            (Some(a_lo), Some(b_lo)) if a_lo >= 0.0 && b_lo >= 0.0 => Self::Interval {
                lo: Some(a_lo * b_lo),
                hi: match (a_hi, b_hi) {
                    (Some(a_hi), Some(b_hi)) => Some(a_hi * b_hi),
                    _ => None,
                },
            },
            _ => Self::Interval { lo: None, hi: None },
        }
    }

    /// Abstract maximum (used e.g. for `duration = max(run_time)`).
    ///
    /// The result is always at least as large as every known operand, so an
    /// unknown operand opens the upper bound instead of being ignored.
    #[must_use]
    pub fn max_with(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Exact(a), Self::Exact(b)) => Self::Exact(a.max(*b)),
            (Self::Symbol(a), Self::Symbol(b)) if a == b => self.clone(),
            (Self::Unknown | Self::Symbol(_), Self::Unknown | Self::Symbol(_)) => Self::Unknown,
            _ => {
                let (a_lo, a_hi) = self.bounds().unwrap_or((None, None));
                let (b_lo, b_hi) = other.bounds().unwrap_or((None, None));
                // max is >= each operand: lower bounds combine with max,
                // treating a missing bound as -inf.
                let lo = match (a_lo, b_lo) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (bound, None) | (None, bound) => bound,
                };
                // The upper bound only survives when both are known.
                let hi = match (a_hi, b_hi) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
                Self::Interval { lo, hi }
            }
        }
    }

    /// Least upper bound over both possible values (branch join).
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        if self == other {
            return self.clone();
        }
        match (self.bounds(), other.bounds()) {
            (Some((a_lo, a_hi)), Some((b_lo, b_hi))) => Self::Interval {
                lo: match (a_lo, b_lo) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    _ => None,
                },
                hi: match (a_hi, b_hi) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                },
            },
            _ => Self::Unknown,
        }
    }

    /// Widening for loop fixpoints: any bound that still moves between the
    /// previous (`self`) and next iterate opens to an infinite bound.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        if self == next {
            return self.clone();
        }
        match (self.bounds(), next.bounds()) {
            (Some((p_lo, p_hi)), Some((n_lo, n_hi))) => Self::Interval {
                lo: match (p_lo, n_lo) {
                    (Some(p), Some(n)) if n >= p => Some(p),
                    _ => None,
                },
                hi: match (p_hi, n_hi) {
                    (Some(p), Some(n)) if n <= p => Some(p),
                    _ => None,
                },
            },
            _ => Self::Unknown,
        }
    }
}

/// How many runtime objects one abstract [`ObjectId`] may stand for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Cardinality {
    /// Exactly one runtime object.
    Singleton,
    /// Definitely more than one (e.g. a loop allocation known to iterate).
    Many,
    /// One or more.
    MaybeMany,
}

impl Cardinality {
    /// Least upper bound: disagreement widens to [`Cardinality::MaybeMany`].
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        if self == other { self } else { Self::MaybeMany }
    }
}

/// A source location used as an allocation site or call site: file plus the
/// byte span of the originating expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AllocationSite {
    /// File the expression lives in.
    pub file: FileId,
    /// Byte offset of the start of the expression.
    pub start: u32,
    /// Byte offset of the end of the expression (exclusive).
    pub end: u32,
}

impl AllocationSite {
    /// Builds a site from a parser byte range.
    #[must_use]
    pub fn new(file: FileId, range: TextRange) -> Self {
        Self {
            file,
            start: range.start().into(),
            end: range.end().into(),
        }
    }
}

/// A call site in a bounded call string; same shape as an allocation site.
pub type CallSite = AllocationSite;

/// Bounded call-string context (k-CFA with `k =`
/// [`MAX_CALL_CONTEXT_DEPTH`]).
///
/// Calling the same helper from two different sites yields two different
/// contexts, so allocations inside the helper are not collapsed into one
/// identity (DESIGN §5.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallContextId {
    frames: Vec<CallSite>,
}

impl CallContextId {
    /// The empty (top-level) context.
    #[must_use]
    pub const fn empty() -> Self {
        Self { frames: Vec::new() }
    }

    /// The context reached by calling through `site`, keeping only the most
    /// recent [`MAX_CALL_CONTEXT_DEPTH`] frames.
    #[must_use]
    pub fn push(&self, site: CallSite) -> Self {
        let mut frames = self.frames.clone();
        frames.push(site);
        if frames.len() > MAX_CALL_CONTEXT_DEPTH {
            frames.remove(0);
        }
        Self { frames }
    }

    /// Call frames, oldest first.
    #[must_use]
    pub fn frames(&self) -> &[CallSite] {
        &self.frames
    }
}

/// Abstract object identity: `(allocation site, bounded call context,
/// cardinality)` (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjectId {
    /// Where the object was allocated.
    pub site: AllocationSite,
    /// Bounded call-string context of the allocation.
    pub context: CallContextId,
    /// How many runtime objects this ID may stand for.
    pub cardinality: Cardinality,
}

impl ObjectId {
    /// Builds an object ID.
    #[must_use]
    pub const fn new(
        site: AllocationSite,
        context: CallContextId,
        cardinality: Cardinality,
    ) -> Self {
        Self {
            site,
            context,
            cardinality,
        }
    }

    /// The same ID with a different cardinality (e.g. after loop widening).
    #[must_use]
    pub fn with_cardinality(&self, cardinality: Cardinality) -> Self {
        Self {
            site: self.site,
            context: self.context.clone(),
            cardinality,
        }
    }

    /// Whether the two IDs are provably the same runtime object.
    ///
    /// Only equal [`Cardinality::Singleton`] IDs qualify; two `Many` IDs from
    /// the same site are never assumed identical (DESIGN §5.5).
    #[must_use]
    pub fn definitely_same(&self, other: &Self) -> bool {
        self == other && self.cardinality == Cardinality::Singleton
    }

    /// Whether the two IDs may refer to the same runtime object.
    ///
    /// Different sites or contexts are distinct allocations
    /// ([`Truth::No`]); an equal non-singleton ID is only [`Truth::Maybe`].
    #[must_use]
    pub fn may_be_same(&self, other: &Self) -> Truth {
        if self.site != other.site || self.context != other.context {
            Truth::No
        } else if self.definitely_same(other) {
            Truth::Yes
        } else {
            Truth::Maybe
        }
    }
}

/// How a copy was produced (DESIGN §5.5: copies always get fresh IDs and
/// only a `copy_of` relation back to the original).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CopyKind {
    /// `mob.copy()`.
    Copy,
    /// `copy.deepcopy(mob)` or equivalent.
    DeepCopy,
    /// `mob.generate_target()`.
    GenerateTarget,
    /// The `starting_mobject` copy an `Animation.begin` takes.
    AnimationStartingCopy,
    /// A Transform-style target copy.
    AnimationTargetCopy,
}

/// Copy provenance edge: which original an object was copied from and how.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CopyOf {
    /// The object that was copied.
    pub original: ObjectId,
    /// The copy mechanism.
    pub kind: CopyKind,
}

/// A candidate set of fully qualified Manim class names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KindSet {
    /// The object's class is one of these candidates.
    Known(BTreeSet<String>),
    /// The class could not be narrowed down.
    #[default]
    Unknown,
}

impl KindSet {
    /// A single known candidate.
    #[must_use]
    pub fn single(name: impl Into<String>) -> Self {
        let mut set = BTreeSet::new();
        set.insert(name.into());
        Self::Known(set)
    }

    /// Whether `name` is a possible class of the object.
    #[must_use]
    pub fn may_be(&self, name: &str) -> bool {
        match self {
            Self::Known(candidates) => candidates.contains(name),
            Self::Unknown => true,
        }
    }

    /// Least upper bound: union of candidates; any unknown side stays
    /// unknown.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Known(a), Self::Known(b)) => Self::Known(a.union(b).cloned().collect()),
            _ => Self::Unknown,
        }
    }

    /// Widening: like [`KindSet::join`], but a candidate set larger than
    /// [`MAX_KIND_CANDIDATES`] collapses to [`KindSet::Unknown`].
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        match self.join(next) {
            Self::Known(candidates) if candidates.len() > MAX_KIND_CANDIDATES => Self::Unknown,
            joined => joined,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESENCES: [Presence; 3] = [Presence::Absent, Presence::Present, Presence::Maybe];
    const TRUTHS: [Truth; 3] = [Truth::No, Truth::Yes, Truth::Maybe];
    const VISIBILITIES: [Visibility; 3] = [
        Visibility::Visible,
        Visibility::Invisible,
        Visibility::Maybe,
    ];
    const CARDINALITIES: [Cardinality; 3] = [
        Cardinality::Singleton,
        Cardinality::Many,
        Cardinality::MaybeMany,
    ];

    macro_rules! lattice_laws {
        ($name:ident, $values:expr, $top:expr) => {
            #[test]
            fn $name() {
                for a in $values {
                    assert_eq!(a.join(a), a, "idempotent: {a:?}");
                    assert_eq!(a.join($top), $top, "top absorbs: {a:?}");
                    for b in $values {
                        assert_eq!(a.join(b), b.join(a), "commutative: {a:?} {b:?}");
                    }
                }
            }
        };
    }

    lattice_laws!(presence_lattice_laws, PRESENCES, Presence::Maybe);
    lattice_laws!(truth_lattice_laws, TRUTHS, Truth::Maybe);
    lattice_laws!(visibility_lattice_laws, VISIBILITIES, Visibility::Maybe);
    lattice_laws!(
        cardinality_lattice_laws,
        CARDINALITIES,
        Cardinality::MaybeMany
    );

    #[test]
    fn disagreement_joins_to_maybe() {
        assert_eq!(Presence::Absent.join(Presence::Present), Presence::Maybe);
        assert_eq!(Truth::No.join(Truth::Yes), Truth::Maybe);
        assert_eq!(
            Visibility::Visible.join(Visibility::Invisible),
            Visibility::Maybe
        );
        assert_eq!(
            Cardinality::Singleton.join(Cardinality::Many),
            Cardinality::MaybeMany
        );
    }

    fn num_samples() -> Vec<Num> {
        vec![
            Num::int(0),
            Num::int(3),
            Num::float(2.5),
            Num::Interval {
                lo: Some(1.0),
                hi: Some(4.0),
            },
            Num::Interval {
                lo: Some(0.0),
                hi: None,
            },
            Num::Symbol("frames".to_owned()),
            Num::Unknown,
        ]
    }

    #[test]
    fn num_join_lattice_laws() {
        for a in num_samples() {
            assert_eq!(a.join(&a), a, "idempotent: {a:?}");
            assert_eq!(a.join(&Num::Unknown), Num::Unknown, "top absorbs: {a:?}");
            for b in num_samples() {
                assert_eq!(a.join(&b), b.join(&a), "commutative: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn exact_integer_arithmetic_stays_exact() {
        assert_eq!(Num::int(2).add(&Num::int(3)), Num::int(5));
        assert_eq!(Num::int(4).mul(&Num::int(6)), Num::int(24));
        assert_eq!(Num::int(4).max_with(&Num::int(6)), Num::int(6));
        // Overflow falls back to float instead of wrapping.
        let big = Num::int(i64::MAX);
        assert!(matches!(
            big.add(&Num::int(1)),
            Num::Exact(NumLit::Float(_))
        ));
    }

    #[test]
    fn unknown_never_collapses_to_zero_or_one() {
        // DESIGN §4.1: never assume an unknown factor is 1 (or drop it as 0).
        assert_eq!(Num::Unknown.mul(&Num::int(1)), Num::Unknown);
        assert_eq!(Num::Unknown.mul(&Num::int(0)), Num::Unknown);
        assert_eq!(Num::Unknown.add(&Num::int(0)), Num::Unknown);
        assert_eq!(
            Num::Symbol("frames".to_owned()).mul(&Num::int(1)),
            Num::Unknown
        );
        assert_eq!(Num::int(1).mul(&Num::Unknown), Num::Unknown);
    }

    #[test]
    fn interval_arithmetic() {
        let a = Num::Interval {
            lo: Some(1.0),
            hi: Some(4.0),
        };
        let b = Num::Interval {
            lo: Some(2.0),
            hi: Some(5.0),
        };
        assert_eq!(
            a.add(&b),
            Num::Interval {
                lo: Some(3.0),
                hi: Some(9.0),
            }
        );
        assert_eq!(
            a.mul(&b),
            Num::Interval {
                lo: Some(2.0),
                hi: Some(20.0),
            }
        );
        // Open upper bound with non-negative factors keeps the lower bound.
        let open = Num::Interval {
            lo: Some(2.0),
            hi: None,
        };
        assert_eq!(
            a.mul(&open),
            Num::Interval {
                lo: Some(2.0),
                hi: None,
            }
        );
        // Sign-uncertain open products stay fully open.
        let negative = Num::Interval {
            lo: Some(-1.0),
            hi: Some(1.0),
        };
        assert_eq!(negative.mul(&open), Num::Interval { lo: None, hi: None });
    }

    #[test]
    fn max_with_unknown_keeps_known_lower_bound() {
        // duration = max(run_time): an unknown operand must not shrink the
        // known one, and must open the upper bound.
        assert_eq!(
            Num::int(3).max_with(&Num::Unknown),
            Num::Interval {
                lo: Some(3.0),
                hi: None,
            }
        );
        assert_eq!(Num::Unknown.max_with(&Num::Unknown), Num::Unknown);
    }

    #[test]
    fn join_of_exacts_is_hull() {
        assert_eq!(
            Num::int(2).join(&Num::int(5)),
            Num::Interval {
                lo: Some(2.0),
                hi: Some(5.0),
            }
        );
        assert_eq!(
            Num::Symbol("a".to_owned()).join(&Num::Symbol("a".to_owned())),
            Num::Symbol("a".to_owned())
        );
        assert_eq!(
            Num::Symbol("a".to_owned()).join(&Num::Symbol("b".to_owned())),
            Num::Unknown
        );
    }

    #[test]
    fn widen_opens_moving_bounds() {
        let prev = Num::Interval {
            lo: Some(0.0),
            hi: Some(10.0),
        };
        let grew = Num::Interval {
            lo: Some(0.0),
            hi: Some(12.0),
        };
        assert_eq!(
            prev.widen(&grew),
            Num::Interval {
                lo: Some(0.0),
                hi: None,
            }
        );
        let shrank_lo = Num::Interval {
            lo: Some(-1.0),
            hi: Some(10.0),
        };
        assert_eq!(
            prev.widen(&shrank_lo),
            Num::Interval {
                lo: None,
                hi: Some(10.0),
            }
        );
        assert_eq!(prev.widen(&prev), prev);
        assert_eq!(prev.widen(&Num::Unknown), Num::Unknown);
    }

    fn site(start: u32) -> AllocationSite {
        let mut sources = crate::source::SourceManager::new(".");
        let file = sources.load_bytes(std::path::Path::new("test.py"), b"pass\n");
        AllocationSite {
            file,
            start,
            end: start + 1,
        }
    }

    #[test]
    fn object_identity_rules() {
        let a = ObjectId::new(site(10), CallContextId::empty(), Cardinality::Singleton);
        let same = ObjectId::new(site(10), CallContextId::empty(), Cardinality::Singleton);
        assert!(a.definitely_same(&same));
        assert_eq!(a.may_be_same(&same), Truth::Yes);

        // Two Many ids from the same site are never assumed identical.
        let many = a.with_cardinality(Cardinality::Many);
        assert!(!many.definitely_same(&many.clone()));
        assert_eq!(many.may_be_same(&many.clone()), Truth::Maybe);

        // Different allocation sites are distinct objects.
        let other_site = ObjectId::new(site(20), CallContextId::empty(), Cardinality::Singleton);
        assert!(!a.definitely_same(&other_site));
        assert_eq!(a.may_be_same(&other_site), Truth::No);

        // Different call contexts are distinct objects.
        let context = CallContextId::empty().push(site(1));
        let other_context = ObjectId::new(site(10), context, Cardinality::Singleton);
        assert_eq!(a.may_be_same(&other_context), Truth::No);
    }

    #[test]
    fn call_context_is_bounded() {
        let mut context = CallContextId::empty();
        for start in 0..10 {
            context = context.push(site(start));
        }
        assert_eq!(context.frames().len(), MAX_CALL_CONTEXT_DEPTH);
        // The most recent frames are kept.
        assert_eq!(context.frames().last(), Some(&site(9)));
    }

    #[test]
    fn kind_set_join_and_widen() {
        let square = KindSet::single("manim.Square");
        let circle = KindSet::single("manim.Circle");
        let joined = square.join(&circle);
        assert!(joined.may_be("manim.Square"));
        assert!(joined.may_be("manim.Circle"));
        assert!(!joined.may_be("manim.Dot"));
        assert_eq!(square.join(&KindSet::Unknown), KindSet::Unknown);

        let mut big = BTreeSet::new();
        for index in 0..MAX_KIND_CANDIDATES {
            big.insert(format!("manim.K{index}"));
        }
        let big = KindSet::Known(big);
        assert_eq!(big.widen(&square), KindSet::Unknown);
        assert_eq!(square.widen(&circle), square.join(&circle));
    }
}
