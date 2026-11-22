//! A name mask, such as `*.txt *.md`, as the window's Select and Unselect
//! commands take it, written as in Total Commander: masks apart by spaces or
//! `;`, double quotes around a mask that holds a space, the masks to leave
//! out after `|`, a regular expression after `<`, and after `>` the masks
//! of the names that go with a matched name, such as `*.jpg>*.cr2`. A
//! wildcard matches a whole name without regard to case, as a search's
//! name wildcard does.

use super::wildcard::{CodePointBuf, Wildcard};
use crate::fold::{fold, fold_into};
use regex::bytes::{Regex, RegexBuilder};

/// The largest compiled regular expression of a mask, as a query's.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// The names that one of its masks matches and none of the masks it leaves
/// out. `*.*` matches every name, also one without a dot, as it does in
/// Windows.
#[derive(Debug)]
pub struct NameMask {
    includes: Vec<Part>,
    excludes: Vec<Part>,
    /// After `>`: the names that also match while a name that `includes`
    /// matches has the same name without its extension.
    related: Vec<Part>,
}

#[derive(Debug)]
enum Part {
    Wildcard(Wildcard),
    /// Matched anywhere in the name without regard to case, so `^` and `$`
    /// anchor it.
    Regex(Regex),
}

/// Why a text is not a mask.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MaskError {
    /// An empty text, or one of separators only, such as `;`.
    #[error("the mask holds no wildcard")]
    Empty,
    #[error("the regular expression isn't valid: {0}")]
    Regex(String),
}

/// What [`NameMask::matches`] reuses from one name to the next, so a pass
/// over many names allocates nothing for each.
#[derive(Debug, Default)]
pub struct MaskScratch {
    folded: Vec<u8>,
    code_points: CodePointBuf,
}

impl NameMask {
    /// The mask that `text` writes. A text with only masks to leave out,
    /// such as `|*.bak`, matches every other name.
    ///
    /// # Errors
    ///
    /// [`MaskError::Empty`] when `text` holds no mask, and
    /// [`MaskError::Regex`] for a regular expression that does not compile.
    pub fn parse(text: &str) -> Result<NameMask, MaskError> {
        let (wanted, unwanted) = split_once(text, '|');
        let (wanted, related) = split_once(wanted, '>');
        let mut includes = parts(wanted)?;
        let excludes = unwanted.map_or_else(|| Ok(Vec::new()), parts)?;
        let related = related.map_or_else(|| Ok(Vec::new()), parts)?;
        if includes.is_empty() {
            if excludes.is_empty() {
                return Err(MaskError::Empty);
            }
            includes.push(Part::Wildcard(Wildcard::compile(b"*")));
        }
        Ok(NameMask {
            includes,
            excludes,
            related,
        })
    }

    /// Whether the mask matches `name`, in WTF-8.
    pub fn matches(&self, name: &[u8], scratch: &mut MaskScratch) -> bool {
        self.fold(name, scratch);
        any(&self.includes, name, scratch) && !any(&self.excludes, name, scratch)
    }

    /// Whether `name` is one of the names that go with a matched name, which
    /// match while a matched name has the same name without its extension.
    pub fn relates(&self, name: &[u8], scratch: &mut MaskScratch) -> bool {
        if self.related.is_empty() {
            return false;
        }
        self.fold(name, scratch);
        any(&self.related, name, scratch) && !any(&self.excludes, name, scratch)
    }

    /// Whether the mask names, after `>`, names that go with a matched name.
    pub fn has_related(&self) -> bool {
        !self.related.is_empty()
    }

    fn fold(&self, name: &[u8], scratch: &mut MaskScratch) {
        scratch.folded.clear();
        fold_into(name, &mut scratch.folded);
    }
}

fn any(parts: &[Part], name: &[u8], scratch: &mut MaskScratch) -> bool {
    parts.iter().any(|part| match part {
        Part::Wildcard(wildcard) => {
            wildcard.matches_into(&scratch.folded, &mut scratch.code_points)
        }
        Part::Regex(regex) => regex.is_match(name),
    })
}

/// `text` before and after the first `separator` outside double quotes.
fn split_once(text: &str, separator: char) -> (&str, Option<&str>) {
    let mut quoted = false;
    for (at, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c == separator && !quoted => {
                return (&text[..at], Some(&text[at + c.len_utf8()..]));
            }
            _ => {}
        }
    }
    (text, None)
}

/// The masks of `text`, apart by spaces or `;` outside double quotes.
fn parts(text: &str) -> Result<Vec<Part>, MaskError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if !quoted && (c == ';' || c.is_whitespace()) => {
                words.push(std::mem::take(&mut word));
            }
            c => word.push(c),
        }
    }
    words.push(word);
    (words.into_iter())
        .filter(|word| !word.is_empty())
        .map(|word| match word.strip_prefix('<') {
            Some(pattern) => RegexBuilder::new(pattern)
                .case_insensitive(true)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map(Part::Regex)
                .map_err(|error| MaskError::Regex(error.to_string())),
            None if word == "*.*" => Ok(Part::Wildcard(Wildcard::compile(b"*"))),
            None => Ok(Part::Wildcard(Wildcard::compile(&fold(word.as_bytes())))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(mask: &str, name: &str) -> bool {
        let mask = NameMask::parse(mask).expect("the mask holds a wildcard");
        mask.matches(name.as_bytes(), &mut MaskScratch::default())
    }

    /// Each part matches whole names without regard to case, apart by
    /// spaces or `;`.
    #[test]
    fn a_mask_matches_whole_names_by_any_part() {
        assert!(matches("*.txt *.md", "Notes.TXT"));
        assert!(matches("*.txt;*.md", "read.me.md"));
        assert!(!matches("*.txt; *.md", "notes.txt.bak"));
    }

    /// `*.*` selects every file, as in Windows and Total Commander.
    #[test]
    fn star_dot_star_matches_a_name_without_a_dot() {
        assert!(matches("*.*", "README"));
    }

    #[test]
    fn a_mask_without_a_wildcard_is_refused() {
        assert_eq!(NameMask::parse(" ; ").unwrap_err(), MaskError::Empty);
    }

    /// Total Commander's syntax: quotes keep a space in one mask, the masks
    /// after `|` are left out, and a mask after `<` is a regular expression.
    #[test]
    fn quotes_exclusions_and_regular_expressions_read_as_in_total_commander() {
        assert!(matches("\"My report*.docx\"", "my report 2.docx"));
        assert!(!matches("My report*.docx", "my report 2.docx"));
        assert!(matches("*.*|*.bak *.old", "a.txt"));
        assert!(!matches("*.*|*.bak *.old", "a.OLD"));
        assert!(!matches("|*.bak", "a.bak"));
        assert!(matches("<^IMG_\\d+\\.jpe?g$", "img_0042.JPG"));
        assert!(matches!(NameMask::parse("<("), Err(MaskError::Regex(_))));
    }
}
