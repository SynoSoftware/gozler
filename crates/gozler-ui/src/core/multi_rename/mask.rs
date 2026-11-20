//! The name and the extension masks: text with placeholders in square
//! brackets, read once for every key the person types and then evaluated
//! for each row (DESIGN.md: Multi-Rename, "The mask").

use super::{Case, Context, Counter, Digits};
use crate::core::text::Numbers;
use gozler_core::output::filetime_to_unix_ms;
use jiff::{Span, Timestamp, Zoned};
use unicode_normalization::UnicodeNormalization;

/// Why a mask cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MaskError {
    /// A bracket that holds no placeholder, as written, such as `[Q]`.
    Unknown(String),
    /// A `[` with no `]` after it.
    Unclosed,
}

/// A mask that was read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mask(Vec<Part>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Text(String),
    Name(Range),
    Extension(Range),
    All(Range),
    Folder(Folder, Range),
    /// `[2-5]`: characters of the whole path.
    Path(Range),
    Counter(CounterMask),
    /// `[c]`, `[c2]`: the rows of the list, padded to the Counter fields'
    /// digits for `[c2]`.
    Rows {
        padded: bool,
    },
    /// `[c3]`: the last value that the Counter fields' counter reaches.
    LastCounter,
    Size,
    Variable {
        /// In uppercase, as the dialog keys its variables.
        name: String,
        form: VariableForm,
        range: Range,
    },
    /// `[X]`, or `[X+]` when `optional`.
    Clipboard {
        optional: bool,
    },
    /// `[u]`: the name is composed before any placeholder reads it.
    Composed,
    Date(Vec<DateField>),
    Source(Source),
    Case(Case),
}

/// How `[%x%…]` writes the value of the variable `x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VariableForm {
    Whole,
    /// `[%x%\]`: the last part of the path.
    LastPart,
    /// `[%x%_]`: `\` replaced by the character, `:` removed.
    Replaced(char),
}

/// Which folder of the path a placeholder names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Folder {
    /// Counted up from the entry: 0 holds it.
    Up(usize),
    /// Counted down from the root: 0 is the drive or the server.
    Down(usize),
}

/// A placeholder's characters, counted in Unicode characters from 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Range {
    #[default]
    Whole,
    /// From `start` to `end`, or to the end of the text.
    Span {
        start: Position,
        end: Option<Position>,
        pad: Option<char>,
    },
    /// `count` characters from `start`.
    Count {
        start: Position,
        count: u32,
        pad: Option<char>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    /// The nth character.
    Front(u32),
    /// The nth character from the end.
    Back(u32),
}

