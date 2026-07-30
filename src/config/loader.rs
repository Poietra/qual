//! Configuration discovery, parsing, and validation (DESIGN §8.2).
//!
//! Reads `[tool.qual]` from `pyproject.toml`, optionally a minimal
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
    /// `manim.cfg` contains a value qual cannot interpret.
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
    /// A CLI value could not be interpreted, or a configured value
    /// failed validation.
    #[error("{0}")]
    InvalidValue(String),
    /// `stub-paths` is configured but stub loading does not exist yet;
    /// silently ignoring the paths would give false assurance.
    #[error(
        "stub-paths is not implemented yet: the configured paths would be \
         silently ignored; remove `stub-paths` from the configuration"
    )]
    StubPathsUnimplemented,
    /// The declared `manim-version` is outside the loaded knowledge
    /// profile's supported Manim range.
    #[error(
        "manim-version {declared} is outside the range {range} supported by \
         knowledge profile {profile}; pick a matching knowledge-profile or \
         fix manim-version"
    )]
    ManimVersionOutOfRange {
        /// `manim-version` as declared in configuration.
        declared: String,
        /// Name of the loaded knowledge profile.
        profile: String,
        /// The profile's supported Manim version range.
        range: String,
    },
}

/// Highest Python grammar the bundled parser actually implements.
///
/// Verified against the vendored `python.lalrpop` of rustpython-parser
/// 0.4.0: `match` statements (3.10), `except*` (3.11), and PEP 695 `type`
/// aliases / type-parameter lists (3.12) are all grammar rules, but PEP 696
/// type-parameter defaults (`class C[T = int]`, new in Python 3.13) are
/// absent — `TypeParam` has no default clause — so 3.12 is the newest
/// grammar the parser implements. A newer `target-python` must be an
/// explicit configuration error (DESIGN §5.2), never a silent downgrade.
pub const MAX_TARGET_PYTHON: (u32, u32) = (3, 12);

/// Lowest `target-python` whose syntax gating is complete: the post-parse
/// gate ([`crate::frontend::features`]) detects every parse-level
/// construct introduced after Python 3.6 that the bundled 3.12 grammar
/// accepts, so a 3.6+ target that passes the gate silently is guaranteed
/// parseable by that target. Below 3.6 the guarantee would break —
/// Python 3.6 added constructs (f-strings, signature trailing commas)
/// that `CPython`'s own `ast.parse(feature_version=...)` oracle does not
/// gate — so an older target is an explicit configuration error, never a
/// silently unenforced promise.
pub const MIN_TARGET_PYTHON: (u32, u32) = (3, 6);

/// Validates the `target-python` format (`X.Y`) and parser bounds.
///
/// The bundled parser has no `feature_version` pinning: `target-python`
/// only gates acceptance, it never changes how sources are parsed.
fn validate_target_python(value: &str) -> Result<(), ConfigError> {
    let parsed = value.split_once('.').and_then(|(major, minor)| {
        let major: u32 = parse_ascii_number(major)?;
        let minor: u32 = parse_ascii_number(minor)?;
        Some((major, minor))
    });
    let Some(version) = parsed else {
        return Err(ConfigError::InvalidValue(format!(
            "invalid target-python {value:?}: expected MAJOR.MINOR, e.g. \"3.11\""
        )));
    };
    if version < MIN_TARGET_PYTHON {
        return Err(ConfigError::InvalidValue(format!(
            "cannot guarantee syntax gating for Python {value}; minimum \
             enforceable target-python is {major}.{minor}",
            major = MIN_TARGET_PYTHON.0,
            minor = MIN_TARGET_PYTHON.1,
        )));
    }
    if version > MAX_TARGET_PYTHON {
        return Err(ConfigError::InvalidValue(format!(
            "target-python {value} is newer than the Python grammar bundled \
             with qual (rustpython-parser 0.4 implements the Python \
             {major}.{minor} grammar); lower target-python or use a \
             qual build with a newer parser",
            major = MAX_TARGET_PYTHON.0,
            minor = MAX_TARGET_PYTHON.1,
        )));
    }
    Ok(())
}

