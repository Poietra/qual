//! Configuration discovery, parsing, and validation (DESIGN §8.2).
//!
//! Reads `[tool.manim-lint]` from `pyproject.toml`, optionally a minimal
//! `manim.cfg` INI, applies the precedence chain from
//! [`crate::config::model`], and validates the result. Every error maps to
//! exit code 2.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::model::{ConfigFragment, Platform, RenderProfile, Renderer, ResolvedConfig};
use crate::diagnostic::{Confidence, Severity};
use crate::rules::registry;

/// A configuration error; always reported with exit code 2.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `pyproject.toml` or `manim.cfg` could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// File that failed to read.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// `pyproject.toml` is not valid TOML or has invalid keys/values.
    #[error("invalid pyproject.toml at {path}: {message}")]
    InvalidPyproject {
        /// File that failed to parse.
        path: PathBuf,
        /// Parser or schema message.
        message: String,
    },
    /// `manim.cfg` contains a value manim-lint cannot interpret.
    #[error("invalid manim.cfg at {path}: {message}")]
    InvalidManimCfg {
        /// File that failed to parse.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// A select/ignore/per-file-ignores entry is not a known rule selector.
    #[error("{0}")]
    UnknownSelector(String),
    /// Two profiles share the same name.
    #[error("duplicate profile name: {0}")]
    DuplicateProfile(String),
    /// `default-profile` (or `--profile`) names a profile that is not defined.
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    /// Profiles are defined but no `default-profile` is set.
    #[error("profiles are defined but default-profile is not set")]
    MissingDefaultProfile,
    /// A CLI value could not be interpreted.
    #[error("{0}")]
    InvalidValue(String),
}

/// `[tool.manim-lint]` exactly as written in `pyproject.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PyprojectSection {
    manim_version: Option<String>,
    target_python: Option<String>,
    select: Option<Vec<String>>,
    ignore: Option<Vec<String>>,
    min_confidence: Option<Confidence>,
    fail_level: Option<Severity>,
    default_profile: Option<String>,
    knowledge_profile: Option<String>,
    respect_manim_cfg: Option<bool>,
    exclude: Option<Vec<String>>,
    per_file_ignores: Option<BTreeMap<String, Vec<String>>>,
    source_roots: Option<Vec<String>>,
    stub_paths: Option<Vec<String>>,
    #[serde(default)]
    profile: Vec<ProfileSection>,
}

/// One `[[tool.manim-lint.profile]]` entry as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ProfileSection {
    name: String,
    renderer: Option<Renderer>,
    platform: Option<Platform>,
    working_directory: Option<String>,
    pixel_width: Option<u32>,
    pixel_height: Option<u32>,
    frame_rate: Option<f64>,
    assets_dir: Option<String>,
    allowed_fonts: Option<Vec<String>>,
    cairo_fork_workers: Option<u32>,
    cairo_static_layers: Option<bool>,
    video_encoder: Option<String>,
    transparent: Option<bool>,
    antialias: Option<String>,
    opengl_readback: Option<String>,
}

impl ProfileSection {
    fn fragment(&self) -> ConfigFragment {
        ConfigFragment {
            renderer: self.renderer,
            platform: self.platform,
            working_directory: self.working_directory.clone(),
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            frame_rate: self.frame_rate,
            assets_dir: self.assets_dir.clone(),
            allowed_fonts: self.allowed_fonts.clone(),
            cairo_fork_workers: self.cairo_fork_workers,
            cairo_static_layers: self.cairo_static_layers,
            video_encoder: self.video_encoder.clone(),
            transparent: self.transparent,
            antialias: self.antialias.clone(),
            opengl_readback: self.opengl_readback.clone(),
            ..ConfigFragment::default()
        }
    }
}

impl PyprojectSection {
    fn base_fragment(&self) -> ConfigFragment {
        ConfigFragment {
            manim_version: self.manim_version.clone(),
            target_python: self.target_python.clone(),
            select: self.select.clone(),
            ignore: self.ignore.clone(),
            min_confidence: self.min_confidence,
            fail_level: self.fail_level,
            default_profile: self.default_profile.clone(),
            knowledge_profile: self.knowledge_profile.clone(),
            respect_manim_cfg: self.respect_manim_cfg,
            exclude: self.exclude.clone(),
            per_file_ignores: self.per_file_ignores.clone(),
            source_roots: self.source_roots.clone(),
            stub_paths: self.stub_paths.clone(),
            ..ConfigFragment::default()
        }
    }
}

