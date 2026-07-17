//! Symbolic cost dimensions and per-operation cost facts (DESIGN §4.1).
//!
//! The value forms (`Exact` / `Interval` / `Symbol` / `Unknown`) come from
//! [`crate::semantic::values::Num`]; invocation contexts, symbolic
//! multiplicities, and operation kinds come from
//! [`crate::semantic::events`]. This module only adds the DESIGN §4.1
//! dimension vocabulary and the pixel-bandwidth helpers. The §4.1 invariant
//! — an unknown value is never assumed to be `1` and never dropped from a
//! product — is inherited from [`Num`]'s arithmetic: every combinator here
//! goes through [`Num::mul`], which keeps `Unknown` tainted.

use std::collections::BTreeMap;

use crate::config::model::RenderProfile;
pub use crate::semantic::events::{InvocationContext, Multiplicity, OperationKind};
pub use crate::semantic::values::Num;

/// One symbolic cost dimension (DESIGN §4.1 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostDimension {
    /// `F`: rendered frame count interval of a play.
    Frames,
    /// `N_root`: top-level Scene root count.
    Roots,
    /// `N_family`: expanded family member count.
    Family,
    /// `N_moving_suffix`: suffix Cairo re-rasterizes every frame.
    MovingSuffix,
    /// `P`: point count.
    Points,
    /// `C`: Bézier curve count.
    Curves,
    /// `S`: subpath count.
    Subpaths,
    /// `A_px`: device pixel coverage / frame pixel area.
    PixelArea,
    /// `D`: draw call / shader wrapper count.
    DrawCalls,
    /// `L_tex`: distinct TeX compile job count.
    TexJobs,
    /// `K_resource`: distinct Text / TeX / SVG / Image cache keys over the
    /// frame sequence.
    ResourceKeys,
    /// `B_frame`: frame buffer bytes.
    FrameBufferBytes,
}

impl CostDimension {
    /// Every dimension in DESIGN §4.1 table order.
    pub const ALL: [Self; 12] = [
        Self::Frames,
        Self::Roots,
        Self::Family,
        Self::MovingSuffix,
        Self::Points,
        Self::Curves,
        Self::Subpaths,
        Self::PixelArea,
        Self::DrawCalls,
        Self::TexJobs,
        Self::ResourceKeys,
        Self::FrameBufferBytes,
    ];

    /// The DESIGN §4.1 symbol name.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Frames => "F",
            Self::Roots => "N_root",
            Self::Family => "N_family",
            Self::MovingSuffix => "N_moving_suffix",
            Self::Points => "P",
            Self::Curves => "C",
            Self::Subpaths => "S",
            Self::PixelArea => "A_px",
            Self::DrawCalls => "D",
            Self::TexJobs => "L_tex",
            Self::ResourceKeys => "K_resource",
            Self::FrameBufferBytes => "B_frame",
        }
    }
}

/// One per-operation symbolic cost fact (DESIGN §4.1-§4.2).
///
/// External compiler / process launches are not a frequency class: they are
/// [`OperationKind::ExternalProcess`] facts whose invocation carries the
/// `distinct_resource_keys` (and other) multipliers.
#[derive(Debug, Clone, PartialEq)]
pub struct CostFact {
    /// The operation performed.
    pub operation: OperationKind,
    /// Named size dimensions, each a symbolic value. Absent dimensions are
    /// unmeasured, never implicitly `1`.
    pub dimensions: BTreeMap<CostDimension, Num>,
    /// Lifecycle phase the operation is invoked from.
    pub context: InvocationContext,
    /// Ordered symbolic invocation-count factors
    /// (`loop_iterations × plays × frames × ... × distinct_resource_keys`).
    pub multiplicity: Multiplicity,
}

impl CostFact {
    /// A fact with no dimensions and the neutral multiplicity.
    #[must_use]
    pub fn new(operation: OperationKind, context: InvocationContext) -> Self {
        Self {
            operation,
            dimensions: BTreeMap::new(),
            context,
            multiplicity: Multiplicity::one(),
        }
    }

    /// Adds (or replaces) one dimension.
    #[must_use]
    pub fn with_dimension(mut self, dimension: CostDimension, value: Num) -> Self {
        self.dimensions.insert(dimension, value);
        self
    }

    /// Replaces the multiplicity.
    #[must_use]
    pub fn with_multiplicity(mut self, multiplicity: Multiplicity) -> Self {
        self.multiplicity = multiplicity;
        self
    }
}

/// `A_px` for full frames of the profile: `pixel_width × pixel_height`.
#[must_use]
pub fn pixel_area(profile: &RenderProfile) -> Num {
    Num::int(i64::from(profile.pixel_width)).mul(&Num::int(i64::from(profile.pixel_height)))
}

/// `B_frame = 4 × pixel_width × pixel_height` for a standard RGBA frame
/// (DESIGN §4.1).
///
/// Transparent output, other pixel formats, and readback paths with extra
/// copies need an `OutputState` correction in a later phase; this helper is
/// the standard-RGBA baseline only.
#[must_use]
pub fn frame_buffer_bytes(profile: &RenderProfile) -> Num {
    Num::int(4).mul(&pixel_area(profile))
}

/// The basic pixel-bandwidth multiplier
/// `pixel_frames = F × pixel_width × pixel_height` (DESIGN §4.1).
///
/// An unknown `frames` yields an unknown product — the frame factor is
/// never dropped or assumed to be `1`.
#[must_use]
pub fn pixel_frames(frames: &Num, profile: &RenderProfile) -> Num {
    frames.mul(&pixel_area(profile))
}
