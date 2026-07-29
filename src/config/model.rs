//! Configuration data model (DESIGN §8.2).
//!
//! Every setting flows through an explicit precedence chain:
//! `CLI > selected profile > pyproject base > manim.cfg > builtin defaults`.
//! Each tier contributes a [`ConfigFragment`] whose fields are all optional;
//! [`ConfigFragment::merge`] resolves them first-`Some`-wins in that order.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::diagnostic::{Confidence, Severity};

/// Renderer backend targeted by a render profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    /// The default Cairo renderer.
    Cairo,
    /// The OpenGL renderer.
    Opengl,
}

impl std::fmt::Display for Renderer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cairo => "cairo",
            Self::Opengl => "opengl",
        })
    }
}

impl std::str::FromStr for Renderer {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "cairo" => Ok(Self::Cairo),
            "opengl" => Ok(Self::Opengl),
            _ => Err(format!("unknown renderer: {value} (expected cairo|opengl)")),
        }
    }
}

/// Operating system a render profile targets (asset/path/font semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Linux path and font semantics.
    Linux,
    /// macOS path and font semantics.
    Macos,
    /// Windows path and font semantics.
    Windows,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        })
    }
}

/// Identifies which precedence tier a [`ConfigFragment`] came from.
///
/// Variants are ordered from highest to lowest precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigSource {
    /// Command-line flags.
    Cli,
    /// The `[[tool.qual.profile]]` entry selected for this run.
    SelectedProfile,
    /// Base keys under `[tool.qual]` in `pyproject.toml`.
    PyprojectBase,
    /// Values read from `manim.cfg` (only when `respect-manim-cfg`).
    ManimCfg,
    /// Hard-coded defaults; always fully populated.
    BuiltinDefaults,
}

/// One precedence tier's contribution to the effective configuration.
///
/// All fields are optional; unset fields defer to lower-precedence tiers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigFragment {
    // --- lint-level settings -------------------------------------------
    /// Target Manim version string, e.g. `"0.20"`.
    pub manim_version: Option<String>,
    /// Target Python version, e.g. `"3.11"`.
    pub target_python: Option<String>,
    /// Enabled rule selectors (rule IDs or prefixes).
    pub select: Option<Vec<String>>,
    /// Disabled rule selectors.
    pub ignore: Option<Vec<String>>,
    /// Minimum confidence a diagnostic needs to be reported.
    pub min_confidence: Option<Confidence>,
    /// Minimum severity that makes `check` exit with code 1.
    pub fail_level: Option<Severity>,
    /// Name of the profile analyzed when `--profile` is omitted.
    pub default_profile: Option<String>,
    /// Versioned Manim knowledge profile ID.
    pub knowledge_profile: Option<String>,
    /// Whether `manim.cfg` feeds resolution/fps/renderer defaults.
    pub respect_manim_cfg: Option<bool>,
    /// Glob patterns excluded from analysis.
    pub exclude: Option<Vec<String>>,
    /// Per-file-glob rule selector suppressions.
    pub per_file_ignores: Option<BTreeMap<String, Vec<String>>>,
    /// Import resolution roots.
    pub source_roots: Option<Vec<String>>,
    /// Additional stub search paths.
    pub stub_paths: Option<Vec<String>>,

    // --- render-profile settings ----------------------------------------
    /// Renderer backend.
    pub renderer: Option<Renderer>,
    /// Target platform.
    pub platform: Option<Platform>,
    /// Render working directory (asset resolution origin).
    pub working_directory: Option<String>,
    /// Output width in pixels.
    pub pixel_width: Option<u32>,
    /// Output height in pixels.
    pub pixel_height: Option<u32>,
    /// Frames per second.
    pub frame_rate: Option<f64>,
    /// Manim `assets_dir` used by the asset resolver.
    pub assets_dir: Option<String>,
    /// Fonts assumed installed on the target platform.
    pub allowed_fonts: Option<Vec<String>>,
    /// Local-fork Cairo fork worker count (0 = not requested).
    pub cairo_fork_workers: Option<u32>,
    /// Local-fork Cairo static layer reuse.
    pub cairo_static_layers: Option<bool>,
    /// Video encoder name, e.g. `libx264`.
    pub video_encoder: Option<String>,
    /// Whether output has an alpha channel.
    pub transparent: Option<bool>,
    /// Antialias mode label.
    pub antialias: Option<String>,
    /// OpenGL readback mode label.
    pub opengl_readback: Option<String>,
}