/// `[C…]`: each part it leaves out comes from the Counter fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CounterMask {
    start: Option<Start>,
    step: Option<i64>,
    every: Option<u64>,
    digits: Option<Digits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Start {
    Number(i64),
    /// Letters, such as `aa`, whose count sets the width.
    Letters(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DateField {
    Year,
    ShortYear,
    Month,
    Day,
    Hour,
    Hour12,
    Minute,
    Second,
    Millisecond,
    AmPm,
    AmPmLetter,
    ShortDate,
    ShortTime,
}

/// `[T1…]` to `[T4…]`: which date the date fields after it read, moved by
/// an offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Source {
    origin: Origin,
    offset: Offset,
}

/// The date that date fields read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Origin {
    #[default]
    Modified,
    Taken,
    /// The time the dialog opened.
    Opened,
    /// The time the fields last changed.
    Changed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Offset {
    amount: i64,
    unit: Unit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Unit {
    #[default]
    Seconds,
    Minutes,
    Hours,
    Days,
    Months,
    Years,
}

impl Mask {
    /// Reads `text`. Placeholders are case-sensitive: `[M]` is the month and
    /// `[m]` the minutes.
    pub(crate) fn read(text: &str) -> Result<Mask, MaskError> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        while let Some(at) = rest.find('[') {
            literal.push_str(&rest[..at]);
            rest = &rest[at..];
            if let Some(after) = rest.strip_prefix("[[]") {
                literal.push('[');
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix("[]]") {
                literal.push(']');
                rest = after;
                continue;
            }
            let end = rest.find(']').ok_or(MaskError::Unclosed)?;
            let content = &rest[1..end];
            let part =
                placeholder(content).ok_or_else(|| MaskError::Unknown(rest[..=end].to_owned()))?;
            if let Some(part) = part {
                if !literal.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut literal)));
                }
                parts.push(part);
            }
            rest = &rest[end + 1..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Mask(parts))
    }

    /// Whether a date field reads Date taken, which is read from the disk
    /// only for a mask that needs it.
    pub(crate) fn reads_date_taken(&self) -> bool {
        let mut taken = false;
        for part in &self.0 {
            match part {
                Part::Source(source) => taken = source.origin == Origin::Taken,
                Part::Date(_) if taken => return true,
                _ => {}
            }
        }
        false
    }

    /// Whether a placeholder reads the clipboard, which the dialog reads
    /// only for a mask that needs it.
    pub(crate) fn reads_clipboard(&self) -> bool {
        self.0
            .iter()
            .any(|part| matches!(part, Part::Clipboard { .. }))
    }

    /// Whether a placeholder names a folder of the path.
    pub(super) fn reads_folders(&self) -> bool {
        self.0.iter().any(|part| matches!(part, Part::Folder(..)))
    }

    /// The text this mask makes for `row`, into `out`.
    pub(super) fn evaluate(&self, row: &Row<'_>, out: &mut Output) -> Result<(), Missing> {
        let compose = |text: &str| text.nfc().collect::<String>();
        let composed = (self.0.iter().any(|part| matches!(part, Part::Composed)))
            .then(|| [row.stem, row.extension, row.name].map(compose));
        let [stem, extension, name] = match &composed {
            Some([stem, extension, name]) => [stem.as_str(), extension.as_str(), name.as_str()],
            None => [row.stem, row.extension, row.name],
        };
        let mut source = Source::default();
        for part in &self.0 {
            match part {
                Part::Text(text) => out.push(text),
                Part::Name(range) => out.push(&range.of(stem)),
                Part::Extension(range) => out.push(&range.of(extension)),
                Part::All(range) => out.push(&range.of(name)),
                Part::Folder(folder, range) => {
                    out.push(&range.of(row.folder(*folder).unwrap_or_default()));
                }
                Part::Path(range) => out.push(&range.of(&String::from_utf16_lossy(row.path))),
                Part::Counter(counter) => out.push(&row.counter.text(counter)),
                Part::Rows { padded } => out.push(&row.counter.rows_text(*padded)),
                Part::LastCounter => out.push(&row.counter.last_text()),
                Part::Size => {
                    if let Some(size) = row.size {
                        out.push(&size.to_string());
                    }
                }
                Part::Variable { name, form, range } => {
                    if let Some(value) = row.context.variables.get(name) {
                        out.push(&range.of(&form.apply(value)));
                    }
                }
                Part::Clipboard { optional } => match (row.context.clipboard, *optional) {
                    (Some(text), _) => out.push(&clipboard_text(text)),
                    (None, true) => {}
                    (None, false) => return Err(Missing::Clipboard),
                },
                Part::Composed => {}
                Part::Date(fields) => {
                    let at = row.date(source)?;
                    for field in fields {
                        out.push(&field.text(&at, row.context.formats));
                    }
                }
                Part::Source(new) => source = *new,
                Part::Case(case) => out.case(*case),
            }
        }
        Ok(())
    }
}

/// What a row lacks that a placeholder needs, so it keeps its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    Taken,
    Modified,
    /// `[X]` and the clipboard holds no text.
    Clipboard,
}

/// What a mask reads of one row.
pub(super) struct Row<'a> {
    /// The name without its extension; a folder's whole name.
    pub(super) stem: &'a str,
    /// Without its dot; empty for a folder.
    pub(super) extension: &'a str,
    pub(super) name: &'a str,
    /// The whole path, in UTF-16 units as the entry holds it.
    pub(super) path: &'a [u16],
    pub(super) size: Option<u64>,
    /// The folders that hold the entry, from the root: the drive letter
    /// without its colon or the server first, the folder that holds it last.
    pub(super) folders: &'a [&'a str],
    pub(super) counter: CounterAt<'a>,
    pub(super) modified: Option<i64>,
    pub(super) taken: Option<i64>,
    pub(super) context: &'a Context<'a>,
}

/// The clipboard's text as a name can hold it.
fn clipboard_text(text: &str) -> String {
    text.chars()
        .map(|c| if "*?:|\"<>".contains(c) { '_' } else { c })
        .collect()
}

