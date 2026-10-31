//! The quick help and the full reference. The quick help is the examples
//! marked quick, so it fits one console screen. The full reference is
//! generated from the vocabulary tables: [`SWITCHES`], [`SORT_KEYS`],
//! [`SEARCH_FUNCTIONS`], the attribute [`LETTERS`] and [`ExitCode::ALL`], and
//! from this module's examples, search terms and notes, so a row added to a
//! table is a line of the full reference. [`search_syntax`] renders what of
//! it holds for the window's search box.

use super::ExitCode;
use super::argv::{Group, HelpScreen, SORT_KEYS, SWITCHES, Spelling, match_native, match_switch};
use crate::attributes::LETTERS;
use crate::output::style::{ColorToken, paint};
use crate::query::parse::SEARCH_FUNCTIONS;
use std::sync::OnceLock;
use std::time::SystemTime;

/// When the running program was linked, which its `main` records.
static LINKED: OnceLock<SystemTime> = OnceLock::new();

/// Records when the running program was linked, for [`version`]. Each
/// program's `main` calls it first, because this crate is built once for
/// every program and so cannot know when each one was linked.
pub fn set_linked(linked: SystemTime) {
    let _ = LINKED.set(linked);
}

/// gozler's version, as `-version` prints it. A release build prints the
/// Cargo version. Every other build appends the local date and time its
/// program was linked, as `0.2.1 (2026-10-18 11:51)`, so a build from a
/// working tree cannot pass for the release, and two builds of one version
/// tell apart.
pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        let linked = LINKED
            .get()
            .filter(|_| !cfg!(gozler_release))
            .and_then(|&linked| jiff::Timestamp::try_from(linked).ok());
        match linked {
            Some(linked) => format!(
                "{version} ({})",
                linked
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%Y-%m-%d %H:%M")
            ),
            None => version.to_owned(),
        }
    })
}

/// The help screen a [`Command::Help`](super::Command::Help) asks for, with
/// ANSI colors when `color` is set. The full reference uses `columns`, or
/// 80 columns without a console, leaving one column free to avoid auto-wrap.
pub fn help(screen: HelpScreen, color: bool, columns: Option<usize>) -> String {
    let line_width = match screen {
        HelpScreen::Quick => DEFAULT_LINE_WIDTH,
        HelpScreen::Full => {
            columns.map_or(DEFAULT_LINE_WIDTH, |width| width.saturating_sub(1).max(1))
        }
    };
    let mut out = String::new();
    let title = format!("gozler {}", version());
    if screen == HelpScreen::Full {
        wrap_usage(
            &mut out,
            0,
            &title,
            Usage::Styled(ColorToken::Strong),
            color,
            line_width,
        );
        wrap(
            &mut out,
            0,
            "Instant file search for Windows NTFS and ReFS",
            0,
            color,
            line_width,
        );
    } else {
        paint(&mut out, ColorToken::Strong, &title, color);
        out.push_str(" - Instant file search for Windows NTFS and ReFS\n");
    }
    out.push_str("Usage: ");
    wrap_usage(
        &mut out,
        7,
        "gozler [options] [search terms]",
        Usage::Command,
        color,
        line_width,
    );
    match screen {
        HelpScreen::Quick => {
            out.push('\n');
            let examples: Vec<Entry> = EXAMPLES
                .iter()
                .filter(|example| example.quick)
                .map(|example| command(example.usage.to_string(), example.text))
                .collect();
            render(&mut out, &examples, color, line_width);
            out.push('\n');
            wrap(&mut out, 0, QUICK_QUOTING, 0, color, line_width);
            quick_exit_codes(&mut out, color);
            out.push_str("For every option, search function and exit code, run ");
            syntax(&mut out, "gozler --help", Usage::Command, color);
            out.push_str(".\n");
        }
        HelpScreen::Full => {
            out.push_str("\n  ");
            wrap(&mut out, 2, INTRO, 2, color, line_width);
            for section in full_sections() {
                render_section(&mut out, &section, color, line_width);
            }
            render_notes(&mut out, Reader::CommandLine, color, line_width);
            render_section(&mut out, &exit_codes(&ExitCode::ALL), color, line_width);
        }
    }
    // A blank line before the shell's next prompt.
    out.push('\n');
    out
}

/// When to quote a term for the shell, which the examples show.
const QUICK_QUOTING: &str = "Put quotes around a term that contains `|`, `<`, `>` or `;`.";

/// Writes the exit codes a script checks after a search that ran, in short;
/// the full reference gives each one's whole meaning.
fn quick_exit_codes(out: &mut String, color: bool) {
    let code = |out: &mut String, code: ExitCode| {
        paint(out, code.token(), &code.code().to_string(), color);
    };
    out.push_str("gozler exits ");
    code(out, ExitCode::Complete);
    out.push_str(" when done, ");
    code(out, ExitCode::Incomplete);
    out.push_str(" if results may be missing, ");
    code(out, ExitCode::Truncated);
    out.push_str(" if some are hidden.\n");
}

