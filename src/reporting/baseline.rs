//! Baseline files (DESIGN §8.3, Phase 5 pulled forward): record the
//! currently known diagnostics so an existing project can adopt the linter
//! incrementally.
//!
//! A fingerprint never contains line numbers. It is
//! `rule ID + relative path + qualified Scene name + surrounding token hash`.
//! The Scene name is the qualified name of the discovered Scene class whose
//! `class` statement encloses the diagnostic's primary span (resolved via
//! [`SceneSpans`] from the project index), or the empty string for
//! diagnostics outside every Scene. It distinguishes otherwise identical
//! findings — e.g. two Scene classes with token-identical `construct`
//! bodies in one file. The token hash covers the token kinds and texts of
//! the diagnostic statement line and its nearest non-blank neighbor line on
//! each side, so inserting unrelated lines elsewhere in the file does not
//! invalidate the entries.
//!
//! # Compatibility: the empty scene as a wildcard
//!
//! Baselines written before scene attribution existed store `scene: ""` for
//! every entry. So that those files keep suppressing, matching treats a
//! *stored* empty scene as a wildcard: it matches a diagnostic regardless
//! of its computed scene (an exact scene match is always consumed first).
//! A stored non-empty scene only matches that same computed scene.
//!
//! The on-disk format is JSON matching `schemas/baseline-v1.json`:
//! `schema_version` 1 plus a sorted entry list, serialized byte-stably.

use std::collections::BTreeMap;

use rustpython_parser::Tok;
use serde::{Deserialize, Serialize};

use crate::diagnostic::Diagnostic;
use crate::frontend::index::ProjectIndex;
use crate::source::{SourceFile, SourceManager};

/// Schema version written to and required from baseline files.
pub const SCHEMA_VERSION: u64 = 1;

/// One baseline fingerprint entry (`schemas/baseline-v1.json`).
///
/// The derived ordering (field declaration order) is the sort order of the
/// serialized entry list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BaselineEntry {
    /// Rule ID, e.g. `MLC101`.
    pub rule_id: String,
    /// Project-relative POSIX path of the diagnosed file.
    pub path: String,
    /// Qualified name of the discovered Scene class enclosing the
    /// diagnostic, or `""` outside every Scene. A *stored* `""` also acts
    /// as a wildcard when matching, for baselines written before scene
    /// attribution existed (module docs).
    pub scene: String,
    /// Hash of the surrounding tokens: `fnv1a64:` plus 16 hex digits.
    pub token_hash: String,
}

/// Scene attribution for fingerprints: per file, the line spans of every
/// discovered Scene class (DESIGN §8.3 "qualified Scene").
///
/// Built from the project index rather than lifecycle facts: the index
/// records every discovered Scene subclass with its file and `class`
/// statement span, whether or not its lifecycle could be interpreted. The
/// default (empty) attribution maps every diagnostic to the empty scene.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SceneSpans {
    /// Relative path → sorted `(start_line, end_line, qualified_name)` of
    /// its Scene class statements.
    per_file: BTreeMap<String, Vec<(usize, usize, String)>>,
}

impl SceneSpans {
    /// Resolves every discovered Scene class of the index to inclusive
    /// one-based line spans in its file.
    #[must_use]
    pub fn build(index: &ProjectIndex, sources: &SourceManager) -> Self {
        let mut per_file: BTreeMap<String, Vec<(usize, usize, String)>> = BTreeMap::new();
        for name in &index.scene_classes {
            let Some(record) = index.classes.get(name) else {
                continue;
            };
            let file = sources.file(record.file);
            let span = file.span_of_range(record.range);
            per_file
                .entry(file.relative_path().to_owned())
                .or_default()
                .push((
                    span.start.line,
                    span.end.line,
                    record.qualified_name.clone(),
                ));
        }
        for spans in per_file.values_mut() {
            spans.sort();
        }
        Self { per_file }
    }

