//! Compare by content's decisions (DESIGN.md: Compare by content) that need
//! neither a window nor a disk: which two files, reading a text file into
//! lines and writing it back, matching the lines and the words that differ,
//! the rows that show, and the edits of edit mode with their undo.
//!
//! The bytes are untrusted: no function panics on any input. The compare
//! window's worker calls the functions that take whole files; the window
//! calls those that take a row or a pane.

pub(crate) mod diff;
pub(crate) mod rtf;

use super::open::Selected;
use super::panel::Side;
use super::viewer::{
    CodePage, CodePageText, Encoding, HEX_ROW_BYTES, byte_order_mark, chosen_encoding,
    decode_piece, detect_encoding, hex_columns, is_word,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

/// Lines are compared for files up to this many bytes; larger files are
/// compared as Hex, which needs no memory for the file.
pub(crate) const LINES_LIMIT: u64 = 32 << 20;

/// And for files of up to this many lines, because each line costs the
/// window memory of its own.
pub(crate) const LINE_COUNT_LIMIT: usize = 1_000_000;

/// Edit mode works for files up to this many bytes, because each edit
/// reads a side's whole text back from its control.
pub(crate) const EDIT_LIMIT: u64 = 4 << 20;

/// The most rows that show: a RichEdit holds every row in memory and lays
/// each one out.
pub(crate) const SHOWN_LIMIT: usize = 200_000;

/// The rows around a difference that Show only differences keeps.
pub(crate) const DEFAULT_EXTRA_LINES: u32 = 2;

/// The most extra lines the box takes.
pub(crate) const MAX_EXTRA_LINES: u32 = 999;

/// How lines and words match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Matching {
    /// Off: "A" and "a" are equal.
    pub(crate) case_sensitive: bool,
    /// On: a run of spaces and tabs is one space.
    pub(crate) ignore_spaces: bool,
}

/// Which two files Compare by content compares, by Total Commander's four
/// rules: two or more selected files, the first two; else the selected or
/// focused file of the active panel with the first selected file of the
/// other panel, or else with the file of the same name in the other panel's
/// folder. `None` when that gives fewer than two files.
pub(crate) fn choose_files(
    source: &[Selected],
    source_selected: bool,
    target: &[Selected],
    target_folder: Option<&str>,
) -> Option<[Vec<u16>; 2]> {
    let mut files = source.iter().filter(|entry| !entry.is_folder);
    let first = files.next()?.path.clone();
    if source_selected {
        if let Some(second) = files.next() {
            return Some([first, second.path.clone()]);
        }
        if let Some(other) = target.iter().find(|entry| !entry.is_folder) {
            return Some([first, other.path.clone()]);
        }
    }
    let folder = target_folder?;
    let name_start = first
        .iter()
        .rposition(|&unit| unit == u16::from(b'\\'))
        .map_or(0, |at| at + 1);
    let mut second: Vec<u16> = folder.encode_utf16().collect();
    if second.last() != Some(&u16::from(b'\\')) {
        second.push(u16::from(b'\\'));
    }
    second.extend_from_slice(&first[name_start..]);
    (second != first).then_some([first, second])
}

/// The line end a file is written with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineEnd {
    CrLf,
    Lf,
    Cr,
}

impl LineEnd {
    fn text(self) -> &'static str {
        match self {
            LineEnd::CrLf => "\r\n",
            LineEnd::Lf => "\n",
            LineEnd::Cr => "\r",
        }
    }
}

/// How a text file was written, which Save writes again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Form {
    pub(crate) encoding: Encoding,
    pub(crate) byte_order_mark: bool,
    /// The line end that most of its lines had.
    pub(crate) line_end: LineEnd,
    /// Whether its last line ended with a line end.
    pub(crate) final_line_end: bool,
}

/// A text file as its lines, without their line ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Text {
    pub(crate) lines: Vec<String>,
    pub(crate) form: Form,
    /// Whether edit mode may change it: its text writes back to the same
    /// bytes, and it has no character that a RichEdit takes as a line
    /// break, which shows as �.
    pub(crate) editable: bool,
}

/// Bytes of a code page as Windows encodes a text; `None` when the code
/// page cannot hold one of its characters.
pub(crate) type CodePageBytes = dyn Fn(CodePage, &str) -> Option<Vec<u8>>;

/// Reads `bytes`, a whole file, as text: in `chosen`, or else in the
/// encoding that the Viewer detects; `None` when it has more lines than
/// [`LINE_COUNT_LIMIT`], which are counted before they are made. `code_page`
/// decodes and `encode` encodes a code page, which only Windows does.
pub(crate) fn read_text(
    bytes: &[u8],
    chosen: Option<Encoding>,
    code_page: &CodePageText,
    encode: &CodePageBytes,
) -> Option<Text> {
    let mark = byte_order_mark(bytes);
    let detected = match chosen {
        Some(encoding) => chosen_encoding(mark, encoding),
        None => detect_encoding(bytes),
    };
    let start = usize::try_from(detected.data_start)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let decoded = decode_piece(detected.encoding, &bytes[start..], code_page);
    if line_count(&decoded) > LINE_COUNT_LIMIT {
        return None;
    }
    let (mut lines, line_end, final_line_end) = split_lines(&decoded);
    let form = Form {
        encoding: detected.encoding,
        byte_order_mark: start > 0,
        line_end,
        final_line_end,
    };
    let mut breaks = false;
    for line in &mut lines {
        if line.contains(breaks_line) {
            breaks = true;
            *line = line.replace(breaks_line, "\u{FFFD}");
        }
    }
    let editable = !breaks
        && bytes.len() as u64 <= EDIT_LIMIT
        && encoded(&lines, form, encode).as_deref() == Some(bytes);
    Some(Text {
        lines,
        form,
        editable,
    })
}

/// How many lines [`split_lines`] makes of `text`.
fn line_count(text: &str) -> usize {
    let bytes = text.as_bytes();
    let ends = (0..bytes.len())
        .filter(|&i| match bytes[i] {
            b'\n' => true,
            b'\r' => bytes.get(i + 1) != Some(&b'\n'),
            _ => false,
        })
        .count();
    let unended = !bytes.is_empty() && !matches!(bytes.last(), Some(b'\n' | b'\r'));
    ends + usize::from(unended)
}

/// A character that a RichEdit takes as a line break, or cannot hold,
/// which would put the two sides' rows out of line.
fn breaks_line(c: char) -> bool {
    matches!(c, '\0' | '\u{0B}' | '\u{0C}' | '\u{2028}' | '\u{2029}')
}

