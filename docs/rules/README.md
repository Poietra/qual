# Rule catalog

manim-lint reserves 92 rule IDs in four families. 52 are **implemented**;
40 are **reserved**. A reserved ID is honestly unimplemented: `manim-lint
rules` lists it as `reserved`, `manim-lint check` never registers it, and it
never fires. Each reserved rule waits on a named analysis capability that
the fact layers do not provide yet; where that capability is known it is
listed in the *Blocked on* column. The authoritative catalog definition is
[`DESIGN.md`](../../DESIGN.md) section 7.

Columns:

- **Default severity** — `error` / `warning` / `info`; what the rule reports
  unless configuration changes it.
- **Min confidence** — the lowest confidence (`certain` / `high` / `medium`
  / `low`) at which the rule is allowed to fire. Individual diagnostics can
  carry a higher confidence than this floor, never a lower one.
- Implemented IDs link to their full documentation, which is also available
  as `manim-lint explain <ID>`.

## MLC — lifecycle / correctness (25 implemented, 6 reserved)

| ID | Status | Default severity | Min confidence | Summary | Blocked on |
| --- | --- | --- | --- | --- | --- |
| [MLC000](MLC000.md) | implemented | error | certain | Python source cannot be parsed | |
| [MLC001](MLC001.md) | implemented | warning | certain | Invalid or unknown inline suppression comment | |
| [MLC101](MLC101.md) | implemented | error | certain | Scene.play requires at least one animation | |
| [MLC102](MLC102.md) | implemented | error | high | Scene.play argument cannot be converted to an Animation | |
| [MLC103](MLC103.md) | implemented | error | certain | Bound mobject method passed to Scene.play (pre-0.6 API) | |
| [MLC104](MLC104.md) | implemented | error | certain | Literal non-positive run_time or wait duration | |
| [MLC105](MLC105.md) | implemented | error | high | Updater callback cannot bind to Manim's positional invocation | |
| [MLC106](MLC106.md) | implemented | error | certain | wait() combines stop_condition with frozen_frame=True | |
| [MLC107](MLC107.md) | implemented | error | high | MoveToTarget without generate_target() on any path | |
| [MLC108](MLC108.md) | implemented | warning | high | Two animations in one play write the same channel of the same mobject | |
| [MLC109](MLC109.md) | implemented | error | certain | AnimationGroup / Succession constructed without animations | |
| [MLC110](MLC110.md) | implemented | error | certain | Mobject.add(self) or a proven parent cycle | |
| MLC111 | reserved | info | medium | Updater-bearing mobject leaves both the scene family and animation ownership for an interval | |
| MLC112 | reserved | warning | high | Default wait() justified only by a one-argument updater proven to read frame-varying state | callback body summaries |
| [MLC113](MLC113.md) | implemented | error | certain | Animation kwargs passed after a .animate method access | |
| MLC114 | reserved | error | high | Unsupported .animate method chain containing an override animation | |
| [MLC115](MLC115.md) | implemented | warning | high | Scene.remove(child) is undone by re-adding the surviving parent | |
| MLC116 | reserved | info | medium | Later operations confuse post-Transform source/target identity with scene membership | post-Transform identity facts |
| [MLC117](MLC117.md) | implemented | warning | high | Mobject changed between .animate builder creation and play | |
| MLC118 | reserved | info | medium | Normal animation targets a mobject with an active updater and the suspend/resume state divergence is provable | |
| [MLC119](MLC119.md) | implemented | error | high | Scene.replace(old, new) with old definitely not in the scene | |
| [MLC120](MLC120.md) | implemented | error | high | Restore without save_state() on any path | |
| [MLC121](MLC121.md) | implemented | error | high | Scene.play / wait / pause called from a per-frame callback | |
| [MLC122](MLC122.md) | implemented | error | high | ApplyMethod receives a method call result, not the bound method | |
| MLC123 | reserved | error | high | Inline ApplyFunction callback does not return a Mobject on every path | callback body summaries |
| [MLC124](MLC124.md) | implemented | warning | high | .animate chains a method that does not mutate the target | |
| [MLC125](MLC125.md) | implemented | warning | high | remove_updater callback identity matches no registered updater | |
| [MLC126](MLC126.md) | implemented | error | high | Family child is not a Mobject, or not a VMobject in a VGroup | |
| [MLC127](MLC127.md) | implemented | info | certain | Duplicate child passed twice in one add() / VGroup() / Group() | |
| [MLC128](MLC128.md) | implemented | error | high | Scene subclass \_\_init\_\_ never calls super().\_\_init\_\_() | |
| [MLC129](MLC129.md) | implemented | warning | medium | play(..., lag_ratio=...) does not stagger multiple animations | |