macro_rules! merge_fields {
    ($result:ident, $layer:ident; $($field:ident),+ $(,)?) => {
        $(
            if $result.$field.is_none() {
                $result.$field = $layer.$field.clone();
            }
        )+
    };
}

impl ConfigFragment {
    /// Resolves fragments ordered from highest to lowest precedence.
    ///
    /// For every field the first tier that sets it wins.
    #[must_use]
    pub fn merge(layers: &[&Self]) -> Self {
        let mut result = Self::default();
        for layer in layers {
            merge_fields!(
                result, layer;
                manim_version, target_python, select, ignore, min_confidence,
                fail_level, default_profile, knowledge_profile, respect_manim_cfg,
                exclude, per_file_ignores, source_roots, stub_paths,
                renderer, platform, working_directory, pixel_width, pixel_height,
                frame_rate, assets_dir, allowed_fonts, cairo_fork_workers,
                cairo_static_layers, video_encoder, transparent, antialias,
                opengl_readback,
            );
        }
        result
    }

    /// The builtin lowest-precedence tier. Every field is populated, which
    /// guarantees total resolution.
    #[must_use]
    pub fn builtin_defaults() -> Self {
        Self {
            manim_version: Some("0.20".to_owned()),
            target_python: Some("3.11".to_owned()),
            select: Some(
                crate::rules::registry::RULE_PREFIXES
                    .iter()
                    .map(|prefix| (*prefix).to_owned())
                    .collect(),
            ),
            ignore: Some(Vec::new()),
            min_confidence: Some(Confidence::High),
            fail_level: Some(Severity::Warning),
            default_profile: Some("default".to_owned()),
            knowledge_profile: None,
            respect_manim_cfg: Some(true),
            exclude: Some(vec![".venv/**".to_owned(), "media/**".to_owned()]),
            per_file_ignores: Some(BTreeMap::new()),
            source_roots: Some(vec![".".to_owned()]),
            stub_paths: Some(Vec::new()),
            renderer: Some(Renderer::Cairo),
            platform: Some(Platform::Linux),
            working_directory: Some(".".to_owned()),
            pixel_width: Some(1920),
            pixel_height: Some(1080),
            frame_rate: Some(60.0),
            assets_dir: Some(".".to_owned()),
            allowed_fonts: Some(Vec::new()),
            cairo_fork_workers: Some(0),
            cairo_static_layers: Some(false),
            video_encoder: Some("libx264".to_owned()),
            transparent: Some(false),
            antialias: Some("default".to_owned()),
            opengl_readback: Some("auto".to_owned()),
        }
    }
}

/// A fully resolved render profile (every field decided).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderProfile {
    /// User-facing profile name.
    pub name: String,
    /// Renderer backend.
    pub renderer: Renderer,
    /// Target platform.
    pub platform: Platform,
    /// Render working directory.
    pub working_directory: String,
    /// Output width in pixels.
    pub pixel_width: u32,
    /// Output height in pixels.
    pub pixel_height: u32,
    /// Frames per second.
    pub frame_rate: f64,
    /// Asset search directory.
    pub assets_dir: String,
    /// Fonts assumed installed.
    pub allowed_fonts: Vec<String>,
    /// Local-fork Cairo fork worker count.
    pub cairo_fork_workers: u32,
    /// Local-fork Cairo static layer reuse.
    pub cairo_static_layers: bool,
    /// Video encoder name.
    pub video_encoder: String,
    /// Whether output has an alpha channel.
    pub transparent: bool,
    /// Antialias mode label.
    pub antialias: String,
    /// OpenGL readback mode label.
    pub opengl_readback: String,
}

