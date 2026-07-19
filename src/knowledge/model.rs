//! Data model for versioned Manim knowledge profiles (DESIGN §5.4).
//!
//! A profile is a reviewed JSON document describing the semantics of one
//! Manim version: qualified symbols, their kinds, lifecycle effects
//! (introducer / remover / replacement / auto-add, DESIGN §3.2), fluent
//! `returns_self` facts, Scene membership effects, renderer compatibility
//! notes, and the star-import `exports` surface consumed by the frontend
//! name resolver.
//!
//! Two shapes exist:
//!
//! - [`ProfileDocument`]: the exact serde image of one JSON file, possibly
//!   an overlay (`base_profile` + `deleted_symbols`).
//! - [`KnowledgeProfile`]: a resolved profile after overlay application and
//!   validation; this is what the rest of the analyzer consumes.
//!
//! Overlay semantics are deliberately shallow: an overlay names its base by
//! `name` **and** `source_digest`, replaces whole symbol entries by
//! qualified key, and deletes via `deleted_symbols`. There is no recursive
//! deep merge of entry fields.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The knowledge profile schema version this build understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Errors from parsing, validating, or resolving knowledge profiles.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KnowledgeError {
    /// The requested profile name is not shipped with this build.
    #[error("unknown knowledge profile `{name}` (available: {available})")]
    UnknownProfile {
        /// The name that was requested.
        name: String,
        /// Comma-separated list of shipped profile names.
        available: String,
    },
    /// A profile document is not valid JSON for the expected schema.
    #[error("knowledge profile `{profile}` is not valid: {message}")]
    Parse {
        /// The file or profile name being parsed.
        profile: String,
        /// The underlying JSON error, stringified for cheap cloning.
        message: String,
    },
    /// A profile document parsed but violates a schema or overlay rule.
    #[error("knowledge profile `{profile}` failed validation: {message}")]
    Invalid {
        /// The profile name being validated.
        profile: String,
        /// Human-readable description of the violated rule.
        message: String,
    },
}

/// What a curated qualified symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    /// A `Scene` class (or subclass shipped by Manim).
    Scene,
    /// A plain `Mobject` class (not vectorized).
    Mobject,
    /// A `VMobject` class (vectorized, Bézier-backed).
    Vmobject,
    /// An `Animation` class.
    Animation,
    /// A camera class.
    Camera,
    /// A module-level function (e.g. `always_redraw`).
    Function,
    /// A method of a curated class (`...Class.method` qualified ID).
    Method,
    /// A module-level constant (e.g. `RIGHT`, `PI`).
    Constant,
}

/// The kind of target an animation constructor accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcceptedTarget {
    /// Any `Mobject` is accepted.
    Mobject,
    /// Only vectorized mobjects are accepted (e.g. `Create`, `Write`).
    #[serde(rename = "VMobject")]
    Vmobject,
}

/// Scene membership / ordering effect of a Scene API method.
///
/// These mirror DESIGN §3.4/§3.5: `Scene.add` re-adds existing objects at
/// the end (a draw-order effect), `Scene.remove` restructures the root list
/// without editing parents, and the 3D fixed-object helpers auto-add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneMembershipEffect {
    /// Adds mobjects to the Scene root list.
    Add,
    /// Removes mobjects from the Scene root list (restructuring only).
    Remove,
    /// Replaces one root-list mobject with another, preserving order.
    Replace,
    /// Re-adds mobjects at the end of the display list.
    ReorderToFront,
    /// Moves mobjects to the beginning of the display list.
    ReorderToBack,
    /// Empties the Scene root and foreground lists.
    Clear,
    /// Registers mobjects as foreground (also widens the moving scope).
    AddForeground,
    /// Unregisters foreground status; the mobject stays in the Scene.
    RemoveForeground,
    /// Fixes mobjects in frame (3D) and adds them to the Scene.
    AddFixedInFrame,
    /// Fixes mobject orientation (3D) and adds them to the Scene.
    AddFixedOrientation,
    /// Unfixes in-frame mobjects; membership effect is renderer-divergent.
    RemoveFixedInFrame,
    /// Unfixes orientation; membership effect is renderer-divergent.
    RemoveFixedOrientation,
    /// Runs the play lifecycle (auto-add, begin, cleanup; DESIGN §3.2).
    Play,
    /// Plays a `Wait` animation (static / dynamic per DESIGN §3.3).
    Wait,
    /// Registers a Scene-level updater callback.
    RegisterSceneUpdater,
    /// Removes a Scene-level updater callback.
    RemoveSceneUpdater,
}

