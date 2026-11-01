//! Query tokenizer and grammar for gozler's search syntax.
//!
//! Supported: space-separated terms (implicit AND), `"quoted phrases"`,
//! `*`/`?` wildcards (anchored to the whole name), path terms (a term
//! containing `\` or `/`, quoted or not, or a `path:` value; a path term with
//! a wildcard is anchored to the whole full path), the functions in
//! `SEARCH_FUNCTIONS`, `!` before a term to exclude it, and `|` between terms
//! to match either. As in es, `|` binds tighter than the space, so `a|b c` is
//! `(a or b) and c`, and quotes make operators and spaces plain text, also in
//! a function's value: `path:"C:\Program Files"`. In a `regex:` value, `|`
//! and `!` belong to the expression without quotes.
//!
//! The parser writes every path the way the daemon compares it: `/` becomes
//! `\`, and a path term that starts with `.\` or `..\`, and a relative
//! `parent:` folder, are joined to the working directory
//! ([`ParseOptions::working_dir`]). A regular expression is never rewritten.
//!
//! A `dm:` value is resolved to UTC bounds here, from the query start
//! ([`ParseOptions::query_start`]), by the date resolver in `dates`.
//!
//! `<` and `>` group expressions. Nesting and Boolean distribution are bounded
//! before expanding clauses, so a short query cannot exhaust the parser.
//!
//! The parser turns the text into a [`ParsedQuery`], which the client sends to
//! the daemon as it is: AND-ed clauses, each an OR of terms. Apart from those
//! paths, terms keep the text as typed; the daemon folds them when it
//! compiles the query ([`super::CompiledQuery`]).

use super::dates::{DateField, DateRange, QueryStart, date_range};
use crate::attributes::Attributes;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Folder,
}

/// An inclusive byte-size range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeRange {
    pub min: u64,
    pub max: u64,
}

impl SizeRange {
    pub fn contains(self, n: u64) -> bool {
        self.min <= n && n <= self.max
    }
}

/// A word that `size:` takes in place of a number, and the range it names.
pub struct SizeWord {
    /// The word, matched case-insensitively.
    pub word: &'static str,
    pub range: SizeRange,
}

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;
const TB: u64 = 1024 * GB;

/// The size words, smallest first, as Everything defines them: each starts
/// above the previous one's maximum, so a size on a boundary has one word.
pub const SIZE_WORDS: &[SizeWord] = &[
    SizeWord {
        word: "empty",
        range: SizeRange { min: 0, max: 0 },
    },
    SizeWord {
        word: "tiny",
        range: SizeRange {
            min: 1,
            max: 10 * KB,
        },
    },
    SizeWord {
        word: "small",
        range: SizeRange {
            min: 10 * KB + 1,
            max: 100 * KB,
        },
    },
    SizeWord {
        word: "medium",
        range: SizeRange {
            min: 100 * KB + 1,
            max: MB,
        },
    },
    SizeWord {
        word: "large",
        range: SizeRange {
            min: MB + 1,
            max: 16 * MB,
        },
    },
    SizeWord {
        word: "huge",
        range: SizeRange {
            min: 16 * MB + 1,
            max: 128 * MB,
        },
    },
    SizeWord {
        word: "gigantic",
        range: SizeRange {
            min: 128 * MB + 1,
            max: u64::MAX,
        },
    },
];

/// A parsed query: the entries that match every clause.
///
/// The client sends it to the daemon as part of a
/// [`SearchRequest`](crate::proto::SearchRequest), so it is a wire type: it
/// only grows, by new [`Test`] variants and new fields, and each such change
/// raises [`PROTO_VERSION`](crate::proto::PROTO_VERSION), so a daemon that
/// cannot read the new form refuses the client's `Hello` instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedQuery {
    pub clauses: Vec<Clause>,
    /// Whether text terms match case-sensitively, after any `case:` directive.
    pub match_case: bool,
}

/// Terms of which at least one must match; a clause with no terms matches
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    pub terms: Vec<Term>,
}

impl Clause {
    /// A clause of one term that must match.
    pub(crate) fn single(test: Test) -> Self {
        Clause {
            terms: vec![Term {
                negated: false,
                test,
            }],
        }
    }
}

/// One test, or with `negated` its opposite. Size and date comparisons never
/// match unknown values, negated or not, so an unread file is not reported as
/// small. [`Test::Unknown`] tests missing metadata explicitly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    pub negated: bool,
    pub test: Test,
}

/// What a [`Term`] tests. Text is as typed, apart from paths, which hold `\`
/// separators and no relative form; the daemon folds it unless the query
/// matches case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Test {
    /// The name contains this text.
    Name(String),
    /// The full path contains this text.
    Path(String),
    /// The whole name matches this pattern of `*` and `?`.
    NameWildcard(String),
    /// The whole full path matches this pattern of `*` and `?`.
    PathWildcard(String),
    /// The entry is directly in this folder: its full path without its last
    /// separator and name equals this absolute path, a trailing separator
    /// ignored. From protocol 3.
    Parent(String),
    /// The name contains this text as a whole word: with no letter or digit
    /// right before or after it. From protocol 4.
    NameWholeWord(String),
    /// The full path contains this text as a whole word. From protocol 4.
    PathWholeWord(String),
    /// This regular expression, in the `regex` crate's syntax, matches
    /// somewhere in the name. From protocol 4.
    NameRegex(String),
    /// This regular expression matches somewhere in the full path. From
    /// protocol 4.
    PathRegex(String),
    /// The name contains this text where a word starts: at the start of the
    /// name, or after a character that is not a letter or digit. From
    /// protocol 7.
    NameWordStart(String),
    /// The full path contains this text where a word starts. From protocol 7.
    PathWordStart(String),
    /// The name starts with this text. From protocol 7.
    NameStart(String),
    /// The full path starts with this text. From protocol 7.
    PathStart(String),
    /// Positive bare literal terms in their typed order, without overlapping
    /// matches. [`TermMatch::NameStart`] anchors the first term and matches
    /// later terms at word starts; the other modes apply to every term.
    Ordered {
        terms: Vec<String>,
        term_match: TermMatch,
        match_path: bool,
    },
    /// The extension is one of these, without the dot, compared ignoring case
    /// whatever the query's case.
    Extension(Vec<String>),
    /// The file's size is in this range.
    Size(SizeRange),
    Kind(Kind),
    /// The date modified is in this range, resolved by the client from its
    /// clock and time zone. From protocol 5.
    DateModified(DateRange),
    /// The date created is in this range, resolved as for
    /// [`Test::DateModified`]. Only an index with Dates created on holds
    /// the dates. From protocol 13.
    DateCreated(DateRange),
    /// The entry has every one of these attributes. From protocol 6.
    Attributes(Attributes),
    Unknown(Metadata),
    /// Length in UTF-16 code units, as Windows counts names and paths.
    Length {
        target: LengthTarget,
        range: SizeRange,
    },
    /// A folder with an immediate visible child whose name matches this text
    /// or whole-name wildcard pattern.
    Child(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metadata {
    Size,
    DateModified,
    DateCreated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LengthTarget {
    Name,
    FullPath,
}

impl ParsedQuery {
    /// Whether a term tests the size or a date, so an answer from a volume
    /// whose sizes and dates could not be read may leave files out.
    pub fn tests_size_or_date(&self) -> bool {
        self.clauses.iter().flat_map(|c| &c.terms).any(|t| {
            matches!(
                t.test,
                Test::Size(_) | Test::DateModified(_) | Test::DateCreated(_) | Test::Unknown(_)
            )
        })
    }

    /// Whether a term tests the date created, which only an index with
    /// Dates created on holds.
    pub fn tests_date_created(&self) -> bool {
        self.clauses.iter().flat_map(|c| &c.terms).any(|t| {
            matches!(
                t.test,
                Test::DateCreated(_) | Test::Unknown(Metadata::DateCreated)
            )
        })
    }

    /// `true` when no term tests text: the query matches everything its
    /// other tests let through.
    #[cfg(test)]
    pub(crate) fn is_text_empty(&self) -> bool {
        !self.clauses.iter().flat_map(|c| &c.terms).any(|t| {
            matches!(
                t.test,
                Test::Name(_)
                    | Test::Path(_)
                    | Test::NameWildcard(_)
                    | Test::PathWildcard(_)
                    | Test::NameWholeWord(_)
                    | Test::PathWholeWord(_)
                    | Test::NameRegex(_)
                    | Test::PathRegex(_)
                    | Test::NameWordStart(_)
                    | Test::PathWordStart(_)
                    | Test::NameStart(_)
                    | Test::PathStart(_)
                    | Test::Ordered { .. }
            )
        })
    }
}

/// A query that cannot be executed as written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("query: groups may nest at most {0} levels")]
    GroupDepth(usize),
    /// A `name:` term whose name is no search function; carries the name.
    #[error("unknown search function: {0}:")]
    UnknownFunction(String),
    /// A value or part of the query that does not parse.
    #[error("{0}")]
    Malformed(Malformed),
    /// A relative path, which only a working directory can resolve, in a
    /// query parsed without one; carries the path.
    #[error(
        "{0} is a relative path, and this query has no working directory to resolve it; give the full path"
    )]
    RelativePath(String),
    /// A `dm:` or `dc:` term in a query parsed without a query start to
    /// resolve it.
    #[error("{0} needs the time the search starts, and this query was parsed without one")]
    NoQueryStart(DateField),
    /// More terms than a query may hold; carries the query's count.
    #[error("the query has {0} terms; a query may have at most {max}", max = super::engine::MAX_QUERY_TERMS)]
    TooManyTerms(usize),
    /// More regular expressions than a query may hold; carries the query's
    /// count.
    #[error(
        "the query has {0} regular expressions; a query may have at most {max}",
        max = super::engine::MAX_QUERY_REGEXES
    )]
    TooManyRegexes(usize),
}

