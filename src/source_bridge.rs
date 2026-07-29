//! Hash-guarded, non-writing source patch candidates and semantic rematching.
//!
//! v0 deliberately supports only three local templates: replacing an
//! existing literal call argument, replacing the sole argument of an existing
//! `.shift(...)` on a uniquely named object, and inserting `.shift(...)`
//! directly after an object's allocation call. Candidates retain original
//! text for rollback and are never applied to disk by this module.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast;
use rustpython_parser::{Mode, parse};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::frontend::index::{CallArgument, QualifiedCall, QualifiedCallFacts};
use crate::semantic::interpreter::{LifecycleFacts, SceneLifecycle};
use crate::semantic::values::ObjectId;
use crate::source::{FileId, SourceManager};
use crate::static_facts::{StaticFactsOutput, project_source_anchor};

const DOMAIN: &str = "source-bridge-v0";

/// Versioned JSON request accepted by the source bridge.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRequest {
    /// Must be zero for this contract.
    pub schema_version: u8,
    /// Exact `StaticFacts` snapshot the request targets.
    pub snapshot_id: String,
    /// Snapshot-scoped Scene, play, or object ID to rematch after editing.
    pub target_id: String,
    /// Bounded local rewrite template.
    pub operation: PatchOperation,
}

impl PatchRequest {
    /// Checks the non-structural constraints published by the request schema.
    pub fn validate_contract(&self) -> Result<(), &'static str> {
        if self.schema_version != 0 {
            return Err("schema_version must be 0");
        }
        if !has_content_id(&self.snapshot_id, "snapshot:sf0:") {
            return Err("snapshot_id is not a StaticFacts v0 snapshot ID");
        }
        if !["scene:sf0:", "play:sf0:", "object:sf0:"]
            .iter()
            .any(|prefix| has_content_id(&self.target_id, prefix))
        {
            return Err("target_id is not a StaticFacts v0 entity ID");
        }
        match &self.operation {
            PatchOperation::ReplaceLiteralArgument {
                call,
                argument,
                replacement,
            } => {
                call.validate_contract()?;
                if replacement.is_empty() {
                    return Err("replacement must not be empty");
                }
                if let ArgumentSelector::Keyword { keyword } = argument {
                    if !is_identifier(keyword) {
                        return Err("keyword is not a Python identifier");
                    }
                }
            }
            PatchOperation::ModifyExistingShift { argument }
            | PatchOperation::InsertShiftChain { argument } => {
                if argument.is_empty() {
                    return Err("shift argument must not be empty");
                }
            }
        }
        Ok(())
    }
}

/// Supported v0 rewrite templates.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PatchOperation {
    /// Replace one statically literal positional or keyword call argument.
    ReplaceLiteralArgument {
        /// Source call whose argument is selected.
        call: RequestAnchor,
        /// Positional index or keyword name.
        argument: ArgumentSelector,
        /// New literal expression source.
        replacement: String,
    },
    /// Replace the sole positional argument of existing `.shift(...)` calls
    /// reached through the target object's unique binding.
    ModifyExistingShift {
        /// New syntactically valid shift vector expression.
        argument: String,
    },
    /// Insert `.shift(argument)` immediately after the target object's
    /// allocation call.
    InsertShiftChain {
        /// New syntactically valid shift vector expression.
        argument: String,
    },
}

/// Hash-guarded source location supplied in a request.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestAnchor {
    /// Project-relative POSIX path.
    pub path: String,
    /// Exact raw source SHA-256 (`sha256:...`).
    pub raw_content_hash: String,
    /// End-exclusive decoded UTF-8 byte range.
    pub utf8_byte_range: ByteRange,
}

impl RequestAnchor {
    fn validate_contract(&self) -> Result<(), &'static str> {
        if self.path.is_empty() {
            return Err("call path must not be empty");
        }
        if !is_sha256(&self.raw_content_hash) {
            return Err("call raw_content_hash is not a lowercase SHA-256");
        }
        if self.utf8_byte_range.start > self.utf8_byte_range.end {
            return Err("call byte range start exceeds end");
        }
        Ok(())
    }
}

/// End-exclusive decoded UTF-8 byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByteRange {
    /// First byte.
    pub start: usize,
    /// One past the final byte.
    pub end: usize,
}

