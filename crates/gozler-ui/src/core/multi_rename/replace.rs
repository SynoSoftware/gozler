//! Search and replace on the masks' result (DESIGN.md: Multi-Rename,
//! "Search and replace"). Plain text becomes a regular expression too, so
//! one matcher serves both, and `*` and `?` need no matcher of their own.

use super::{Case, first_letter_upper};
use regex::{Captures, Regex, RegexBuilder};

/// The search and replace fields, read once for every change of a field.
#[derive(Debug)]
pub(super) enum Replacer {
    /// Search for is empty.
    None,
    /// Pairs apart by `|`, each applied in turn.
    Plain {
        pairs: Vec<(Regex, String)>,
        first_only: bool,
    },
    Pattern {
        regex: Regex,
        replacement: Vec<Token>,
        first_only: bool,
        /// Whole name: a match replaces the whole text.
        whole: bool,
    },
}

/// A part of a regular expression's Replace with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Token {
    Text(String),
    Group { group: Group, case: Case },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Group {
    Index(usize),
    Name(String),
}

/// The fields that make a [`Replacer`].
pub(super) struct Wanted<'a> {
    pub(super) search: &'a str,
    pub(super) replace: &'a str,
    pub(super) match_case: bool,
    pub(super) first_only: bool,
    pub(super) regex: bool,
    pub(super) whole_name: bool,
}

impl Replacer {
    /// The matcher of `wanted`, or the reason a regular expression does not
    /// parse, as the regex crate gives it.
    pub(super) fn new(wanted: &Wanted<'_>) -> Result<Replacer, String> {
        if wanted.search.is_empty() {
            return Ok(Replacer::None);
        }
        let build = |pattern: &str| {
            RegexBuilder::new(pattern)
                .case_insensitive(!wanted.match_case)
                .build()
        };
        if wanted.regex {
            let regex = build(wanted.search).map_err(|error| reason(&error))?;
            return Ok(Replacer::Pattern {
                regex,
                replacement: tokens(wanted.replace),
                first_only: wanted.first_only,
                whole: wanted.whole_name,
            });
        }
        let mut replacements = wanted.replace.split('|');
        let pairs = wanted
            .search
            .split('|')
            .map(|search| (search, replacements.next().unwrap_or("")))
            .filter(|(search, _)| !search.is_empty())
            .map(|(search, replace)| {
                let regex = build(&wildcards(search)).expect("an escaped pattern parses");
                (regex, replace.to_owned())
            })
            .collect();
        Ok(Replacer::Plain {
            pairs,
            first_only: wanted.first_only,
        })
    }

    pub(super) fn apply(&self, text: String) -> String {
        match self {
            Replacer::None => text,
            Replacer::Plain { pairs, first_only } => {
                let limit = if *first_only { 1 } else { 0 };
                pairs.iter().fold(text, |text, (regex, replace)| {
                    regex
                        .replacen(&text, limit, regex::NoExpand(replace))
                        .into_owned()
                })
            }
            Replacer::Pattern {
                regex,
                replacement,
                first_only,
                whole,
            } => {
                if *whole {
                    return match regex.captures(&text) {
                        Some(captures) => expand(replacement, &captures),
                        None => text,
                    };
                }
                let limit = if *first_only { 1 } else { 0 };
                regex
                    .replacen(&text, limit, |captures: &Captures<'_>| {
                        expand(replacement, captures)
                    })
                    .into_owned()
            }
        }
    }
}

/// A plain search as a regular expression: `*` any characters, the last
/// one to the end of the name, others as few as the rest allows, so
/// `(*)` removes each bracketed part and `abc*` everything from `abc`;
/// `?` one character.
fn wildcards(search: &str) -> String {
    let last_star = search.rfind('*');
    let mut pattern = String::new();
    for (at, c) in search.char_indices() {
        match c {
            '*' if Some(at) == last_star && at + 1 == search.len() => pattern.push_str(".*"),
            '*' => pattern.push_str(".*?"),
            '?' => pattern.push('.'),
            c => pattern.push_str(&regex::escape(c.encode_utf8(&mut [0; 4]))),
        }
    }
    pattern
}

/// The line of a regex error that says what is wrong, without the copy of
/// the pattern and its caret.
fn reason(error: &regex::Error) -> String {
    let text = error.to_string();
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    line.trim().trim_start_matches("error: ").to_owned()
}