/// Parses a plain ASCII decimal number (no sign, no leading `+`).
fn parse_ascii_number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A dotted numeric version, padded to three components (`0.20` reads as
/// `0.20.0`). The semver-ish subset both `manim-version` values and
/// knowledge-profile ranges use; no external dependency.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = [0_u64; 3];
    let mut count = 0;
    for piece in text.trim().split('.') {
        if count == parts.len() || piece.is_empty() {
            return None;
        }
        if !piece.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        parts[count] = piece.parse().ok()?;
        count += 1;
    }
    (count >= 1).then_some(parts)
}

/// Evaluates `version` against a comma-separated comparator list
/// (`>=0.20,<0.21`). A bare version means exact equality.
///
/// Returns `None` when the range spec itself cannot be parsed: an
/// unintelligible profile range is informational, and inventing an error
/// from it would be a fabricated finding (DESIGN §15).
fn version_in_range(version: [u64; 3], range: &str) -> Option<bool> {
    let mut satisfied = true;
    for comparator in range.split(',') {
        let comparator = comparator.trim();
        let (operator, bound_text) = ["<=", ">=", "==", "<", ">", "="]
            .iter()
            .find_map(|prefix| {
                comparator
                    .strip_prefix(prefix)
                    .map(|rest| (*prefix, rest.trim()))
            })
            .unwrap_or(("==", comparator));
        let bound = parse_version(bound_text)?;
        satisfied &= match operator {
            "<" => version < bound,
            "<=" => version <= bound,
            ">" => version > bound,
            ">=" => version >= bound,
            _ => version == bound,
        };
    }
    Some(satisfied)
}

/// Validates a declared `manim-version` against a knowledge profile's
/// supported Manim range (DESIGN §8.2 honesty: a declared version the
/// loaded profile does not cover must be an explicit exit-2 error, not a
/// silently ignored setting).
pub fn validate_manim_version(
    declared: &str,
    profile: &str,
    range: &str,
) -> Result<(), ConfigError> {
    let Some(version) = parse_version(declared) else {
        return Err(ConfigError::InvalidValue(format!(
            "invalid manim-version {declared:?}: expected a dotted numeric \
             version, e.g. \"0.20\""
        )));
    };
    match version_in_range(version, range) {
        Some(false) => Err(ConfigError::ManimVersionOutOfRange {
            declared: declared.to_owned(),
            profile: profile.to_owned(),
            range: range.to_owned(),
        }),
        // `None`: the profile's range spec is unparsable, so the declared
        // version cannot be checked against it — informational only.
        Some(true) | None => Ok(()),
    }
}

/// Rejects resolved profile numbers no analysis can honor: a non-positive
/// or non-finite frame rate and a zero pixel dimension would silently
/// produce meaningless frame math, so they are configuration errors
/// regardless of which tier (CLI flag, profile, `manim.cfg`) supplied them.
fn validate_profile_numbers(profile: &RenderProfile) -> Result<(), ConfigError> {
    if !profile.frame_rate.is_finite() || profile.frame_rate <= 0.0 {
        return Err(ConfigError::InvalidValue(format!(
            "frame rate must be a positive finite number; profile \
             \"{name}\" resolved to {value}",
            name = profile.name,
            value = profile.frame_rate,
        )));
    }
    if profile.pixel_width == 0 || profile.pixel_height == 0 {
        return Err(ConfigError::InvalidValue(format!(
            "resolution dimensions must be nonzero; profile \"{name}\" \
             resolved to {width}x{height}",
            name = profile.name,
            width = profile.pixel_width,
            height = profile.pixel_height,
        )));
    }
    Ok(())
}

/// `[tool.qual]` exactly as written in `pyproject.toml`.
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

/// One `[[tool.qual.profile]]` entry as written.
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

/// Parses `[tool.qual]` from a `pyproject.toml` file.
///
/// Returns `Ok(None)` when the file has no `[tool.qual]` table.
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

/// Parses `[tool.qual]` from pyproject TOML text.
pub fn parse_pyproject(text: &str) -> Result<Option<PyprojectSection>, String> {
    #[derive(Deserialize)]
    struct PyprojectFile {
        tool: Option<ToolTable>,
    }
    #[derive(Deserialize)]
    struct ToolTable {
        #[serde(rename = "qual")]
        qual: Option<PyprojectSection>,
    }

    let file: PyprojectFile = toml::from_str(text).map_err(|error| error.to_string())?;
    Ok(file.tool.and_then(|tool| tool.qual))
}