/// Selects one call argument.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ArgumentSelector {
    /// Zero-based positional argument index.
    Position {
        /// Zero-based position.
        position: usize,
    },
    /// Explicit keyword argument.
    Keyword {
        /// Keyword name without `=`.
        keyword: String,
    },
}

/// Inputs needed to generate candidates without executing Python.
pub struct GenerationInput<'a> {
    /// Parsed immutable source snapshot.
    pub sources: &'a SourceManager,
    /// Raw bytes parallel to `sources.files()`.
    pub raw_sources: &'a [Vec<u8>],
    /// Qualified calls from the same snapshot.
    pub calls: &'a QualifiedCallFacts,
    /// Lifecycle state snapshots used to prove receiver identity at a call.
    pub lifecycle: &'a LifecycleFacts,
    /// Public `StaticFacts` document and ID index.
    pub static_facts: &'a StaticFactsOutput,
}

/// One local edit before virtual post-edit validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchEdit {
    /// Project-relative POSIX path.
    pub path: String,
    /// Exact raw hash precondition.
    pub raw_content_hash: String,
    /// Decoded UTF-8 byte range.
    pub range: ByteRange,
    /// Text currently occupying `range`; applying it restores/guards edits.
    pub original_text: String,
    /// Replacement source text.
    pub replacement: String,
}

/// One deterministic patch candidate.
#[derive(Debug, Clone)]
pub struct PatchCandidate {
    /// Content-derived candidate ID.
    pub id: String,
    /// `high` for unique structural matches, `medium` when multiple matches
    /// must be presented without choosing one.
    pub confidence: &'static str,
    /// v0 emits exactly one edit, represented as a vector for later templates.
    pub edits: Vec<PatchEdit>,
}

/// Candidate generation result before virtual validation.
#[derive(Debug)]
pub struct CandidateSet {
    /// Candidates in deterministic source order.
    pub candidates: Vec<PatchCandidate>,
    /// Structured reasons when generation could not safely produce a patch.
    pub unknowns: Vec<Value>,
}

/// Generates bounded local candidates. No file is written and no Python code
/// is imported or executed.
#[must_use]
pub fn generate(input: &GenerationInput<'_>, request: &PatchRequest) -> CandidateSet {
    if request.schema_version != 0 {
        return unavailable("unsupported-request-version", None);
    }
    if request.snapshot_id != input.static_facts.document["snapshot"]["id"] {
        return unavailable("snapshot-mismatch", None);
    }
    if find_entity(&input.static_facts.document, &request.target_id).is_none() {
        return unavailable("missing-target", None);
    }
    match &request.operation {
        PatchOperation::ReplaceLiteralArgument {
            call,
            argument,
            replacement,
        } => replace_literal_candidate(input, &request.target_id, call, argument, replacement),
        PatchOperation::ModifyExistingShift { argument } => {
            shift_candidates(input, &request.target_id, argument)
        }
        PatchOperation::InsertShiftChain { argument } => {
            insert_shift_candidate(input, &request.target_id, argument)
        }
    }
}

fn replace_literal_candidate(
    input: &GenerationInput<'_>,
    target_id: &str,
    anchor: &RequestAnchor,
    selector: &ArgumentSelector,
    replacement: &str,
) -> CandidateSet {
    if !is_literal_expression(replacement) {
        return unavailable(
            "replacement-not-literal",
            Some(request_anchor_value(anchor)),
        );
    }
    let Some((file, source)) = source_for_request(input, anchor) else {
        return unavailable(
            "source-precondition-failed",
            Some(request_anchor_value(anchor)),
        );
    };
    let Some((_, target)) = find_entity(&input.static_facts.document, target_id) else {
        return unavailable("missing-target", None);
    };
    let target_anchor = target
        .get("allocation_anchor")
        .or_else(|| target.get("call_anchor"));
    if !target_anchor
        .is_some_and(|target_anchor| request_matches_public_anchor(anchor, target_anchor))
    {
        return unavailable("call-not-target-anchor", Some(request_anchor_value(anchor)));
    }
    let matching_calls: Vec<&QualifiedCall> = input
        .calls
        .calls
        .iter()
        .filter(|call| {
            call.file == file
                && usize::from(call.call_range.start()) == anchor.utf8_byte_range.start
                && usize::from(call.call_range.end()) == anchor.utf8_byte_range.end
        })
        .collect();
    if matching_calls.len() != 1 {
        return unavailable("call-anchor-not-unique", Some(request_anchor_value(anchor)));
    }
    let Some(argument) = selected_argument(matching_calls[0], selector) else {
        return unavailable("argument-not-found", Some(request_anchor_value(anchor)));
    };
    if argument.literal.is_none() {
        return unavailable(
            "dynamic-existing-argument",
            Some(full_anchor(input, file, argument.range)),
        );
    }
    candidate_set(input, file, argument.range, replacement, source, true)
}