impl VariableForm {
    fn apply(self, value: &str) -> String {
        match self {
            VariableForm::Whole => value.to_owned(),
            VariableForm::LastPart => value
                .trim_end_matches('\\')
                .rsplit('\\')
                .next()
                .unwrap_or_default()
                .to_owned(),
            VariableForm::Replaced(with) => value.replace(':', "").replace('\\', &with.to_string()),
        }
    }
}

impl Row<'_> {
    fn folder(&self, folder: Folder) -> Option<&str> {
        let index = match folder {
            Folder::Up(up) => self.folders.len().checked_sub(up + 1)?,
            Folder::Down(down) => down,
        };
        self.folders.get(index).copied()
    }

    /// The date `source` names. A date that the offset moves out of range
    /// is reported as the row's missing Date modified, or Date taken.
    fn date(&self, source: Source) -> Result<Zoned, Missing> {
        let missing = match source.origin {
            Origin::Taken => Missing::Taken,
            Origin::Modified | Origin::Opened | Origin::Changed => Missing::Modified,
        };
        let from_file = |filetime: Option<i64>| {
            let filetime = filetime.ok_or(missing)?;
            Timestamp::from_millisecond(filetime_to_unix_ms(filetime)).map_err(|_| missing)
        };
        let at = match source.origin {
            Origin::Modified => from_file(self.modified)?,
            Origin::Taken => from_file(self.taken)?,
            Origin::Opened => self.context.opened,
            Origin::Changed => self.context.changed,
        };
        let at = at.to_zoned(self.context.zone.clone());
        source.offset.apply(at).ok_or(missing)
    }
}

/// A row's place in the list, which its counters count from.
#[derive(Clone, Copy)]
pub(super) struct CounterAt<'a> {
    /// The row's position in the list, from 0.
    pub(super) index: u64,
    /// The rows of the list, for Auto digits.
    pub(super) rows: u64,
    pub(super) fields: &'a Counter,
}

impl CounterAt<'_> {
    fn text(&self, mask: &CounterMask) -> String {
        let step = mask.step.unwrap_or(self.fields.step);
        let steps = i64::try_from(self.index / mask.every.unwrap_or(1).max(1)).unwrap_or(i64::MAX);
        match &mask.start {
            Some(Start::Letters(start)) => letters(start, step.saturating_mul(steps)),
            Some(Start::Number(start)) => self.number(*start, step, steps, mask),
            None => self.number(self.fields.start, step, steps, mask),
        }
    }

    /// The rows of the list, padded to the Counter fields' digits when
    /// `padded`, where Auto digits are as many as the number needs.
    fn rows_text(&self, padded: bool) -> String {
        match (padded, self.fields.digits) {
            (true, Digits::Fixed(digits)) => {
                format!("{:0>width$}", self.rows, width = usize::from(digits))
            }
            _ => self.rows.to_string(),
        }
    }

    /// The last value of the Counter fields' counter, padded as `[C]` pads.
    fn last_text(&self) -> String {
        let steps = i64::try_from(self.rows.saturating_sub(1)).unwrap_or(i64::MAX);
        let fields = self.fields;
        self.number(fields.start, fields.step, steps, &CounterMask::default())
    }

    fn number(&self, start: i64, step: i64, steps: i64, mask: &CounterMask) -> String {
        let value = start.saturating_add(step.saturating_mul(steps));
        let width = match mask.digits.unwrap_or(self.fields.digits) {
            Digits::Fixed(digits) => usize::from(digits),
            Digits::Auto => {
                let every = mask.every.unwrap_or(1).max(1);
                let last_steps =
                    i64::try_from(self.rows.saturating_sub(1) / every).unwrap_or(i64::MAX);
                let last = start.saturating_add(step.saturating_mul(last_steps));
                width_of(start).max(width_of(last))
            }
        };
        let digits = value.unsigned_abs().to_string();
        let sign = if value < 0 { "-" } else { "" };
        format!("{sign}{digits:0>width$}")
    }
}

fn width_of(value: i64) -> usize {
    value.unsigned_abs().to_string().len()
}

/// The letters `index` steps after `start`, as `aa`, `ab` … `az`, `ba`, in
/// the case of `start`'s first letter, never fewer than `start` has.
fn letters(start: &str, steps: i64) -> String {
    let upper = start.starts_with(|c: char| c.is_ascii_uppercase());
    let first = start.bytes().fold(0i64, |sum, letter| {
        sum.saturating_mul(26)
            .saturating_add(i64::from(letter.to_ascii_lowercase() - b'a'))
    });
    let mut value = first.saturating_add(steps).max(0);
    let mut out = Vec::new();
    while value > 0 || out.len() < start.len() {
        out.push(b'a' + (value % 26) as u8);
        value /= 26;
    }
    out.reverse();
    let text = String::from_utf8(out).expect("letters are ASCII");
    match upper {
        true => text.to_ascii_uppercase(),
        false => text,
    }
}

