//! Source loading, decoding, parsing, and span conversion (DESIGN §5.2).
//!
//! Python AST columns are UTF-8 byte offsets while every external report uses
//! one-based Unicode character columns. All conversions between those views
//! live here so that later phases never re-implement them.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustpython_parser::lexer::lex;
use rustpython_parser::text_size::TextRange;
use rustpython_parser::{Mode, Tok, ast, parse};
use serde::{Deserialize, Serialize};

use crate::diagnostic::{Diagnostic, SourcePosition, SourceSpan};
use crate::rules::registry;
use crate::source_limits::{check_source_bytes, check_source_length, check_token_nesting};

/// Stable handle for a file registered in a [`SourceManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
    ///
    /// The size and file-type admission checks run on the directory entry
    /// before any read: `fs::read` on a FIFO blocks forever and on a character
    /// device never ends, so a limit applied to the returned bytes would come
    /// too late.
    pub fn load_file(&mut self, path: &Path) -> FileId {
        let file = build_from_disk(FileId(self.files.len()), &self.project_root, path);
        let id = file.id;
        self.files.push(file);
        id
    }

    /// Registers an in-memory byte stream as if it had been read from `path`.
    ///
    /// A source that exceeds an admission limit is recorded as `MLC000` and
    /// never reaches the parser.
    pub fn load_bytes(&mut self, path: &Path, bytes: &[u8]) -> FileId {
        let file = build_from_bytes(FileId(self.files.len()), &self.project_root, path, bytes);
        let id = file.id;
        self.files.push(file);
        id
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

    /// Loads many files, decoding and parsing them in parallel.
    ///
    /// Files are registered in the order given, so `FileId`s — and every
    /// ordering derived from them — are identical to loading one at a time.
    /// Each file is built from its own bytes and the project root alone, which
    /// is what makes the parallelism sound.
    pub fn load_all<'a>(
        &mut self,
        inputs: impl IntoIterator<Item = (&'a Path, Option<&'a [u8]>)>,
    ) -> Vec<FileId> {
        let inputs: Vec<(&Path, Option<&[u8]>)> = inputs.into_iter().collect();
        let base = self.files.len();
        let built: Vec<SourceFile> = inputs
            .par_iter()
            .enumerate()
            .map(|(offset, (path, bytes))| {
                let id = FileId(base + offset);
                match bytes {
                    Some(bytes) => build_from_bytes(id, &self.project_root, path, bytes),
                    None => build_from_disk(id, &self.project_root, path),
                }
            })
            .collect();
        let ids = built.iter().map(|file| file.id).collect();
        self.files.extend(built);
        ids
    }
}

/// Reads, decodes, and parses one file, honoring the admission limits.
fn build_from_disk(id: FileId, project_root: &Path, path: &Path) -> SourceFile {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return build_failure(
                    id,
                    project_root,
                    path,
                    "cannot read file: not a regular file (a directory, symlink target, \
                     socket, device, or FIFO cannot be analyzed)",
                );
            }
            if let Err(violation) = check_source_length(metadata.len()) {
                return build_failure(id, project_root, path, &violation.message());
            }
        }
        Err(error) => {
            return build_failure(
                id,
                project_root,
                path,
                &format!("cannot read file: {error}"),
            );
        }
    }
    match std::fs::read(path) {
        Ok(bytes) => build_from_bytes(id, project_root, path, &bytes),
        Err(error) => build_failure(
            id,
            project_root,
            path,
            &format!("cannot read file: {error}"),
        ),
    }
}

/// Decodes and parses one in-memory byte stream.
fn build_from_bytes(id: FileId, project_root: &Path, path: &Path, bytes: &[u8]) -> SourceFile {
    if let Err(violation) = check_source_bytes(bytes) {
        return build_failure(id, project_root, path, &violation.message());
    }
    match decode_python_source(bytes) {
        Ok((text, encoding)) => build_parsed(id, project_root, path, text, encoding),
        Err(message) => build_failure(id, project_root, path, &message),
    }
}

