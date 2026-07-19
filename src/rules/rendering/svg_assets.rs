//! `MLR118`: unsupported content in a project-local literal SVG.
//!
//! Manim delegates XML parsing to `ElementTree` / `svgelements`, but its
//! `SVGMobject` conversion only produces geometry for a narrow shape set;
//! text and images are ignored, and filter/mask/clipPath effects are not
//! converted. This rule reads only the resolved project asset and never
//! imports Manim or executes analyzed code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::rules::base::{Rule, RuleContext};

use super::{SVG_MOBJECT, assets, build_diagnostic, single_knowledge_symbol};

const MLR118: RuleMetadata = RuleMetadata {
    id: "MLR118",
    summary: "Project SVG contains content SVGMobject cannot faithfully convert",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct UnsupportedSvgContent;

impl Rule for UnsupportedSvgContent {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR118
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        let project_root = context.sources().project_root();
        let Ok(canonical_root) = project_root.canonicalize() else {
            return Vec::new();
        };
        let mut cache: BTreeMap<PathBuf, Option<Vec<String>>> = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor, _)) = single_knowledge_symbol(knowledge, &call.candidates)
            else {
                continue;
            };
            if constructor != SVG_MOBJECT || call.has_star_args || call.has_star_star_kwargs {
                continue;
            }
            let Some(argument) = call.keyword("file_name").or_else(|| call.positional(0)) else {
                continue;
            };
            let Some(LiteralFact::Str {
                value,
                prefix,
                range,
            }) = &argument.literal
            else {
                continue;
            };
            if prefix.bytes || value.is_empty() {
                continue;
            }
            let mut resolved: Option<PathBuf> = None;
            let mut applicable_profiles = Vec::new();
            let mut conclusive = true;
            for render_profile in context.active_profiles() {
                let working_dir = project_root.join(&render_profile.working_directory);
                let Some(path) = assets::resolved_literal_path(
                    &working_dir,
                    &render_profile.assets_dir,
                    value,
                    assets::SVG_EXTENSIONS,
                )
                .and_then(|path| path.canonicalize().ok()) else {
                    conclusive = false;
                    break;
                };
                if !path.starts_with(&canonical_root) {
                    // MLR118 is intentionally project-local. A symlink that
                    // leaves the project is external, even if the lexical
                    // path starts below the root.
                    conclusive = false;
                    break;
                }
                if resolved.as_ref().is_some_and(|known| known != &path) {
                    // Different profiles open different files: one call no
                    // longer has a single asset-content fact.
                    conclusive = false;
                    break;
                }
                resolved = Some(path);
                applicable_profiles.push(render_profile.name.clone());
            }
            if !conclusive || applicable_profiles.is_empty() {
                continue;
            }
            let Some(path) = resolved else {
                continue;
            };
            let issues = cache
                .entry(path.clone())
                .or_insert_with(|| scan_svg(&path).ok())
                .clone();
            let Some(issues) = issues.filter(|issues| !issues.is_empty()) else {
                continue;
            };
            let asset = path
                .strip_prefix(&canonical_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("asset".to_owned(), json!(asset));
            evidence.insert("issues".to_owned(), json!(issues));
            diagnostics.push(build_diagnostic(
                &MLR118,
                file,
                *range,
                Confidence::High,
                format!(
                    "`{asset}` contains SVG content that SVGMobject cannot faithfully convert: {issues}",
                    issues = issues.join(", "),
                ),
                "SVGMobject converts supported vector shape elements into VMobjects; embedded text/images and filter, mask, or clipPath definitions are not reproduced faithfully. A <use> whose local href has no matching id also loses its referenced geometry. Convert these features to ordinary paths (for example with the source graphics editor) before rendering.",
                evidence,
                applicable_profiles,
                None,
            ));
        }
        diagnostics
    }
}

/// A deliberately small, fail-closed XML scanner. It validates nesting,
/// quoting, comments, CDATA, and processing instructions before returning
/// any issue. Unsupported declarations, malformed UTF-8/XML, or attribute
/// entity tricks are `Unknown` and silence the rule rather than guessing.
fn scan_svg(path: &Path) -> Result<Vec<String>, ()> {
    let source = std::fs::read_to_string(path).map_err(|_| ())?;
    let mut parser = XmlScanner::new(&source);
    parser.scan()?;
    let mut issues = parser.issues;
    for reference in parser.hrefs {
        let Some(fragment) = reference.strip_prefix('#') else {
            continue;
        };
        if !fragment.is_empty() && !parser.ids.contains(fragment) {
            issues.insert(format!("unresolved href `{reference}`"));
        }
    }
    Ok(issues.into_iter().collect())
}

struct XmlScanner<'a> {
    source: &'a str,
    cursor: usize,
    stack: Vec<String>,
    saw_root: bool,
    ids: BTreeSet<String>,
    hrefs: Vec<String>,
    issues: BTreeSet<String>,
}