/// Curated lifecycle / state effects of a symbol.
///
/// Every field is optional; `None` means "not curated / not applicable",
/// never "false". Consumers must not treat an absent fact as a negative
/// fact (DESIGN §15 invariant 2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolEffects {
    /// A mobject method has an ``@override_animate`` implementation, so
    /// `_AnimationBuilder` must build that custom Animation instead of a
    /// normal `_MethodAnimation`. Chaining any other method with it raises
    /// `NotImplementedError` (mobject.py `_AnimationBuilder.__getattr__`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animate_override: Option<bool>,
    /// Animation adds its target to the Scene during play setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introducer: Option<bool>,
    /// Animation removes its target from the Scene during play cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remover: Option<bool>,
    /// Cleanup replaces the source with the target in the Scene
    /// (`ReplacementTransform` and the `TransformMatching*` family).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<bool>,
    /// `begin()` suspends updaters on the live target and `finish()`
    /// resumes them (the `suspend_mobject_updating` default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspends_updaters: Option<bool>,
    /// Auto-adds mobjects to the Scene as a side effect (`Scene.play` on
    /// non-introducer targets, the 3D fixed-object helpers, foreground
    /// registration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_adds: Option<bool>,
    /// Requires `mobject.generate_target()` on every path (`MoveToTarget`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_target: Option<bool>,
    /// Requires `mobject.save_state()` on every path (`Restore`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_saved_state: Option<bool>,
    /// Produces a `target` copy on the mobject (`generate_target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generates_target: Option<bool>,
    /// Stores a `saved_state` copy on the mobject (`save_state`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saves_state: Option<bool>,
    /// Scene membership / ordering effect (Scene API methods only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_membership: Option<SceneMembershipEffect>,
    /// `Scene.add` semantics: an already-present object is first removed
    /// and re-inserted at the end, changing draw order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reorders_existing_to_front: Option<bool>,
    /// The membership effect differs between Cairo and OpenGL (e.g.
    /// `ThreeDScene.remove_fixed_in_frame_mobjects`, DESIGN §3.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer_divergent_membership: Option<bool>,
    /// Registers an updater callback (on a mobject or the Scene).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registers_updater: Option<bool>,
    /// Removes updater callback(s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removes_updater: Option<bool>,
    /// The attached callback runs every rendered frame (hot context entry
    /// point for the cost model, DESIGN §4.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_frame_callback: Option<bool>,
}

/// One parameter of a curated signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamFact {
    /// Parameter name as it appears in the Manim source.
    pub name: String,
    /// Whether the parameter has no default (must be supplied).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

/// Lightweight signature facts, curated only where a rule needs them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureFacts {
    /// Curated (subset of) named parameters, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamFact>,
    /// The callable accepts leading variadic positional args (`*mobjects`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_var_args: bool,
}

/// Renderer compatibility notes for a symbol.
///
/// `None` means "not curated"; `Some(false)` is a positive incompatibility
/// fact for the given renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererCompat {
    /// Works under the Cairo renderer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cairo: Option<bool>,
    /// Works under the OpenGL renderer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opengl: Option<bool>,
    /// Positive renderer-requirement fact: the class is an OpenGL mesh /
    /// `Object3D`-style scene object that only the OpenGL renderer can
    /// display (`MLR123`).
    ///
    /// These classes (`renderer/shader.py Object3D` / `Mesh`, the
    /// `OpenGLMobject`-rooted `OpenGLSurface` family) are not Cairo
    /// `Mobject`s: `Scene.add` diverts `Object3D` instances into
    /// `Scene.meshes` only under `RendererType.OPENGL` (`scene.py`), and
    /// under Cairo the object lands in `Scene.mobjects` where
    /// `Camera.type_or_raise` raises `TypeError` at the first captured
    /// frame (`camera/camera.py` — none of the `display_funcs` types
    /// match).
    ///
    /// This deliberately does **not** imply `cairo: Some(false)`:
    /// construction itself is renderer-independent, and the `cairo`
    /// flag would make the generic call-site rule `MLR107` flag every
    /// constructor call. The failure is a display-time contract, so the
    /// scene-membership rule `MLR123` owns it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opengl_only_mesh: Option<bool>,
    /// Free-form reviewer note about the divergence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One curated symbol entry, keyed by canonical qualified ID