fn shift_candidates(input: &GenerationInput<'_>, target_id: &str, argument: &str) -> CandidateSet {
    if !is_expression(argument) {
        return unavailable("invalid-replacement-expression", None);
    }
    let Some(object) = find_record(&input.static_facts.document["objects"], target_id) else {
        return unavailable("target-not-object", None);
    };
    let Some(binding) = unique_binding(object) else {
        return unavailable(
            "binding-not-unique",
            object.get("allocation_anchor").cloned(),
        );
    };
    if !has_known_kind(object) {
        return unavailable(
            "object-kind-unknown",
            object.get("allocation_anchor").cloned(),
        );
    }
    let Some((file, source)) = source_for_public_anchor(input, &object["allocation_anchor"]) else {
        return unavailable(
            "source-precondition-failed",
            object.get("allocation_anchor").cloned(),
        );
    };
    let Some(allocation_range) = byte_range(&object["allocation_anchor"]) else {
        return unavailable(
            "invalid-source-anchor",
            object.get("allocation_anchor").cloned(),
        );
    };
    let allocation_calls: Vec<&QualifiedCall> = input
        .calls
        .calls
        .iter()
        .filter(|call| {
            call.file == file
                && usize::from(call.call_range.start()) == allocation_range.start
                && usize::from(call.call_range.end()) == allocation_range.end
        })
        .collect();
    if allocation_calls.len() != 1 {
        return unavailable(
            "allocation-call-not-unique",
            object.get("allocation_anchor").cloned(),
        );
    }
    let Some((scene_name, target_object)) = input.static_facts.index.object_location(target_id)
    else {
        return unavailable(
            "target-object-identity-unavailable",
            object.get("allocation_anchor").cloned(),
        );
    };
    let Some(scene) = input.lifecycle.scene(scene_name) else {
        return unavailable(
            "target-object-identity-unavailable",
            object.get("allocation_anchor").cloned(),
        );
    };
    let checked = receiver_checked_shift_arguments(
        input.calls,
        scene,
        target_object,
        file,
        allocation_range,
        allocation_calls[0],
        binding,
    );
    let matches = checked.arguments;
    if matches.is_empty() {
        return unavailable(
            if checked.receiver_reassigned {
                "binding-reassigned-before-shift"
            } else if checked.receiver_unknown {
                "shift-receiver-identity-unknown"
            } else {
                "existing-shift-not-found"
            },
            object.get("allocation_anchor").cloned(),
        );
    }
    let unique = matches.len() == 1;
    let candidates = matches
        .into_iter()
        .map(|matched| make_candidate(input, file, matched.range, argument, source, unique))
        .collect();
    CandidateSet {
        candidates,
        unknowns: Vec::new(),
    }
}

struct ReceiverCheckedShiftArguments<'a> {
    arguments: Vec<&'a CallArgument>,
    receiver_reassigned: bool,
    receiver_unknown: bool,
}

fn receiver_checked_shift_arguments<'a>(
    calls: &'a QualifiedCallFacts,
    scene: &SceneLifecycle,
    target_object: &ObjectId,
    file: FileId,
    allocation_range: ByteRange,
    allocation_call: &QualifiedCall,
    binding: &str,
) -> ReceiverCheckedShiftArguments<'a> {
    let mut checked = ReceiverCheckedShiftArguments {
        arguments: Vec::new(),
        receiver_reassigned: false,
        receiver_unknown: false,
    };
    for call in calls.calls.iter().filter(|call| {
        call.file == file
            && call.context == allocation_call.context
            && usize::from(call.call_range.start()) >= allocation_range.end
            && is_binding_shift(call, binding)
            && call.positional_count == 1
            && call.keyword_names.is_empty()
            && !call.has_star_args
            && !call.has_star_star_kwargs
    }) {
        let Some(argument) = call.positional(0) else {
            continue;
        };
        let Some(snapshot) = scene.state_at(call.file, call.call_range.start().into()) else {
            checked.receiver_unknown = true;
            continue;
        };
        let Some(receiver) = snapshot.object_bindings.get(binding) else {
            checked.receiver_unknown = true;
            continue;
        };
        if snapshot.heap.resolve(receiver) == snapshot.heap.resolve(target_object) {
            checked.arguments.push(argument);
        } else {
            checked.receiver_reassigned = true;
        }
    }
    checked
        .arguments
        .sort_by_key(|argument| argument.range.start());
    checked
}