impl<'a> XmlScanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor: 0,
            stack: Vec::new(),
            saw_root: false,
            ids: BTreeSet::new(),
            hrefs: Vec::new(),
            issues: BTreeSet::new(),
        }
    }

    fn scan(&mut self) -> Result<(), ()> {
        while let Some(relative) = self.source[self.cursor..].find('<') {
            let text = &self.source[self.cursor..self.cursor + relative];
            if text.contains('&') || (self.stack.is_empty() && !text.trim().is_empty()) {
                return Err(());
            }
            self.cursor += relative;
            if self.rest().starts_with("<!--") {
                self.skip_comment()?;
            } else if self.rest().starts_with("<![CDATA[") {
                if self.stack.is_empty() {
                    return Err(());
                }
                self.skip_through("]]>")?;
            } else if self.rest().starts_with("<?") {
                self.skip_through("?>")?;
            } else if self.rest().starts_with("</") {
                self.parse_end_tag()?;
            } else if self.rest().starts_with("<!") {
                // DOCTYPE/internal subsets need a full XML parser. Staying
                // silent is safer than accepting only part of that grammar.
                return Err(());
            } else {
                self.parse_start_tag()?;
            }
        }
        let trailing = &self.source[self.cursor..];
        if trailing.contains('&')
            || !trailing.trim().is_empty()
            || !self.stack.is_empty()
            || !self.saw_root
        {
            return Err(());
        }
        Ok(())
    }

    fn rest(&self) -> &'a str {
        &self.source[self.cursor..]
    }

    fn skip_through(&mut self, terminator: &str) -> Result<(), ()> {
        let end = self.rest().find(terminator).ok_or(())?;
        self.cursor += end + terminator.len();
        Ok(())
    }

    fn skip_comment(&mut self) -> Result<(), ()> {
        let rest = self.rest();
        let end = rest.find("-->").ok_or(())?;
        if rest[4..end].contains("--") {
            return Err(());
        }
        self.cursor += end + 3;
        Ok(())
    }

    fn parse_start_tag(&mut self) -> Result<(), ()> {
        self.cursor += 1;
        let name = self.parse_name()?;
        if self.stack.is_empty() {
            if self.saw_root {
                return Err(());
            }
            self.saw_root = true;
            if local_name(&name) != "svg" {
                return Err(());
            }
        }
        let local = local_name(&name);
        if matches!(local, "text" | "image" | "filter" | "mask" | "clipPath") {
            self.issues.insert(format!("unsupported <{local}>"));
        }
        let mut self_closing = false;
        let mut attributes = BTreeSet::new();
        loop {
            self.skip_space();
            if self.rest().starts_with("/>") {
                self.cursor += 2;
                self_closing = true;
                break;
            }
            if self.rest().starts_with('>') {
                self.cursor += 1;
                break;
            }
            let attribute = self.parse_name()?;
            if !attributes.insert(attribute.clone()) {
                return Err(());
            }
            self.skip_space();
            if !self.rest().starts_with('=') {
                return Err(());
            }
            self.cursor += 1;
            self.skip_space();
            let value = self.parse_quoted()?;
            if value.contains(['&', '<']) {
                return Err(());
            }
            match local_name(&attribute) {
                "id" if !value.is_empty() => {
                    self.ids.insert(value.to_owned());
                }
                "href" => self.hrefs.push(value.to_owned()),
                _ => {}
            }
        }
        if !self_closing {
            self.stack.push(name);
        }
        Ok(())
    }

    fn parse_end_tag(&mut self) -> Result<(), ()> {
        self.cursor += 2;
        let name = self.parse_name()?;
        self.skip_space();
        if !self.rest().starts_with('>') {
            return Err(());
        }
        self.cursor += 1;
        if self.stack.pop().as_deref() != Some(name.as_str()) {
            return Err(());
        }
        Ok(())
    }

    fn parse_name(&mut self) -> Result<String, ()> {
        let start = self.cursor;
        let first = self.source.as_bytes().get(self.cursor).copied().ok_or(())?;
        if !(first.is_ascii_alphabetic() || matches!(first, b'_' | b':')) {
            return Err(());
        }
        self.cursor += 1;
        while let Some(byte) = self.source.as_bytes().get(self.cursor).copied() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-' | b'.') {
                self.cursor += 1;
            } else {
                break;
            }
        }
        Ok(self.source[start..self.cursor].to_owned())
    }

    fn parse_quoted(&mut self) -> Result<&'a str, ()> {
        let quote = *self.source.as_bytes().get(self.cursor).ok_or(())?;
        if !matches!(quote, b'\'' | b'"') {
            return Err(());
        }
        self.cursor += 1;
        let start = self.cursor;
        while self.source.as_bytes().get(self.cursor).copied() != Some(quote) {
            if self.source.as_bytes().get(self.cursor).is_none() {
                return Err(());
            }
            self.cursor += 1;
        }
        let value = &self.source[start..self.cursor];
        self.cursor += 1;
        Ok(value)
    }

    fn skip_space(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.cursor += 1;
        }
    }
}

fn local_name(qualified: &str) -> &str {
    qualified.rsplit(':').next().unwrap_or(qualified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(source: &str) -> Result<Vec<String>, ()> {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("asset.svg");
        std::fs::write(&path, source).unwrap();
        scan_svg(&path)
    }

    #[test]
    fn finds_unsupported_elements_and_missing_local_references() {
        let issues = scan(
            r##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><defs><clipPath id="crop"/></defs><text>Hello</text><use href="#missing"/></svg>"##,
        )
        .unwrap();
        assert_eq!(
            issues,
            [
                "unresolved href `#missing`".to_owned(),
                "unsupported <clipPath>".to_owned(),
                "unsupported <text>".to_owned(),
            ]
        );
    }

    #[test]
    fn valid_paths_and_resolved_uses_are_silent() {
        assert_eq!(
            scan(r##"<svg><path id="shape" d="M0 0L1 1"/><use xlink:href="#shape"/></svg>"##),
            Ok(Vec::new())
        );
    }

    #[test]
    fn malformed_or_doctype_xml_is_unknown() {
        assert!(scan("<svg><path></svg>").is_err());
        assert!(scan("<!DOCTYPE svg><svg/>").is_err());
    }
}