## MLR — rendering / renderer compatibility (14 implemented, 13 reserved)

| ID | Status | Default severity | Min confidence | Summary | Blocked on |
| --- | --- | --- | --- | --- | --- |
| [MLR101](MLR101.md) | implemented | error | high | Create/Write-style animations require a vectorized (VMobject) target | |
| [MLR102](MLR102.md) | implemented | warning | high | Bare `.animate` without a method call is played; the animation changes nothing | |
| [MLR103](MLR103.md) | implemented | error | high | Python escape in a non-raw Tex/MathTex literal corrupts a TeX command | |
| [MLR104](MLR104.md) | implemented | error | high | Literal asset path does not resolve in the render search path | |
| [MLR105](MLR105.md) | implemented | error | high | MarkupText literal contains provably invalid Pango markup | |
| [MLR106](MLR106.md) | implemented | error | high | Literal NaN/inf flows into mobject geometry | |
| MLR107 | reserved | warning | high | API / mobject combination unsupported or semantically different on the target renderer | |
| MLR108 | reserved | warning | high | Renderer-divergent path assumes a fixed object stays visible after un-fixing | |
| MLR109 | reserved | warning | medium | Updater read-after-write ordering makes a one-frame lag definite | |
| MLR110 | reserved | error | high | Literal TeX brace / environment mismatch proven by a conservative parser | |
| MLR111 | reserved | warning | high | Scene updater mutates a mobject that can escape Cairo's moving scope | |
| MLR112 | reserved | warning | high | Raw `.points` access hard-codes one renderer's point layout | |
| [MLR113](MLR113.md) | implemented | info | high | Transform source and target are the same object | |
| [MLR114](MLR114.md) | implemented | error | high | Literal points array is not N x 3 | |
| [MLR115](MLR115.md) | implemented | error | certain | Literal font_size is zero or negative | |
| MLR116 | reserved | error | high | add_line_to / close_path on an empty path, or an incomplete curve is drawn | interpreter tracking of point_count |
| [MLR117](MLR117.md) | implemented | error | high | register_font() context manager is called without `with` | |
| MLR118 | reserved | warning | high | Project SVG uses unsupported text/image/filter/mask/clipPath or an unresolved href | |
| MLR119 | reserved | error | high | MovingCameraScene / camera.frame path incompatible with an OpenGL-including profile set | |
| MLR120 | reserved | warning | high | Assumes the focal_distance setter is effective on an OpenGL profile | |
| MLR121 | reserved | warning | high | shift(OUT) / set_z used only for stacking order in a 2D Cairo scene | |
| MLR122 | reserved | warning | high | bring_to_front is defeated by a lower z_index | |
| MLR123 | reserved | error | high | 3D object / mesh added under a Cairo or unknown-renderer profile | |
| [MLR124](MLR124.md) | implemented | warning | high | Text() literal contains Pango markup that plain Text renders verbatim | |
| [MLR125](MLR125.md) | implemented | info | high | Bare Mobject() leaf added to the scene displays nothing | |
| [MLR126](MLR126.md) | implemented | error | high | Literal opacity outside [0, 1] or negative stroke width | |
| [MLR127](MLR127.md) | implemented | warning | high | Literal by-tex key cannot occur in the MathTex literal | |

## MLP — performance (6 implemented, 21 reserved)