/// `text`'s lines, split at CR LF, LF and CR; the line end that most of
/// them had, CR LF when none had one or it ties with another, and LF when
/// LF and CR tie; and whether the last line had one.
pub(crate) fn split_lines(text: &str) -> (Vec<String>, LineEnd, bool) {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            // ASCII bytes never stand inside a UTF-8 character, so each
            // split lies between characters.
            b'\r' => {
                lines.push(text[start..i].to_owned());
                if bytes.get(i + 1) == Some(&b'\n') {
                    crlf += 1;
                    i += 2;
                } else {
                    cr += 1;
                    i += 1;
                }
                start = i;
            }
            b'\n' => {
                lines.push(text[start..i].to_owned());
                lf += 1;
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    let final_line_end = !text.is_empty() && start == text.len();
    if start < text.len() {
        lines.push(text[start..].to_owned());
    }
    let line_end = if lf > crlf && lf >= cr {
        LineEnd::Lf
    } else if cr > crlf && cr > lf {
        LineEnd::Cr
    } else {
        LineEnd::CrLf
    };
    (lines, line_end, final_line_end)
}

/// The bytes that Save writes for `lines` in `form`; `None` when the code
/// page cannot hold one of their characters.
pub(crate) fn encoded(lines: &[String], form: Form, encode: &CodePageBytes) -> Option<Vec<u8>> {
    let mut text = lines.join(form.line_end.text());
    if form.final_line_end && !lines.is_empty() {
        text.push_str(form.line_end.text());
    }
    let mark = form.byte_order_mark;
    Some(match form.encoding {
        Encoding::Utf8 => {
            let mut bytes = if mark {
                vec![0xEF, 0xBB, 0xBF]
            } else {
                Vec::new()
            };
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let little = form.encoding == Encoding::Utf16Le;
            let unit = |unit: u16| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            };
            let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
            if mark {
                bytes.extend_from_slice(&unit(0xFEFF));
            }
            for each in text.encode_utf16() {
                bytes.extend_from_slice(&unit(each));
            }
            bytes
        }
        Encoding::CodePage(page) => encode(page, &text)?,
    })
}

/// `text` as `matching` compares it.
fn normalized(text: &str, matching: Matching) -> Cow<'_, str> {
    if matching.case_sensitive && !(matching.ignore_spaces && text.contains(is_space)) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if matching.ignore_spaces && is_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
            continue;
        }
        in_space = false;
        if matching.case_sensitive {
            out.push(c);
        } else {
            out.extend(c.to_lowercase());
        }
    }
    Cow::Owned(out)
}

fn is_space(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Keys of `left` and `right` that are equal where `matching` takes their
/// texts as equal.
fn keys<'a>(left: &'a [&'a str], right: &'a [&'a str], matching: Matching) -> [Vec<u32>; 2] {
    let mut table: HashMap<Cow<'a, str>, u32> = HashMap::new();
    let mut key = |text: &'a str| {
        let next = u32::try_from(table.len()).unwrap_or(u32::MAX);
        *table.entry(normalized(text, matching)).or_insert(next)
    };
    let left: Vec<u32> = left.iter().map(|&text| key(text)).collect();
    let right: Vec<u32> = right.iter().map(|&text| key(text)).collect();
    [left, right]
}

/// One row of a text comparison: each side's line, absent on the side that
/// has none there, and whether the two differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) left: Option<u32>,
    pub(crate) right: Option<u32>,
    pub(crate) differs: bool,
}

/// The rows of `left` and `right` matched, with each pair in `anchors`
/// matched by hand (Resync from here); `None` when `cancelled` says so.
pub(crate) fn match_lines(
    left: &[String],
    right: &[String],
    matching: Matching,
    anchors: &[(usize, usize)],
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<Row>> {
    fn texts(lines: &[String]) -> Vec<&str> {
        lines.iter().map(String::as_str).collect()
    }
    let (left_texts, right_texts) = (texts(left), texts(right));
    let [a, b] = keys(&left_texts, &right_texts, matching);
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0, 0);
    let ends = anchors
        .iter()
        .copied()
        .filter(|&(x, y)| x < a.len() && y < b.len())
        .chain([(a.len(), b.len())]);
    for (x, y) in ends {
        if x < i || y < j {
            continue;
        }
        for hunk in diff::diff(&a[i..x], &b[j..y], cancelled)? {
            hunks.push(diff::Hunk {
                a: hunk.a.start + i..hunk.a.end + i,
                b: hunk.b.start + j..hunk.b.end + j,
            });
        }
        // The anchor's two lines pair, whether or not they are equal.
        if x < a.len() && y < b.len() && a[x] != b[y] {
            hunks.push(diff::Hunk {
                a: x..x + 1,
                b: y..y + 1,
            });
        }
        (i, j) = (x + 1, y + 1);
    }
    Some(rows_of(&hunks, a.len(), b.len()))
}

/// The rows that `hunks` give: equal lines pair; within a hunk the lines
/// pair in order, and the longer side's other lines face empty rows.
fn rows_of(hunks: &[diff::Hunk], left_len: usize, right_len: usize) -> Vec<Row> {
    let line = |index: usize| u32::try_from(index).ok();
    let mut rows = Vec::with_capacity(left_len.max(right_len));
    let (mut i, mut j) = (0, 0);
    // The empty hunk at the ends adds the equal lines after the last one.
    let ends = (hunks.iter().map(|hunk| (hunk.a.clone(), hunk.b.clone())))
        .chain([(left_len..left_len, right_len..right_len)]);
    for (a, b) in ends {
        while i < a.start && j < b.start {
            rows.push(Row {
                left: line(i),
                right: line(j),
                differs: false,
            });
            i += 1;
            j += 1;
        }
        for k in 0..a.len().max(b.len()) {
            rows.push(Row {
                left: if k < a.len() { line(a.start + k) } else { None },
                right: if k < b.len() { line(b.start + k) } else { None },
                differs: true,
            });
        }
        (i, j) = (a.end, b.end);
    }
    rows
}

/// How many differences `differs` holds: runs of rows that differ.
pub(crate) fn count_differences(differs: impl Iterator<Item = bool>) -> usize {
    let mut count = 0;
    let mut previous = false;
    for each in differs {
        if each && !previous {
            count += 1;
        }
        previous = each;
    }
    count
}

/// The longest line, in bytes, whose words [`word_marks`] matches. Their
/// match takes memory and time in proportion to the line, and the window's
/// thread also asks for it, so the one line of a minified file of 32 MB
/// would stall the window.
const WORD_MARKS_LIMIT: usize = 64 * 1024;

/// The marks of a row that differs, the same in the sides and in the
/// two-line list: the words that differ when both sides have a line, else
/// each side's line whole.
pub(crate) fn row_marks(lines: [Option<&str>; 2], matching: Matching) -> [Vec<Range<usize>>; 2] {
    match lines {
        [Some(left), Some(right)] => word_marks(left, right, matching),
        _ => lines.map(|line| line.map_or_else(Vec::new, whole)),
    }
}

