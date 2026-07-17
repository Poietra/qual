//! Conservative Pango-markup subset validation for `MLR105` / `MLR124`
//! (DESIGN §7.2 prose).
//!
//! The validator never equates a general XML parser with Pango: only a
//! literal subset whose acceptance was verified against Manim/Pango is
//! judged. Any construct outside that subset makes the whole literal
//! `Unknown`, which must silence the rule (DESIGN §15 invariant 2).

/// Pango convenience tags validated by the subset parser. `span` is the
/// only tag on which attributes are understood.
pub const SUBSET_TAGS: &[&str] = &[
    "b", "big", "i", "s", "small", "span", "sub", "sup", "tt", "u",
];

/// Entity names Pango (glib XML) accepts without a definition.
const KNOWN_ENTITIES: &[&str] = &["amp", "apos", "gt", "lt", "quot"];

/// One provable markup error inside the validated subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkupError {
    /// `</close>` closes a different tag than the innermost open `<open>`.
    Mismatched {
        /// The innermost open tag.
        open: String,
        /// The closing tag as written.
        close: String,
    },
    /// `</close>` appears with no matching open tag.
    Stray {
        /// The closing tag as written.
        close: String,
    },
    /// `<open>` is never closed.
    Unclosed {
        /// The tag left open at the end of the literal.
        open: String,
    },
    /// `&name;` is not an entity Pango accepts.
    UnknownEntity {
        /// The entity body as written (without `&`/`;`).
        name: String,
    },
}

impl MarkupError {
    /// Human-readable description of the error.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Mismatched { open, close } => {
                format!("closing tag </{close}> does not match open tag <{open}>")
            }
            Self::Stray { close } => {
                format!("closing tag </{close}> has no matching open tag")
            }
            Self::Unclosed { open } => format!("tag <{open}> is never closed"),
            Self::UnknownEntity { name } => format!(
                "unknown entity &{name}; (Pango only accepts \
                 &amp; &lt; &gt; &quot; &apos; and numeric &#...;)"
            ),
        }
    }
}

/// Verdict of the subset validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkupVerdict {
    /// Every construct is inside the subset and consistent.
    Valid,
    /// A provable error inside the validated subset.
    Invalid(MarkupError),
    /// A construct outside the validated subset was seen first; nothing
    /// may be claimed about the literal.
    Unknown,
}

/// Validates the decoded literal text against the Pango subset.
///
/// Errors that can fire: mismatched or unclosed subset tags and unknown
/// entities (`&xyz;` outside `amp/lt/gt/quot/apos/&#...;`). The scan stops
/// at the first event; an out-of-subset construct seen before any error
/// yields [`MarkupVerdict::Unknown`].
#[must_use]
pub fn validate(text: &str) -> MarkupVerdict {
    let chars: Vec<char> = text.chars().collect();
    let mut stack: Vec<String> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '<' => match parse_tag(&chars, index) {
                TagParse::Open { name, next } => {
                    stack.push(name);
                    index = next;
                }
                TagParse::SelfClosing { next } => index = next,
                TagParse::Close { name, next } => {
                    match stack.pop() {
                        Some(open) if open == name => {}
                        Some(open) => {
                            return MarkupVerdict::Invalid(MarkupError::Mismatched {
                                open,
                                close: name,
                            });
                        }
                        None => {
                            return MarkupVerdict::Invalid(MarkupError::Stray { close: name });
                        }
                    }
                    index = next;
                }
                TagParse::Unknown => return MarkupVerdict::Unknown,
            },
            '&' => match parse_entity(&chars, index) {
                EntityParse::Valid { next } => index = next,
                EntityParse::UnknownEntity(name) => {
                    return MarkupVerdict::Invalid(MarkupError::UnknownEntity { name });
                }
                EntityParse::Unparseable => return MarkupVerdict::Unknown,
            },
            _ => index += 1,
        }
    }
    if let Some(open) = stack.last() {
        return MarkupVerdict::Invalid(MarkupError::Unclosed { open: open.clone() });
    }
    MarkupVerdict::Valid
}

/// Whether the text contains a matching `<tag>...</tag>` pair of a subset
/// tag (used by `MLR124` on plain `Text` literals). Everything outside the
/// subset is ignored, so `Text("a < b")` or `<html>` never match.
#[must_use]
pub fn matched_subset_pair(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut open_names: Vec<String> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '<' {
            match parse_tag(&chars, index) {
                TagParse::Open { name, next } => {
                    open_names.push(name);
                    index = next;
                    continue;
                }
                TagParse::Close { name, next } => {
                    if open_names.contains(&name) {
                        return Some(name);
                    }
                    index = next;
                    continue;
                }
                TagParse::SelfClosing { next } => {
                    index = next;
                    continue;
                }
                TagParse::Unknown => {}
            }
        }
        index += 1;
    }
    None
}

enum TagParse {
    Open { name: String, next: usize },
    Close { name: String, next: usize },
    SelfClosing { next: usize },
    Unknown,
}

fn read_name(chars: &[char], mut index: usize) -> (String, usize) {
    let mut name = String::new();
    while index < chars.len() && (chars[index].is_ascii_alphanumeric()) {
        name.push(chars[index]);
        index += 1;
    }
    (name, index)
}

