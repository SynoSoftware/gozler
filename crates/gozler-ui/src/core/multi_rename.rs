//! Multi-Rename's rules (DESIGN.md: Multi-Rename): what new name each row
//! of the dialog's list gets from the fields, which rows keep their names
//! and why, and which new names are numbered because they are in use. The
//! engine renames ([`gozler_fileops::Request::Rename`]) and decides again
//! from the disk; this preview only shows what will happen, so it reads
//! nothing from the disk.
//!
//! For each row: the masks, then search and replace, then Case, then the
//! checks of the names the person types, then numbering.

pub(crate) mod mask;
mod replace;

use super::text::Numbers;
use super::typed::{self, Refusal};
use gozler_fileops::names::{fold, numbered_name};
use gozler_fileops::{Rename, Renamed};
use jiff::tz::TimeZone;
use mask::{CounterAt, Mask, MaskError, Missing, Output};
use replace::{Replacer, Wanted};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The words that English title rules keep lowercase, except as a name's
/// first or last word.
const SMALL_WORDS: [&str; 19] = [
    "a", "an", "the", "at", "by", "for", "in", "of", "on", "to", "from", "up", "and", "as", "but",
    "or", "nor", "yet", "so",
];

/// The longest name Windows takes, in UTF-16 units.
const LONGEST_NAME: usize = 255;

/// The dialog's fields: everything that makes the new names, as a saved
/// rename holds them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Fields {
    pub(crate) name: String,
    pub(crate) extension: String,
    pub(crate) search: String,
    pub(crate) replace: String,
    pub(crate) match_case: bool,
    pub(crate) first_only: bool,
    pub(crate) extension_too: bool,
    pub(crate) regex: bool,
    /// Only with `regex`.
    pub(crate) whole_name: bool,
    pub(crate) case: Case,
    pub(crate) counter: Counter,
}

impl Default for Fields {
    fn default() -> Self {
        Fields {
            name: "[N]".to_owned(),
            extension: "[E]".to_owned(),
            search: String::new(),
            replace: String::new(),
            match_case: false,
            first_only: false,
            extension_too: true,
            regex: false,
            whole_name: false,
            case: Case::Unchanged,
            counter: Counter::default(),
        }
    }
}

/// A case: the Case list's, and the one a case placeholder sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Case {
    #[default]
    Unchanged,
    Lower,
    Upper,
    /// The first letter uppercase and the rest lowercase.
    FirstLetter,
    /// The first letter of each word uppercase and the rest lowercase.
    EachWord,
    /// [`Case::EachWord`] with English title rules.
    Title,
}

impl Case {
    pub(crate) const ALL: [Case; 6] = [
        Case::Unchanged,
        Case::Lower,
        Case::Upper,
        Case::FirstLetter,
        Case::EachWord,
        Case::Title,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Case::Unchanged => "multi_rename.case_unchanged",
            Case::Lower => "multi_rename.case_lower",
            Case::Upper => "multi_rename.case_upper",
            Case::FirstLetter => "multi_rename.case_first_letter",
            Case::EachWord => "multi_rename.case_each_word",
            Case::Title => "multi_rename.case_title",
        }
    }
}

/// The Counter fields, which `[C]` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Counter {
    pub(crate) start: i64,
    pub(crate) step: i64,
    pub(crate) digits: Digits,
}

impl Default for Counter {
    fn default() -> Self {
        Counter {
            start: 1,
            step: 1,
            digits: Digits::Fixed(1),
        }
    }
}

/// How many digits a counter shows at least; more when its value needs
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Digits {
    /// 1 to 10.
    Fixed(u8),
    /// As many as the largest value the counter reaches in the list.
    Auto,
}

/// An entry the dialog renames, with what the list already holds of it.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) path: Vec<u16>,
    pub(crate) is_folder: bool,
    pub(crate) size: Option<u64>,
    /// The last write time as a `FILETIME`.
    pub(crate) modified: Option<i64>,
}

/// One row of the dialog's list.
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) entry: Entry,
    /// Where the name starts in the path.
    name_at: usize,
    pub(crate) name: String,
    /// Where the extension's dot is in `name`; its length when it has none.
    dot: usize,
    /// The folder and the name, as Windows compares them.
    folder_key: Arc<[u16]>,
    name_key: Box<[u16]>,
    /// Date taken, or Media created for a video, once it is read.
    pub(crate) taken: Option<i64>,
    /// The name that Edit names gave it.
    pub(crate) edited: Option<String>,
    /// A rename that failed keeps the name it had until a field changes,
    /// so the counter does not number it again.
    pub(crate) failed: Option<Failed>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failed {
    pub(crate) name: String,
    /// Windows' error.
    pub(crate) error: u32,
}