/// `line` marked whole, as UTF-16 units; nothing for an empty line.
#[allow(
    clippy::single_range_in_vec_init,
    reason = "marks are a list of ranges"
)]
fn whole(line: &str) -> Vec<Range<usize>> {
    match line.encode_utf16().count() {
        0 => Vec::new(),
        units => vec![0..units],
    }
}

/// The parts of the lines `left` and `right` that differ, word by word, as
/// ranges of UTF-16 units of each line: runs of word characters, runs of
/// spaces and tabs, and each other character are the words. Lines longer
/// than [`WORD_MARKS_LIMIT`] are marked whole.
fn word_marks<'a>(left: &'a str, right: &'a str, matching: Matching) -> [Vec<Range<usize>>; 2] {
    if left.len().max(right.len()) > WORD_MARKS_LIMIT {
        return [left, right].map(whole);
    }
    let (left_words, right_words) = (words(left), words(right));
    let texts = |line: &'a str, words: &[Word]| -> Vec<&'a str> {
        words.iter().map(|word| &line[word.bytes.clone()]).collect()
    };
    let (left_texts, right_texts) = (texts(left, &left_words), texts(right, &right_words));
    let [a, b] = keys(&left_texts, &right_texts, matching);
    let hunks = diff::diff(&a, &b, &|| false).unwrap_or_default();
    let mut marks = [Vec::new(), Vec::new()];
    for hunk in hunks {
        for (side, range, words) in [(0, hunk.a, &left_words), (1, hunk.b, &right_words)] {
            let units = words[range].iter().map(|word| word.units.clone());
            for units in units {
                add_mark(&mut marks[side], units);
            }
        }
    }
    marks
}

/// Adds `range` to `marks`, joined to the last one when the two touch.
fn add_mark(marks: &mut Vec<Range<usize>>, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = marks.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    marks.push(range);
}

/// A word of a line, as its byte range and its range of UTF-16 units.
struct Word {
    bytes: Range<usize>,
    units: Range<usize>,
}

fn words(line: &str) -> Vec<Word> {
    #[derive(PartialEq)]
    enum Kind {
        Word,
        Space,
        Other,
    }
    let kind = |c: char| {
        if is_word(c) {
            Kind::Word
        } else if is_space(c) {
            Kind::Space
        } else {
            Kind::Other
        }
    };
    let mut words: Vec<Word> = Vec::new();
    let mut previous: Option<Kind> = None;
    let mut unit = 0;
    for (at, c) in line.char_indices() {
        let this = kind(c);
        let width = c.len_utf16();
        match words.last_mut() {
            Some(last) if this != Kind::Other && previous.as_ref() == Some(&this) => {
                last.bytes.end = at + c.len_utf8();
                last.units.end = unit + width;
            }
            _ => words.push(Word {
                bytes: at..at + c.len_utf8(),
                units: unit..unit + width,
            }),
        }
        previous = Some(this);
        unit += width;
    }
    words
}

/// The parts of two hex rows that differ: each byte that differs, or that
/// only one side has, in its hex digits and its character, as
/// [`hex_columns`] places them after an offset of `offset_digits`.
pub(crate) fn hex_marks(
    left: Option<&[u8]>,
    right: Option<&[u8]>,
    offset_digits: usize,
) -> [Vec<Range<usize>>; 2] {
    let mut marks = [Vec::new(), Vec::new()];
    for i in 0..HEX_ROW_BYTES {
        let (a, b) = (
            left.and_then(|bytes| bytes.get(i)),
            right.and_then(|bytes| bytes.get(i)),
        );
        if a == b {
            continue;
        }
        for (side, byte) in [(0, a), (1, b)] {
            if byte.is_some() {
                for columns in hex_columns(offset_digits, i..i + 1) {
                    marks[side].push(columns);
                }
            }
        }
    }
    for side in &mut marks {
        side.sort_by_key(|range| range.start);
    }
    marks
}

/// The bytes of a hex row from its `text`, as the Viewer's hex row writes
/// them after an offset of `offset_digits`, so the two-line list, which has
/// only the text, marks the bytes as [`hex_marks`] does in the sides.
pub(crate) fn hex_row_bytes(text: &str, offset_digits: usize) -> Vec<u8> {
    (0..HEX_ROW_BYTES)
        .map_while(|i| {
            let [digits, _] = hex_columns(offset_digits, i..i + 1);
            u8::from_str_radix(text.get(digits)?, 16).ok()
        })
        .collect()
}

/// What a row of the panes shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// A row of the comparison: each side's line, or its hex row, absent
    /// on the side that has none, and whether the two differ.
    Row {
        left: Option<u64>,
        right: Option<u64>,
        differs: bool,
    },
    /// Rows that Show only differences hides: each side's lines or hex
    /// rows.
    Fold { left: Range<u64>, right: Range<u64> },
}

impl Shown {
    pub(crate) fn differs(&self) -> bool {
        matches!(self, Shown::Row { differs: true, .. })
    }

    /// The side's line or hex row, for a row that has one.
    pub(crate) fn line(&self, side: Side) -> Option<u64> {
        match (self, side) {
            (Shown::Row { left, .. }, Side::Left) => *left,
            (Shown::Row { right, .. }, Side::Right) => *right,
            (Shown::Fold { .. }, _) => None,
        }
    }

    /// How many rows a fold hides.
    pub(crate) fn hidden(&self) -> Option<u64> {
        match self {
            Shown::Fold { left, right } => {
                Some((left.end - left.start).max(right.end - right.start))
            }
            Shown::Row { .. } => None,
        }
    }

    fn swapped(self) -> Shown {
        match self {
            Shown::Row {
                left,
                right,
                differs,
            } => Shown::Row {
                left: right,
                right: left,
                differs,
            },
            Shown::Fold { left, right } => Shown::Fold {
                left: right,
                right: left,
            },
        }
    }
}

/// The rows that show, and why some do not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownRows {
    pub(crate) rows: Vec<Shown>,
    /// Only differences show because all the rows would be more than
    /// [`SHOWN_LIMIT`].
    pub(crate) forced: bool,
    /// The rows stop after [`SHOWN_LIMIT`].
    pub(crate) truncated: bool,
}

impl ShownRows {
    /// The sides swapped, as Swap sides shows them.
    pub(crate) fn swapped(self) -> ShownRows {
        ShownRows {
            rows: self.rows.into_iter().map(Shown::swapped).collect(),
            ..self
        }
    }
}