fn skip_spaces(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

/// Parses a tag starting at `start` (which points at `<`).
fn parse_tag(chars: &[char], start: usize) -> TagParse {
    let mut index = start + 1;
    let closing = chars.get(index) == Some(&'/');
    if closing {
        index += 1;
    }
    let (name, after_name) = read_name(chars, index);
    if name.is_empty() || !SUBSET_TAGS.contains(&name.as_str()) {
        return TagParse::Unknown;
    }
    index = skip_spaces(chars, after_name);
    if closing {
        return match chars.get(index) {
            Some('>') => TagParse::Close {
                name,
                next: index + 1,
            },
            _ => TagParse::Unknown,
        };
    }
    // Attributes are understood on `span` only; anything else with
    // attributes leaves the subset.
    while index < chars.len() && chars[index] != '>' && chars[index] != '/' {
        if name != "span" {
            return TagParse::Unknown;
        }
        let (attribute, after_attribute) = read_name(chars, index);
        if attribute.is_empty() {
            return TagParse::Unknown;
        }
        index = skip_spaces(chars, after_attribute);
        if chars.get(index) != Some(&'=') {
            return TagParse::Unknown;
        }
        index = skip_spaces(chars, index + 1);
        let Some(&quote) = chars.get(index) else {
            return TagParse::Unknown;
        };
        if quote != '"' && quote != '\'' {
            return TagParse::Unknown;
        }
        index += 1;
        while index < chars.len() && chars[index] != quote {
            index += 1;
        }
        if index >= chars.len() {
            return TagParse::Unknown;
        }
        index = skip_spaces(chars, index + 1);
    }
    match chars.get(index) {
        Some('>') => TagParse::Open {
            name,
            next: index + 1,
        },
        Some('/') if chars.get(index + 1) == Some(&'>') => {
            TagParse::SelfClosing { next: index + 2 }
        }
        _ => TagParse::Unknown,
    }
}

enum EntityParse {
    Valid { next: usize },
    UnknownEntity(String),
    Unparseable,
}

/// Parses an entity starting at `start` (which points at `&`).
fn parse_entity(chars: &[char], start: usize) -> EntityParse {
    let mut index = start + 1;
    let mut body = String::new();
    while index < chars.len() && body.len() <= 16 {
        let current = chars[index];
        if current == ';' {
            if is_valid_entity_body(&body) {
                return EntityParse::Valid { next: index + 1 };
            }
            if body
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
                && body.chars().all(|part| part.is_ascii_alphanumeric())
            {
                return EntityParse::UnknownEntity(body);
            }
            return EntityParse::Unparseable;
        }
        if !current.is_ascii_alphanumeric() && current != '#' {
            // A bare `&` in running text: outside the validated subset.
            return EntityParse::Unparseable;
        }
        body.push(current);
        index += 1;
    }
    EntityParse::Unparseable
}

fn is_valid_entity_body(body: &str) -> bool {
    if KNOWN_ENTITIES.contains(&body) {
        return true;
    }
    if let Some(digits) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
        return !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_hexdigit());
    }
    if let Some(digits) = body.strip_prefix('#') {
        return !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_digit());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_subset_literals() {
        for text in [
            "plain text",
            "<b>bold</b>",
            "<b><i>nested</i></b>",
            "a &amp; b &lt; c &#38; d &#x26; e",
            "<span foreground=\"red\" size='small'>x</span>",
            "<big>big</big><small>small</small>",
            "self-closing <b/> tag",
        ] {
            assert_eq!(validate(text), MarkupVerdict::Valid, "{text}");
        }
    }

    #[test]
    fn provable_errors() {
        assert_eq!(
            validate("<b>bold</i>"),
            MarkupVerdict::Invalid(MarkupError::Mismatched {
                open: "b".to_owned(),
                close: "i".to_owned(),
            })
        );
        assert_eq!(
            validate("<u>never closed"),
            MarkupVerdict::Invalid(MarkupError::Unclosed {
                open: "u".to_owned(),
            })
        );
        assert_eq!(
            validate("stray </b> close"),
            MarkupVerdict::Invalid(MarkupError::Stray {
                close: "b".to_owned(),
            })
        );
        assert_eq!(
            validate("a &foo; b"),
            MarkupVerdict::Invalid(MarkupError::UnknownEntity {
                name: "foo".to_owned(),
            })
        );
        assert!(
            MarkupError::UnknownEntity {
                name: "foo".to_owned()
            }
            .message()
            .contains("&foo;")
        );
    }

    #[test]
    fn out_of_subset_constructs_are_unknown() {
        for text in [
            "<gradient from='RED' to='BLUE'>x</gradient>", // Manim extension tag
            "<html>outside subset</html>",
            "x < y",              // bare comparison, unterminated tag
            "a & b",              // bare ampersand
            "<b weird>x</b>",     // attributes on a non-span tag
            "<span a=b>x</span>", // unquoted attribute value
        ] {
            assert_eq!(validate(text), MarkupVerdict::Unknown, "{text}");
        }
    }

    #[test]
    fn error_before_unknown_still_fires_and_vice_versa() {
        assert!(matches!(
            validate("</b> then <custom>"),
            MarkupVerdict::Invalid(_)
        ));
        assert_eq!(validate("<custom> then </b>"), MarkupVerdict::Unknown);
    }

    #[test]
    fn matched_pair_detection_for_plain_text() {
        assert_eq!(matched_subset_pair("<b>bold</b>").as_deref(), Some("b"));
        assert_eq!(
            matched_subset_pair("x <span foreground=\"red\">y</span>").as_deref(),
            Some("span")
        );
        assert_eq!(matched_subset_pair("a < b > c"), None);
        assert_eq!(matched_subset_pair("<html>nope</html>"), None);
        assert_eq!(matched_subset_pair("<b>unclosed"), None);
    }
}