impl Range {
    /// The characters of `text` this range names. A range past the end
    /// gives the characters that exist, and nothing when there are none.
    fn of(&self, text: &str) -> String {
        let (start, end, pad) = match *self {
            Range::Whole => return text.to_owned(),
            Range::Span { start, end, pad } => (start, end, pad),
            Range::Count { start, count, pad } => {
                let length = text.chars().count() as i64;
                let from = start.index(length);
                let found = slice(text, from, from + i64::from(count) - 1);
                return padded(found, pad, Some(count as usize));
            }
        };
        let length = text.chars().count() as i64;
        let from = start.index(length);
        let to = end.map_or(length, |end| end.index(length));
        let width = match (start, end) {
            (Position::Front(a), Some(Position::Front(b))) => b.checked_sub(a).map(|w| w + 1),
            (Position::Back(a), Some(Position::Back(b))) => a.checked_sub(b).map(|w| w + 1),
            _ => None,
        };
        padded(slice(text, from, to), pad, width.map(|w| w as usize))
    }
}

impl Position {
    /// The position from 1 in a text of `length` characters; 0 or less
    /// before its start.
    fn index(self, length: i64) -> i64 {
        match self {
            Position::Front(n) => i64::from(n),
            Position::Back(n) => length - i64::from(n) + 1,
        }
    }
}

/// The characters of `text` at positions `from` to `to`, from 1, that
/// exist.
fn slice(text: &str, from: i64, to: i64) -> String {
    let from = from.max(1);
    if to < from {
        return String::new();
    }
    text.chars()
        .skip((from - 1) as usize)
        .take((to - from + 1) as usize)
        .collect()
}

fn padded(text: String, pad: Option<char>, width: Option<usize>) -> String {
    match (pad, width) {
        (Some(pad), Some(width)) => {
            let missing = width.saturating_sub(text.chars().count());
            std::iter::repeat_n(pad, missing)
                .chain(text.chars())
                .collect()
        }
        _ => text,
    }
}

impl DateField {
    fn of(letter: char) -> Option<DateField> {
        Some(match letter {
            'Y' => DateField::Year,
            'y' => DateField::ShortYear,
            'M' => DateField::Month,
            'D' => DateField::Day,
            'h' => DateField::Hour,
            'H' => DateField::Hour12,
            'm' => DateField::Minute,
            's' => DateField::Second,
            'v' => DateField::Millisecond,
            _ => return None,
        })
    }

    fn text(self, at: &Zoned, formats: &dyn Numbers) -> String {
        let hour12 = match at.hour() % 12 {
            0 => 12,
            hour => hour,
        };
        let am = at.hour() < 12;
        match self {
            DateField::Year => format!("{:04}", at.year()),
            DateField::ShortYear => format!("{:02}", at.year().rem_euclid(100)),
            DateField::Month => format!("{:02}", at.month()),
            DateField::Day => format!("{:02}", at.day()),
            DateField::Hour => format!("{:02}", at.hour()),
            DateField::Hour12 => format!("{hour12:02}"),
            DateField::Minute => format!("{:02}", at.minute()),
            DateField::Second => format!("{:02}", at.second()),
            DateField::Millisecond => format!("{:03}", at.millisecond()),
            DateField::AmPm => (if am { "am" } else { "pm" }).to_owned(),
            DateField::AmPmLetter => (if am { "a" } else { "p" }).to_owned(),
            // `/` and `:` cannot be in a name.
            DateField::ShortDate => formats.date(at.date()).replace('/', "-"),
            DateField::ShortTime => formats.time(at.time()).replace(':', "."),
        }
    }
}

impl Offset {
    fn apply(self, at: Zoned) -> Option<Zoned> {
        if self.amount == 0 {
            return Some(at);
        }
        let span = match self.unit {
            Unit::Seconds => Span::new().try_seconds(self.amount),
            Unit::Minutes => Span::new().try_minutes(self.amount),
            Unit::Hours => Span::new().try_hours(self.amount),
            Unit::Days => Span::new().try_days(self.amount),
            Unit::Months => Span::new().try_months(self.amount),
            Unit::Years => Span::new().try_years(self.amount),
        }
        .ok()?;
        at.checked_add(span).ok()
    }
}