fn insert_shift_candidate(
    input: &GenerationInput<'_>,
    target_id: &str,
    argument: &str,
) -> CandidateSet {
    if !is_expression(argument) {
        return unavailable("invalid-replacement-expression", None);
    }
    let Some(object) = find_record(&input.static_facts.document["objects"], target_id) else {
        return unavailable("target-not-object", None);
    };
    if unique_binding(object).is_none() {
        return unavailable(
            "binding-not-unique",
            object.get("allocation_anchor").cloned(),
        );
    }
    if !has_known_kind(object) {
        return unavailable(
            "object-kind-unknown",
            object.get("allocation_anchor").cloned(),
        );
    }
    let allocation = &object["allocation_anchor"];
    let Some((file, source)) = source_for_public_anchor(input, allocation) else {
        return unavailable("source-precondition-failed", Some(allocation.clone()));
    };
    let Some(range) = byte_range(allocation) else {
        return unavailable("invalid-source-anchor", Some(allocation.clone()));
    };
    let Some(slice) = source.text().get(range.start..range.end) else {
        return unavailable("invalid-source-anchor", Some(allocation.clone()));
    };
    if !is_call_expression(slice) {
        return unavailable("allocation-not-clear-call", Some(allocation.clone()));
    }
    let insertion = ByteRange {
        start: range.end,
        end: range.end,
    };
    let replacement = format!(".shift({argument})");
    candidate_set_range(input, file, insertion, &replacement, source, true)
}

fn candidate_set(
    input: &GenerationInput<'_>,
    file: FileId,
    range: rustpython_parser::text_size::TextRange,
    replacement: &str,
    source: &crate::source::SourceFile,
    unique: bool,
) -> CandidateSet {
    candidate_set_range(
        input,
        file,
        ByteRange {
            start: range.start().into(),
            end: range.end().into(),
        },
        replacement,
        source,
        unique,
    )
}

fn candidate_set_range(
    input: &GenerationInput<'_>,
    file: FileId,
    range: ByteRange,
    replacement: &str,
    source: &crate::source::SourceFile,
    unique: bool,
) -> CandidateSet {
    CandidateSet {
        candidates: vec![make_candidate_range(
            input,
            file,
            range,
            replacement,
            source,
            unique,
        )],
        unknowns: Vec::new(),
    }
}

fn make_candidate(
    input: &GenerationInput<'_>,
    file: FileId,
    range: rustpython_parser::text_size::TextRange,
    replacement: &str,
    source: &crate::source::SourceFile,
    unique: bool,
) -> PatchCandidate {
    make_candidate_range(
        input,
        file,
        ByteRange {
            start: range.start().into(),
            end: range.end().into(),
        },
        replacement,
        source,
        unique,
    )
}

fn make_candidate_range(
    input: &GenerationInput<'_>,
    file: FileId,
    range: ByteRange,
    replacement: &str,
    source: &crate::source::SourceFile,
    unique: bool,
) -> PatchCandidate {
    let original_text = source
        .text()
        .get(range.start..range.end)
        .unwrap_or_default()
        .to_owned();
    let edit = PatchEdit {
        path: source.relative_path().to_owned(),
        raw_content_hash: raw_hash(&input.raw_sources[file.index()]),
        range,
        original_text,
        replacement: replacement.to_owned(),
    };
    let id = patch_id(&edit);
    PatchCandidate {
        id,
        confidence: if unique { "high" } else { "medium" },
        edits: vec![edit],
    }
}

fn selected_argument<'a>(
    call: &'a QualifiedCall,
    selector: &ArgumentSelector,
) -> Option<&'a CallArgument> {
    match selector {
        ArgumentSelector::Position { position } => call.positional(*position),
        ArgumentSelector::Keyword { keyword } => call.keyword(keyword),
    }
}

fn is_binding_shift(call: &QualifiedCall, binding: &str) -> bool {
    call.callee_dotted
        .as_deref()
        .is_some_and(|dotted| dotted.len() == 2 && dotted[0] == binding && dotted[1] == "shift")
}