/// (e.g. `manim.animation.creation.Create`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolEntry {
    /// What the symbol is.
    pub kind: SymbolKind,
    /// Canonical IDs of the direct base classes (informational; a base may
    /// itself be curated, in which case hierarchy queries can chain).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<String>,
    /// For animations: the target kind the constructor accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_target: Option<AcceptedTarget>,
    /// Curated lifecycle / membership effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<SymbolEffects>,
    /// For fluent mutators: the method returns `self` (identity
    /// propagation, DESIGN §5.5). `Some(false)` is a positive fact that a
    /// *new* object (or a non-mobject value) is returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns_self: Option<bool>,
    /// Curated signature facts where a rule needs them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SignatureFacts>,
    /// Renderer compatibility notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<RendererCompat>,
    /// Free-form reviewer note (shown in explanations, never parsed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One condition that gates a fork fast path back onto the canonical
/// (serial / legacy) render path.
///
/// The vocabulary is shared by every [`ForkCapabilities`] blocker list; each
/// capability lists only the subset its gate actually checks. Values are
/// curated from the fork source (the gate functions cited in each
/// capability's `note`), never invented from rule prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkBlocker {
    /// The parent partial-movie encoder has already opened (writes leave
    /// native encoder state that makes a later `os.fork` unsafe).
    ParentEncoderOpened,
    /// `config.transparent` output.
    TransparentOutput,
    /// Output format is not plain `.mp4`.
    NonMp4Output,
    /// `video_encoder` is not exactly `libx264` (including `auto`).
    NonLibx264Encoder,
    /// `config.dry_run`.
    DryRun,
    /// `config.save_last_frame`.
    SaveLastFrame,
    /// `config.save_sections`.
    SaveSections,
    /// `config.log_to_file`.
    LogToFile,
    /// The file writer has sounds registered (`Scene.add_sound`).
    SoundAdded,
    /// Scene-level updaters are registered (`Scene.add_updater`).
    SceneUpdaters,
    /// Foreground mobjects are registered.
    ForegroundMobjects,
    /// OpenGL meshes are attached to the Scene.
    Meshes,
    /// `Scene.always_update_mobjects` is set.
    AlwaysUpdateMobjects,
    /// The play has a `stop_condition` callback.
    StopCondition,
    /// Interactive / preview mode is active.
    InteractiveMode,
    /// The renderer or camera is a subclass, not the exact Cairo classes.
    SubclassedRendererOrCamera,
    /// The Scene type overrides `__getattribute__` / `__setattr__`.
    CustomSceneAttributeHooks,
    /// Audited renderer / writer / scene / camera methods, module functions
    /// or signal handlers were monkeypatched after import.
    MonkeypatchedInternals,
    /// A family member's type overrides audited Mobject/VMobject hooks.
    UntrustedMobjectHooks,
    /// An animation type outside the curated allowlist.
    UnsupportedAnimationType,
    /// An allowlisted animation instance with an overridden lifecycle
    /// (`begin` / `finish` / `clean_up_from_scene` / `_setup_scene`, a
    /// custom `_on_finish`, or non-standard lifecycle attributes).
    UntrustedAnimationLifecycle,
    /// A rate function other than the audited defaults.
    CustomRateFunc,
    /// A `path_func` other than the straight path.
    NonStraightPathFunc,
    /// In-flight TeX compilation futures at the fork boundary.
    InFlightTexWorkers,
    /// Any live thread besides the main thread at the fork boundary.
    ExtraLiveThreads,
    /// A frozen static `Wait` play (already rendered optimally; a layer
    /// plan would only warm updater state).
    FrozenStaticWait,
    /// Camera background image set or background opacity below 1.0.
    NonOpaqueBackground,
    /// Any updater anywhere in the Scene's mobject families.
    UpdaterBearingFamily,
    /// An animation with `lag_ratio != 0`.
    NonzeroLagRatio,
    /// An animation with `reverse_rate_function` set.
    ReversedRateFunction,
    /// Two animations of one play share live family members.
    OverlappingAnimationFamilies,
    /// Animation caching is enabled (`disable_caching` is not set).
    CachingEnabled,
}