/// A named Manim quality preset from `manim.constants.QUALITIES`.
///
/// Mirrors the upstream table verbatim; see [`ManimQuality::from_cfg_value`]
/// for how a `manim.cfg` string maps onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManimQuality {
    /// `fourk_quality` — 3840x2160 @ 60.
    FourK,
    /// `production_quality` — 2560x1440 @ 60.
    Production,
    /// `high_quality` — 1920x1080 @ 60.
    High,
    /// `medium_quality` — 1280x720 @ 30.
    Medium,
    /// `low_quality` — 854x480 @ 15.
    Low,
    /// `example_quality` — 854x480 @ 30. Has no CLI flag upstream.
    Example,
}

/// One row of the upstream `QUALITIES` table.
struct QualityRow {
    quality: ManimQuality,
    /// Preset name, e.g. `fourk_quality`.
    name: &'static str,
    /// Single-letter `-q` flag; `None` upstream for `example_quality`.
    flag: Option<&'static str>,
    pixel_width: u32,
    pixel_height: u32,
    frame_rate: f64,
}

/// The upstream `QUALITIES` table.
///
/// Transcribed from Manim Community `manim/constants.py` `QUALITIES`.
/// `example_quality` carries `"flag": None` upstream and is therefore
/// reachable only by its full name.
const MANIM_QUALITIES: &[QualityRow] = &[
    QualityRow {
        quality: ManimQuality::FourK,
        name: "fourk_quality",
        flag: Some("k"),
        pixel_width: 3840,
        pixel_height: 2160,
        frame_rate: 60.0,
    },
    QualityRow {
        quality: ManimQuality::Production,
        name: "production_quality",
        flag: Some("p"),
        pixel_width: 2560,
        pixel_height: 1440,
        frame_rate: 60.0,
    },
    QualityRow {
        quality: ManimQuality::High,
        name: "high_quality",
        flag: Some("h"),
        pixel_width: 1920,
        pixel_height: 1080,
        frame_rate: 60.0,
    },
    QualityRow {
        quality: ManimQuality::Medium,
        name: "medium_quality",
        flag: Some("m"),
        pixel_width: 1280,
        pixel_height: 720,
        frame_rate: 30.0,
    },
    QualityRow {
        quality: ManimQuality::Low,
        name: "low_quality",
        flag: Some("l"),
        pixel_width: 854,
        pixel_height: 480,
        frame_rate: 15.0,
    },
    QualityRow {
        quality: ManimQuality::Example,
        name: "example_quality",
        flag: None,
        pixel_width: 854,
        pixel_height: 480,
        frame_rate: 30.0,
    },
];

impl ManimQuality {
    /// Resolves a `manim.cfg` `quality` value.
    ///
    /// Upstream `_determine_quality` (`manim/_config/utils.py`) accepts the
    /// single-letter flag first and otherwise passes the string through to
    /// the `quality` setter, which raises `KeyError` unless it names a
    /// preset. Both spellings are accepted here; anything else is an error,
    /// because Manim would refuse to run at all.
    fn from_cfg_value(value: &str) -> Result<Self, String> {
        let trimmed = value.trim();
        if let Some(row) = MANIM_QUALITIES.iter().find(|row| row.flag == Some(trimmed)) {
            return Ok(row.quality);
        }
        MANIM_QUALITIES
            .iter()
            .find(|row| row.name == trimmed)
            .map(|row| row.quality)
            .ok_or_else(|| {
                let names: Vec<&str> = MANIM_QUALITIES.iter().map(|row| row.name).collect();
                format!(
                    "invalid quality value: {trimmed} (expected one of {})",
                    names.join(", ")
                )
            })
    }

    fn row(self) -> &'static QualityRow {
        MANIM_QUALITIES
            .iter()
            .find(|row| row.quality == self)
            .expect("every ManimQuality variant is present in MANIM_QUALITIES")
    }

    /// The preset's canonical Manim name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.row().name
    }

    /// Pixel width, height, and frame rate this preset forces.
    #[must_use]
    pub fn profile(self) -> (u32, u32, f64) {
        let row = self.row();
        (row.pixel_width, row.pixel_height, row.frame_rate)
    }
}