fn unique_binding(object: &Map<String, Value>) -> Option<&str> {
    let bindings = &object["binding_candidates"];
    if bindings["status"] != "known" {
        return None;
    }
    let values = bindings["values"].as_array()?;
    (values.len() == 1).then(|| values[0].as_str()).flatten()
}

fn request_matches_public_anchor(request: &RequestAnchor, public: &Value) -> bool {
    public["path"].as_str() == Some(request.path.as_str())
        && public["raw_content_hash"].as_str() == Some(request.raw_content_hash.as_str())
        && byte_range(public) == Some(request.utf8_byte_range)
}

fn has_known_kind(object: &Map<String, Value>) -> bool {
    object["kind_candidates"]["status"] == "known"
        && object["kind_candidates"]["values"]
            .as_array()
            .is_some_and(|values| !values.is_empty())
}

fn source_for_request<'a>(
    input: &'a GenerationInput<'_>,
    anchor: &RequestAnchor,
) -> Option<(FileId, &'a crate::source::SourceFile)> {
    let (file, source) = source_by_path(input.sources, &anchor.path)?;
    (raw_hash(&input.raw_sources[file.index()]) == anchor.raw_content_hash)
        .then_some((file, source))
}

fn source_for_public_anchor<'a>(
    input: &'a GenerationInput<'_>,
    anchor: &Value,
) -> Option<(FileId, &'a crate::source::SourceFile)> {
    let path = anchor["path"].as_str()?;
    let expected = anchor["raw_content_hash"].as_str()?;
    let (file, source) = source_by_path(input.sources, path)?;
    (raw_hash(&input.raw_sources[file.index()]) == expected).then_some((file, source))
}

fn source_by_path<'a>(
    sources: &'a SourceManager,
    path: &str,
) -> Option<(FileId, &'a crate::source::SourceFile)> {
    sources
        .files()
        .iter()
        .find(|source| source.relative_path() == path)
        .map(|source| (source.id(), source))
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn has_content_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn byte_range(anchor: &Value) -> Option<ByteRange> {
    Some(ByteRange {
        start: usize::try_from(anchor["utf8_byte_range"]["start"].as_u64()?).ok()?,
        end: usize::try_from(anchor["utf8_byte_range"]["end"].as_u64()?).ok()?,
    })
}

fn full_anchor(
    input: &GenerationInput<'_>,
    file: FileId,
    range: rustpython_parser::text_size::TextRange,
) -> Value {
    project_source_anchor(
        input.sources,
        input.raw_sources,
        crate::semantic::values::AllocationSite::new(file, range),
    )
}

fn request_anchor_value(anchor: &RequestAnchor) -> Value {
    json!({
        "path": anchor.path,
        "raw_content_hash": anchor.raw_content_hash,
        "utf8_byte_range": {
            "start": anchor.utf8_byte_range.start,
            "end": anchor.utf8_byte_range.end,
        },
    })
}

fn unavailable(kind: &str, anchor: Option<Value>) -> CandidateSet {
    let mut reason = Map::new();
    reason.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(anchor) = anchor {
        reason.insert("anchor".to_owned(), anchor);
    }
    CandidateSet {
        candidates: Vec::new(),
        unknowns: vec![json!({ "reasons": [Value::Object(reason)] })],
    }
}

fn find_entity<'a>(
    document: &'a Value,
    id: &str,
) -> Option<(&'static str, &'a Map<String, Value>)> {
    for (kind, field) in [
        ("scene", "scenes"),
        ("play", "plays"),
        ("object", "objects"),
    ] {
        if let Some(record) = find_record(&document[field], id) {
            return Some((kind, record));
        }
    }
    None
}

fn find_record<'a>(records: &'a Value, id: &str) -> Option<&'a Map<String, Value>> {
    records.as_array()?.iter().find_map(|record| {
        (record["id"].as_str() == Some(id))
            .then(|| record.as_object())
            .flatten()
    })
}

fn is_expression(source: &str) -> bool {
    parsed_assignment_value(source).is_some()
}

fn is_literal_expression(source: &str) -> bool {
    parsed_assignment_value(source)
        .as_deref()
        .is_some_and(is_literal)
}

fn is_call_expression(source: &str) -> bool {
    parsed_assignment_value(source)
        .as_deref()
        .is_some_and(|value| matches!(value, ast::Expr::Call(_)))
}