/// The rows of a text comparison that show: all of them, or with
/// `only_differences` the rows within that many extra rows of a
/// difference, a fold in place of each run of the others. Only differences
/// show when all the rows are more than `limit`, and at most `limit` rows
/// show.
pub(crate) fn shown_rows(rows: &[Row], only_differences: Option<u32>, limit: usize) -> ShownRows {
    let row = |row: &Row| Shown::Row {
        left: row.left.map(u64::from),
        right: row.right.map(u64::from),
        differs: row.differs,
    };
    let forced = only_differences.is_none() && rows.len() > limit;
    let Some(extra) = only_differences.or(forced.then_some(DEFAULT_EXTRA_LINES)) else {
        return ShownRows {
            rows: rows.iter().map(row).collect(),
            ..ShownRows::default()
        };
    };
    let keep = kept_rows(rows.iter().map(|row| row.differs), rows.len(), extra);
    let mut shown = Vec::new();
    // The next line of each side, where a fold starts.
    let (mut next_left, mut next_right) = (0u64, 0u64);
    let mut index = 0;
    let mut truncated = false;
    while index < rows.len() {
        if shown.len() >= limit {
            truncated = true;
            break;
        }
        if keep[index] {
            let each = &rows[index];
            if let Some(line) = each.left {
                next_left = u64::from(line) + 1;
            }
            if let Some(line) = each.right {
                next_right = u64::from(line) + 1;
            }
            shown.push(row(each));
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && !keep[index] {
            index += 1;
        }
        let end_of = |side: fn(&Row) -> Option<u32>, next: u64| {
            rows[start..index]
                .iter()
                .rev()
                .find_map(side)
                .map_or(next, |line| u64::from(line) + 1)
        };
        let (left_end, right_end) = (
            end_of(|row| row.left, next_left),
            end_of(|row| row.right, next_right),
        );
        shown.push(Shown::Fold {
            left: next_left..left_end,
            right: next_right..right_end,
        });
        (next_left, next_right) = (left_end, right_end);
    }
    ShownRows {
        rows: shown,
        forced,
        truncated,
    }
}

/// The rows of a Hex comparison that show, as [`shown_rows`] decides them
/// for lines. `rows` is each side's number of hex rows; `differing` holds
/// the rows that differ, in order, and `complete` says whether it holds
/// all of them, because a scan of large files keeps at most `limit`.
pub(crate) fn hex_shown_rows(
    rows: [u64; 2],
    differing: &[u64],
    complete: bool,
    only_differences: Option<u32>,
    limit: usize,
) -> ShownRows {
    let total = rows[0].max(rows[1]);
    let row = |k: u64, differs: bool| Shown::Row {
        left: (k < rows[0]).then_some(k),
        right: (k < rows[1]).then_some(k),
        differs,
    };
    let mut differing = differing.iter().copied().peekable();
    let forced = only_differences.is_none() && total > limit as u64;
    let Some(extra) = only_differences.or(forced.then_some(DEFAULT_EXTRA_LINES)) else {
        return ShownRows {
            rows: (0..total)
                .map(|k| row(k, differing.next_if_eq(&k).is_some()))
                .collect(),
            ..ShownRows::default()
        };
    };
    let extra = u64::from(extra);
    let mut shown = Vec::new();
    let mut truncated = !complete;
    // The row to place next, and the end of the rows kept around the
    // differences seen so far.
    let (mut k, mut keep_until) = (0u64, 0u64);
    while k < total {
        if shown.len() >= limit {
            truncated = true;
            break;
        }
        if let Some(&next) = differing.peek()
            && next.saturating_sub(extra) <= k
        {
            keep_until = keep_until.max(next.saturating_add(extra + 1));
        }
        if k < keep_until {
            shown.push(row(k, differing.next_if_eq(&k).is_some()));
            k += 1;
        } else {
            let end = differing
                .peek()
                .map_or(total, |&next| next.saturating_sub(extra))
                .clamp(k + 1, total);
            shown.push(Shown::Fold {
                left: k.min(rows[0])..end.min(rows[0]),
                right: k.min(rows[1])..end.min(rows[1]),
            });
            k = end;
        }
    }
    ShownRows {
        rows: shown,
        forced,
        truncated,
    }
}

/// For each of `count` rows, whether it lies within `extra` rows of a row
/// that differs.
fn kept_rows(differs: impl Iterator<Item = bool>, count: usize, extra: u32) -> Vec<bool> {
    let extra = extra as usize;
    let mut keep = vec![false; count];
    // The rows before `kept` are decided, so each row is written once, also
    // with many extra lines around many differing rows.
    let mut kept = 0;
    for (index, _) in differs.enumerate().filter(|(_, differs)| *differs) {
        let start = index.saturating_sub(extra).max(kept);
        let end = index.saturating_add(extra + 1).min(count);
        keep[start..end].fill(true);
        kept = end;
    }
    keep
}

/// The row of `rows` that starts the difference after the one with `row`,
/// or after `row`.
pub(crate) fn next_difference(rows: &[Shown], row: usize) -> Option<usize> {
    let starts = |k: usize| rows[k].differs() && (k == 0 || !rows[k - 1].differs());
    (row + 1..rows.len()).find(|&k| starts(k))
}

/// The row of `rows` that starts the difference before the one with `row`,
/// or before `row`.
pub(crate) fn previous_difference(rows: &[Shown], row: usize) -> Option<usize> {
    let starts = |k: usize| rows[k].differs() && (k == 0 || !rows[k - 1].differs());
    let mut current = row.min(rows.len());
    while current > 0 && current < rows.len() && rows[current].differs() && !starts(current) {
        current -= 1;
    }
    (0..current).rev().find(|&k| starts(k))
}

/// The rows of the difference with `row`, or `row` alone when it does not
/// differ.
pub(crate) fn difference_at(rows: &[Shown], row: usize) -> Range<usize> {
    if row >= rows.len() || !rows[row].differs() {
        return row..row + 1;
    }
    let mut start = row;
    while start > 0 && rows[start - 1].differs() {
        start -= 1;
    }
    let mut end = row + 1;
    while end < rows.len() && rows[end].differs() {
        end += 1;
    }
    start..end
}

/// A change to one side's lines: `removed` at `at` became `inserted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) side: Side,
    pub(crate) at: usize,
    pub(crate) removed: Vec<String>,
    pub(crate) inserted: Vec<String>,
}

impl Change {
    /// Makes the change in `lines`; `false`, and no change, when `lines`
    /// no longer holds what it removes.
    pub(crate) fn apply(&self, lines: &mut Vec<String>) -> bool {
        replace(lines, self.at, &self.removed, self.inserted.clone())
    }

    /// Undoes the change in `lines`, which holds it.
    pub(crate) fn revert(&self, lines: &mut Vec<String>) -> bool {
        replace(lines, self.at, &self.inserted, self.removed.clone())
    }
}

fn replace(lines: &mut Vec<String>, at: usize, old: &[String], new: Vec<String>) -> bool {
    let Some(end) = at.checked_add(old.len()).filter(|end| *end <= lines.len()) else {
        return false;
    };
    if lines[at..end] != *old {
        return false;
    }
    lines.splice(at..end, new);
    true
}

