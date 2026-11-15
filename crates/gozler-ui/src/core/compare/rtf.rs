//! The text that the compare window streams into its RichEdit controls, as
//! RTF: one paragraph for each row, in one monospaced font, with the
//! colours of the marks (DESIGN.md: Compare by content). Building it takes
//! the rows and the colours, so the window's worker builds a large one.

use std::fmt::Write as _;
use std::ops::Range;

/// A colour as its red, green and blue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
        if self.rows > 0 {
            self.text.push_str("\\par\n");
        }
        self.rows += 1;
        let mut marks = marks.iter().peekable();
        let mut unit = 0;
        let mut run = String::new();
        let mut run_style = style;
        for c in text.chars() {
            while marks.next_if(|mark| mark.end <= unit).is_some() {}
            let marked = marks.peek().is_some_and(|mark| mark.start <= unit);
            let wanted = if marked { Style::Mark } else { style };
            if wanted != run_style {
                self.push_run(&run, run_style);
                run.clear();
                run_style = wanted;
            }
            run.push(c);
            unit += c.len_utf16();
        }
        self.push_run(&run, run_style);
    }

    fn push_run(&mut self, run: &str, style: Style) {
        if run.is_empty() {
            return;
        }
        let plain = style == Style::Text;
        if !plain {
            let (text, back) = style.colours();
            let _ = write!(self.text, r"{{\cf{text}\highlight{back} ");
        }
        for c in run.chars() {
            match c {
                '\\' => self.text.push_str(r"\\"),
                '{' => self.text.push_str(r"\{"),
                '}' => self.text.push_str(r"\}"),
                '\t' => self.text.push_str(r"\tab "),
                ' '..='~' => self.text.push(c),
                _ => {
                    let mut units = [0; 2];
                    for unit in c.encode_utf16(&mut units) {
                        // RTF writes a code unit as a signed 16-bit number.
                        let _ = write!(self.text, r"\u{}?", *unit as i16);
                    }
                }
            }
        }
        if !plain {
            self.text.push('}');
        }
    }

    pub(crate) fn finish(mut self) -> String {
        self.text.push('}');
        self.text
    }
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
}