const INTRO: &str = "Find files and folders by name, path, size, date or attribute. \
     Search terms combine with AND. By default gozler prints one full path per line, \
     at most 20 unless `-n` says otherwise, and writes warnings and errors to stderr.";

/// One example: a complete command line and what it finds.
struct Example {
    usage: &'static str,
    text: &'static str,
    /// Whether the quick help lists it.
    quick: bool,
    /// The same search as typed in the window's search box, for an example
    /// that uses no option; [`search_syntax`] lists it.
    search_box: Option<&'static str>,
}

const fn quick(usage: &'static str, text: &'static str) -> Example {
    Example {
        usage,
        text,
        quick: true,
        search_box: None,
    }
}

const fn full_only(usage: &'static str, text: &'static str) -> Example {
    Example {
        usage,
        text,
        quick: false,
        search_box: None,
    }
}

impl Example {
    /// The example, which the window also lists as `query`.
    const fn typed(self, query: &'static str) -> Example {
        Example {
            search_box: Some(query),
            ..self
        }
    }
}

/// Complete command lines that work as typed in cmd and in PowerShell.
const EXAMPLES: &[Example] = &[
    quick("gozler report", "Find names that contain report").typed("report"),
    quick(
        "gozler readme*.txt",
        "Find .txt files that start with readme",
    )
    .typed("readme*.txt"),
    quick(
        "gozler report !draft",
        "Find names with report but not draft",
    )
    .typed("report !draft"),
    quick(r#"gozler "ext:cs;txt""#, "Find .cs and .txt files").typed("ext:cs;txt"),
    quick(
        r"gozler C:\repo\ *.cs",
        r"Find .cs files anywhere in C:\repo",
    )
    .typed(r"C:\repo\ *.cs"),
    quick(
        "gozler *.log dm:7days",
        "Find .log files changed in the last 7 days",
    )
    .typed("*.log dm:7days"),
    quick(
        r#"gozler "size:>1gb""#,
        "Find files and folders larger than 1 GB",
    )
    .typed("size:>1gb"),
    quick(r#"gozler "*.jpg|*.png""#, "Find .jpg and .png files").typed("*.jpg|*.png"),
    quick(
        "gozler *.iso -sort size -n 10",
        "Show the 10 largest .iso files",
    ),
    quick(
        "gozler -n all report",
        "Show every match, not just the first 20",
    ),
    quick("gozler -i README", "Find README but not readme"),
    quick(
        "gozler -export-csv out.csv ext:cs",
        "Save the .cs files to out.csv",
    ),
    quick("gozler --status", "Check whether the index is ready"),
    full_only(
        r"gozler -path C:\repo ext:cs",
        r"Find .cs files anywhere in C:\repo, with -path",
    ),
    full_only(
        "gozler /a-d report",
        "Find files, not folders, that contain report",
    ),
    full_only(
        r"gozler -parent C:\repo -n all",
        r"List everything directly in C:\repo",
    ),
    full_only(
        r#"gozler -r "^test[0-9]+\.cs$" -path C:\repo"#,
        "Find names that match a regular expression",
    ),
    full_only(
        r"gozler /ah -attributes -path C:\repo",
        r"Find hidden items anywhere in C:\repo, with their attributes",
    ),
    full_only(
        "gozler -export-csv out.csv -size -dm ext:cs",
        "Save every .cs file to out.csv, with sizes and dates",
    ),
    full_only("gozler -get-result-count ext:cs", "Count the .cs files"),
    full_only(
        r"gozler -get-folder-size C:\repo",
        r"Show the total size of everything in C:\repo",
    ),
    full_only(
        "gozler --json -n 0 report",
        "Count matches as JSON without fetching rows",
    ),
    full_only(
        "gozler -timeout 5000 -no-result-error report",
        "Wait up to 5 seconds for indexing, and exit 9 if nothing matches",
    ),
];

/// Search terms as gozler receives them, after the shell removes its quotes.
const SEARCH_TERMS: &[(&str, &str)] = &[
    ("report", "Name contains report"),
    ("*.cs", "Whole name matches a wildcard, `*` or `?`"),
    ("\"two words\"", "Name contains the phrase"),
    ("!test", "Name does not contain test"),
    ("a|b", "Either term; `|` binds tighter than a space"),
    (
        "<a b>|<c d>",
        "Both a and b, or both c and d; `!<a b>` negates a group",
    ),
    (r"C:\repo\", "Everything below that folder"),
    ("ext:cs;txt", "Extension is cs or txt"),
    ("dm:7days", "Modified in the last 7 days"),
    ("size:>1mb", "Larger than 1 MB"),
    (
        r"docs\agents",
        r"Full path contains `docs\agents`; `/` works as `\` too",
    ),
    (r"C:\repo\*.cs", ".cs files below that folder"),
    (
        r".\src\",
        r"Everything below src in the current folder; `..\` works too",
    ),
    (
        r#""C:\Program Files (x86)\Microsoft""#,
        "A quoted path keeps its spaces",
    ),
    (
        r"*\node_modules\*",
        r"A wildcard with \ matches the whole full path",
    ),
    (
        r#"path:"C:\Program Files""#,
        "Quotes keep the spaces in a function's value",
    ),
];

/// How to type in cmd and PowerShell the search terms that a shell would
/// otherwise change.
const QUOTING: &[(&str, &str)] = &[
    (
        r#"gozler "a|b""#,
        "Quote a term with |, < or > in cmd and PowerShell",
    ),
    (
        r#"gozler \"two words\""#,
        "Search for a phrase in cmd or Windows PowerShell 5.1",
    ),
    (
        r#"gozler '"two words"'"#,
        "Search for a phrase in PowerShell 7.3 or later",
    ),
];

/// Which help a note is for. A note whose text holds for both is written
/// once, so the two helps cannot drift apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reader {
    /// The full reference.
    CommandLine,
    /// The window's [`search_syntax`].
    SearchBox,
    Both,
}

const NOTES: &[(Reader, &str)] = &[
    (
        Reader::CommandLine,
        "Single-dash options also accept `/`, and dashes inside their names are \
         optional. Double-dash options must be written exactly as listed. \
         A recognized `/` option is an option, as `/p` is `-p`, so write a relative \
         path without a leading `/`: `docs/agents`.",
    ),
    (
        Reader::CommandLine,
        "gozler joins its arguments with spaces and splits them into terms again, \
         so the single argument `\"ext:cs report\"` is two terms. A bare `--` is \
         search text; it does not end option parsing.",
    ),
    (
        Reader::CommandLine,
        "`/ad` finds folders, `/a-d` files. Prefix each unwanted attribute letter \
         with `-`: `/a-h-s` excludes hidden and system; `/a-hs` excludes hidden \
         and requires system.",
    ),
    (
        Reader::CommandLine,
        "Without `-n`, gozler shows 20 results. When the limit hides matches, gozler \
         writes the shown and total counts to stderr and exits 10. `-export-*` writes \
         every match unless `-n` is given, and `-get-result-count` and the size totals \
         count every match.",
    ),
    (
        Reader::CommandLine,
        "When a volume in scope is not ready (indexing, rescanning, offline or \
         failed), gozler prints the results it has, warns on stderr and exits 2. With `-path`, \
         scope includes volumes reached through folder mounts and links. `--status` \
         exits 2 when any volume is not ready or its sizes and dates could not be read. \
         With both `--status` and `--server-version`, the last report wins.",
    ),
    (
        Reader::CommandLine,
        "When several exit codes apply, the most serious wins: a failure, then 2, \
         then 9 or 10, then 0. So no matches while a volume in scope is indexing \
         exits 2, because the matches may still appear.",
    ),
    (
        Reader::SearchBox,
        "The Match list decides how a plain name term matches. The descriptions \
         here say \"contains\", as Anywhere matches; the default, Start of words, \
         matches only where a word of the name starts.",
    ),
    (
        Reader::Both,
        "Sizes use 1,024-based units: `size:1kb` and `size:=1kb` both mean exactly \
         1,024 bytes. Use comparisons or a range such as `size:1kb..2kb` for other sizes.",
    ),
    (
        Reader::Both,
        "Missing sizes and dates never match numeric or date comparisons, even \
         negated. Use `size:unknown`, `dm:unknown` or `dc:unknown` to find them. \
         A folder with only a lower-bound size also matches `size:unknown`.",
    ),
    (
        Reader::Both,
        "Use `<` and `>` to group expressions, and `!` to negate a group. \
         Put a space after an unquoted regular expression before a closing `>`, \
         or quote the expression, so `>` is not read as part of the pattern. \
         Nesting and expanded query terms are bounded.",
    ),
    (
        Reader::CommandLine,
        "Files whose size or date Windows did not report add nothing to the size \
         totals. When reading sizes and dates failed on a volume in scope, a search \
         that uses them warns and exits 2. When the read succeeded, a file it could \
         not read is rare and is left out without a warning.",
    ),
    (
        Reader::CommandLine,
        "A folder's size is the total of the known sizes of the files below it, \
         each name of a hard-linked file counted; `size:`, `-size` and `-sort size` \
         use it. `-get-total-size` and `-get-folder-size` add only files, so no file \
         is counted twice.",
    ),
    (
        Reader::CommandLine,
        "A search shows the files in folders you can open, and folder sizes only if \
         you see every file. The users and groups that `gozlerd see-every-file` \
         lists see every file.",
    ),
    (
        Reader::Both,
        "Dates use local time. `today`, `yesterday` and `tomorrow` are calendar days. \
         `this`, `last` and `next` before `week`, `month` or `year` select a calendar \
         period; weeks start on the first day set in Windows. `current` means `this`, \
         `past` and `prev` mean `last`, and `coming` means `next`. \
         `7days` and `last7days` are the 7 times 24 hours before the search starts; \
         `1weeks` is the same duration. `10minutes` and `2hours` count back from \
         the search start. `dm:<30days` finds files older than 30 days.",
    ),
    (
        Reader::CommandLine,
        r"`regex:` and `-r` use Rust regular expressions, without look-around or backreferences. A pattern matches the name; with `-p`, or when it contains `\\`, it matches the full path, whose separators are `\`, so write `\\` or `[\\/]` for a separator. `|` and `!` inside a pattern belong to the expression.",
    ),
    (
        Reader::SearchBox,
        r"With Regular expression in the Match list, the whole search box is one pattern. `regex:` and that pattern use Rust regular expressions, without look-around or backreferences. A pattern matches the name; with Match path, or when it contains `\\`, it matches the full path, whose separators are `\`, so write `\\` or `[\\/]` for a separator.",
    ),
    (
        Reader::CommandLine,
        "Dates print as ISO 8601 in local time, whatever the locale. Plain output \
         groups the digits of a size with commas; CSV and TSV print plain bytes. \
         Attributes print as letters, such as HA, and in CSV and TSV as their \
         FILE_ATTRIBUTE_* number, such as 34.",
    ),
    (
        Reader::CommandLine,
        "JSON search results include readable local `mtime` and `created` dates \
         beside `mtime_ft` and `created_ft`. Unknown dates are omitted. `total` \
         counts every match; `more` is true when matching rows were left out. \
         Check `volumes_incomplete`, `metadata_pending` and the exit code before \
         treating an answer as complete. Warnings and errors go to stderr, not \
         into JSON. See https://github.com/SynoSoftware/gozler/blob/main/docs/JSON.md \
         for fields and script examples.",
    ),
    (
        Reader::CommandLine,
        "An unknown search function, such as `unknown:`, or an unbalanced or empty \
         group is invalid input and exits 4.",
    ),
];

/// The full reference's option sections, in the order it lists them.
const GROUPS: [(Group, &str); 5] = [
    (Group::Search, "Search options:"),
    (Group::LimitAndSort, "Limit and sort:"),
    (Group::Output, "Output:"),
    (Group::Totals, "Totals:"),
    (Group::Other, "Other options:"),
];

/// A titled list of help entries, generated from one table.
struct Section {
    title: &'static str,
    entries: Vec<Entry>,
}

/// One help line: what to type, and what it does.
struct Entry {
    usage: String,
    text: &'static str,
    usage_kind: Usage,
}

#[derive(Clone, Copy)]
enum Usage {
    Expression,
    Command,
    Options,
    Styled(ColorToken),
}

/// The examples that use no option, the search terms, the search functions,
/// the attribute letters and the notes for a search box, without color: what
/// a search box accepts, with nothing about options, shell quoting or exit
/// codes. Its rows are the full reference's, except the notes about the
/// window's own match controls.
pub fn search_syntax() -> String {
    let examples = EXAMPLES
        .iter()
        .filter_map(|example| {
            example
                .search_box
                .map(|query| entry(query.to_string(), example.text))
        })
        .collect();
    let mut out = String::new();
    for section in [
        section("Examples:", examples),
        search_terms("Search terms:"),
        search_functions(),
        attribute_letters("Attribute letters:"),
    ] {
        render_section(&mut out, &section, false, DEFAULT_LINE_WIDTH);
    }
    render_notes(&mut out, Reader::SearchBox, false, DEFAULT_LINE_WIDTH);
    // Each section starts with a blank line, which the first one does not
    // need.
    out.remove(0);
    out
}

/// The full reference's sections above its notes and exit codes.
fn full_sections() -> Vec<Section> {
    let examples = EXAMPLES
        .iter()
        .map(|example| command(example.usage.to_string(), example.text))
        .collect();
    let mut sections = vec![
        section("Examples:", examples),
        search_terms(
            "Search syntax (what gozler receives; quote it for the shell as shown below):",
        ),
        section(
            "Quoting in the shell:",
            QUOTING
                .iter()
                .map(|&(usage, text)| command(usage.to_string(), text))
                .collect(),
        ),
        search_functions(),
        attribute_letters("Attribute letters (for attrib: and /a):"),
    ];
    for (group, title) in GROUPS {
        let options = SWITCHES
            .iter()
            .filter(|switch| switch.group == group)
            .map(|switch| Entry {
                usage_kind: Usage::Options,
                ..entry(switch_usage(&switch.spelling), switch.help)
            })
            .collect();
        sections.push(section(title, options));
        if group == Group::LimitAndSort {
            sections.push(section(
                "Sort keys (for -sort KEY and -sort-KEY):",
                SORT_KEYS
                    .iter()
                    .map(|row| entry(row.names.join(", "), row.help))
                    .collect(),
            ));
        }
    }
    sections
}

fn search_terms(title: &'static str) -> Section {
    section(title, pairs(SEARCH_TERMS))
}

fn search_functions() -> Section {
    section(
        "Search functions:",
        SEARCH_FUNCTIONS
            .iter()
            .map(|function| {
                let forms: Vec<String> = function
                    .names
                    .iter()
                    .map(|name| format!("{name}:{}", function.example))
                    .collect();
                entry(forms.join(", "), function.help)
            })
            .collect(),
    )
}

fn attribute_letters(title: &'static str) -> Section {
    section(
        title,
        LETTERS
            .iter()
            .map(|row| entry(row.letter.to_string(), row.meaning))
            .collect(),
    )
}

/// The entries of a table of usages and their texts.
fn pairs(table: &[(&str, &'static str)]) -> Vec<Entry> {
    table
        .iter()
        .map(|&(usage, text)| entry(usage.to_string(), text))
        .collect()
}

fn exit_codes(codes: &[ExitCode]) -> Section {
    section(
        "Exit codes:",
        codes
            .iter()
            .map(|&code| Entry {
                usage_kind: Usage::Styled(code.token()),
                ..entry(code.code().to_string(), code.meaning())
            })
            .collect(),
    )
}

/// An entry whose usage is text the reader types.
fn entry(usage: String, text: &'static str) -> Entry {
    Entry {
        usage,
        text,
        usage_kind: Usage::Expression,
    }
}

fn command(usage: String, text: &'static str) -> Entry {
    Entry {
        usage_kind: Usage::Command,
        ..entry(usage, text)
    }
}

fn section(title: &'static str, entries: Vec<Entry>) -> Section {
    Section { title, entries }
}

/// The forms of a switch the help lists, such as `-n N, -max-results N`.
fn switch_usage(spelling: &Spelling) -> String {
    match spelling {
        Spelling::Es { names, action } => {
            let param = action.param().map(|p| format!(" {p}")).unwrap_or_default();
            let forms: Vec<String> = names.iter().map(|n| format!("-{n}{param}")).collect();
            forms.join(", ")
        }
        Spelling::Family { usage, .. } => (*usage).to_string(),
        Spelling::Native { name, action } => {
            let param = action.param().map(|p| format!(" {p}")).unwrap_or_default();
            format!("--{name}{param}")
        }
    }
}

/// The quick help and non-console text fit an 80-column console.
const DEFAULT_LINE_WIDTH: usize = 79;

/// Writes the section's title and its entries, after a blank line.
fn render_section(out: &mut String, section: &Section, color: bool, line_width: usize) {
    out.push('\n');
    wrap_usage(
        out,
        0,
        section.title,
        Usage::Styled(ColorToken::Strong),
        color,
        line_width,
    );
    render(out, &section.entries, color, line_width);
}

/// Writes the notes for `reader` and for both helps, after a blank line.
fn render_notes(out: &mut String, reader: Reader, color: bool, line_width: usize) {
    out.push('\n');
    wrap_usage(
        out,
        0,
        "Notes:",
        Usage::Styled(ColorToken::Strong),
        color,
        line_width,
    );
    for &(_, note) in NOTES
        .iter()
        .filter(|(note_reader, _)| [reader, Reader::Both].contains(note_reader))
    {
        out.push_str("  - ");
        wrap(out, 4, note, 4, color, line_width);
    }
}

/// Uses one description column for the whole section, stacking every row
/// when the longest usage would leave less than 24 columns for descriptions.
fn render(out: &mut String, entries: &[Entry], color: bool, line_width: usize) {
    let width = entries.iter().map(|e| e.usage.len()).max().unwrap_or(0) + 2;
    let longest = entries
        .iter()
        .flat_map(|entry| entry.text.split(' '))
        .map(word_width)
        .max()
        .unwrap_or(0);
    let indent = if 2 + width + longest.max(24) > line_width {
        2
    } else {
        2 + width
    };
    for entry in entries {
        out.push_str("  ");
        if indent > 2 {
            syntax(out, &entry.usage, entry.usage_kind, color);
            out.push_str(&" ".repeat(width - entry.usage.len()));
        } else {
            wrap_usage(out, 2, &entry.usage, entry.usage_kind, color, line_width);
            out.push_str(&" ".repeat(indent));
        }
        let mut description = String::new();
        wrap(
            &mut description,
            indent,
            entry.text,
            indent,
            false,
            line_width,
        );
        paint(out, ColorToken::Muted, &description, color);
    }
}

fn wrap_usage(
    out: &mut String,
    column: usize,
    text: &str,
    usage: Usage,
    color: bool,
    line_width: usize,
) {
    let mut wrapped = String::new();
    wrap(&mut wrapped, column, text, column, false, line_width);
    syntax(out, &wrapped, usage, color);
}

fn syntax(out: &mut String, text: &str, usage: Usage, color: bool) {
    if !color {
        out.push_str(text);
        return;
    }
    if let Usage::Styled(token) = usage {
        let code = text.trim_end_matches(char::is_whitespace);
        paint(out, token, code, color);
        out.push_str(&text[code.len()..]);
        return;
    }
    let mut parameter = None;
    let mut quote = None;
    let mut quoted_token = None;
    for part in text.split_inclusive(char::is_whitespace) {
        let word = part.trim_end_matches(char::is_whitespace);
        let bare = word.trim_end_matches(',');
        let option =
            if quote.is_none() && parameter.is_none() && !matches!(usage, Usage::Expression) {
                bare.strip_prefix("--")
                    .and_then(match_native)
                    .or_else(|| bare.strip_prefix(['-', '/']).and_then(match_switch))
            } else {
                None
            };
        let token = if quote.is_some() {
            quoted_token
        } else if parameter.is_some() {
            Some(match parameter {
                Some("DIR" | "PATH") => ColorToken::Path,
                _ => ColorToken::Value,
            })
        } else if word == "gozler" && matches!(usage, Usage::Command) {
            Some(ColorToken::Executable)
        } else if option.is_some()
            || matches!(usage, Usage::Options) && bare.starts_with(['-', '/'])
        {
            Some(ColorToken::Primary)
        } else if crate::winpath::is_absolute(word.trim_matches(['\'', '"'])) {
            Some(ColorToken::Path)
        } else {
            None
        };
        if let Some(token) = token {
            paint(out, token, word, color);
        } else {
            out.push_str(word);
        }
        out.push_str(&part[word.len()..]);
        let mut escaped = false;
        for c in word.chars() {
            if !escaped && matches!(c, '\'' | '"') {
                if quote == Some(c) {
                    quote = None;
                    quoted_token = None;
                } else if quote.is_none() {
                    quote = Some(c);
                    quoted_token = token;
                }
            }
            escaped = c == '\\' && !escaped;
        }
        if quote.is_none() && !word.is_empty() {
            parameter = option.and_then(|action| action.param());
        }
    }
}

fn word_width(word: &str) -> usize {
    word.chars().filter(|&c| c != '`').count()
}

/// Appends `text` word-wrapped to `line_width`, starting at `column` of the
/// current line, with every line after the first indented by `indent` spaces.
///
/// Text between backticks is text the reader types, such as `` `-n` ``: the
/// backticks are dropped, and the text is colored when `color` is set.
fn wrap(
    out: &mut String,
    column: usize,
    text: &str,
    indent: usize,
    color: bool,
    line_width: usize,
) {
    let mut column = column;
    let mut at_line_start = true;
    let mut typed = false;
    for word in text.split(' ') {
        let width = word_width(word);
        if !at_line_start && column + 1 + width > line_width {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            column = indent;
            at_line_start = true;
        }
        if !at_line_start {
            out.push(' ');
            column += 1;
        }
        for (i, part) in word.split('`').enumerate() {
            if i > 0 {
                typed = !typed;
            }
            if typed {
                syntax(out, part, Usage::Command, color);
            } else {
                out.push_str(part);
            }
        }
        column += width;
        at_line_start = false;
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escompat::{Command, parse_argv};
    use crate::query::{ParseOptions, ParsedQuery, QueryError, QueryStart, Test, parse_query_with};

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(ToString::to_string).collect()
    }

    /// Splits a command line into arguments as Windows does for a program,
    /// for the quoting the examples use: `"` groups spaces and is removed,
    /// and `\"` is a literal `"`.
    fn command_line_args(line: &str) -> Vec<String> {
        let (mut args, mut arg, mut quoted) = (Vec::new(), String::new(), false);
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&'"') => {
                    chars.next();
                    arg.push('"');
                }
                '"' => quoted = !quoted,
                ' ' if !quoted => {
                    if !arg.is_empty() {
                        args.push(std::mem::take(&mut arg));
                    }
                }
                c => arg.push(c),
            }
        }
        if !arg.is_empty() {
            args.push(arg);
        }
        args
    }

    /// `query` parsed in `C:\repo`, as gozler parses it, so relative paths
    /// and `dm:` values resolve.
    fn parse(query: &str) -> Result<ParsedQuery, QueryError> {
        let start = QueryStart {
            week_start: jiff::civil::Weekday::Monday,
            now: jiff::Timestamp::UNIX_EPOCH,
            time_zone: jiff::tz::TimeZone::UTC,
        };
        let options = ParseOptions {
            working_dir: Some(r"C:\repo"),
            query_start: Some(&start),
            ..ParseOptions::default()
        };
        parse_query_with(query, options)
    }

    /// The usages of the full reference's section whose title starts with
    /// `title`.
    fn entries(title: &str) -> Vec<String> {
        full_sections()
            .into_iter()
            .find(|section| section.title.starts_with(title))
            .unwrap_or_else(|| panic!("the full reference has a {title} section"))
            .entries
            .into_iter()
            .map(|entry| entry.usage)
            .collect()
    }

    /// A search that searches no text: a listed switch the parser does not
    /// know would fail, or with `/` become search text. A switch may add a
    /// function, as `/ad` adds `folder:`.
    fn assert_switch_parses(tokens: &[&str]) {
        match parse_argv(&argv(tokens)) {
            Ok(Command::Run(plan)) => {
                let parsed = parse(&plan.query).unwrap_or_else(|e| panic!("{tokens:?}: {e}"));
                assert!(
                    parsed.is_text_empty(),
                    "{tokens:?} searches {:?}",
                    plan.query
                );
            }
            Ok(Command::Help(_) | Command::Version) => {}
            Ok(Command::Overview) => panic!("the help lists no empty command line"),
            Err(fatal) => panic!("{tokens:?}: {fatal}"),
        }
    }

    /// Every switch, sort key, search function, search term and example the
    /// full reference lists parses as the help describes it, so the help
    /// cannot list something gozler refuses or searches as text. The quick
    /// help lists a subset of the same rows.
    #[test]
    fn everything_the_help_lists_parses() {
        let sort_keys: Vec<String> = entries("Sort keys")
            .iter()
            .flat_map(|names| names.split(", ").map(str::to_string).collect::<Vec<_>>())
            .collect();
        for key in &sort_keys {
            for suffix in ["", "-ascending", "-descending"] {
                assert_switch_parses(&["-sort", &format!("{key}{suffix}")]);
                assert_switch_parses(&[&format!("-sort-{key}{suffix}")]);
            }
        }

        let options: Vec<String> = GROUPS
            .iter()
            .flat_map(|(_, title)| entries(title))
            .collect();
        assert_eq!(
            options.len(),
            SWITCHES.len(),
            "every switch is in a group the full reference lists"
        );
        let switch_forms: Vec<String> = options
            .iter()
            .flat_map(|usage| usage.split(", ").map(str::to_string).collect::<Vec<_>>())
            .collect();
        let letters = entries("Attribute letters");
        for form in &switch_forms {
            let mut tokens: Vec<&str> = form.split(' ').collect();
            match tokens.get(1).copied() {
                None if form.contains("KEY") => {
                    for key in &sort_keys {
                        assert_switch_parses(&[&form.replace("KEY", key)]);
                    }
                    continue;
                }
                None if form.contains("LETTERS") => {
                    for letter in &letters {
                        assert_switch_parses(&[&form.replace("LETTERS", letter)]);
                    }
                    continue;
                }
                None => {}
                Some("N") => tokens[1] = "5",
                Some("MS") => tokens[1] = "1000",
                Some("DIR") => tokens[1] = r"C:\repo",
                Some("PATH") => tokens[1] = r"C:\repo\x",
                Some("FILE") => tokens[1] = "out.csv",
                Some("KEY") => tokens[1] = "name",
                Some("PATTERN") => tokens[1] = r"^test\d+\.cs$",
                Some(param) => panic!("{form}: the test has no value for {param}"),
            }
            assert_switch_parses(&tokens);
        }

        let searched_as_text = |parsed: &ParsedQuery| {
            parsed
                .clauses
                .iter()
                .flat_map(|clause| &clause.terms)
                .any(|term| matches!(&term.test, Test::Name(name) if name.contains(':')))
        };
        for usage in entries("Search functions") {
            for form in usage.split(", ") {
                let parsed = parse(form).unwrap_or_else(|e| panic!("{form}: {e}"));
                assert!(
                    !searched_as_text(&parsed),
                    "{form} is searched as text, not as a function"
                );
            }
        }

        for term in entries("Search syntax") {
            let parsed = parse(&term).unwrap_or_else(|e| panic!("{term}: {e}"));
            assert!(!parsed.clauses.is_empty(), "{term} searches for nothing");
            assert!(
                !searched_as_text(&parsed),
                "{term} searches a function as text"
            );
        }

        // The window's form of an example is the query its command line
        // gives, so the two cannot drift apart.
        for example in EXAMPLES {
            let usage = example.usage;
            let args = command_line_args(usage);
            assert_eq!(args[0], "gozler", "{usage}");
            match parse_argv(&args[1..]) {
                Ok(Command::Run(plan)) => {
                    parse(&plan.query).unwrap_or_else(|e| panic!("{usage}: {e}"));
                    if let Some(query) = example.search_box {
                        assert_eq!(plan.query, query, "{usage}");
                    }
                }
                other => panic!("{usage}: {other:?}"),
            }
        }
    }

    /// Alias lists fit the console; the guide URL stays intact for copying even
    /// in a narrower console. Colors preserve text and alignment.
    #[test]
    fn color_changes_no_text_or_alignment() {
        for (screen, columns) in [
            (HelpScreen::Quick, None),
            (HelpScreen::Full, Some(30)),
            (HelpScreen::Full, Some(60)),
            (HelpScreen::Full, None),
            (HelpScreen::Full, Some(120)),
        ] {
            let plain = help(screen, false, columns);
            let colored = help(screen, true, columns);
            let line_width = columns.unwrap_or(80) - 1;
            assert!(!plain.contains('\x1b'));
            if screen == HelpScreen::Quick {
                assert!(
                    plain.lines().count() <= 23,
                    "quick help leaves room for the command and prompt"
                );
            }
            for line in plain.lines() {
                let intact_url =
                    line.trim() == "https://github.com/SynoSoftware/gozler/blob/main/docs/JSON.md";
                assert!(
                    line.chars().count() <= line_width || intact_url,
                    "{screen:?} at {line_width} columns: {line}"
                );
            }
            if columns == Some(120) {
                assert!(plain.lines().any(|line| line.chars().count() > 79));
            }
            assert!(colored.starts_with("\x1b[1mgozler "));
            assert!(colored.contains("\x1b[1;94mgozler\x1b[0m report"));
            assert!(colored.contains("\x1b[36m-n\x1b[0m"));
            assert!(colored.contains("\x1b[33m10\x1b[0m"));
            assert!(colored.contains("\x1b[33mout.csv\x1b[0m"));
            assert!(colored.contains("\x1b[32mC:\\repo\\\x1b[0m *.cs"));
            assert!(colored.contains("\x1b[32m0\x1b[0m"));
            assert!(colored.contains("\x1b[33m2\x1b[0m"));
            if screen == HelpScreen::Full {
                assert!(colored.contains("\x1b[1mExamples:\x1b[0m\n"));
                assert!(colored.contains("\x1b[31m4\x1b[0m"));
            }
            let mut stripped = String::new();
            let mut in_code = false;
            for c in colored.chars() {
                match c {
                    '\x1b' => in_code = true,
                    'm' if in_code => in_code = false,
                    c if !in_code => stripped.push(c),
                    _ => {}
                }
            }
            assert!(!in_code, "every ANSI sequence is complete");
            assert_eq!(stripped, plain, "{screen:?}");
        }
    }

    #[test]
    fn descriptions_use_one_column_or_stack_the_whole_section() {
        let entries = [
            command("gozler report".into(), "Find reports"),
            command("gozler -export-csv out.csv ext:cs".into(), "Save C# files"),
        ];
        let mut wide = String::new();
        render(&mut wide, &entries, false, 119);
        let lines: Vec<_> = wide.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].find("Find"), lines[1].find("Save"));

        let mut narrow = String::new();
        render(&mut narrow, &entries, false, 50);
        assert_eq!(
            narrow,
            "  gozler report\n  Find reports\n  gozler -export-csv out.csv ext:cs\n  Save C# files\n"
        );
    }

    #[test]
    fn quoted_values_keep_their_color_without_coloring_search_expressions() {
        let mut out = String::new();
        syntax(
            &mut out,
            r#"gozler -path "C:\My Files" -r "two -n words" ext:cs"#,
            Usage::Command,
            true,
        );
        assert_eq!(
            out,
            concat!(
                "\x1b[1;94mgozler\x1b[0m \x1b[36m-path\x1b[0m ",
                "\x1b[32m\"C:\\My\x1b[0m \x1b[32mFiles\"\x1b[0m ",
                "\x1b[36m-r\x1b[0m \x1b[33m\"two\x1b[0m ",
                "\x1b[33m-n\x1b[0m \x1b[33mwords\"\x1b[0m ext:cs"
            )
        );
    }
}
