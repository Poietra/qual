//! Source loading, decoding, parsing, and span conversion (DESIGN §5.2).
//!
//! Python AST columns are UTF-8 byte offsets while every external report uses
//! one-based Unicode character columns. All conversions between those views
//! live here so that later phases never re-implement them.

use std::path::{Path, PathBuf};

use rustpython_parser::lexer::lex;
use rustpython_parser::text_size::TextRange;
use rustpython_parser::{Mode, Tok, ast, parse};

use crate::diagnostic::{Diagnostic, SourcePosition, SourceSpan};
use crate::rules::registry;

/// Stable handle for a file registered in a [`SourceManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(usize);

impl FileId {
    /// Zero-based index of the file inside its [`SourceManager`].
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }
}

/// Newline convention observed in a decoded source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineStyle {
    /// `\n` only, or no newline at all.
    Lf,
    /// `\r\n` only.
    CrLf,
    /// Bare `\r` only.
    Cr,
    /// More than one convention in the same file.
    Mixed,
}

/// Encoding information preserved from the on-disk byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEncoding {
    /// Normalized encoding label, e.g. `utf-8` or `shift_jis`.
    pub label: String,
    /// Whether the file started with a UTF-8 byte order mark.
    pub byte_order_mark: bool,
}

/// One comment token with enough position information for suppressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Comment text including the leading `#`.
    pub text: String,
    /// Byte range of the comment in the decoded text.
    pub range: TextRange,
    /// One-based line the comment starts on.
    pub line: usize,
    /// Whether only whitespace precedes the comment on its line.
    pub own_line: bool,
}

/// A decoded, parsed source file together with its span conversion tables.
#[derive(Debug)]
pub struct SourceFile {
    id: FileId,
    path: PathBuf,
    relative_path: String,
    text: String,
    encoding: SourceEncoding,
    newline: NewlineStyle,
    line_starts: Vec<usize>,
    ast: Option<ast::ModModule>,
    tokens: Vec<(Tok, TextRange)>,
    comments: Vec<Comment>,
    diagnostic: Option<Diagnostic>,
}

impl SourceFile {
    /// Handle of this file inside its manager.
    #[must_use]
    pub const fn id(&self) -> FileId {
        self.id
    }

    /// Absolute on-disk path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Project-relative POSIX path used in every diagnostic.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Decoded source text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Encoding preserved from disk.
    #[must_use]
    pub const fn encoding(&self) -> &SourceEncoding {
        &self.encoding
    }

    /// Newline convention preserved from disk.
    #[must_use]
    pub const fn newline(&self) -> NewlineStyle {
        self.newline
    }

    /// Parsed module body, or `None` when the file failed to decode or parse.
    #[must_use]
    pub const fn ast(&self) -> Option<&ast::ModModule> {
        self.ast.as_ref()
    }

    /// Lexer token stream including comments (empty when lexing failed early).
    #[must_use]
    pub fn tokens(&self) -> &[(Tok, TextRange)] {
        &self.tokens
    }

    /// All comments in source order.
    #[must_use]
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    /// `MLC000` diagnostic for a decode or parse failure, if any.
    #[must_use]
    pub const fn parse_diagnostic(&self) -> Option<&Diagnostic> {
        self.diagnostic.as_ref()
    }

    /// Whether the file decoded and parsed successfully.
    #[must_use]
    pub const fn is_parsed(&self) -> bool {
        self.ast.is_some()
    }

    /// Number of physical lines (a trailing newline opens a final empty line).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Converts `(one-based line, zero-based UTF-8 byte column)` — the Python
    /// AST convention — to an absolute Unicode character offset.
    ///
    /// Returns `None` when the position is outside the file or not on a
    /// character boundary.
    #[must_use]
    pub fn char_offset(&self, line: usize, utf8_byte_column: usize) -> Option<usize> {
        let line_start = *self.line_starts.get(line.checked_sub(1)?)?;
        let byte = line_start.checked_add(utf8_byte_column)?;
        if byte > self.text.len() || !self.text.is_char_boundary(byte) {
            return None;
        }
        Some(self.text[..byte].chars().count())
    }