/// The effective configuration after full precedence resolution.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedConfig {
    /// Absolute project root all paths are relative to.
    pub project_root: PathBuf,
    /// Target Manim version string.
    pub manim_version: String,
    /// `manim-version` exactly as declared in `pyproject.toml`, when set.
    ///
    /// Only a declared version is validated against the knowledge
    /// profile's supported range; the builtin default in
    /// [`manim_version`](Self::manim_version) is informational. Not
    /// serialized: the resolved value above is the public one.
    #[serde(skip)]
    pub declared_manim_version: Option<String>,
    /// Target Python version string.
    pub target_python: String,
    /// Enabled rule selectors.
    pub select: Vec<String>,
    /// Disabled rule selectors.
    pub ignore: Vec<String>,
    /// Minimum reported confidence.
    pub min_confidence: Confidence,
    /// Severity threshold for exit code 1.
    pub fail_level: Severity,
    /// Knowledge profile ID, when configured.
    pub knowledge_profile: Option<String>,
    /// Whether `manim.cfg` was consulted.
    pub respect_manim_cfg: bool,
    /// Exclusion globs.
    pub exclude: Vec<String>,
    /// Per-file-glob rule suppressions.
    pub per_file_ignores: BTreeMap<String, Vec<String>>,
    /// Import resolution roots.
    pub source_roots: Vec<String>,
    /// Stub search paths. Always empty today: stub loading is not
    /// implemented, and a non-empty `stub-paths` is rejected as a
    /// configuration error instead of being silently ignored.
    pub stub_paths: Vec<String>,
    /// Name of the default profile.
    pub default_profile: String,
    /// Names of every configured profile.
    pub all_profile_names: Vec<String>,
    /// Profiles analyzed in this run, fully resolved.
    pub active_profiles: Vec<RenderProfile>,
}

impl ResolvedConfig {
    /// Names of the profiles analyzed in this run.
    #[must_use]
    pub fn active_profile_names(&self) -> Vec<String> {
        self.active_profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment_with_fps(fps: f64) -> ConfigFragment {
        ConfigFragment {
            frame_rate: Some(fps),
            ..ConfigFragment::default()
        }
    }

    #[test]
    fn precedence_cli_beats_profile_beats_base_beats_manim_cfg_beats_defaults() {
        let cli = fragment_with_fps(24.0);
        let profile = fragment_with_fps(30.0);
        let base = fragment_with_fps(48.0);
        let manim_cfg = fragment_with_fps(50.0);
        let defaults = ConfigFragment::builtin_defaults();

        let all = ConfigFragment::merge(&[&cli, &profile, &base, &manim_cfg, &defaults]);
        assert_eq!(all.frame_rate, Some(24.0), "CLI wins");

        let no_cli = ConfigFragment::merge(&[
            &ConfigFragment::default(),
            &profile,
            &base,
            &manim_cfg,
            &defaults,
        ]);
        assert_eq!(no_cli.frame_rate, Some(30.0), "profile beats base");

        let no_profile = ConfigFragment::merge(&[
            &ConfigFragment::default(),
            &ConfigFragment::default(),
            &base,
            &manim_cfg,
            &defaults,
        ]);
        assert_eq!(no_profile.frame_rate, Some(48.0), "base beats manim.cfg");

        let manim_cfg_only = ConfigFragment::merge(&[
            &ConfigFragment::default(),
            &ConfigFragment::default(),
            &ConfigFragment::default(),
            &manim_cfg,
            &defaults,
        ]);
        assert_eq!(
            manim_cfg_only.frame_rate,
            Some(50.0),
            "manim.cfg beats builtin defaults"
        );

        let defaults_only = ConfigFragment::merge(&[&defaults]);
        assert_eq!(defaults_only.frame_rate, Some(60.0), "builtin default");
    }

    #[test]
    fn builtin_defaults_populate_every_field_needed_for_resolution() {
        let defaults = ConfigFragment::builtin_defaults();
        assert!(defaults.target_python.is_some());
        assert!(defaults.select.is_some());
        assert!(defaults.min_confidence.is_some());
        assert!(defaults.fail_level.is_some());
        assert!(defaults.renderer.is_some());
        assert!(defaults.pixel_width.is_some());
        assert!(defaults.pixel_height.is_some());
        assert!(defaults.frame_rate.is_some());
    }
}