/// The part that a bracket's `content` makes: `Some(None)` for one that
/// inserts nothing, `None` for one that is not a placeholder.
fn placeholder(content: &str) -> Option<Option<Part>> {
    let mut chars = content.chars();
    let first = chars.next()?;
    let rest = chars.as_str();
    let part = match first {
        'N' => Part::Name(range(rest)?),
        'E' => Part::Extension(range(rest)?),
        'A' => Part::All(range(rest)?),
        'P' => Part::Folder(Folder::Up(0), range(rest)?),
        'G' => Part::Folder(Folder::Up(1), range(rest)?),
        'B' => {
            let (folder, after) = match rest.strip_prefix('+') {
                Some(down) => (Folder::Down as fn(usize) -> Folder, down),
                None => (Folder::Up as fn(usize) -> Folder, rest),
            };
            let mut after = after.chars();
            let level = after.next()?.to_digit(10)? as usize;
            let after = after.as_str();
            let range = match after.strip_prefix('_') {
                Some(range_text) => range(range_text)?,
                None if after.is_empty() => Range::Whole,
                None => return None,
            };
            Part::Folder(folder(level), range)
        }
        '0'..='9' | '-' | ' ' => Part::Path(range(content)?),
        'C' => Part::Counter(counter(rest)?),
        'c' => match rest {
            "" | "1" => Part::Rows { padded: false },
            "2" => Part::Rows { padded: true },
            "3" => Part::LastCounter,
            _ => return None,
        },
        'S' if rest.is_empty() => Part::Size,
        'X' if matches!(rest, "" | "+") => Part::Clipboard {
            optional: !rest.is_empty(),
        },
        'u' if rest.is_empty() => Part::Composed,
        '%' => variable(rest)?,
        'T' => Part::Source(source(rest)?),
        'I' if matches!(rest, "" | "0" | "1") => return Some(None),
        'U' if rest.is_empty() => Part::Case(Case::Upper),
        'L' if rest.is_empty() => Part::Case(Case::Lower),
        'F' if rest.is_empty() => Part::Case(Case::EachWord),
        'f' if rest.is_empty() => Part::Case(Case::Title),
        'n' if rest.is_empty() => Part::Case(Case::Unchanged),
        'd' if rest.is_empty() => Part::Date(vec![DateField::ShortDate]),
        't' if rest.is_empty() => Part::Date(vec![DateField::ShortTime]),
        'i' if rest.is_empty() => Part::Date(vec![DateField::AmPm]),
        'i' if rest == "1" => Part::Date(vec![DateField::AmPmLetter]),
        _ => Part::Date(content.chars().map(DateField::of).collect::<Option<_>>()?),
    };
    Some(Some(part))
}

/// A range written after a placeholder's letter, such as `2-5` or `-8,5`.
fn range(text: &str) -> Option<Range> {
    if text.is_empty() {
        return Some(Range::Whole);
    }
    let (pad, text) = match text.as_bytes() {
        [b'0', next, ..] if next.is_ascii_digit() || *next == b'-' => (Some('0'), &text[1..]),
        [b' ', ..] => (Some(' '), &text[1..]),
        _ => (None, text),
    };
    let (start, rest) = position(text)?;
    if rest.is_empty() {
        return Some(Range::Span {
            start,
            end: Some(start),
            pad,
        });
    }
    if let Some(count) = rest.strip_prefix(',') {
        return Some(Range::Count {
            start,
            count: count.parse().ok()?,
            pad,
        });
    }
    let end = rest.strip_prefix('-')?;
    if end.is_empty() {
        return Some(Range::Span {
            start,
            end: None,
            pad,
        });
    }
    // After a position from the end, the end counts from the end too;
    // after one from the start, a second `-` makes it count from the end.
    let end = match (start, end.strip_prefix('-')) {
        (Position::Front(_), Some(back)) => Position::Back(number(back)?),
        (Position::Front(_), None) => Position::Front(number(end)?),
        (Position::Back(_), None) => Position::Back(number(end)?),
        (Position::Back(_), Some(_)) => return None,
    };
    Some(Range::Span {
        start,
        end: Some(end),
        pad,
    })
}

