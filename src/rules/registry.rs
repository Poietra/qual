use crate::diagnostic::{Confidence, RuleMetadata, Severity};
use crate::rules::base::Rule;

pub const SYNTAX_ERROR: RuleMetadata = RuleMetadata {
    id: "MLC000",
    summary: "Python source cannot be parsed",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 0,
    required_profiles: &[],
    required_capabilities: &["source"],
    supersedes: &[],
};

/// Metadata for the dedicated warning about an invalid or unknown inline
/// suppression comment (DESIGN §8.3: an unknown rule ID inside an inline
/// suppression warns and does not suppress).
pub const INVALID_SUPPRESSION: RuleMetadata = RuleMetadata {
    id: "MLC001",
    summary: "Invalid or unknown inline suppression comment",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 0,
    required_profiles: &[],
    required_capabilities: &["source"],
    supersedes: &[],
};

pub const RULE_PREFIXES: [&str; 4] = ["MLC", "MLR", "MLP", "MLD"];

/// Rule IDs emitted by the analysis pipeline itself (parse and suppression
/// handling) rather than by a registered [`Rule`].
pub const PIPELINE_RULE_IDS: [&str; 2] = ["MLC000", "MLC001"];

/// Every registered [`Rule`] instance, composed from the rule-group modules.
///
/// Each group module (`lifecycle`, `rendering`, `performance`,
/// `portability`) owns its own registration list; nothing else needs to
/// change when a rule is added there.
#[must_use]
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    let mut rules = crate::rules::lifecycle::rules();
    rules.extend(crate::rules::rendering::rules());
    rules.extend(crate::rules::performance::rules());
    rules.extend(crate::rules::portability::rules());
    rules
}

/// Whether the selector configuration enables `rule_id`: some `select`
/// selector matches it and no `ignore` selector does. This is the same
/// predicate the post-run diagnostic filter applies, evaluated up front.
fn selector_enabled(rule_id: &str, select: &[String], ignore: &[String]) -> bool {
    let matches = |selectors: &[String]| {
        selectors
            .iter()
            .any(|selector| crate::reporting::suppressions::selector_matches(selector, rule_id))
    };
    matches(select) && !matches(ignore)
}

/// The registered rules a `check` run must execute for the resolved
/// `select` / `ignore` configuration: every selected-and-not-ignored
/// rule, plus — transitively — every rule that `supersedes` one of them.
///
/// Opt-in rules (`default_enabled: false`, currently `MLP225`) join the
/// set only when a selector names their exact rule ID: a family prefix
/// like `MLP` never enables them, so a normal `check` run never fires
/// them (DESIGN §7.3 — their home is the `cost` command; `--select
/// MLP225` is the explicit opt-in).
///
/// Supersession is part of diagnostic *production* (DESIGN §7.3: the
/// specific diagnostic permanently replaces the generic one), so a
/// superseding rule must still run when only the generic rule is
/// selected; otherwise the generic diagnostic would resurrect at spans
/// the specific rule owns. Per-file ignores never disable a rule
/// globally — they only filter matching files — so they cannot shrink
/// this set.
#[must_use]
pub fn enabled_rules(select: &[String], ignore: &[String]) -> Vec<Box<dyn Rule>> {
    let all = all_rules();
    let mut wanted: std::collections::BTreeSet<&'static str> = all
        .iter()
        .map(|rule| rule.metadata())
        .filter(|metadata| {
            selector_enabled(metadata.id, select, ignore)
                && (metadata.default_enabled
                    || select.iter().any(|selector| selector == metadata.id))
        })
        .map(|metadata| metadata.id)
        .collect();
    // Fixpoint: pull in supersessors of already-wanted rules (supersedes
    // chains are short, but the closure keeps the guarantee uniform).
    loop {
        let mut grew = false;
        for rule in &all {
            let metadata = rule.metadata();
            if !wanted.contains(metadata.id)
                && metadata.supersedes.iter().any(|id| wanted.contains(id))
            {
                wanted.insert(metadata.id);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    all.into_iter()
        .filter(|rule| wanted.contains(rule.metadata().id))
        .collect()
}

/// The union of `required_capabilities` over a rule set: the fact layers
/// a run executing exactly these rules can ever consume.
#[must_use]
pub fn capability_union(rules: &[Box<dyn Rule>]) -> std::collections::BTreeSet<&'static str> {
    rules
        .iter()
        .flat_map(|rule| rule.metadata().required_capabilities.iter().copied())
        .collect()
}

fn metadata_index() -> &'static std::collections::BTreeMap<&'static str, &'static RuleMetadata> {
    static INDEX: std::sync::OnceLock<
        std::collections::BTreeMap<&'static str, &'static RuleMetadata>,
    > = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = std::collections::BTreeMap::new();
        index.insert(SYNTAX_ERROR.id, &SYNTAX_ERROR);
        index.insert(INVALID_SUPPRESSION.id, &INVALID_SUPPRESSION);
        for rule in all_rules() {
            index.insert(rule.metadata().id, rule.metadata());
        }
        index
    })
}