/// What is wrong with a value or a part of the query that does not parse.
/// Each reason belongs to one search function or to the query, which its
/// message names first, ending in a colon, before what is wrong and how to
/// fix it. A `String` carries the text that is wrong, as typed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Malformed {
    #[error("query: use balanced < and > around a nonempty expression")]
    Grouping,
    #[error("length: {0} is not a character count; use a number, comparison or range")]
    Length(String),
    /// A `!` that does not start a term.
    #[error("query: ! must start a term; quote it to search for it")]
    MisplacedNot,
    /// A `!` with no term after it.
    #[error("query: ! needs a term after it")]
    NotWithoutTerm,
    /// A `|` without a term on each side.
    #[error("query: | needs a term on each side")]
    OrWithoutTerms,
    /// `!` or `|` applied to a term that tests nothing, such as `case:`.
    #[error("query: {0} tests nothing, so ! and | cannot apply to it")]
    OperatorOnNothing(String),
    #[error("size: {0} is not a size; write a number with kb, mb, gb or tb, such as 10mb")]
    Size(String),
    /// A date range whose lower value starts after its upper one.
    #[error("{0} {1} is a reversed range; write the earlier date first")]
    ReversedDates(DateField, String),
    /// A date before or after every date the resolver can hold.
    #[error("{0} {1} is out of range")]
    DateOutOfRange(DateField, String),
    /// `Ndays` without a number of days above 0.
    #[error("{0} {1} needs a number of days above 0")]
    Days(DateField, String),
    #[error("{0} {1} is not a date; use today, Ndays, lastweek, thismonth or YYYY-MM-DD")]
    Date(DateField, String),
    /// `parent:` without a folder.
    #[error("parent: needs a folder")]
    NoParentFolder,
    #[error(
        "attrib: {0} is not an attribute letter; the letters are {letters}",
        letters = Attributes::all_letters()
    )]
    AttributeLetter(char),
    /// A regular expression that does not compile, or compiles past its
    /// size limit; carries the `regex` crate's message, in English.
    #[error("regular expression: {0}")]
    Regex(String),
}

/// One search function: its names, what it does, and its line in the help.
pub(crate) struct SearchFunction {
    /// Names without the colon, matched case-insensitively. The first is the
    /// one the help lists first.
    pub(crate) names: &'static [&'static str],
    function: Function,
    /// The value the help shows after each name, such as `>1mb` for `size:`.
    pub(crate) example: &'static str,
    pub(crate) help: &'static str,
}

/// What a [`SearchFunction`] does to the query.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Function {
    Ext,
    Size,
    Length(LengthTarget),
    Child,
    Date(DateField),
    Path,
    Parent,
    NoPath,
    WholeWord,
    WordStart,
    NameStart,
    WholeName,
    Regex,
    Attributes,
    File,
    Folder,
    /// Handled by [`case_directive`] before the other functions; a `case:`
    /// value it does not accept is searched as text.
    Case,
}

/// Every search function, in the order the help lists them. A `name:` token
/// that names none of them is refused ([`QueryError::UnknownFunction`]).
pub(crate) const SEARCH_FUNCTIONS: &[SearchFunction] = &[
    SearchFunction {
        names: &["ext"],
        function: Function::Ext,
        example: "cs;txt",
        help: "Extension is one in the list; `ext:*.cs` also works; `ext:` alone finds files without one",
    },
    SearchFunction {
        names: &["size"],
        function: Function::Size,
        example: ">1mb",
        help: "Size of a file, or of the files below a folder; also `<`, `<=`, `>=`, `=`, \
               ranges such as `1mb..10mb`, and `empty`, `tiny`, `small`, `medium`, `large`, \
               `huge` or `gigantic`; `empty` is 0 bytes, and `unknown` finds items whose \
               size gozler does not know",
    },
    SearchFunction {
        names: &["dm", "datemodified", "date-modified"],
        function: Function::Date(DateField::Modified),
        example: "today",
        help: "Date modified: `today`, `yesterday`, `YYYY-MM-DD`, `Nminutes`, `Nhours`, `Ndays`, `lastNdays`, `Nweeks`, \
               or `thisweek`, `lastmonth`, `thisyear` and other calendar periods; also `<`, `<=`, `>`, \
               `>=` and ranges such as `2026-01-01..2026-03-31`; `unknown` finds items whose \
               date gozler does not know",
    },
    SearchFunction {
        names: &["dc", "datecreated", "date-created"],
        function: Function::Date(DateField::Created),
        example: "today",
        help: "Date created, with the values of `dm:`; needs Dates created, on the window's \
               Settings > Drives and service page",
    },
    SearchFunction {
        names: &["path", "full-path"],
        function: Function::Path,
        example: "text",
        help: "Full path contains text; with `*` or `?`, the whole full path must match",
    },
    SearchFunction {
        names: &["parent"],
        function: Function::Parent,
        example: r"C:\repo",
        help: "Only the files and folders directly in this folder; a relative folder \
               starts from the current folder",
    },
    SearchFunction {
        names: &["nopath", "name"],
        function: Function::NoPath,
        example: "text",
        help: "Name contains text, also when other terms test the full path; \
               with `*` or `?`, the whole name must match",
    },
    SearchFunction {
        names: &["name-length", "namelen"],
        function: Function::Length(LengthTarget::Name),
        example: ">100",
        help: "Name length in UTF-16 characters; accepts comparisons and ranges such as `10..100`",
    },
    SearchFunction {
        names: &["full-path-length", "fullpathlen"],
        function: Function::Length(LengthTarget::FullPath),
        example: ">260",
        help: "Full path length in UTF-16 characters, including the mount prefix",
    },
    SearchFunction {
        names: &["child"],
        function: Function::Child,
        example: "Cargo.toml",
        help: "Folders with an immediate visible child whose name contains text; `*` and `?` match the whole child name",
    },
    SearchFunction {
        names: &["ww"],
        function: Function::WholeWord,
        example: "word",
        help: "Name contains word, with no letter or digit right before or after it, \
               so `ww:log` finds app.log but not catalog; with `\\` or `/` in the value, \
               the full path must contain it",
    },
    SearchFunction {
        names: &["wordstart"],
        function: Function::WordStart,
        example: "ann",
        help: "A word in the name starts with ann, as in my_annual_report.pdf; \
               with `\\` or `/` in the value, a word in the full path must",
    },
    SearchFunction {
        names: &["startwith"],
        function: Function::NameStart,
        example: "read",
        help: "Name starts with read; with `\\` or `/` in the value, the full path must",
    },
    SearchFunction {
        names: &["wfn", "wholefilename", "whole-filename", "whole", "entire"],
        function: Function::WholeName,
        example: "readme.txt",
        help: "Match the whole name, with `*` and `?` as wildcards; \
               `wfn:regex:pattern` matches the whole name as a regular expression",
    },
    SearchFunction {
        names: &["regex"],
        function: Function::Regex,
        example: r"^test\d+\.cs$",
        help: "Name matches a regular expression in Rust syntax; with `\\\\` in the \
               pattern, the full path must",
    },
    SearchFunction {
        names: &["attrib", "attributes", "attr"],
        function: Function::Attributes,
        example: "hs",
        help: "Has every attribute in the value, here hidden and system; letters in \
               either case; `!attrib:h` excludes hidden items",
    },
    SearchFunction {
        names: &["file"],
        function: Function::File,
        example: "",
        help: "Files only; `file:text` also matches text",
    },
    SearchFunction {
        names: &["folder", "dir"],
        function: Function::Folder,
        example: "",
        help: "Folders only; `folder:text` also matches text",
    },
    SearchFunction {
        names: &["case"],
        function: Function::Case,
        example: "",
        help: "Match case for the whole query; `case:1` also enables it, and \
               `case:0` turns it off",
    },
];

/// Recognizes the `case:` directive: `case:`/`case:1` enable case-sensitivity,
/// `case:0` disables it. Returns `None` for anything else (so it is not
/// mistaken for a search term). Case-insensitive on the function name itself.
fn case_directive(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "case:" | "case:1" => Some(true),
        "case:0" => Some(false),
        _ => None,
    }
}

