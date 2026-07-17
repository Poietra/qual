//! `MLD307`: wall-clock, filesystem, or network I/O inside a frame
//! callback (DESIGN §7.4).
//!
//! A frame callback runs on the render time grid; render *time* is not
//! wall time (a 60 FPS second of animation may take minutes to raster).
//! Reading the wall clock makes the geometry depend on the machine's
//! rendering speed, and filesystem / network calls both block the frame
//! loop and make output depend on external state.
//!
//! Gates: the call must be provably hot (cost facts), the callee must
//! resolve through the file's imports to a curated wall-clock /
//! filesystem / network function (a name rebound anywhere in the file is
//! never trusted — silence), and the call must not resolve to any Manim /
//! project symbol.

use std::collections::BTreeMap;

use rustpython_parser::ast;
use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::source::FileId;

use super::{ImportMap, build_diagnostic, call_nodes_by_range};

/// Canonical wall-clock reads.
const WALL_CLOCK: [&str; 12] = [
    "datetime.date.today",
    "datetime.datetime.now",
    "datetime.datetime.today",
    "datetime.datetime.utcnow",
    "time.monotonic",
    "time.monotonic_ns",
    "time.perf_counter",
    "time.perf_counter_ns",
    "time.process_time",
    "time.process_time_ns",
    "time.time",
    "time.time_ns",
];

/// `pathlib.Path` methods that touch the filesystem when chained directly
/// onto a `Path(...)` construction.
const PATH_IO_METHODS: [&str; 5] = [
    "open",
    "read_bytes",
    "read_text",
    "write_bytes",
    "write_text",
];

/// Canonical network entry points.
const NETWORK: [&str; 13] = [
    "http.client.HTTPConnection",
    "http.client.HTTPSConnection",
    "requests.delete",
    "requests.get",
    "requests.head",
    "requests.patch",
    "requests.post",
    "requests.put",
    "requests.request",
    "socket.create_connection",
    "socket.getaddrinfo",
    "socket.gethostbyname",
    "urllib.request.urlopen",
];

pub(super) const MLD307: RuleMetadata = RuleMetadata {
    id: "MLD307",
    summary: "Wall-clock, filesystem, or network call inside a frame callback",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

pub(super) struct FrameCallbackIo;

/// What kind of impure call was classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IoKind {
    WallClock,
    Filesystem,
    Network,
}

impl IoKind {
    const fn label(self) -> &'static str {
        match self {
            Self::WallClock => "wall-clock",
            Self::Filesystem => "filesystem",
            Self::Network => "network",
        }
    }

    const fn consequence(self) -> &'static str {
        match self {
            Self::WallClock => {
                "render time is not wall time, so the geometry depends on how fast \
                 this machine renders"
            }
            Self::Filesystem => {
                "the read/write repeats every frame and couples the output to \
                 external file state"
            }
            Self::Network => {
                "the request repeats every frame, blocks the frame loop, and couples \
                 the output to network state"
            }
        }
    }
}

/// Per-file classification state.
struct FileFacts<'a> {
    imports: ImportMap,
    calls: BTreeMap<(u32, u32), &'a ast::ExprCall>,
}

impl Rule for FrameCallbackIo {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD307
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cost = context.cost_facts();
        let profiles = context.config().active_profile_names();
        let mut per_file: BTreeMap<FileId, FileFacts<'_>> = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for (call_index, call) in context.qualified_calls().calls.iter().enumerate() {
            let Some(hot) = cost.is_call_in_hot_context(call_index) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let facts = per_file.entry(call.file).or_insert_with(|| FileFacts {
                imports: ImportMap::build(file),
                calls: call_nodes_by_range(file),
            });
            let key = (call.call_range.start().into(), call.call_range.end().into());
            let Some(node) = facts.calls.get(&key) else {
                continue;
            };
            let Some((kind, shown)) =
                classify_io(context.project_index(), call, node, &facts.imports)
            else {
                continue;
            };
            let mut evidence = BTreeMap::new();
            evidence.insert("category".to_owned(), json!(kind.label()));
            evidence.insert("function".to_owned(), json!(shown));
            evidence.insert("entry".to_owned(), json!(hot.entry.label()));
            evidence.insert("state_path".to_owned(), json!(hot.state_path()));
            diagnostics.push(build_diagnostic(
                &MLD307,
                file,
                call.call_range,
                Confidence::Medium,
                format!(
                    "`{shown}` performs {label} I/O inside a per-frame callback; {why}",
                    label = kind.label(),
                    why = kind.consequence(),
                ),
                "Frame callbacks run once per rendered frame on the animation time \
                 grid. Wall-clock reads make motion depend on rendering speed instead \
                 of scene time (use the dt parameter or a ValueTracker), and \
                 filesystem / network calls repeat per frame and tie the rendered \
                 output to external state — load data once in construct instead.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

/// Classifies the callee of one hot call as impure I/O. Returns the kind
/// and a canonical display name; `None` means silence.
fn classify_io(
    index: &crate::frontend::index::ProjectIndex,
    fact: &crate::frontend::index::QualifiedCall,
    call: &ast::ExprCall,
    imports: &ImportMap,
) -> Option<(IoKind, String)> {
    // `open(...)`: the builtin, only when nothing resolves or rebinds it.
    if let ast::Expr::Name(name) = call.func.as_ref() {
        if name.id.as_str() == "open"
            && fact.candidates.is_empty()
            && imports.is_probably_builtin("open")
        {
            return Some((IoKind::Filesystem, "open".to_owned()));
        }
    }
    // `Path("...").read_text()`-style direct chains: the receiver is a
    // call, so only the structural shape identifies the Path method.
    if let ast::Expr::Attribute(attribute) = call.func.as_ref() {
        if let ast::Expr::Call(inner) = attribute.value.as_ref() {
            if PATH_IO_METHODS.contains(&attribute.attr.as_str())
                && imports.resolve_expr(&inner.func).as_deref() == Some("pathlib.Path")
            {
                return Some((
                    IoKind::Filesystem,
                    format!("Path(...).{}", attribute.attr.as_str()),
                ));
            }
            return None;
        }
    }
    // Dotted / bare external functions: every resolution must classify to
    // the same kind, otherwise silence.
    let targets = super::external_call_targets(index, fact, imports, &call.func);
    let mut classified: Option<(IoKind, String)> = None;
    for target in &targets {
        let current = classify_target(target)?;
        match &classified {
            None => classified = Some(current),
            Some(existing) if *existing == current => {}
            Some(_) => return None,
        }
    }
    classified
}

fn classify_target(target: &str) -> Option<(IoKind, String)> {
    if WALL_CLOCK.contains(&target) {
        return Some((IoKind::WallClock, target.to_owned()));
    }
    if NETWORK.contains(&target) {
        return Some((IoKind::Network, target.to_owned()));
    }
    None
}
