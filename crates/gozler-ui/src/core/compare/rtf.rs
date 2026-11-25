//! The text that the compare window streams into its RichEdit controls, as
//! RTF: one paragraph for each row, in one monospaced font, with the
//! colours of the marks (DESIGN.md: Compare by content). Building it takes
//! the rows and the colours, so the window's worker builds a large one.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// A colour as its red, green and blue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

impl Rgb {
    /// `self` mixed with `other`, `percent` of it.
    pub(crate) fn mixed(self, other: Rgb, percent: u8) -> Rgb {
        let mix = |a: u8, b: u8| {
            let percent = u16::from(percent.min(100));
            ((u16::from(a) * (100 - percent) + u16::from(b) * percent) / 100) as u8
        };
        Rgb(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }
}

/// The colours of a comparison, which the window takes from the system's
/// colours of now, high contrast included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Palette {
    pub(crate) text: Rgb,
    pub(crate) muted: Rgb,
    /// The words that differ.
    pub(crate) mark_text: Rgb,
    pub(crate) mark_back: Rgb,
    /// The line number of a row that differs.
    pub(crate) differs_text: Rgb,
    pub(crate) differs_back: Rgb,
    /// The line-number column of an empty row.
    pub(crate) gap_back: Rgb,
}

/// The font: its height in half points, and the width of eight of its
/// characters in twips, where a tab stops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Font {
    pub(crate) half_points: u32,
    pub(crate) tab_twips: u32,
}

/// How a row's text, or a part of it, shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    Text,
    Muted,
    /// The words that differ.
    Mark,
    /// The line number of a row that differs.
    Differs,
    /// The line-number column of an empty row.
    Gap,
}

impl Style {
    /// Its text colour and its background in the colour table; 0 is the
    /// control's own background.
    fn colours(self) -> (u8, u8) {
        match self {
            Style::Text => (1, 0),
            Style::Muted => (2, 0),
            Style::Mark => (3, 4),
            Style::Differs => (5, 6),
            Style::Gap => (2, 7),
        }
    }
}

/// An RTF document being built, one row at a time.
pub(crate) struct Rtf {
    text: String,
    rows: usize,
}

impl Rtf {
    pub(crate) fn new(palette: &Palette, font: Font) -> Rtf {
        let mut text = String::from(
            r"{\rtf1\ansi\ansicpg1252\deff0\uc1{\fonttbl{\f0\fmodern\fcharset0 Consolas;}}{\colortbl ;",
        );
        for Rgb(red, green, blue) in [
            palette.text,
            palette.muted,
            palette.mark_text,
            palette.mark_back,
            palette.differs_text,
            palette.differs_back,
            palette.gap_back,
        ] {
            let _ = write!(text, r"\red{red}\green{green}\blue{blue};");
        }
        text.push_str(r"}\pard");
        for stop in 1..=64 {
            let _ = write!(text, r"\tx{}", font.tab_twips.saturating_mul(stop));
        }
        let _ = write!(text, r"\plain\f0\fs{}\cf1 ", font.half_points.max(2));
        Rtf { text, rows: 0 }
    }

    /// Adds a row whose text shows as `style`, with the UTF-16 units in
    /// `marks` as [`Style::Mark`].
    pub(crate) fn row(&mut self, text: &str, style: Style, marks: &[Range<usize>]) {
        self.push(&row_rtf(text, style, marks));
    }

    /// Adds a row that [`row_rtf`] wrote.
    pub(crate) fn push(&mut self, row: &str) {
        if self.rows > 0 {
            self.text.push_str("\\par\n");
        }
        self.rows += 1;
        self.text.push_str(row);
    }

    /// The document, which ends without a paragraph mark, so it replaces
    /// a control's rows with as many rows as it holds.
    pub(crate) fn finish(mut self) -> String {
        self.text.push('}');
        self.text
    }
}

/// A row's RTF without the document around it: its text as `style`, with
/// the UTF-16 units in `marks` as [`Style::Mark`].
pub(crate) fn row_rtf(text: &str, style: Style, marks: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut marks = marks.iter().peekable();
    let mut unit = 0;
    let mut run = String::new();
    let mut run_style = style;
    for c in text.chars() {
        while marks.next_if(|mark| mark.end <= unit).is_some() {}
        let marked = marks.peek().is_some_and(|mark| mark.start <= unit);
        let wanted = if marked { Style::Mark } else { style };
        if wanted != run_style {
            push_run(&mut out, &run, run_style);
            run.clear();
            run_style = wanted;
        }
        run.push(c);
        unit += c.len_utf16();
    }
    push_run(&mut out, &run, run_style);
    out
}