/// How [`parse_query_with`] reads a query, beyond its text: what the command
/// line's switches and working directory say.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParseOptions<'a> {
    /// es `-case`: whether text terms match case unless a `case:` directive
    /// says otherwise.
    pub match_case: bool,
    /// es `-p`: whether text terms without a separator match the full path,
    /// as `path:` values do, instead of the name. `nopath:` still matches the
    /// name.
    pub match_path: bool,
    /// Wildcards without a separator test the name even with `match_path`.
    /// Explicit `path:` patterns keep testing the full path.
    pub name_wildcards: bool,
    /// Positive literal words without a separator match in typed order.
    /// Search functions, negated terms and alternatives keep their meanings.
    pub ordered_terms: bool,
    /// How text terms without a wildcard match: es `-ww` asks for whole
    /// words, as `ww:` values match, and the window passes its match mode.
    pub term_match: TermMatch,
    /// The absolute folder that a path term starting with `.\` or `..\`, and
    /// a relative `parent:` folder, are below. `None` refuses them with
    /// [`QueryError::RelativePath`]: the daemon parses a protocol 1 query
    /// without the client's working directory.
    pub working_dir: Option<&'a str>,
    /// What `dm:` values resolve against. `None` refuses them with
    /// [`QueryError::NoQueryStart`].
    pub query_start: Option<&'a QueryStart>,
}

/// Where a text term's text must lie in the name, or in the full path for a
/// path term. A wildcard already matches the whole name or path, so it
/// ignores this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TermMatch {
    /// Anywhere, as a substring.
    #[default]
    Anywhere,
    /// As a whole word, as `ww:` matches.
    WholeWords,
    /// Where a word starts, as `wordstart:` matches.
    WordStart,
    /// At the start, as `startwith:` matches.
    NameStart,
}

impl TermMatch {
    /// How a path term, and the value of a search function other than
    /// `ww:`, `wordstart:` and `startwith:`, match when this is the default
    /// for text terms. Word start and name start reach name terms only, so
    /// a typed path still finds what is below it; whole words reach every
    /// text term, as `-ww` always has.
    fn beyond_name_terms(self) -> TermMatch {
        match self {
            TermMatch::WholeWords => TermMatch::WholeWords,
            TermMatch::Anywhere | TermMatch::WordStart | TermMatch::NameStart => {
                TermMatch::Anywhere
            }
        }
    }
}

/// Parses a case-insensitive query string, without a working directory or a
/// query start, into a [`ParsedQuery`]; a `case:` directive in the query makes
/// it case-sensitive.
pub fn parse_query(input: &str) -> Result<ParsedQuery, QueryError> {
    parse_query_with(input, ParseOptions::default())
}

/// Parses a query string into a [`ParsedQuery`], read as `options` say. A
/// `case:` directive in the query overrides [`ParseOptions::match_case`], so
/// [`ParsedQuery::match_case`] is the one value that both matching and the
/// `-path` scope lookup follow.
///
/// Each group of terms joined by `|` is a clause, so the groups are AND-ed.
/// A later `ext:`, `size:`, `file:` or `folder:` that stands alone, without
/// `!` or `|`, replaces an earlier one.
pub fn parse_query_with(input: &str, options: ParseOptions) -> Result<ParsedQuery, QueryError> {
    let lexemes = lex(input)?;

    // The last `case:` directive wins.
    let match_case = lexemes
        .iter()
        .filter_map(|lexeme| match lexeme {
            Lexeme::Word(word) if word.quote_at.is_none() => case_directive(&word.text),
            _ => None,
        })
        .next_back()
        .unwrap_or(options.match_case);
    let mut lexemes = lexemes.into_iter().peekable();
    let groups = parse_groups(&mut lexemes, 0)?;
    let clauses = compile_groups(&groups, &options)?;
    Ok(ParsedQuery {
        clauses,
        match_case,
    })
}