impl ForkBlocker {
    /// Stable `snake_case` name, identical to the JSON encoding (for cost
    /// reports and diagnostics).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParentEncoderOpened => "parent_encoder_opened",
            Self::TransparentOutput => "transparent_output",
            Self::NonMp4Output => "non_mp4_output",
            Self::NonLibx264Encoder => "non_libx264_encoder",
            Self::DryRun => "dry_run",
            Self::SaveLastFrame => "save_last_frame",
            Self::SaveSections => "save_sections",
            Self::LogToFile => "log_to_file",
            Self::SoundAdded => "sound_added",
            Self::SceneUpdaters => "scene_updaters",
            Self::ForegroundMobjects => "foreground_mobjects",
            Self::Meshes => "meshes",
            Self::AlwaysUpdateMobjects => "always_update_mobjects",
            Self::StopCondition => "stop_condition",
            Self::InteractiveMode => "interactive_mode",
            Self::SubclassedRendererOrCamera => "subclassed_renderer_or_camera",
            Self::CustomSceneAttributeHooks => "custom_scene_attribute_hooks",
            Self::MonkeypatchedInternals => "monkeypatched_internals",
            Self::UntrustedMobjectHooks => "untrusted_mobject_hooks",
            Self::UnsupportedAnimationType => "unsupported_animation_type",
            Self::UntrustedAnimationLifecycle => "untrusted_animation_lifecycle",
            Self::CustomRateFunc => "custom_rate_func",
            Self::NonStraightPathFunc => "non_straight_path_func",
            Self::InFlightTexWorkers => "in_flight_tex_workers",
            Self::ExtraLiveThreads => "extra_live_threads",
            Self::FrozenStaticWait => "frozen_static_wait",
            Self::NonOpaqueBackground => "non_opaque_background",
            Self::UpdaterBearingFamily => "updater_bearing_family",
            Self::NonzeroLagRatio => "nonzero_lag_ratio",
            Self::ReversedRateFunction => "reversed_rate_function",
            Self::OverlappingAnimationFamilies => "overlapping_animation_families",
            Self::CachingEnabled => "caching_enabled",
        }
    }
}

/// Parallel TeX compilation capability (`MLP214`).
///
/// `Some(true)` fields are positive curated facts; `None` means "not
/// curated", never "false" (DESIGN §15 invariant 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TexParallelCompile {
    /// Qualified IDs of the submit-side API. Every entry must be a curated
    /// symbol of the resolved profile (validated), so precompile advice can
    /// never name an API the selected profile does not have.
    pub entry_points: Vec<String>,
    /// Submissions for the same compile key join one in-flight job, and a
    /// later synchronous construction joins it too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_key_coalesced: Option<bool>,
    /// An already-cached SVG resolves immediately without a compile job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_short_circuits: Option<bool>,
    /// In-flight TeX futures force the Cairo fork pipeline into serial
    /// fallback for that play (the idle pool is joined at fork boundaries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight_blocks_cairo_fork: Option<bool>,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Cairo fork-per-play pipeline gate (`MLP225` cost reports).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CairoForkGate {
    /// Manim config option requesting the pipeline (`snake_case`, as in
    /// `default.cfg`).
    pub config_key: String,
    /// Smallest enabled value; below it the pipeline is *unrequested*, not
    /// blocked (workers 0 must never be reported as a fork loss).
    pub min_workers: u32,
    /// The pipeline exists only where `os.fork` exists (Linux).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_only: Option<bool>,
    /// Renderer-wide monotonic disabling: once a written play opens the
    /// parent encoder, every later play — eligible or not — renders
    /// serially. Per-play eligibility must not be modeled as independent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monotonic_disable: Option<bool>,
    /// Exact animation types with a built-in fork lifecycle model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animation_allowlist: Vec<String>,
    /// Composition containers whose children are audited recursively.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composition_allowlist: Vec<String>,
    /// Rate functions accepted by identity for the general animation scope
    /// (some allowlisted animations additionally accept their own exact
    /// default, see `note`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_rate_functions: Vec<String>,
    /// Conditions that force serial fallback for a requested pipeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<ForkBlocker>,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Cairo static-layer retention (`MLP209` severity, `MLP225`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CairoStaticLayers {
    /// Manim config option enabling the layer plan.
    pub config_key: String,
    /// Plays shorter than this many frames stay on the legacy path (the
    /// base and transparent-run rasters cannot amortize).
    pub min_play_frames: u32,
    /// Static runs *after* (above) moving objects are retained as cached
    /// transparent layers in a z-ordered run plan — a dynamic object early
    /// in the display order no longer forces the static suffix to re-raster
    /// every frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retains_trailing_static_runs: Option<bool>,
    /// Conditions that fall back to the legacy static-image path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<ForkBlocker>,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Cairo packed (bulk) interpolation fast path (`MLP225`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CairoBulkInterpolation {
    /// Minimum frames in the play's time progression.
    pub min_frames: u32,
    /// Minimum point-bearing family members per play.
    pub min_family_count: u32,
    /// Required `members × remaining frames` product before recipe
    /// construction amortizes (the effective member floor is
    /// `max(min_family_count, ceil(min_amortization / (frames - 2)))`).
    pub min_amortization: u32,
    /// Exact animation types the packed recipe can drive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animation_allowlist: Vec<String>,
    /// Rate functions accepted by identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_rate_functions: Vec<String>,
    /// Conditions that keep the play on canonical per-member interpolation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<ForkBlocker>,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Process-global SVG mobject cache semantics (`MLP217`'s gate: the rule is