    /// Converts an absolute Unicode character offset to a one-based
    /// `(line, display column)` position. Offsets past the end of the file
    /// clamp to the final position.
    #[must_use]
    pub fn position_of_char_offset(&self, char_offset: usize) -> SourcePosition {
        let byte = self
            .text
            .char_indices()
            .nth(char_offset)
            .map_or(self.text.len(), |(byte, _)| byte);
        self.position_of_byte(byte)
    }

    /// Converts an absolute byte offset to a one-based `(line, display
    /// column)` position with a Unicode character column.
    #[must_use]
    pub fn position_of_byte(&self, byte: usize) -> SourcePosition {
        let byte = byte.min(self.text.len());
        let line_index = self
            .line_starts
            .partition_point(|start| *start <= byte)
            .saturating_sub(1);
        let line_start = self.line_starts[line_index];
        let column = self.text[line_start..byte].chars().count() + 1;
        SourcePosition {
            line: line_index + 1,
            column,
        }
    }

    /// Converts an AST byte range to a one-based character-column span.
    #[must_use]
    pub fn span_of_range(&self, range: TextRange) -> SourceSpan {
        SourceSpan {
            start: self.position_of_byte(range.start().into()),
            end: self.position_of_byte(range.end().into()),
        }
    }

    /// Slices the source text covered by an AST byte range.
    #[must_use]
    pub fn slice(&self, range: TextRange) -> &str {
        let start = usize::from(range.start()).min(self.text.len());
        let end = usize::from(range.end()).min(self.text.len());
        &self.text[start..end]
    }

    /// Text of a one-based line without its trailing newline.
    #[must_use]
    pub fn line_text(&self, line: usize) -> &str {
        let Some(&start) = self.line_starts.get(line.wrapping_sub(1)) else {
            return "";
        };
        let end = self
            .line_starts
            .get(line)
            .map_or(self.text.len(), |next| *next);
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    /// One-based line containing an absolute byte offset.
    #[must_use]
    pub fn line_of_byte(&self, byte: usize) -> usize {
        self.position_of_byte(byte).line
    }
}

/// Loads, decodes, and parses every analyzed file (DESIGN §5.2).
///
/// Failures never abort the run: an unreadable, undecodable, or unparsable
/// file is registered with an `MLC000` diagnostic and analysis of the other
/// files continues.
#[derive(Debug)]
pub struct SourceManager {
    project_root: PathBuf,
    files: Vec<SourceFile>,
}

impl SourceManager {
    /// Creates a manager whose diagnostics are relative to `project_root`.
    #[must_use]
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            files: Vec::new(),
        }
    }

    /// Root all diagnostic paths are made relative to.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Reads, decodes, and parses a file from disk.
    ///
    /// Read and decode failures are recorded as `MLC000` on the returned
    /// file; they never abort the whole analysis.
    pub fn load_file(&mut self, path: &Path) -> FileId {
        match std::fs::read(path) {
            Ok(bytes) => self.load_bytes(path, &bytes),
            Err(error) => self.register_failure(path, &format!("cannot read file: {error}")),
        }
    }

    /// Registers an in-memory byte stream as if it had been read from `path`.
    pub fn load_bytes(&mut self, path: &Path, bytes: &[u8]) -> FileId {
        match decode_python_source(bytes) {
            Ok((text, encoding)) => self.register_text(path, text, encoding),
            Err(message) => self.register_failure(path, &message),
        }
    }

    /// All registered files in load order.
    #[must_use]
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// Looks up a file by handle.
    #[must_use]
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0]
    }

    fn register_text(&mut self, path: &Path, text: String, encoding: SourceEncoding) -> FileId {
        let id = FileId(self.files.len());
        let relative_path = relative_posix_path(&self.project_root, path);
        let line_starts = compute_line_starts(&text);
        let newline = detect_newline_style(&text);
        let tokens = collect_tokens(&text);
        let comments = collect_comments(&text, &tokens, &line_starts);

        let mut file = SourceFile {
            id,
            path: path.to_path_buf(),
            relative_path,
            text,
            encoding,
            newline,
            line_starts,
            ast: None,
            tokens,
            comments,
            diagnostic: None,
        };
        match parse(&file.text, Mode::Module, &file.path.display().to_string()) {
            Ok(ast::Mod::Module(module)) => file.ast = Some(module),
            Ok(_) => unreachable!("Mode::Module always produces Mod::Module"),
            Err(error) => {
                let start = file.position_of_byte(error.offset.into());
                let span = SourceSpan { start, end: start };
                file.diagnostic = Some(syntax_error_diagnostic(
                    &file.relative_path,
                    span,
                    &format!("syntax error: {}", error.error),
                ));
            }
        }
        self.files.push(file);
        id
    }

    fn register_failure(&mut self, path: &Path, message: &str) -> FileId {
        let id = FileId(self.files.len());
        let relative_path = relative_posix_path(&self.project_root, path);
        let position = SourcePosition { line: 1, column: 1 };
        let span = SourceSpan {
            start: position,
            end: position,
        };
        let diagnostic = syntax_error_diagnostic(&relative_path, span, message);
        self.files.push(SourceFile {
            id,
            path: path.to_path_buf(),
            relative_path,
            text: String::new(),
            encoding: SourceEncoding {
                label: "unknown".to_owned(),
                byte_order_mark: false,
            },
            newline: NewlineStyle::Lf,
            line_starts: vec![0],
            ast: None,
            tokens: Vec::new(),
            comments: Vec::new(),
            diagnostic: Some(diagnostic),
        });
        id
    }
}