    /// The qualified name of the innermost Scene class whose `class`
    /// statement spans `line` of `path`; `""` outside every Scene.
    fn scene_for(&self, path: &str, line: usize) -> &str {
        let Some(spans) = self.per_file.get(path) else {
            return "";
        };
        spans
            .iter()
            .filter(|(start, end, _)| (*start..=*end).contains(&line))
            .max_by_key(|(start, ..)| *start)
            .map_or("", |(_, _, name)| name.as_str())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BaselineDocument {
    schema_version: u64,
    entries: Vec<BaselineEntry>,
}

/// A parsed baseline used to filter already-known diagnostics.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Baseline {
    /// Fingerprint multiset: each entry hides at most one diagnostic.
    counts: BTreeMap<BaselineEntry, usize>,
}

impl Baseline {
    /// Parses and validates baseline JSON.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the text is not valid JSON for
    /// the baseline document shape or declares a different `schema_version`;
    /// callers map this to exit code 2.
    pub fn parse(text: &str) -> Result<Self, String> {
        let document: BaselineDocument = serde_json::from_str(text)
            .map_err(|error| format!("not a valid baseline file: {error}"))?;
        if document.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported baseline schema_version {} (this build reads version {SCHEMA_VERSION})",
                document.schema_version
            ));
        }
        let mut counts: BTreeMap<BaselineEntry, usize> = BTreeMap::new();
        for entry in document.entries {
            *counts.entry(entry).or_default() += 1;
        }
        Ok(Self { counts })
    }

    /// Number of fingerprint entries (duplicates included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.counts.values().sum()
    }

    /// Whether the baseline holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// Removes the diagnostics already recorded in this baseline, keeping
    /// the input order of the survivors.
    ///
    /// Each entry hides at most one diagnostic: duplicated code producing
    /// the same fingerprint twice needs two entries to hide both. An exact
    /// scene match is consumed first; a stored empty scene then matches
    /// any computed scene (wildcard compatibility with baselines written
    /// before scene attribution, see the module docs).
    #[must_use]
    pub fn filter(
        &self,
        diagnostics: Vec<Diagnostic>,
        sources: &SourceManager,
        scenes: &SceneSpans,
    ) -> Vec<Diagnostic> {
        let mut remaining = self.counts.clone();
        diagnostics
            .into_iter()
            .filter(|diagnostic| {
                let entry = entry_for(diagnostic, sources, scenes);
                if consume(&mut remaining, &entry) {
                    return false;
                }
                if !entry.scene.is_empty() {
                    let legacy = BaselineEntry {
                        scene: String::new(),
                        ..entry
                    };
                    if consume(&mut remaining, &legacy) {
                        return false;
                    }
                }
                true
            })
            .collect()
    }
}

/// Consumes one count of `entry` from the remaining multiset, if any.
fn consume(remaining: &mut BTreeMap<BaselineEntry, usize>, entry: &BaselineEntry) -> bool {
    match remaining.get_mut(entry) {
        Some(count) if *count > 0 => {
            *count -= 1;
            true
        }
        _ => false,
    }
}

/// Renders the baseline document for the given diagnostics: schema version
/// 1, sorted entries, byte-stable, terminated by one newline.
#[must_use]
pub fn render(diagnostics: &[Diagnostic], sources: &SourceManager, scenes: &SceneSpans) -> String {
    let mut entries: Vec<BaselineEntry> = diagnostics
        .iter()
        .map(|diagnostic| entry_for(diagnostic, sources, scenes))
        .collect();
    entries.sort();
    let document = BaselineDocument {
        schema_version: SCHEMA_VERSION,
        entries,
    };
    let mut output =
        serde_json::to_string_pretty(&document).expect("baseline serialization cannot fail");
    output.push('\n');
    output
}

/// Computes the fingerprint entry for one diagnostic.
///
/// A diagnostic whose path is not among the loaded sources (or whose file
/// failed to lex) hashes an empty token context; the computation is the
/// same on the write and the match side, so such entries still round-trip.
#[must_use]
pub fn entry_for(
    diagnostic: &Diagnostic,
    sources: &SourceManager,
    scenes: &SceneSpans,
) -> BaselineEntry {
    let file = sources
        .files()
        .iter()
        .find(|file| file.relative_path() == diagnostic.path);
    BaselineEntry {
        rule_id: diagnostic.rule_id.clone(),
        path: diagnostic.path.clone(),
        scene: scenes
            .scene_for(&diagnostic.path, diagnostic.primary_span.start.line)
            .to_owned(),
        token_hash: token_hash(file, diagnostic.primary_span.start.line),
    }
}