/// enabled only where the profile declares this shared-cache behavior).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SvgCacheFacts {
    /// One module-global map shared by every SVG-backed mobject in the
    /// process (`Text`, `MathTex`, `SVGMobject`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_global: Option<bool>,
    /// Components of the cache key, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyed_by: Vec<String>,
    /// Entries are never evicted: frame-varying keys in hot callbacks grow
    /// the cache for the life of the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unbounded: Option<bool>,
    /// A cache hit still deep-copies the stored mobject (and insertion
    /// stores a deep copy), so per-construction cost never reaches zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copies_on_hit: Option<bool>,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Continuous partial-movie stream (`MLP210`'s `OutputState` gate: per-play
/// partial stream boundaries disappear when the writer merges plays).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousMovieStream {
    /// Uncached Cairo plays are merged into one encoder stream, removing
    /// per-play partial-stream open/close boundaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merges_partial_movie_files: Option<bool>,
    /// The merge only activates with `disable_caching = True`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_disable_caching: Option<bool>,
    /// Conditions that keep per-play partial movie files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<ForkBlocker>,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One fork-added Manim config option (`snake_case` `default.cfg` name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkConfigKey {
    /// Option name as it appears in the fork's `default.cfg`.
    pub name: String,
    /// Shipped default value, verbatim.
    pub default: String,
    /// Source citation and reviewer notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Curated fork fast-path capabilities (DESIGN §7.3).
///
/// Present only on the local fork overlay; the upstream profile lacks the
/// field entirely, which keeps every fork-gated rule interpretation inert
/// under `upstream_0_20` (fork-gated advice must never name APIs or config
/// semantics the selected profile does not declare). Each sub-capability is
/// optional; in an overlay the whole block replaces the base's block — there
/// is no per-capability merge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkCapabilities {
    /// Submit-all/collect TeX compilation (`MLP214`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tex_parallel_compile: Option<TexParallelCompile>,
    /// Fork-per-play Cairo pipeline gate (`MLP225`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cairo_fork_gate: Option<CairoForkGate>,
    /// Static-layer retention (`MLP209` / `MLP225`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cairo_static_layers: Option<CairoStaticLayers>,
    /// Packed interpolation fast path (`MLP225`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cairo_bulk_interpolation: Option<CairoBulkInterpolation>,
    /// Process-global SVG cache semantics (`MLP217`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg_cache: Option<SvgCacheFacts>,
    /// Continuous partial-movie stream (`MLP210` `OutputState`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuous_movie_stream: Option<ContinuousMovieStream>,
    /// Fork-added Manim config options (config display support: local-only
    /// keys are inert, not rejected, under profiles lacking them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_keys: Vec<ForkConfigKey>,
    /// Source citation and reviewer notes for the whole block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Reference from an overlay to its base profile.