/// Static metadata for an implemented rule ID.
#[must_use]
pub fn metadata_for(rule_id: &str) -> Option<&'static RuleMetadata> {
    metadata_index().get(rule_id).copied()
}

/// Whether the current build can emit this rule ID.
#[must_use]
pub fn is_implemented(rule_id: &str) -> bool {
    metadata_index().contains_key(rule_id)
}

#[must_use]
pub fn is_reserved_rule_id(rule_id: &str) -> bool {
    if rule_id == "MLC000" || rule_id == "MLC001" {
        return true;
    }
    let Some((prefix, number)) = split_rule_id(rule_id) else {
        return false;
    };
    match prefix {
        "MLC" => (101..=129).contains(&number),
        "MLR" => (101..=127).contains(&number),
        "MLP" => (201..=227).contains(&number),
        "MLD" => (301..=307).contains(&number),
        _ => false,
    }
}

pub fn validate_selectors(selectors: &[String], source: &str) -> Result<(), String> {
    for selector in selectors {
        if RULE_PREFIXES.contains(&selector.as_str()) || is_reserved_rule_id(selector) {
            continue;
        }
        return Err(format!("unknown rule selector in {source}: {selector}"));
    }
    Ok(())
}

#[must_use]
pub fn implementation_phase(rule_id: &str) -> Option<u8> {
    if rule_id == "MLC000" || rule_id == "MLC001" {
        return Some(0);
    }
    let (prefix, number) = split_rule_id(rule_id)?;
    match prefix {
        "MLP" if (201..=227).contains(&number) => Some(3),
        "MLD" if (301..=307).contains(&number) => Some(4),
        "MLR" if (101..=127).contains(&number) => {
            let renderer_phase = [107, 108, 109, 111, 112, 118, 119, 120, 121, 122, 123];
            Some(if renderer_phase.contains(&number) {
                4
            } else {
                1
            })
        }
        "MLC" if (101..=129).contains(&number) => {
            let direct_phase = [101, 102, 103, 104, 105, 106, 109, 122, 126, 127];
            Some(if direct_phase.contains(&number) { 1 } else { 2 })
        }
        _ => None,
    }
}

#[must_use]
pub fn all_reserved_rule_ids() -> Vec<String> {
    let mut result = vec!["MLC000".to_owned(), "MLC001".to_owned()];
    result.extend((101..=129).map(|number| format!("MLC{number}")));
    result.extend((101..=127).map(|number| format!("MLR{number}")));
    result.extend((201..=227).map(|number| format!("MLP{number}")));
    result.extend((301..=307).map(|number| format!("MLD{number}")));
    result.sort();
    result
}