fn push_run(out: &mut String, run: &str, style: Style) {
    if run.is_empty() {
        return;
    }
    let plain = style == Style::Text;
    if !plain {
        let (text, back) = style.colours();
        let _ = write!(out, r"{{\cf{text}\highlight{back} ");
    }
    for c in run.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            '\t' => out.push_str(r"\tab "),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0; 2];
                for unit in c.encode_utf16(&mut units) {
                    // RTF writes a code unit as a signed 16-bit number.
                    let _ = write!(out, r"\u{}?", *unit as i16);
                }
            }
        }
    }
    if !plain {
        out.push('}');
    }
}

/// The fingerprint of a row that a control holds and nobody knows, such as
/// one the person typed in; it matches no row.
pub(crate) const UNKNOWN: u64 = 0;

/// About the most bytes of RTF in a slice of a [`Patch`], which the
/// window's thread streams in one message. 64 KB of hex rows that differ in every byte take
/// about 40 ms; 16 KB slices made no message shorter and the load twice as
/// long.
const SLICE_BYTES: usize = 64 << 10;

/// The most bytes of RTF of a patch that keeps rows, which streams in one
/// message, so an edit or a check box keeps the view where it is. A larger
/// change replaces all the rows and streams in slices instead.
const KEPT_BYTES: usize = 4 * SLICE_BYTES;

/// The rows of a control that a patch replaces: its rows in `rows` give way
/// to the rows of `slices`, which hold one row or more.
#[derive(Debug)]
pub(crate) struct Patch {
    pub(crate) rows: Range<usize>,
    /// `rows` are all the rows the control held. Only such a patch has
    /// more than one slice, so the rows of a later slice always go below
    /// all the rows the control holds, and move none of them; a patch that
    /// keeps rows is one document of at most [`KEPT_BYTES`].
    pub(crate) whole: bool,
    /// Its rows as documents of their own. The first replaces `rows`; each
    /// later one replaces the last row of the one before, with which it
    /// starts, because a document replaces a row or more.
    pub(crate) slices: Vec<String>,
}

/// Turns a control's rows into `rows`, each written by [`row_rtf`], in
/// `palette` and `font`. `shown` holds the fingerprints of the control's
/// rows; the answer holds those of `rows`, and the patch that streams only
/// the rows that changed, or `None` when the control shows `rows` already.
/// A control always holds a row, so no rows show as one empty row.
pub(crate) fn patch(
    shown: &[u64],
    rows: &[String],
    palette: &Palette,
    font: Font,
) -> (Vec<u64>, Option<Patch>) {
    let empty = [String::new()];
    let rows = if rows.is_empty() { &empty[..] } else { rows };
    let shown = if shown.is_empty() {
        &[UNKNOWN][..]
    } else {
        shown
    };
    // The colours and the font are in the document's header, so a row
    // looks different in other ones.
    let look = {
        let mut hasher = DefaultHasher::new();
        (palette, font).hash(&mut hasher);
        hasher.finish()
    };
    let prints: Vec<u64> = (rows.iter())
        .map(|row| {
            let mut hasher = DefaultHasher::new();
            (look, row).hash(&mut hasher);
            hasher.finish().max(UNKNOWN + 1)
        })
        .collect();
    let same = |a: &u64, b: &u64| a == b && *a != UNKNOWN;
    let shortest = shown.len().min(prints.len());
    let mut before = (shown.iter().zip(&prints))
        .take_while(|(a, b)| same(a, b))
        .count();
    let mut after = (shown.iter().rev().zip(prints.iter().rev()))
        .take_while(|(a, b)| same(a, b))
        .count()
        .min(shortest - before);
    if before == shown.len() && before == prints.len() {
        return (prints, None);
    }
    // The patch replaces a row or more with a row or more, because a
    // document that ends with a paragraph mark would add a row.
    if before == shown.len() - after || before == prints.len() - after {
        if before > 0 {
            before -= 1;
        } else {
            after -= 1;
        }
    }
    let size: usize = rows[before..rows.len() - after]
        .iter()
        .map(String::len)
        .sum();
    if size > KEPT_BYTES {
        (before, after) = (0, 0);
    }
    let whole = before == 0 && after == 0;
    let slices = if whole {
        slice(rows, palette, font)
    } else {
        let mut rtf = Rtf::new(palette, font);
        for row in &rows[before..rows.len() - after] {
            rtf.push(row);
        }
        vec![rtf.finish()]
    };
    let patch = Patch {
        rows: before..shown.len() - after,
        whole,
        slices,
    };
    (prints, Some(patch))
}

/// `rows` as slices of about [`SLICE_BYTES`], each after the first starting
/// again with the last row of the one before.
fn slice(rows: &[String], palette: &Palette, font: Font) -> Vec<String> {
    let mut slices = Vec::new();
    // The rows that the slices so far hold.
    let mut end = 0;
    while end < rows.len() {
        let mut rtf = Rtf::new(palette, font);
        let mut to = end.saturating_sub(1);
        while to < rows.len() && (to <= end || rtf.text.len() < SLICE_BYTES) {
            rtf.push(&rows[to]);
            to += 1;
        }
        slices.push(rtf.finish());
        end = to;
    }
    slices
}