/// Replace with's parts: `$1` to `$9`, `${name}`, `$$` for `$`, and `\U`,
/// `\L`, `\F` or `\n` before a group for that group's case.
fn tokens(replace: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut case = Case::Unchanged;
    let mut chars = replace.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next = chars.peek().copied();
                let named = match next {
                    Some('U') => Some(Case::Upper),
                    Some('L') => Some(Case::Lower),
                    Some('F') => Some(Case::FirstLetter),
                    Some('n') => Some(Case::Unchanged),
                    _ => None,
                };
                match named {
                    Some(named) => {
                        case = named;
                        chars.next();
                    }
                    None => text.push('\\'),
                }
            }
            '$' => {
                let group = match chars.peek().copied() {
                    Some('$') => {
                        chars.next();
                        text.push('$');
                        continue;
                    }
                    Some(digit @ '1'..='9') => {
                        chars.next();
                        Group::Index(digit as usize - '0' as usize)
                    }
                    Some('{') => {
                        let name: String =
                            chars.clone().skip(1).take_while(|&c| c != '}').collect();
                        let closed = chars.clone().nth(1 + name.chars().count()) == Some('}');
                        if !closed || name.is_empty() {
                            text.push('$');
                            continue;
                        }
                        for _ in 0..name.chars().count() + 2 {
                            chars.next();
                        }
                        match name.parse() {
                            Ok(index) => Group::Index(index),
                            Err(_) => Group::Name(name),
                        }
                    }
                    _ => {
                        text.push('$');
                        continue;
                    }
                };
                if !text.is_empty() {
                    tokens.push(Token::Text(std::mem::take(&mut text)));
                }
                tokens.push(Token::Group { group, case });
                case = Case::Unchanged;
            }
            c => text.push(c),
        }
    }
    if !text.is_empty() {
        tokens.push(Token::Text(text));
    }
    tokens
}

fn expand(tokens: &[Token], captures: &Captures<'_>) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Text(text) => out.push_str(text),
            Token::Group { group, case } => {
                let found = match group {
                    Group::Index(index) => captures.get(*index),
                    Group::Name(name) => captures.name(name),
                };
                let found = found.map_or("", |found| found.as_str());
                match case {
                    Case::Upper => out.push_str(&found.to_uppercase()),
                    Case::Lower => out.push_str(&found.to_lowercase()),
                    Case::FirstLetter => out.push_str(&first_letter_upper(found)),
                    _ => out.push_str(found),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(search: &str, replace: &str) -> Replacer {
        Replacer::new(&Wanted {
            search,
            replace,
            match_case: false,
            first_only: false,
            regex: false,
            whole_name: false,
        })
        .unwrap()
    }

    /// Pairs apart by `|` apply in turn and ignore case but not accents;
    /// `*` takes as few characters as it can, except at the end.
    #[test]
    fn plain_pairs_and_wildcards() {
        let replacer = plain("ä|ö|X", "ae|oe");
        assert_eq!(replacer.apply("Äpfel ö x".into()), "aepfel oe ");
        assert_eq!(plain("(*)", "").apply("a (1) b (2)".into()), "a  b ");
        assert_eq!(plain("b*", "").apply("abcb".into()), "a");
        assert_eq!(plain("a?c", "-").apply("abcadc".into()), "--");
    }

    /// `\U` and the like change only the next group, and Whole name keeps
    /// the text when nothing matches.
    #[test]
    fn regex_groups_and_whole_name() {
        let pattern = |replace: &str, whole: bool| {
            Replacer::new(&Wanted {
                search: r"(\w+)-(?<day>\d+)",
                replace,
                match_case: true,
                first_only: false,
                regex: true,
                whole_name: whole,
            })
            .unwrap()
        };
        assert_eq!(
            pattern(r"\U$1 ${day} $1$$", false).apply("ab-12 x".into()),
            "AB 12 ab$ x"
        );
        assert_eq!(pattern("$2", true).apply("x ab-12 y".into()), "12");
        assert_eq!(pattern("$2", true).apply("nothing".into()), "nothing");
        let broken = Replacer::new(&Wanted {
            search: "(",
            replace: "",
            match_case: false,
            first_only: false,
            regex: true,
            whole_name: false,
        });
        assert_eq!(broken.unwrap_err(), "unclosed group");
    }
}
