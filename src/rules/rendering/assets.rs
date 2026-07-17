//! Literal asset path resolution for `MLR104` (DESIGN §7.2 prose).
//!
//! The resolver mirrors Manim's runtime search **exactly**
//! (`manim/utils/file_ops.py::seek_full_path_from_defaults`):
//!
//! 1. `Path(file_name)` resolved against the profile working directory
//!    (Manim's cwd at render time);
//! 2. `assets_dir / f"{file_name}{extension}"` for extension in
//!    `["", *extensions]`.
//!
//! The source file's directory and the project root are **not** part of
//! the runtime search path; a hit there is reported as extra evidence
//! only. When the working directory itself cannot be found on the
//! analysis machine, nothing can be verified and the caller must stay
//! silent.

use std::path::{Component, Path, PathBuf};

/// Outcome of resolving one literal asset path for one render profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetResolution {
    /// Some candidate exists exactly as written.
    Found,
    /// No candidate exists as written, but the literal path matches an
    /// existing file when compared case-insensitively. `corrected` is the
    /// literal rewritten with the on-disk character case.
    CaseOnlyMismatch {
        /// The literal path with on-disk case.
        corrected: String,
    },
    /// Every candidate is missing. `tried` lists the candidates in Manim's
    /// search order (project-root-relative where possible).
    NotFound {
        /// Candidate paths in search order, for evidence.
        tried: Vec<String>,
    },
    /// The search cannot be reproduced on the analysis machine (missing
    /// working directory, `~` expansion, or foreign-platform path syntax).
    Unverifiable,
}

/// Extensions Manim appends for `SVGMobject`
/// (`manim/utils/images.py::get_full_vector_image_path`).
pub const SVG_EXTENSIONS: &[&str] = &[".svg"];

/// Extensions Manim appends for `ImageMobject`
/// (`manim/utils/images.py::get_full_raster_image_path`).
pub const RASTER_EXTENSIONS: &[&str] = &[".jpg", ".jpeg", ".png", ".gif", ".ico"];

/// Resolves `literal` exactly like Manim would for a profile whose render
/// working directory is `working_dir` (already joined to the project root)
/// and whose `assets_dir` is `assets_dir`.
#[must_use]
pub fn resolve_literal(
    project_root: &Path,
    working_dir: &Path,
    assets_dir: &str,
    literal: &str,
    extensions: &[&str],
) -> AssetResolution {
    // `~` expands to the *render-time* user's home; the analysis machine
    // cannot verify that. Backslashes and drive prefixes are foreign
    // (Windows) path syntax that a POSIX `Path` would misinterpret.
    if literal.starts_with('~') || literal.contains('\\') || has_drive_prefix(literal) {
        return AssetResolution::Unverifiable;
    }
    if !working_dir.is_dir() {
        // The render working directory is absent here: cannot verify.
        return AssetResolution::Unverifiable;
    }
    let assets_base = working_dir.join(assets_dir);

    // Candidates in Manim's exact order. `Path::join` with an absolute
    // `literal` replaces the base, matching Python's `Path` semantics.
    let mut candidates: Vec<PathBuf> = vec![working_dir.join(literal)];
    candidates.push(assets_base.join(literal));
    for extension in extensions {
        candidates.push(assets_base.join(format!("{literal}{extension}")));
    }

    for candidate in &candidates {
        if candidate.exists() {
            return AssetResolution::Found;
        }
    }

    // Case-only mismatch scan (relative literals only, no extension
    // variants: the correction must map 1:1 back onto the literal).
    if !Path::new(literal).is_absolute() {
        for base in [working_dir, &assets_base] {
            if let Some(corrected) = case_insensitive_match(base, literal) {
                return AssetResolution::CaseOnlyMismatch { corrected };
            }
        }
    }

    let tried = candidates
        .iter()
        .map(|candidate| display_path(project_root, candidate))
        .collect();
    AssetResolution::NotFound { tried }
}

/// Whether the file also exists next to the analyzed source file. Manim
/// never searches there; this is evidence for the diagnostic message only.
#[must_use]
pub fn exists_relative_to(source_dir: &Path, literal: &str) -> bool {
    if literal.starts_with('~') || literal.contains('\\') || Path::new(literal).is_absolute() {
        return false;
    }
    source_dir.join(literal).exists()
}