/// `[CLI]` keys that change the analyzed render profile or the frame count
/// it is multiplied by, and that qual does not interpret.
///
/// Present so a project that sets one is told its configuration was
/// dropped rather than being handed a confident number derived from a
/// profile qual never saw (DESIGN §8.2 honesty).
const UNINTERPRETED_PROFILE_KEYS: &[&str] = &[
    "resolution",
    "frame_size",
    "from_animation_number",
    "upto_animation_number",
    "save_last_frame",
    "dry_run",
    "transparent",
    "format",
];

/// Values qual reads from `manim.cfg` (resolution, fps, renderer).
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
    /// `quality` from the `[CLI]` section.
    ///
    /// Overrides the three keys above, matching Manim: `digest_parser`
    /// applies `quality` *after* the int and float keys, and the `quality`
    /// setter assigns `frame_size` and `frame_rate` unconditionally.
    pub quality: Option<ManimQuality>,
    /// Human-readable notes about `[CLI]` content qual did not honor.
    pub warnings: Vec<String>,
}

impl ManimCfgValues {
    fn fragment(&self) -> ConfigFragment {
        let mut fragment = ConfigFragment {
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
            frame_rate: self.frame_rate,
            renderer: self.renderer,
            ..ConfigFragment::default()
        };
        // Manim applies `quality` last and lets it win; so do we.
        if let Some(quality) = self.quality {
            let (width, height, frame_rate) = quality.profile();
            fragment.pixel_width = Some(width);
            fragment.pixel_height = Some(height);
            fragment.frame_rate = Some(frame_rate);
        }
        fragment
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

/// Parses the minimal INI subset of `manim.cfg` that qual consumes.
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
            "quality" => {
                values.quality = Some(ManimQuality::from_cfg_value(&value)?);
            }
            other => {
                if UNINTERPRETED_PROFILE_KEYS.contains(&other) {
                    values.warnings.push(format!(
                        "manim.cfg [CLI] {other} affects the render profile but \
                         qual does not interpret it; the analyzed profile ignores it"
                    ));
                }
            }
        }
    }
    // Manim resolves `quality` after the explicit keys and overwrites them.
    // Say so rather than silently discarding what the file asked for.
    if let Some(quality) = values.quality {
        let overridden: Vec<&str> = [
            values.pixel_width.map(|_| "pixel_width"),
            values.pixel_height.map(|_| "pixel_height"),
            values.frame_rate.map(|_| "frame_rate"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !overridden.is_empty() {
            values.warnings.push(format!(
                "manim.cfg [CLI] quality = {name} overrides {keys} in the same \
                 file, matching Manim (digest_parser applies quality last)",
                name = quality.name(),
                keys = overridden.join(", "),
            ));
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
    /// Parsed `[tool.qual]`, when a pyproject was found.
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
    let (manim_cfg, manim_cfg_warnings) = if respect_manim_cfg {
        let values = input.manim_cfg.clone().unwrap_or_default();
        (values.fragment(), values.warnings)
    } else {
        (ConfigFragment::default(), Vec::new())
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

    let active_profiles: Vec<RenderProfile> = active
        .iter()
        .map(|section| resolve_profile(section, &input.cli, &base, &manim_cfg, &defaults))
        .collect();
    for profile in &active_profiles {
        validate_profile_numbers(profile)?;
    }

    // Honesty checks (DESIGN §8.2): settings that cannot be honored are
    // explicit errors, never silently accepted.
    let stub_paths = lint.stub_paths.unwrap_or_default();
    if !stub_paths.is_empty() {
        return Err(ConfigError::StubPathsUnimplemented);
    }
    let target_python = lint.target_python.unwrap_or_default();
    validate_target_python(&target_python)?;
    // A declared manim-version must at least be a parseable version here;
    // the range check against the loaded knowledge profile happens in the
    // application layer, which owns profile loading.
    let declared_manim_version = base.manim_version.clone();
    if let Some(declared) = &declared_manim_version {
        if parse_version(declared).is_none() {
            return Err(ConfigError::InvalidValue(format!(
                "invalid manim-version {declared:?}: expected a dotted \
                 numeric version, e.g. \"0.20\""
            )));
        }
    }

    Ok(ResolvedConfig {
        project_root: input.project_root.clone(),
        manim_version: lint.manim_version.unwrap_or_default(),
        declared_manim_version,
        target_python,
        select,
        ignore,
        min_confidence: lint.min_confidence.unwrap_or(Confidence::High),
        fail_level: lint.fail_level.unwrap_or(Severity::Warning),
        knowledge_profile: lint.knowledge_profile,
        respect_manim_cfg,
        manim_cfg_warnings,
        exclude: lint.exclude.unwrap_or_default(),
        per_file_ignores,
        source_roots: lint.source_roots.unwrap_or_default(),
        stub_paths,
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
[tool.qual]
select = ["MLC", "MLR"]
min-confidence = "medium"
fail-level = "error"
default-profile = "production"
exclude = ["media/**"]

[[tool.qual.profile]]
name = "production"
renderer = "cairo"
frame-rate = 30
pixel-width = 3840
pixel-height = 2160

[[tool.qual.profile]]
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
            ..ManimCfgValues::default()
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
        let pyproject = "[tool.qual]\nrespect-manim-cfg = false\n";
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
        let pyproject = "[tool.qual]\nselect = [\"MLX999\"]\n";
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::UnknownSelector(_))
        ));
    }

    #[test]
    fn duplicate_profile_names_are_rejected() {
        let pyproject = r#"
[tool.qual]
default-profile = "a"
[[tool.qual.profile]]
name = "a"
[[tool.qual.profile]]
name = "a"
"#;
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::DuplicateProfile(_))
        ));
    }

    #[test]
    fn missing_default_profile_is_rejected() {
        let pyproject = "[[tool.qual.profile]]\nname = \"a\"\n";
        let full = format!("[tool.qual]\n{pyproject}");
        assert!(matches!(
            resolve(&input_with(&full)),
            Err(ConfigError::MissingDefaultProfile)
        ));
    }

    #[test]
    fn unknown_default_profile_is_rejected() {
        let pyproject = r#"
[tool.qual]
default-profile = "missing"
[[tool.qual.profile]]
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
        let pyproject = "[tool.qual]\nnot-a-key = 1\n";
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

    // --- manim.cfg `quality` (#8) ---------------------------------------
    //
    // `quality` is the common way to set resolution and frame rate; before
    // this it fell through the catch-all arm and every cost estimate was
    // computed against 1920x1080/60 while `respect_manim_cfg: true` was
    // reported. The table and the precedence below are transcribed from
    // Manim Community, not invented:
    //
    //   * `manim/constants.py` `QUALITIES` — the five named presets plus
    //     `example_quality`, each with a `flag`, `pixel_width`,
    //     `pixel_height`, and `frame_rate`.
    //   * `manim/_config/utils.py` `ManimConfig.digest_parser` — reads the
    //     int keys (`pixel_width`, `pixel_height`) and float keys
    //     (`frame_rate`) first, then near the end of the method does
    //     `quality = parser["CLI"].get("quality", ...)` /
    //     `if quality: self.quality = _determine_quality(quality)`.
    //   * `manim/_config/utils.py` `ManimConfig.quality` setter — assigns
    //     `self.frame_size` and `self.frame_rate` from the preset
    //     unconditionally.
    //
    // Applying `quality` last and assigning unconditionally is what makes
    // `quality` win over explicit keys in the same file.

    #[test]
    fn manim_cfg_quality_names_match_upstream_qualities_table() {
        let expected = [
            ("fourk_quality", 3840, 2160, 60.0),
            ("production_quality", 2560, 1440, 60.0),
            ("high_quality", 1920, 1080, 60.0),
            ("medium_quality", 1280, 720, 30.0),
            ("low_quality", 854, 480, 15.0),
            ("example_quality", 854, 480, 30.0),
        ];
        for (name, width, height, frame_rate) in expected {
            let values = parse_manim_cfg(&format!("[CLI]\nquality = {name}\n")).expect("parses");
            let quality = values.quality.expect("quality recognized");
            assert_eq!(quality.name(), name);
            assert_eq!(quality.profile(), (width, height, frame_rate), "{name}");
        }
    }

    #[test]
    fn manim_cfg_quality_accepts_the_single_letter_flags() {
        // `_determine_quality` matches `values["flag"]` before falling
        // through to the preset name.
        for (flag, name) in [
            ("k", "fourk_quality"),
            ("p", "production_quality"),
            ("h", "high_quality"),
            ("m", "medium_quality"),
            ("l", "low_quality"),
        ] {
            let values = parse_manim_cfg(&format!("[CLI]\nquality = {flag}\n")).expect("parses");
            assert_eq!(values.quality.expect("recognized").name(), name);
        }
        // `example_quality` carries `"flag": None` upstream, so no flag
        // resolves to it and a literal "None" is not a quality.
        assert!(parse_manim_cfg("[CLI]\nquality = None\n").is_err());
    }

    #[test]
    fn manim_cfg_quality_only_produces_matching_resolution_and_frame_rate() {
        // Acceptance: a manim.cfg setting only `quality`.
        let mut input = ResolutionInput {
            project_root: PathBuf::from("/project"),
            ..ResolutionInput::default()
        };
        input.manim_cfg =
            Some(parse_manim_cfg("[CLI]\nquality = fourk_quality\n").expect("parses"));
        let config = resolve(&input).expect("resolves");
        let profile = &config.active_profiles[0];
        assert_eq!(profile.pixel_width, 3840);
        assert_eq!(profile.pixel_height, 2160);
        assert!((profile.frame_rate - 60.0).abs() < f64::EPSILON);
        assert!(config.respect_manim_cfg);
        assert!(
            config.manim_cfg_warnings.is_empty(),
            "an interpreted quality is not a warning: {:?}",
            config.manim_cfg_warnings
        );
    }

    #[test]
    fn manim_cfg_quality_beats_explicit_keys_in_the_same_file_like_manim() {
        // digest_parser applies quality after the int/float keys and the
        // setter assigns unconditionally, so quality wins.
        let values = parse_manim_cfg(
            "[CLI]\npixel_width = 1280\npixel_height = 720\nframe_rate = 24\nquality = low_quality\n",
        )
        .expect("parses");
        let mut input = ResolutionInput {
            project_root: PathBuf::from("/project"),
            ..ResolutionInput::default()
        };
        input.manim_cfg = Some(values);
        let config = resolve(&input).expect("resolves");
        let profile = &config.active_profiles[0];
        assert_eq!((profile.pixel_width, profile.pixel_height), (854, 480));
        assert!((profile.frame_rate - 15.0).abs() < f64::EPSILON);
        assert!(
            config
                .manim_cfg_warnings
                .iter()
                .any(|note| note.contains("quality = low_quality")
                    && note.contains("pixel_width")
                    && note.contains("frame_rate")),
            "the override is stated, not silent: {:?}",
            config.manim_cfg_warnings
        );
    }

    #[test]
    fn cli_and_pyproject_still_outrank_manim_cfg_quality() {
        // quality wins *within* manim.cfg; it does not change the outer
        // chain (CLI > profile > pyproject > manim.cfg > defaults).
        let mut input = input_with(PYPROJECT);
        input.manim_cfg = Some(parse_manim_cfg("[CLI]\nquality = low_quality\n").expect("parses"));
        input.cli.pixel_width = Some(3840);
        input.cli.pixel_height = Some(2160);
        let config = resolve(&input).expect("resolves");
        let profile = &config.active_profiles[0];
        assert_eq!((profile.pixel_width, profile.pixel_height), (3840, 2160));
        // fps was not overridden on the CLI, so manim.cfg's quality supplies it
        // unless the pyproject profile already set one.
        assert!(profile.frame_rate > 0.0);
    }

    #[test]
    fn invalid_manim_cfg_quality_is_an_error() {
        // Manim's `quality` setter raises KeyError, so the project would
        // not render at all; guessing a profile here would be worse.
        let error = parse_manim_cfg("[CLI]\nquality = ultra\n").expect_err("rejected");
        assert!(error.contains("ultra"), "{error}");
        assert!(error.contains("fourk_quality"), "{error}");
    }

    #[test]
    fn uninterpreted_profile_key_warns_instead_of_being_silent() {
        // Acceptance: an unrecognized profile-affecting key produces a
        // warning rather than silence.
        let values = parse_manim_cfg("[CLI]\nresolution = 1920,1080\n").expect("parses");
        assert_eq!(values.warnings.len(), 1, "{:?}", values.warnings);
        assert!(values.warnings[0].contains("resolution"));
        // A key qual has no opinion about stays silent — warning on
        // everything would be noise, not honesty.
        let quiet = parse_manim_cfg("[CLI]\nbackground_color = BLACK\n").expect("parses");
        assert!(quiet.warnings.is_empty(), "{:?}", quiet.warnings);
    }

    #[test]
    fn manim_cfg_warnings_are_dropped_when_the_file_is_not_consulted() {
        let mut input = input_with(PYPROJECT);
        input.manim_cfg = Some(parse_manim_cfg("[CLI]\nresolution = 1,1\n").expect("parses"));
        input.cli.respect_manim_cfg = Some(false);
        let config = resolve(&input).expect("resolves");
        assert!(!config.respect_manim_cfg);
        assert!(config.manim_cfg_warnings.is_empty());
    }

    // --- configuration honesty (review finding: accepted-but-ignored /
    // accepted-but-nonsensical values must be exit-2 config errors) ------

    #[test]
    fn cli_zero_or_non_finite_frame_rate_is_rejected() {
        for bad in [0.0, -24.0, f64::INFINITY, f64::NAN] {
            let mut input = input_with(PYPROJECT);
            input.cli.frame_rate = Some(bad);
            assert!(
                matches!(resolve(&input), Err(ConfigError::InvalidValue(_))),
                "fps {bad} must be a config error"
            );
        }
    }

    #[test]
    fn cli_zero_resolution_is_rejected() {
        let mut input = input_with(PYPROJECT);
        input.cli.pixel_width = Some(0);
        input.cli.pixel_height = Some(0);
        assert!(matches!(resolve(&input), Err(ConfigError::InvalidValue(_))));
    }

    #[test]
    fn profile_zero_frame_rate_is_rejected() {
        let pyproject = r#"
[tool.qual]
default-profile = "p"
[[tool.qual.profile]]
name = "p"
frame-rate = 0
"#;
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::InvalidValue(_))
        ));
    }

    #[test]
    fn profile_non_finite_frame_rate_is_rejected() {
        let pyproject = r#"
[tool.qual]
default-profile = "p"
[[tool.qual.profile]]
name = "p"
frame-rate = inf
"#;
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::InvalidValue(_))
        ));
    }

    #[test]
    fn profile_zero_pixel_dimension_is_rejected() {
        let pyproject = r#"
[tool.qual]
default-profile = "p"
[[tool.qual.profile]]
name = "p"
pixel-width = 0
"#;
        assert!(matches!(
            resolve(&input_with(pyproject)),
            Err(ConfigError::InvalidValue(_))
        ));
    }

    #[test]
    fn manim_cfg_zero_values_are_rejected() {
        let mut input = input_with("[tool.qual]\n");
        input.manim_cfg = Some(ManimCfgValues {
            frame_rate: Some(0.0),
            ..ManimCfgValues::default()
        });
        assert!(matches!(resolve(&input), Err(ConfigError::InvalidValue(_))));

        let mut input = input_with("[tool.qual]\n");
        input.manim_cfg = Some(ManimCfgValues {
            pixel_width: Some(0),
            ..ManimCfgValues::default()
        });
        assert!(matches!(resolve(&input), Err(ConfigError::InvalidValue(_))));
    }

    /// An invalid tier value that a higher-precedence tier overrides is
    /// never consulted, so it does not error: only resolved values are
    /// validated.
    #[test]
    fn overridden_invalid_manim_cfg_value_is_not_consulted() {
        let mut input = input_with(PYPROJECT);
        input.manim_cfg = Some(ManimCfgValues {
            frame_rate: Some(0.0),
            ..ManimCfgValues::default()
        });
        // The "production" profile sets frame-rate = 30, which wins.
        assert!(resolve(&input).is_ok());
    }

    #[test]
    fn non_empty_stub_paths_is_an_explicit_refusal() {
        let pyproject = "[tool.qual]\nstub-paths = [\"stubs\"]\n";
        let error = resolve(&input_with(pyproject)).expect_err("must refuse");
        assert!(matches!(error, ConfigError::StubPathsUnimplemented));
        assert!(
            error
                .to_string()
                .contains("stub-paths is not implemented yet")
        );
    }

    #[test]
    fn target_python_format_and_bounds_are_validated() {
        for good in ["3.6", "3.8", "3.11", "3.12"] {
            let pyproject = format!("[tool.qual]\ntarget-python = \"{good}\"\n");
            assert!(
                resolve(&input_with(&pyproject)).is_ok(),
                "{good} must be accepted"
            );
        }
        for bad in [
            "2.7", "3.0", "3.5", "3.13", "4.0", "3", "3.11.2", "py3", "3.x", "",
        ] {
            let pyproject = format!("[tool.qual]\ntarget-python = \"{bad}\"\n");
            assert!(
                matches!(
                    resolve(&input_with(&pyproject)),
                    Err(ConfigError::InvalidValue(_))
                ),
                "{bad:?} must be a config error"
            );
        }
    }

    #[test]
    fn target_python_below_the_gating_floor_names_the_floor() {
        let pyproject = "[tool.qual]\ntarget-python = \"3.5\"\n";
        let error = resolve(&input_with(pyproject)).expect_err("must refuse");
        assert_eq!(
            error.to_string(),
            "cannot guarantee syntax gating for Python 3.5; minimum \
             enforceable target-python is 3.6"
        );
    }

    #[test]
    fn target_python_newer_than_parser_grammar_names_the_bound() {
        let pyproject = "[tool.qual]\ntarget-python = \"3.13\"\n";
        let error = resolve(&input_with(pyproject)).expect_err("must refuse");
        let message = error.to_string();
        assert!(message.contains("3.13"), "{message}");
        assert!(message.contains("3.12"), "{message}");
    }

    #[test]
    fn declared_manim_version_is_tracked_and_format_checked() {
        let config = resolve(&input_with("[tool.qual]\nmanim-version = \"0.20.1\"\n")).unwrap();
        assert_eq!(config.declared_manim_version.as_deref(), Some("0.20.1"));

        let config = resolve(&input_with("[tool.qual]\n")).unwrap();
        assert_eq!(config.declared_manim_version, None);
        assert_eq!(config.manim_version, "0.20", "builtin default kept");

        assert!(matches!(
            resolve(&input_with("[tool.qual]\nmanim-version = \"latest\"\n")),
            Err(ConfigError::InvalidValue(_))
        ));
    }

    #[test]
    fn manim_version_range_validation() {
        assert!(validate_manim_version("0.20", "profile", ">=0.20,<0.21").is_ok());
        assert!(validate_manim_version("0.20.5", "profile", ">=0.20,<0.21").is_ok());
        let error = validate_manim_version("0.19", "upstream_0_20", ">=0.20,<0.21")
            .expect_err("below range");
        assert!(matches!(
            &error,
            ConfigError::ManimVersionOutOfRange { profile, range, .. }
                if profile == "upstream_0_20" && range == ">=0.20,<0.21"
        ));
        assert!(validate_manim_version("0.21", "profile", ">=0.20,<0.21").is_err());
        // Exact comparators, with and without an operator.
        assert!(validate_manim_version("0.20", "profile", "0.20").is_ok());
        assert!(validate_manim_version("0.20", "profile", "==0.20").is_ok());
        assert!(validate_manim_version("0.20.1", "profile", "=0.20").is_err());
        // An unparsable range cannot be enforced: accepted as informational.
        assert!(validate_manim_version("0.19", "profile", "whatever").is_ok());
        // An unparsable declared version is always an error.
        assert!(validate_manim_version("dev", "profile", ">=0.20").is_err());
    }

    #[test]
    fn version_parsing_is_strictly_numeric() {
        assert_eq!(parse_version("0.20"), Some([0, 20, 0]));
        assert_eq!(parse_version("0.20.1"), Some([0, 20, 1]));
        assert_eq!(parse_version(" 3 "), Some([3, 0, 0]));
        assert_eq!(parse_version("0.20.1.2"), None);
        assert_eq!(parse_version("0..1"), None);
        assert_eq!(parse_version("v0.20"), None);
        assert_eq!(parse_version(""), None);
    }
}
