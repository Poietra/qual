//! Curated static geometry knowledge (DESIGN §4.1 `P` / `C` dimensions).
//!
//! Every entry is proven against the sibling Manim checkout
//! (`/home/hosi/manim`, upstream 0.20 line); the source anchors are cited
//! per entry. The counts describe the object **as constructed with the
//! default point-generation path**; each entry therefore carries guards
//! (banned kwargs / maximum positional arity) so a constructor call that
//! *may* reach a count-affecting parameter yields no claim at all
//! (DESIGN §15: prefer Unknown over a guess).
//!
//! Background invariants from the Manim source:
//!
//! - Cairo `VMobject` stores exactly 4 points per cubic curve
//!   (`vectorized_mobject.py` `VMobject.__init__`
//!   `n_points_per_cubic_curve: int = 4`; `set_anchors_and_handles`
//!   dispatches 4 rows per curve). Hence `points = 4 × curves` for every
//!   entry below.
//! - `Mobject.reset_points` initializes `points = np.zeros((0, dim))`
//!   (`mobject.py`), so plain containers own zero points/curves.
//!
//! Content-dependent families (`Text`, `MarkupText`, `MathTex`,
//! `SingleStringMathTex`, `Tex`, `SVGMobject`, `ImageMobject`) are
//! **deliberately absent**: their family / point / curve counts depend on
//! the rendered content and stay `Num::Unknown` (DESIGN §4.3 Text/TeX/SVG
//! stage; never a fabricated number).

use crate::frontend::index::QualifiedCall;

/// Curve / point counts of one primitive kind as constructed by its
/// default point-generation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeometryEntry {
    /// Number of cubic Bézier curves (`C`).
    pub curves: i64,
    /// Number of stored points (`P = 4 × C` on the Cairo layout).
    pub points: i64,
    /// Keyword arguments that change the generated point count (or add
    /// submobjects). A call writing any of these — or carrying a `**kwargs`
    /// splat that could — voids the claim.
    pub banned_kwargs: &'static [&'static str],
    /// Highest positional arity that provably cannot reach a
    /// count-affecting parameter. More positional arguments (or a `*args`
    /// splat) void the claim.
    pub max_positional: usize,
}

/// Curated geometry of a canonical Manim class id, or `None` when the
/// class has no curated (or no content-independent) geometry.
#[must_use]
pub fn curated_geometry(canonical: &str) -> Option<GeometryEntry> {
    let entry = match canonical {
        // polygram.py `Polygram.__init__`: `start_new_path(first_vertex)` +
        // `add_points_as_corners([*vertices, first_vertex])` → one cubic
        // per side. `Square(side_length=...)` → `Rectangle` →
        // `Polygon(UR, UL, DL, DR)`: 4 sides. Positional arity: `Square`
        // takes only `side_length`.
        "manim.mobject.geometry.polygram.Square" => GeometryEntry {
            curves: 4,
            points: 16,
            // Rectangle kwargs reachable through `**kwargs`: grid steps
            // add `Line` submobjects (polygram.py `Rectangle.__init__`).
            banned_kwargs: &["grid_xstep", "grid_ystep"],
            max_positional: 1,
        },
        // polygram.py `Rectangle.__init__(color, height, width,
        // grid_xstep, grid_ystep, ...)`: grid steps are positional 4/5.
        "manim.mobject.geometry.polygram.Rectangle" => GeometryEntry {
            curves: 4,
            points: 16,
            banned_kwargs: &["grid_xstep", "grid_ystep"],
            max_positional: 3,
        },
        // arc.py `Arc.__init__(radius, start_angle, angle,
        // num_components=9, ...)`; `_set_pre_positioned_points` builds
        // `num_components` anchors → `set_anchors_and_handles(anchors[:-1],
        // ...)` → 8 cubic curves. `num_components` is positional 4.
        "manim.mobject.geometry.arc.Arc" => GeometryEntry {
            curves: 8,
            points: 32,
            banned_kwargs: &["num_components"],
            max_positional: 3,
        },
        // arc.py `Circle.__init__(radius, color, **kwargs)` →
        // `Arc(angle=TAU)` with the default 9 components → 8 curves.
        "manim.mobject.geometry.arc.Circle" => GeometryEntry {
            curves: 8,
            points: 32,
            banned_kwargs: &["num_components"],
            max_positional: 2,
        },
        // arc.py `Dot.__init__(point, radius, stroke_width, fill_opacity,
        // color, **kwargs)` → `Circle` defaults → 8 curves.
        "manim.mobject.geometry.arc.Dot" => GeometryEntry {
            curves: 8,
            points: 32,
            banned_kwargs: &["num_components"],
            max_positional: 5,
        },
        // line.py `Line.__init__(start, end, buff, path_arc, ...)`;
        // `set_points_by_ends` with the default `path_arc=0` runs
        // `set_points_as_corners([start, end])` → 1 cubic curve.
        // `path_arc` (positional 4) switches to `ArcBetweenPoints`.
        "manim.mobject.geometry.line.Line" => GeometryEntry {
            curves: 1,
            points: 4,
            banned_kwargs: &["path_arc"],
            max_positional: 3,
        },
        // Containers own no points of their own; their positional
        // arguments become submobjects and are counted structurally:
        // - vectorized_mobject.py `VGroup` / `VMobject`
        //   (`Mobject.reset_points` → zero-length points array; `VGroup`
        //   has no `generate_points`),
        // - mobject.py `Group` / `Mobject` (same zero-point init).
        "manim.mobject.types.vectorized_mobject.VGroup"
        | "manim.mobject.types.vectorized_mobject.VMobject"
        | "manim.mobject.mobject.Group"
        | "manim.mobject.mobject.Mobject" => GeometryEntry {
            curves: 0,
            points: 0,
            banned_kwargs: &[],
            max_positional: usize::MAX,
        },
        _ => return None,
    };
    Some(entry)
}

impl GeometryEntry {
    /// Whether a constructor call fact provably keeps the default
    /// point-generation path this entry describes.
    ///
    /// `None` for the call fact (site not analyzed by the frontend) fails
    /// closed only for entries with guards: guard-free container entries
    /// hold for every call shape.
    #[must_use]
    pub fn guards_hold(&self, call: Option<&QualifiedCall>) -> bool {
        if self.banned_kwargs.is_empty() && self.max_positional == usize::MAX {
            return true;
        }
        let Some(call) = call else {
            return false;
        };
        // A `**kwargs` splat may carry a banned keyword unseen.
        if call.has_star_star_kwargs && !self.banned_kwargs.is_empty() {
            return false;
        }
        if self
            .banned_kwargs
            .iter()
            .any(|name| call.keyword_names.contains(*name))
        {
            return false;
        }
        // A `*args` splat makes positional arity a lower bound only.
        if self.max_positional != usize::MAX
            && (call.has_star_args || call.positional_count > self.max_positional)
        {
            return false;
        }
        true
    }
}