///
/// Both fields must match the shipped base exactly; a digest mismatch is a
/// validation error so a stale overlay can never be applied silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseProfileRef {
    /// `name` of the base profile document.
    pub name: String,
    /// `source_digest` of the base profile document.
    pub source_digest: String,
}

/// The exact serde image of one profile JSON file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    /// Must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Profile name; this is the `knowledge-profile` config value.
    pub name: String,
    /// Compatible Manim version range (informational, e.g. `>=0.20,<0.21`).
    pub manim_version: String,
    /// `sha256:<64 hex>` digest of the Manim source this profile was
    /// curated against (see `profiles/README.md` for what it covers).
    pub source_digest: String,
    /// Present only on overlay profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_profile: Option<BaseProfileRef>,
    /// Curated fork fast-path capabilities. In an overlay, a present block
    /// replaces the base's block wholesale (no per-capability merge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_capabilities: Option<ForkCapabilities>,
    /// Curated symbols keyed by canonical qualified ID. In an overlay,
    /// each entry replaces the base entry with the same key wholesale.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symbols: BTreeMap<String, SymbolEntry>,
    /// Overlay-only: base symbol IDs removed from the resolved profile.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub deleted_symbols: BTreeSet<String>,
    /// Overlay-only: base export names removed from the resolved profile.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub deleted_exports: BTreeSet<String>,
    /// Star-import surface: public name available via `from manim import *`
    /// (e.g. `Create`) → canonical symbol ID. In an overlay, entries
    /// replace base entries with the same name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exports: BTreeMap<String, String>,
}

impl ProfileDocument {
    /// Parses one profile document from JSON text.
    ///
    /// `origin` names the file or profile for error messages. Structural
    /// (per-document) validation runs immediately; overlay rules are
    /// checked later during resolution.
    pub fn from_json(origin: &str, json: &str) -> Result<Self, KnowledgeError> {
        let document: Self = serde_json::from_str(json).map_err(|error| KnowledgeError::Parse {
            profile: origin.to_owned(),
            message: error.to_string(),
        })?;
        document.validate_structure()?;
        Ok(document)
    }

    /// Validates rules that hold for any single document.
    fn validate_structure(&self) -> Result<(), KnowledgeError> {
        let invalid = |message: String| KnowledgeError::Invalid {
            profile: self.name.clone(),
            message,
        };
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported schema_version {} (supported: {SCHEMA_VERSION})",
                self.schema_version
            )));
        }
        if self.name.is_empty() {
            return Err(invalid("profile name must not be empty".to_owned()));
        }
        validate_digest(&self.name, "source_digest", &self.source_digest)?;
        if let Some(base) = &self.base_profile {
            validate_digest(
                &self.name,
                "base_profile.source_digest",
                &base.source_digest,
            )?;
            if let Some(key) = self
                .deleted_symbols
                .iter()
                .find(|key| self.symbols.contains_key(*key))
            {
                return Err(invalid(format!(
                    "symbol `{key}` appears in both `symbols` and `deleted_symbols`"
                )));
            }
        } else {
            if !self.deleted_symbols.is_empty() {
                return Err(invalid(
                    "`deleted_symbols` requires a `base_profile`".to_owned(),
                ));
            }
            if !self.deleted_exports.is_empty() {
                return Err(invalid(
                    "`deleted_exports` requires a `base_profile`".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Resolves a base (non-overlay) document into a usable profile.
    ///
    /// Fails if the document declares a `base_profile` (use
    /// [`apply_overlay`] instead) or if an export points at a symbol that
    /// is not in the table.
    pub fn into_resolved(self) -> Result<KnowledgeProfile, KnowledgeError> {
        if let Some(base) = &self.base_profile {
            return Err(KnowledgeError::Invalid {
                profile: self.name.clone(),
                message: format!(
                    "profile declares base_profile `{}`; overlays must be resolved against their base",
                    base.name
                ),
            });
        }
        let resolved = KnowledgeProfile {
            schema_version: self.schema_version,
            name: self.name,
            manim_version: self.manim_version,
            source_digest: self.source_digest,
            base_profile: None,
            fork_capabilities: self.fork_capabilities,
            symbols: self.symbols,
            exports: self.exports,
        };
        resolved.validate_exports()?;
        resolved.validate_fork_capabilities()?;
        Ok(resolved)
    }
}

fn validate_digest(profile: &str, field: &str, digest: &str) -> Result<(), KnowledgeError> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| KnowledgeError::Invalid {
            profile: profile.to_owned(),
            message: format!("`{field}` must start with `sha256:`"),
        })?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(KnowledgeError::Invalid {
            profile: profile.to_owned(),
            message: format!("`{field}` must be `sha256:` followed by 64 hex digits"),
        });
    }
    Ok(())
}