fn compile_groups(groups: &[Vec<Item>], options: &ParseOptions) -> Result<Vec<Clause>, QueryError> {
    let mut clauses = Vec::new();
    let mut ordered = Vec::new();
    let mut kind = None;
    let mut size = None;
    let mut ext = None;
    for group in groups {
        if let [
            Item {
                negated: false,
                value: Atom::Word(word),
            },
        ] = group.as_slice()
        {
            match operand(word, options)? {
                Operand::Text(test) => {
                    if options.ordered_terms
                        && function_parts(word).is_none()
                        && !word.text.contains(['\\', '/'])
                        && matches!(
                            test,
                            Test::Name(_)
                                | Test::Path(_)
                                | Test::NameWordStart(_)
                                | Test::PathWordStart(_)
                                | Test::NameWholeWord(_)
                                | Test::PathWholeWord(_)
                                | Test::NameStart(_)
                                | Test::PathStart(_)
                        )
                    {
                        ordered.push(word.text.clone());
                    } else {
                        clauses.push(Clause::single(test));
                    }
                }
                Operand::Kind(new_kind, text) => {
                    kind = Some(new_kind);
                    clauses.extend(text.map(Clause::single));
                }
                Operand::Size(test) => size = Some(test),
                Operand::Extensions(extensions) => ext = Some(extensions),
                Operand::Nothing => {}
            }
            continue;
        }
        let alternatives = group
            .iter()
            .map(|item| {
                let clauses = match &item.value {
                    Atom::Word(word) => operand(word, options)?
                        .terms()
                        .into_iter()
                        .map(|term| Clause { terms: vec![term] })
                        .collect(),
                    Atom::Group(groups) => compile_groups(groups, options)?,
                };
                if clauses.is_empty() {
                    let text = match &item.value {
                        Atom::Word(word) => word.text.clone(),
                        Atom::Group(_) => "< >".to_owned(),
                    };
                    return Err(QueryError::Malformed(Malformed::OperatorOnNothing(text)));
                }
                if item.negated {
                    // Negating AND-ed clauses OR-s their negations.
                    any_of(
                        clauses
                            .into_iter()
                            .map(|clause| {
                                clause
                                    .terms
                                    .into_iter()
                                    .map(|term| Clause {
                                        terms: vec![Term {
                                            negated: !term.negated,
                                            test: term.test,
                                        }],
                                    })
                                    .collect()
                            })
                            .collect(),
                    )
                } else {
                    Ok(clauses)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        clauses.extend(any_of(alternatives)?);
    }

    if ordered.len() == 1 {
        clauses.extend(
            text_of(
                ordered.pop().expect("one ordered word remains"),
                options.match_path,
                Form::Text(options.term_match),
            )
            .map(Clause::single),
        );
    } else if !ordered.is_empty() {
        clauses.push(Clause::single(Test::Ordered {
            terms: ordered,
            term_match: options.term_match,
            match_path: options.match_path,
        }));
    }
    clauses.extend(kind.map(|kind| Clause::single(Test::Kind(kind))));
    clauses.extend(size.map(Clause::single));
    let ext_terms = ext.map(Extensions::terms).unwrap_or_default();
    clauses.extend(
        ext_terms
            .into_iter()
            .map(|term| Clause { terms: vec![term] }),
    );
    Ok(clauses)
}

pub(super) fn term_count(term: &Term) -> usize {
    match &term.test {
        Test::Extension(values) | Test::Ordered { terms: values, .. } => values.len().max(1),
        _ => 1,
    }
}

/// The clauses that pass when any of `alternatives` passes, where each
/// alternative is clauses that must all pass. By distribution, there is one
/// clause for each way to pick one clause of every alternative, and it holds
/// the picked terms: `a|file:b` becomes `(a or file:) and (a or b)`.
fn any_of(alternatives: Vec<Vec<Clause>>) -> Result<Vec<Clause>, QueryError> {
    // The clauses multiply, so a group past the term cap is refused before it
    // is built, which could otherwise take memory exponential in its length.
    // `CompiledQuery::new` refuses the whole query, which has at least these
    // terms, anyway.
    let count = alternatives
        .iter()
        .fold(1usize, |n, alternative| n.saturating_mul(alternative.len()));
    let terms = alternatives.iter().fold(0usize, |sum, alternative| {
        let picks = count / alternative.len().max(1);
        let terms: usize = alternative
            .iter()
            .flat_map(|clause| &clause.terms)
            .map(term_count)
            .sum();
        sum.saturating_add(terms.saturating_mul(picks))
    });
    if terms > super::engine::MAX_QUERY_TERMS {
        return Err(QueryError::TooManyTerms(terms));
    }

    let mut clauses = vec![Clause { terms: Vec::new() }];
    for alternative in &alternatives {
        clauses = clauses
            .iter()
            .flat_map(|picked| {
                alternative.iter().map(move |clause| Clause {
                    terms: [picked.terms.as_slice(), clause.terms.as_slice()].concat(),
                })
            })
            .collect();
    }
    Ok(clauses)
}

/// What one term of the query stands for.
enum Operand {
    /// A test that is a clause of its own: a name, path, date or attribute
    /// test.
    Text(Test),
    /// `file:` or `folder:`, and the text after it.
    Kind(Kind, Option<Test>),
    Size(Test),
    Extensions(Extensions),
    /// A `case:` directive or an empty `path:`, `nopath:` or `attrib:`, which
    /// test nothing.
    Nothing,
}

impl Operand {
    /// The terms that must all pass for the operand to match.
    fn terms(self) -> Vec<Term> {
        let positive = |test| Term {
            negated: false,
            test,
        };
        match self {
            Operand::Text(test) => vec![positive(test)],
            Operand::Kind(kind, text) => std::iter::once(Test::Kind(kind))
                .chain(text)
                .map(positive)
                .collect(),
            Operand::Size(test) => vec![positive(test)],
            Operand::Extensions(extensions) => extensions.terms(),
            Operand::Nothing => Vec::new(),
        }
    }
}

/// What an `ext:` term asks for.
enum Extensions {
    /// One of these, without the dot.
    OneOf(Vec<String>),
    /// No extension: `ext:` with none listed. As in es, where folders have
    /// no extension, only files match.
    Without,
}

impl Extensions {
    /// The terms that must all pass. `Without` is written with tests the
    /// protocol already carries, so a daemon of the same protocol version
    /// answers it: a file whose name `*.*` does not match has no dot.
    fn terms(self) -> Vec<Term> {
        match self {
            Extensions::OneOf(exts) => vec![Term {
                negated: false,
                test: Test::Extension(exts),
            }],
            Extensions::Without => vec![
                Term {
                    negated: false,
                    test: Test::Kind(Kind::File),
                },
                Term {
                    negated: true,
                    test: Test::NameWildcard("*.*".to_string()),
                },
            ],
        }
    }
}

/// What `word` stands for, or why it cannot be searched.
fn operand(word: &Word, options: &ParseOptions) -> Result<Operand, QueryError> {
    let text = word.text.as_str();
    if word.quote_at.is_none() && case_directive(text).is_some() {
        return Ok(Operand::Nothing);
    }
    if let Some((name, value)) = function_parts(word) {
        // A function value may hold `<` and `>`, as `size:<1mb` does, and its
        // quotes only delimit it, so `*` and `?` in it stay wildcards.
        let text_operand = |test: Option<Test>| test.map_or(Operand::Nothing, Operand::Text);
        // `ww:`, `wordstart:` and `startwith:` match their value as they
        // say, a path value included.
        let matched_as = |term_match| {
            let options = ParseOptions {
                term_match,
                ..*options
            };
            Ok(text_operand(text_test(value, false, &options, term_match)?))
        };
        // Every other function's value keeps its own meaning.
        let value_match = options.term_match.beyond_name_terms();
        let value_options = ParseOptions {
            term_match: value_match,
            ..*options
        };
        match function_named(name) {
            Some(Function::Ext) => return Ok(Operand::Extensions(extensions(value))),
            Some(Function::Size) => {
                return Ok(Operand::Size(if value.eq_ignore_ascii_case("unknown") {
                    Test::Unknown(Metadata::Size)
                } else {
                    Test::Size(parse_size(value)?)
                }));
            }
            Some(Function::Length(target)) => {
                let range = parse_range(value, |s| s.trim().parse().ok())
                    .ok_or_else(|| QueryError::Malformed(Malformed::Length(value.to_owned())))?;
                return Ok(Operand::Text(Test::Length { target, range }));
            }
            Some(Function::Child) => return Ok(Operand::Text(Test::Child(value.to_owned()))),
            Some(Function::Date(field)) => {
                if value.eq_ignore_ascii_case("unknown") {
                    return Ok(Operand::Text(Test::Unknown(match field {
                        DateField::Modified => Metadata::DateModified,
                        DateField::Created => Metadata::DateCreated,
                    })));
                }
                let start = options.query_start.ok_or(QueryError::NoQueryStart(field))?;
                let range = date_range(value, start, field)?;
                return Ok(Operand::Text(match field {
                    DateField::Modified => Test::DateModified(range),
                    DateField::Created => Test::DateCreated(range),
                }));
            }
            Some(Function::Path) => {
                let path = path_text(value, options.working_dir)?;
                let form = Form::of(has_wildcard(value), value_match);
                return Ok(text_operand(text_of(path, true, form)));
            }
            Some(Function::NoPath) => {
                let name = value.replace('/', r"\");
                let form = Form::of(has_wildcard(value), value_match);
                return Ok(text_operand(text_of(name, false, form)));
            }
            Some(Function::WholeWord) => return matched_as(TermMatch::WholeWords),
            Some(Function::WordStart) => return matched_as(TermMatch::WordStart),
            Some(Function::NameStart) => return matched_as(TermMatch::NameStart),
            Some(Function::WholeName) => return whole_name(value, options),
            Some(Function::Regex) => {
                return Ok(text_operand(regex_test(value, options.match_path)));
            }
            Some(Function::Parent) => {
                return Ok(Operand::Text(Test::Parent(parent_folder(
                    value,
                    options.working_dir,
                )?)));
            }
            Some(Function::Attributes) => {
                let attributes = Attributes::from_letters(value)
                    .map_err(|c| QueryError::Malformed(Malformed::AttributeLetter(c)))?;
                return Ok(if attributes.is_empty() {
                    Operand::Nothing
                } else {
                    Operand::Text(Test::Attributes(attributes))
                });
            }
            Some(Function::File) => {
                let test = text_test(value, false, &value_options, value_match)?;
                return Ok(Operand::Kind(Kind::File, test));
            }
            Some(Function::Folder) => {
                let test = text_test(value, false, &value_options, value_match)?;
                return Ok(Operand::Kind(Kind::Folder, test));
            }
            // A `case:` value that is not a directive is searched as text.
            Some(Function::Case) => {}
            None => return Err(QueryError::UnknownFunction(name.to_string())),
        }
    }
    // Only `""` is an empty word; it searches for the empty phrase, which
    // every name contains.
    let path_match = options.term_match.beyond_name_terms();
    let test = text_test(text, word.quote_at.is_some(), options, path_match)?;
    Ok(Operand::Text(test.unwrap_or(Test::Name(String::new()))))
}

fn whole_name(value: &str, options: &ParseOptions) -> Result<Operand, QueryError> {
    let test = if let Some((modifier, pattern)) = value
        .split_once(':')
        .filter(|(modifier, _)| modifier.len() > 1)
    {
        if function_named(modifier) != Some(Function::Regex) {
            return Err(QueryError::UnknownFunction(format!("wfn:{modifier}")));
        }
        if pattern.is_empty() {
            return Ok(Operand::Nothing);
        }
        let match_path = options.match_path || has_escaped_backslash(pattern);
        // Parse before anchoring so an extended-mode comment cannot swallow
        // the closing group and anchor.
        let expression = regex_syntax::ast::parse::Parser::new()
            .parse(pattern)
            .map_err(|error| QueryError::Malformed(Malformed::Regex(error.to_string())))?;
        regex_test(&format!(r"\A(?:{expression})\z"), match_path)
    } else {
        let full_path = options.match_path || value.contains(['\\', '/']);
        let text = if full_path {
            path_text(value, options.working_dir)?
        } else {
            value.to_owned()
        };
        text_of(text, full_path, Form::Wildcard)
    };
    Ok(test.map_or(Operand::Nothing, Operand::Text))
}

/// The name and the value of a function term. The text before the word's
/// first colon names a function when it is unquoted and could be the name of
/// one; a single letter is a drive, as in `C:\repo`.
fn function_parts(word: &Word) -> Option<(&str, &str)> {
    let colon = word.text.find(':')?;
    if word.quote_at.is_some_and(|at| at <= colon) {
        return None;
    }
    let name = &word.text[..colon];
    let could_be_function = name.len() > 1
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    could_be_function.then(|| (name, &word.text[colon + 1..]))
}

/// The search function `name` names, compared case-insensitively.
fn function_named(name: &str) -> Option<Function> {
    SEARCH_FUNCTIONS
        .iter()
        .find(|row| row.names.iter().any(|n| n.eq_ignore_ascii_case(name)))
        .map(|row| row.function)
}

/// The extensions of an `ext:` value: a list separated by `;`, each without
/// its leading dots.
fn extensions(value: &str) -> Extensions {
    let exts: Vec<String> = value
        .split(';')
        .map(|ext| {
            ext.strip_prefix("*.")
                .unwrap_or(ext)
                .trim_start_matches('.')
        })
        .filter(|ext| !ext.is_empty())
        .map(str::to_string)
        .collect();
    if exts.is_empty() {
        Extensions::Without
    } else {
        Extensions::OneOf(exts)
    }
}

/// The test of a text term, `None` for an empty one. A term with a separator
/// matches the full path, any other term the name, or with -p the full path
/// too, and a wildcard anchors either to the whole of what it matches. A
/// `quoted` term without a separator is a phrase, whose `*` and `?` are text;
/// with a separator it is a path term like any other, because a literal `*`
/// can never match a path. A term with a separator matches as `path_match`
/// says, every other term as [`ParseOptions::term_match`] says.
fn text_test(
    text: &str,
    quoted: bool,
    options: &ParseOptions,
    path_match: TermMatch,
) -> Result<Option<Test>, QueryError> {
    let is_path = text.contains(['\\', '/']);
    let wildcard = has_wildcard(text) && (is_path || !quoted);
    let text = if is_path {
        path_text(text, options.working_dir)?
    } else {
        text.to_string()
    };
    Ok(text_of(
        text,
        is_path || options.match_path && !(wildcard && options.name_wildcards),
        Form::of(
            wildcard,
            if is_path {
                path_match
            } else {
                options.term_match
            },
        ),
    ))
}

/// How a text term matches what it tests.
#[derive(Clone, Copy)]
enum Form {
    Text(TermMatch),
    /// A pattern of `*` and `?` that matches the whole of it.
    Wildcard,
}

impl Form {
    fn of(wildcard: bool, term_match: TermMatch) -> Self {
        if wildcard {
            Form::Wildcard
        } else {
            Form::Text(term_match)
        }
    }
}

/// A test of the full path, or else of the name, for `text`, matched as
/// `form` says; `None` when `text` is empty.
fn text_of(text: String, full_path: bool, form: Form) -> Option<Test> {
    if text.is_empty() {
        return None;
    }
    Some(match (form, full_path) {
        (Form::Text(TermMatch::Anywhere), true) => Test::Path(text),
        (Form::Text(TermMatch::Anywhere), false) => Test::Name(text),
        (Form::Text(TermMatch::WholeWords), true) => Test::PathWholeWord(text),
        (Form::Text(TermMatch::WholeWords), false) => Test::NameWholeWord(text),
        (Form::Text(TermMatch::WordStart), true) => Test::PathWordStart(text),
        (Form::Text(TermMatch::WordStart), false) => Test::NameWordStart(text),
        (Form::Text(TermMatch::NameStart), true) => Test::PathStart(text),
        (Form::Text(TermMatch::NameStart), false) => Test::NameStart(text),
        (Form::Wildcard, true) => Test::PathWildcard(text),
        (Form::Wildcard, false) => Test::NameWildcard(text),
    })
}

/// The test of a regular expression, from `regex:` or `-r`; `None` for an
/// empty one. As in es, it matches the name, and the full path with -p
/// (`match_path`) or when it holds an escaped backslash, `\\`, which can
/// only match a separator. The pattern is never rewritten: its `/` stays a
/// `/`, which no full path holds.
pub fn regex_test(pattern: &str, match_path: bool) -> Option<Test> {
    if pattern.is_empty() {
        return None;
    }
    let pattern = pattern.to_string();
    Some(if match_path || has_escaped_backslash(&pattern) {
        Test::PathRegex(pattern)
    } else {
        Test::NameRegex(pattern)
    })
}

/// Whether `pattern` holds `\\`. Each `\` escapes the character after it, so
/// the pair is read together and `\\d` is a backslash and a `d`.
fn has_escaped_backslash(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.next() == Some('\\') {
            return true;
        }
    }
    false
}

/// A path term's text as the daemon compares it: every `/` a `\`, and one
/// that starts with `.\` or `..\` joined to `working_dir`, keeping a trailing
/// separator, which limits a path term to what is below the folder.
fn path_text(text: &str, working_dir: Option<&str>) -> Result<String, QueryError> {
    let text = text.replace('/', r"\");
    if !(text.starts_with(r".\") || text.starts_with(r"..\")) {
        return Ok(text);
    }
    let working_dir = working_dir.ok_or_else(|| QueryError::RelativePath(text.clone()))?;
    let mut path = crate::winpath::absolute(&text, working_dir);
    if text.ends_with('\\') && !path.ends_with('\\') {
        path.push('\\');
    }
    Ok(path)
}

/// A `parent:` folder as the daemon compares it: absolute, joined to
/// `working_dir` when it is not, and every `/` a `\`.
fn parent_folder(value: &str, working_dir: Option<&str>) -> Result<String, QueryError> {
    let value = value.replace('/', r"\");
    if value.is_empty() {
        return Err(QueryError::Malformed(Malformed::NoParentFolder));
    }
    match working_dir {
        Some(working_dir) => Ok(crate::winpath::absolute(&value, working_dir)),
        None if crate::winpath::is_absolute(&value) => Ok(value),
        None => Err(QueryError::RelativePath(value)),
    }
}

fn has_wildcard(text: &str) -> bool {
    text.contains('*') || text.contains('?')
}

/// One term of the query as typed, without its quotes.
#[derive(Default)]
struct Word {
    text: String,
    /// Where in `text` the first quoted part starts; `None` when nothing is
    /// quoted. A function's name and colon lie before it.
    quote_at: Option<usize>,
}

enum Lexeme {
    Word(Word),
    /// `!`
    Not,
    /// `|`
    Or,
    Open,
    Close,
}

impl Word {
    /// Whether the word so far is a `regex:` term, whose `|` and `!` belong
    /// to the expression, as in es, where `|` is the regex alternation.
    fn is_regex(&self) -> bool {
        function_parts(self).is_some_and(|(name, value)| match function_named(name) {
            Some(Function::Regex) => true,
            Some(Function::WholeName) => value
                .split_once(':')
                .is_some_and(|(modifier, _)| function_named(modifier) == Some(Function::Regex)),
            _ => false,
        })
    }
}

/// Splits the query into words and operators. Unquoted whitespace and `|`
/// end a word, and an unquoted `!` must start one, except in a `regex:`
/// value; inside double quotes every character is text.
fn lex(input: &str) -> Result<Vec<Lexeme>, QueryError> {
    let mut lexemes = Vec::new();
    let mut word: Option<Word> = None;
    let mut in_quotes = false;
    for ch in input.chars() {
        if in_quotes {
            if ch == '"' {
                in_quotes = false;
            } else {
                word.get_or_insert_default().text.push(ch);
            }
            continue;
        }
        if matches!(ch, '|' | '!' | '<' | '>')
            && let Some(regex) = word.as_mut().filter(|word| {
                word.is_regex() && (!matches!(ch, '<' | '>') || word.quote_at.is_none())
            })
        {
            regex.text.push(ch);
            continue;
        }
        match ch {
            '"' => {
                let word = word.get_or_insert_default();
                if word.quote_at.is_none() {
                    word.quote_at = Some(word.text.len());
                }
                in_quotes = true;
            }
            '|' => {
                lexemes.extend(word.take().map(Lexeme::Word));
                lexemes.push(Lexeme::Or);
            }
            '!' if word.is_none() => lexemes.push(Lexeme::Not),
            '!' => {
                return Err(QueryError::Malformed(Malformed::MisplacedNot));
            }
            '<' | '>'
                if !word.as_ref().is_some_and(|word| {
                    function_parts(word).is_some_and(|(name, value)| {
                        value.is_empty()
                            && matches!(
                                function_named(name),
                                Some(Function::Size | Function::Date(_) | Function::Length(_))
                            )
                    })
                }) =>
            {
                lexemes.extend(word.take().map(Lexeme::Word));
                lexemes.push(if ch == '<' {
                    Lexeme::Open
                } else {
                    Lexeme::Close
                });
            }
            c if c.is_whitespace() => lexemes.extend(word.take().map(Lexeme::Word)),
            c => {
                let word = word.get_or_insert_default();
                word.text.push(c);
            }
        }
    }
    lexemes.extend(word.map(Lexeme::Word));
    Ok(lexemes)
}

/// A word after its `!` operators; an odd number of them negates it.
struct Item {
    negated: bool,
    value: Atom,
}

enum Atom {
    Word(Word),
    Group(Vec<Vec<Item>>),
}

const MAX_GROUP_DEPTH: usize = 32;

/// The lexemes as groups of items joined by `|`; the query AND-s the groups.
fn parse_groups(
    lexemes: &mut std::iter::Peekable<std::vec::IntoIter<Lexeme>>,
    depth: usize,
) -> Result<Vec<Vec<Item>>, QueryError> {
    if depth > MAX_GROUP_DEPTH {
        return Err(QueryError::GroupDepth(MAX_GROUP_DEPTH));
    }
    let or_needs_terms = || QueryError::Malformed(Malformed::OrWithoutTerms);
    let mut groups: Vec<Vec<Item>> = Vec::new();
    let mut nots = 0usize;
    // After a `|`, the next word joins the last group.
    let mut joined = false;
    let mut closed = false;
    while let Some(lexeme) = lexemes.next() {
        match lexeme {
            Lexeme::Close => {
                if depth == 0 {
                    return Err(QueryError::Malformed(Malformed::Grouping));
                }
                closed = true;
                break;
            }
            Lexeme::Not => nots += 1,
            Lexeme::Or => {
                if groups.is_empty() || joined || nots > 0 {
                    return Err(or_needs_terms());
                }
                joined = true;
            }
            lexeme @ (Lexeme::Word(_) | Lexeme::Open) => {
                let value = match lexeme {
                    Lexeme::Word(word) => Atom::Word(word),
                    Lexeme::Open => Atom::Group(parse_groups(lexemes, depth + 1)?),
                    _ => unreachable!("only words and opening groups reach this arm"),
                };
                let value = match value {
                    // Redundant AND groups must not restart ordered name
                    // matching or turn a later filter into a separate rule.
                    Atom::Group(inner)
                        if nots.is_multiple_of(2)
                            && !joined
                            && !matches!(lexemes.peek(), Some(Lexeme::Or)) =>
                    {
                        groups.extend(inner);
                        nots = 0;
                        continue;
                    }
                    value => value,
                };
                let item = Item {
                    negated: nots % 2 == 1,
                    value,
                };
                match groups.last_mut() {
                    Some(group) if joined => group.push(item),
                    _ => groups.push(vec![item]),
                }
                nots = 0;
                joined = false;
            }
        }
    }
    if depth > 0 && (!closed || groups.is_empty()) {
        return Err(QueryError::Malformed(Malformed::Grouping));
    }
    if joined {
        return Err(or_needs_terms());
    }
    if nots > 0 {
        return Err(QueryError::Malformed(Malformed::NotWithoutTerm));
    }
    Ok(groups)
}

/// Parses an es-style `size:` expression: a bare number, a comparison
/// (`>`, `>=`, `<`, `<=`, `=`), a range `a..b`, or a word of
/// [`SIZE_WORDS`]. Numbers accept `kb`/`mb`/`gb`/`tb` (1024-based)
/// suffixes.
fn parse_size(value: &str) -> Result<SizeRange, QueryError> {
    let v = value.trim();
    let malformed = |bad: &str| QueryError::Malformed(Malformed::Size(bad.to_string()));

    if let Some(word) = SIZE_WORDS.iter().find(|w| w.word.eq_ignore_ascii_case(v)) {
        return Ok(word.range);
    }

    parse_range(v, parse_bytes).ok_or_else(|| malformed(v))
}

fn parse_range(value: &str, number: impl Fn(&str) -> Option<u64>) -> Option<SizeRange> {
    let v = value.trim();

    // Range a..b (either side optional).
    if let Some((lo, hi)) = v.split_once("..") {
        let min = if lo.trim().is_empty() { 0 } else { number(lo)? };
        let max = if hi.trim().is_empty() {
            u64::MAX
        } else {
            number(hi)?
        };
        return Some(SizeRange { min, max });
    }

    // Comparison operators. Order matters: two-char ops before their one-char
    // prefixes so `>=` is not read as `>` plus a stray `=`.
    for op in [">=", "<=", ">", "<"] {
        if let Some(rest) = v.strip_prefix(op) {
            let n = number(rest)?;
            return Some(match (op, n) {
                (">=", n) => SizeRange {
                    min: n,
                    max: u64::MAX,
                },
                (">", u64::MAX) | ("<", 0) => SizeRange { min: 1, max: 0 },
                (">", n) => SizeRange {
                    min: n + 1,
                    max: u64::MAX,
                },
                ("<=", n) => SizeRange { min: 0, max: n },
                // The only remaining op.
                _ => SizeRange { min: 0, max: n - 1 },
            });
        }
    }

    // Bare number or `=N` → exact.
    let rest = v.strip_prefix('=').unwrap_or(v);
    let n = number(rest)?;
    Some(SizeRange { min: n, max: n })
}

/// Parses `"10mb"` / `"1024"` / `"2gb"` into a byte count (1024-based units).
fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let (num, mult) = if let Some(n) = lower.strip_suffix("tb") {
        (n, TB)
    } else if let Some(n) = lower.strip_suffix("gb") {
        (n, GB)
    } else if let Some(n) = lower.strip_suffix("mb") {
        (n, MB)
    } else if let Some(n) = lower.strip_suffix("kb") {
        (n, KB)
    } else if let Some(n) = lower.strip_suffix('b') {
        (n, 1)
    } else {
        (lower.as_str(), 1)
    };
    let num = num.trim();
    let value: u64 = num.parse().ok()?;
    value.checked_mul(mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> ParsedQuery {
        parse_query(s).unwrap()
    }

    /// The tests of a query whose clauses each hold one positive term.
    fn tests(s: &str) -> Vec<Test> {
        tests_with(s, ParseOptions::default())
    }

    /// [`tests`] of a query read as `options` say.
    fn tests_with(s: &str, options: ParseOptions) -> Vec<Test> {
        parse_query_with(s, options)
            .unwrap_or_else(|e| panic!("{s}: {e}"))
            .clauses
            .into_iter()
            .map(|clause| match <[Term; 1]>::try_from(clause.terms) {
                Ok(
                    [
                        Term {
                            negated: false,
                            test,
                        },
                    ],
                ) => test,
                Ok([term]) => panic!("{s}: a negated {term:?}"),
                Err(terms) => panic!("{s}: a clause of {terms:?}"),
            })
            .collect()
    }

    fn name(s: &str) -> Test {
        Test::Name(s.to_string())
    }

    #[test]
    fn space_separated_terms_are_anded_as_typed() {
        assert_eq!(tests("Report Invoice"), [name("Report"), name("Invoice")]);
        assert!(!q("Report Invoice").match_case);
    }

    #[test]
    fn quoted_phrase_keeps_spaces() {
        assert_eq!(tests("\"quarterly report\""), [name("quarterly report")]);
    }

    #[test]
    fn case_directive_variants_are_consumed_not_searched() {
        // `case:0` must be recognized (and disable case sensitivity), never
        // leak as a literal "case:0" search term.
        assert!(q("Report case:").match_case);
        assert!(!q("Report case:0").match_case);
        assert_eq!(tests("Report case:0"), [name("Report")]);
        assert!(q("Report case:1").match_case);
        // Last directive wins.
        assert!(!q("case:1 foo case:0").match_case);
        assert!(q("case:0 foo case:1").match_case);
    }

    #[test]
    fn path_term_detected_by_separator() {
        assert_eq!(
            tests("projects\\src"),
            [Test::Path("projects\\src".to_string())]
        );
    }

    /// A wildcard with a separator, and a `path:` value with a wildcard, match
    /// the full path; neither is a name wildcard, which no name with a
    /// separator could ever match.
    #[test]
    fn a_wildcard_with_a_separator_is_a_path_wildcard() {
        for query in [r"*\docs\*.md", "*/docs/*.md", "path:*.md"] {
            assert!(
                matches!(tests(query)[..], [Test::PathWildcard(_)]),
                "{query}"
            );
        }
        assert_eq!(
            tests(r"file:*\docs\*"),
            [
                Test::PathWildcard(r"*\docs\*".to_string()),
                Test::Kind(Kind::File)
            ]
        );
        assert_eq!(tests("path:docs"), [Test::Path("docs".to_string())]);
        assert_eq!(tests("*.rs"), [Test::NameWildcard("*.rs".to_string())]);
    }

    /// `-case` sets the default; a `case:` directive overrides it either way.
    #[test]
    fn the_case_switch_is_the_default_a_directive_overrides() {
        let case = |text, match_case| {
            let options = ParseOptions {
                match_case,
                ..ParseOptions::default()
            };
            parse_query_with(text, options).unwrap().match_case
        };
        assert!(case("Report", true));
        assert!(!case("Report case:0", true));
        assert!(case("Report case:", false));
        assert!(!case("Report", false));
    }

    #[test]
    fn ext_function_splits_and_drops_the_dot() {
        assert_eq!(
            tests("ext:JPG;png;.Gif"),
            [Test::Extension(vec![
                "JPG".to_string(),
                "png".to_string(),
                "Gif".to_string()
            ])]
        );
    }

    #[test]
    fn file_and_folder_kinds() {
        assert_eq!(tests("file:"), [Test::Kind(Kind::File)]);
        assert_eq!(tests("folder:"), [Test::Kind(Kind::Folder)]);
        assert_eq!(
            tests("file:report"),
            [name("report"), Test::Kind(Kind::File)]
        );
    }

    /// A later `ext:`, `size:` or kind replaces an earlier one; AND-ing them
    /// would make `ext:cs ext:txt` match nothing.
    #[test]
    fn a_later_filter_replaces_an_earlier_one() {
        assert_eq!(
            tests("ext:cs size:1 file: a ext:txt size:2 folder:"),
            [
                name("a"),
                Test::Kind(Kind::Folder),
                Test::Size(SizeRange { min: 2, max: 2 }),
                Test::Extension(vec!["txt".to_string()]),
            ]
        );
    }

    fn size(query: &str) -> SizeRange {
        match tests(query)[..] {
            [Test::Size(range)] => range,
            ref other => panic!("{query}: {other:?}"),
        }
    }

    #[test]
    fn size_operators_ranges_and_constants() {
        let range = |min, max| SizeRange { min, max };
        assert_eq!(size("size:>1mb"), range(MB + 1, u64::MAX));
        assert_eq!(size("size:>=1mb"), range(MB, u64::MAX));
        assert_eq!(size("size:1mb..10mb"), range(MB, 10 * MB));
        assert_eq!(size("size:1024"), range(1024, 1024));
        assert_eq!(size("size:..500"), range(0, 500));
    }

    /// As in Everything, a size constant starts above the previous one's
    /// maximum, so a file of exactly 10 KB is tiny and not small, and an
    /// empty file is not tiny (#20).
    #[test]
    fn a_size_on_a_boundary_matches_one_constant() {
        let range = |min, max| SizeRange { min, max };
        assert_eq!(size("size:tiny"), range(1, 10 * KB));
        assert_eq!(size("size:small"), range(10 * KB + 1, 100 * KB));
    }

    fn clauses(s: &str) -> Vec<Vec<Term>> {
        q(s).clauses
            .into_iter()
            .map(|clause| clause.terms)
            .collect()
    }

    fn term(negated: bool, test: Test) -> Term {
        Term { negated, test }
    }

    /// `|` binds tighter than the space, as in es, and `!` negates the term
    /// after it, a function included. `!file:report` excludes the files whose
    /// names contain report, so either test failing passes, and a negated
    /// filter leaves the filter that stands alone in place.
    #[test]
    fn operators_build_clauses_of_possibly_negated_terms() {
        assert_eq!(
            clauses("a|b !c"),
            [
                vec![term(false, name("a")), term(false, name("b"))],
                vec![term(true, name("c"))],
            ]
        );
        assert_eq!(clauses("a | b"), clauses("a|b"));
        assert_eq!(clauses("!!a"), clauses("a"));
        assert_eq!(
            clauses("!file:report"),
            [vec![
                term(true, Test::Kind(Kind::File)),
                term(true, name("report"))
            ]]
        );
        let x = || term(false, name("x"));
        assert_eq!(
            clauses("x|file:report"),
            [
                vec![x(), term(false, Test::Kind(Kind::File))],
                vec![x(), term(false, name("report"))],
            ]
        );
        let ext = |e: &str| Test::Extension(vec![e.to_string()]);
        assert_eq!(
            clauses("ext:cs !ext:txt"),
            [vec![term(true, ext("txt"))], vec![term(false, ext("cs"))]]
        );
        // Quoted, an operator is text.
        assert_eq!(tests("\"a|b\" \"!c\""), [name("a|b"), name("!c")]);
    }

    /// An operator without its term, or a `!` inside a term, has no meaning
    /// that a search could answer without guessing.
    #[test]
    fn an_operator_without_its_term_is_refused() {
        for query in [
            "a|", "|a", "a||b", "!", "a !", "!|a", "a!b", "!case:", "a|path:",
        ] {
            assert!(
                matches!(
                    parse_query(query),
                    Err(QueryError::Malformed(
                        Malformed::MisplacedNot
                            | Malformed::NotWithoutTerm
                            | Malformed::OrWithoutTerms
                            | Malformed::OperatorOnNothing(_)
                    ))
                ),
                "{query}"
            );
        }
    }

    #[test]
    fn groups_preserve_comparisons_and_reject_unbalanced_or_empty_expressions() {
        for query in ["a<b", "a>", "<>", "<a|>", "<a !>"] {
            assert!(parse_query(query).is_err(), "{query}");
        }
        assert_eq!(parse_query("<<a b>|c>"), parse_query("a|c b|c"));
        assert_eq!(parse_query("!<a b>"), parse_query("!a|!b"));
        assert_eq!(parse_query("!<a|b>"), parse_query("!a !b"));
        assert_eq!(parse_query("<size:<1kb>"), parse_query("size:<1kb"));
        assert_eq!(parse_query(r#"<regex:"^a$">"#), parse_query("regex:^a$"));
        assert_eq!(parse_query("<regex:^a$ >"), parse_query("regex:^a$"));
        assert_eq!(
            size("size:<1kb"),
            SizeRange {
                min: 0,
                max: KB - 1
            }
        );
        assert_eq!(tests("\"a<b\""), [name("a<b")]);
    }

    #[test]
    fn groups_bound_nesting_and_boolean_expansion() {
        let nested = format!(
            "{}a{}",
            "<".repeat(MAX_GROUP_DEPTH + 1),
            ">".repeat(MAX_GROUP_DEPTH + 1)
        );
        assert_eq!(
            parse_query(&nested),
            Err(QueryError::GroupDepth(MAX_GROUP_DEPTH))
        );
        let expanded = std::iter::repeat_n("<a b>", 9)
            .collect::<Vec<_>>()
            .join("|");
        assert!(matches!(
            parse_query(&expanded),
            Err(QueryError::TooManyTerms(_))
        ));
    }

    #[test]
    fn redundant_groups_preserve_ordered_words_and_last_wins_filters() {
        let options = ParseOptions {
            ordered_terms: true,
            term_match: TermMatch::NameStart,
            ..ParseOptions::default()
        };
        for text in ["red <blue green>", "<red> blue <green>"] {
            assert_eq!(
                parse_query_with(text, options),
                parse_query_with("red blue green", options)
            );
        }
        assert_eq!(parse_query("size:1 <size:2>"), parse_query("size:2"));
    }

    #[test]
    fn explicit_index_filters_share_existing_semantics() {
        assert_eq!(parse_query("ext:*.rs;*.txt"), parse_query("ext:rs;txt"));
        assert_eq!(
            parse_query(r"name:report full-path:C:\repo"),
            parse_query(r"nopath:report path:C:\repo")
        );
        assert_eq!(
            tests("size:unknown size:1"),
            [Test::Size(SizeRange { min: 1, max: 1 })]
        );
        assert_eq!(
            tests("size:1 size:unknown dm:unknown"),
            [
                Test::Unknown(Metadata::DateModified),
                Test::Unknown(Metadata::Size)
            ]
        );
        assert!(parse_query("namelen:1kb").is_err());
        assert!(parse_query("<case:0 report> case:1").unwrap().match_case);
        assert!(!parse_range("<0", |s| s.parse().ok()).unwrap().contains(0));
        assert!(
            !parse_range(">18446744073709551615", |s| s.parse().ok())
                .unwrap()
                .contains(u64::MAX)
        );
    }

    /// Bug 2: an unknown function is refused, because searched as text it
    /// would match no name and the empty answer would look valid. A drive
    /// letter is not a function.
    #[test]
    fn an_unknown_function_is_refused() {
        assert_eq!(
            parse_query("x !DA:today"),
            Err(QueryError::UnknownFunction("DA".to_string()))
        );
        assert_eq!(tests(r"C:\repo"), [Test::Path(r"C:\repo".to_string())]);
    }

    /// Quotes delimit a function's value, as in es (bug 10); quoted before
    /// its colon, a term is a phrase.
    #[test]
    fn quotes_delimit_a_function_value() {
        assert_eq!(
            tests(r#"path:"C:\Program Files""#),
            [Test::Path(r"C:\Program Files".to_string())]
        );
        assert_eq!(tests(r#""path:x""#), [name("path:x")]);
    }

    fn path(s: &str) -> Test {
        Test::Path(s.to_string())
    }

    fn path_wildcard(s: &str) -> Test {
        Test::PathWildcard(s.to_string())
    }

    fn parent(s: &str) -> Test {
        Test::Parent(s.to_string())
    }

    /// Full paths hold `\`, so a `/` in a path term, a path wildcard or a
    /// `path:` or `parent:` value would never match (bug 9).
    #[test]
    fn a_slash_in_a_path_is_a_backslash() {
        assert_eq!(
            tests("docs/agents C:/repo/*.cs path:a/b parent:C:/repo"),
            [
                path(r"docs\agents"),
                path_wildcard(r"C:\repo\*.cs"),
                path(r"a\b"),
                parent(r"C:\repo"),
            ]
        );
    }

    /// A path term that starts with `.\` or `..\`, and a `parent:` folder
    /// that is not absolute, are below the working directory; a path term
    /// keeps its trailing separator, which limits it to what is below the
    /// folder. Without a working directory, as on the daemon's text path,
    /// each is refused, never searched as text that matches nothing.
    #[test]
    fn a_relative_path_is_below_the_working_directory() {
        let in_src = ParseOptions {
            working_dir: Some(r"C:\repo\src"),
            ..ParseOptions::default()
        };
        assert_eq!(
            tests_with(r"./ ..\docs\*.md docs\.\x parent:.. parent:sub", in_src),
            [
                path(r"C:\repo\src\"),
                path_wildcard(r"C:\repo\docs\*.md"),
                path(r"docs\.\x"),
                parent(r"C:\repo"),
                parent(r"C:\repo\src\sub"),
            ]
        );
        assert_eq!(
            parse_query(r"x .\src"),
            Err(QueryError::RelativePath(r".\src".to_string()))
        );
        assert_eq!(
            parse_query("parent:src"),
            Err(QueryError::RelativePath("src".to_string()))
        );
        assert!(matches!(
            parse_query("parent:"),
            Err(QueryError::Malformed(Malformed::NoParentFolder))
        ));
    }

    /// A quoted term with a separator is a path term, wildcards included,
    /// because a literal `*` can never match a path; without one it is a
    /// phrase of the name, whose `*` is text (bug 10).
    #[test]
    fn a_quoted_term_with_a_separator_is_a_path_term() {
        assert_eq!(
            tests(r#""C:\Program Files (x86)\Microsoft" "C:\Program Files\*.cs" "a*b""#),
            [
                path(r"C:\Program Files (x86)\Microsoft"),
                path_wildcard(r"C:\Program Files\*.cs"),
                name("a*b"),
            ]
        );
    }

    /// `-p` makes every text term a path term, as es's Match Path does, and
    /// `nopath:` still matches the name.
    #[test]
    fn match_path_makes_text_terms_path_terms_except_nopath() {
        let match_path = ParseOptions {
            match_path: true,
            ..ParseOptions::default()
        };
        assert_eq!(
            tests_with(
                r#"report *.cs "two words" nopath:readme nopath:*.md"#,
                match_path
            ),
            [
                path("report"),
                path_wildcard("*.cs"),
                path("two words"),
                name("readme"),
                Test::NameWildcard("*.md".to_string()),
            ]
        );
    }

    /// `ww:` matches one term as whole words and `-ww` every text term; a
    /// wildcard, which already matches a whole name or path, stays a
    /// wildcard.
    #[test]
    fn whole_word_terms_keep_their_target() {
        let whole = |s: &str| Test::NameWholeWord(s.to_string());
        let whole_path = |s: &str| Test::PathWholeWord(s.to_string());
        assert_eq!(
            tests(r"ww:report ww:docs/agents ww:*.cs report"),
            [
                whole("report"),
                whole_path(r"docs\agents"),
                Test::NameWildcard("*.cs".to_string()),
                name("report"),
            ]
        );
        let whole_word = ParseOptions {
            term_match: TermMatch::WholeWords,
            ..ParseOptions::default()
        };
        assert_eq!(
            tests_with(r#"report "two words" path:docs nopath:x *.cs"#, whole_word),
            [
                whole("report"),
                whole("two words"),
                whole_path("docs"),
                whole("x"),
                Test::NameWildcard("*.cs".to_string()),
            ]
        );
    }

    /// `wordstart:` and `startwith:` match one term as `ww:` does, a value
    /// with a separator testing the full path. The window's match mode,
    /// passed as [`ParseOptions::term_match`], reaches name terms only: a
    /// path term, a `path:` or `nopath:` value and a `file:` value keep
    /// their plain meaning, so a typed folder still finds what is below it,
    /// and with -p the mode tests the full path.
    #[test]
    fn word_start_and_name_start_terms_keep_their_target() {
        let word_start = |s: &str| Test::NameWordStart(s.to_string());
        let start = |s: &str| Test::NameStart(s.to_string());
        assert_eq!(
            tests(r"wordstart:para startwith:read wordstart:docs/ag startwith:C:\repo *.cs"),
            [
                word_start("para"),
                start("read"),
                Test::PathWordStart(r"docs\ag".to_string()),
                Test::PathStart(r"C:\repo".to_string()),
                Test::NameWildcard("*.cs".to_string()),
            ]
        );
        let options = |term_match, match_path| ParseOptions {
            term_match,
            match_path,
            ..ParseOptions::default()
        };
        assert_eq!(
            tests_with(
                r"para docs\ag path:docs nopath:x ww:y file:z",
                options(TermMatch::WordStart, false)
            ),
            [
                word_start("para"),
                path(r"docs\ag"),
                path("docs"),
                name("x"),
                Test::NameWholeWord("y".to_string()),
                name("z"),
                Test::Kind(Kind::File),
            ]
        );
        assert_eq!(
            tests_with(r"para docs\ag", options(TermMatch::NameStart, true)),
            [Test::PathStart("para".to_string()), path(r"docs\ag")]
        );
    }

    #[test]
    fn ordered_words_leave_functions_and_explicit_paths_independent() {
        assert_eq!(
            tests_with(
                r"ww:para wordstart:50 startwith:par C:\docs para 50",
                ParseOptions {
                    match_path: true,
                    ordered_terms: true,
                    term_match: TermMatch::WordStart,
                    ..ParseOptions::default()
                },
            ),
            [
                Test::PathWholeWord("para".into()),
                Test::PathWordStart("50".into()),
                Test::PathStart("par".into()),
                path(r"C:\docs"),
                Test::Ordered {
                    terms: vec!["para".into(), "50".into()],
                    term_match: TermMatch::WordStart,
                    match_path: true,
                },
            ]
        );
    }

    /// A regular expression keeps its text: its `|` and `!` belong to it, as
    /// es's parser gives them to it, and its `/` is not a separator. It
    /// matches the name, or the full path with -p or when it holds `\\`.
    #[test]
    fn a_regex_keeps_its_text_and_matches_the_path_only_with_a_backslash() {
        let regex = |s: &str| Test::NameRegex(s.to_string());
        let path_regex = |s: &str| Test::PathRegex(s.to_string());
        assert_eq!(
            tests(r#"regex:^a|b!$ regex:a/b regex:\d+ regex:C:\\repo\\ regex:[\\/]x REGEX:"a b""#),
            [
                regex("^a|b!$"),
                regex("a/b"),
                regex(r"\d+"),
                path_regex(r"C:\\repo\\"),
                path_regex(r"[\\/]x"),
                regex("a b"),
            ]
        );
        assert_eq!(
            clauses("x|regex:a|b !regex:c|d"),
            [
                vec![term(false, name("x")), term(false, regex("a|b"))],
                vec![term(true, regex("c|d"))],
            ]
        );
        let match_path = ParseOptions {
            match_path: true,
            ..ParseOptions::default()
        };
        assert_eq!(tests_with(r"regex:^C:", match_path), [path_regex("^C:")]);
    }

    #[test]
    fn malformed_size_is_reported() {
        assert!(matches!(
            parse_query("size:abc"),
            Err(QueryError::Malformed(Malformed::Size(_)))
        ));
    }

    #[test]
    fn empty_query_has_no_text_constraints() {
        assert!(q("").is_text_empty());
        assert!(q("   ").is_text_empty());
    }

    mod arbitrary {
        use super::*;
        use proptest::prelude::*;

        /// Query text built from the parser's own syntax and arbitrary
        /// characters, so that functions, ranges and wildcards are reached.
        fn arb_query() -> impl Strategy<Value = String> {
            let fragment = prop_oneof![
                prop::sample::select(vec![
                    "size:",
                    "ext:",
                    "path:",
                    "parent:",
                    "nopath:",
                    "ww:",
                    "wordstart:",
                    "startwith:",
                    "wfn:",
                    "wfn:regex:",
                    "regex:",
                    "file:",
                    "folder:",
                    "case:",
                    "case:0",
                    "dm:",
                    "child:",
                    "unknown",
                    "namelen:",
                    "..",
                    ">=",
                    "<",
                    ">",
                    "=",
                    "*",
                    "?",
                    ";",
                    ".",
                    "\"",
                    " ",
                    "\\",
                    "/",
                    "!",
                    "|",
                    "kb",
                    "gb",
                    "tiny",
                    "today",
                    "thisweek",
                    "lastmonth",
                    "nextyear",
                    "last999999999days",
                    "days",
                    "-",
                ])
                .prop_map(str::to_string),
                "[0-9]{1,24}",
                "\\PC{0,4}",
            ];
            prop::collection::vec(fragment, 0..12).prop_map(|parts| parts.concat())
        }

        proptest! {
            /// The daemon parses the query text of a protocol 1 request, so
            /// any text can arrive, and a panic would fail the query instead
            /// of reporting it. The query start is the earliest and the
            /// latest one jiff holds, so date arithmetic leaves its range.
            #[test]
            fn arbitrary_text_errors_instead_of_panicking(
                input in arb_query(),
                match_case in any::<bool>(),
                latest in any::<bool>(),
            ) {
                let start = QueryStart {
                    week_start: jiff::civil::Weekday::Monday,
                    now: if latest { jiff::Timestamp::MAX } else { jiff::Timestamp::MIN },
                    time_zone: jiff::tz::TimeZone::UTC,
                };
                let options = ParseOptions {
                    match_case,
                    query_start: Some(&start),
                    ..ParseOptions::default()
                };
                let _ = parse_query_with(&input, options);
            }
        }
    }
}
