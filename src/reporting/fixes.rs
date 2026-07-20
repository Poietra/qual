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
//! The bundled parser grammar is fixed — rustpython-parser 0.4 implements
//! the Python 3.12 grammar and exposes no `feature_version` pinning — so
//! the post-fix re-parse accepts that grammar regardless of the configured
//! `target-python`. [`apply_with_target`] therefore re-runs the post-parse
//! syntax feature gate ([`crate::frontend::features`]) on every fixed
//! file: a file whose fixes *introduced* a construct newer than
//! `target-python` (any gated feature occurring more often than in the
//! original text) is rolled back like a parse failure. Pre-existing gated
//! constructs never block a fix — the file already carries their `MLC000`
//! and analysis of it continues by design.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use rustpython_parser::{Mode, ast, parse};

use crate::diagnostic::{Diagnostic, Fix, FixApplicability, SourcePosition};
use crate::frontend::features;
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

/// Applies fixes without a `target-python` syntax gate.
///
/// Equivalent to [`apply_with_target`] with a target at the bundled
/// grammar's own version: only the re-parse and re-encode checks guard the
/// rollback. The CLI path always uses [`apply_with_target`] with the
/// configured `target-python`.
pub fn apply(
    sources: &SourceManager,
    diagnostics: &[Diagnostic],
    unsafe_fixes: bool,
) -> Result<FixReport, FixError> {
    apply_with_target(sources, diagnostics, unsafe_fixes, None)
}