/// Edit mode's changes, which Undo takes back one step at a time. Each
/// change has a number, which only grows, so a save forgets only the
/// changes it wrote. The two sides' lines never depend on each other, so
/// each side's changes are kept and forgotten apart.
///
/// The methods' `writing` argument gives, for each side, the number of the
/// last change that a save that runs writes: those changes are on their way
/// to the disk, so they are neither joined nor taken back.
#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    changes: Vec<(u64, Change)>,
    next: u64,
    /// The number of the last change while typing made it, which more
    /// typing in the same line joins.
    typing: Option<u64>,
}

impl History {
    /// Keeps `change` as a step of its own, as Copy to right is.
    pub(crate) fn record(&mut self, change: Change) {
        self.typing = None;
        self.push(change);
    }

    /// Keeps `change`, which typing made; typing in one line joins the
    /// typing before it, so Undo takes back the line's typing at once.
    pub(crate) fn record_typing(&mut self, change: Change, writing: [Option<u64>; 2]) {
        if let Some((number, last)) = self.changes.last_mut()
            && self.typing == Some(*number)
            && last.side == change.side
            && writing[change.side.index()].is_none_or(|mark| *number > mark)
            && last.at == change.at
            && last.inserted.len() == 1
            && change.removed.len() == 1
            && change.inserted.len() == 1
            && last.inserted == change.removed
        {
            last.inserted = change.inserted;
            return;
        }
        self.typing = Some(self.next);
        self.push(change);
    }

    fn push(&mut self, change: Change) {
        self.changes.push((self.next, change));
        self.next += 1;
    }

    /// Takes the last change back, unless a save writes it.
    pub(crate) fn take_last(&mut self, writing: [Option<u64>; 2]) -> Option<Change> {
        let (number, last) = self.changes.last()?;
        if writing[last.side.index()].is_some_and(|mark| *number <= mark) {
            return None;
        }
        self.changes.pop().map(|(_, change)| change)
    }

    /// Whether Undo can take a change back.
    pub(crate) fn can_take_last(&self, writing: [Option<u64>; 2]) -> bool {
        self.changes.last().is_some_and(|(number, last)| {
            writing[last.side.index()].is_none_or(|mark| *number > mark)
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// How many lines the changes hold, which dropping them takes time for.
    pub(crate) fn lines(&self) -> usize {
        (self.changes.iter())
            .map(|(_, change)| change.removed.len() + change.inserted.len())
            .sum()
    }

    /// The number of `side`'s last change.
    pub(crate) fn last(&self, side: Side) -> Option<u64> {
        (self.changes.iter().rev())
            .find(|(_, change)| change.side == side)
            .map(|(number, _)| *number)
    }

    /// Whether `side` has changes after the one numbered `mark`, or any
    /// without a mark.
    pub(crate) fn changed_after(&self, side: Side, mark: Option<u64>) -> bool {
        self.last(side)
            .is_some_and(|last| mark.is_none_or(|mark| last > mark))
    }

    /// Forgets `side`'s changes up to the one numbered `mark`, which a
    /// save wrote.
    pub(crate) fn forget_through(&mut self, side: Side, mark: u64) {
        self.changes
            .retain(|(number, change)| change.side != side || *number > mark);
    }

    /// Takes out `side`'s changes after the one numbered `mark`, or all of
    /// them without a mark, newest first, as they are reverted.
    pub(crate) fn take_after(&mut self, side: Side, mark: Option<u64>) -> Vec<Change> {
        let mut taken: Vec<Change> = self
            .changes
            .extract_if(.., |(number, change)| {
                change.side == side && mark.is_none_or(|mark| *number > mark)
            })
            .map(|(_, change)| change)
            .collect();
        taken.reverse();
        taken
    }

    /// The sides swapped, as Swap sides swaps the files.
    pub(crate) fn swap(&mut self) {
        for (_, change) in &mut self.changes {
            change.side = change.side.other();
        }
    }
}

/// The change that Copy to right or Copy to left makes: the lines of
/// `from` in `rows` of `shown` replace the lines they are paired with on
/// the other side. `None` when the rows hold no line of either side.
pub(crate) fn copy_change(
    shown: &[Shown],
    rows: Range<usize>,
    from: Side,
    lines: [&[String]; 2],
) -> Option<Change> {
    let to = from.other();
    let source = side_lines(shown, rows.clone(), from)?;
    let target = side_lines(shown, rows.clone(), to)?;
    let (source, target) = match (source, target) {
        (Some(source), Some(target)) => (source, target),
        (Some(source), None) => {
            let at = insertion(shown, rows.start, to);
            (source, at..at)
        }
        (None, Some(target)) => (0..0, target),
        (None, None) => return None,
    };
    let lines_of = |side: Side, range: Range<usize>| -> Option<Vec<String>> {
        lines[side.index()].get(range).map(<[String]>::to_vec)
    };
    let inserted = lines_of(from, source)?;
    let removed = lines_of(to, target.clone())?;
    (inserted != removed).then_some(Change {
        side: to,
        at: target.start,
        removed,
        inserted,
    })
}

/// The lines of `side` that `rows` of `shown` hold, folds included;
/// `Some(None)` when they hold none.
fn side_lines(shown: &[Shown], rows: Range<usize>, side: Side) -> Option<Option<Range<usize>>> {
    let mut range: Option<Range<u64>> = None;
    for each in shown.get(rows)? {
        let lines = match each {
            Shown::Row { .. } => each.line(side).map(|line| line..line + 1),
            Shown::Fold { left, right } => Some(match side {
                Side::Left => left.clone(),
                Side::Right => right.clone(),
            })
            .filter(|lines| !lines.is_empty()),
        };
        if let Some(lines) = lines {
            range = Some(match range {
                Some(range) => range.start.min(lines.start)..range.end.max(lines.end),
                None => lines,
            });
        }
    }
    Some(range.and_then(|range| {
        Some(usize::try_from(range.start).ok()?..usize::try_from(range.end).ok()?)
    }))
}

/// Where a line of `side` goes that is inserted at `row` of `shown`: after
/// the side's last line before that row.
fn insertion(shown: &[Shown], row: usize, side: Side) -> usize {
    shown[..row.min(shown.len())]
        .iter()
        .rev()
        .find_map(|each| match each {
            Shown::Row { .. } => each.line(side).map(|line| line + 1),
            Shown::Fold { left, right } => Some(match side {
                Side::Left => left.end,
                Side::Right => right.end,
            }),
        })
        .and_then(|line| usize::try_from(line).ok())
        .unwrap_or(0)
}

/// One row of a pane in edit mode: a line of its file, an empty row facing
/// the other file's line, or a fold of the file's lines.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PaneRow {
    Line(usize),
    Gap,
    Fold(Range<usize>),
}

/// What one side's pane holds in edit mode, so that a change of the
/// control's text can be read back as a change of the file's lines. The
/// control separates rows with CR, as a RichEdit does.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pane {
    rows: Vec<PaneRow>,
    text: Vec<u16>,
}

/// Why an edit in a pane is taken back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// It reached into the rows that Show only differences hides.
    Fold,
    /// It changed the control's rows but no line, as Delete does that joins
    /// an empty row facing the other file's line, so the rows that show no
    /// longer match the control's.
    Gap,
}