fn parsed_assignment_value(source: &str) -> Option<Box<ast::Expr>> {
    let wrapped = format!("__qual_bridge__ = ({source})\n");
    let Ok(ast::Mod::Module(module)) = parse(&wrapped, Mode::Module, "<source-bridge>") else {
        return None;
    };
    let [ast::Stmt::Assign(assign)] = module.body.as_slice() else {
        return None;
    };
    Some(assign.value.clone())
}

fn is_literal(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Constant(_) => true,
        ast::Expr::UnaryOp(unary)
            if matches!(unary.op, ast::UnaryOp::UAdd | ast::UnaryOp::USub) =>
        {
            matches!(unary.operand.as_ref(), ast::Expr::Constant(_))
        }
        ast::Expr::Tuple(tuple) => tuple.elts.iter().all(is_literal),
        ast::Expr::List(list) => list.elts.iter().all(is_literal),
        ast::Expr::Set(set) => set.elts.iter().all(is_literal),
        ast::Expr::Dict(dict) => {
            dict.keys
                .iter()
                .all(|key| key.as_ref().is_none_or(is_literal))
                && dict.values.iter().all(is_literal)
        }
        _ => false,
    }
}

fn patch_id(edit: &PatchEdit) -> String {
    let preimage = json!([
        DOMAIN,
        edit.path,
        edit.raw_content_hash,
        edit.range.start,
        edit.range.end,
        edit.original_text,
        edit.replacement,
    ]);
    format!(
        "patch:sb0:{:x}",
        Sha256::digest(serde_json::to_vec(&preimage).expect("patch preimage serializes"))
    )
}

fn raw_hash(raw: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(raw))
}

/// Rematches one original entity after a virtual candidate edit.
#[must_use]
pub fn rematch(before: &Value, after: &Value, target_id: &str, edit: &PatchEdit) -> Value {
    let Some((kind, original)) = find_entity(before, target_id) else {
        return rematch_value("missing", target_id, Vec::new());
    };
    let candidates = match kind {
        "scene" => rematch_scene(before, after, original),
        "play" => rematch_owned_entity(before, after, original, "plays", "call_anchor", edit),
        "object" => rematch_owned_entity(
            before,
            after,
            original,
            "objects",
            "allocation_anchor",
            edit,
        ),
        _ => Vec::new(),
    };
    let status = match candidates.len() {
        0 => "missing",
        1 => "match",
        _ => "ambiguous",
    };
    rematch_value(status, target_id, candidates)
}

fn rematch_scene(before: &Value, after: &Value, original: &Map<String, Value>) -> Vec<String> {
    let qualified = original["qualified_name"].as_str().unwrap_or_default();
    let _ = before;
    after["scenes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|scene| scene["qualified_name"] == qualified)
        .filter_map(|scene| scene["id"].as_str().map(str::to_owned))
        .collect()
}

fn rematch_owned_entity(
    before: &Value,
    after: &Value,
    original: &Map<String, Value>,
    field: &str,
    anchor_field: &str,
    edit: &PatchEdit,
) -> Vec<String> {
    let original_scene = scene_name(before, original["scene_id"].as_str().unwrap_or_default());
    let Some(original_range) = byte_range(&original[anchor_field]) else {
        return Vec::new();
    };
    let expected = transformed_range(original_range, edit);
    let original_path = original[anchor_field]["path"].as_str().unwrap_or_default();
    let mut candidates: Vec<String> = after[field]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| {
            scene_name(after, candidate["scene_id"].as_str().unwrap_or_default()) == original_scene
                && candidate[anchor_field]["path"] == original_path
                && byte_range(&candidate[anchor_field]).is_some_and(|range| range == expected)
        })
        .filter(|candidate| semantic_shape_matches(original, candidate, field, edit))
        .filter_map(|candidate| candidate["id"].as_str().map(str::to_owned))
        .collect();
    candidates.sort();
    candidates.dedup();
    candidates
}

fn semantic_shape_matches(
    original: &Map<String, Value>,
    candidate: &Value,
    field: &str,
    edit: &PatchEdit,
) -> bool {
    match field {
        "objects" => {
            original["cardinality"] == candidate["cardinality"]
                && original["kind_candidates"] == candidate["kind_candidates"]
                && original["binding_candidates"] == candidate["binding_candidates"]
                && call_path_signature(&original["call_context"], Some(edit))
                    == call_path_signature(&candidate["call_context"], None)
        }
        "plays" => {
            original["kind"] == candidate["kind"]
                && original["cardinality"] == candidate["cardinality"]
                && call_path_signature(&original["helper_call_path"], Some(edit))
                    == call_path_signature(&candidate["helper_call_path"], None)
        }
        _ => false,
    }
}

