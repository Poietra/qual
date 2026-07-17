//! Autofix application (DESIGN §6.3, Phase 5 pulled forward).
//!
//! `SAFE` and `UNSAFE` fixes are never mixed: unsafe fixes are skipped
//! unless `--unsafe-fixes` was given. Edit spans are one-based Unicode
//! character positions with exclusive ends, converted through the
//! [`SourceManager`] tables so multi-byte text (e.g. Japanese) is edited
//! correctly. Within one file all applied edits must be non-overlapping;
//! a fix whose edits overlap an already-accepted edit (or each other) is
//! skipped whole and reported. After editing, every touched file is
//! re-parsed with the Python parser; a file whose fixed text no longer
//! parses (or no longer encodes in its original source encoding) is rolled
//! back entirely — nothing is written for it — and reported.
//!
//! The parser accepts current-Python grammar regardless of the configured
//! `target-python`; a stricter per-version re-parse can be layered in once
//! the parser exposes feature versions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rustpython_parser::{Mode, parse};

use crate::diagnostic::{Diagnostic, Fix, FixApplicability, SourcePosition};
use crate::source::{SourceFile, SourceManager};

/// Errors that abort fix application; callers map them to exit code 2.
#[derive(Debug, thiserror::Error)]
pub enum FixError {
    /// Writing a fixed file to disk failed.
    #[error("cannot write fixed file {path}: {source}")]
    Write {
        /// Absolute path of the file being written.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
}

/// A file reverted because its fixed text failed to re-parse or re-encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledBackFile {
    /// Project-relative POSIX path.
    pub path: String,
    /// Human-readable reason for the rollback.
    pub reason: String,
}

/// Summary of one fix-application pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FixReport {
    /// Fixes whose edits were applied and whose files were kept.
    pub applied: usize,
    /// Unsafe fixes skipped because `--unsafe-fixes` was not given.
    pub skipped_unsafe: usize,
    /// Fixes skipped whole because an edit overlapped another accepted
    /// edit in the same file (or another edit of the same fix).
    pub skipped_overlapping: usize,
    /// Fixes skipped because an edit path or span could not be resolved.
    pub skipped_invalid: usize,
    /// Project-relative paths of files written, in sorted order.
    pub files_changed: Vec<String>,
    /// Files reverted after their fixed text failed to re-parse or encode.
    pub rolled_back: Vec<RolledBackFile>,
}

impl FixReport {
    /// Whether the pass applied, skipped, or rolled back anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.applied == 0
            && self.skipped_unsafe == 0
            && self.skipped_overlapping == 0
            && self.skipped_invalid == 0
            && self.files_changed.is_empty()
            && self.rolled_back.is_empty()
    }
}

/// One resolved edit in original-text Unicode character coordinates.
#[derive(Debug, Clone)]
struct ResolvedEdit {
    fix_index: usize,
    start: usize,
    end: usize,
    replacement: String,
}