impl Row {
    /// The rows of `entries`, in their order. Rows of one folder share its
    /// key.
    pub(crate) fn of(entries: Vec<Entry>) -> Vec<Row> {
        let mut folders: HashMap<Vec<u16>, Arc<[u16]>> = HashMap::new();
        entries
            .into_iter()
            .map(|entry| {
                let name_at = entry
                    .path
                    .iter()
                    .rposition(|&unit| unit == u16::from(b'\\'))
                    .map_or(0, |at| at + 1);
                let folder = fold(&entry.path[..name_at.saturating_sub(1)]);
                let folder_key = Arc::clone(
                    folders
                        .entry(folder)
                        .or_insert_with_key(|folder| Arc::from(folder.as_slice())),
                );
                let name = String::from_utf16_lossy(&entry.path[name_at..]);
                let dot = extension_dot(&name, entry.is_folder);
                Row {
                    name_key: fold(&entry.path[name_at..]).into(),
                    name_at,
                    name,
                    dot,
                    folder_key,
                    entry,
                    taken: None,
                    edited: None,
                    failed: None,
                }
            })
            .collect()
    }

    /// The folder that holds the entry, as text, with its `\`.
    pub(crate) fn folder(&self) -> String {
        String::from_utf16_lossy(&self.entry.path[..self.name_at])
    }

    /// Whether `other`'s entry is in the same folder.
    pub(crate) fn shares_folder(&self, other: &Row) -> bool {
        self.folder_key == other.folder_key
    }

    /// The name without its extension, which `[N]` inserts.
    pub(crate) fn stem(&self) -> &str {
        &self.name[..self.dot]
    }

    /// The extension without its dot, which `[E]` inserts.
    pub(crate) fn extension(&self) -> &str {
        self.name.get(self.dot + 1..).unwrap_or_default()
    }

    /// The folders from the root: the drive letter without its colon or the
    /// server first, and how many of them are below the root.
    fn folders(&self) -> (Vec<String>, usize) {
        let text = String::from_utf16_lossy(&self.entry.path[..self.name_at]);
        let text = text.trim_end_matches('\\');
        let (text, root) = match text.strip_prefix(r"\\") {
            Some(share) => (share, 2),
            None => (text, 1),
        };
        let folders: Vec<String> = text
            .split('\\')
            .enumerate()
            .map(|(index, name)| match index {
                0 => name.trim_end_matches(':').to_owned(),
                _ => name.to_owned(),
            })
            .collect();
        let depth = folders.len().saturating_sub(root);
        (folders, depth)
    }
}

/// Where a name's extension starts: its last dot, but not a leading one,
/// and none for a folder, as Keep both's numbering splits it.
fn extension_dot(name: &str, is_folder: bool) -> usize {
    match name.rfind('.') {
        Some(dot) if dot > 0 && !is_folder => dot,
        _ => name.len(),
    }
}

/// The fields, read once for every change of a field.
#[derive(Debug)]
pub(crate) struct Rule {
    name: Mask,
    extension: Mask,
    replacer: Replacer,
    extension_too: bool,
    case: Case,
    counter: Counter,
}

/// Why the fields give no new names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RuleError {
    Mask(MaskError),
    /// With the regex crate's reason.
    Regex(String),
}

impl Rule {
    pub(crate) fn new(fields: &Fields) -> Result<Rule, RuleError> {
        let name = Mask::read(&fields.name).map_err(RuleError::Mask)?;
        let extension = Mask::read(&fields.extension).map_err(RuleError::Mask)?;
        let replacer = Replacer::new(&Wanted {
            search: &fields.search,
            replace: &fields.replace,
            match_case: fields.match_case,
            first_only: fields.first_only,
            regex: fields.regex,
            whole_name: fields.regex && fields.whole_name,
        })
        .map_err(RuleError::Regex)?;
        Ok(Rule {
            name,
            extension,
            replacer,
            extension_too: fields.extension_too,
            case: fields.case,
            counter: fields.counter,
        })
    }

    /// Whether a mask reads Date taken, which the dialog reads first.
    pub(crate) fn reads_date_taken(&self) -> bool {
        self.name.reads_date_taken() || self.extension.reads_date_taken()
    }

    /// The new name the rule makes for the row at `index` of a list of
    /// `rows`, relative to its folder, before the checks.
    fn make(
        &self,
        row: &Row,
        index: u64,
        rows: u64,
        context: &Context<'_>,
    ) -> Result<String, Missing> {
        let reads_folders = self.name.reads_folders() || self.extension.reads_folders();
        let (folders, _) = if reads_folders {
            row.folders()
        } else {
            (Vec::new(), 0)
        };
        let folders: Vec<&str> = folders.iter().map(String::as_str).collect();
        let source = mask::Row {
            stem: row.stem(),
            extension: row.extension(),
            name: &row.name,
            folders: &folders,
            counter: CounterAt {
                index,
                rows,
                fields: &self.counter,
            },
            modified: row.entry.modified,
            taken: row.taken,
            zone: context.zone,
            formats: context.formats,
        };
        let mut name = Output::default();
        self.name.evaluate(&source, &mut name)?;
        let mut extension = Output::default();
        self.extension.evaluate(&source, &mut extension)?;
        let (name, extension) = (name.cased(), extension.cased());
        let joined = |name: String, extension: &str| match extension.is_empty() {
            true => name,
            false => format!("{name}.{extension}"),
        };
        let replaced = match self.extension_too {
            true => self.replacer.apply(joined(name, &extension)),
            false => joined(self.replacer.apply(name), &extension),
        };
        Ok(with_case(&replaced, self.case, row.entry.is_folder))
    }
}

