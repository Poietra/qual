//! Baseline files (DESIGN §8.3, Phase 5 pulled forward): record the
//! currently known diagnostics so an existing project can adopt the linter
//! incrementally.
//!
//! A fingerprint never contains line numbers. It is
//! `rule ID + relative path + qualified Scene name + surrounding token hash`.
//! The Scene name is an empty placeholder string until lifecycle facts
//! exist; the field is designed in now so the file format never changes.
//! The token hash covers the token kinds and texts of the diagnostic
//! statement line and its nearest non-blank neighbor line on each side, so
//! inserting unrelated lines elsewhere in the file does not invalidate the
//! entries.
//!
//! The on-disk format is JSON matching `schemas/baseline-v1.json`:
//! `schema_version` 1 plus a sorted entry list, serialized byte-stably.

use std::collections::BTreeMap;

use rustpython_parser::Tok;
use serde::{Deserialize, Serialize};

use crate::diagnostic::Diagnostic;
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
    /// Qualified Scene name. Currently always the empty placeholder string;
    /// it is populated once lifecycle facts exist.
    pub scene: String,
    /// Hash of the surrounding tokens: `fnv1a64:` plus 16 hex digits.
    pub token_hash: String,
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
    /// the same fingerprint twice needs two entries to hide both.
    #[must_use]
    pub fn filter(&self, diagnostics: Vec<Diagnostic>, sources: &SourceManager) -> Vec<Diagnostic> {
        let mut remaining = self.counts.clone();
        diagnostics
            .into_iter()
            .filter(|diagnostic| {
                let entry = entry_for(diagnostic, sources);
                match remaining.get_mut(&entry) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        false
                    }
                    _ => true,
                }
            })
            .collect()
    }
}

/// Renders the baseline document for the given diagnostics: schema version
/// 1, sorted entries, byte-stable, terminated by one newline.
#[must_use]
pub fn render(diagnostics: &[Diagnostic], sources: &SourceManager) -> String {
    let mut entries: Vec<BaselineEntry> = diagnostics
        .iter()
        .map(|diagnostic| entry_for(diagnostic, sources))
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
pub fn entry_for(diagnostic: &Diagnostic, sources: &SourceManager) -> BaselineEntry {
    let file = sources
        .files()
        .iter()
        .find(|file| file.relative_path() == diagnostic.path);
    BaselineEntry {
        rule_id: diagnostic.rule_id.clone(),
        path: diagnostic.path.clone(),
        scene: String::new(),
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

    #[test]
    fn fingerprint_ignores_line_shifts_from_distant_edits() {
        let original = sources_from("a = 1\nb = 2\n\ntarget = 3\n\nc = 4\n");
        let shifted = sources_from("inserted = 0\na = 1\nb = 2\n\ntarget = 3\n\nc = 4\n");
        let before = entry_for(&diagnostic_at(4), &original);
        let after = entry_for(&diagnostic_at(5), &shifted);
        assert_eq!(before, after, "distant insertion must not change the hash");
    }

    #[test]
    fn fingerprint_changes_when_a_neighbor_line_changes() {
        let original = sources_from("a = 1\ntarget = 3\nc = 4\n");
        let touched = sources_from("a = 999\ntarget = 3\nc = 4\n");
        let before = entry_for(&diagnostic_at(2), &original);
        let after = entry_for(&diagnostic_at(2), &touched);
        assert_ne!(before.token_hash, after.token_hash);
    }

    #[test]
    fn duplicate_entries_hide_exactly_that_many_diagnostics() {
        let sources = sources_from("target = 3\n");
        let diagnostics = vec![diagnostic_at(1), diagnostic_at(1), diagnostic_at(1)];
        let baseline_text = render(&diagnostics[..2], &sources);
        let baseline = Baseline::parse(&baseline_text).expect("valid baseline");
        assert_eq!(baseline.len(), 2);
        let survivors = baseline.filter(diagnostics, &sources);
        assert_eq!(survivors.len(), 1, "two entries hide two diagnostics");
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
        let first_render = render(&diagnostics, &sources);
        let second_render = render(&diagnostics, &sources);
        assert_eq!(first_render.as_bytes(), second_render.as_bytes());
        let value: serde_json::Value = serde_json::from_str(&first_render).expect("valid JSON");
        let entries = value["entries"].as_array().expect("entries");
        assert_eq!(entries[0]["rule_id"], "MLC000", "entries sorted by rule ID");
        assert_eq!(entries[1]["rule_id"], "MLC101");
    }
}
