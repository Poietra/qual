//! Python-escape / TeX-command collision detection for `MLR103`
//! (DESIGN §7.2 prose).
//!
//! The check works on the **raw source text** of a non-raw string literal
//! (prefix and quotes included), so `"\\frac"` (a correctly escaped
//! backslash) and `r"\frac"` are distinguishable from `"\frac"`. Only the
//! curated collisions below fire: a recognized short Python escape
//! (`\f`, `\a`, `\b`, `\t`, `\v`, `\r`) whose escape character together
//! with the letters that immediately follow it spells a known TeX command.
//! Intentional `\n` newlines are never flagged.

/// One escape/TeX-command collision found in the raw literal text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeCollision {
    /// Byte offset of the backslash inside the scanned text.
    pub offset: usize,
    /// The colliding Python escape as written, e.g. `\f`.
    pub escape: String,
    /// The TeX command the author plausibly meant, e.g. `frac` (the
    /// escape character plus the letters that follow it).
    pub command: String,
    /// Human name of the control character the escape produces.
    pub control_character: &'static str,
}

/// TeX commands (without backslash) that collide with a short Python
/// escape, keyed by their first letter being one of `f a b t v r`.
/// Matching is exact against the maximal ASCII-letter run after the
/// backslash, so `\tot` (not a command) never fires while `\tau2` does.
const EXACT_COMMANDS: &[&str] = &[
    // \f — form feed
    "fbox",
    "flat",
    "footnotesize",
    "forall",
    "frac",
    "frown",
    // \a — bell
    "aleph",
    "alpha",
    "amalg",
    "angle",
    "approx",
    "ast",
    "asymp",
    "atop",
    // \b — backspace
    "bar",
    "begin",
    "beta",
    "big",
    "binom",
    "bmod",
    "bot",
    "boxed",
    "breve",
    "bullet",
    // \t — tab
    "tan",
    "tanh",
    "tau",
    "text",
    "tfrac",
    "therefore",
    "theta",
    "tilde",
    "times",
    "tiny",
    "to",
    "top",
    "triangle",
    // \v — vertical tab
    "vdash",
    "vdots",
    "vec",
    "vee",
    "verb",
    "vert",
    // \r — carriage return
    "rangle",
    "rceil",
    "ref",
    "renewcommand",
    "rfloor",
    "rho",
    "right",
    "rm",
];

/// Command families matched by prefix so `\textbf`, `\varphi`, `\arccos`,
/// `\bigcup`, and `\rightarrow` collide as well.
const PREFIX_FAMILIES: &[&str] = &["arc", "big", "right", "text", "var"];

/// Python short escapes that produce a control character colliding with a
/// TeX command start, mapped to the produced character's name.
const COLLIDING_ESCAPES: &[(char, &str)] = &[
    ('f', "form feed"),
    ('a', "bell"),
    ('b', "backspace"),
    ('t', "tab"),
    ('v', "vertical tab"),
    ('r', "carriage return"),
];

fn is_known_command(word: &str) -> bool {
    EXACT_COMMANDS.contains(&word)
        || PREFIX_FAMILIES
            .iter()
            .any(|family| word.len() > family.len() && word.starts_with(family))
}

/// Scans the raw source text of a non-raw string literal for escape
/// sequences that collide with a plausible TeX command.
///
/// `raw_literal` is the source slice covering the literal exactly
/// (prefix and quotes included). Escaped backslashes (`\\`) and every
/// escape not in the curated collision set are skipped.
#[must_use]
pub fn find_collisions(raw_literal: &str) -> Vec<EscapeCollision> {
    let bytes = raw_literal.as_bytes();
    let mut collisions = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let Some(&next) = bytes.get(index + 1) else {
            break;
        };
        if next == b'\\' {
            // A correctly escaped backslash: skip both characters.
            index += 2;
            continue;
        }
        let escape_char = char::from(next);
        let Some((_, control_character)) = COLLIDING_ESCAPES
            .iter()
            .find(|(escape, _)| *escape == escape_char)
        else {
            index += 2;
            continue;
        };
        // The maximal ASCII-letter run starting at the escape character.
        let mut end = index + 1;
        while end < bytes.len() && bytes[end].is_ascii_alphabetic() {
            end += 1;
        }
        let word = &raw_literal[index + 1..end];
        // "immediately followed by ascii letters": the escape character
        // alone (e.g. a bare `\t` tab) never fires.
        if word.len() >= 2 && is_known_command(word) {
            collisions.push(EscapeCollision {
                offset: index,
                escape: format!("\\{escape_char}"),
                command: word.to_owned(),
                control_character,
            });
        }
        index = end.max(index + 2);
    }
    collisions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_curated_collisions() {
        for (literal, command) in [
            (r#""\frac{1}{2}""#, "frac"),
            (r#""\alpha + x""#, "alpha"),
            (r#""\begin{align}""#, "begin"),
            (r#""\tau + \pi""#, "tau"),
            (r#""a \times b""#, "times"),
            (r#""\textbf{x}""#, "textbf"),
            (r#""\varphi""#, "varphi"),
            (r#""\rightarrow""#, "rightarrow"),
            (r#""\vec{v}""#, "vec"),
        ] {
            let collisions = find_collisions(literal);
            assert_eq!(collisions.len(), 1, "{literal} must collide");
            assert_eq!(collisions[0].command, command, "{literal}");
        }
    }

    #[test]
    fn escaped_backslashes_and_benign_escapes_do_not_fire() {
        for literal in [
            r#""\\frac{1}{2}""#, // escaped backslash: correct TeX spelling
            r#""a\nb""#,         // intentional newline is never flagged
            r#""\t""#,           // bare tab with no following letters
            r#""x + y""#,        // no backslash at all
            r#""\tomato""#,      // not a plausible TeX command
            r#""\phi""#,         // \p is not a Python short escape
        ] {
            assert!(
                find_collisions(literal).is_empty(),
                "{literal} must not collide"
            );
        }
    }

    #[test]
    fn reports_every_collision_in_order() {
        let collisions = find_collisions(r#""\frac{\tau}{2}""#);
        let commands: Vec<&str> = collisions
            .iter()
            .map(|collision| collision.command.as_str())
            .collect();
        assert_eq!(commands, ["frac", "tau"]);
    }

    #[test]
    fn trailing_backslash_is_ignored() {
        assert!(find_collisions("\"x\\").is_empty());
    }
}