impl Pane {
    /// The pane of `side` for `shown`, whose control holds `text`.
    pub(crate) fn new(shown: &[Shown], side: Side, text: Vec<u16>) -> Pane {
        let index = |line: u64| usize::try_from(line).unwrap_or(usize::MAX);
        let mut rows: Vec<PaneRow> = shown
            .iter()
            .map(|each| match each {
                Shown::Row { .. } => each
                    .line(side)
                    .map_or(PaneRow::Gap, |line| PaneRow::Line(index(line))),
                Shown::Fold { left, right } => {
                    let lines = match side {
                        Side::Left => left,
                        Side::Right => right,
                    };
                    PaneRow::Fold(index(lines.start)..index(lines.end))
                }
            })
            .collect();
        // A control always holds one row, also for two empty files.
        if rows.is_empty() {
            rows.push(PaneRow::Gap);
        }
        Pane { rows, text }
    }

    /// The control's text, as the pane last read it.
    #[cfg(test)]
    pub(crate) fn text(&self) -> &[u16] {
        &self.text
    }

    #[cfg(test)]
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Reads back the person's edit, after which the control holds `text`
    /// with the caret at `caret`, as a change of `side`'s `lines`: the rows
    /// around the caret that changed replace the lines they held. The
    /// caret limits the part that changed, because the same text can come
    /// from removing either of two equal rows. `Ok(None)` when neither a
    /// line nor a row changed.
    pub(crate) fn edited(
        &mut self,
        side: Side,
        lines: &[String],
        text: Vec<u16>,
        caret: usize,
    ) -> Result<Option<Change>, Refused> {
        const CR: u16 = 0x0D;
        // The control breaks a row at a vertical tab, which Shift+Enter
        // types, and at the other characters that `breaks_line` names, so
        // they end a line here too, and the sides' rows stay in line.
        let text: Vec<u16> = (text.into_iter())
            .map(|unit| match char::from_u32(u32::from(unit)) {
                Some('\0') => 0xFFFD,
                Some(c) if breaks_line(c) => CR,
                _ => unit,
            })
            .collect();
        let (old, new) = (&self.text, &text);
        if old == new {
            return Ok(None);
        }
        let shortest = old.len().min(new.len());
        let caret = caret.min(new.len());
        let prefix = old
            .iter()
            .zip(new)
            .take_while(|(a, b)| a == b)
            .count()
            .min(caret);
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
            .min(shortest - prefix)
            .min(new.len() - caret);
        let old_end = old.len() - suffix;
        let new_end = new.len() - suffix;
        // A row is the number of line ends before it, counted without a list
        // of every row's start, because this runs at each key.
        let row_of = |position: usize| old[..position].iter().filter(|unit| **unit == CR).count();
        // The control's rows are the pane's rows; the bound only guards
        // against a control that holds others.
        let end = self.rows.len() - 1;
        let first = row_of(prefix).min(end);
        let last = row_of(old_end).clamp(first, end);
        let row_start = match first.checked_sub(1) {
            None => 0,
            Some(before) => (old.iter().enumerate())
                .filter(|(_, unit)| **unit == CR)
                .nth(before)
                .map_or(old.len(), |(at, _)| at + 1),
        };
        let row_end = |text: &[u16], from: usize| {
            text[from.min(text.len())..]
                .iter()
                .position(|unit| *unit == CR)
                .map_or(text.len(), |at| from + at)
        };
        let new_region = &new[row_start.min(new.len())..row_end(new, new_end)];
        let mut inserted: Vec<String> = new_region
            .split(|unit| *unit == CR)
            .map(String::from_utf16_lossy)
            .collect();
        let replaced = &self.rows[first..=last];
        if replaced.iter().any(|row| matches!(row, PaneRow::Fold(_))) {
            return Err(Refused::Fold);
        }
        // An empty row facing the other file takes an empty line at its end
        // of the change, which otherwise would add a line, and stays an
        // empty row, so the pane keeps one row for each of the control's
        // rows. After Enter at the end of the line above it, both ends are
        // empty and the caret is in the first row: the last row stays the
        // empty one, so what the person types next goes into the new line.
        let empty = |line: Option<&String>| line.is_some_and(String::is_empty);
        let mut gap_before = replaced.first() == Some(&PaneRow::Gap) && empty(inserted.first());
        let mut gap_after = replaced.len() > 1
            && replaced.last() == Some(&PaneRow::Gap)
            && inserted.len() > usize::from(gap_before)
            && empty(inserted.last());
        if gap_before
            && !gap_after
            && inserted.len() > 1
            && empty(inserted.last())
            && caret <= row_end(new, row_start)
        {
            (gap_before, gap_after) = (false, true);
        }
        if gap_before {
            inserted.remove(0);
        }
        if gap_after {
            inserted.pop();
        }
        let held: Vec<usize> = replaced
            .iter()
            .filter_map(|row| match row {
                PaneRow::Line(line) => Some(*line),
                PaneRow::Gap | PaneRow::Fold(_) => None,
            })
            .collect();
        let at = match held.first() {
            Some(first) => *first,
            None => self.rows[..first]
                .iter()
                .rev()
                .find_map(|row| match row {
                    PaneRow::Line(line) => Some(line + 1),
                    PaneRow::Fold(lines) => Some(lines.end),
                    PaneRow::Gap => None,
                })
                .unwrap_or(0),
        };
        let removed = lines
            .get(at..at + held.len())
            .map(<[String]>::to_vec)
            .unwrap_or_default();
        let new_rows: Vec<PaneRow> = (gap_before.then_some(PaneRow::Gap).into_iter())
            .chain((0..inserted.len()).map(|k| PaneRow::Line(at + k)))
            .chain(gap_after.then_some(PaneRow::Gap))
            .collect();
        if removed == inserted {
            if replaced != new_rows.as_slice() {
                return Err(Refused::Gap);
            }
            self.text = text;
            return Ok(None);
        }
        // The rows that replaced them hold the new lines, and the lines
        // after them moved.
        let moved = inserted.len() as isize - held.len() as isize;
        let shift = |line: usize| line.saturating_add_signed(moved);
        for row in &mut self.rows[last + 1..] {
            match row {
                PaneRow::Line(line) => *line = shift(*line),
                PaneRow::Fold(lines) => *lines = shift(lines.start)..shift(lines.end),
                PaneRow::Gap => {}
            }
        }
        self.rows.splice(first..=last, new_rows);
        self.text = text;
        Ok(Some(Change {
            side,
            at,
            removed,
            inserted,
        }))
    }
}

#[cfg(test)]
#[allow(
    clippy::single_range_in_vec_init,
    reason = "marks are a list of ranges"
)]
mod tests {
    use super::*;