/// What makes the names: the fields, or Edit names.
pub(crate) enum Source<'a> {
    Rule(&'a Rule),
    Edited,
}

/// What formats dates for a mask: the time zone the list shows dates in,
/// and Windows' short formats for `[d]` and `[t]`.
pub(crate) struct Context<'a> {
    pub(crate) zone: &'a TimeZone,
    pub(crate) formats: &'a dyn Numbers,
}

/// The names of the folder that a folder panel shows, which the preview
/// numbers against; the engine numbers in every other folder.
#[derive(Debug, Default)]
pub(crate) struct Known {
    folder: Box<[u16]>,
    names: HashSet<Box<[u16]>>,
}

impl Known {
    /// `folder` without its trailing `\`, and its entries' names.
    pub(crate) fn new<'a>(folder: &str, names: impl Iterator<Item = &'a [u16]>) -> Known {
        let folder: Vec<u16> = folder.trim_end_matches('\\').encode_utf16().collect();
        Known {
            folder: fold(&folder).into(),
            names: names.map(|name| fold(name).into()).collect(),
        }
    }
}

/// What a row gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The new name, relative to its folder; it may hold `\` and `..`.
    New { name: String, numbered: bool },
    /// The rule gives the name it has.
    NoChange,
    /// It keeps its name, because the rule cannot give it one.
    Kept(Problem),
    /// The rename failed; Rename tries the same name again.
    Failed(Failed),
}

impl Outcome {
    /// The name the New name column shows.
    pub(crate) fn name<'a>(&'a self, row: &'a Row) -> &'a str {
        match self {
            Outcome::New { name, .. } | Outcome::Failed(Failed { name, .. }) => name,
            Outcome::NoChange | Outcome::Kept(_) => &row.name,
        }
    }

    /// The name Rename gives the row, if it gets one.
    pub(crate) fn renames(&self) -> Option<&str> {
        match self {
            Outcome::New { name, .. } | Outcome::Failed(Failed { name, .. }) => Some(name),
            Outcome::NoChange | Outcome::Kept(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    Invalid(Invalid),
    NoDateTaken,
    NoDateModified,
}

/// Why a new name cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Invalid {
    /// A name holds a character that no name can hold.
    Characters,
    /// A name that Windows keeps for a device, such as `NUL`.
    Reserved(String),
    TooLong,
    /// The name, or a folder's name, is empty.
    Empty,
    AboveDrive,
    /// It starts with a drive, such as `D:`.
    Drive,
}

/// What Rename will do, for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) rows: usize,
    /// The rows that get a new name, numbered ones included.
    pub(crate) renamed: usize,
    pub(crate) numbered: usize,
    /// The rows that keep their name because of a problem.
    pub(crate) kept: usize,
}

impl Counts {
    pub(crate) fn of(outcomes: &[Outcome]) -> Counts {
        let mut counts = Counts {
            rows: outcomes.len(),
            ..Counts::default()
        };
        for outcome in outcomes {
            match outcome {
                Outcome::New { numbered, .. } => {
                    counts.renamed += 1;
                    counts.numbered += usize::from(*numbered);
                }
                Outcome::Failed(_) => counts.renamed += 1,
                Outcome::Kept(_) => counts.kept += 1,
                Outcome::NoChange => {}
            }
        }
        counts
    }
}

/// Each row's new name, in the list's order.
pub(crate) fn preview(
    rows: &[Row],
    source: &Source<'_>,
    known: Option<&Known>,
    context: &Context<'_>,
) -> Vec<Outcome> {
    let count = rows.len() as u64;
    let mut outcomes: Vec<Outcome> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            if let Some(failed) = &row.failed {
                return Outcome::Failed(failed.clone());
            }
            let made = match source {
                Source::Rule(rule) => rule.make(row, index as u64, count, context),
                Source::Edited => Ok(row.edited.clone().unwrap_or_else(|| row.name.clone())),
            };
            let made = match made {
                Ok(made) => made,
                Err(Missing::Taken) => return Outcome::Kept(Problem::NoDateTaken),
                Err(Missing::Modified) => return Outcome::Kept(Problem::NoDateModified),
            };
            match checked(&made, || row.folders().1) {
                Err(invalid) => Outcome::Kept(Problem::Invalid(invalid)),
                Ok(name) if name == row.name => Outcome::NoChange,
                Ok(name) => Outcome::New {
                    name,
                    numbered: false,
                },
            }
        })
        .collect();
    number(rows, &mut outcomes, known);
    outcomes
}

