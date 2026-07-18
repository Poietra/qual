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
            symbols: self.symbols,
            exports: self.exports,
        };
        resolved.validate_exports()?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
///   must point at a surviving symbol.
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
        symbols,
        exports,
    };
    resolved.validate_exports()?;
    Ok(resolved)
}