fn split_rule_id(rule_id: &str) -> Option<(&str, u16)> {
    if rule_id.len() != 6 || !rule_id.is_ascii() {
        return None;
    }
    let (prefix, suffix) = rule_id.split_at(3);
    Some((prefix, suffix.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(selectors: &[&str]) -> Vec<String> {
        selectors.iter().map(|s| (*s).to_owned()).collect()
    }

    fn ids(rules: &[Box<dyn Rule>]) -> Vec<&'static str> {
        rules.iter().map(|rule| rule.metadata().id).collect()
    }

    #[test]
    fn selector_validation_rejects_unknown_ids() {
        assert!(validate_selectors(&["MLC".to_owned()], "select").is_ok());
        assert!(validate_selectors(&["MLC101".to_owned()], "select").is_ok());
        assert!(validate_selectors(&["MLC999".to_owned()], "select").is_err());
    }

    /// The builtin default select (all four prefixes) enables every
    /// default-enabled registered rule, so a default run computes every
    /// fact layer. Opt-in rules (`default_enabled: false`) stay out.
    #[test]
    fn default_prefixes_enable_every_rule_and_every_capability() {
        let select = owned(&RULE_PREFIXES);
        let rules = enabled_rules(&select, &[]);
        let default_enabled = all_rules()
            .iter()
            .filter(|rule| rule.metadata().default_enabled)
            .count();
        assert_eq!(rules.len(), default_enabled);
        assert!(
            !ids(&rules).contains(&"MLP225"),
            "the opt-in MLP225 must not run under a prefix select"
        );
        let capabilities = capability_union(&rules);
        for capability in ["qualified-calls", "lifecycle", "cost-facts"] {
            assert!(capabilities.contains(capability), "missing {capability}");
        }
    }

    /// An opt-in rule needs its exact ID in `select` (DESIGN §7.3:
    /// `MLP225` never fires in a normal check; `--select MLP225` is the
    /// explicit opt-in and pulls in the full cost fact stack via its
    /// `cost-report` capability).
    #[test]
    fn opt_in_rules_need_their_exact_id_in_select() {
        assert!(!ids(&enabled_rules(&owned(&["MLP"]), &[])).contains(&"MLP225"));
        let rules = enabled_rules(&owned(&["MLP225"]), &[]);
        assert_eq!(ids(&rules), ["MLP225"]);
        assert!(capability_union(&rules).contains("cost-report"));
        // An ignore still wins over the explicit opt-in.
        assert!(enabled_rules(&owned(&["MLP225"]), &owned(&["MLP225"])).is_empty());
    }

    /// A select matching no registered rule (`MLC000` is produced by the
    /// pipeline, not a registered rule) yields an empty capability union:
    /// the run skips the lifecycle interpreter and the cost model.
    #[test]
    fn pipeline_only_select_needs_no_fact_capability() {
        let rules = enabled_rules(&owned(&["MLC000"]), &[]);
        assert!(rules.is_empty());
        assert!(capability_union(&rules).is_empty());
    }

    /// A narrow deep-capability select keeps its own requirements: MLP226
    /// declares qualified-calls + cost-facts, so the union demands them.
    #[test]
    fn narrow_select_keeps_the_selected_rules_capabilities() {
        let rules = enabled_rules(&owned(&["MLP226"]), &[]);
        assert_eq!(ids(&rules), ["MLP226"]);
        let capabilities = capability_union(&rules);
        assert!(capabilities.contains("qualified-calls"));
        assert!(capabilities.contains("cost-facts"));
    }

    /// Selecting only a superseded generic rule still runs its superseding
    /// specific rule (supersession is diagnostic production, DESIGN §7.3):
    /// `--select MLP201` must not resurrect MLP201 diagnostics at spans
    /// MLP226 owns, so MLP226 joins the enabled set — and its capability
    /// requirements join the union.
    #[test]
    fn superseding_rules_join_the_enabled_set_of_their_generic_rule() {
        let rules = enabled_rules(&owned(&["MLP201"]), &[]);
        let ids = ids(&rules);
        assert!(ids.contains(&"MLP201"));
        assert!(ids.contains(&"MLP226"), "supersessor must run: {ids:?}");
        assert!(capability_union(&rules).contains("cost-facts"));
    }

    /// An `ignore`d rule stays out of the directly-selected set, but a
    /// supersessor pulled back in via the closure still runs (its
    /// diagnostics are filtered afterwards; its supersession effect is
    /// not) — exactly the pre-gating pipeline behavior.
    #[test]
    fn ignored_supersessor_still_runs_for_its_superseded_rule() {
        let rules = enabled_rules(&owned(&["MLP"]), &owned(&["MLP226"]));
        assert!(ids(&rules).contains(&"MLP226"));
        // Ignoring the generic rule as well removes both.
        let rules = enabled_rules(&owned(&["MLP"]), &owned(&["MLP226", "MLP201"]));
        let ids = ids(&rules);
        assert!(!ids.contains(&"MLP201"));
        assert!(!ids.contains(&"MLP226"));
    }
}