/// Applies the fixes attached to `diagnostics` to the files on disk.
///
/// Only `applicability = safe` fixes are applied unless `unsafe_fixes` is
/// set. Fixes are considered in the given (stable diagnostic) order, so the
/// outcome is deterministic. Files whose fixed text fails to re-parse are
/// rolled back per file; a (rare) multi-file fix touching a rolled-back
/// file is not counted as applied, but its edits to other, surviving files
/// are still written.
pub fn apply(
    sources: &SourceManager,
    diagnostics: &[Diagnostic],
    unsafe_fixes: bool,
) -> Result<FixReport, FixError> {
    let mut report = FixReport::default();
    let files: BTreeMap<&str, &SourceFile> = sources
        .files()
        .iter()
        .map(|file| (file.relative_path(), file))
        .collect();

    let candidates: Vec<&Fix> = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.fix.as_ref())
        .collect();

    let mut per_file: BTreeMap<String, Vec<ResolvedEdit>> = BTreeMap::new();
    let mut accepted: Vec<usize> = Vec::new();
    for (fix_index, fix) in candidates.iter().enumerate() {
        if fix.applicability == FixApplicability::Unsafe && !unsafe_fixes {
            report.skipped_unsafe += 1;
            continue;
        }
        let Some(resolved) = resolve_fix(fix_index, fix, &files) else {
            report.skipped_invalid += 1;
            continue;
        };
        if has_overlap(&resolved, &per_file) {
            report.skipped_overlapping += 1;
            continue;
        }
        for (path, edit) in resolved {
            per_file.entry(path).or_default().push(edit);
        }
        accepted.push(fix_index);
    }

    // Edit, re-parse, and re-encode every touched file before writing
    // anything, so a parse failure rolls the whole file back.
    let mut failed_fixes: BTreeSet<usize> = BTreeSet::new();
    let mut writes: Vec<(&SourceFile, String, Vec<u8>)> = Vec::new();
    for (path, edits) in &per_file {
        let Some(file) = files.get(path.as_str()) else {
            continue; // unreachable: resolve_fix only accepts known paths
        };
        let new_text = apply_edits(file.text(), edits);
        if new_text == file.text() {
            continue;
        }
        let checked = reparse(&new_text, path).and_then(|()| encode(file, &new_text));
        match checked {
            Ok(bytes) => writes.push((file, path.clone(), bytes)),
            Err(reason) => {
                failed_fixes.extend(edits.iter().map(|edit| edit.fix_index));
                report.rolled_back.push(RolledBackFile {
                    path: path.clone(),
                    reason,
                });
            }
        }
    }

    for (file, path, bytes) in writes {
        std::fs::write(file.path(), &bytes).map_err(|source| FixError::Write {
            path: file.path().to_path_buf(),
            source,
        })?;
        report.files_changed.push(path);
    }
    report.applied = accepted
        .iter()
        .filter(|fix_index| !failed_fixes.contains(fix_index))
        .count();
    Ok(report)
}

/// Resolves every edit of one fix to character offsets, or `None` when any
/// path is unknown, any span is invalid, or the fix has no edits.
fn resolve_fix(
    fix_index: usize,
    fix: &Fix,
    files: &BTreeMap<&str, &SourceFile>,
) -> Option<Vec<(String, ResolvedEdit)>> {
    let mut resolved = Vec::with_capacity(fix.edits.len());
    for edit in &fix.edits {
        let file = files.get(edit.path.as_str())?;
        let start = char_offset_of(file, edit.span.start)?;
        let end = char_offset_of(file, edit.span.end)?;
        if start > end {
            return None;
        }
        resolved.push((
            edit.path.clone(),
            ResolvedEdit {
                fix_index,
                start,
                end,
                replacement: edit.replacement.clone(),
            },
        ));
    }
    if resolved.is_empty() {
        return None;
    }
    Some(resolved)
}

/// Converts a one-based `(line, Unicode character column)` position to an
/// absolute character offset.
///
/// The column may point one past the last character of the line (the
/// exclusive-end convention); an edit that must remove a line break ends at
/// column 1 of the following line instead.
fn char_offset_of(file: &SourceFile, position: SourcePosition) -> Option<usize> {
    let line_start = file.char_offset(position.line, 0)?;
    let column_index = position.column.checked_sub(1)?;
    if column_index > file.line_text(position.line).chars().count() {
        return None;
    }
    Some(line_start + column_index)
}

/// Whether any candidate edit overlaps another candidate or accepted edit
/// in the same file. Ranges are half-open; zero-width insertions at the
/// same offset do not overlap.
fn has_overlap(
    resolved: &[(String, ResolvedEdit)],
    accepted: &BTreeMap<String, Vec<ResolvedEdit>>,
) -> bool {
    for (index, (path, edit)) in resolved.iter().enumerate() {
        let internal = resolved[index + 1..]
            .iter()
            .any(|(other_path, other)| other_path == path && ranges_overlap(edit, other));
        let external = accepted
            .get(path)
            .is_some_and(|edits| edits.iter().any(|other| ranges_overlap(edit, other)));
        if internal || external {
            return true;
        }
    }
    false
}