fn call_path_signature<'a>(
    value: &'a Value,
    edit: Option<&PatchEdit>,
) -> Vec<(&'a str, usize, usize)> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|anchor| {
            let path = anchor["path"].as_str()?;
            let range = byte_range(anchor)?;
            let range = edit
                .filter(|edit| edit.path == path)
                .map_or(range, |edit| transformed_range(range, edit));
            Some((path, range.start, range.end))
        })
        .collect()
}

fn transformed_range(range: ByteRange, edit: &PatchEdit) -> ByteRange {
    let replacement_len = edit.replacement.len();
    let original_len = edit.range.end.saturating_sub(edit.range.start);
    ByteRange {
        start: transform_offset(range.start, edit, replacement_len, original_len),
        end: transform_offset(range.end, edit, replacement_len, original_len),
    }
}

fn transform_offset(
    offset: usize,
    edit: &PatchEdit,
    replacement_len: usize,
    original_len: usize,
) -> usize {
    if offset <= edit.range.start {
        offset
    } else if offset >= edit.range.end {
        if replacement_len >= original_len {
            offset.saturating_add(replacement_len - original_len)
        } else {
            offset.saturating_sub(original_len - replacement_len)
        }
    } else {
        edit.range.start.saturating_add(replacement_len)
    }
}

fn scene_name<'a>(document: &'a Value, id: &str) -> &'a str {
    find_record(&document["scenes"], id)
        .and_then(|scene| scene["qualified_name"].as_str())
        .unwrap_or_default()
}

fn rematch_value(status: &str, original_id: &str, mut candidate_ids: Vec<String>) -> Value {
    candidate_ids.sort();
    json!({
        "status": status,
        "original_id": original_id,
        "candidate_ids": candidate_ids,
    })
}

/// Coverage regression relative to the pre-edit `StaticFacts` document.
#[must_use]
pub fn coverage_validation(before: &Value, after: &Value, allowed_new: &[(&str, usize)]) -> Value {
    let before_counts = frontier_counts(before);
    let after_counts = frontier_counts(after);
    let allowed: BTreeMap<&str, usize> = allowed_new.iter().copied().collect();
    let mut new_frontier_kinds = Vec::new();
    for (kind, after_count) in after_counts {
        let baseline = before_counts.get(&kind).copied().unwrap_or(0);
        let allowance = allowed.get(kind.as_str()).copied().unwrap_or(0);
        if after_count > baseline.saturating_add(allowance) {
            new_frontier_kinds.push(kind);
        }
    }
    json!({
        "status": if new_frontier_kinds.is_empty() { "preserved" } else { "decreased" },
        "new_frontier_kinds": new_frontier_kinds,
    })
}

/// Returns one only when the post-edit coverage frontier is the exact outer
/// `.shift(...)` call introduced by a generated insertion edit.
#[must_use]
pub fn inserted_shift_frontier_allowance(after: &Value, edit: &PatchEdit) -> usize {
    if edit.range.start != edit.range.end
        || !edit.replacement.starts_with(".shift(")
        || !edit.replacement.ends_with(')')
    {
        return 0;
    }
    let expected_end = edit.range.start.saturating_add(edit.replacement.len());
    usize::from(
        after["coverage"]["frontiers"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|frontier| {
                frontier["kind"] == "call-resolution"
                    && frontier["reasons"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|reason| reason["kind"] == "dynamic-call-target")
                    && frontier["anchor"]["path"].as_str() == Some(edit.path.as_str())
                    && byte_range(&frontier["anchor"]).is_some_and(|range| {
                        range.start < edit.range.start && range.end == expected_end
                    })
            }),
    )
}

