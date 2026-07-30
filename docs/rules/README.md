# Rule catalog

qual defines 92 rule IDs in four families, and all 92 are
**implemented**. This directory, together with the `RuleMetadata` in
`src/rules/registry.rs`, is the catalog: one page per rule ID with its fixed
meaning, default severity, and minimum confidence.

One implemented rule is opt-in: `MLP225` has `default_enabled: false` and
the capabilities `cost-report` + `local-fork-overlay` — its home is the
`cost` command's fork fast-path section under a local-fork knowledge
profile, and only an exact `--select MLP225` evaluates it in `check`.

Rule selection controls diagnostics only. The
[`StaticFacts v0`](../rfcs/0001-static-facts-v0.md) public semantic projection
emitted by `qual static-facts` requests all facts in its contract independently of `--select`,
`--ignore`, suppressions, confidence thresholds, and baselines. Renderer risk
facts in that projection are static evidence, not new diagnostics and not
permission to skip or fork rendering.

Columns:

- **Default severity** — `error` / `warning` / `info`; what the rule reports
  unless configuration changes it.
- **Min confidence** — the lowest confidence (`certain` / `high` / `medium`
  / `low`) at which the rule is allowed to fire. Individual diagnostics can
  carry a higher confidence than this floor, never a lower one.
- Implemented IDs link to their full documentation, which is also available
  as `qual explain <ID>`.

## MLC — lifecycle / correctness (31 implemented, 0 reserved)

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
| [MLC111](MLC111.md) | implemented | info | medium | Updater-bearing object is in neither the scene family nor an animation | |
| [MLC112](MLC112.md) | implemented | warning | high | Default wait() freezes while a frame-varying one-argument updater is active | |
| [MLC113](MLC113.md) | implemented | error | certain | Animation kwargs passed after a .animate method access | |
| [MLC114](MLC114.md) | implemented | error | high | Unsupported .animate method chain containing an override animation | |
| [MLC115](MLC115.md) | implemented | warning | high | Scene.remove(child) is undone by re-adding the surviving parent | |
| [MLC116](MLC116.md) | implemented | info | medium | Later operations confuse post-Transform source/target identity with scene membership | |
| [MLC117](MLC117.md) | implemented | warning | high | Mobject changed between .animate builder creation and play | |
| [MLC118](MLC118.md) | implemented | info | medium | Normal animation targets a mobject with an active updater and the suspend/resume state divergence is provable | |
| [MLC119](MLC119.md) | implemented | error | high | Scene.replace(old, new) with old definitely not in the scene | |
| [MLC120](MLC120.md) | implemented | error | high | Restore without save_state() on any path | |
| [MLC121](MLC121.md) | implemented | error | high | Scene.play / wait / pause called from a per-frame callback | |
| [MLC122](MLC122.md) | implemented | error | high | ApplyMethod receives a method call result, not the bound method | |
| [MLC123](MLC123.md) | implemented | error | high | ApplyFunction callback returns no mobject on some path | |
| [MLC124](MLC124.md) | implemented | warning | high | .animate chains a method that does not mutate the target | |
| [MLC125](MLC125.md) | implemented | warning | high | remove_updater callback identity matches no registered updater | |
| [MLC126](MLC126.md) | implemented | error | high | Family child is not a Mobject, or not a VMobject in a VGroup | |
| [MLC127](MLC127.md) | implemented | info | certain | Duplicate child passed twice in one add() / VGroup() / Group() | |
| [MLC128](MLC128.md) | implemented | error | high | Scene subclass \_\_init\_\_ never calls super().\_\_init\_\_() | |
| [MLC129](MLC129.md) | implemented | warning | medium | play(..., lag_ratio=...) does not stagger multiple animations | |

## MLR — rendering / renderer compatibility (27 implemented, 0 reserved)

