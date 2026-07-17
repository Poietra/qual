//! `MLD303` / `MLD305`: asset path portability (DESIGN §7.4, §7.2 prose).
//!
//! - `MLD303`: a literal asset path whose *syntax* belongs to a different
//!   platform than the render profile targets — a Windows drive /
//!   backslash path under a POSIX profile, or a POSIX absolute path under
//!   a Windows profile. `MLR104` deliberately treats foreign-platform
//!   syntax as unverifiable and stays silent, so this rule owns that
//!   territory (per-profile `applicable_profiles`).
//! - `MLD305`: a relative asset path whose target exists on disk only
//!   under a case-only mismatch, reported for profiles whose target
//!   platform is case-sensitive (Linux). The two case scans split by
//!   specificity (DESIGN §7.3 prose: one finding per span): a
//!   literal-level mismatch (1:1 rewrite of the literal) is `MLR104`'s,
//!   which carries the SAFE case fix; this rule covers Manim's
//!   extension-augmented candidates (`assets_dir/name.svg` for
//!   `SVGMobject("Name")`) that `MLR104`'s scan cannot map back onto the
//!   literal.
//!
//! `has_drive_prefix` and the component-wise case-insensitive scan are
//! shared with `rules::rendering::assets` (exported through
//! `rules::rendering`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::config::model::Platform;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::knowledge::KnowledgeProfile;
use crate::rules::base::{Rule, RuleContext};
use crate::rules::rendering::{case_insensitive_match, has_drive_prefix};

use super::{build_diagnostic, short_name, single_knowledge_symbol};

const SVG_MOBJECT: &str = "manim.mobject.svg.svg_mobject.SVGMobject";
const IMAGE_MOBJECT: &str = "manim.mobject.types.image_mobject.ImageMobject";

/// Extensions Manim appends under `assets_dir` for `SVGMobject`.
const SVG_EXTENSIONS: &[&str] = &[".svg"];

/// Extensions Manim appends under `assets_dir` for `ImageMobject`.
const RASTER_EXTENSIONS: &[&str] = &[".jpg", ".jpeg", ".png", ".gif", ".ico"];

/// One literal asset-path argument of a knowledge-resolved constructor.
struct LiteralAssetPath<'a> {
    constructor_id: &'a str,
    extensions: &'static [&'static str],
    value: &'a str,
    range: TextRange,
    file: crate::source::FileId,
}

/// Collects every literal asset path passed to `SVGMobject` /
/// `ImageMobject`, mirroring `MLR104`'s argument extraction.
fn literal_asset_paths<'a>(
    context: &'a RuleContext<'_>,
    knowledge: &'a KnowledgeProfile,
) -> Vec<LiteralAssetPath<'a>> {
    let mut paths = Vec::new();
    for call in &context.qualified_calls().calls {
        let Some((constructor_id, _)) = single_knowledge_symbol(knowledge, &call.candidates) else {
            continue;
        };
        let (extensions, parameter) = match constructor_id {
            SVG_MOBJECT => (SVG_EXTENSIONS, "file_name"),
            IMAGE_MOBJECT => (RASTER_EXTENSIONS, "filename_or_array"),
            _ => continue,
        };
        let Some(argument) = call.keyword(parameter).or_else(|| call.positional(0)) else {
            continue;
        };
        let Some(LiteralFact::Str {
            value,
            prefix,
            range,
        }) = &argument.literal
        else {
            continue;
        };
        if prefix.bytes || value.is_empty() {
            continue;
        }
        paths.push(LiteralAssetPath {
            constructor_id,
            extensions,
            value,
            range: *range,
            file: call.file,
        });
    }
    paths
}

/// Windows-only path syntax: a drive prefix or backslash separators.
fn is_windows_style(literal: &str) -> bool {
    has_drive_prefix(literal) || literal.contains('\\')
}

// ---------------------------------------------------------------------------
// MLD303: literal asset path with foreign-platform syntax.
// ---------------------------------------------------------------------------