/// A position and what follows it.
fn position(text: &str) -> Option<(Position, &str)> {
    let (back, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let n = number(&digits[..end])?;
    let position = match back {
        true => Position::Back(n),
        false => Position::Front(n),
    };
    Some((position, &digits[end..]))
}

/// A position or a count: digits, from 1.
fn number(text: &str) -> Option<u32> {
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|&n| n > 0)
}

/// `[%x%…]` after its first `%`: the closing `%`, then a range, or `\` or
/// a replacement character and then a range. A range is tried first, so a
/// replacement character is never a digit, a `-` that starts a range or a
/// space.
fn variable(text: &str) -> Option<Part> {
    let (name, after) = text.split_once('%')?;
    if name.is_empty() {
        return None;
    }
    let (form, range) = match (range(after), after.chars().next()) {
        (Some(range), _) => (VariableForm::Whole, range),
        (None, Some('\\')) => (VariableForm::LastPart, range(&after[1..])?),
        (None, Some(with)) => (
            VariableForm::Replaced(with),
            range(&after[with.len_utf8()..])?,
        ),
        (None, None) => return None,
    };
    Some(Part::Variable {
        name: name.to_uppercase(),
        form,
        range,
    })
}

/// `[C…]` after its `C`: `10+5:3`, `+1/100`, `aa+1`, `:a`.
fn counter(text: &str) -> Option<CounterMask> {
    let mut mask = CounterMask::default();
    let mut rest = text;
    let letters_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    if letters_end > 0 {
        mask.start = Some(Start::Letters(rest[..letters_end].to_owned()));
        rest = &rest[letters_end..];
    } else {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end > 0 {
            mask.start = Some(Start::Number(rest[..digits_end].parse().ok()?));
            rest = &rest[digits_end..];
        }
    }
    if let Some(sign) = rest.chars().next().filter(|&c| c == '+' || c == '-') {
        let after = &rest[1..];
        let end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        let step: i64 = after[..end].parse().ok()?;
        mask.step = Some(if sign == '-' { -step } else { step });
        rest = &after[end..];
    }
    if let Some(after) = rest.strip_prefix('/') {
        let end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        mask.every = Some(after[..end].parse().ok().filter(|&n| n > 0)?);
        rest = &after[end..];
    }
    if let Some(digits) = rest.strip_prefix(':') {
        mask.digits = Some(match digits {
            "a" => Digits::Auto,
            _ => Digits::Fixed(digits.parse().ok().filter(|n| (1..=10).contains(n))?),
        });
        rest = "";
    }
    rest.is_empty().then_some(mask)
}

/// `[T…]` after its `T`: `1` to `4`, and an offset such as `+60m`.
fn source(text: &str) -> Option<Source> {
    let mut chars = text.chars();
    let origin = match chars.next()? {
        '1' => Origin::Modified,
        '2' => Origin::Opened,
        '3' => Origin::Changed,
        '4' => Origin::Taken,
        _ => return None,
    };
    let rest = chars.as_str();
    if rest.is_empty() {
        return Some(Source {
            origin,
            offset: Offset::default(),
        });
    }
    let sign = match rest.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &rest[1..];
    let unit = rest.chars().last()?;
    let amount: i64 = rest[..rest.len() - unit.len_utf8()].parse().ok()?;
    let unit = match unit {
        's' => Unit::Seconds,
        'm' => Unit::Minutes,
        'h' => Unit::Hours,
        'D' => Unit::Days,
        'M' => Unit::Months,
        'Y' => Unit::Years,
        _ => return None,
    };
    Some(Source {
        origin,
        offset: Offset {
            amount: sign * amount,
            unit,
        },
    })
}

/// A mask's text with the case placeholders that apply to each part.
#[derive(Default)]
pub(super) struct Output {
    pub(super) text: String,
    /// From each byte offset on, a case placeholder's case, in order.
    pub(super) cases: Vec<(usize, Case)>,
}

impl Output {
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn case(&mut self, case: Case) {
        self.cases.push((self.text.len(), case));
    }

    /// The text with each case placeholder applied from where it stands.
    /// Words are found in the whole text, so a word that two parts make is
    /// one word.
    pub(super) fn cased(self) -> String {
        if self.cases.is_empty() {
            return self.text;
        }
        super::cased(&self.text, |at| {
            let from = self.cases.partition_point(|&(from, _)| from <= at);
            from.checked_sub(1)
                .map_or(Case::Unchanged, |last| self.cases[last].1)
        })
    }
}