fn syntax_error_diagnostic(relative_path: &str, span: SourceSpan, message: &str) -> Diagnostic {
    let metadata = &registry::SYNTAX_ERROR;
    Diagnostic {
        rule_id: metadata.id.to_owned(),
        severity: metadata.default_severity,
        confidence: metadata.minimum_confidence,
        path: relative_path.to_owned(),
        primary_span: span,
        message: message.to_owned(),
        explanation: Some(
            "The file cannot be analyzed and is skipped; every other selected file is still analyzed."
                .to_owned(),
        ),
        related_locations: Vec::new(),
        evidence: std::collections::BTreeMap::new(),
        estimated_cost: None,
        applicable_profiles: Vec::new(),
        fix: None,
    }
}

/// Converts a path to a POSIX-style string relative to `root` when possible.
#[must_use]
pub fn relative_posix_path(root: &Path, path: &Path) -> String {
    let relative = pathdiff::diff_paths(path, root).unwrap_or_else(|| path.to_path_buf());
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

/// Decodes Python source bytes per PEP 263: UTF-8 by default, honoring a
/// `# -*- coding: ... -*-` declaration in the first two lines and a UTF-8 BOM.
fn decode_python_source(bytes: &[u8]) -> Result<(String, SourceEncoding), String> {
    const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
    let (bytes, byte_order_mark) = match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };

    let declared = if byte_order_mark {
        None
    } else {
        detect_coding_declaration(bytes)
    };
    match declared {
        Some(label) => {
            let Some(encoding) = resolve_declared_encoding(&label) else {
                // MLC000's contract is "the configured target Python cannot
                // decode this file" (DESIGN §7.1). A codec we cannot map is
                // reported as a linter limitation, not as a decode failure
                // CPython would raise (DESIGN §15.2 / AGENTS rule 4).
                return Err(format!(
                    "source encoding {label} is not supported by manim-lint; the file is skipped"
                ));
            };
            if encoding == encoding_rs::UTF_8 {
                return decode_strict_utf8(bytes, byte_order_mark);
            }
            let (text, actual, had_errors) = encoding.decode(bytes);
            if had_errors {
                return Err(format!("cannot decode file with declared encoding {label}"));
            }
            Ok((
                text.into_owned(),
                SourceEncoding {
                    label: actual.name().to_ascii_lowercase(),
                    byte_order_mark,
                },
            ))
        }
        None => decode_strict_utf8(bytes, byte_order_mark),
    }
}