pub(super) const MLD303: RuleMetadata = RuleMetadata {
    id: "MLD303",
    summary: "Literal asset path syntax does not match the profile platform",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct ForeignPlatformAssetPath;

impl Rule for ForeignPlatformAssetPath {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD303
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        let mut diagnostics = Vec::new();
        for literal in literal_asset_paths(context, knowledge) {
            let windows_style = is_windows_style(literal.value);
            let posix_absolute = literal.value.starts_with('/');
            let mut windows_profiles: Vec<String> = Vec::new();
            let mut posix_profiles: Vec<String> = Vec::new();
            for profile in context.active_profiles() {
                match profile.platform {
                    Platform::Windows if posix_absolute && !windows_style => {
                        windows_profiles.push(profile.name.clone());
                    }
                    Platform::Linux | Platform::Macos if windows_style => {
                        posix_profiles.push(profile.name.clone());
                    }
                    _ => {}
                }
            }
            let file = context.sources().file(literal.file);
            let constructor = short_name(literal.constructor_id);
            if !posix_profiles.is_empty() {
                let mut evidence = BTreeMap::new();
                evidence.insert("constructor".to_owned(), json!(literal.constructor_id));
                evidence.insert("literal".to_owned(), json!(literal.value));
                evidence.insert("path_syntax".to_owned(), json!("windows"));
                diagnostics.push(build_diagnostic(
                    &MLD303,
                    file,
                    literal.range,
                    Confidence::High,
                    format!(
                        "Windows-style path \"{value}\" passed to `{constructor}()` cannot \
                         resolve on the POSIX platform this profile targets",
                        value = literal.value,
                    ),
                    "A drive prefix or backslash separators are Windows path syntax. On \
                     Linux or macOS the whole string is treated as a single (or wrong) \
                     file name, so the asset lookup fails at render time. Use a relative \
                     forward-slash path under the render working directory or \
                     assets_dir.",
                    evidence,
                    posix_profiles,
                    None,
                ));
            }
            if !windows_profiles.is_empty() {
                let mut evidence = BTreeMap::new();
                evidence.insert("constructor".to_owned(), json!(literal.constructor_id));
                evidence.insert("literal".to_owned(), json!(literal.value));
                evidence.insert("path_syntax".to_owned(), json!("posix-absolute"));
                diagnostics.push(build_diagnostic(
                    &MLD303,
                    file,
                    literal.range,
                    Confidence::High,
                    format!(
                        "POSIX absolute path \"{value}\" passed to `{constructor}()` does \
                         not name a drive on the Windows platform this profile targets",
                        value = literal.value,
                    ),
                    "A leading `/` is rooted but drive-less on Windows: it resolves \
                     against the current drive of the render process, not the POSIX \
                     location it was written for. Use a relative forward-slash path \
                     under the render working directory or assets_dir.",
                    evidence,
                    windows_profiles,
                    None,
                ));
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLD305: case-only mismatch on a case-sensitive target platform.
// ---------------------------------------------------------------------------

pub(super) const MLD305: RuleMetadata = RuleMetadata {
    id: "MLD305",
    summary: "Asset path matches an existing file only case-insensitively on a case-sensitive target platform",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct CaseOnlyAssetMismatch;

impl Rule for CaseOnlyAssetMismatch {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD305
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        let project_root = context.sources().project_root();
        let mut diagnostics = Vec::new();
        for literal in literal_asset_paths(context, knowledge) {
            // Only relative POSIX-style literals can be scanned soundly.
            if is_windows_style(literal.value)
                || literal.value.starts_with('~')
                || literal.value.starts_with('/')
            {
                continue;
            }
            // corrected path -> profiles it applies to.
            let mut corrections: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for profile in context.active_profiles() {
                // Only Linux target filesystems are case-sensitive by
                // default; macOS and Windows mask case-only mismatches.
                if profile.platform != Platform::Linux {
                    continue;
                }
                let working_dir = project_root.join(&profile.working_directory);
                if !working_dir.is_dir() {
                    continue;
                }
                let assets_base = working_dir.join(&profile.assets_dir);
                if let Some(corrected) = case_only_correction(
                    &working_dir,
                    &assets_base,
                    literal.value,
                    literal.extensions,
                ) {
                    corrections
                        .entry(corrected)
                        .or_default()
                        .push(profile.name.clone());
                }
            }
            let file = context.sources().file(literal.file);
            let constructor = short_name(literal.constructor_id);
            for (corrected, profile_names) in corrections {
                let mut evidence = BTreeMap::new();
                evidence.insert("constructor".to_owned(), json!(literal.constructor_id));
                evidence.insert("literal".to_owned(), json!(literal.value));
                evidence.insert("on_disk".to_owned(), json!(corrected));
                diagnostics.push(build_diagnostic(
                    &MLD305,
                    file,
                    literal.range,
                    Confidence::High,
                    format!(
                        "Asset \"{value}\" passed to `{constructor}()` exists on disk only \
                         as \"{corrected}\"; the lookup on the case-sensitive target \
                         platform (linux) fails",
                        value = literal.value,
                    ),
                    "Manim resolves the path against the render working directory and \
                     then assets_dir (with known extensions appended) using the exact \
                     character case. A case-insensitive development filesystem can mask \
                     the mismatch locally, but the render on a case-sensitive platform \
                     raises OSError. Rewrite the literal to the on-disk case.",
                    evidence,
                    profile_names,
                    None,
                ));
            }
        }
        diagnostics
    }
}

/// Runs Manim's candidate search; when no candidate exists exactly but an
/// *extension-augmented* candidate matches case-insensitively, returns the
/// on-disk spelling of the match (search-base-relative, forward slashes).
///
/// Literal-level mismatches (a 1:1 rewrite of the literal as written) are
/// deliberately `None`: `MLR104` reports exactly those with a SAFE case
/// correction, and one span never carries the same finding twice
/// (DESIGN §7.3 specificity prose).
fn case_only_correction(
    working_dir: &Path,
    assets_base: &Path,
    literal: &str,
    extensions: &[&str],
) -> Option<String> {
    let mut candidates: Vec<(PathBuf, String)> = vec![
        (working_dir.to_path_buf(), literal.to_owned()),
        (assets_base.to_path_buf(), literal.to_owned()),
    ];
    for extension in extensions {
        candidates.push((assets_base.to_path_buf(), format!("{literal}{extension}")));
    }
    if candidates
        .iter()
        .any(|(base, relative)| base.join(relative).exists())
    {
        return None;
    }
    // Literal-level scan: MLR104's territory.
    for base in [working_dir, assets_base] {
        if case_insensitive_match(base, literal).is_some() {
            return None;
        }
    }
    for extension in extensions {
        let variant = format!("{literal}{extension}");
        if let Some(corrected) = case_insensitive_match(assets_base, &variant) {
            return Some(corrected);
        }
    }
    None
}