/// A new name with the checks of "Names the person types" applied to each
/// of its names: trailing dots and spaces trimmed, a device name and a
/// character that no name holds refused. A trailing `\` is dropped; `..`
/// goes up a folder, never above the drive, where `depth` gives how many
/// folders hold the entry.
fn checked(made: &str, depth: impl FnOnce() -> usize) -> Result<String, Invalid> {
    let made = made.trim_end_matches('\\');
    if made.is_empty() {
        return Err(Invalid::Empty);
    }
    if let [letter, b':', ..] = made.as_bytes()
        && letter.is_ascii_alphabetic()
    {
        return Err(Invalid::Drive);
    }
    let names: Vec<&str> = made.split('\\').collect();
    let mut level = match names.contains(&"..") {
        true => depth(),
        false => 0,
    };
    let mut out = String::with_capacity(made.len());
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            out.push('\\');
        }
        let last = index + 1 == names.len();
        if *name == ".." {
            if last {
                return Err(Invalid::Empty);
            }
            level = level.checked_sub(1).ok_or(Invalid::AboveDrive)?;
            out.push_str(name);
            continue;
        }
        let name = typed::folder_name(name).map_err(|refusal| match refusal {
            Refusal::Characters => Invalid::Characters,
            Refusal::Reserved(name) => Invalid::Reserved(name),
        })?;
        if name.is_empty() {
            return Err(Invalid::Empty);
        }
        if name.encode_utf16().count() > LONGEST_NAME {
            return Err(Invalid::TooLong);
        }
        out.push_str(&name);
        level += 1;
    }
    Ok(out)
}

/// A name and its folder, as Windows compares them.
type Key = (Arc<[u16]>, Box<[u16]>);

/// Numbers each new name that is in use, with Keep both's numbering: by an
/// earlier row's new name, by a row that keeps its name, or by an entry of
/// the folder `known`. A name that a renamed row gives up is free, so a
/// swap and a change of case are not numbered.
fn number(rows: &[Row], outcomes: &mut [Outcome], known: Option<&Known>) {
    let old = |row: &Row| (Arc::clone(&row.folder_key), row.name_key.clone());
    let mut given_up = HashSet::new();
    let mut held = HashSet::new();
    for (row, outcome) in rows.iter().zip(outcomes.iter()) {
        match outcome.renames() {
            Some(_) => given_up.insert(old(row)),
            None => held.insert(old(row)),
        };
    }
    let mut claimed: HashSet<Key> = HashSet::new();
    let in_use = |key: &Key, claimed: &HashSet<Key>| {
        claimed.contains(key)
            || held.contains(key)
            || (!given_up.contains(key)
                && known
                    .is_some_and(|known| *known.folder == *key.0 && known.names.contains(&key.1)))
    };
    // A failed row keeps the name it had.
    for (row, outcome) in rows.iter().zip(outcomes.iter()) {
        if let Outcome::Failed(failed) = outcome {
            claimed.insert(target(row, &failed.name));
        }
    }
    for (row, outcome) in rows.iter().zip(outcomes.iter_mut()) {
        let Outcome::New { name, numbered } = outcome else {
            continue;
        };
        let key = target(row, name);
        if !in_use(&key, &claimed) {
            claimed.insert(key);
            continue;
        }
        let (folders, last) = match name.rfind('\\') {
            Some(at) => name.split_at(at + 1),
            None => ("", name.as_str()),
        };
        let last: Vec<u16> = last.encode_utf16().collect();
        for number in 2u32.. {
            let candidate = format!(
                "{folders}{}",
                String::from_utf16_lossy(&numbered_name(&last, row.entry.is_folder, number))
            );
            let key = target(row, &candidate);
            if !in_use(&key, &claimed) {
                claimed.insert(key);
                *name = candidate;
                *numbered = true;
                break;
            }
        }
    }
}

/// The folder and the name that `name`, relative to `row`'s folder,
/// names.
fn target(row: &Row, name: &str) -> Key {
    let Some(at) = name.rfind('\\') else {
        let units: Vec<u16> = name.encode_utf16().collect();
        return (Arc::clone(&row.folder_key), fold(&units).into());
    };
    let mut folder = row.folder_key.to_vec();
    for part in name[..at].split('\\') {
        if part == ".." {
            let cut = folder
                .iter()
                .rposition(|&unit| unit == u16::from(b'\\'))
                .unwrap_or(0);
            folder.truncate(cut);
        } else {
            folder.push(u16::from(b'\\'));
            let units: Vec<u16> = part.encode_utf16().collect();
            folder.extend(fold(&units));
        }
    }
    let units: Vec<u16> = name[at + 1..].encode_utf16().collect();
    (folder.into(), fold(&units).into())
}

/// `name` in the Case list's `case`: lowercase and uppercase change the
/// whole name, the other cases only the name without its extension,
/// because an extension such as `Jpg` is never wanted.
fn with_case(name: &str, case: Case, is_folder: bool) -> String {
    match case {
        Case::Unchanged => name.to_owned(),
        Case::Lower | Case::Upper => cased(name, |_| case),
        Case::FirstLetter | Case::EachWord | Case::Title => {
            let last = name.rfind('\\').map_or(0, |at| at + 1);
            let dot = last + extension_dot(&name[last..], is_folder);
            let mut out = cased(&name[..dot], |_| case);
            out.push_str(&name[dot..]);
            out
        }
    }
}