#[cfg(test)]
#[allow(
    clippy::single_range_in_vec_init,
    reason = "marks are a list of ranges"
)]
mod tests {
    use super::*;

    /// Marks colour only their units, RTF's special characters are
    /// escaped, and a character beyond ASCII is written as its code units.
    #[test]
    fn rows_escape_their_text_and_colour_their_marks() {
        let mut rtf = Rtf::new(&Palette::default(), Font::default());
        rtf.row("a{b}", Style::Text, &[1..2]);
        rtf.row("\u{e9}\u{1F600}", Style::Muted, &[]);
        let text = rtf.finish();
        assert!(text.contains(r"a{\cf3\highlight4 \{}b\}"), "{text}");
        assert!(
            text.contains("\\par\n{\\cf2\\highlight0 \\u233?\\u-10179?\\u-8704?}"),
            "{text}"
        );
        assert!(text.ends_with('}'));
        assert_eq!(Rgb(0, 0, 0).mixed(Rgb(200, 100, 0), 50), Rgb(100, 50, 0));
    }

    /// A patch replaces only the rows that changed, and at least one row
    /// with at least one row, so the control ends with the new rows' count:
    /// an added row takes its neighbour along, and a row the person typed
    /// in streams again.
    #[test]
    fn a_patch_streams_the_rows_that_changed() {
        let rows = |texts: &[&str]| -> Vec<String> {
            texts
                .iter()
                .map(|text| row_rtf(text, Style::Text, &[]))
                .collect()
        };
        let (palette, font) = (Palette::default(), Font::default());
        let (shown, first) = patch(&[], &rows(&["a", "b", "c"]), &palette, font);
        assert_eq!(first.map(|patch| patch.rows), Some(0..1));
        let (_, same) = patch(&shown, &rows(&["a", "b", "c"]), &palette, font);
        assert!(same.is_none());
        let (_, added) = patch(&shown, &rows(&["a", "b", "x", "c"]), &palette, font);
        let added = added.expect("a row was added");
        assert_eq!(added.rows, 1..2);
        let [slice] = added.slices.as_slice() else {
            panic!("a small patch is one slice");
        };
        assert!(slice.ends_with("b\\par\nx}"), "{slice}");
        let (_, removed) = patch(&shown, &rows(&["a", "c"]), &palette, font);
        assert_eq!(removed.map(|patch| patch.rows), Some(0..2));
        let typed = [shown[0], UNKNOWN, shown[2]];
        let (_, again) = patch(&typed, &rows(&["a", "b", "c"]), &palette, font);
        assert_eq!(again.map(|patch| patch.rows), Some(1..2));
    }

    /// A large patch streams in slices, each after the first starting again
    /// with the last row of the one before, so the control ends with every
    /// row once. Rows that change in the middle stream in one document
    /// while they are few, so an edit keeps the view, and replace all the
    /// rows when they are many, so no slice goes above rows the control
    /// keeps, which would move the caret and the view.
    #[test]
    fn a_large_patch_streams_in_slices_that_overlap_by_a_row() {
        let (palette, font) = (Palette::default(), Font::default());
        let rows: Vec<String> = (0..20_000)
            .map(|n| row_rtf(&format!("row {n:05} of the file"), Style::Text, &[]))
            .collect();
        let (shown, sliced) = patch(&[], &rows, &palette, font);
        let slices = sliced.expect("no row showed").slices;
        assert!(slices.len() > 1);
        let row = |n: usize| format!("row {n:05} of");
        let mut first = 0;
        for slice in &slices {
            assert!(slice.contains(&row(first)));
            assert!(first == 0 || !slice.contains(&row(first - 1)));
            let last: usize = slice.rsplit("row ").next().expect("a row")[..5]
                .parse()
                .expect("the row's number");
            assert!(last > first);
            first = last;
        }
        assert_eq!(first + 1, rows.len());

        let changed = |rows_changed: std::ops::Range<usize>| {
            let mut changed = rows.clone();
            for (n, row) in changed[rows_changed].iter_mut().enumerate() {
                *row = row_rtf(&format!("changed row {n:05}"), Style::Text, &[]);
            }
            patch(&shown, &changed, &palette, font)
                .1
                .expect("rows changed")
        };
        let few = changed(10..1_010);
        assert!(!few.whole);
        assert_eq!(few.rows, 10..1_010);
        assert_eq!(few.slices.len(), 1);
        let many = changed(10..19_990);
        assert!(many.whole);
        assert_eq!(many.rows, 0..rows.len());
        assert!(many.slices.len() > 1);
    }
}
