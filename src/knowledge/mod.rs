//! Versioned Manim knowledge profiles (DESIGN §5.4).
//!
//! The linter never imports Manim; everything it knows about Manim symbols,
//! effects, and renderer capabilities comes from reviewed JSON profiles
//! embedded at compile time from `src/knowledge/profiles/*.json`.
//!
//! - [`load`] resolves a profile by its `name` field (the value of the
//!   `knowledge-profile` config key), applying overlay semantics when the
//!   document declares a `base_profile`.
//! - [`list_available`] enumerates the shipped profile names.
//!
//! The shipped `v0_20.json` carries the name `upstream_0_20` and describes
//! upstream Manim Community 0.20 public semantics as of the clean base
//! commit `4d25c031`. The shipped `local_0_20_1_4d25c031.json` overlay
//! carries the sibling fork's local additions on top of that base; overlays
//! replace whole symbol entries of their base (see `profiles/README.md`).

pub mod generator;
pub mod model;

use std::collections::BTreeMap;
use std::sync::OnceLock;

pub use model::{
    AcceptedTarget, BaseProfileRef, KnowledgeError, KnowledgeProfile, ParamFact, ProfileDocument,
    RendererCompat, SceneMembershipEffect, SignatureFacts, SymbolEffects, SymbolEntry, SymbolKind,
    apply_overlay,
};

/// Name of the profile used when the configuration does not set
/// `knowledge-profile`: the shipped upstream Manim Community 0.20 profile.
pub const DEFAULT_PROFILE: &str = "upstream_0_20";

/// Profile JSON files embedded at compile time: `(file name, content)`.
const EMBEDDED_PROFILES: &[(&str, &str)] = &[
    ("v0_20.json", include_str!("profiles/v0_20.json")),
    (
        "local_0_20_1_4d25c031.json",
        include_str!("profiles/local_0_20_1_4d25c031.json"),
    ),
];

/// Accepted alternate spellings: `(alias, canonical profile name)`.
///
/// `v0_20` is accepted for the upstream 0.20 profile because DESIGN names
/// the file `v0_20.json` while the config examples use `upstream_0_20`.
const NAME_ALIASES: &[(&str, &str)] = &[("v0_20", "upstream_0_20")];

/// Parsed embedded documents, keyed by their `name` field.
type Registry = BTreeMap<&'static str, ProfileDocument>;

fn registry() -> &'static Result<Registry, KnowledgeError> {
    static REGISTRY: OnceLock<Result<Registry, KnowledgeError>> = OnceLock::new();
    REGISTRY.get_or_init(build_registry)
}

fn build_registry() -> Result<Registry, KnowledgeError> {
    let mut registry = Registry::new();
    for (file, json) in EMBEDDED_PROFILES {
        let document = ProfileDocument::from_json(file, json)?;
        // Keys must borrow from the embedded document names; leak once at
        // startup so `list_available` can hand out `&'static str`.
        let name: &'static str = Box::leak(document.name.clone().into_boxed_str());
        if registry.insert(name, document).is_some() {
            return Err(KnowledgeError::Invalid {
                profile: name.to_owned(),
                message: format!("duplicate embedded profile name in `{file}`"),
            });
        }
    }
    Ok(registry)
}

/// Loads and resolves the knowledge profile with the given name.
///
/// `name` is matched against each shipped document's `name` field (plus the
/// documented aliases). Overlay documents are resolved against their base:
/// the base must be shipped, must not itself be an overlay, and must match
/// the overlay's `base_profile` name and digest exactly.
///
/// An unknown name returns [`KnowledgeError::UnknownProfile`]; the
/// application maps configuration-level failures to exit code 2.
pub fn load(name: &str) -> Result<KnowledgeProfile, KnowledgeError> {
    let registry = registry().as_ref().map_err(Clone::clone)?;
    let canonical = NAME_ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map_or(name, |(_, canonical)| *canonical);
    let Some(document) = registry.get(canonical) else {
        return Err(KnowledgeError::UnknownProfile {
            name: name.to_owned(),
            available: registry.keys().copied().collect::<Vec<_>>().join(", "),
        });
    };
    match &document.base_profile {
        None => document.clone().into_resolved(),
        Some(base_ref) => {
            let Some(base_document) = registry.get(base_ref.name.as_str()) else {
                return Err(KnowledgeError::Invalid {
                    profile: document.name.clone(),
                    message: format!("base profile `{}` is not shipped", base_ref.name),
                });
            };
            if base_document.base_profile.is_some() {
                return Err(KnowledgeError::Invalid {
                    profile: document.name.clone(),
                    message: format!(
                        "base profile `{}` is itself an overlay; overlay chains are not supported",
                        base_ref.name
                    ),
                });
            }
            let base = base_document.clone().into_resolved()?;
            apply_overlay(&base, document)
        }
    }
}

/// Names of all shipped knowledge profiles, sorted.
///
/// Aliases are not listed; only canonical `name` field values appear. If an
/// embedded profile fails to parse (a build defect), the list is empty and
/// [`load`] reports the underlying error.
#[must_use]
pub fn list_available() -> Vec<&'static str> {
    registry()
        .as_ref()
        .map(|registry| registry.keys().copied().collect())
        .unwrap_or_default()
}
