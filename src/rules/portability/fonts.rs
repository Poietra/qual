//! `MLD306`: literal `font=` outside the profile's allowed-fonts list
//! (DESIGN §7.4).
//!
//! A font that is not installed on the render platform makes Pango fall
//! back to a different family, silently changing typography between
//! machines. The rule fires only when a profile actually configures a
//! non-empty `allowed-fonts` list — an empty list means "no allowlist
//! configured", never "nothing is allowed" (silence). Comparison is
//! case-insensitive, matching fontconfig's tolerant family matching.

use std::collections::BTreeMap;

use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::rules::base::{Rule, RuleContext};

use super::{build_diagnostic, short_name, single_knowledge_symbol};

const TEXT: &str = "manim.mobject.text.text_mobject.Text";
const MARKUP_TEXT: &str = "manim.mobject.text.text_mobject.MarkupText";

pub(super) const MLD306: RuleMetadata = RuleMetadata {
    id: "MLD306",
    summary: "Literal font is not in the profile's allowed-fonts list",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct FontOutsideAllowlist;

impl Rule for FontOutsideAllowlist {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD306
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(knowledge, &call.candidates)
            else {
                continue;
            };
            if constructor_id != TEXT && constructor_id != MARKUP_TEXT {
                continue;
            }
            let Some(argument) = call.keyword("font") else {
                continue;
            };
            let Some(LiteralFact::Str { value, range, .. }) = &argument.literal else {
                continue;
            };
            let font = value.trim();
            if font.is_empty() {
                continue;
            }
            let mut mismatching: Vec<String> = Vec::new();
            let mut allowed_by_profile: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for profile in context.active_profiles() {
                // Empty list = no allowlist configured = silence.
                if profile.allowed_fonts.is_empty() {
                    continue;
                }
                let allowed = profile
                    .allowed_fonts
                    .iter()
                    .any(|candidate| candidate.trim().to_lowercase() == font.to_lowercase());
                if !allowed {
                    mismatching.push(profile.name.clone());
                    allowed_by_profile.insert(profile.name.clone(), profile.allowed_fonts.clone());
                }
            }
            if mismatching.is_empty() {
                continue;
            }
            let file = context.sources().file(call.file);
            let constructor = short_name(constructor_id);
            let mut evidence = BTreeMap::new();
            evidence.insert("font".to_owned(), json!(font));
            evidence.insert("allowed_fonts".to_owned(), json!(allowed_by_profile));
            diagnostics.push(build_diagnostic(
                &MLD306,
                file,
                *range,
                Confidence::High,
                format!(
                    "Font \"{font}\" passed to `{constructor}()` is not in the \
                     allowed-fonts list of the targeted profile(s)"
                ),
                "The profile declares the fonts assumed installed on the render \
                 platform. A family outside that list makes Pango fall back to a \
                 different font there, so the rendered typography differs between \
                 machines. Use an allowed family or add this one to allowed-fonts.",
                evidence,
                mismatching,
                None,
            ));
        }
        diagnostics
    }
}