| ID | Status | Default severity | Min confidence | Summary | Blocked on |
| --- | --- | --- | --- | --- | --- |
| [MLP201](MLP201.md) | implemented | warning | high | Expensive Text/TeX/SVG/Surface construction inside a per-frame callback | |
| MLP202 | reserved | warning | high | copy / deepcopy / become / align_data inside a hot context | family/points cardinality in CostFacts |
| MLP203 | reserved | info | high | Family / bounding / arc-length / point-proportion queries inside a hot context | family/points cardinality in CostFacts |
| [MLP204](MLP204.md) | implemented | warning | high | Scene graph grows with a fresh mobject every frame inside an updater | |
| [MLP205](MLP205.md) | implemented | warning | high | wait(frozen_frame=False) re-renders provably identical frames | |
| [MLP206](MLP206.md) | implemented | warning | certain | Literal play duration shorter than one frame is clamped | |
| MLP207 | reserved | info | medium | Transform between mobjects with very different topology / family / curve counts | family/points cardinality in CostFacts |
| MLP208 | reserved | info | high | Transform of a large Text / MathTex family | family/points cardinality in CostFacts |
| MLP209 | reserved | info | medium | Updater-bearing object early in Cairo's display order invalidates a large static suffix every frame | |
| MLP210 | reserved | info | medium | Many short sequential plays inside a fixed-count loop | |
| MLP211 | reserved | info | medium | Large per-frame list / ndarray / points allocation inside a hot context | family/points cardinality in CostFacts |
| MLP212 | reserved | info | medium | Long animation of a full-screen translucent object or layer | |
| MLP213 | reserved | info | medium | Calibrated workload / renderer mismatch (e.g. a large Cairo 3D Surface) | |
| MLP214 | reserved | info | high | Serial construction of distinct MathTex the local fork could precompile in parallel | local fork overlay profile |
| MLP215 | reserved | warning | high | Provably no-op updater widens the moving scope or keeps a wait dynamic | |
| MLP216 | reserved | info | medium | always_redraw rebuilds stable geometry that only receives affine / style mutation | family/points cardinality in CostFacts |
| MLP217 | reserved | warning | high | Variable-key use_svg_cache=True in a hot callback grows the global cache every frame | |
| MLP218 | reserved | info | high | Provably idempotent updater (no dt, no frame-varying reads) keeps a plain wait dynamic | callback body summaries |
| MLP219 | reserved | info | medium | Updater's estimated lifetime spans many later plays | |
| [MLP220](MLP220.md) | implemented | warning | high | TracedPath without dissipating_time accumulates over a long span | |
| MLP221 | reserved | warning | high | Excessive sample count proven from a literal ParametricFunction / plot step | tuple literal facts, curated ParametricFunction |
| MLP222 | reserved | warning | high | Moving ImageMobject, or a large image caught in a Cairo moving suffix | |
| MLP223 | reserved | info | high | Fully transparent stroke keeps a positive width that is processed every frame | |
| MLP224 | reserved | info | high | Repeated point_from_proportion / apply_function over long paths in a hot callback | |
| MLP225 | reserved | info | high | Cost-report-only explanation of features that block local-fork fast paths | local fork overlay profile |
| [MLP226](MLP226.md) | implemented | warning | high | Frame-varying Text/TeX cache key inside a per-frame callback | |
| MLP227 | reserved | warning | high | always_update_mobjects=True with no time-dependent updater, scene updater, stop condition, or camera motion | |

## MLD — determinism / portability (7 implemented, 0 reserved)

| ID | Status | Default severity | Min confidence | Summary | Blocked on |
| --- | --- | --- | --- | --- | --- |
| [MLD301](MLD301.md) | implemented | warning | high | Updater applies a fixed per-frame step without dt scaling (FPS-dependent motion) | |
| [MLD302](MLD302.md) | implemented | warning | medium | Unseeded global random state read inside a frame callback | |
| [MLD303](MLD303.md) | implemented | warning | high | Literal asset path syntax does not match the profile platform | |
| [MLD304](MLD304.md) | implemented | warning | medium | Renderer-divergent membership effect reached without a guard in a multi-renderer run | |
| [MLD305](MLD305.md) | implemented | warning | high | Asset path matches an existing file only case-insensitively on a case-sensitive target platform | |
| [MLD306](MLD306.md) | implemented | info | high | Literal font is not in the profile's allowed-fonts list | |
| [MLD307](MLD307.md) | implemented | warning | medium | Wall-clock, filesystem, or network call inside a frame callback | |