/// Applies the fixes attached to `diagnostics` to the files on disk.
///
/// Only `applicability = safe` fixes are applied unless `unsafe_fixes` is
/// set. Fixes are considered in the given (stable diagnostic) order, so the
/// outcome is deterministic. Files whose fixed text fails to re-parse, or
/// (with a `target_python`) whose fixes introduced syntax newer than the
/// target, are rolled back per file; a (rare) multi-file fix touching a
/// rolled-back file is not counted as applied, but its edits to other,
/// surviving files are still written.
pub fn apply_with_target(
    sources: &SourceManager,
    diagnostics: &[Diagnostic],
    unsafe_fixes: bool,
    target_python: Option<&str>,
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
        let checked = reparse(&new_text, path)
            .and_then(|module| check_target_syntax(file, &new_text, &module, target_python))
            .and_then(|()| encode(file, &new_text));
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
fn reparse(text: &str, path: &str) -> Result<ast::ModModule, String> {
    match parse(text, Mode::Module, path) {
        Ok(ast::Mod::Module(module)) => Ok(module),
        Ok(_) => unreachable!("Mode::Module always produces Mod::Module"),
        Err(error) => Err(format!("fixed text no longer parses: {}", error.error)),
    }
}

/// Rolls the file back when a fix *introduced* syntax newer than the
/// configured `target-python`: a gated feature counts as introduced when
/// the fixed text uses it more often than the original text did.
/// Pre-existing gated constructs (the file already carries their `MLC000`)
/// never block a fix that edits something else.
fn check_target_syntax(
    file: &SourceFile,
    fixed_text: &str,
    module: &ast::ModModule,
    target_python: Option<&str>,
) -> Result<(), String> {
    let Some(target_python) = target_python else {
        return Ok(());
    };
    let Some(target) = features::parse_python_version(target_python) else {
        debug_assert!(false, "target-python is validated at config time");
        return Ok(());
    };
    let fixed_tokens = features::lex_tokens(fixed_text);
    let fixed = feature_counts(&features::violations(&fixed_tokens, module, target));
    if fixed.is_empty() {
        return Ok(());
    }
    let original = file
        .ast()
        .map(|module| feature_counts(&features::violations(file.tokens(), module, target)))
        .unwrap_or_default();
    for (feature, count) in &fixed {
        if *count > original.get(feature).copied().unwrap_or(0) {
            let (major, minor) = feature.introduced_in();
            return Err(format!(
                "fix introduced {label}, which requires Python {major}.{minor} \
                 but target-python is {target_python}",
                label = feature.label(),
            ));
        }
    }
    Ok(())
}

/// Occurrences of each gated feature, for the introduced-syntax check.
fn feature_counts(violations: &[features::Violation]) -> BTreeMap<features::Feature, usize> {
    let mut counts = BTreeMap::new();
    for violation in violations {
        *counts.entry(violation.feature).or_insert(0) += 1;
    }
    counts
}

/// Re-encodes the fixed text in the file's original source encoding.
pub(crate) fn encode(file: &SourceFile, text: &str) -> Result<Vec<u8>, String> {
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
    if encoding_info.label == "latin-1" {
        // Round-trip of the true Latin-1 decode in `source.rs`: code
        // points U+0000..=U+00FF map 1:1 to bytes; anything else cannot
        // be represented and rolls the file back. (The WHATWG lookup
        // below would encode via windows-1252, which diverges from
        // CPython's latin-1 in 0x80–0x9F.)
        for ch in text.chars() {
            match u8::try_from(u32::from(ch)) {
                Ok(byte) => bytes.push(byte),
                Err(_) => {
                    return Err("fixed text cannot be encoded as latin-1".to_owned());
                }
            }
        }
        return Ok(bytes);
    }
    // Round-trip of the C1-override decode in `source.rs`: CPython's
    // iso8859-9 / iso8859-11 differ from the WHATWG windows-1254 /
    // windows-874 decoders only in 0x80–0x9F, so the C1 controls map back
    // to those bytes directly and every other char goes through the WHATWG
    // encoder — which must then never emit a byte in 0x80–0x9F (e.g. '€' →
    // 0x80 in windows-1254 would re-decode as U+0080; CPython's iso8859-9
    // cannot encode '€' at all, so the file rolls back instead).
    let c1_override = match encoding_info.label.as_str() {
        "iso-8859-9" => Some(encoding_rs::WINDOWS_1254),
        "iso-8859-11" => Some(encoding_rs::WINDOWS_874),
        _ => None,
    };
    if let Some(encoding) = c1_override {
        encode_single_byte_with_c1_controls(text, encoding, &mut bytes).map_err(|()| {
            format!(
                "fixed text cannot be encoded as {label}",
                label = encoding_info.label
            )
        })?;
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

/// Encodes text for a single-byte WHATWG encoding decoded with the C1
/// override: U+0080–U+009F map to bytes 0x80–0x9F, everything else through
/// the WHATWG encoder, rejecting any char the encoder cannot represent or
/// would place in 0x80–0x9F (those bytes mean the C1 controls here).
fn encode_single_byte_with_c1_controls(
    text: &str,
    encoding: &'static encoding_rs::Encoding,
    bytes: &mut Vec<u8>,
) -> Result<(), ()> {
    let is_c1 = |ch: char| ('\u{80}'..='\u{9f}').contains(&ch);
    let mut rest = text;
    while !rest.is_empty() {
        let run_end = rest.find(is_c1).unwrap_or(rest.len());
        if run_end > 0 {
            let (encoded, _, had_errors) = encoding.encode(&rest[..run_end]);
            if had_errors || encoded.iter().any(|byte| (0x80..=0x9f).contains(byte)) {
                return Err(());
            }
            bytes.extend_from_slice(&encoded);
            rest = &rest[run_end..];
        }
        while let Some(ch) = rest.chars().next() {
            if !is_c1(ch) {
                break;
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "C1 controls are code points 0x80..=0x9F"
            )]
            bytes.push(u32::from(ch) as u8);
            rest = &rest[ch.len_utf8()..];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::diagnostic::{Confidence, Severity, SourceSpan};

    fn sources_from(text: &str) -> SourceManager {
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(Path::new("/project/scene.py"), text.as_bytes());
        sources
    }

    fn replace_first_line_fix(path: &str, line_length: usize, replacement: &str) -> Diagnostic {
        Diagnostic {
            rule_id: "MLC101".to_owned(),
            severity: Severity::Warning,
            confidence: Confidence::High,
            path: path.to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition { line: 1, column: 1 },
                end: SourcePosition { line: 1, column: 1 },
            },
            message: String::new(),
            explanation: None,
            related_locations: Vec::new(),
            evidence: std::collections::BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: Some(Fix {
                applicability: FixApplicability::Safe,
                message: String::new(),
                edits: vec![crate::diagnostic::TextEdit {
                    path: path.to_owned(),
                    span: SourceSpan {
                        start: SourcePosition { line: 1, column: 1 },
                        end: SourcePosition {
                            line: 1,
                            column: line_length + 1,
                        },
                    },
                    replacement: replacement.to_owned(),
                }],
            }),
        }
    }

    /// A fix that introduces a `match` statement rolls the file back under
    /// a pre-3.10 `target-python` and applies under 3.10.
    #[test]
    fn fix_introducing_gated_syntax_rolls_back_under_old_target() {
        let original = "value = command\n";
        let replacement = "match command:\n    case 1:\n        pass";
        for (target, expect_applied) in [(Some("3.9"), false), (Some("3.10"), true), (None, true)] {
            let project = tempfile::tempdir().unwrap();
            let file_path = project.path().join("scene.py");
            std::fs::write(&file_path, original).unwrap();
            let mut sources = SourceManager::new(project.path());
            sources.load_file(&file_path);
            let diagnostic =
                replace_first_line_fix("scene.py", original.trim_end().len(), replacement);
            let report = apply_with_target(&sources, &[diagnostic], false, target).unwrap();
            let on_disk = std::fs::read_to_string(&file_path).unwrap();
            if expect_applied {
                assert_eq!(report.applied, 1, "target {target:?}");
                assert!(on_disk.contains("match command"), "target {target:?}");
            } else {
                assert_eq!(report.applied, 0, "target {target:?}");
                assert_eq!(report.rolled_back.len(), 1);
                let rolled_back = &report.rolled_back[0];
                assert_eq!(rolled_back.path, "scene.py");
                assert!(
                    rolled_back.reason.contains("requires Python 3.10")
                        && rolled_back.reason.contains("target-python is 3.9"),
                    "reason must name the construct and versions: {}",
                    rolled_back.reason
                );
                assert_eq!(on_disk, original, "rolled back file must be untouched");
            }
        }
    }

    /// The rollback gate covers token-detected constructs too: a fix that
    /// introduces a self-documenting f-string `=` (invisible in the AST,
    /// found in the token text) rolls back under a pre-3.8 target.
    #[test]
    fn fix_introducing_fstring_debug_rolls_back_under_old_target() {
        let original = "value = name\n";
        let replacement = "value = f\"{name=}\"";
        for (target, expect_applied) in [(Some("3.7"), false), (Some("3.8"), true)] {
            let project = tempfile::tempdir().unwrap();
            let file_path = project.path().join("scene.py");
            std::fs::write(&file_path, original).unwrap();
            let mut sources = SourceManager::new(project.path());
            sources.load_file(&file_path);
            let diagnostic =
                replace_first_line_fix("scene.py", original.trim_end().len(), replacement);
            let report = apply_with_target(&sources, &[diagnostic], false, target).unwrap();
            let on_disk = std::fs::read_to_string(&file_path).unwrap();
            if expect_applied {
                assert_eq!(report.applied, 1, "target {target:?}");
                assert!(on_disk.contains("{name=}"), "target {target:?}");
            } else {
                assert_eq!(report.applied, 0, "target {target:?}");
                assert_eq!(report.rolled_back.len(), 1);
                assert!(
                    report.rolled_back[0].reason.contains("requires Python 3.8")
                        && report.rolled_back[0]
                            .reason
                            .contains("target-python is 3.7"),
                    "reason must name the construct and versions: {}",
                    report.rolled_back[0].reason
                );
                assert_eq!(on_disk, original, "rolled back file must be untouched");
            }
        }
    }

    /// A gated construct that already existed in the file never blocks a
    /// fix that edits something else: the file keeps its MLC000 and the
    /// fix still lands.
    #[test]
    fn preexisting_gated_syntax_does_not_block_unrelated_fixes() {
        let original = "flag = False\nmatch flag:\n    case 1:\n        pass\n";
        let project = tempfile::tempdir().unwrap();
        let file_path = project.path().join("scene.py");
        std::fs::write(&file_path, original).unwrap();
        let mut sources = SourceManager::new(project.path());
        sources.load_file(&file_path);
        let diagnostic = replace_first_line_fix("scene.py", "flag = False".len(), "flag = True");
        let report = apply_with_target(&sources, &[diagnostic], false, Some("3.9")).unwrap();
        assert_eq!(report.applied, 1);
        assert!(report.rolled_back.is_empty());
        assert!(
            std::fs::read_to_string(&file_path)
                .unwrap()
                .starts_with("flag = True\n")
        );
    }

    /// Round-trip of the iso-8859-9 C1-override decode: C1 controls map
    /// back to 0x80–0x9F, Turkish letters to their windows-1254 bytes, and
    /// '€' — encodable in windows-1254 (0x80) but not in `CPython`'s
    /// iso8859-9 — is a rollback, never a silent byte change.
    #[test]
    fn iso_8859_9_reencode_round_trips_and_rejects_euro() {
        let mut bytes = b"# coding: iso-8859-9\nx = \"".to_vec();
        bytes.extend([0xd0, 0x9f, 0xfd]); // G-breve, C1 0x9F, dotless i
        bytes.extend(b"\"\n");
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(Path::new("/project/tr.py"), &bytes);
        let file = &sources.files()[0];
        assert!(file.is_parsed());
        assert_eq!(
            encode(file, file.text()).expect("identity re-encode"),
            bytes,
            "decode then encode must reproduce the original bytes"
        );
        let euro = encode(file, "x = \"\u{20ac}\"\n");
        assert!(euro.is_err(), "'€' is not encodable in CPython iso8859-9");
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