fn frontier_counts(document: &Value) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for frontier in document["coverage"]["frontiers"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let kind = frontier["kind"].as_str().unwrap_or("unknown");
        let reasons: BTreeSet<&str> = frontier["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|reason| reason["kind"].as_str())
            .collect();
        if reasons.is_empty() {
            *counts.entry(kind.to_owned()).or_insert(0) += 1;
        } else {
            for reason in reasons {
                *counts.entry(format!("{kind}:{reason}")).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Renders one validated candidate into the public output record.
#[must_use]
pub fn candidate_value(
    candidate: &PatchCandidate,
    validation: &Value,
    sources: &SourceManager,
    raw_sources: &[Vec<u8>],
) -> Value {
    let edits: Vec<Value> = candidate
        .edits
        .iter()
        .filter_map(|edit| {
            let (file, _) = source_by_path(sources, &edit.path)?;
            let range = rustpython_parser::text_size::TextRange::new(
                u32::try_from(edit.range.start).ok()?.into(),
                u32::try_from(edit.range.end).ok()?.into(),
            );
            let anchor = project_source_anchor(
                sources,
                raw_sources,
                crate::semantic::values::AllocationSite::new(file, range),
            );
            Some(json!({
                "path": edit.path,
                "raw_content_hash": edit.raw_content_hash,
                "anchor": anchor,
                "original_text": edit.original_text,
                "replacement": edit.replacement,
            }))
        })
        .collect();
    json!({
        "id": candidate.id,
        "confidence": candidate.confidence,
        "preconditions": candidate.edits.iter().map(|edit| json!({
            "path": edit.path,
            "raw_content_hash": edit.raw_content_hash,
        })).collect::<Vec<_>>(),
        "edits": edits,
        "validation": validation,
    })
}

/// Stable operation label for public output.
#[must_use]
pub const fn operation_name(operation: &PatchOperation) -> &'static str {
    match operation {
        PatchOperation::ReplaceLiteralArgument { .. } => "replace-literal-argument",
        PatchOperation::ModifyExistingShift { .. } => "modify-existing-shift",
        PatchOperation::InsertShiftChain { .. } => "insert-shift-chain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rematch_documents(after_objects: &[Value]) -> (Value, Value, PatchEdit) {
        let scene = json!({
            "id": "scene:sf0:old",
            "qualified_name": "scene.Demo",
        });
        let object = json!({
            "id": "object:sf0:old",
            "scene_id": "scene:sf0:old",
            "allocation_anchor": {
                "path": "scene.py",
                "utf8_byte_range": { "start": 10, "end": 18 },
            },
            "call_context": [],
            "cardinality": "singleton",
            "kind_candidates": { "status": "known", "values": ["manim.Square"] },
            "binding_candidates": { "status": "known", "values": ["square"] },
        });
        let after = json!({
            "scenes": [{
                "id": "scene:sf0:new",
                "qualified_name": "scene.Demo",
            }],
            "objects": after_objects,
            "plays": [],
        });
        let before = json!({
            "scenes": [scene],
            "objects": [object],
            "plays": [],
        });
        let edit = PatchEdit {
            path: "scene.py".to_owned(),
            raw_content_hash: format!("sha256:{}", "0".repeat(64)),
            range: ByteRange { start: 30, end: 30 },
            original_text: String::new(),
            replacement: ".shift(RIGHT)".to_owned(),
        };
        (before, after, edit)
    }

    fn after_object(id: &str) -> Value {
        json!({
            "id": id,
            "scene_id": "scene:sf0:new",
            "allocation_anchor": {
                "path": "scene.py",
                "utf8_byte_range": { "start": 10, "end": 18 },
            },
            "call_context": [],
            "cardinality": "singleton",
            "kind_candidates": { "status": "known", "values": ["manim.Square"] },
            "binding_candidates": { "status": "known", "values": ["square"] },
        })
    }

    #[test]
    fn rematch_reports_all_three_contract_states() {
        let (before, missing_after, edit) = rematch_documents(&[]);
        assert_eq!(
            rematch(&before, &missing_after, "object:sf0:old", &edit)["status"],
            "missing"
        );

        let (_, matched_after, _) = rematch_documents(&[after_object("object:sf0:new-a")]);
        assert_eq!(
            rematch(&before, &matched_after, "object:sf0:old", &edit)["status"],
            "match"
        );

        let (_, ambiguous_after, _) = rematch_documents(&[
            after_object("object:sf0:new-a"),
            after_object("object:sf0:new-b"),
        ]);
        let result = rematch(&before, &ambiguous_after, "object:sf0:old", &edit);
        assert_eq!(result["status"], "ambiguous");
        assert_eq!(result["candidate_ids"].as_array().unwrap().len(), 2);
    }
}
