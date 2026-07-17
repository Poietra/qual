use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Info => 1,
            Self::Warning => 2,
            Self::Error => 3,
        }
    }

    #[must_use]
    pub const fn reaches(self, threshold: Self) -> bool {
        self.rank() >= threshold.rank()
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        })
    }
}

impl std::str::FromStr for Severity {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "warning" => Ok(Self::Warning),
            "info" => Ok(Self::Info),
            _ => Err(format!("unknown severity: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Certain,
    High,
    Medium,
    Low,
}

impl Confidence {
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Certain => 4,
        }
    }

    #[must_use]
    pub const fn reaches(self, threshold: Self) -> bool {
        self.rank() >= threshold.rank()
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Certain => "certain",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        })
    }
}

impl std::str::FromStr for Confidence {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "certain" => Ok(Self::Certain),
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            _ => Err(format!("unknown confidence: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourcePosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedLocation {
    pub path: String,
    pub span: SourceSpan,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub path: String,
    pub span: SourceSpan,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixApplicability {
    Safe,
    Unsafe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub applicability: FixApplicability,
    pub message: String,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub rule_id: String,
    pub severity: Severity,
    pub confidence: Confidence,
    pub path: String,
    #[serde(rename = "span")]
    pub primary_span: SourceSpan,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_locations: Vec<RelatedLocation>,
    #[serde(default)]
    pub evidence: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<BTreeMap<String, Value>>,
    pub applicable_profiles: Vec<String>,
    pub fix: Option<Fix>,
}

impl Diagnostic {
    #[must_use]
    pub fn compare_stable(&self, other: &Self) -> Ordering {
        (
            &self.path,
            self.primary_span.start.line,
            self.primary_span.start.column,
            &self.rule_id,
        )
            .cmp(&(
                &other.path,
                other.primary_span.start.line,
                other.primary_span.start.column,
                &other.rule_id,
            ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleMetadata {
    pub id: &'static str,
    pub summary: &'static str,
    pub default_enabled: bool,
    pub default_severity: Severity,
    pub minimum_confidence: Confidence,
    pub implementation_phase: u8,
    pub required_profiles: &'static [&'static str],
    pub required_capabilities: &'static [&'static str],
    pub supersedes: &'static [&'static str],
}