/// Which profiles a `check` run analyzes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProfileSelection {
    /// The configured `default-profile`.
    #[default]
    Default,
    /// One named profile (`--profile NAME`).
    Named(String),
    /// Every configured profile (`--profile all`).
    All,
}

impl ProfileSelection {
    /// Interprets the raw `--profile` CLI value.
    #[must_use]
    pub fn from_cli(value: Option<&str>) -> Self {
        match value {
            None => Self::Default,
            Some("all") => Self::All,
            Some(name) => Self::Named(name.to_owned()),
        }
    }
}

/// Walks up from `start` looking for a `pyproject.toml`.
#[must_use]
pub fn find_pyproject(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join("pyproject.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

/// Parses `[tool.manim-lint]` from a `pyproject.toml` file.
///
/// Returns `Ok(None)` when the file has no `[tool.manim-lint]` table.
pub fn load_pyproject(path: &Path) -> Result<Option<PyprojectSection>, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_pyproject(&text).map_err(|message| ConfigError::InvalidPyproject {
        path: path.to_path_buf(),
        message,
    })
}

/// Parses `[tool.manim-lint]` from pyproject TOML text.
pub fn parse_pyproject(text: &str) -> Result<Option<PyprojectSection>, String> {
    #[derive(Deserialize)]
    struct PyprojectFile {
        tool: Option<ToolTable>,
    }
    #[derive(Deserialize)]
    struct ToolTable {
        #[serde(rename = "manim-lint")]
        manim_lint: Option<PyprojectSection>,
    }

    let file: PyprojectFile = toml::from_str(text).map_err(|error| error.to_string())?;
    Ok(file.tool.and_then(|tool| tool.manim_lint))
}

/// Values manim-lint reads from `manim.cfg` (resolution, fps, renderer).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManimCfgValues {
    /// `pixel_width` from the `[CLI]` section.
    pub pixel_width: Option<u32>,
    /// `pixel_height` from the `[CLI]` section.
    pub pixel_height: Option<u32>,
    /// `frame_rate` from the `[CLI]` section.
    pub frame_rate: Option<f64>,
    /// `renderer` from the `[CLI]` section.
    pub renderer: Option<Renderer>,
}

impl ManimCfgValues {
    fn fragment(&self) -> ConfigFragment {
        ConfigFragment {
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            frame_rate: self.frame_rate,
            renderer: self.renderer,
            ..ConfigFragment::default()
        }
    }
}

/// Reads `manim.cfg` from `dir` when present.
pub fn load_manim_cfg(dir: &Path) -> Result<Option<ManimCfgValues>, ConfigError> {
    let path = dir.join("manim.cfg");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
        path: path.clone(),
        source,
    })?;
    parse_manim_cfg(&text)
        .map(Some)
        .map_err(|message| ConfigError::InvalidManimCfg { path, message })
}

/// Parses the minimal INI subset of `manim.cfg` that manim-lint consumes.
///
/// Sections are `[name]` lines; entries are `key = value` or `key : value`;
/// `#` and `;` start comments. Only the `[CLI]` section is interpreted.
pub fn parse_manim_cfg(text: &str) -> Result<ManimCfgValues, String> {
    let mut values = ManimCfgValues::default();
    let mut in_cli_section = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_cli_section = section.trim().eq_ignore_ascii_case("cli");
            continue;
        }
        if !in_cli_section {
            continue;
        }
        let Some((key, value)) = split_ini_entry(line) else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "pixel_width" => {
                values.pixel_width = Some(parse_number(&value, "pixel_width")?);
            }
            "pixel_height" => {
                values.pixel_height = Some(parse_number(&value, "pixel_height")?);
            }
            "frame_rate" => {
                let parsed: f64 = value
                    .parse()
                    .map_err(|_| format!("invalid frame_rate value: {value}"))?;
                values.frame_rate = Some(parsed);
            }
            "renderer" => {
                values.renderer = Some(value.parse::<Renderer>()?);
            }
            _ => {}
        }
    }
    Ok(values)
}

fn parse_number(value: &str, key: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {key} value: {value}"))
}

fn split_ini_entry(line: &str) -> Option<(String, String)> {
    let separator = line.find(['=', ':'])?;
    let key = line[..separator].trim().to_owned();
    let mut value = line[separator + 1..].trim();
    // Strip trailing inline comments.
    if let Some(comment) = value.find(['#', ';']) {
        value = value[..comment].trim();
    }
    if key.is_empty() {
        return None;
    }
    Some((key, value.to_owned()))
}