/// A column of the dialog's list, which a click on its header sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Column {
    Name,
    NewName,
    Note,
    Size,
    Modified,
    Folder,
}

/// The order of `rows` by `column`, as the index of the row that goes
/// first, then the next. `outcomes` are the rows' new names. Rows that
/// compare equal keep their order, so a second column's sort stays inside
/// the first's.
pub(crate) fn sorted(
    rows: &[Row],
    outcomes: &[Outcome],
    column: Column,
    descending: bool,
) -> Vec<usize> {
    let folded = |text: &str| fold(&text.encode_utf16().collect::<Vec<u16>>());
    let new_names: Vec<Vec<u16>> = match column {
        Column::NewName => (rows.iter().enumerate())
            .map(|(index, row)| {
                let name = outcomes
                    .get(index)
                    .map_or(row.name.as_str(), |outcome| outcome.name(row));
                folded(name)
            })
            .collect(),
        _ => Vec::new(),
    };
    // Each kind of note sorts as a group.
    let note = |index: usize| match outcomes.get(index) {
        Some(Outcome::NoChange) | None => 0,
        Some(Outcome::New {
            numbered: false, ..
        }) => 1,
        Some(Outcome::New { numbered: true, .. }) => 2,
        Some(Outcome::Kept(_)) => 3,
        Some(Outcome::Failed(_)) => 4,
    };
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        let (one, other) = (&rows[a], &rows[b]);
        let ordering = match column {
            Column::Name => one.name_key.cmp(&other.name_key),
            Column::NewName => new_names[a].cmp(&new_names[b]),
            Column::Note => note(a).cmp(&note(b)),
            Column::Size => one.entry.size.cmp(&other.entry.size),
            Column::Modified => one.entry.modified.cmp(&other.entry.modified),
            Column::Folder => (one.folder_key.cmp(&other.folder_key))
                .then_with(|| one.name_key.cmp(&other.name_key)),
        };
        match descending {
            true => ordering.reverse(),
            false => ordering,
        }
    });
    order
}

/// The range placeholder of the letter `letter`, such as `N`, that names
/// the characters `selected` of `text`, given as UTF-16 units as an edit
/// control gives its selection: counted from the start, as `[N3-6]`, or
/// from the end, as `[N-8-5]`. `None` when nothing is selected.
pub(crate) fn range(
    letter: char,
    text: &str,
    selected: (usize, usize),
    from_end: bool,
) -> Option<String> {
    // The characters before a UTF-16 offset, because a range counts a
    // character that Windows stores as two units once.
    let before = |units: usize| {
        let mut seen = 0;
        text.chars()
            .take_while(|c| {
                seen += c.len_utf16();
                seen <= units
            })
            .count()
    };
    let (start, end) = (before(selected.0), before(selected.1));
    if start >= end {
        return None;
    }
    let count = text.chars().count();
    let (first, last, sign) = match from_end {
        false => (start + 1, end, ""),
        true => (count - start, count + 1 - end, "-"),
    };
    Some(match first == last {
        true => format!("[{letter}{sign}{first}]"),
        false => format!("[{letter}{sign}{first}-{last}]"),
    })
}

/// The request that renames each entry of `batch`, a batch's renames in
/// the order they happened, back to its old name, the last rename first.
pub(crate) fn undo(batch: &[Renamed]) -> Vec<Rename> {
    (batch.iter().rev())
        .map(|renamed| Rename::new(renamed.to.clone(), relative(&renamed.to, &renamed.from)))
        .collect()
}

/// The path of `from` relative to the folder of `at`: its name, after a
/// `..` for each folder up from `at` and the folders down to `from`.
fn relative(at: &Path, from: &Path) -> PathBuf {
    let folders = |path: &Path| -> Vec<Vec<u16>> {
        (path.parent().into_iter().flat_map(Path::iter))
            .map(|part| part.encode_wide().collect())
            .collect()
    };
    let (here, there) = (folders(at), folders(from));
    let shared = (here.iter().zip(&there))
        .take_while(|(one, other)| fold(one) == fold(other))
        .count();
    let mut name = PathBuf::new();
    for _ in shared..here.len() {
        name.push("..");
    }
    for part in &there[shared..] {
        name.push(OsString::from_wide(part));
    }
    name.push(from.file_name().unwrap_or_default());
    name
}