    fn latin1(_: CodePage, bytes: &[u8]) -> String {
        bytes.iter().map(|&b| char::from(b)).collect()
    }

    fn latin1_bytes(_: CodePage, text: &str) -> Option<Vec<u8>> {
        text.chars()
            .map(|c| u8::try_from(u32::from(c)).ok())
            .collect()
    }

    fn strings(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    const PLAIN: Matching = Matching {
        case_sensitive: true,
        ignore_spaces: false,
    };

    /// The four rules of Total Commander, folders left out.
    #[test]
    fn two_files_come_from_the_selection_or_the_same_name() {
        let entry = |path: &str, is_folder| Selected {
            path: path.encode_utf16().collect(),
            is_folder,
        };
        let wide = |path: &str| path.encode_utf16().collect::<Vec<u16>>();
        let a = entry(r"C:\x\a.txt", false);
        let b = entry(r"C:\x\b.txt", false);
        let folder = entry(r"C:\x\sub", true);
        let other = entry(r"D:\y\c.txt", false);
        let pick = |source: &[Selected], selected, target: &[Selected]| {
            choose_files(source, selected, target, Some(r"D:\y"))
        };
        assert_eq!(
            pick(
                &[folder.clone(), a.clone(), b.clone()],
                true,
                std::slice::from_ref(&other)
            ),
            Some([wide(r"C:\x\a.txt"), wide(r"C:\x\b.txt")])
        );
        let one = std::slice::from_ref(&a);
        assert_eq!(
            pick(one, true, std::slice::from_ref(&other)),
            Some([wide(r"C:\x\a.txt"), wide(r"D:\y\c.txt")])
        );
        assert_eq!(
            pick(one, false, &[]),
            Some([wide(r"C:\x\a.txt"), wide(r"D:\y\a.txt")])
        );
        assert_eq!(pick(&[folder], true, &[]), None);
        assert_eq!(choose_files(one, false, &[], Some(r"C:\x")), None);
    }

    /// Save writes what the file held: its encoding, byte order mark,
    /// line end and final line end, so a file read and saved unchanged
    /// keeps its bytes.
    #[test]
    fn a_text_reads_and_writes_back_to_the_same_bytes() {
        for bytes in [
            &b"one\r\ntwo\r\n"[..],
            b"one\ntwo",
            b"\xEF\xBB\xBFone\rtwo\r",
            b"\xFF\xFEo\0n\0e\0\n\0",
            b"caf\xE9\r\n",
            b"",
        ] {
            let text = read_text(bytes, None, &latin1, &latin1_bytes).expect("few lines");
            assert!(text.editable, "{bytes:?}");
            assert_eq!(
                encoded(&text.lines, text.form, &latin1_bytes).as_deref(),
                Some(bytes)
            );
        }
        let mixed = read_text(b"a\nb\nc\r\nd", None, &latin1, &latin1_bytes).expect("few lines");
        assert_eq!(mixed.lines, strings(&["a", "b", "c", "d"]));
        assert_eq!(mixed.form.line_end, LineEnd::Lf);
        assert!(!mixed.form.final_line_end);
        // A form feed would break a RichEdit's row, so it shows as � and
        // the file is not edited.
        let feed = read_text(b"a\x0Cb\n", None, &latin1, &latin1_bytes).expect("few lines");
        assert_eq!(feed.lines, strings(&["a\u{FFFD}b"]));
        assert!(!feed.editable);
    }

    #[test]
    fn case_and_repeated_spaces_match_as_the_options_say() {
        let rows = |matching| {
            match_lines(
                &strings(&["Hello  world"]),
                &strings(&["hello world"]),
                matching,
                &[],
                &|| false,
            )
            .expect("nothing cancels")
        };
        assert!(rows(PLAIN)[0].differs);
        let loose = Matching {
            case_sensitive: false,
            ignore_spaces: true,
        };
        assert!(!rows(loose)[0].differs);
    }

    /// The rows pair lines and face an inserted line with an empty row; a
    /// resync pairs the two lines the person chose.
    #[test]
    fn rows_pair_lines_and_a_resync_pairs_the_chosen_ones() {
        let left = strings(&["a", "b", "c"]);
        let right = strings(&["a", "x", "b", "c"]);
        let rows = match_lines(&left, &right, PLAIN, &[], &|| false).expect("nothing cancels");
        let pairs: Vec<_> = rows
            .iter()
            .map(|row| (row.left, row.right, row.differs))
            .collect();
        assert_eq!(
            pairs,
            [
                (Some(0), Some(0), false),
                (None, Some(1), true),
                (Some(1), Some(2), false),
                (Some(2), Some(3), false),
            ]
        );
        assert_eq!(count_differences(rows.iter().map(|row| row.differs)), 1);
        let resynced =
            match_lines(&left, &right, PLAIN, &[(0, 1)], &|| false).expect("nothing cancels");
        assert_eq!((resynced[1].left, resynced[1].right), (Some(0), Some(1)));
        assert!(resynced[1].differs);
    }

    #[test]
    fn only_the_words_that_differ_are_marked() {
        let [left, right] = word_marks("line two", "line 2", PLAIN);
        assert_eq!((left, right), (vec![5..8], vec![5..6]));
        let [left, right] = word_marks("a b c", "a x b c", PLAIN);
        assert_eq!((left, right), (vec![], vec![2..4]));
        // UTF-16 units: the emoji takes two.
        let [left, right] = word_marks("\u{1F600} a", "\u{1F600} b", PLAIN);
        assert_eq!((left, right), (vec![3..4], vec![3..4]));
    }

    /// Show only differences keeps the extra rows around each difference
    /// and folds the others, with the lines each fold hides.
    #[test]
    fn only_differences_fold_the_rows_far_from_a_difference() {
        let left: Vec<String> = (0..10).map(|n| n.to_string()).collect();
        let mut right = left.clone();
        right[5] = "five".into();
        let rows = match_lines(&left, &right, PLAIN, &[], &|| false).expect("nothing cancels");
        let shown = shown_rows(&rows, Some(1), SHOWN_LIMIT);
        assert_eq!(shown.rows.len(), 5);
        assert_eq!(
            shown.rows[0],
            Shown::Fold {
                left: 0..4,
                right: 0..4
            }
        );
        assert!(shown.rows[2].differs());
        assert_eq!(
            shown.rows[4],
            Shown::Fold {
                left: 7..10,
                right: 7..10
            }
        );
        let all = shown_rows(&rows, None, 7);
        assert!(all.forced && !all.truncated);
        let few = shown_rows(&rows, Some(9), 3);
        assert!(few.truncated);
        assert_eq!(few.rows.len(), 3);
        assert_eq!(next_difference(&shown.rows, 0), Some(2));
        assert_eq!(previous_difference(&shown.rows, 4), Some(2));
        assert_eq!(difference_at(&shown.rows, 2), 2..3);
    }

    /// Hex rows fold as lines do, and the rows past the shorter file show
    /// on its side as absent.
    #[test]
    fn hex_rows_fold_far_from_a_difference() {
        let all = hex_shown_rows([3, 4], &[3], true, None, SHOWN_LIMIT);
        assert_eq!(all.rows.len(), 4);
        assert_eq!(
            all.rows[3],
            Shown::Row {
                left: None,
                right: Some(3),
                differs: true
            }
        );
        let folded = hex_shown_rows([10, 10], &[5], true, Some(1), SHOWN_LIMIT);
        assert_eq!(folded.rows.len(), 5);
        assert_eq!(
            folded.rows[0],
            Shown::Fold {
                left: 0..4,
                right: 0..4
            }
        );
        assert!(folded.rows[2].differs());
        assert_eq!(
            folded.rows[4],
            Shown::Fold {
                left: 7..10,
                right: 7..10
            }
        );
        let forced = hex_shown_rows([1 << 40, 1 << 40], &[0], true, None, SHOWN_LIMIT);
        assert!(forced.forced && !forced.truncated);
        assert_eq!(forced.rows.len(), 4);
        let partial = hex_shown_rows([100, 100], &[0], false, Some(0), SHOWN_LIMIT);
        assert!(partial.truncated);
    }

    /// Copy to right replaces the lines paired with the difference, and
    /// inserts them where the other side has none.
    #[test]
    fn copying_a_difference_replaces_the_lines_it_pairs_with() {
        let left = strings(&["a", "b", "c"]);
        let right = strings(&["a", "x", "c"]);
        let rows = match_lines(&left, &right, PLAIN, &[], &|| false).expect("nothing cancels");
        let shown = shown_rows(&rows, None, SHOWN_LIMIT).rows;
        let change = copy_change(&shown, 1..2, Side::Left, [&left, &right]).expect("they differ");
        let mut changed = right.clone();
        assert!(change.apply(&mut changed));
        assert_eq!(changed, left);
        let inserted = strings(&["a", "c"]);
        let rows = match_lines(&left, &inserted, PLAIN, &[], &|| false).expect("nothing cancels");
        let shown = shown_rows(&rows, None, SHOWN_LIMIT).rows;
        let change =
            copy_change(&shown, 1..2, Side::Left, [&left, &inserted]).expect("one side has it");
        assert_eq!((change.at, change.inserted.clone()), (1, strings(&["b"])));
        let mut changed = inserted.clone();
        assert!(change.apply(&mut changed) && change.revert(&mut changed));
        assert_eq!(changed, inserted);
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// Typing, a new line, and removing a line end next to an empty row
    /// read back as the change the person made, and typing in one line is
    /// one step of Undo.
    #[test]
    fn edits_read_back_as_changes_of_the_lines() {
        let lines = strings(&["x", "y"]);
        let shown = vec![
            Shown::Row {
                left: Some(0),
                right: Some(0),
                differs: false,
            },
            Shown::Row {
                left: None,
                right: Some(1),
                differs: true,
            },
            Shown::Row {
                left: Some(1),
                right: Some(2),
                differs: false,
            },
        ];
        let start = units("x\r\ry");
        // Typing "a" in the empty row adds a line there.
        let mut pane = Pane::new(&shown, Side::Left, start.clone());
        let change = pane
            .edited(Side::Left, &lines, units("x\ra\ry"), 3)
            .expect("no fold");
        assert_eq!(
            change,
            Some(Change {
                side: Side::Left,
                at: 1,
                removed: vec![],
                inserted: strings(&["a"])
            })
        );
        // Delete at the end of "x" joins the empty row, which changes no
        // line but the control's rows.
        let mut pane = Pane::new(&shown, Side::Left, start.clone());
        assert_eq!(
            pane.edited(Side::Left, &lines, units("x\ry"), 1),
            Err(Refused::Gap)
        );
        // Enter after "x" adds an empty line after it, and the empty row
        // stays below the caret's row, so typing and the next Enter change
        // that line and never "y" (reported defect).
        let mut pane = Pane::new(&shown, Side::Left, start);
        let mut edit = |lines: &[String], text: &str, caret: usize| {
            let change = pane
                .edited(Side::Left, lines, units(text), caret)
                .expect("no fold")
                .expect("a line changed");
            (change.at, change.removed, change.inserted)
        };
        assert_eq!(edit(&lines, "x\r\r\ry", 2), (1, vec![], strings(&[""])));
        let lines = strings(&["x", "", "y"]);
        assert_eq!(
            edit(&lines, "x\rfoo\r\ry", 5),
            (1, strings(&[""]), strings(&["foo"]))
        );
        let lines = strings(&["x", "foo", "y"]);
        assert_eq!(
            edit(&lines, "x\rfoo\r\r\ry", 6),
            (2, vec![], strings(&[""]))
        );
        // Shift+Enter types a vertical tab, which the control shows as a
        // new row, so it ends a line as Enter does.
        let single = [Shown::Row {
            left: Some(0),
            right: Some(0),
            differs: false,
        }];
        let mut pane = Pane::new(&single, Side::Left, units("ab"));
        let change = pane
            .edited(Side::Left, &strings(&["ab"]), units("a\u{0B}b"), 2)
            .expect("no fold")
            .expect("a line changed");
        assert_eq!((change.at, change.inserted), (0, strings(&["a", "b"])));
        let mut history = History::default();
        let typed = |from: &str, to: &str| Change {
            side: Side::Right,
            at: 4,
            removed: strings(&[from]),
            inserted: strings(&[to]),
        };
        history.record_typing(typed("ab", "abc"), [None, None]);
        history.record_typing(typed("abc", "abcd"), [None, None]);
        assert_eq!(history.take_last([None, None]), Some(typed("ab", "abcd")));
        // Typing after Copy to right is a step of its own (reported defect).
        history.record(typed("ab", "x"));
        history.record_typing(typed("x", "xy"), [None, None]);
        assert_eq!(history.take_last([None, None]), Some(typed("x", "xy")));
        assert_eq!(history.take_last([None, None]), Some(typed("ab", "x")));
        assert!(history.is_empty());
    }

    proptest::proptest! {
        /// Lines from the disk can be anything; reading, matching and
        /// marking them never panics.
        #[test]
        fn reading_and_matching_any_bytes_never_panics(
            a in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            b in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
        ) {
            let left = read_text(&a, Some(Encoding::Utf16Be), &latin1, &latin1_bytes)
                .expect("few lines");
            let right = read_text(&b, None, &latin1, &latin1_bytes).expect("few lines");
            let rows = match_lines(&left.lines, &right.lines, PLAIN, &[(1, 2)], &|| false);
            for row in rows.unwrap_or_default() {
                if let (Some(x), Some(y)) = (row.left, row.right) {
                    let _ = word_marks(&left.lines[x as usize], &right.lines[y as usize], PLAIN);
                }
            }
        }
    }
}