/// Everything needed to resolve the effective configuration for one run.
#[derive(Debug, Clone, Default)]
pub struct ResolutionInput {
    /// Absolute project root.
    pub project_root: PathBuf,
    /// CLI-provided overrides (highest precedence).
    pub cli: ConfigFragment,
    /// Parsed `[tool.manim-lint]`, when a pyproject was found.
    pub pyproject: Option<PyprojectSection>,
    /// Parsed `manim.cfg`, when present.
    pub manim_cfg: Option<ManimCfgValues>,
    /// Profile selection from `--profile`.
    pub profile_selection: ProfileSelection,
}

/// Applies the full precedence chain and validates the result.
pub fn resolve(input: &ResolutionInput) -> Result<ResolvedConfig, ConfigError> {
    let defaults = ConfigFragment::builtin_defaults();
    let pyproject = input.pyproject.clone().unwrap_or_default();
    let base = pyproject.base_fragment();

    // manim.cfg participates only when respect-manim-cfg resolves true.
    let respect_manim_cfg = ConfigFragment::merge(&[&input.cli, &base, &defaults])
        .respect_manim_cfg
        .unwrap_or(true);
    let manim_cfg = if respect_manim_cfg {
        input.manim_cfg.clone().unwrap_or_default().fragment()
    } else {
        ConfigFragment::default()
    };

    validate_profiles(&pyproject.profile)?;

    // Lint-level settings resolve without a profile tier.
    let empty = ConfigFragment::default();
    let lint = ConfigFragment::merge(&[&input.cli, &empty, &base, &manim_cfg, &defaults]);

    let select = lint.select.clone().unwrap_or_default();
    let ignore = lint.ignore.clone().unwrap_or_default();
    let per_file_ignores = lint.per_file_ignores.clone().unwrap_or_default();
    validate_selectors(&select, "select")?;
    validate_selectors(&ignore, "ignore")?;
    for (pattern, selectors) in &per_file_ignores {
        validate_selectors(selectors, &format!("per-file-ignores[{pattern}]"))?;
    }

    let (default_profile, all_profile_names, active) =
        select_profiles(&pyproject, &lint, &input.profile_selection)?;

    let active_profiles = active
        .iter()
        .map(|section| resolve_profile(section, &input.cli, &base, &manim_cfg, &defaults))
        .collect();

    Ok(ResolvedConfig {
        project_root: input.project_root.clone(),
        manim_version: lint.manim_version.unwrap_or_default(),
        target_python: lint.target_python.unwrap_or_default(),
        select,
        ignore,
        min_confidence: lint.min_confidence.unwrap_or(Confidence::High),
        fail_level: lint.fail_level.unwrap_or(Severity::Warning),
        knowledge_profile: lint.knowledge_profile,
        respect_manim_cfg,
        exclude: lint.exclude.unwrap_or_default(),
        per_file_ignores,
        source_roots: lint.source_roots.unwrap_or_default(),
        stub_paths: lint.stub_paths.unwrap_or_default(),
        default_profile,
        all_profile_names,
        active_profiles,
    })
}

fn validate_profiles(profiles: &[ProfileSection]) -> Result<(), ConfigError> {
    let mut seen = std::collections::BTreeSet::new();
    for profile in profiles {
        if !seen.insert(profile.name.as_str()) {
            return Err(ConfigError::DuplicateProfile(profile.name.clone()));
        }
    }
    Ok(())
}

fn validate_selectors(selectors: &[String], source: &str) -> Result<(), ConfigError> {
    registry::validate_selectors(selectors, source).map_err(ConfigError::UnknownSelector)
}