/// Whether a token is layout/comment trivia excluded from the hash.
const fn is_trivia(token: &Tok) -> bool {
    matches!(
        token,
        Tok::Comment(_)
            | Tok::Newline
            | Tok::NonLogicalNewline
            | Tok::Indent
            | Tok::Dedent
            | Tok::EndOfFile
    )
}

/// Sorted one-based lines containing at least one non-trivia token.
fn code_lines(file: &SourceFile) -> Vec<usize> {
    let mut lines: Vec<usize> = file
        .tokens()
        .iter()
        .filter(|(token, _)| !is_trivia(token))
        .map(|(_, range)| file.line_of_byte(range.start().into()))
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// FNV-1a hash over the token kinds and texts of the diagnostic statement
/// line and its nearest non-blank neighbor line on each side.
fn token_hash(file: Option<&SourceFile>, line: usize) -> String {
    let mut hash = Fnv1a::new();
    if let Some(file) = file {
        let lines = code_lines(file);
        let mut context: Vec<usize> = Vec::with_capacity(3);
        let before = lines.partition_point(|candidate| *candidate < line);
        if before > 0 {
            context.push(lines[before - 1]);
        }
        context.push(line);
        let after = lines.partition_point(|candidate| *candidate <= line);
        if let Some(next) = lines.get(after) {
            context.push(*next);
        }
        for (token, range) in file.tokens() {
            if is_trivia(token) {
                continue;
            }
            let token_line = file.line_of_byte(range.start().into());
            if context.contains(&token_line) {
                // Kind and embedded value via the stable Debug form, plus
                // the exact source text (covers operator spellings).
                hash.write(format!("{token:?}").as_bytes());
                hash.write(&[0]);
                hash.write(file.slice(*range).as_bytes());
                hash.write(&[0]);
            }
        }
    }
    format!("fnv1a64:{:016x}", hash.finish())
}

/// Minimal 64-bit FNV-1a. Implemented locally because the standard
/// `DefaultHasher` is not guaranteed stable across Rust releases and the
/// hash is persisted to disk.
struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, Severity, SourcePosition, SourceSpan};
    use std::path::Path;

    fn sources_from(text: &str) -> SourceManager {
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(Path::new("/project/scene.py"), text.as_bytes());
        sources
    }

    fn diagnostic_at(line: usize) -> Diagnostic {
        Diagnostic {
            rule_id: "MLC101".to_owned(),
            severity: Severity::Error,
            confidence: Confidence::Certain,
            path: "scene.py".to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition { line, column: 1 },
                end: SourcePosition { line, column: 2 },
            },
            message: "m".to_owned(),
            explanation: None,
            related_locations: Vec::new(),
            evidence: BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: None,
        }
    }

    /// Attribution mapping lines 1..=5 of `scene.py` to `scene.Demo`.
    fn demo_scene_spans() -> SceneSpans {
        let mut per_file = BTreeMap::new();
        per_file.insert("scene.py".to_owned(), vec![(1, 5, "scene.Demo".to_owned())]);
        SceneSpans { per_file }
    }

    #[test]
    fn fingerprint_ignores_line_shifts_from_distant_edits() {
        let original = sources_from("a = 1\nb = 2\n\ntarget = 3\n\nc = 4\n");
        let shifted = sources_from("inserted = 0\na = 1\nb = 2\n\ntarget = 3\n\nc = 4\n");
        let before = entry_for(&diagnostic_at(4), &original, &SceneSpans::default());
        let after = entry_for(&diagnostic_at(5), &shifted, &SceneSpans::default());
        assert_eq!(before, after, "distant insertion must not change the hash");
    }

    #[test]
    fn fingerprint_changes_when_a_neighbor_line_changes() {
        let original = sources_from("a = 1\ntarget = 3\nc = 4\n");
        let touched = sources_from("a = 999\ntarget = 3\nc = 4\n");
        let before = entry_for(&diagnostic_at(2), &original, &SceneSpans::default());
        let after = entry_for(&diagnostic_at(2), &touched, &SceneSpans::default());
        assert_ne!(before.token_hash, after.token_hash);
    }

    #[test]
    fn duplicate_entries_hide_exactly_that_many_diagnostics() {
        let sources = sources_from("target = 3\n");
        let scenes = SceneSpans::default();
        let diagnostics = vec![diagnostic_at(1), diagnostic_at(1), diagnostic_at(1)];
        let baseline_text = render(&diagnostics[..2], &sources, &scenes);
        let baseline = Baseline::parse(&baseline_text).expect("valid baseline");
        assert_eq!(baseline.len(), 2);
        let survivors = baseline.filter(diagnostics, &sources, &scenes);
        assert_eq!(survivors.len(), 1, "two entries hide two diagnostics");
    }

    #[test]
    fn entries_carry_the_enclosing_scene_and_empty_outside() {
        let sources = sources_from("a = 1\nb = 2\nc = 3\nd = 4\ne = 5\nf = 6\n");
        let scenes = demo_scene_spans();
        let inside = entry_for(&diagnostic_at(2), &sources, &scenes);
        assert_eq!(inside.scene, "scene.Demo");
        let outside = entry_for(&diagnostic_at(6), &sources, &scenes);
        assert_eq!(outside.scene, "", "line 6 is outside the Scene span");
    }

    #[test]
    fn stored_empty_scene_matches_any_computed_scene() {
        // A baseline written before scene attribution stores `scene: ""`;
        // it must keep suppressing a diagnostic now attributed to a Scene.
        let sources = sources_from("target = 3\n");
        let legacy = render(&[diagnostic_at(1)], &sources, &SceneSpans::default());
        let baseline = Baseline::parse(&legacy).expect("valid baseline");
        let survivors = baseline.filter(vec![diagnostic_at(1)], &sources, &demo_scene_spans());
        assert!(survivors.is_empty(), "legacy empty scene is a wildcard");
    }

    #[test]
    fn stored_scene_does_not_match_a_different_scene() {
        let sources = sources_from("target = 3\n");
        let mut other = BTreeMap::new();
        other.insert(
            "scene.py".to_owned(),
            vec![(1, 5, "scene.Other".to_owned())],
        );
        let written = render(
            &[diagnostic_at(1)],
            &sources,
            &SceneSpans { per_file: other },
        );
        let baseline = Baseline::parse(&written).expect("valid baseline");
        let survivors = baseline.filter(vec![diagnostic_at(1)], &sources, &demo_scene_spans());
        assert_eq!(
            survivors.len(),
            1,
            "a stored non-empty scene only matches that scene"
        );
    }

    #[test]
    fn scene_attribution_round_trips_through_render_and_filter() {
        let sources = sources_from("target = 3\n");
        let scenes = demo_scene_spans();
        let written = render(&[diagnostic_at(1)], &sources, &scenes);
        assert!(written.contains("scene.Demo"));
        let baseline = Baseline::parse(&written).expect("valid baseline");
        let survivors = baseline.filter(vec![diagnostic_at(1)], &sources, &scenes);
        assert!(survivors.is_empty(), "same attribution matches exactly");
    }

    #[test]
    fn wrong_schema_version_is_rejected() {
        assert!(Baseline::parse(r#"{"schema_version": 2, "entries": []}"#).is_err());
        assert!(Baseline::parse("nonsense").is_err());
        assert!(Baseline::parse(r#"{"schema_version": 1, "entries": []}"#).is_ok());
    }

    #[test]
    fn rendered_entries_are_sorted_and_byte_stable() {
        let sources = sources_from("a = 1\nb = 2\n");
        let mut second = diagnostic_at(2);
        second.rule_id = "MLC000".to_owned();
        let diagnostics = vec![diagnostic_at(1), second];
        let first_render = render(&diagnostics, &sources, &SceneSpans::default());
        let second_render = render(&diagnostics, &sources, &SceneSpans::default());
        assert_eq!(first_render.as_bytes(), second_render.as_bytes());
        let value: serde_json::Value = serde_json::from_str(&first_render).expect("valid JSON");
        let entries = value["entries"].as_array().expect("entries");
        assert_eq!(entries[0]["rule_id"], "MLC000", "entries sorted by rule ID");
        assert_eq!(entries[1]["rule_id"], "MLC101");
    }
}