/// The placeholders that More offers, in groups, each with the catalogue
/// key of what it inserts.
pub(crate) const MORE: [&[(&str, &str)]; 5] = [
    &[
        ("[A]", "multi_rename.more_whole_name"),
        ("[P]", "multi_rename.more_folder"),
        ("[G]", "multi_rename.more_parent"),
        ("[B+0]", "multi_rename.more_drive"),
        ("[B+1]", "multi_rename.more_top_folder"),
    ],
    &[
        ("[Y]", "multi_rename.more_year"),
        ("[y]", "multi_rename.more_short_year"),
        ("[M]", "multi_rename.more_month"),
        ("[D]", "multi_rename.more_day"),
        ("[d]", "multi_rename.more_short_date"),
    ],
    &[
        ("[h]", "multi_rename.more_hour"),
        ("[H]", "multi_rename.more_twelve_hour"),
        ("[i]", "multi_rename.more_am_pm"),
        ("[m]", "multi_rename.more_minutes"),
        ("[s]", "multi_rename.more_seconds"),
        ("[v]", "multi_rename.more_milliseconds"),
        ("[t]", "multi_rename.more_short_time"),
    ],
    &[
        ("[T1]", "multi_rename.more_date_modified"),
        ("[T4]", "multi_rename.more_date_taken"),
    ],
    &[
        ("[U]", "multi_rename.more_upper"),
        ("[L]", "multi_rename.more_lower"),
        ("[F]", "multi_rename.more_each_word"),
        ("[f]", "multi_rename.more_title"),
        ("[n]", "multi_rename.more_as_is"),
    ],
];

/// The first letter uppercase and the rest lowercase.
pub(super) fn first_letter_upper(text: &str) -> String {
    cased(text, |_| Case::FirstLetter)
}