/// Resolves a PEP 263 encoding label to a decoder, accepting both WHATWG
/// labels and `CPython` codec names/aliases (`latin-1`, `cp932`, ...) for
/// codecs `encoding_rs` can represent.
///
/// The declared label is what the *target Python* resolves through its own
/// codec alias table, so spellings like `latin-1` (WHATWG only knows
/// `latin1`) or `cp932` (WHATWG calls it `shift_jis`/`ms932`) are valid
/// sources that must decode, not MLC000 errors (DESIGN §7.1).
fn resolve_declared_encoding(label: &str) -> Option<&'static encoding_rs::Encoding> {
    if let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) {
        return Some(encoding);
    }
    // CPython `encodings.normalize_encoding`: lowercase, runs of
    // non-alphanumeric characters collapse to a single `_`.
    let mut normalized = String::with_capacity(label.len());
    let mut pending_separator = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_separator && !normalized.is_empty() {
                normalized.push('_');
            }
            pending_separator = false;
            normalized.push(ch.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
    }
    // CPython codec names and aliases whose spelling no WHATWG label
    // covers, mapped onto the closest encoding_rs decoder.
    let mapped: Option<&str> = match normalized.as_str() {
        "latin" | "latin_1" | "l1" | "8859" | "iso8859" | "cp819" => Some("iso-8859-1"),
        "cp932" | "ms_kanji" | "mskanji" | "windows_31j" | "shiftjis" => Some("shift_jis"),
        "cp936" | "ms936" | "euc_cn" | "euccn" | "eucgb2312_cn" => Some("gbk"),
        "cp950" | "big5_tw" => Some("big5"),
        "cp949" | "ms949" | "uhc" | "euckr" | "ks_c_5601" | "ks_x_1001" => Some("euc-kr"),
        "eucjp" | "ujis" | "u_jis" => Some("euc-jp"),
        "iso2022_jp" | "iso2022jp" => Some("iso-2022-jp"),
        "cp874" | "tis620" | "tis_620" => Some("windows-874"),
        "mac_roman" | "macroman" => Some("macintosh"),
        "mac_cyrillic" | "maccyrillic" => Some("x-mac-cyrillic"),
        "u8" | "cp65001" => Some("utf-8"),
        _ => None,
    };
    if let Some(mapped) = mapped {
        return encoding_rs::Encoding::for_label(mapped.as_bytes());
    }
    // The remaining CPython spellings differ from a WHATWG label only in
    // using `_` where WHATWG uses `-` (`iso_8859_5`, `euc_kr`, `koi8_r`,
    // `utf_8`, `us_ascii`, `shift_jis`, ...).
    let hyphenated = normalized.replace('_', "-");
    encoding_rs::Encoding::for_label(hyphenated.as_bytes())
}

fn decode_strict_utf8(
    bytes: &[u8],
    byte_order_mark: bool,
) -> Result<(String, SourceEncoding), String> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok((
            text.to_owned(),
            SourceEncoding {
                label: "utf-8".to_owned(),
                byte_order_mark,
            },
        )),
        Err(error) => Err(format!(
            "cannot decode file as utf-8 (invalid byte at offset {})",
            error.valid_up_to()
        )),
    }
}

/// Finds a PEP 263 `coding: name` declaration in the first two lines.
fn detect_coding_declaration(bytes: &[u8]) -> Option<String> {
    for line in bytes.split(|byte| *byte == b'\n').take(2) {
        let trimmed = trim_ascii(line);
        if !trimmed.starts_with(b"#") {
            continue;
        }
        if let Some(label) = extract_coding_label(trimmed) {
            return Some(label);
        }
    }
    None
}

fn extract_coding_label(comment: &[u8]) -> Option<String> {
    let marker = b"coding";
    let position = comment
        .windows(marker.len())
        .position(|window| window == marker)?;
    let mut rest = &comment[position + marker.len()..];
    let first = rest.first()?;
    if *first != b':' && *first != b'=' {
        return None;
    }
    rest = &rest[1..];
    while rest
        .first()
        .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
    {
        rest = &rest[1..];
    }
    let end = rest
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric() && !b"-_.".contains(byte))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

/// Byte offsets of every physical line start, honoring `\n`, `\r\n`, and `\r`.
fn compute_line_starts(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut starts = vec![0];
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => starts.push(index + 1),
            b'\r' => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
                starts.push(index + 1);
            }
            _ => {}
        }
        index += 1;
    }
    starts
}

