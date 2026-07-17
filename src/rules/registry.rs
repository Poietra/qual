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

    #[test]
    fn selector_validation_rejects_unknown_ids() {
        assert!(validate_selectors(&["MLC".to_owned()], "select").is_ok());
        assert!(validate_selectors(&["MLC101".to_owned()], "select").is_ok());
        assert!(validate_selectors(&["MLC999".to_owned()], "select").is_err());
    }
}