/// Decides which profile sections this run analyzes.
///
/// Returns `(default profile name, all profile names, active sections)`.
fn select_profiles(
    pyproject: &PyprojectSection,
    lint: &ConfigFragment,
    selection: &ProfileSelection,
) -> Result<(String, Vec<String>, Vec<ProfileSection>), ConfigError> {
    let builtin_profile = ProfileSection {
        name: "default".to_owned(),
        renderer: None,
        platform: None,
        working_directory: None,
        pixel_width: None,
        pixel_height: None,
        frame_rate: None,
        assets_dir: None,
        allowed_fonts: None,
        cairo_fork_workers: None,
        cairo_static_layers: None,
        video_encoder: None,
        transparent: None,
        antialias: None,
        opengl_readback: None,
    };

    let configured = &pyproject.profile;
    let (profiles, default_name): (Vec<ProfileSection>, String) = if configured.is_empty() {
        // With no configured profiles a synthesized "default" exists; naming
        // any other default is a config error.
        match &pyproject.default_profile {
            Some(name) if name != "default" => {
                return Err(ConfigError::UnknownProfile(name.clone()));
            }
            _ => {}
        }
        (vec![builtin_profile], "default".to_owned())
    } else {
        let Some(default_name) = lint.default_profile.clone() else {
            return Err(ConfigError::MissingDefaultProfile);
        };
        if pyproject.default_profile.is_none() && !matches!(selection, ProfileSelection::Named(_)) {
            // `default-profile` must be written in pyproject when profiles
            // are configured; the builtin "default" name never matches them.
            if !configured
                .iter()
                .any(|profile| profile.name == default_name)
            {
                return Err(ConfigError::MissingDefaultProfile);
            }
        }
        if !configured
            .iter()
            .any(|profile| profile.name == default_name)
        {
            return Err(ConfigError::UnknownProfile(default_name));
        }
        (configured.clone(), default_name)
    };

    let all_names: Vec<String> = profiles
        .iter()
        .map(|profile| profile.name.clone())
        .collect();
    let active = match selection {
        ProfileSelection::Default => profiles
            .iter()
            .filter(|profile| profile.name == default_name)
            .cloned()
            .collect(),
        ProfileSelection::Named(name) => {
            let matched: Vec<ProfileSection> = profiles
                .iter()
                .filter(|profile| &profile.name == name)
                .cloned()
                .collect();
            if matched.is_empty() {
                return Err(ConfigError::UnknownProfile(name.clone()));
            }
            matched
        }
        ProfileSelection::All => profiles,
    };
    Ok((default_name, all_names, active))
}

fn resolve_profile(
    section: &ProfileSection,
    cli: &ConfigFragment,
    base: &ConfigFragment,
    manim_cfg: &ConfigFragment,
    defaults: &ConfigFragment,
) -> RenderProfile {
    let profile = section.fragment();
    let merged = ConfigFragment::merge(&[cli, &profile, base, manim_cfg, defaults]);
    RenderProfile {
        name: section.name.clone(),
        renderer: merged.renderer.unwrap_or(Renderer::Cairo),
        platform: merged.platform.unwrap_or(Platform::Linux),
        working_directory: merged.working_directory.unwrap_or_else(|| ".".to_owned()),
        pixel_width: merged.pixel_width.unwrap_or(1920),
        pixel_height: merged.pixel_height.unwrap_or(1080),
        frame_rate: merged.frame_rate.unwrap_or(60.0),
        assets_dir: merged.assets_dir.unwrap_or_else(|| ".".to_owned()),
        allowed_fonts: merged.allowed_fonts.unwrap_or_default(),
        cairo_fork_workers: merged.cairo_fork_workers.unwrap_or(0),
        cairo_static_layers: merged.cairo_static_layers.unwrap_or(false),
        video_encoder: merged.video_encoder.unwrap_or_else(|| "libx264".to_owned()),
        transparent: merged.transparent.unwrap_or(false),
        antialias: merged.antialias.unwrap_or_else(|| "default".to_owned()),
        opengl_readback: merged.opengl_readback.unwrap_or_else(|| "auto".to_owned()),
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "tests compare exact literal frame rates that pass through unchanged"
)]
mod tests {
    use super::*;

    const PYPROJECT: &str = r#"
[tool.manim-lint]
select = ["MLC", "MLR"]
min-confidence = "medium"
fail-level = "error"
default-profile = "production"
exclude = ["media/**"]

[[tool.manim-lint.profile]]
name = "production"
renderer = "cairo"
frame-rate = 30
pixel-width = 3840
pixel-height = 2160

[[tool.manim-lint.profile]]
name = "preview"
renderer = "opengl"
"#;

    fn input_with(pyproject: &str) -> ResolutionInput {
        ResolutionInput {
            project_root: PathBuf::from("/project"),
            pyproject: parse_pyproject(pyproject).expect("valid pyproject"),
            ..ResolutionInput::default()
        }
    }

    #[test]
    fn resolves_default_profile_from_pyproject() {
        let config = resolve(&input_with(PYPROJECT)).expect("resolves");
        assert_eq!(config.default_profile, "production");
        assert_eq!(config.active_profiles.len(), 1);
        let profile = &config.active_profiles[0];
        assert_eq!(profile.name, "production");
        assert_eq!(profile.frame_rate, 30.0);
        assert_eq!(profile.pixel_width, 3840);
        assert_eq!(config.min_confidence, Confidence::Medium);
        assert_eq!(config.fail_level, Severity::Error);
    }

    #[test]
    fn cli_overrides_selected_profile() {
        let mut input = input_with(PYPROJECT);
        input.cli.frame_rate = Some(24.0);
        input.cli.renderer = Some(Renderer::Opengl);
        let config = resolve(&input).expect("resolves");
        let profile = &config.active_profiles[0];
        assert_eq!(profile.frame_rate, 24.0);
        assert_eq!(profile.renderer, Renderer::Opengl);
    }