fn has_drive_prefix(literal: &str) -> bool {
    let bytes = literal.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Resolves `relative` against `base` component by component, matching
/// case-insensitively when the exact component is missing. Returns the
/// literal rewritten with the on-disk case only when it differs from the
/// original (a true case-only mismatch).
fn case_insensitive_match(base: &Path, relative: &str) -> Option<String> {
    let mut current = base.to_path_buf();
    let mut corrected_parts: Vec<String> = Vec::new();
    for component in Path::new(relative).components() {
        let Component::Normal(part) = component else {
            // `..`, `.` and friends: keep the scan simple and give up.
            return None;
        };
        let part = part.to_str()?;
        let exact = current.join(part);
        if exact.exists() {
            corrected_parts.push(part.to_owned());
            current = exact;
            continue;
        }
        let entries = std::fs::read_dir(&current).ok()?;
        let mut matched: Option<String> = None;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            if name.to_lowercase() == part.to_lowercase() {
                matched = Some(name);
                break;
            }
        }
        let matched = matched?;
        current = current.join(&matched);
        corrected_parts.push(matched);
    }
    let corrected = corrected_parts.join("/");
    (corrected != relative).then_some(corrected)
}

fn display_path(project_root: &Path, path: &Path) -> String {
    let shown = path.strip_prefix(project_root).unwrap_or(path);
    let parts: Vec<String> = shown
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("logo.svg"), b"<svg/>").unwrap();
        std::fs::write(dir.path().join("assets/picture.png"), b"png").unwrap();
        dir
    }

    #[test]
    fn direct_and_extension_search_matches_manim_order() {
        let dir = project();
        let root = dir.path();
        assert_eq!(
            resolve_literal(root, root, ".", "logo.svg", SVG_EXTENSIONS),
            AssetResolution::Found
        );
        // Extension appended in the assets_dir step.
        assert_eq!(
            resolve_literal(root, root, ".", "logo", SVG_EXTENSIONS),
            AssetResolution::Found
        );
        assert_eq!(
            resolve_literal(root, root, "assets", "picture", RASTER_EXTENSIONS),
            AssetResolution::Found
        );
    }

    #[test]
    fn missing_file_lists_the_tried_candidates() {
        let dir = project();
        let root = dir.path();
        let AssetResolution::NotFound { tried } =
            resolve_literal(root, root, ".", "absent", SVG_EXTENSIONS)
        else {
            panic!("must be NotFound");
        };
        assert_eq!(tried, ["absent", "absent", "absent.svg"]);
    }

    #[test]
    fn case_only_mismatch_yields_the_on_disk_case() {
        let dir = project();
        let root = dir.path();
        assert_eq!(
            resolve_literal(root, root, ".", "Logo.svg", SVG_EXTENSIONS),
            AssetResolution::CaseOnlyMismatch {
                corrected: "logo.svg".to_owned()
            }
        );
        assert_eq!(
            resolve_literal(root, root, ".", "Assets/Picture.png", RASTER_EXTENSIONS),
            AssetResolution::CaseOnlyMismatch {
                corrected: "assets/picture.png".to_owned()
            }
        );
    }

    #[test]
    fn unverifiable_inputs_stay_silent() {
        let dir = project();
        let root = dir.path();
        for literal in ["~/x.svg", "C:img.png", "dir\\file.svg"] {
            assert_eq!(
                resolve_literal(root, root, ".", literal, SVG_EXTENSIONS),
                AssetResolution::Unverifiable,
                "{literal}"
            );
        }
        // Missing working directory: nothing can be verified.
        assert_eq!(
            resolve_literal(root, &root.join("nope"), ".", "logo.svg", SVG_EXTENSIONS),
            AssetResolution::Unverifiable
        );
    }

    #[test]
    fn source_directory_hit_is_evidence_only() {
        let dir = project();
        assert!(exists_relative_to(dir.path(), "logo.svg"));
        assert!(!exists_relative_to(dir.path(), "missing.svg"));
    }
}