| ID | Status | Default severity | Min confidence | Summary | Blocked on |
| --- | --- | --- | --- | --- | --- |
| [MLR101](MLR101.md) | implemented | error | high | Create/Write-style animations require a vectorized (VMobject) target | |
| [MLR102](MLR102.md) | implemented | warning | high | Bare `.animate` without a method call is played; the animation changes nothing | |
| [MLR103](MLR103.md) | implemented | error | high | Python escape in a non-raw Tex/MathTex literal corrupts a TeX command | |
| [MLR104](MLR104.md) | implemented | error | high | Literal asset path does not resolve in the render search path | |
| [MLR105](MLR105.md) | implemented | error | high | MarkupText literal contains provably invalid Pango markup | |
| [MLR106](MLR106.md) | implemented | error | high | Literal NaN/inf flows into mobject geometry | |
| [MLR107](MLR107.md) | implemented | warning | high | API/mobject is unsupported under a renderer this run targets | |
| [MLR108](MLR108.md) | implemented | warning | high | Object treated as still visible after a renderer-divergent remove_fixed_* call | |
| [MLR109](MLR109.md) | implemented | warning | medium | Updater read-after-write ordering makes a one-frame lag definite | |
| [MLR110](MLR110.md) | implemented | error | high | Literal TeX has a definite brace/environment imbalance | |
| [MLR111](MLR111.md) | implemented | warning | high | Scene updater mutates a mobject; the change may escape Cairo's moving scope | |
| [MLR112](MLR112.md) | implemented | warning | high | Raw `.points` access assumes one renderer's fixed points-per-curve layout | |
| [MLR113](MLR113.md) | implemented | info | high | Transform source and target are the same object | |
| [MLR114](MLR114.md) | implemented | error | high | Literal points array is not N x 3 | |
| [MLR115](MLR115.md) | implemented | error | certain | Literal font_size is zero or negative | |
| [MLR116](MLR116.md) | implemented | error | high | add_line_to/close_path on a provably empty path raises at render time | |
| [MLR117](MLR117.md) | implemented | error | high | register_font() context manager is called without `with` | |
| [MLR118](MLR118.md) | implemented | warning | high | Project SVG uses unsupported text/image/filter/mask/clipPath or an unresolved href | |
| [MLR119](MLR119.md) | implemented | error | high | MovingCameraScene is incompatible with the OpenGL renderer this run targets | |
| [MLR120](MLR120.md) | implemented | warning | high | focal_distance camera setter has no effect under the OpenGL renderer | |
| [MLR121](MLR121.md) | implemented | warning | high | shift(OUT)/set_z has no effect in a 2D Cairo scene; use set_z_index | |
| [MLR122](MLR122.md) | implemented | warning | high | bring_to_front is defeated by a lower z_index | |
| [MLR123](MLR123.md) | implemented | error | high | OpenGL-only mesh mobject is added to a scene under a Cairo-target profile | |
| [MLR124](MLR124.md) | implemented | warning | high | Text() literal contains Pango markup that plain Text renders verbatim | |
| [MLR125](MLR125.md) | implemented | info | high | Bare Mobject() leaf added to the scene displays nothing | |
| [MLR126](MLR126.md) | implemented | error | high | Literal opacity outside [0, 1] or negative stroke width | |
| [MLR127](MLR127.md) | implemented | warning | high | Literal by-tex key cannot occur in the MathTex literal | |

## MLP — performance (27 implemented, 0 reserved)

| ID | Status | Default severity | Min confidence | Summary | Blocked on |
| --- | --- | --- | --- | --- | --- |
| [MLP201](MLP201.md) | implemented | warning | high | Expensive Text/TeX/SVG/Surface construction inside a per-frame callback | |
| [MLP202](MLP202.md) | implemented | warning | high | copy/become/align of a confirmed-large mobject inside a per-frame callback | |
| [MLP203](MLP203.md) | implemented | info | high | Family-walk query of a confirmed-large mobject inside a per-frame callback | |
| [MLP204](MLP204.md) | implemented | warning | high | Scene graph grows with a fresh mobject every frame inside an updater | |
| [MLP205](MLP205.md) | implemented | warning | high | wait(frozen_frame=False) re-renders provably identical frames | |
| [MLP206](MLP206.md) | implemented | warning | certain | Literal play duration shorter than one frame is clamped | |
| [MLP207](MLP207.md) | implemented | info | medium | Transform whose confirmed family size or curve insertion is large at begin | |
| [MLP208](MLP208.md) | implemented | info | high | Transform of a large Text/MathTex family (copy + align + per-glyph interpolation) | |
| [MLP209](MLP209.md) | implemented | info | medium | Animated or updater-bearing object early in Cairo's display order re-rasterizes a large static suffix | |
| [MLP210](MLP210.md) | implemented | info | medium | Fixed-count loop issues many short sequential plays | |
| [MLP211](MLP211.md) | implemented | info | medium | Large per-frame allocation inside a per-frame callback | |
| [MLP212](MLP212.md) | implemented | info | medium | Long animation of a full-screen translucent object or layer | |
| [MLP213](MLP213.md) | implemented | info | medium | Calibrated workload / renderer mismatch (e.g. a large Cairo 3D Surface) | |
| [MLP214](MLP214.md) | implemented | info | high | Serial distinct TeX compile keys the local fork could precompile in parallel | |
| [MLP215](MLP215.md) | implemented | warning | high | Provably no-op updater widens dynamic waits or the play moving scope | |
| [MLP216](MLP216.md) | implemented | info | medium | always_redraw rebuilds identical curated topology every frame | |
| [MLP217](MLP217.md) | implemented | warning | high | Frame-varying use_svg_cache=True key in a hot callback grows the process-global cache every frame | |
| [MLP218](MLP218.md) | implemented | info | high | Provably frame-invariant updater is the only reason a wait renders dynamically | |
| [MLP219](MLP219.md) | implemented | info | medium | Updater's estimated lifetime spans many subsequent plays | |
| [MLP220](MLP220.md) | implemented | warning | high | TracedPath without dissipating_time accumulates over a long span | |
| [MLP221](MLP221.md) | implemented | warning | high | Literal t_range/x_range step proves an excessive sample count | |
| [MLP222](MLP222.md) | implemented | warning | high | ImageMobject re-rasterized every frame inside Cairo's moving suffix | |
| [MLP223](MLP223.md) | implemented | info | high | Fully transparent stroke keeps a positive width that is processed every frame | |
| [MLP224](MLP224.md) | implemented | info | high | point_from_proportion on a confirmed-long path inside a per-frame callback | |
| [MLP225](MLP225.md) | implemented (opt-in) | info | high | Cost-report-only explanation of features that block local-fork fast paths | |
| [MLP226](MLP226.md) | implemented | warning | high | Frame-varying Text/TeX cache key inside a per-frame callback | |
| [MLP227](MLP227.md) | implemented | warning | high | always_update_mobjects=True dynamicizes a provably static wait | |

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