    #[test]
    fn manim_cfg_fills_gaps_but_profile_wins() {
        let mut input = input_with(PYPROJECT);
        input.manim_cfg = Some(ManimCfgValues {
            pixel_width: Some(854),
            pixel_height: Some(480),
            frame_rate: Some(15.0),
            renderer: None,
        });
        // "preview" defines renderer only; manim.cfg provides resolution/fps.
        input.profile_selection = ProfileSelection::Named("preview".to_owned());
        let config = resolve(&input).expect("resolves");
        let profile = &config.active_profiles[0];
        assert_eq!(profile.renderer, Renderer::Opengl, "profile value kept");
        assert_eq!(profile.pixel_width, 854, "manim.cfg fills the gap");
        assert_eq!(profile.frame_rate, 15.0);
    }

    #[test]
    fn respect_manim_cfg_false_ignores_manim_cfg() {
        let pyproject = "[tool.manim-lint]\nrespect-manim-cfg = false\n";
        let mut input = input_with(pyproject);
        input.manim_cfg = Some(ManimCfgValues {
            frame_rate: Some(15.0),
            ..ManimCfgValues::default()
        });
        let config = resolve(&input).expect("resolves");
        assert_eq!(config.active_profiles[0].frame_rate, 60.0);
    }

    #[test]
    fn profile_all_selects_every_profile() {
        let mut input = input_with(PYPROJECT);
        input.profile_selection = ProfileSelection::All;
        let config = resolve(&input).expect("resolves");
        assert_eq!(config.active_profile_names(), vec!["production", "preview"]);
    }

    #[test]
    fn unknown_selector_is_a_config_error() {
        let pyproject = "[tool.manim-lint]\nselect = [\"MLX999\"]\n";
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::UnknownSelector(_))
        ));
    }

    #[test]
    fn duplicate_profile_names_are_rejected() {
        let pyproject = r#"
[tool.manim-lint]
default-profile = "a"
[[tool.manim-lint.profile]]
name = "a"
[[tool.manim-lint.profile]]
name = "a"
"#;
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::DuplicateProfile(_))
        ));
    }

    #[test]
    fn missing_default_profile_is_rejected() {
        let pyproject = "[[tool.manim-lint.profile]]\nname = \"a\"\n";
        let full = format!("[tool.manim-lint]\n{pyproject}");
        assert!(matches!(
            resolve(&input_with(&full)),
            Err(ConfigError::MissingDefaultProfile)
        ));
    }

    #[test]
    fn unknown_default_profile_is_rejected() {
        let pyproject = r#"
[tool.manim-lint]
default-profile = "missing"
[[tool.manim-lint.profile]]
name = "a"
"#;
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::UnknownProfile(_))
        ));
    }

    #[test]
    fn unknown_cli_profile_is_rejected() {
        let mut input = input_with(PYPROJECT);
        input.profile_selection = ProfileSelection::Named("nope".to_owned());
        assert!(matches!(
            resolve(&input),
            Err(ConfigError::UnknownProfile(_))
        ));
    }

    #[test]
    fn unknown_pyproject_key_is_rejected() {
        let pyproject = "[tool.manim-lint]\nnot-a-key = 1\n";
        assert!(parse_pyproject(pyproject).is_err());
    }

    #[test]
    fn no_pyproject_synthesizes_builtin_default_profile() {
        let input = ResolutionInput {
            project_root: PathBuf::from("/project"),
            ..ResolutionInput::default()
        };
        let config = resolve(&input).expect("resolves");
        assert_eq!(config.default_profile, "default");
        assert_eq!(config.active_profiles.len(), 1);
        assert_eq!(config.active_profiles[0].renderer, Renderer::Cairo);
    }

    #[test]
    fn parses_minimal_manim_cfg() {
        let cfg = "\n[CLI]\npixel_width = 1280 # comment\npixel_height: 720\nframe_rate = 24\nrenderer = opengl\n\n[other]\npixel_width = 1\n";
        let values = parse_manim_cfg(cfg).expect("parses");
        assert_eq!(values.pixel_width, Some(1280));
        assert_eq!(values.pixel_height, Some(720));
        assert_eq!(values.frame_rate, Some(24.0));
        assert_eq!(values.renderer, Some(Renderer::Opengl));
    }

    #[test]
    fn invalid_manim_cfg_number_is_an_error() {
        assert!(parse_manim_cfg("[CLI]\npixel_width = wide\n").is_err());
    }
}