/// A resolved, validated knowledge profile ready for the analyzer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnowledgeProfile {
    /// Schema version of the source document(s).
    pub schema_version: u32,
    /// Profile name (`knowledge-profile` config value).
    pub name: String,
    /// Compatible Manim version range.
    pub manim_version: String,
    /// Source digest of the (overlay) document that produced this profile.
    pub source_digest: String,
    /// Name of the base profile if this was resolved from an overlay.
    pub base_profile: Option<String>,
    /// Curated fork fast-path capabilities (`None` on upstream profiles:
    /// fork-gated rule interpretation stays inert there).
    pub fork_capabilities: Option<ForkCapabilities>,
    /// Curated symbols keyed by canonical qualified ID.
    pub symbols: BTreeMap<String, SymbolEntry>,
    /// Star-import name → canonical symbol ID.
    pub exports: BTreeMap<String, String>,
}

impl KnowledgeProfile {
    /// Looks up a symbol by canonical qualified ID.
    #[must_use]
    pub fn symbol(&self, id: &str) -> Option<&SymbolEntry> {
        self.symbols.get(id)
    }

    /// Resolves a star-import name (e.g. `Create`) to its canonical ID and
    /// entry. This is the bridge consumed by the frontend name resolver.
    #[must_use]
    pub fn resolve_export(&self, name: &str) -> Option<(&str, &SymbolEntry)> {
        let id = self.exports.get(name)?;
        let entry = self.symbols.get(id)?;
        Some((id.as_str(), entry))
    }

    /// Curated fork fast-path capabilities, if this profile declares any.
    ///
    /// `None` on upstream profiles: every fork-gated rule interpretation
    /// (MLP214 / MLP217 gating / MLP225, `cairo_fork_workers` /
    /// `cairo_static_layers` fast-path semantics) must stay inert then.
    #[must_use]
    pub fn fork_capabilities(&self) -> Option<&ForkCapabilities> {
        self.fork_capabilities.as_ref()
    }

    /// Parallel TeX compilation capability (`MLP214`), if declared.
    #[must_use]
    pub fn tex_parallel_compile(&self) -> Option<&TexParallelCompile> {
        self.fork_capabilities()?.tex_parallel_compile.as_ref()
    }

    /// Cairo fork-per-play gate (`MLP225` cost reports), if declared.
    #[must_use]
    pub fn cairo_fork_gate(&self) -> Option<&CairoForkGate> {
        self.fork_capabilities()?.cairo_fork_gate.as_ref()
    }

    /// Cairo static-layer retention facts, if declared.
    #[must_use]
    pub fn cairo_static_layers(&self) -> Option<&CairoStaticLayers> {
        self.fork_capabilities()?.cairo_static_layers.as_ref()
    }

    /// Cairo packed interpolation fast-path facts, if declared.
    #[must_use]
    pub fn cairo_bulk_interpolation(&self) -> Option<&CairoBulkInterpolation> {
        self.fork_capabilities()?.cairo_bulk_interpolation.as_ref()
    }

    /// Process-global SVG cache semantics (`MLP217`'s gate), if declared.
    #[must_use]
    pub fn svg_cache(&self) -> Option<&SvgCacheFacts> {
        self.fork_capabilities()?.svg_cache.as_ref()
    }

    /// Continuous partial-movie stream facts (`MLP210`), if declared.
    #[must_use]
    pub fn continuous_movie_stream(&self) -> Option<&ContinuousMovieStream> {
        self.fork_capabilities()?.continuous_movie_stream.as_ref()
    }

