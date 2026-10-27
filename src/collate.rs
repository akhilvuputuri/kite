//! `String.prototype.localeCompare(other, 'en')` for the strings npm sorts:
//! package paths, names and manifest keys.
//!
//! Node uses ICU root collation at tertiary strength with punctuation
//! non-ignorable. For ASCII that means: compare primary weights first
//! (letters case-insensitively, punctuation in DUCET order before digits
//! before letters), then case (lowercase before uppercase) left to right.
//! Non-ASCII characters sort after ASCII letters by code point, with
//! common Latin accents folded to their base letter at the primary level.

use std::cmp::Ordering;

/// DUCET order of ASCII punctuation and symbols (all sort before digits).
const PUNCT: &str = "\t\n\u{b}\u{c}\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary(c: char) -> (u32, u32) {
    if let Some(i) = PUNCT.find(c) {
        return (1, i as u32);
    }
    if c.is_ascii_digit() {
        return (2, c as u32 - '0' as u32);
    }
    if c.is_ascii_alphabetic() {
        return (3, c.to_ascii_lowercase() as u32 - 'a' as u32);
    }
    if let Some(base) = fold_latin(c) {
        return (3, base as u32 - 'a' as u32);
    }
    (4, c as u32)
}

fn fold_latin(c: char) -> Option<char> {
    let base = match c {
        'à'..='å' | 'À'..='Å' => 'a',
        'ç' | 'Ç' => 'c',
        'è'..='ë' | 'È'..='Ë' => 'e',
        'ì'..='ï' | 'Ì'..='Ï' => 'i',
        'ñ' | 'Ñ' => 'n',
        'ò'..='ö' | 'Ò'..='Ö' => 'o',
        'ù'..='ü' | 'Ù'..='Ü' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        _ => return None,
    };
    Some(base)
}

/// 0 = unaccented, 1 = accented (secondary level).
fn secondary(c: char) -> u32 {
    if c.is_ascii() {
        0
    } else if fold_latin(c).is_some() {
        1 + c as u32
    } else {
        0
    }
}

/// 0 = lowercase or uncased, 1 = uppercase (tertiary level).
fn tertiary(c: char) -> u32 {
    if c.is_uppercase() { 1 } else { 0 }
}

pub fn compare(a: &str, b: &str) -> Ordering {
    let pa: Vec<(u32, u32)> = a.chars().map(primary).collect();
    let pb: Vec<(u32, u32)> = b.chars().map(primary).collect();
    pa.cmp(&pb)
        .then_with(|| a.chars().map(secondary).cmp(b.chars().map(secondary)))
        .then_with(|| a.chars().map(tertiary).cmp(b.chars().map(tertiary)))
        .then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_node_icu_order() {
        // Verified order from Node 22 (`localeCompare(b, 'en')`).
        let expected = [
            "local-lib",
            "node_modules/@a/b",
            "node_modules/~a",
            "node_modules/$a",
            "node_modules/a",
            "node_modules/A",
            "node_modules/a_b",
            "node_modules/a-b",
            "node_modules/a-b/node_modules/c",
            "node_modules/a.b",
            "node_modules/a/node_modules/b",
            "node_modules/a0",
            "node_modules/ab",
            "node_modules/é",
            "node_modules/zed",
            "node_modules/Zed",
        ];
        let mut shuffled: Vec<&str> = expected.iter().rev().copied().collect();
        shuffled.sort_by(|a, b| compare(a, b));
        assert_eq!(shuffled, expected);
    }
}
