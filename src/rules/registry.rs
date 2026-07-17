use crate::diagnostic::{Confidence, RuleMetadata, Severity};

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

pub const RULE_PREFIXES: [&str; 4] = ["MLC", "MLR", "MLP", "MLD"];

#[must_use]
pub fn is_reserved_rule_id(rule_id: &str) -> bool {
    if rule_id == "MLC000" {
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
    if rule_id == "MLC000" {
        return Some(0);
    }
    let (prefix, number) = split_rule_id(rule_id)?;
    match prefix {
        "MLP" if (201..=227).contains(&number) => Some(3),
        "MLD" if (301..=307).contains(&number) => Some(4),
        "MLR" if (101..=127).contains(&number) => {
            let renderer_phase = [107, 108, 109, 111, 112, 118, 119, 120, 121, 122, 123];
            Some(if renderer_phase.contains(&number) { 4 } else { 1 })
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
    let mut result = vec!["MLC000".to_owned()];
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