/// Tokenizes and parses decoded text into a registered file.
fn build_parsed(
    id: FileId,
    project_root: &Path,
    path: &Path,
    text: String,
    encoding: SourceEncoding,
) -> SourceFile {
    let relative_path = relative_posix_path(project_root, path);
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
    if let Err(violation) = check_token_nesting(&file.tokens) {
        // Parsing this file would recurse past the native stack and abort
        // the process, so the file is skipped while every other file is
        // still analyzed.
        let position = SourcePosition { line: 1, column: 1 };
        file.diagnostic = Some(syntax_error_diagnostic(
            &file.relative_path,
            SourceSpan {
                start: position,
                end: position,
            },
            &violation.message(),
        ));
        return file;
    }
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
    file
}

/// Builds a file that carries only an `MLC000` load failure.
fn build_failure(id: FileId, project_root: &Path, path: &Path, message: &str) -> SourceFile {
    let relative_path = relative_posix_path(project_root, path);
    let position = SourcePosition { line: 1, column: 1 };
    let span = SourceSpan {
        start: position,
        end: position,
    };
    let diagnostic = syntax_error_diagnostic(&relative_path, span, message);
    SourceFile {
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
/// UTF-8 BOM and a `# -*- coding: ... -*-` declaration on line 1, or on
/// line 2 when line 1 is blank or comment-only (`CPython`'s exact rule).
fn decode_python_source(bytes: &[u8]) -> Result<(String, SourceEncoding), String> {
    const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
    let (bytes, byte_order_mark) = match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };

    match detect_coding_declaration(bytes) {
        Some(label) => {
            let Some(codec) = resolve_declared_encoding(&label) else {
                // MLC000's contract is "the configured target Python cannot
                // decode this file" (DESIGN §7.1). A codec we cannot map is
                // reported as a linter limitation, not as a decode failure
                // CPython would raise (DESIGN §15.2 / AGENTS rule 4).
                return Err(format!(
                    "source encoding {label} is not supported by manim-lint; the file is skipped"
                ));
            };
            // CPython refuses a UTF-8 BOM combined with any coding
            // declaration that does not resolve to UTF-8: compiling such a
            // file raises `SyntaxError: encoding problem: <codec> with BOM`
            // (verified with CPython 3.12 `compile`/`tokenize`). Only a
            // UTF-8 cookie may coexist with the BOM.
            if byte_order_mark && !matches!(codec, DeclaredCodec::Utf8) {
                return Err(format!(
                    "declared encoding {label} conflicts with the UTF-8 byte \
                     order mark; Python rejects this file (encoding problem: \
                     {label} with BOM)"
                ));
            }
            match codec {
                DeclaredCodec::Utf8 => decode_strict_utf8(bytes, byte_order_mark),
                DeclaredCodec::Latin1 => Ok((
                    decode_latin_1(bytes),
                    SourceEncoding {
                        label: "latin-1".to_owned(),
                        byte_order_mark,
                    },
                )),
                DeclaredCodec::WhatwgC1Controls {
                    encoding,
                    canonical,
                } => match decode_single_byte_with_c1_controls(bytes, encoding) {
                    Some(text) => Ok((
                        text,
                        SourceEncoding {
                            label: canonical.to_owned(),
                            byte_order_mark,
                        },
                    )),
                    None => Err(format!("cannot decode file with declared encoding {label}")),
                },
                DeclaredCodec::Whatwg(encoding) => {
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
            }
        }
        None => decode_strict_utf8(bytes, byte_order_mark),
    }
}

/// How a resolved PEP 263 coding declaration decodes bytes.
enum DeclaredCodec {
    /// Strict UTF-8 (the label resolves to `CPython`'s `utf-8` codec).
    Utf8,
    /// True ISO-8859-1: every byte maps 1:1 to U+0000..=U+00FF.
    Latin1,
    /// A WHATWG decoder from `encoding_rs`.
    Whatwg(&'static encoding_rs::Encoding),
    /// A single-byte WHATWG decoder whose only divergence from the
    /// `CPython` codec the label names is the 0x80–0x9F range: those bytes
    /// decode to the C1 controls U+0080–U+009F instead of the WHATWG
    /// index entries. `canonical` is the label stored on the decoded file
    /// (and matched by the fix re-encoder).
    WhatwgC1Controls {
        /// Underlying WHATWG decoder for every byte outside 0x80–0x9F.
        encoding: &'static encoding_rs::Encoding,
        /// Canonical label, e.g. `iso-8859-9`.
        canonical: &'static str,
    },
}

/// True ISO-8859-1 (`CPython`'s `latin-1` codec): each byte is the code
/// point U+0000..=U+00FF; decoding never fails.
///
/// `encoding_rs` cannot express this — in WHATWG, the `iso-8859-1` /
/// `latin1` labels all mean windows-1252, which decodes 0x80–0x9F to
/// punctuation (0x80 → U+20AC) instead of the C1 controls `CPython`
/// produces — so the 1:1 mapping is implemented directly.
fn decode_latin_1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// Decodes a single-byte WHATWG encoding with 0x80–0x9F overridden to the
/// C1 controls U+0080–U+009F, or `None` when any byte outside that range
/// fails to decode.
///
/// This reproduces `CPython`'s `iso8859-9` / `iso8859-11` codecs exactly:
/// the WHATWG label space resolves those names to windows-1254 /
/// windows-874, which were verified byte-for-byte (including which bytes
/// are decode errors) to differ from the `CPython` codecs only in
/// 0x80–0x9F. Chunking at C1 bytes is sound because both decoders are
/// stateless single-byte codecs.
fn decode_single_byte_with_c1_controls(
    bytes: &[u8],
    encoding: &'static encoding_rs::Encoding,
) -> Option<String> {
    let is_c1 = |byte: u8| (0x80..=0x9f).contains(&byte);
    let mut text = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        let run_end = rest
            .iter()
            .position(|byte| is_c1(*byte))
            .unwrap_or(rest.len());
        if run_end > 0 {
            let (decoded, had_errors) = encoding.decode_without_bom_handling(&rest[..run_end]);
            if had_errors {
                return None;
            }
            text.push_str(&decoded);
            rest = &rest[run_end..];
        }
        while let Some((&byte, tail)) = rest.split_first() {
            if !is_c1(byte) {
                break;
            }
            text.push(char::from(byte));
            rest = tail;
        }
    }
    Some(text)
}

/// Resolves a PEP 263 encoding label to a decoder, accepting both WHATWG
/// labels and `CPython` codec names/aliases (`latin-1`, `cp932`, ...) for
/// codecs manim-lint can represent.
///
/// The declared label is what the *target Python* resolves through its own
/// codec alias table, so spellings like `latin-1` (WHATWG only knows
/// `latin1`) or `cp932` (WHATWG calls it `shift_jis`/`ms932`) are valid
/// sources that must decode, not MLC000 errors (DESIGN §7.1).
fn resolve_declared_encoding(label: &str) -> Option<DeclaredCodec> {
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
    // CPython's latin-1 alias family (`Lib/encodings/aliases.py` entries
    // for `latin_1`, plus the codec name itself) decodes as true Latin-1.
    // This must be checked before any WHATWG lookup: WHATWG maps `latin1`,
    // `iso-8859-1`, `l1`, ... to windows-1252, whose 0x80–0x9F differ
    // from CPython's decode. `cp1252` spellings intentionally stay
    // windows-1252 — that is what CPython's `cp1252` codec is.
    if matches!(
        normalized.as_str(),
        "latin"
            | "latin1"
            | "latin_1"
            | "l1"
            | "8859"
            | "iso8859"
            | "iso8859_1"
            | "iso_8859_1"
            | "iso_8859_1_1987"
            | "iso_ir_100"
            | "cp819"
            | "ibm819"
            | "csisolatin1"
    ) {
        return Some(DeclaredCodec::Latin1);
    }
    // CPython's iso8859-9 and iso8859-11 codecs (alias sets from
    // `Lib/encodings/aliases.py`, plus the codec names). WHATWG resolves
    // several of these labels (`latin5`, `l5`, `iso-ir-148`, ...) to
    // windows-1254 / windows-874, whose 0x80–0x9F decode to punctuation
    // instead of CPython's C1 controls — verified byte-for-byte that the
    // C1 range is the ONLY divergence, so these decode through the WHATWG
    // codec with a C1 override (see `decode_single_byte_with_c1_controls`).
    // `cp1254` / `cp874` spellings intentionally keep their plain WHATWG
    // meaning — that is what those CPython codecs are closest to.
    if matches!(
        normalized.as_str(),
        "iso8859_9"
            | "iso_8859_9"
            | "iso_8859_9_1989"
            | "iso_ir_148"
            | "l5"
            | "latin5"
            | "csisolatin5"
    ) {
        return Some(DeclaredCodec::WhatwgC1Controls {
            encoding: encoding_rs::WINDOWS_1254,
            canonical: "iso-8859-9",
        });
    }
    if matches!(
        normalized.as_str(),
        "iso8859_11" | "iso_8859_11" | "iso_8859_11_2001" | "thai"
    ) {
        return Some(DeclaredCodec::WhatwgC1Controls {
            encoding: encoding_rs::WINDOWS_874,
            canonical: "iso-8859-11",
        });
    }
    resolve_whatwg_encoding(label, &normalized).map(|encoding| {
        if encoding == encoding_rs::UTF_8 {
            DeclaredCodec::Utf8
        } else {
            DeclaredCodec::Whatwg(encoding)
        }
    })
}

/// WHATWG-side resolution of a non-latin-1 label: the raw label first,
/// then `CPython`-only alias spellings, then `_` → `-` respelling.
fn resolve_whatwg_encoding(
    label: &str,
    normalized: &str,
) -> Option<&'static encoding_rs::Encoding> {
    if let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) {
        return Some(encoding);
    }
    // CPython codec names and aliases whose spelling no WHATWG label
    // covers, mapped onto the closest encoding_rs decoder.
    let mapped: Option<&str> = match normalized {
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
        // CPython-only spellings of the ISO-8859 family (aliases.py).
        // Safe because the WHATWG decoders for these targets were verified
        // byte-for-byte identical to the CPython codecs, C1 range and
        // error positions included (see the iso-8859 tests below).
        "iso_8859_2_1987" => Some("iso-8859-2"),
        "iso_8859_3_1988" => Some("iso-8859-3"),
        "iso_8859_4_1988" => Some("iso-8859-4"),
        "iso_8859_5_1988" => Some("iso-8859-5"),
        "iso_8859_6_1987" => Some("iso-8859-6"),
        "iso_8859_7_1987" => Some("iso-8859-7"),
        "iso_8859_8_1988" => Some("iso-8859-8"),
        "iso_8859_10_1992" => Some("iso-8859-10"),
        "l7" | "latin7" => Some("iso-8859-13"),
        "l8" | "latin8" | "iso_celtic" | "iso_ir_199" | "iso_8859_14_1998" => Some("iso-8859-14"),
        "latin9" => Some("iso-8859-15"),
        "l10" | "latin10" | "iso_ir_226" | "iso_8859_16_2001" => Some("iso-8859-16"),
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

/// Finds a PEP 263 `coding: name` declaration, mirroring `CPython`'s
/// `tokenize.detect_encoding` exactly: the cookie is looked for on line 1;
/// line 2 is examined only when line 1 is blank or comment-only
/// (`blank_re` = `^[ \t\f]*(?:[#\r\n]|$)`). A cookie after a code line —
/// `x = 1` then `# coding: latin-1` — is NOT honored (verified against
/// `CPython` 3.12 `tokenize.detect_encoding` and `compile`).
///
/// `CPython` also raises `SyntaxError` when an examined line is not valid
/// UTF-8; returning no cookie here reaches the same per-file `MLC000`
/// through the strict UTF-8 decode of the whole file.
fn detect_coding_declaration(bytes: &[u8]) -> Option<String> {
    let mut lines = bytes.split(|byte| *byte == b'\n');
    let first = lines.next().unwrap_or(b"");
    if std::str::from_utf8(first).is_err() {
        return None;
    }
    if let Some(label) = find_cookie(first) {
        return Some(label);
    }
    if !is_blank_or_comment_line(first) {
        return None;
    }
    let second = lines.next()?;
    if std::str::from_utf8(second).is_err() {
        return None;
    }
    find_cookie(second)
}

/// `CPython`'s `tokenize.blank_re`, `^[ \t\f]*(?:[#\r\n]|$)`: the only
/// first lines whose successor may carry the cookie. The `\n` alternative
/// cannot occur here because lines are already split on `\n`; a line
/// produced from `\r\n` still ends with (or, when empty, is) `\r`.
fn is_blank_or_comment_line(line: &[u8]) -> bool {
    let rest = skip_cookie_whitespace(line);
    matches!(rest.first(), None | Some(b'#' | b'\r'))
}

/// One line of `CPython`'s `tokenize.cookie_re`,
/// `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)`: an own-line comment whose
/// text contains `coding:` or `coding=` followed by a codec label.
fn find_cookie(line: &[u8]) -> Option<String> {
    let comment = skip_cookie_whitespace(line);
    if comment.first() != Some(&b'#') {
        return None;
    }
    extract_coding_label(comment)
}

/// Leading whitespace `cookie_re` and `blank_re` accept: space, tab, and
/// form feed only (NOT `\v` or `\r` — verified against `CPython`, where a
/// `\v`-indented comment line carries no cookie).
fn skip_cookie_whitespace(line: &[u8]) -> &[u8] {
    let start = line
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\x0c'))
        .unwrap_or(line.len());
    &line[start..]
}

/// Emulates `.*?coding[:=][ \t]*([-\w.]+)` with regex backtracking: every
/// `coding` occurrence is tried in order until one is followed by `:`/`=`
/// and a non-empty label (`# coding: , coding=latin-1` matches `latin-1`,
/// verified against `CPython`). The label charset is the ASCII subset of
/// `[-\w.]`; a non-ASCII label `CPython` would reject as an unknown
/// encoding is treated as no cookie.
fn extract_coding_label(comment: &[u8]) -> Option<String> {
    let marker = b"coding";
    let mut search_start = 0;
    while let Some(position) = find_subslice(&comment[search_start..], marker) {
        let after_marker = search_start + position + marker.len();
        search_start += position + 1;
        let mut rest = &comment[after_marker..];
        if !matches!(rest.first(), Some(b':' | b'=')) {
            continue;
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
        if end > 0 {
            return Some(String::from_utf8_lossy(&rest[..end]).into_owned());
        }
    }
    None
}

/// First offset of `needle` in `haystack`, if any.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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

    /// `CPython`'s `latin-1` codec is true ISO-8859-1: bytes 0x80–0x9F are
    /// the C1 controls U+0080–U+009F. The WHATWG `iso-8859-1` label means
    /// windows-1252 (0x80 → U+20AC '€'), so decoding through `encoding_rs`
    /// would diverge from what the target Python sees.
    #[test]
    fn latin_1_family_decodes_c1_range_like_cpython() {
        for cookie in ["latin-1", "latin_1", "iso-8859-1", "L1", "cp819"] {
            let mut bytes = format!("# -*- coding: {cookie} -*-\nx = \"").into_bytes();
            bytes.extend(0x80..=0x9f_u8);
            bytes.extend(b"\"\n");
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/latin.py"), &bytes);
            let file = &sources.files()[0];
            assert!(file.is_parsed(), "{cookie} must decode");
            assert_eq!(file.encoding().label, "latin-1");
            let expected: String = ('\u{80}'..='\u{9f}').collect();
            assert!(
                file.text().contains(&expected),
                "{cookie}: 0x80–0x9F must decode to U+0080–U+009F"
            );
            assert!(
                !file.text().contains('\u{20ac}'),
                "{cookie}: 0x80 must not decode to windows-1252 '€'"
            );
        }
    }

    /// `CPython` only examines line 2 for the cookie when line 1 is blank
    /// or comment-only (`tokenize.blank_re`); after a code line the cookie
    /// is ignored (verified against `CPython` 3.12 `detect_encoding`).
    #[test]
    fn cookie_after_a_code_line_is_not_honored() {
        // Pure ASCII: the file stays UTF-8, the cookie is dead text.
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/late.py"), b"x = 1\n# coding: latin-1\n");
        let file = &sources.files()[0];
        assert!(file.is_parsed());
        assert_eq!(file.encoding().label, "utf-8");

        // With a latin-1 byte later on, CPython raises (the file is
        // decoded as UTF-8); manim-lint reports the decode MLC000.
        let mut sources = manager();
        sources.load_bytes(
            Path::new("/project/late.py"),
            b"x = 1\n# coding: latin-1\n# caf\xe9\n",
        );
        let file = &sources.files()[0];
        assert!(!file.is_parsed(), "the ignored cookie must not decode");
        assert_eq!(
            file.parse_diagnostic().expect("decode failure").rule_id,
            "MLC000"
        );
    }

    /// Line 1 variants `blank_re` accepts: the line-2 cookie is honored
    /// after a comment, an empty line, whitespace (space/tab/form feed),
    /// a bare `\r` (from `\r\n`), and a shebang.
    #[test]
    fn cookie_on_line_two_is_honored_after_blank_or_comment_lines() {
        for first_line in [
            b"# hi\n".as_slice(),
            b"\n",
            b"  \t\x0c\n",
            b"\r\n",
            b"#!/usr/bin/env python\n",
        ] {
            let mut bytes = first_line.to_vec();
            bytes.extend(b"# coding: latin-1\n# caf\xe9\n");
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/two.py"), &bytes);
            let file = &sources.files()[0];
            // The decode is what is under test: rustpython's lexer happens
            // to reject the tab-after-space indentation line, but the
            // cookie must be honored for every blank_re-matching line 1.
            assert_eq!(
                file.encoding().label,
                "latin-1",
                "cookie after {first_line:?} must be honored"
            );
            assert!(file.text().contains("café"));
        }
    }

    /// A vertical tab is NOT in `blank_re`/`cookie_re`'s `[ \t\f]`, and
    /// line 3 is never examined (`CPython` reads at most two lines).
    #[test]
    fn cookie_after_vertical_tab_or_on_line_three_is_not_honored() {
        for bytes in [
            b"\x0b# coding: latin-1\n# caf\xe9\n".as_slice(),
            b"# a\n# b\n# coding: latin-1\n# caf\xe9\n",
        ] {
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/no.py"), bytes);
            let file = &sources.files()[0];
            assert!(!file.is_parsed(), "{bytes:?} must not honor the cookie");
        }
    }

    /// `cookie_re` backtracks over `coding` occurrences: an occurrence
    /// with no label after `:` falls through to a later `coding=`
    /// (verified against `CPython` 3.12 `detect_encoding`).
    #[test]
    fn cookie_extraction_backtracks_like_cpython() {
        let mut sources = manager();
        sources.load_bytes(
            Path::new("/project/bt.py"),
            b"# coding: , coding=latin-1\n# caf\xe9\n",
        );
        let file = &sources.files()[0];
        assert!(file.is_parsed());
        assert!(file.text().contains("café"));
        assert_eq!(file.encoding().label, "latin-1");
    }

    /// `CPython`'s `iso8859-9` codec, byte for byte: the WHATWG label
    /// space resolves `iso-8859-9` (and `latin5`, `l5`, ...) to
    /// windows-1254, whose 0x80–0x9F decode to punctuation (0x80 → '€');
    /// `CPython` decodes them to the C1 controls. Everything else is
    /// latin-1 except the six Turkish letters. Expected values generated
    /// from `CPython` 3.12 `bytes(range(0x80, 0x100)).decode("iso8859_9")`.
    #[test]
    fn iso_8859_9_decodes_all_bytes_like_cpython() {
        let expected: String = (0x80..=0xff_u32)
            .map(|byte| match byte {
                0xd0 => 'Ğ',
                0xdd => 'İ',
                0xde => 'Ş',
                0xf0 => 'ğ',
                0xfd => 'ı',
                0xfe => 'ş',
                other => char::from_u32(other).expect("latin-1 range"),
            })
            .collect();
        for cookie in [
            "iso-8859-9",
            "iso8859-9",
            "latin5",
            "L5",
            "iso-ir-148",
            "csisolatin5",
            "ISO_8859-9:1989",
        ] {
            let mut bytes = format!("# -*- coding: {cookie} -*-\nx = \"").into_bytes();
            bytes.extend(0x80..=0xff_u8);
            bytes.extend(b"\"\n");
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/tr.py"), &bytes);
            let file = &sources.files()[0];
            assert!(file.is_parsed(), "{cookie} must decode");
            assert_eq!(file.encoding().label, "iso-8859-9");
            assert!(
                file.text().contains(&expected),
                "{cookie}: full upper half must match CPython's iso8859-9"
            );
            assert!(
                !file.text().contains('\u{20ac}'),
                "{cookie}: 0x80 must not decode to windows-1254 '€'"
            );
        }
    }

    /// `CPython`'s `iso8859-11` codec: C1 controls, NBSP at 0xA0, Thai
    /// letters at 0xA1–0xDA (U+0E01..) and 0xDF–0xFB (U+0E3F..). The
    /// WHATWG counterpart windows-874 diverges only in 0x80–0x9F
    /// (verified byte for byte against `CPython` 3.12).
    #[test]
    fn iso_8859_11_decodes_like_cpython() {
        let expected: String = (0x80..=0xa0_u32)
            .filter_map(char::from_u32)
            .chain((0xa1..=0xda_u32).filter_map(|byte| char::from_u32(byte - 0xa1 + 0x0e01)))
            .chain((0xdf..=0xfb_u32).filter_map(|byte| char::from_u32(byte - 0xdf + 0x0e3f)))
            .collect();
        for cookie in ["iso-8859-11", "thai", "iso8859_11"] {
            let mut bytes = format!("# coding: {cookie}\nx = \"").into_bytes();
            bytes.extend(0x80..=0xda_u8);
            bytes.extend(0xdf..=0xfb_u8);
            bytes.extend(b"\"\n");
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/th.py"), &bytes);
            let file = &sources.files()[0];
            assert!(file.is_parsed(), "{cookie} must decode");
            assert_eq!(file.encoding().label, "iso-8859-11");
            assert!(file.text().contains(&expected), "{cookie} table mismatch");
        }
    }

    /// Bytes `CPython`'s `iso8859-11` rejects (0xDB–0xDE, 0xFC–0xFF) are a
    /// decode failure here too — never silently replaced.
    #[test]
    fn iso_8859_11_undefined_bytes_are_a_decode_failure() {
        let mut bytes = b"# coding: iso-8859-11\nx = \"".to_vec();
        bytes.push(0xdb);
        bytes.extend(b"\"\n");
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/bad_thai.py"), &bytes);
        let file = &sources.files()[0];
        assert!(!file.is_parsed());
        let diagnostic = file.parse_diagnostic().expect("undefined byte");
        assert_eq!(diagnostic.rule_id, "MLC000");
        assert!(diagnostic.message.contains("cannot decode"));
    }

    /// For every other ISO-8859-N `CPython` supports, the WHATWG decoder
    /// was verified byte-for-byte identical to the `CPython` codec —
    /// including the C1 controls and error positions — so those labels
    /// decode through `encoding_rs` unchanged. The C1 range plus one
    /// distinctive letter per encoding pin the verification.
    #[test]
    fn identical_iso_8859_family_decodes_c1_and_letters_like_cpython() {
        let cases: [(&str, u8, char); 12] = [
            ("iso-8859-2", 0xa3, 'Ł'),
            ("iso-8859-3", 0xa1, 'Ħ'),
            ("iso-8859-4", 0xff, '˙'),
            ("iso-8859-5", 0xd4, 'д'),
            ("iso-8859-6", 0xc1, 'ء'),
            ("iso-8859-7", 0xd3, 'Σ'),
            ("iso-8859-8", 0xe0, 'א'),
            ("iso-8859-10", 0xa1, 'Ą'),
            ("iso-8859-13", 0xd0, 'Š'),
            ("iso-8859-14", 0xa1, 'Ḃ'),
            ("iso-8859-15", 0xa4, '€'),
            ("iso-8859-16", 0xa4, '€'),
        ];
        for (cookie, byte, letter) in cases {
            let mut bytes = format!("# coding: {cookie}\nx = \"").into_bytes();
            bytes.extend(0x80..=0x9f_u8);
            bytes.push(byte);
            bytes.extend(b"\"\n");
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/iso.py"), &bytes);
            let file = &sources.files()[0];
            assert!(file.is_parsed(), "{cookie} must decode");
            let mut expected: String = ('\u{80}'..='\u{9f}').collect();
            expected.push(letter);
            assert!(
                file.text().contains(&expected),
                "{cookie}: C1 range + 0x{byte:02X} must match CPython"
            );
        }
    }

    /// `CPython`-only alias spellings of the verified-identical family
    /// (`latin7`..`latin10`, `iso_celtic`, year variants) resolve to the
    /// same decoders instead of being refused.
    #[test]
    fn cpython_only_iso_8859_aliases_resolve() {
        let cases: [(&str, u8, char); 5] = [
            ("latin7", 0xd0, 'Š'),
            ("latin8", 0xa1, 'Ḃ'),
            ("latin9", 0xa4, '€'),
            ("latin10", 0xa4, '€'),
            ("iso_8859_14_1998", 0xa1, 'Ḃ'),
        ];
        for (cookie, byte, letter) in cases {
            let mut bytes = format!("# coding: {cookie}\nx = \"").into_bytes();
            bytes.push(byte);
            bytes.extend(b"\"\n");
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/alias.py"), &bytes);
            let file = &sources.files()[0];
            assert!(file.is_parsed(), "{cookie} must decode");
            assert!(
                file.text().contains(letter),
                "{cookie}: 0x{byte:02X} must decode to {letter}"
            );
        }
    }

    /// `CPython` rejects a UTF-8 BOM combined with a non-UTF-8 coding
    /// cookie (`SyntaxError: encoding problem: cp932 with BOM`, verified
    /// with `CPython` 3.12); the file gets a per-file MLC000, analysis of
    /// other files continues.
    #[test]
    fn bom_with_non_utf8_cookie_is_a_decode_diagnostic() {
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend(b"# -*- coding: cp932 -*-\nx = 1\n");
        let mut sources = manager();
        sources.load_bytes(Path::new("/project/bom.py"), &bytes);
        let file = &sources.files()[0];
        assert!(!file.is_parsed());
        let diagnostic = file.parse_diagnostic().expect("BOM conflict");
        assert_eq!(diagnostic.rule_id, "MLC000");
        assert!(diagnostic.message.contains("cp932"));
        assert!(diagnostic.message.contains("byte order mark"));
    }

    /// A UTF-8 cookie (any `CPython` alias of it) may coexist with the BOM,
    /// exactly as in `CPython`.
    #[test]
    fn bom_with_utf8_cookie_is_fine() {
        for cookie in ["utf-8", "UTF8", "u8"] {
            let mut bytes = b"\xef\xbb\xbf".to_vec();
            bytes.extend(format!("# -*- coding: {cookie} -*-\nx = \"あ\"\n").into_bytes());
            let mut sources = manager();
            sources.load_bytes(Path::new("/project/bom_ok.py"), &bytes);
            let file = &sources.files()[0];
            assert!(file.is_parsed(), "{cookie} with BOM must decode");
            assert_eq!(file.encoding().label, "utf-8");
            assert!(file.encoding().byte_order_mark);
            assert!(file.text().contains('あ'));
        }
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