    /// Checks that every export points at a curated symbol.
    fn validate_exports(&self) -> Result<(), KnowledgeError> {
        for (name, id) in &self.exports {
            if !self.symbols.contains_key(id) {
                return Err(KnowledgeError::Invalid {
                    profile: self.name.clone(),
                    message: format!("export `{name}` points at unknown symbol `{id}`"),
                });
            }
        }
        Ok(())
    }

    /// Checks the internal consistency of a declared capability block.
    ///
    /// A `tex_parallel_compile` declaration must name at least one entry
    /// point, and every entry point must be a curated symbol of the resolved
    /// profile — precompile advice may only ever cite APIs the selected
    /// profile actually has (DESIGN §7.3).
    fn validate_fork_capabilities(&self) -> Result<(), KnowledgeError> {
        let Some(capabilities) = &self.fork_capabilities else {
            return Ok(());
        };
        if let Some(tex) = &capabilities.tex_parallel_compile {
            if tex.entry_points.is_empty() {
                return Err(KnowledgeError::Invalid {
                    profile: self.name.clone(),
                    message: "`fork_capabilities.tex_parallel_compile` declares no entry_points"
                        .to_owned(),
                });
            }
            for entry_point in &tex.entry_points {
                if !self.symbols.contains_key(entry_point) {
                    return Err(KnowledgeError::Invalid {
                        profile: self.name.clone(),
                        message: format!(
                            "`fork_capabilities.tex_parallel_compile` entry point \
                             `{entry_point}` is not a curated symbol"
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Applies an overlay document on top of a resolved base profile.
///
/// Overlay rules (DESIGN §5.4):
///
/// - the overlay's `base_profile` must name the base's `name` and
///   `source_digest` exactly;
/// - each `symbols` entry replaces the base entry with the same qualified
///   key wholesale — there is no recursive deep merge;
/// - each `deleted_symbols` / `deleted_exports` key must exist in the base
///   and is removed from the result;
/// - exports merge per name (overlay wins), and every surviving export
///   must point at a surviving symbol;
/// - a present overlay `fork_capabilities` block replaces the base's block
///   wholesale (an absent block inherits the base's, which is `None` for
///   the upstream profile).
pub fn apply_overlay(
    base: &KnowledgeProfile,
    overlay: &ProfileDocument,
) -> Result<KnowledgeProfile, KnowledgeError> {
    overlay.validate_structure()?;
    let invalid = |message: String| KnowledgeError::Invalid {
        profile: overlay.name.clone(),
        message,
    };
    let Some(base_ref) = &overlay.base_profile else {
        return Err(invalid("overlay is missing `base_profile`".to_owned()));
    };
    if base_ref.name != base.name {
        return Err(invalid(format!(
            "base_profile.name `{}` does not match base `{}`",
            base_ref.name, base.name
        )));
    }
    if base_ref.source_digest != base.source_digest {
        return Err(invalid(format!(
            "base_profile.source_digest `{}` does not match base digest `{}`",
            base_ref.source_digest, base.source_digest
        )));
    }

    let mut symbols = base.symbols.clone();
    for key in &overlay.deleted_symbols {
        if symbols.remove(key).is_none() {
            return Err(invalid(format!(
                "deleted symbol `{key}` does not exist in base profile `{}`",
                base.name
            )));
        }
    }
    for (key, entry) in &overlay.symbols {
        symbols.insert(key.clone(), entry.clone());
    }

    let mut exports = base.exports.clone();
    for name in &overlay.deleted_exports {
        if exports.remove(name).is_none() {
            return Err(invalid(format!(
                "deleted export `{name}` does not exist in base profile `{}`",
                base.name
            )));
        }
    }
    for (name, id) in &overlay.exports {
        exports.insert(name.clone(), id.clone());
    }

    let resolved = KnowledgeProfile {
        schema_version: overlay.schema_version,
        name: overlay.name.clone(),
        manim_version: overlay.manim_version.clone(),
        source_digest: overlay.source_digest.clone(),
        base_profile: Some(base.name.clone()),
        fork_capabilities: overlay
            .fork_capabilities
            .clone()
            .or_else(|| base.fork_capabilities.clone()),
        symbols,
        exports,
    };
    resolved.validate_exports()?;
    resolved.validate_fork_capabilities()?;
    Ok(resolved)
}