/// `text` with each character in the case `case_at` gives for its byte
/// offset. A word starts at the start of the text or after a character
/// that is not a letter or a digit, as gozler's search reads words.
fn cased(text: &str, case_at: impl Fn(usize) -> Case) -> String {
    let mut words: Vec<(usize, usize, bool)> = Vec::new();
    let mut start = None;
    for (at, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                let word = text[from..at].to_lowercase();
                words.push((from, at, SMALL_WORDS.contains(&word.as_str())));
                start = None;
            }
            _ => {}
        }
    }
    let last = words.len().saturating_sub(1);
    let mut out = String::with_capacity(text.len());
    let mut word = 0;
    let mut letter_seen = false;
    for (at, c) in text.char_indices() {
        while words.get(word).is_some_and(|&(_, end, _)| end <= at) {
            word += 1;
        }
        let place = words.get(word).filter(|&&(from, _, _)| from <= at);
        let starts = place.is_some_and(|&(from, _, _)| from == at);
        let small = place.is_some_and(|&(_, _, small)| small) && word != 0 && word != last;
        let upper = match case_at(at) {
            Case::Unchanged => {
                out.push(c);
                continue;
            }
            Case::Upper => true,
            Case::Lower => false,
            Case::FirstLetter => c.is_alphabetic() && !letter_seen,
            Case::EachWord => starts,
            Case::Title => starts && !small,
        };
        letter_seen |= c.is_alphabetic();
        match upper {
            true => out.extend(c.to_uppercase()),
            false => out.extend(c.to_lowercase()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::text::tests::English;

    fn rows(paths: &[&str]) -> Vec<Row> {
        Row::of(
            paths
                .iter()
                .map(|path| Entry {
                    path: path.trim_end_matches('\\').encode_utf16().collect(),
                    is_folder: path.ends_with('\\'),
                    size: None,
                    modified: None,
                })
                .collect(),
        )
    }

    fn names(rows: &[Row], fields: &Fields, known: Option<&Known>) -> Vec<Outcome> {
        let rule = Rule::new(fields).expect("the fields read");
        preview(
            rows,
            &Source::Rule(&rule),
            known,
            &Context {
                zone: &TimeZone::UTC,
                formats: &English,
            },
        )
    }

    fn new(name: &str) -> Outcome {
        Outcome::New {
            name: name.to_owned(),
            numbered: false,
        }
    }

    fn numbered(name: &str) -> Outcome {
        Outcome::New {
            name: name.to_owned(),
            numbered: true,
        }
    }

    /// The masks come first, then search and replace, then Case, then the
    /// checks: a replacement can make a name the checks refuse, and Case
    /// does not change the extension.
    #[test]
    fn the_steps_run_in_their_order() {
        let list = rows(&[r"C:\a\Photo one.JPG", r"C:\a\x.txt", r"C:\a\keep.txt"]);
        let fields = Fields {
            name: "[N]-[C:2]".into(),
            search: "one|x-02".into(),
            replace: "the one|nul".into(),
            case: Case::Title,
            ..Fields::default()
        };
        assert_eq!(
            names(&list, &fields, None),
            [
                new("Photo the One-01.JPG"),
                Outcome::Kept(Problem::Invalid(Invalid::Reserved("Nul.txt".into()))),
                new("Keep-03.txt"),
            ]
        );
    }

    /// A folder's name has no extension, so `My.Folder` keeps its dot, and
    /// a name the rule leaves as it is changes nothing.
    #[test]
    fn a_folder_keeps_its_dots() {
        let list = rows(&[r"C:\a\My.Folder\", r"C:\a\same.txt"]);
        let fields = Fields {
            name: "[N]_x".into(),
            search: "same.txt_x".into(),
            replace: "same.txt".into(),
            ..Fields::default()
        };
        let outcomes = names(&list, &fields, None);
        assert_eq!(outcomes[0], new("My.Folder_x"));
        assert_eq!(
            names(&list[1..], &Fields::default(), None),
            [Outcome::NoChange]
        );
    }

    /// `\` makes folders and `..` goes up, never above the drive; a
    /// trailing `\` is dropped and an empty folder name is refused.
    #[test]
    fn folders_in_a_new_name() {
        let list = rows(&[r"C:\a\b.txt", r"C:\c.txt"]);
        let up = |mask: &str| Fields {
            name: mask.into(),
            ..Fields::default()
        };
        let trailing = Fields {
            name: r"x\[N]".into(),
            extension: r"[E]\".into(),
            ..Fields::default()
        };
        assert_eq!(names(&list[..1], &trailing, None), [new(r"x\b.txt")]);
        assert_eq!(names(&list[..1], &up(r"..\[N]"), None), [new(r"..\b.txt")]);
        assert_eq!(
            names(&list[1..], &up(r"..\[N]"), None),
            [Outcome::Kept(Problem::Invalid(Invalid::AboveDrive))]
        );
        assert_eq!(
            names(&list[..1], &up(r"x\\[N]"), None),
            [Outcome::Kept(Problem::Invalid(Invalid::Empty))]
        );
        assert_eq!(
            names(&list[..1], &up(r"D:[N]"), None),
            [Outcome::Kept(Problem::Invalid(Invalid::Drive))]
        );
    }

    /// Two equal new names number the later one; a name an entry of the
    /// panel's folder holds is numbered; a swap and a change of case are
    /// not, because the batch gives those names up.
    #[test]
    fn numbering_of_equal_names_and_names_in_use() {
        let list = rows(&[r"C:\a\1.txt", r"C:\a\2.txt", r"C:\a\3.txt"]);
        let same = Fields {
            name: "photo".into(),
            ..Fields::default()
        };
        let other: Vec<u16> = "photo (2).txt".encode_utf16().collect();
        let known = Known::new(r"C:\A\", [other.as_slice()].into_iter());
        assert_eq!(
            names(&list, &same, Some(&known)),
            [
                new("photo.txt"),
                numbered("photo (3).txt"),
                numbered("photo (4).txt")
            ]
        );
        let swap = Fields {
            name: "[C3-1]".into(),
            ..Fields::default()
        };
        let one: Vec<u16> = "1.txt".encode_utf16().collect();
        let known = Known::new(r"C:\a", [one.as_slice()].into_iter());
        assert_eq!(
            names(&list, &swap, Some(&known)),
            [new("3.txt"), Outcome::NoChange, new("1.txt")]
        );
        let upper = Fields {
            case: Case::Upper,
            ..Fields::default()
        };
        assert_eq!(names(&list[..1], &upper, None), [new("1.TXT")]);
    }

    /// The Case list leaves the extension alone except for lowercase and
    /// UPPERCASE, and title rules keep small words lowercase inside a name.
    #[test]
    fn case_list() {
        let cased = |name: &str, case: Case| {
            let fields = Fields {
                case,
                ..Fields::default()
            };
            names(&rows(&[&format!(r"C:\a\{name}")]), &fields, None)
        };
        assert_eq!(
            cased("the end of the WORLD.Jpg", Case::Title),
            [new("The End of the World.Jpg")]
        );
        assert_eq!(
            cased("hELLO wORLD.Jpg", Case::FirstLetter),
            [new("Hello world.Jpg")]
        );
        assert_eq!(
            cased("rock'n'roll.mp3", Case::EachWord),
            [new("Rock'N'Roll.mp3")]
        );
        assert_eq!(cased("a.B", Case::Lower), [new("a.b")]);
    }

    /// The name one mask makes for an entry of `C:\Photos\2026\Trip`, and
    /// "" where it makes an empty name.
    fn made(name: &str, mask: &str) -> String {
        let fields = Fields {
            name: mask.into(),
            ..Fields::default()
        };
        let list = rows(&[&format!(r"C:\Photos\2026\Trip\{name}")]);
        match &names(&list, &fields, None)[0] {
            Outcome::Kept(Problem::Invalid(Invalid::Empty)) => String::new(),
            outcome => outcome.name(&list[0]).to_owned(),
        }
    }

    /// Each range form names the characters Total Commander's names,
    /// counted from the end where it says so, padded where it starts with 0
    /// or a space, and a range past the end gives what exists (DESIGN.md:
    /// Multi-Rename).
    #[test]
    fn each_range_form_names_its_characters() {
        for (mask, expected) in [
            ("[N3]", "c"),
            ("[N-3]", "h"),
            ("[N2-5]", "bcde"),
            ("[N2,5]", "bcdef"),
            ("[N2-]", "bcdefghij"),
            ("[N-8,5]", "cdefg"),
            ("[N-8-5]", "cdef"),
            ("[N2--5]", "bcdef"),
            ("[N-5-]", "fghij"),
            ("[N8-20]", "hij"),
            ("[N-20,3]", ""),
        ] {
            assert_eq!(made("abcdefghij", mask), expected, "{mask}");
        }
        assert_eq!(made("abc", "[N02-9]"), "000000bc");
        assert_eq!(made("abc", "[N 2-9]"), "      bc");
        // A character that Windows stores as two UTF-16 units counts once.
        assert_eq!(made("a😀b", "[N2]"), "😀");
    }

    /// Folders count up from the entry and down from the root, `[I]` is
    /// accepted and ignored, and `[[]` and `[]]` insert brackets.
    #[test]
    fn folders_and_escapes() {
        assert_eq!(
            made("x", "[I][P]-[G]-[B2]-[B+0]-[B+1_1-3]"),
            "Trip-2026-Photos-C-Pho"
        );
        assert_eq!(made("My", "[[][N]1[]]"), "[My1]");
        assert_eq!(Mask::read("[Q]"), Err(MaskError::Unknown("[Q]".into())));
        assert_eq!(Mask::read("a[N"), Err(MaskError::Unclosed));
    }

    /// The counter takes its fields' parts unless the placeholder sets
    /// them, steps once every `/n` rows, pads to its digits, and counts in
    /// letters from a letter start. Every row counts, also one that keeps
    /// its name.
    #[test]
    fn the_counter_forms_count_by_the_row() {
        let paths: Vec<String> = (0..12).map(|n| format!(r"C:\a\f{n}")).collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let list = rows(&paths);
        let counted = |mask: &str, row: usize| {
            let fields = Fields {
                name: format!("[N]-{mask}"),
                counter: Counter {
                    start: 1,
                    step: 1,
                    digits: Digits::Fixed(3),
                },
                ..Fields::default()
            };
            names(&list, &fields, None)[row].name(&list[row]).to_owned()
        };
        assert_eq!(counted("[C]", 4), "f4-005");
        assert_eq!(counted("[C10+5:2]", 2), "f2-20");
        assert_eq!(counted("[C+1/5]", 11), "f11-003");
        assert_eq!(counted("[C-1:1]", 3), "f3--2");
        assert_eq!(counted("[C:a]", 0), "f0-01", "12 rows reach 12");
        assert_eq!(counted("[Caa+1]", 1), "f1-ab");
        assert_eq!(counted("[C99:1]", 0), "f0-99", "too few digits show all");
    }

    /// Range… names the selected characters in the form that inserts
    /// them, counted from the start or from the end.
    #[test]
    fn range_names_the_selected_characters() {
        assert_eq!(
            range('N', "abcdefghij", (2, 6), false).as_deref(),
            Some("[N3-6]")
        );
        assert_eq!(
            range('N', "abcdefghij", (2, 6), true).as_deref(),
            Some("[N-8-5]")
        );
        assert_eq!(range('E', "a😀b", (3, 4), false).as_deref(), Some("[E3]"));
        assert_eq!(range('N', "abc", (1, 1), false), None);
    }

    /// Undo names each entry's old name from the folder it is in now: up
    /// out of folders the batch moved it into, and not up out of a folder
    /// whose name differs only in case. The last rename goes first.
    #[test]
    fn undo_names_the_old_path_from_the_new_one() {
        let renamed = |from: &str, to: &str| Renamed {
            entry: 0,
            from: from.into(),
            to: to.into(),
        };
        let request = undo(&[
            renamed(r"C:\p\z.txt", r"C:\p\new\sub\z.txt"),
            renamed(r"C:\p\a.txt", r"C:\P\b.txt"),
        ]);
        assert_eq!(
            request,
            [
                Rename::new(r"C:\P\b.txt".into(), "a.txt".into()),
                Rename::new(r"C:\p\new\sub\z.txt".into(), r"..\..\z.txt".into()),
            ]
        );
    }

    /// Dates read Date modified in local time, `[T4]` makes the date fields
    /// after it read Date taken, an offset moves it, and an entry without
    /// Date taken keeps its name.
    #[test]
    fn dates_read_their_source() {
        let at: jiff::Timestamp = "2026-03-04T05:06:07.089Z".parse().unwrap();
        let modified = (at.as_millisecond() + 11_644_473_600_000) * 10_000;
        let mut list = Row::of(vec![Entry {
            path: r"C:\a\x".encode_utf16().collect(),
            is_folder: false,
            size: None,
            modified: Some(modified),
        }]);
        let dated = |list: &[Row], mask: &str| {
            let fields = Fields {
                name: mask.into(),
                ..Fields::default()
            };
            names(list, &fields, None).remove(0)
        };
        assert_eq!(dated(&list, "[YMD]-[hms]-[v]"), new("20260304-050607-089"));
        assert_eq!(dated(&list, "[y][M][i][H]"), new("2603am05"));
        assert_eq!(dated(&list, "[T1+60m][h][m]"), new("0606"));
        assert_eq!(
            dated(&list, "[T4][YMD]"),
            Outcome::Kept(Problem::NoDateTaken)
        );
        list[0].taken = Some(modified - 36_000_000_000);
        assert_eq!(dated(&list, "[T4-1D][D][h]"), new("0304"));
    }
}