const fn ranges_overlap(a: &ResolvedEdit, b: &ResolvedEdit) -> bool {
    a.start < b.end && b.start < a.end
}

/// Applies non-overlapping edits in descending span order so earlier
/// offsets stay valid. Ties between zero-width insertions are broken by the
/// resolution order, keeping the result deterministic.
fn apply_edits(original: &str, edits: &[ResolvedEdit]) -> String {
    let mut order: Vec<&ResolvedEdit> = edits.iter().collect();
    order.sort_by_key(|edit| (edit.start, edit.end));
    let mut text = original.to_owned();
    for edit in order.iter().rev() {
        let start = byte_of_char(&text, edit.start);
        let end = byte_of_char(&text, edit.end);
        text.replace_range(start..end, &edit.replacement);
    }
    text
}

/// Byte offset of a character offset in `text`, clamping past-the-end.
fn byte_of_char(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map_or(text.len(), |(byte, _)| byte)
}

/// Re-parses the fixed text; a failure message triggers a rollback.
fn reparse(text: &str, path: &str) -> Result<(), String> {
    parse(text, Mode::Module, path)
        .map(|_| ())
        .map_err(|error| format!("fixed text no longer parses: {}", error.error))
}

/// Re-encodes the fixed text in the file's original source encoding.
fn encode(file: &SourceFile, text: &str) -> Result<Vec<u8>, String> {
    const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
    let encoding_info = file.encoding();
    let mut bytes = Vec::with_capacity(text.len() + UTF8_BOM.len());
    if encoding_info.byte_order_mark {
        bytes.extend_from_slice(UTF8_BOM);
    }
    if encoding_info.label == "utf-8" {
        bytes.extend_from_slice(text.as_bytes());
        return Ok(bytes);
    }
    let Some(encoding) = encoding_rs::Encoding::for_label(encoding_info.label.as_bytes()) else {
        return Err(format!(
            "unknown source encoding {label}",
            label = encoding_info.label
        ));
    };
    let (encoded, _, had_errors) = encoding.encode(text);
    if had_errors {
        return Err(format!(
            "fixed text cannot be encoded as {label}",
            label = encoding_info.label
        ));
    }
    bytes.extend_from_slice(&encoded);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sources_from(text: &str) -> SourceManager {
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(Path::new("/project/scene.py"), text.as_bytes());
        sources
    }

    #[test]
    fn char_offsets_use_unicode_columns() {
        let sources = sources_from("label = \"こんにちは\"\n");
        let file = &sources.files()[0];
        // Column 10 is こ (after 8 ASCII chars and the opening quote).
        assert_eq!(
            char_offset_of(
                file,
                SourcePosition {
                    line: 1,
                    column: 10
                }
            ),
            Some(9)
        );
        // One past the closing quote (15 characters on the line).
        assert_eq!(
            char_offset_of(
                file,
                SourcePosition {
                    line: 1,
                    column: 16
                }
            ),
            Some(15)
        );
        // Beyond the exclusive end of the line: invalid.
        assert_eq!(
            char_offset_of(
                file,
                SourcePosition {
                    line: 1,
                    column: 17
                }
            ),
            None
        );
    }

    #[test]
    fn edits_apply_in_descending_order() {
        let edits = [
            ResolvedEdit {
                fix_index: 0,
                start: 0,
                end: 1,
                replacement: "xx".to_owned(),
            },
            ResolvedEdit {
                fix_index: 1,
                start: 2,
                end: 3,
                replacement: "yy".to_owned(),
            },
        ];
        assert_eq!(apply_edits("abc", &edits), "xxbyy");
    }

    #[test]
    fn zero_width_insertions_at_one_offset_keep_fix_order() {
        let edits = [
            ResolvedEdit {
                fix_index: 0,
                start: 1,
                end: 1,
                replacement: "A".to_owned(),
            },
            ResolvedEdit {
                fix_index: 1,
                start: 1,
                end: 1,
                replacement: "B".to_owned(),
            },
        ];
        assert_eq!(apply_edits("xy", &edits), "xABy");
    }
}