fn detect_newline_style(text: &str) -> NewlineStyle {
    let bytes = text.as_bytes();
    let mut lf = false;
    let mut crlf = false;
    let mut cr = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    crlf = true;
                    index += 1;
                } else {
                    cr = true;
                }
            }
            b'\n' => lf = true,
            _ => {}
        }
        index += 1;
    }
    match (lf, crlf, cr) {
        (false, true, false) => NewlineStyle::CrLf,
        (false, false, true) => NewlineStyle::Cr,
        (_, false, false) => NewlineStyle::Lf,
        _ => NewlineStyle::Mixed,
    }
}

/// Lexes the file, keeping tokens up to the first lexical error.
///
/// The `full-lexer` feature makes the lexer emit [`Tok::Comment`] tokens, so
/// no separate comment scanner is needed.
fn collect_tokens(text: &str) -> Vec<(Tok, TextRange)> {
    let mut tokens = Vec::new();
    for item in lex(text, Mode::Module) {
        match item {
            Ok(token) => tokens.push(token),
            Err(_) => break,
        }
    }
    tokens
}

fn collect_comments(
    text: &str,
    tokens: &[(Tok, TextRange)],
    line_starts: &[usize],
) -> Vec<Comment> {
    let mut comments = Vec::new();
    for (token, range) in tokens {
        let Tok::Comment(comment_text) = token else {
            continue;
        };
        let start = usize::from(range.start());
        let line_index = line_starts
            .partition_point(|line| *line <= start)
            .saturating_sub(1);
        let line_start = line_starts[line_index];
        let own_line = text[line_start..start].chars().all(char::is_whitespace);
        comments.push(Comment {
            text: comment_text.clone(),
            range: *range,
            line: line_index + 1,
            own_line,
        });
    }
    comments
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::ast::Ranged;

    fn manager() -> SourceManager {
        SourceManager::new("/project")
    }

    fn load(text: &str) -> SourceManager {
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/scene.py"), text.as_bytes());
        sources
    }

    #[test]
    fn japanese_byte_columns_round_trip_to_character_columns() {
        // `こんにちは` occupies 15 UTF-8 bytes but 5 characters.
        let sources = load("x = \"こんにちは\"\ny = 1\n");
        let file = &sources.files()[0];

        // Byte column of the closing quote on line 1: 5 + 15 = byte 20.
        let char_offset = file.char_offset(1, 20).expect("valid position");
        assert_eq!(char_offset, 10, "5 ASCII + quote + 5 chars - 1");
        let position = file.position_of_char_offset(char_offset);
        assert_eq!(
            position,
            SourcePosition {
                line: 1,
                column: 11
            }
        );

        // Round-trip every character boundary in the file.
        for (byte, _) in file.text().char_indices() {
            let position = file.position_of_byte(byte);
            let line_start = file.char_offset(position.line, 0).expect("line start");
            let absolute = line_start + position.column - 1;
            assert_eq!(file.position_of_char_offset(absolute), position);
        }
    }

    #[test]
    fn syntax_error_after_japanese_text_reports_character_column() {
        let sources = load("x = \"こんにちは\"; def = 1\n");
        let file = &sources.files()[0];
        let diagnostic = file.parse_diagnostic().expect("syntax error");
        assert_eq!(diagnostic.rule_id, "MLC000");
        assert_eq!(diagnostic.primary_span.start.line, 1);
        // `def` starts at byte 23 of the line but there are only 13
        // characters before it, so the display column is 14.
        assert_eq!(diagnostic.primary_span.start.column, 14);
    }

    #[test]
    fn rejects_byte_column_inside_multibyte_character() {
        let sources = load("x = \"こんにちは\"\n");
        let file = &sources.files()[0];
        assert!(file.char_offset(1, 6).is_none(), "middle of こ");
        assert!(file.char_offset(1, 5).is_some(), "start of こ");
    }

    #[test]
    fn decodes_declared_shift_jis() {
        let mut bytes = b"# -*- coding: shift_jis -*-\nx = \"".to_vec();
        bytes.extend([0x82, 0xa0]); // あ in Shift_JIS
        bytes.extend(b"\"\n");
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/sjis.py"), &bytes);
        let file = &sources.files()[0];
        assert!(file.is_parsed());
        assert!(file.text().contains('あ'));
        assert_eq!(file.encoding().label, "shift_jis");
    }

    #[test]
    fn undecodable_file_gets_mlc000_and_no_ast() {
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/bad.py"), &[0xff, 0xfe, 0x00]);
        let file = &sources.files()[0];
        assert!(!file.is_parsed());
        let diagnostic = file.parse_diagnostic().expect("decode failure");
        assert_eq!(diagnostic.rule_id, "MLC000");
        assert_eq!(
            diagnostic.primary_span.start,
            SourcePosition { line: 1, column: 1 }
        );
    }

    #[test]
    fn unknown_declared_encoding_is_a_decode_failure() {
        let mut sources = manager();
        sources.load_bytes(
            Path::new("/project/enc.py"),
            b"# -*- coding: not-an-encoding -*-\nx = 1\n",
        );
        let file = &sources.files()[0];
        assert!(!file.is_parsed());
        // The message reports a linter limitation, not a decode failure the
        // target Python would raise (DESIGN §7.1, §15.2).
        let diagnostic = file.parse_diagnostic().expect("unsupported encoding");
        assert!(diagnostic.message.contains("not supported by manim-lint"));
    }

    #[test]
    fn decodes_python_codec_aliases_latin_1_and_cp932() {
        // `latin-1` is CPython's alias spelling; WHATWG only knows
        // `latin1`. Verified against CPython 3.11: tokenize.open decodes
        // and ast.parse parses this file, so MLC000 must stay silent.
        let mut sources = manager();
        sources.load_bytes(
            Path::new("/project/latin.py"),
            b"# -*- coding: latin-1 -*-\n# caf\xe9\nfrom manim import *\n",
        );
        let file = &sources.files()[0];
        assert!(file.is_parsed(), "latin-1 must decode");
        assert!(file.text().contains("café"));

        // `cp932` is CPython's canonical name for Windows Shift_JIS;
        // WHATWG's shift_jis label set omits it.
        let mut bytes = b"# -*- coding: cp932 -*-\nx = \"".to_vec();
        bytes.extend([0x82, 0xa0]); // あ in cp932
        bytes.extend(b"\"\n");
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/cp932.py"), &bytes);
        let file = &sources.files()[0];
        assert!(file.is_parsed(), "cp932 must decode");
        assert!(file.text().contains('あ'));

        // Underscore spellings of hyphenated WHATWG labels resolve too.
        let mut sources = manager();
        sources.load_bytes(
            Path::new("/project/koi8.py"),
            b"# -*- coding: koi8_r -*-\nx = 1\n",
        );
        assert!(sources.files()[0].is_parsed(), "koi8_r must decode");
    }

    #[test]
    fn newline_styles_are_preserved() {
        assert_eq!(detect_newline_style("a\nb\n"), NewlineStyle::Lf);
        assert_eq!(detect_newline_style("a\r\nb\r\n"), NewlineStyle::CrLf);
        assert_eq!(detect_newline_style("a\rb\r"), NewlineStyle::Cr);
        assert_eq!(detect_newline_style("a\r\nb\n"), NewlineStyle::Mixed);
        assert_eq!(detect_newline_style("a"), NewlineStyle::Lf);
    }

    #[test]
    fn comments_are_collected_with_own_line_flag() {
        let sources = load("# header\nx = 1  # trailing\n");
        let file = &sources.files()[0];
        let comments = file.comments();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].text, "# header");
        assert!(comments[0].own_line);
        assert_eq!(comments[1].line, 2);
        assert!(!comments[1].own_line);
    }

    #[test]
    fn ast_span_maps_to_source_slice() {
        let sources = load("x = \"あい\"\n");
        let file = &sources.files()[0];
        let module = file.ast().expect("parsed");
        let stmt = &module.body[0];
        assert_eq!(file.slice(stmt.range()), "x = \"あい\"");
        let span = file.span_of_range(stmt.range());
        assert_eq!(span.start, SourcePosition { line: 1, column: 1 });
        assert_eq!(span.end, SourcePosition { line: 1, column: 9 });
    }
}
