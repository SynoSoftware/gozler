//! The Viewer window's decisions (DESIGN.md: The Viewer) on top of
//! [`super::viewer`]'s: which blocks of the file it keeps, the first shown row,
//! the page of rows that the window paints, the caret and the selection, the
//! scroll bar, the keys, the menus and the status bar.
//!
//! The window owns the file and its reading thread. It gives a [`Document`]
//! each block that the thread reads, and reads the block that
//! [`Document::advance`] asks for. A page, a move of the page and a move of
//! the caret go on row by row from where they waited for a block, so one that
//! needs more blocks than the Viewer keeps still ends, and reads each block
//! once.

use super::commands::{Key, Keystroke, Mark, MenuNode};
use super::glyph::Glyph;
use super::layout::{Dpi, Px, Rect, Size};
use super::text::{self, Numbers, Strings, menu_label};
use super::viewer::{
    self, CODE_PAGES, CodePage, CodePageText, Detected, Encoding, HEX_ROW_BYTES, LINE_BYTES, Mode,
    Place, SNIFF_BYTES, TextPoint,
};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::{Range, RangeInclusive};
use std::rc::Rc;

/// The bytes the reading thread reads at once. The first block holds the
/// bytes that choose Text or Hex and the encoding.
pub(crate) const BLOCK_BYTES: usize = SNIFF_BYTES;

/// How many of the blocks read last the Viewer keeps.
const KEPT_BLOCKS: usize = 16;

/// The wheel delta of one row: a notch of 120 scrolls three rows, as
/// Windows does by default.
const WHEEL_ROW: i32 = 40;

const HEX_ROW: u64 = HEX_ROW_BYTES as u64;

/// The most UTF-16 units that Copy puts on the clipboard, 64 MB there.
/// Copy holds the whole file and its text in memory at once, so a larger
/// file would take hundreds of megabytes from the machine.
const COPY_UNITS: u64 = 32 << 20;

/// The text size, in percent of the window's message font, that `Ctrl+0`
/// restores.
pub(crate) const DEFAULT_ZOOM: u16 = 100;

/// The text sizes that zoom reaches, in steps of [`ZOOM_STEP`] percent.
const ZOOM: RangeInclusive<u16> = 50..=400;
const ZOOM_STEP: u16 = 10;

/// The text size `steps` steps of zoom from `zoom`, within the sizes zoom
/// reaches; with 0 steps, `zoom` brought into them, as a saved size is.
pub(crate) fn zoomed(zoom: u16, steps: i32) -> u16 {
    let step = i32::from(ZOOM_STEP);
    let size =
        (i32::from(zoom) + steps * step).clamp(i32::from(*ZOOM.start()), i32::from(*ZOOM.end()));
    u16::try_from(size).expect("the zoom range fits a u16")
}

/// A block that the page or a move waits for, by its index.
type Need<T> = Result<T, u64>;

/// The blocks the Viewer keeps, and the file's length.
struct Blocks {
    /// The length when the file opened, or where its bytes ended since.
    len: u64,
    /// The oldest first.
    kept: VecDeque<(u64, Box<[u8]>)>,
}

impl Blocks {
    fn range(&self, index: u64) -> Range<u64> {
        let start = index.saturating_mul(BLOCK_BYTES as u64).min(self.len);
        start..start.saturating_add(BLOCK_BYTES as u64).min(self.len)
    }

    /// Keeps the block `index`, and forgets the oldest beyond
    /// [`KEPT_BLOCKS`]. Fewer bytes than the length promises mean that the
    /// file shrank since it opened, so it ends there; more mean that it
    /// grew, which the Viewer shows after Reload.
    fn keep(&mut self, index: u64, mut bytes: Vec<u8>) {
        let range = self.range(index);
        let expected = (range.end - range.start) as usize;
        if bytes.len() < expected {
            self.len = range.start + bytes.len() as u64;
            let len = self.len;
            self.kept
                .retain(|(i, _)| i.saturating_mul(BLOCK_BYTES as u64) < len);
            for (i, block) in &mut self.kept {
                let room = (len - *i * BLOCK_BYTES as u64).min(BLOCK_BYTES as u64) as usize;
                if block.len() > room {
                    *block = block[..room].into();
                }
            }
        }
        bytes.truncate(expected);
        if bytes.is_empty() {
            return;
        }
        self.kept.retain(|(i, _)| *i != index);
        self.kept.push_back((index, bytes.into_boxed_slice()));
        while self.kept.len() > KEPT_BLOCKS {
            self.kept.pop_front();
        }
    }

    /// The bytes of `range`, cut at the file's end; `Err` names a block
    /// that is not kept.
    fn bytes(&self, range: Range<u64>) -> Need<Cow<'_, [u8]>> {
        let range = range.start.min(self.len)..range.end.min(self.len);
        if range.is_empty() {
            return Ok(Cow::Borrowed(&[]));
        }
        let block_bytes = BLOCK_BYTES as u64;
        let (first, last) = (range.start / block_bytes, (range.end - 1) / block_bytes);
        if first == last {
            return self.part(first, &range).map(Cow::Borrowed);
        }
        let mut joined = Vec::with_capacity((range.end - range.start) as usize);
        for index in first..=last {
            joined.extend_from_slice(self.part(index, &range)?);
        }
        Ok(Cow::Owned(joined))
    }

    /// The bytes of `range`, within the file, that the block `index` holds.
    fn part(&self, index: u64, range: &Range<u64>) -> Need<&[u8]> {
        let block = self
            .kept
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, block)| &**block)
            .ok_or(index)?;
        let base = index * BLOCK_BYTES as u64;
        let from = (range.start.max(base) - base) as usize;
        let to = (range.end.min(base + BLOCK_BYTES as u64) - base) as usize;
        Ok(block
            .get(from..to)
            .expect("a kept block holds its bytes up to the file's end"))
    }
}

/// Where a row starts: the line that holds it, and which of the line's
/// wrapped rows it is. In Hex a line is a row of 16 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowAt {
    line: u64,
    row: usize,
}

/// One row of the page: the row `at.row` of its decoded line.
#[derive(Clone, Debug)]
struct Row {
    at: RowAt,
    line: Rc<LineRows>,
}

impl Row {
    fn text(&self) -> &str {
        &self.line.rows[self.at.row]
    }

    /// The column of the line where the row starts.
    fn start(&self) -> usize {
        self.at.row * self.line.columns
    }

    /// The column of the line after the row's last character.
    fn end(&self) -> usize {
        self.start() + self.text().chars().count()
    }

    /// Whether a line end follows the row: it is the last row of a line
    /// that has one.
    fn breaks(&self) -> bool {
        self.line.broken && self.at.row + 1 == self.line.rows.len()
    }

    /// Which of the `shown` characters of `text` from its character `skip`
    /// lie between `from` and `to`, and the cell after them for a line end
    /// between them, when the row's last character shows.
    fn selected(&self, from: TextPoint, to: TextPoint, skip: usize, shown: usize) -> Range<usize> {
        let line = self.at.line;
        if line < from.line || line > to.line {
            return 0..0;
        }
        let end = self.end();
        let first = if line == from.line {
            viewer::column_of(&self.line.text, from.char)
        } else {
            0
        };
        let last = if line == to.line {
            viewer::column_of(&self.line.text, to.char).min(end)
        } else {
            end + usize::from(self.breaks())
        };
        let left = self.start() + skip;
        let right = left + shown + usize::from(left + shown == end);
        let first = first.clamp(left, right) - left;
        let last = last.clamp(left, right) - left;
        if first < last { first..last } else { 0..0 }
    }
}

/// What follows the page's last row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    Row(RowAt),
    /// The last row is the file's last.
    End,
}

/// A key that moves the page, or the caret while it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Up,
    Down,
    PageUp,
    PageDown,
    Left,
    Right,
    /// `Home` and `End`: the caret's line's start and end, or without the
    /// caret, the file's.
    Home,
    End,
    /// `Ctrl+Home` and `Ctrl+End`.
    FileStart,
    FileEnd,
}

/// The caret in Text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Caret {
    point: TextPoint,
    /// The column within its row that `Up` and `Down` keep while they move
    /// the caret through shorter rows.
    goal: Option<usize>,
}

/// A move of the caret that runs row by row as the kept blocks allow. Only
/// its end writes the caret and the selection, so a move that another
/// request replaces leaves both as they were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaretMove {
    motion: Motion,
    /// The steps still to take: one, or a page's rows for `PgUp` and `PgDn`.
    steps: usize,
    /// The steps taken, by which `PgUp` and `PgDn` scroll the page, so the
    /// caret keeps its row on the screen.
    moved: usize,
    /// Where the selection starts while Shift is held.
    anchor: Option<TextPoint>,
    /// Where the steps taken put the caret.
    caret: Caret,
}

/// What the document does until its page is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    Idle,
    /// Adds rows to the page from [`Document::after`], then, when `reveal`,
    /// scrolls the page to the caret that a move of the caret placed.
    Page {
        reveal: bool,
    },
    /// Moves up by `rows` rows from `at`, then shows the page there as
    /// `Page { reveal }`.
    Up {
        at: RowAt,
        rows: usize,
        reveal: bool,
    },
    /// Shows the page from the first line that starts at or after
    /// `offset`.
    Jump {
        offset: u64,
    },
    /// Moves the caret, then shows the page with the caret on it.
    Caret(CaretMove),
}

/// A decoded line and its rows, which the page's rows of it share.
#[derive(Debug)]
struct LineRows {
    line: u64,
    rows: Vec<String>,
    /// How many characters at the start of each row show in the gray text
    /// colour: a hex row's offset.
    muted: usize,
    /// The characters of each row but the last.
    columns: usize,
    /// The line without its terminator, before its tabs expand; empty in
    /// Hex.
    text: String,
    /// A line end follows the line.
    broken: bool,
}

/// What the Viewer has selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    None,
    /// Select all: the whole file, in Text and in Hex.
    All,
    /// The text from `anchor`, where the button went down or Shift was
    /// first held, to `focus`, where the pointer or the caret is; nothing
    /// while both are the same place.
    Text {
        anchor: TextPoint,
        focus: TextPoint,
    },
    /// The bytes from `start` to `end`, a match of Find in Hex.
    Bytes {
        start: u64,
        end: u64,
    },
}

/// A file as the Viewer shows it.
pub(crate) struct Document {
    blocks: Blocks,
    /// The file's byte order mark, which a chosen encoding skips only when
    /// the mark names it.
    mark: Option<Detected>,
    detected: Detected,
    /// The person chose the encoding, so the file keeps it when it opens
    /// again.
    chosen: bool,
    mode: Mode,
    wrap: bool,
    /// The characters of a row that fit the area's width.
    columns: usize,
    /// The rows that fit the area's height.
    height: usize,
    /// The first shown column while long lines do not wrap.
    left: usize,
    top: RowAt,
    rows: Vec<Row>,
    after: After,
    task: Task,
    decoded: Option<Rc<LineRows>>,
    /// The wheel's turn not yet scrolled, less than a row.
    wheel: i32,
    selection: Selection,
    /// The caret in Text, after `F6` or a move with Shift.
    caret: Option<Caret>,
    /// The bytes of Find's last match and the selection it made: the next
    /// search starts from them while that selection stays.
    found: Option<(Range<u64>, Selection)>,
}

/// What Copy reads, and how [`CopyText::text`] makes the clipboard's text
/// of it.
#[derive(Clone, Debug)]
pub(crate) struct CopyText {
    /// The bytes of the file that Copy reads.
    pub(crate) bytes: Range<u64>,
    copied: Copied,
}

#[derive(Clone, Copy, Debug)]
enum Copied {
    /// Select all: the whole file, with [`viewer::copy_text`].
    File { mode: Mode, detected: Detected },
    /// Part of the text, with [`viewer::text_between`]; `last` when the
    /// bytes reach the file's end.
    Between {
        encoding: Encoding,
        from: TextPoint,
        to: TextPoint,
        last: bool,
    },
    /// Bytes in Hex, with [`viewer::hex_bytes_text`].
    Bytes,
}

impl CopyText {
    /// The clipboard's text of `read`, the bytes of [`CopyText::bytes`]
    /// that the reading thread read: fewer when the file shrank since.
    pub(crate) fn text(&self, read: &[u8], code_page: &CodePageText) -> String {
        match self.copied {
            Copied::File { mode, detected } => viewer::copy_text(mode, detected, read, code_page),
            Copied::Between {
                encoding,
                from,
                to,
                last,
            } => {
                let short = (read.len() as u64) < self.bytes.end - self.bytes.start;
                viewer::text_between(encoding, read, from, to, last || short, code_page)
            }
            Copied::Bytes => viewer::hex_bytes_text(read),
        }
    }
}

/// Why Copy copies nothing: the selection's text could pass
/// [`COPY_UNITS`]. Each holds the most that Copy takes, in bytes of the
/// file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TooLarge {
    /// Select all in Text.
    Text(u64),
    /// Select all in Hex.
    Hex(u64),
    /// Part of the text, or bytes in Hex.
    Selection(u64),
}

/// A shown row: its text from the first shown column, cut after the
/// area's width, how many of its first characters show in gray, and which
/// show selected, up to one past the text for a line end: one range in
/// Text, and in Hex the bytes' digits and their characters.
pub(crate) struct ShownRow<'a> {
    pub(crate) text: &'a str,
    pub(crate) muted: usize,
    pub(crate) selected: [Range<usize>; 2],
}

/// What a search of Find needs of the shown file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FindIn {
    pub(crate) mode: Mode,
    pub(crate) detected: Detected,
    /// The file's length as the Viewer knows it.
    pub(crate) len: u64,
    /// Where the search starts: a forward search finds a match that starts
    /// there or later, a backward one a match that starts before it.
    pub(crate) from: u64,
    pub(crate) backward: bool,
    /// The first byte that a match of text may start at: after the byte
    /// order mark in Text.
    pub(crate) start: u64,
}

/// A match that Find found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) bytes: Range<u64>,
    /// In Text: the places before and after the match, and the column where
    /// it starts once tabs expand.
    pub(crate) text: Option<(TextPoint, usize, TextPoint)>,
}

/// Where a file that opens again shows, as it showed before: after Retry,
/// a removed drive or Reload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reopening {
    mode: Mode,
    offset: u64,
    /// The encoding the person chose, if they chose one.
    encoding: Option<Encoding>,
    /// The page showed the file's end, so it shows the end again, also of a
    /// file that grew, as a log does.
    end: bool,
    /// The selection, which Copy pressed while the file opened copies, and
    /// Find's last match, which Find next goes on from.
    selection: Selection,
    found: Option<(Range<u64>, Selection)>,
}

impl Document {
    /// A file of `len` bytes, whose first block `first` the thread read,
    /// shown as its first bytes choose, from its start.
    pub(crate) fn new(len: u64, first: Vec<u8>, wrap: bool) -> Document {
        let mode = viewer::choose_mode(&first);
        let detected = viewer::detect_encoding(&first);
        let mark = viewer::byte_order_mark(&first);
        let mut blocks = Blocks {
            len,
            kept: VecDeque::new(),
        };
        blocks.keep(0, first);
        let mut document = Document {
            blocks,
            mark,
            detected,
            chosen: false,
            mode,
            wrap,
            columns: 1,
            height: 1,
            left: 0,
            top: RowAt { line: 0, row: 0 },
            rows: Vec::new(),
            after: After::End,
            task: Task::Idle,
            decoded: None,
            wheel: 0,
            selection: Selection::None,
            caret: None,
            found: None,
        };
        document.show_from(document.start_row());
        document
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    pub(crate) fn encoding(&self) -> Encoding {
        self.detected.encoding
    }

    /// Where the file shows when it opens again, as it shows now, or where a
    /// jump that waits for a block shows it.
    pub(crate) fn reopening(&self) -> Reopening {
        let offset = match self.task {
            Task::Jump { offset } => offset,
            _ => self.top.line,
        };
        Reopening {
            mode: self.mode,
            offset,
            encoding: self.chosen.then_some(self.detected.encoding),
            end: self.end_shown(),
            selection: self.selection,
            found: self.found.clone(),
        }
    }

    /// Shows the file as it showed when `at` was taken, once it has read
    /// what that needs, with the same selection.
    pub(crate) fn reopen(&mut self, at: Reopening) {
        if let Some(encoding) = at.encoding {
            self.choose_encoding(encoding);
        }
        self.jump_as(at.mode, at.offset);
        if at.end {
            self.end();
        }
        self.selection = at.selection;
        self.found = at.found;
    }

    /// The first byte of the text: after the byte order mark in Text.
    fn start(&self) -> u64 {
        match self.mode {
            Mode::Text => self.detected.data_start.min(self.blocks.len),
            Mode::Hex => 0,
        }
    }

    fn start_row(&self) -> RowAt {
        RowAt {
            line: self.start(),
            row: 0,
        }
    }

    /// Where a row after the file's last would start, from which End
    /// counts back.
    fn end_row(&self) -> RowAt {
        let len = self.blocks.len;
        let line = match self.mode {
            Mode::Text => len,
            Mode::Hex => viewer::hex_row_start(len.saturating_sub(1)) + HEX_ROW,
        };
        RowAt { line, row: 0 }
    }

    fn show_from(&mut self, at: RowAt) {
        self.top = at;
        self.rows.clear();
        self.after = After::Row(at);
        self.task = Task::Page { reveal: false };
    }

    /// Builds the page's rows again from its first line after their text
    /// changed: another width, wrapping, or a file that ended sooner. A
    /// move of the caret ends where the caret was, because its steps counted
    /// the old rows. Up and a jump build the page when they end. An idle
    /// page resumes, as after a key, because rows of the old width would
    /// show until then.
    fn relayout(&mut self) {
        self.decoded = None;
        let reveal = match self.task {
            Task::Page { reveal } => reveal,
            Task::Caret(_) | Task::Idle => false,
            Task::Up { .. } | Task::Jump { .. } => return,
        };
        self.rows.clear();
        self.after = After::Row(self.top);
        self.task = Task::Page { reveal };
    }

    /// Ends a move of the caret that waits for a block, with the caret and
    /// the selection where they were, so the request that comes meanwhile
    /// decides them. The page goes on from the rows the move started from,
    /// as many as fit the area now.
    fn end_caret_move(&mut self) {
        if let Task::Caret(_) = self.task {
            self.task = Task::Page { reveal: false };
            self.fit_height();
        }
    }

    /// Shows the page at once, but from the first line at or after
    /// `offset` as `mode`, after it has read what that needs, as a reopened
    /// file shows where it was.
    fn jump_as(&mut self, mode: Mode, offset: u64) {
        self.mode = mode;
        self.decoded = None;
        self.left = 0;
        self.rows.clear();
        self.after = After::End;
        self.task = Task::Jump { offset };
    }

    /// Fits the page to an area of `columns` characters and `height` rows,
    /// each at least 1, as [`text_room`] gives them.
    pub(crate) fn set_area(&mut self, columns: usize, height: usize) {
        if (columns, height) == (self.columns, self.height) {
            return;
        }
        // A wrapped line has other rows at another width.
        let rewrap = self.wrap && self.mode == Mode::Text && columns != self.columns;
        self.columns = columns;
        self.height = height;
        if rewrap {
            self.top.row = 0;
            self.relayout();
        } else if let Task::Page { .. } = self.task {
            self.fit_height();
        }
    }

    /// Keeps the rows that fit the area's height, and goes on after them.
    /// Up and a jump build their page anew, and an idle page or one that a
    /// move of the caret waits on keeps its rows, which show again when the
    /// area grows back after "Waiting for…".
    fn fit_height(&mut self) {
        if self.rows.len() > self.height {
            self.after = After::Row(self.rows[self.height].at);
            self.rows.truncate(self.height);
        }
    }

    /// Shows the file as `mode`, from the byte the page shows first.
    pub(crate) fn set_mode(&mut self, mode: Mode) {
        if mode != self.mode {
            // Select all stays, but a place in the text has none in Hex,
            // and bytes have none in the text.
            if matches!(
                self.selection,
                Selection::Text { .. } | Selection::Bytes { .. }
            ) {
                self.selection = Selection::None;
            }
            self.caret = None;
            self.jump_as(mode, self.top.line);
        }
    }

    pub(crate) fn set_wrap(&mut self, wrap: bool) {
        if wrap == self.wrap {
            return;
        }
        self.wrap = wrap;
        self.left = 0;
        if self.mode == Mode::Text {
            self.top.row = 0;
            self.relayout();
        }
    }

    /// Reads the file's text as `encoding`, which the person chose, from
    /// the first line at or after the byte the page shows first. A choice
    /// shows the text, so it shows Text also from Hex.
    pub(crate) fn choose_encoding(&mut self, encoding: Encoding) {
        self.detected = viewer::chosen_encoding(self.mark, encoding);
        self.chosen = true;
        // Places in the text and their bytes are other ones now.
        if self.selection != Selection::All {
            self.selection = Selection::None;
        }
        self.caret = None;
        self.found = None;
        self.jump_as(Mode::Text, self.top.line);
    }

    /// Keeps the block `index` that the thread read.
    pub(crate) fn receive(&mut self, index: u64, bytes: Vec<u8>) {
        let len = self.blocks.len;
        self.blocks.keep(index, bytes);
        if self.blocks.len == len {
            return;
        }
        // The file shrank, so the rows built past its new end are wrong.
        let end = self.blocks.len;
        self.caret = self.caret.filter(|caret| caret.point.line < end);
        if self.top.line > end {
            return self.jump_as(self.mode, end);
        }
        // A move up from past the new end, such as End's, starts at the new
        // end instead.
        let end_row = self.end_row();
        if let Task::Up { at, .. } = &mut self.task
            && at.line > end
        {
            *at = end_row;
        }
        self.relayout();
    }

    /// Goes on with the page or the move as far as the kept blocks allow,
    /// and returns the block it waits for. `code_page` decodes the bytes
    /// of a code page.
    pub(crate) fn advance(&mut self, code_page: &CodePageText) -> Option<u64> {
        loop {
            let waits = match self.task {
                Task::Idle => return None,
                Task::Jump { offset } => match self.jump_line(offset) {
                    Ok(line) => {
                        self.show_from(RowAt { line, row: 0 });
                        continue;
                    }
                    Err(block) => block,
                },
                Task::Up {
                    at,
                    rows: 0,
                    reveal,
                } => {
                    self.show_from(at);
                    self.task = Task::Page { reveal };
                    continue;
                }
                Task::Up { at, rows, reveal } => match self.row_before(at, code_page) {
                    Ok(Some(before)) => {
                        self.task = Task::Up {
                            at: before,
                            rows: rows - 1,
                            reveal,
                        };
                        continue;
                    }
                    Ok(None) => {
                        self.show_from(at);
                        self.task = Task::Page { reveal };
                        continue;
                    }
                    Err(block) => block,
                },
                Task::Caret(step) => match self.caret_step(step.caret, step.motion, code_page) {
                    Ok(caret) => {
                        let moved = caret.point != step.caret.point;
                        let step = CaretMove {
                            moved: step.moved + usize::from(moved),
                            caret,
                            ..step
                        };
                        if moved && step.steps > 1 {
                            self.task = Task::Caret(CaretMove {
                                steps: step.steps - 1,
                                ..step
                            });
                        } else {
                            self.caret_moved(step);
                        }
                        continue;
                    }
                    Err(block) => block,
                },
                Task::Page { reveal } => match self.add_row(code_page) {
                    Ok(true) => continue,
                    Ok(false) => {
                        // Empty rows under the file's last: the top moves
                        // up until the last row is at the bottom, as after
                        // End. Each time it moves up, so this ends at the
                        // file's start at the latest.
                        let short = self.height.saturating_sub(self.rows.len());
                        if self.after == After::End && short > 0 && self.top != self.start_row() {
                            self.task = Task::Up {
                                at: self.top,
                                rows: short,
                                reveal,
                            };
                            continue;
                        }
                        if !reveal {
                            return None;
                        }
                        // Scrolling to the caret builds a page that does
                        // not reveal it again.
                        match self.reveal_caret(code_page) {
                            Ok(true) => continue,
                            Ok(false) => {
                                self.task = Task::Page { reveal: false };
                                return None;
                            }
                            Err(block) => block,
                        }
                    }
                    Err(block) => block,
                },
            };
            return Some(waits);
        }
    }

    /// Stops what waits for the disk. A complete page stays complete, so
    /// the rows that "Waiting for…" covered show again when it goes, and
    /// the next key moves from the page.
    pub(crate) fn cancel(&mut self) {
        if !self.settled() {
            self.task = Task::Idle;
        }
    }

    pub(crate) fn resume(&mut self) {
        if self.task == Task::Idle {
            self.show_from(self.top);
        }
    }

    /// Adds the next row to the page; `false` when the page is complete.
    fn add_row(&mut self, code_page: &CodePageText) -> Need<bool> {
        let After::Row(at) = self.after else {
            return Ok(false);
        };
        if self.rows.len() >= self.height {
            return Ok(false);
        }
        let line = Rc::clone(self.line_rows(at.line, code_page)?);
        let count = line.rows.len();
        if count == 0 {
            self.after = After::End;
            return Ok(true);
        }
        // A row that a narrower width took away is the line's last.
        let row = at.row.min(count - 1);
        // Before the row is added, because it may wait for a block.
        let after = if row + 1 < count {
            After::Row(RowAt { row: row + 1, ..at })
        } else {
            match self.next_line(at.line)? {
                Some(line) => After::Row(RowAt { line, row: 0 }),
                None => After::End,
            }
        };
        let at = RowAt { row, ..at };
        if self.rows.is_empty() {
            self.top = at;
        }
        self.rows.push(Row { at, line });
        self.after = after;
        Ok(true)
    }

    /// The rows of the line that starts at `line`.
    fn line_rows(&mut self, line: u64, code_page: &CodePageText) -> Need<&Rc<LineRows>> {
        if !matches!(&self.decoded, Some(decoded) if decoded.line == line) {
            let decoded = self.decode_line(line, code_page)?;
            self.decoded = Some(Rc::new(decoded));
        }
        Ok(self.decoded.as_ref().expect("the line was just decoded"))
    }

    fn decode_line(&self, line: u64, code_page: &CodePageText) -> Need<LineRows> {
        let len = self.blocks.len;
        match self.mode {
            Mode::Hex => {
                if line >= len {
                    return Ok(LineRows {
                        line,
                        rows: Vec::new(),
                        muted: 0,
                        columns: self.columns,
                        text: String::new(),
                        broken: false,
                    });
                }
                let bytes = self.blocks.bytes(line..(line + HEX_ROW).min(len))?;
                Ok(LineRows {
                    line,
                    rows: vec![viewer::hex_row(len, line, &bytes)],
                    muted: viewer::offset_text(len, line).len(),
                    columns: self.columns,
                    text: String::new(),
                    broken: false,
                })
            }
            Mode::Text => {
                let end = self.line_end(line)?;
                let bytes = self.blocks.bytes(line..end)?;
                let decoded = viewer::decode_piece(self.detected.encoding, &bytes, code_page);
                let text = viewer::without_terminator(&decoded);
                let expanded = viewer::expand_tabs(text);
                let rows = if self.wrap {
                    wrapped(expanded, self.columns)
                } else {
                    vec![expanded]
                };
                Ok(LineRows {
                    line,
                    rows,
                    muted: 0,
                    columns: self.columns,
                    text: text.to_owned(),
                    broken: text.len() != decoded.len(),
                })
            }
        }
    }

    /// Where the text line, or the piece of a long line, that starts at
    /// `line` ends.
    fn line_end(&self, line: u64) -> Need<u64> {
        let len = self.blocks.len;
        if line >= len {
            return Ok(line);
        }
        let end = line.saturating_add(LINE_BYTES as u64).min(len);
        let ahead = self.blocks.bytes(line..end)?;
        Ok(
            viewer::next_line_start(self.detected.encoding, &ahead, line, end == len)
                .expect("LINE_BYTES bytes, or the rest of the file, decide where a line ends"),
        )
    }

    /// The start of the line after the one at `line`; `None` when that is
    /// the file's last.
    fn next_line(&self, line: u64) -> Need<Option<u64>> {
        let next = match self.mode {
            Mode::Text => self.line_end(line)?,
            Mode::Hex => line + HEX_ROW,
        };
        let len = self.blocks.len;
        Ok((line < len && next < len).then_some(next))
    }

    /// The start of the line before the one at `line`; `None` at the start.
    fn previous_line(&self, line: u64) -> Need<Option<u64>> {
        match self.mode {
            Mode::Hex => Ok(line.checked_sub(HEX_ROW)),
            Mode::Text => {
                let start = self.start();
                let line = line.min(self.blocks.len);
                if line <= start {
                    return Ok(None);
                }
                let from = line.saturating_sub(LINE_BYTES as u64).max(start);
                let before = self.blocks.bytes(from..line)?;
                let previous =
                    viewer::previous_line_start(self.detected.encoding, start, &before, line);
                Ok((previous < line).then_some(previous))
            }
        }
    }

    /// The row before the one at `at`; `None` at the first.
    fn row_before(&mut self, at: RowAt, code_page: &CodePageText) -> Need<Option<RowAt>> {
        if at.row > 0 {
            return Ok(Some(RowAt {
                row: at.row - 1,
                ..at
            }));
        }
        let Some(line) = self.previous_line(at.line)? else {
            return Ok(None);
        };
        let count = self.line_rows(line, code_page)?.rows.len();
        Ok(Some(RowAt {
            line,
            row: count.saturating_sub(1),
        }))
    }

    /// The line that a jump to `offset` shows first: in Text the first that
    /// starts at or after it, in Hex the row that holds it.
    fn jump_line(&self, offset: u64) -> Need<u64> {
        let len = self.blocks.len;
        match self.mode {
            Mode::Hex => Ok(viewer::hex_row_start(offset.min(len.saturating_sub(1)))),
            Mode::Text => {
                let start = self.start();
                let position = offset.clamp(start, len);
                let from = position.saturating_sub(LINE_BYTES as u64).max(start);
                let to = position.saturating_add(LINE_BYTES as u64).min(len);
                let window = self.blocks.bytes(from..to)?;
                Ok(viewer::line_at_or_after(
                    self.detected.encoding,
                    start,
                    &window,
                    from,
                    position,
                    to == len,
                )
                .unwrap_or(position))
            }
        }
    }

    /// Whether the page is complete, so a move starts from it. A move
    /// asked for meanwhile is dropped, because the page it starts from is
    /// not known yet; Home, End and a jump replace what runs.
    fn settled(&self) -> bool {
        self.task == Task::Page { reveal: false }
            && (self.rows.len() >= self.height || self.after == After::End)
    }

    /// Whether the page shows the file's last row.
    fn end_shown(&self) -> bool {
        matches!(self.task, Task::Page { .. } | Task::Idle) && self.after == After::End
    }

    /// Moves down by `rows` rows, as far as the last row at the bottom.
    fn down(&mut self, rows: usize) {
        let After::Row(after) = self.after else {
            return;
        };
        if !self.settled() || rows == 0 {
            return;
        }
        if rows >= self.rows.len() {
            self.show_from(after);
        } else {
            self.rows.drain(..rows);
            self.top = self.rows[0].at;
        }
    }

    fn up(&mut self, rows: usize) {
        if self.settled() && rows > 0 && self.top != self.start_row() {
            self.task = Task::Up {
                at: self.top,
                rows,
                reveal: false,
            };
        }
    }

    /// A key that moves: the caret while it shows in Text or with Shift
    /// (`extend`), which selects from where it was; otherwise the page.
    pub(crate) fn go(&mut self, motion: Motion, extend: bool) {
        if self.mode == Mode::Text && (extend || self.caret.is_some()) {
            self.move_caret(motion, extend);
            return;
        }
        match motion {
            Motion::Up => self.up(1),
            Motion::Down => self.down(1),
            Motion::PageUp => self.up(self.height),
            Motion::PageDown => self.down(self.height),
            Motion::Left => self.scroll_left(),
            Motion::Right => self.scroll_right(),
            Motion::Home | Motion::FileStart => self.home(),
            Motion::End | Motion::FileEnd => self.end(),
        }
    }

    // The scroll bar's arrows and the space beside its box, and a drag past
    // an edge: each ends a move of the caret that waits for a block, as a
    // click does.

    pub(crate) fn line_up(&mut self) {
        self.end_caret_move();
        self.up(1);
    }

    pub(crate) fn line_down(&mut self) {
        self.end_caret_move();
        self.down(1);
    }

    pub(crate) fn page_up(&mut self) {
        self.end_caret_move();
        self.up(self.height);
    }

    pub(crate) fn page_down(&mut self) {
        self.end_caret_move();
        self.down(self.height);
    }

    /// The file's start, at the left edge.
    pub(crate) fn home(&mut self) {
        self.left = 0;
        self.show_from(self.start_row());
    }

    /// The last page.
    pub(crate) fn end(&mut self) {
        self.task = Task::Up {
            at: self.end_row(),
            rows: self.height,
            reveal: false,
        };
    }

    /// The mouse wheel turned by `delta`, positive away from the person.
    pub(crate) fn wheel(&mut self, delta: i32) {
        self.wheel = self.wheel.saturating_add(delta);
        let rows = self.wheel / WHEEL_ROW;
        self.wheel -= rows * WHEEL_ROW;
        if rows > 0 {
            self.up(rows.unsigned_abs() as usize);
        } else if rows < 0 {
            self.down(rows.unsigned_abs() as usize);
        }
    }

    /// The scroll box dragged to `position`, which maps to a byte of the
    /// file.
    pub(crate) fn jump(&mut self, position: i32) {
        self.task = Task::Jump {
            offset: viewer::scroll_offset(self.blocks.len, position),
        };
    }

    /// Whether `Left` and `Right` scroll sideways: in Text while long lines
    /// do not wrap.
    fn sideways(&self) -> bool {
        self.mode == Mode::Text && !self.wrap
    }

    pub(crate) fn scroll_left(&mut self) {
        if self.sideways() {
            self.left = self.left.saturating_sub(1);
        }
    }

    /// One column to the right, while a shown row goes on past the area.
    pub(crate) fn scroll_right(&mut self) {
        let widest = self
            .rows
            .iter()
            .map(|row| row.text().chars().count())
            .max()
            .unwrap_or(0);
        if self.sideways() && self.left + self.columns < widest {
            self.left += 1;
        }
    }

    /// The first shown column of every row.
    fn skip(&self) -> usize {
        if self.sideways() { self.left } else { 0 }
    }

    /// The characters of each wrapped row of a line, or 0 while lines do
    /// not wrap and a line is one row.
    fn row_width(&self) -> usize {
        if self.wrap && self.mode == Mode::Text {
            self.columns.max(1)
        } else {
            0
        }
    }

    /// Which of a line's `rows` wrapped rows shows its `column`: the row
    /// after a full row shows the place after that row's last character.
    fn row_of(&self, column: usize, rows: usize) -> usize {
        match self.row_width() {
            0 => 0,
            width => (column / width).min(rows.saturating_sub(1)),
        }
    }

    /// `F6`: the caret shows at the page's first character, or hides, when
    /// [`Document::can_toggle_caret`]. A move of the caret that waits ends
    /// first.
    pub(crate) fn toggle_caret(&mut self) {
        if !self.can_toggle_caret() {
            return;
        }
        self.end_caret_move();
        self.caret = match self.caret {
            None => self.point_at(0, 0).map(|point| Caret { point, goal: None }),
            Some(_) => None,
        };
    }

    /// Whether `F6` shows or hides the caret now: it hides a shown caret,
    /// and shows one in Text on a complete page, as a move starts from one.
    pub(crate) fn can_toggle_caret(&self) -> bool {
        self.caret.is_some() || (self.mode == Mode::Text && self.settled())
    }

    pub(crate) fn has_caret(&self) -> bool {
        self.caret.is_some()
    }

    /// Moves the caret by `motion`, selecting from where it was when
    /// `extend`, and otherwise clearing the selection, as an edit control
    /// does. A caret that did not show yet starts at the page's first
    /// character. Like a move of the page, it starts from a complete page.
    fn move_caret(&mut self, motion: Motion, extend: bool) {
        if !self.settled() {
            return;
        }
        let caret = self
            .caret
            .or_else(|| self.point_at(0, 0).map(|point| Caret { point, goal: None }));
        let Some(caret) = caret else {
            return;
        };
        let anchor = extend.then_some(match self.selection {
            Selection::Text { anchor, focus } if focus == caret.point => anchor,
            _ => caret.point,
        });
        let steps = match motion {
            Motion::PageUp | Motion::PageDown => self.height,
            _ => 1,
        };
        self.task = Task::Caret(CaretMove {
            motion,
            steps,
            moved: 0,
            anchor,
            caret,
        });
    }

    /// Where one step of `motion` takes `caret`.
    fn caret_step(
        &mut self,
        caret: Caret,
        motion: Motion,
        code_page: &CodePageText,
    ) -> Need<Caret> {
        let Caret { point: caret, goal } = caret;
        let line = Rc::clone(self.line_rows(caret.line, code_page)?);
        let count = line.text.chars().count();
        let column = viewer::column_of(&line.text, caret.char);
        let row = self.row_of(column, line.rows.len());
        let goal = goal.unwrap_or(column - row * self.row_width());
        let to = match motion {
            Motion::Left if caret.char > 0 => TextPoint {
                char: caret.char - 1,
                ..caret
            },
            Motion::Left => match self.previous_line(caret.line)? {
                Some(previous) => TextPoint {
                    line: previous,
                    char: self.line_rows(previous, code_page)?.text.chars().count(),
                },
                None => caret,
            },
            Motion::Right if caret.char < count => TextPoint {
                char: caret.char + 1,
                ..caret
            },
            Motion::Right => match self.next_line(caret.line)? {
                Some(next) => TextPoint {
                    line: next,
                    char: 0,
                },
                None => caret,
            },
            Motion::Up | Motion::PageUp if row > 0 => self.point_in_row(&line, row - 1, goal),
            Motion::Up | Motion::PageUp => match self.previous_line(caret.line)? {
                Some(previous) => {
                    let above = Rc::clone(self.line_rows(previous, code_page)?);
                    self.point_in_row(&above, above.rows.len().saturating_sub(1), goal)
                }
                None => caret,
            },
            Motion::Down | Motion::PageDown if row + 1 < line.rows.len() => {
                self.point_in_row(&line, row + 1, goal)
            }
            Motion::Down | Motion::PageDown => match self.next_line(caret.line)? {
                Some(next) => {
                    let below = Rc::clone(self.line_rows(next, code_page)?);
                    self.point_in_row(&below, 0, goal)
                }
                None => caret,
            },
            Motion::Home => TextPoint { char: 0, ..caret },
            Motion::End => TextPoint {
                char: count,
                ..caret
            },
            Motion::FileStart => self.text_start(),
            Motion::FileEnd => self.last_point(code_page)?,
        };
        let vertical = matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        );
        Ok(Caret {
            point: to,
            goal: vertical.then_some(goal),
        })
    }

    /// The place in the wrapped row `row` of `line` nearest to the column
    /// `goal` of that row, before its last character if the row is full,
    /// so the caret stays in the row.
    fn point_in_row(&self, line: &LineRows, row: usize, goal: usize) -> TextPoint {
        let width = self.row_width();
        let end = if row + 1 >= line.rows.len() {
            viewer::column_of(&line.text, usize::MAX)
        } else {
            (row + 1) * width - 1
        };
        let column = (row * width + goal).min(end);
        TextPoint {
            line: line.line,
            char: viewer::index_at_column(&line.text, column),
        }
    }

    /// The place after the text's last character.
    fn last_point(&mut self, code_page: &CodePageText) -> Need<TextPoint> {
        let (start, len) = (self.start(), self.blocks.len);
        let line = {
            let from = len.saturating_sub(LINE_BYTES as u64).max(start);
            let before = self.blocks.bytes(from..len)?;
            viewer::line_start(self.detected.encoding, start, &before, len)
        };
        // After a final line end, the last line is the one it ends.
        let line = if line == len && line > start {
            self.previous_line(len)?.unwrap_or(start)
        } else {
            line
        };
        let char = self.line_rows(line, code_page)?.text.chars().count();
        Ok(TextPoint { line, char })
    }

    /// Ends a move of the caret: the caret goes where it took it, the
    /// selection follows, `PgUp` and `PgDn` scroll the page by the steps
    /// taken, and the page shows the caret once it is complete.
    fn caret_moved(&mut self, step: CaretMove) {
        self.caret = Some(step.caret);
        self.selection = match step.anchor {
            Some(anchor) => Selection::Text {
                anchor,
                focus: step.caret.point,
            },
            None => Selection::None,
        };
        // The page's rows are those the move started from, as
        // `end_caret_move` says.
        self.end_caret_move();
        match step.motion {
            Motion::PageDown => self.down(step.moved),
            Motion::PageUp => self.up(step.moved),
            _ => {}
        }
        if let Task::Page { reveal } | Task::Up { reveal, .. } = &mut self.task {
            *reveal = true;
        }
    }

    /// Scrolls the complete page so it shows the caret: up to put its row
    /// at the top, down to put it at the bottom, and sideways while lines
    /// do not wrap. Whether the page moved up or down.
    fn reveal_caret(&mut self, code_page: &CodePageText) -> Need<bool> {
        let Some(Caret { point: caret, .. }) = self.caret else {
            return Ok(false);
        };
        let line = Rc::clone(self.line_rows(caret.line, code_page)?);
        let column = viewer::column_of(&line.text, caret.char);
        if self.sideways() {
            if column < self.left {
                self.left = column;
            } else if column >= self.left + self.columns {
                self.left = column + 1 - self.columns;
            }
        }
        let at = RowAt {
            line: caret.line,
            row: self.row_of(column, line.rows.len()),
        };
        if self.rows.iter().any(|row| row.at == at) {
            return Ok(false);
        }
        if (at.line, at.row) < (self.top.line, self.top.row) {
            self.show_from(at);
        } else {
            self.task = Task::Up {
                at,
                rows: self.height.saturating_sub(1),
                reveal: false,
            };
        }
        Ok(true)
    }

    /// Where the caret shows: its row of the page and its column of the
    /// area; `None` while it is hidden or off the page.
    pub(crate) fn caret_cell(&self) -> Option<(usize, usize)> {
        let caret = self.caret?.point;
        self.rows.iter().enumerate().find_map(|(index, row)| {
            if row.at.line != caret.line {
                return None;
            }
            let column = viewer::column_of(&row.line.text, caret.char);
            if self.row_of(column, row.line.rows.len()) != row.at.row {
                return None;
            }
            let column = column.checked_sub(row.start() + self.skip())?;
            Some((index, column))
        })
    }

    pub(crate) fn select_all(&mut self) {
        self.end_caret_move();
        self.selection = Selection::All;
    }

    /// Whether a press and a drag select text, which they do in Text.
    pub(crate) fn selects_text(&self) -> bool {
        self.mode == Mode::Text
    }

    /// The left button went down at `column`, a place between two
    /// characters, of the shown `row`, both counted from the area's first.
    /// With `extend`, as with Shift, the selection reaches there from where
    /// it started, or from a shown caret, as Shift with the arrows does, or
    /// from the text's start. A shown caret goes there. In Hex the
    /// selection clears.
    pub(crate) fn press(&mut self, row: usize, column: usize, extend: bool) {
        self.end_caret_move();
        let Some(point) = self.point_at(row, column) else {
            self.selection = Selection::None;
            return;
        };
        let anchor = match (self.selection, self.caret) {
            (Selection::Text { anchor, .. }, _) if extend => anchor,
            (_, Some(caret)) if extend => caret.point,
            _ if extend => self.text_start(),
            _ => point,
        };
        self.selection = Selection::Text {
            anchor,
            focus: point,
        };
        self.place_caret(point);
    }

    /// The pointer moved, with the button down since [`Document::press`],
    /// to `column` of the shown `row`; `true` when the selection changed.
    pub(crate) fn drag(&mut self, row: usize, column: usize) -> bool {
        self.end_caret_move();
        let (Selection::Text { anchor, focus }, Some(point)) =
            (self.selection, self.point_at(row, column))
        else {
            return false;
        };
        self.selection = Selection::Text {
            anchor,
            focus: point,
        };
        self.place_caret(point);
        point != focus
    }

    /// A double-click on the character in the cell `column` of the shown
    /// `row` selects its word, as [`viewer::word_at`] decides it.
    pub(crate) fn select_word(&mut self, row: usize, column: usize) {
        self.end_caret_move();
        let Some(row) = self.text_row(row) else {
            return;
        };
        let column = (row.start() + self.skip() + column).min(row.end().saturating_sub(1));
        let line = &row.line.text;
        let word = viewer::word_at(line, viewer::index_under_column(line, column));
        let point = |char| TextPoint {
            line: row.at.line,
            char,
        };
        let (anchor, focus) = (point(word.start), point(word.end));
        self.selection = Selection::Text { anchor, focus };
        self.place_caret(focus);
    }

    /// Moves a shown caret to `point`, where the mouse put the selection's
    /// end.
    fn place_caret(&mut self, point: TextPoint) {
        if self.caret.is_some() {
            self.caret = Some(Caret { point, goal: None });
        }
    }

    /// The shown row `row` in Text, or the last when the rows end above it.
    fn text_row(&self, row: usize) -> Option<&Row> {
        if !self.selects_text() {
            return None;
        }
        self.rows.get(row.min(self.rows.len().checked_sub(1)?))
    }

    /// The place in the text at `column` of the shown `row`, or at the
    /// row's end when the row ends before it.
    fn point_at(&self, row: usize, column: usize) -> Option<TextPoint> {
        let row = self.text_row(row)?;
        let column = (row.start() + self.skip() + column).min(row.end());
        Some(TextPoint {
            line: row.at.line,
            char: viewer::index_at_column(&row.line.text, column),
        })
    }

    /// The place before the text's first character.
    fn text_start(&self) -> TextPoint {
        TextPoint {
            line: self.start(),
            char: 0,
        }
    }

    /// The selected text, from its first place to its last; `None` while
    /// no text is selected.
    fn selected_text(&self) -> Option<(TextPoint, TextPoint)> {
        match self.selection {
            Selection::None | Selection::Bytes { .. } => None,
            Selection::All => Some((
                self.text_start(),
                TextPoint {
                    line: self.blocks.len,
                    char: 0,
                },
            )),
            Selection::Text { anchor, focus } => {
                (anchor != focus).then(|| (anchor.min(focus), anchor.max(focus)))
            }
        }
    }

    pub(crate) fn selected(&self) -> bool {
        matches!(self.selection, Selection::Bytes { .. }) || self.selected_text().is_some()
    }

    /// What Copy reads for the selection, or why it copies nothing: the
    /// selection's text could pass [`COPY_UNITS`]. In Text every encoding
    /// makes at most one UTF-16 unit of each byte, so the bytes bound the
    /// text. `None` while nothing is selected.
    pub(crate) fn copy(&self) -> Option<Result<CopyText, TooLarge>> {
        match self.selection {
            Selection::None => None,
            Selection::All => Some(self.copy_file()),
            Selection::Text { .. } => {
                let (from, to) = self.selected_text()?;
                Some(self.copy_between(from, to))
            }
            Selection::Bytes { start, end } => Some(self.copy_bytes(start, end)),
        }
    }

    fn copy_between(&self, from: TextPoint, to: TextPoint) -> Result<CopyText, TooLarge> {
        let len = self.blocks.len;
        let end = to.line.saturating_add(LINE_BYTES as u64).min(len);
        let start = from.line.min(end);
        if end - start > COPY_UNITS {
            return Err(TooLarge::Selection(COPY_UNITS));
        }
        Ok(CopyText {
            bytes: start..end,
            copied: Copied::Between {
                encoding: self.detected.encoding,
                from,
                to,
                last: end == len,
            },
        })
    }

    /// Bytes copy as 3 characters each: two hex digits and a space.
    fn copy_bytes(&self, start: u64, end: u64) -> Result<CopyText, TooLarge> {
        let end = end.min(self.blocks.len);
        let start = start.min(end);
        let limit = COPY_UNITS / 3;
        if end - start > limit {
            return Err(TooLarge::Selection(limit));
        }
        Ok(CopyText {
            bytes: start..end,
            copied: Copied::Bytes,
        })
    }

    fn copy_file(&self) -> Result<CopyText, TooLarge> {
        let len = self.blocks.len;
        let (units, too_large) = match self.mode {
            Mode::Text => {
                let start = self.start();
                (len - start, TooLarge::Text(COPY_UNITS + start))
            }
            Mode::Hex => {
                let row = viewer::hex_row(len, 0, &[0; HEX_ROW_BYTES]).len() as u64 + 2;
                let limit = COPY_UNITS / row * HEX_ROW;
                (len.div_ceil(HEX_ROW) * row, TooLarge::Hex(limit))
            }
        };
        if units > COPY_UNITS {
            return Err(too_large);
        }
        Ok(CopyText {
            bytes: 0..len,
            copied: Copied::File {
                mode: self.mode,
                detected: self.detected,
            },
        })
    }

    /// What a search of Find needs: forward from the end of the match it
    /// found last while that match stays selected, else from the page's
    /// first byte; backward from that match's start, else from the page's
    /// end.
    pub(crate) fn find_in(&self, backward: bool) -> FindIn {
        let from = match &self.found {
            Some((bytes, selection)) if *selection == self.selection => {
                if backward {
                    bytes.start
                } else {
                    bytes.end
                }
            }
            _ if backward => match self.after {
                After::Row(at) => at.line,
                After::End => self.blocks.len,
            },
            _ => self.top.line,
        };
        FindIn {
            mode: self.mode,
            detected: self.detected,
            len: self.blocks.len,
            from,
            backward,
            start: self.start(),
        }
    }

    /// Selects `found`, a match of Find, and shows it: its line at the top
    /// unless the page already shows it. In Hex it selects the bytes, also
    /// when the person switched from Text meanwhile; in Text, an answer
    /// without the match's characters, from Hex, selects nothing. `found`
    /// comes from a search in the encoding that shows, because the places
    /// of another encoding are other ones.
    pub(crate) fn show_found(&mut self, found: Found) {
        self.end_caret_move();
        match (self.mode, found.text) {
            (Mode::Text, Some((start, column, end))) => {
                self.selection = Selection::Text {
                    anchor: start,
                    focus: end,
                };
                self.place_caret(end);
                if self.sideways() && (column < self.left || column >= self.left + self.columns) {
                    self.left = column.saturating_sub(self.columns / 4);
                }
                let at = RowAt {
                    line: start.line,
                    row: match self.row_width() {
                        0 => 0,
                        width => column / width,
                    },
                };
                if !self.rows.iter().any(|row| row.at == at) {
                    self.show_from(at);
                }
            }
            (Mode::Hex, _) => {
                self.selection = Selection::Bytes {
                    start: found.bytes.start,
                    end: found.bytes.end,
                };
                let row = viewer::hex_row_start(found.bytes.start);
                if !self.rows.iter().any(|shown| shown.at.line == row) {
                    self.task = Task::Jump {
                        offset: found.bytes.start,
                    };
                }
            }
            (Mode::Text, None) => return,
        }
        self.found = Some((found.bytes, self.selection));
    }

    /// The shown rows.
    pub(crate) fn shown(&self) -> Vec<ShownRow<'_>> {
        let skip = self.skip();
        let selected = self.selected_text();
        self.rows
            .iter()
            .map(|row| {
                let text = characters(row.text(), skip, self.columns + 1);
                let count = text.chars().count();
                let none = [0..0, 0..0];
                let selected = match (self.mode, self.selection) {
                    (Mode::Hex, Selection::All) => [0..count, 0..0],
                    (Mode::Hex, Selection::Bytes { start, end }) => {
                        let first = row.at.line;
                        let from = start.max(first);
                        let to = end.min(first + HEX_ROW);
                        if from < to {
                            let bytes = (from - first) as usize..(to - first) as usize;
                            viewer::hex_columns(row.line.muted, bytes)
                        } else {
                            none
                        }
                    }
                    (Mode::Text, _) => match selected {
                        Some((from, to)) => [row.selected(from, to, skip, count), 0..0],
                        None => none,
                    },
                    (Mode::Hex, Selection::None | Selection::Text { .. }) => none,
                };
                ShownRow {
                    text,
                    muted: row.line.muted.saturating_sub(skip),
                    selected,
                }
            })
            .collect()
    }

    /// The status bar's position: "Top", "37%" or "End" in Text, the first
    /// row's offset in Hex.
    pub(crate) fn place(&self, strings: &Strings, numbers: &dyn Numbers) -> String {
        if self.mode == Mode::Hex {
            return viewer::offset_text(self.blocks.len, self.top.line);
        }
        // A wrapped row after a line's first is past that line's start.
        let first = self.top.line + u64::from(self.top.row > 0);
        match viewer::text_place(self.start(), self.blocks.len, first, self.end_shown()) {
            Place::Top => strings.text("viewer.top").to_owned(),
            Place::Percent(percent) => {
                strings.format("viewer.percent", &[&numbers.integer(u64::from(percent))])
            }
            Place::End => strings.text("viewer.end").to_owned(),
        }
    }

    /// The scroll bar, a position for each byte as
    /// [`viewer::scroll_position`] maps them; its page covers the whole
    /// range, which disables it, while the whole file shows.
    pub(crate) fn scroll_bar(&self) -> ScrollBar {
        let len = self.blocks.len;
        let max = (viewer::scroll_max(len) - 1).max(0);
        let position = viewer::scroll_position(len, self.top.line);
        let page = if self.top == self.start_row() && self.end_shown() {
            max.unsigned_abs() + 1
        } else {
            let shown_end = match self.after {
                After::Row(at) => at.line,
                After::End => len,
            };
            (viewer::scroll_position(len, shown_end) - position)
                .max(1)
                .unsigned_abs()
        };
        ScrollBar {
            max,
            page,
            position,
        }
    }
}

/// `text` in rows of `columns` characters.
fn wrapped(text: String, columns: usize) -> Vec<String> {
    if text.chars().nth(columns).is_none() {
        return vec![text];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    for (i, c) in text.chars().enumerate() {
        if i > 0 && i % columns == 0 {
            rows.push(std::mem::take(&mut row));
        }
        row.push(c);
    }
    rows.push(row);
    rows
}

/// The part of `text` from its character `skip` on, at most `take`
/// characters long.
fn characters(text: &str, skip: usize, take: usize) -> &str {
    let rest = &text[viewer::char_offset(text, skip)..];
    &rest[..viewer::char_offset(rest, take)]
}

/// The area's vertical scroll bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScrollBar {
    /// The range is 0 to `max`.
    pub(crate) max: i32,
    pub(crate) page: u32,
    pub(crate) position: i32,
}

impl ScrollBar {
    /// While no file shows: disabled.
    pub(crate) const NONE: ScrollBar = ScrollBar {
        max: 0,
        page: 1,
        position: 0,
    };
}

/// What a Viewer shows in its area.
pub(crate) enum Shown {
    /// Until the first block arrives.
    Reading,
    Cancelled,
    File(Box<Document>),
    /// Windows could not open the file, or, when `opened`, read it.
    Failed {
        opened: bool,
        reason: String,
    },
    Removing,
    Removed,
    /// None of the entries that `F3` acted on is a file.
    NoFiles,
}

impl Shown {
    pub(crate) fn document(&self) -> Option<&Document> {
        match self {
            Shown::File(document) => Some(document.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn document_mut(&mut self) -> Option<&mut Document> {
        match self {
            Shown::File(document) => Some(document.as_mut()),
            _ => None,
        }
    }

    /// The message the area shows instead of the file `name`, and whether
    /// Retry shows under it.
    pub(crate) fn message(&self, strings: &Strings, name: &str) -> Option<(String, bool)> {
        Some(match self {
            Shown::File(_) => return None,
            Shown::Reading => (strings.text("panel.reading").to_owned(), false),
            Shown::Cancelled => (strings.text("work.cancelled").to_owned(), true),
            Shown::Failed { opened, reason } => {
                let key = if *opened {
                    "viewer.cannot_read"
                } else {
                    "panel.cannot_open"
                };
                (strings.format(key, &[name, reason.as_str()]), true)
            }
            Shown::Removing => (strings.text("viewer.removing").to_owned(), true),
            Shown::Removed => (strings.text("viewer.removed").to_owned(), true),
            Shown::NoFiles => (strings.text("viewer.no_files").to_owned(), false),
        })
    }
}

/// The status bar's position part at its widest, which sets its width:
/// "Top", "End", "99%" or a 16-digit offset.
pub(crate) fn place_samples(strings: &Strings, numbers: &dyn Numbers) -> [String; 4] {
    [
        strings.text("viewer.top").to_owned(),
        strings.text("viewer.end").to_owned(),
        strings.format("viewer.percent", &[&numbers.integer(99)]),
        viewer::offset_text(u64::MAX, 0),
    ]
}

/// The status bar's left part: the file's name, its encoding in Text and
/// its size, or only the name while no file shows.
pub(crate) fn file_text(
    strings: &Strings,
    numbers: &dyn Numbers,
    name: &str,
    document: Option<&Document>,
) -> String {
    let Some(document) = document else {
        return name.to_owned();
    };
    let size = text::bytes(strings, numbers, document.blocks.len);
    match document.mode {
        Mode::Text => strings.format(
            "viewer.status_text",
            &[name, document.detected.encoding.name(), size.as_str()],
        ),
        Mode::Hex => strings.format("viewer.status_hex", &[name, size.as_str()]),
    }
}

/// The Viewer's title: the file's name and the command, and while the
/// Viewer walks through several files, which one of them shows; only the
/// command while `name` is empty, before the Viewer knows its files.
pub(crate) fn title(
    strings: &Strings,
    numbers: &dyn Numbers,
    name: &str,
    (index, count): (usize, usize),
) -> String {
    if name.is_empty() {
        return strings.text("key_bar.view").to_owned();
    }
    if count < 2 {
        return strings.format("viewer.title", &[name]);
    }
    let number = |n: usize| numbers.integer(u64::try_from(n).unwrap_or(u64::MAX));
    strings.format(
        "viewer.title_of",
        &[name, &number(index + 1), &number(count)],
    )
}

/// The space between the area's edges and its text, in pixels at 96 DPI,
/// as the prototype draws it.
const INSET: (i32, i32) = (8, 4);

/// The space before the text and above its first row in the area.
pub(crate) fn text_inset(dpi: Dpi) -> (Px, Px) {
    let at = |px: i32| Px(px).rescale(Dpi::new(96).expect("96 is not zero"), dpi);
    (at(INSET.0), at(INSET.1))
}

pub(crate) fn waiting_layout(
    area: Rect,
    text_height: Px,
    button: Size,
    dpi: Dpi,
) -> (Rect, Rect, Rect) {
    let (x, y) = text_inset(dpi);
    let width = button.width.min((area.width - x - x).max(Px(0)));
    let height = text_height.max(button.height);
    let message = Rect {
        x: area.x + x,
        y: area.y + y,
        width: (area.width - width - x - x - x).max(Px(0)),
        height,
    };
    let button = Rect {
        x: area.right() - x - width,
        y: area.y + y,
        width,
        height: button.height,
    };
    let top = (message.bottom() + y).min(area.bottom());
    let page = Rect {
        y: top,
        height: (area.bottom() - top).max(Px(0)),
        ..area
    };
    (page, message, button)
}

/// The characters and the whole rows that fit an area of `size` inside
/// its inset, in a font whose characters are `char_width` wide and whose
/// rows are `line_height` tall.
pub(crate) fn text_room(size: Size, char_width: Px, line_height: Px, dpi: Dpi) -> (usize, usize) {
    let (x, y) = text_inset(dpi);
    let fit = |room: Px, unit: Px| usize::try_from(room.0 / unit.0.max(1)).unwrap_or(0).max(1);
    (
        fit(size.width - x - x, char_width),
        fit(size.height - y - y, line_height),
    )
}

/// Where a point of the area falls on its text, each counted from the
/// area's first row or column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TextAt {
    pub(crate) row: usize,
    /// The place between two characters nearest to the point.
    pub(crate) place: usize,
    /// The character cell under the point.
    pub(crate) cell: usize,
}

/// Where `point`, in the area's client coordinates, falls on its text in a
/// font whose characters are `char_width` wide and whose rows are
/// `line_height` tall. A point before the text counts as on its first row
/// or column.
pub(crate) fn text_at(point: (i32, i32), char_width: Px, line_height: Px, dpi: Dpi) -> TextAt {
    let (left, top) = text_inset(dpi);
    let (width, height) = (char_width.0.max(1), line_height.0.max(1));
    let count = |at: i32, unit: i32| usize::try_from(at.div_euclid(unit)).unwrap_or(0);
    let x = point.0 - left.0;
    TextAt {
        row: count(point.1 - top.0, height),
        place: count(x + width / 2, width),
        cell: count(x, width),
    }
}

/// A command of the Viewer's menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewerCommand {
    NextFile,
    PreviousFile,
    Reload,
    Close,
    Copy,
    SelectAll,
    Find,
    FindNext,
    FindPrevious,
    /// `F6`: the caret shows or hides.
    Caret,
    Text,
    Hex,
    Wrap,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Encoding(Encoding),
}

impl ViewerCommand {
    /// The commands besides the encodings, which follow them.
    const FIXED: [ViewerCommand; 16] = [
        ViewerCommand::NextFile,
        ViewerCommand::PreviousFile,
        ViewerCommand::Reload,
        ViewerCommand::Close,
        ViewerCommand::Copy,
        ViewerCommand::SelectAll,
        ViewerCommand::Find,
        ViewerCommand::FindNext,
        ViewerCommand::FindPrevious,
        ViewerCommand::Caret,
        ViewerCommand::Text,
        ViewerCommand::Hex,
        ViewerCommand::Wrap,
        ViewerCommand::ZoomIn,
        ViewerCommand::ZoomOut,
        ViewerCommand::ZoomReset,
    ];

    /// The first `WM_COMMAND` ID, above `IDOK` and `IDCANCEL`, which `Enter`
    /// and `Esc` send through `IsDialogMessageW`.
    const FIRST_ID: u16 = 0x100;

    fn all() -> impl Iterator<Item = ViewerCommand> {
        Self::FIXED
            .into_iter()
            .chain(Encoding::all().map(ViewerCommand::Encoding))
    }

    /// Its `WM_COMMAND` ID in the Viewer's window.
    pub(crate) fn id(self) -> u16 {
        let index = Self::all()
            .position(|command| command == self)
            .expect("all holds every command");
        Self::FIRST_ID + u16::try_from(index).expect("the list is short")
    }

    pub(crate) fn from_id(id: u16) -> Option<ViewerCommand> {
        let index = id.checked_sub(Self::FIRST_ID)?;
        Self::all().nth(usize::from(index))
    }

    fn label(self, strings: &Strings) -> String {
        let key = match self {
            ViewerCommand::NextFile => "viewer.next_file",
            ViewerCommand::PreviousFile => "viewer.previous_file",
            ViewerCommand::Reload => "viewer.reload",
            ViewerCommand::Close => "viewer.close",
            ViewerCommand::Copy => "command.copy",
            ViewerCommand::SelectAll => "command.select_all",
            ViewerCommand::Find => "viewer.find",
            ViewerCommand::FindNext => "viewer.find_next",
            ViewerCommand::FindPrevious => "viewer.find_previous",
            ViewerCommand::Caret => "viewer.caret",
            ViewerCommand::Text => "viewer.text",
            ViewerCommand::Hex => "viewer.hex",
            ViewerCommand::Wrap => "viewer.wrap",
            ViewerCommand::ZoomIn => "viewer.zoom_in",
            ViewerCommand::ZoomOut => "viewer.zoom_out",
            ViewerCommand::ZoomReset => "viewer.zoom_reset",
            ViewerCommand::Encoding(Encoding::CodePage(CodePage::Ansi)) => "viewer.ansi",
            ViewerCommand::Encoding(Encoding::CodePage(CodePage::Oem)) => "viewer.oem",
            ViewerCommand::Encoding(Encoding::Utf8) => "viewer.utf8",
            ViewerCommand::Encoding(Encoding::Utf16Le) => "viewer.utf16_le",
            ViewerCommand::Encoding(Encoding::Utf16Be) => "viewer.utf16_be",
            ViewerCommand::Encoding(Encoding::CodePage(CodePage::Number(number))) => {
                let (_, name, script) = CODE_PAGES
                    .iter()
                    .find(|(each, ..)| *each == number)
                    .expect("the menu offers only the listed code pages");
                return strings.format("viewer.code_page", &[strings.text(script), name]);
            }
        };
        strings.text(key).to_owned()
    }

    /// The key the menu shows: the first of [`KEYS`] that runs it, and
    /// `Esc` for Close, which cancels a pending read first.
    fn keystroke(self) -> Option<Keystroke> {
        if self == ViewerCommand::Close {
            return Some(Keystroke::plain(Key::Escape));
        }
        KEYS.iter()
            .find(|(_, key)| *key == ViewerKey::Command(self))
            .map(|(keystroke, _)| *keystroke)
    }

    fn mark(self) -> Mark {
        match self {
            ViewerCommand::Text | ViewerCommand::Hex | ViewerCommand::Encoding(_) => Mark::Radio,
            ViewerCommand::Wrap | ViewerCommand::Caret => Mark::Check,
            _ => Mark::None,
        }
    }

    /// The icon, as the main window's command of the same name has.
    fn glyph(self) -> Option<Glyph> {
        match self {
            ViewerCommand::Close => Some(Glyph::Cancel),
            ViewerCommand::Copy => Some(Glyph::Copy),
            ViewerCommand::SelectAll => Some(Glyph::SelectAll),
            ViewerCommand::Find => Some(Glyph::Search),
            ViewerCommand::Reload => Some(Glyph::Refresh),
            _ => None,
        }
    }

    fn item(self, strings: &Strings) -> MenuNode {
        MenuNode::Item {
            id: self.id(),
            text: menu_label(
                &self.label(strings),
                self.keystroke().map(|keystroke| keystroke.text(strings)),
            ),
            mark: self.mark(),
            glyph: self.glyph(),
        }
    }
}

/// The Viewer's menu bar: File: Next file, Previous file | Reload | Close.
/// Edit: Copy | Select all | Find…, Find next, Find previous | Text cursor.
/// View: Text, Hex | Wrap long lines | Zoom. Encoding: ANSI, OEM, UTF-8,
/// UTF-16 LE, UTF-16 BE | Other code pages.
pub(crate) fn menu_bar(strings: &Strings) -> Vec<MenuNode> {
    use ViewerCommand as C;
    let menu = |label: &str, children| MenuNode::Submenu {
        text: strings.text(label).to_owned(),
        children,
        aligned_right: false,
    };
    let code_pages = CODE_PAGES
        .iter()
        .map(|(number, ..)| {
            C::Encoding(Encoding::CodePage(CodePage::Number(*number))).item(strings)
        })
        .collect();
    vec![
        menu(
            "menu.file",
            vec![
                C::NextFile.item(strings),
                C::PreviousFile.item(strings),
                MenuNode::Separator,
                C::Reload.item(strings),
                MenuNode::Separator,
                C::Close.item(strings),
            ],
        ),
        menu(
            "menu.edit",
            vec![
                C::Copy.item(strings),
                MenuNode::Separator,
                C::SelectAll.item(strings),
                MenuNode::Separator,
                C::Find.item(strings),
                C::FindNext.item(strings),
                C::FindPrevious.item(strings),
                MenuNode::Separator,
                C::Caret.item(strings),
            ],
        ),
        menu(
            "menu.view",
            vec![
                C::Text.item(strings),
                C::Hex.item(strings),
                MenuNode::Separator,
                C::Wrap.item(strings),
                MenuNode::Separator,
                menu(
                    "viewer.zoom",
                    vec![
                        C::ZoomIn.item(strings),
                        C::ZoomOut.item(strings),
                        MenuNode::Separator,
                        C::ZoomReset.item(strings),
                    ],
                ),
            ],
        ),
        menu(
            "viewer.encoding",
            vec![
                C::Encoding(Encoding::CodePage(CodePage::Ansi)).item(strings),
                C::Encoding(Encoding::CodePage(CodePage::Oem)).item(strings),
                C::Encoding(Encoding::Utf8).item(strings),
                C::Encoding(Encoding::Utf16Le).item(strings),
                C::Encoding(Encoding::Utf16Be).item(strings),
                MenuNode::Separator,
                menu("viewer.other_code_pages", code_pages),
            ],
        ),
    ]
}

/// What the Viewer's menus show of its state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MenuState {
    /// The shown file's mode and encoding, while a file shows.
    pub(crate) shown: Option<(Mode, Encoding)>,
    pub(crate) wrap: bool,
    pub(crate) selected: bool,
    pub(crate) caret: bool,
    /// [`Document::can_toggle_caret`].
    pub(crate) can_toggle_caret: bool,
    /// Which of the files the Viewer walks through shows, and how many
    /// there are.
    pub(crate) file: (usize, usize),
    pub(crate) zoom: u16,
}

/// Each menu item's ID, whether it is enabled and whether it is checked.
/// Wrap long lines is disabled in Hex, because a hex row is always 16
/// bytes; the text cursor too, because Hex has no text to place it in, and
/// while it is hidden and the page waits for a block, because it shows on
/// the page; Copy
/// without a selection, as in Notepad; Next file and Previous file past the
/// ends of the files; zoom at the ends of its range.
pub(crate) fn menu_states(state: MenuState) -> Vec<(u16, bool, bool)> {
    use ViewerCommand as C;
    let mode = state.shown.map(|(mode, _)| mode);
    let shown = state.shown.is_some();
    let (index, count) = state.file;
    ViewerCommand::all()
        .map(|command| {
            let (enabled, checked) = match command {
                C::NextFile => (index + 1 < count, false),
                C::PreviousFile => (index > 0, false),
                C::Reload | C::Close => (true, false),
                C::Copy => (state.selected, false),
                C::SelectAll | C::Find | C::FindNext | C::FindPrevious => (shown, false),
                C::Caret => (state.can_toggle_caret, state.caret),
                C::Text => (shown, mode == Some(Mode::Text)),
                C::Hex => (shown, mode == Some(Mode::Hex)),
                C::Wrap => (mode != Some(Mode::Hex), state.wrap),
                C::ZoomIn => (state.zoom < *ZOOM.end(), false),
                C::ZoomOut => (state.zoom > *ZOOM.start(), false),
                C::ZoomReset => (state.zoom != DEFAULT_ZOOM, false),
                C::Encoding(encoding) => {
                    (shown, state.shown.map(|(_, shown)| shown) == Some(encoding))
                }
            };
            (command.id(), enabled, checked)
        })
        .collect()
}

/// What a key does in a Viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewerKey {
    /// `Esc` cancels a pending read, and otherwise closes the Viewer.
    Cancel,
    Command(ViewerCommand),
    /// Moves the page, or the caret while it shows; with `extend`, as with
    /// Shift, the caret shows and the selection follows it.
    Move {
        motion: Motion,
        extend: bool,
    },
}

/// The Viewer's keys, those of Total Commander's Lister (DESIGN.md: The
/// Viewer). The first key of a command is the one its menu item shows.
const KEYS: &[(Keystroke, ViewerKey)] = {
    use ViewerCommand as C;
    use ViewerKey::{Cancel, Command};
    const fn shift(key: Key) -> Keystroke {
        Keystroke {
            shift: true,
            ..Keystroke::plain(key)
        }
    }
    const fn go(motion: Motion, extend: bool) -> ViewerKey {
        ViewerKey::Move { motion, extend }
    }
    const ANSI: Encoding = Encoding::CodePage(CodePage::Ansi);
    const OEM: Encoding = Encoding::CodePage(CodePage::Oem);
    &[
        (Keystroke::plain(Key::Escape), Cancel),
        (Keystroke::plain(Key::Letter(b'Q')), Command(C::Close)),
        (Keystroke::plain(Key::Function(10)), Command(C::Close)),
        (Keystroke::plain(Key::Letter(b'N')), Command(C::NextFile)),
        (Keystroke::alt(Key::Right), Command(C::NextFile)),
        (
            Keystroke::plain(Key::Letter(b'P')),
            Command(C::PreviousFile),
        ),
        (Keystroke::alt(Key::Left), Command(C::PreviousFile)),
        (Keystroke::plain(Key::Function(2)), Command(C::Reload)),
        (Keystroke::ctrl(Key::Letter(b'C')), Command(C::Copy)),
        (Keystroke::ctrl(Key::Letter(b'A')), Command(C::SelectAll)),
        (Keystroke::ctrl(Key::Letter(b'F')), Command(C::Find)),
        (Keystroke::plain(Key::Function(7)), Command(C::Find)),
        (Keystroke::plain(Key::Function(3)), Command(C::FindNext)),
        (shift(Key::Function(3)), Command(C::FindPrevious)),
        (Keystroke::plain(Key::Function(6)), Command(C::Caret)),
        (Keystroke::plain(Key::Digit(b'1')), Command(C::Text)),
        (Keystroke::plain(Key::Digit(b'3')), Command(C::Hex)),
        (Keystroke::plain(Key::Letter(b'W')), Command(C::Wrap)),
        (Keystroke::ctrl(Key::Plus), Command(C::ZoomIn)),
        (Keystroke::plain(Key::NumPlus), Command(C::ZoomIn)),
        (Keystroke::ctrl(Key::NumPlus), Command(C::ZoomIn)),
        (Keystroke::ctrl(Key::Minus), Command(C::ZoomOut)),
        (Keystroke::plain(Key::NumMinus), Command(C::ZoomOut)),
        (Keystroke::ctrl(Key::NumMinus), Command(C::ZoomOut)),
        (Keystroke::ctrl(Key::Digit(b'0')), Command(C::ZoomReset)),
        (
            Keystroke::plain(Key::Letter(b'A')),
            Command(C::Encoding(ANSI)),
        ),
        (
            Keystroke::plain(Key::Letter(b'S')),
            Command(C::Encoding(OEM)),
        ),
        (
            Keystroke::plain(Key::Digit(b'7')),
            Command(C::Encoding(Encoding::Utf8)),
        ),
        (
            Keystroke::plain(Key::Digit(b'6')),
            Command(C::Encoding(Encoding::Utf16Le)),
        ),
        (Keystroke::plain(Key::Up), go(Motion::Up, false)),
        (shift(Key::Up), go(Motion::Up, true)),
        (Keystroke::plain(Key::Down), go(Motion::Down, false)),
        (shift(Key::Down), go(Motion::Down, true)),
        (Keystroke::plain(Key::PageUp), go(Motion::PageUp, false)),
        (shift(Key::PageUp), go(Motion::PageUp, true)),
        (Keystroke::plain(Key::PageDown), go(Motion::PageDown, false)),
        (shift(Key::PageDown), go(Motion::PageDown, true)),
        (Keystroke::plain(Key::Left), go(Motion::Left, false)),
        (shift(Key::Left), go(Motion::Left, true)),
        (Keystroke::plain(Key::Right), go(Motion::Right, false)),
        (shift(Key::Right), go(Motion::Right, true)),
        (Keystroke::plain(Key::Home), go(Motion::Home, false)),
        (shift(Key::Home), go(Motion::Home, true)),
        (Keystroke::plain(Key::End), go(Motion::End, false)),
        (shift(Key::End), go(Motion::End, true)),
        (Keystroke::ctrl(Key::Home), go(Motion::FileStart, false)),
        (
            Keystroke::ctrl_shift(Key::Home),
            go(Motion::FileStart, true),
        ),
        (Keystroke::ctrl(Key::End), go(Motion::FileEnd, false)),
        (Keystroke::ctrl_shift(Key::End), go(Motion::FileEnd, true)),
    ]
};

/// The Viewer's key that `keystroke` presses, if any.
pub(crate) fn viewer_key(keystroke: Keystroke) -> Option<ViewerKey> {
    KEYS.iter()
        .find(|(each, _)| *each == keystroke)
        .map(|(_, key)| *key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latin1(_: CodePage, bytes: &[u8]) -> String {
        bytes.iter().map(|&b| char::from(b)).collect()
    }

    /// Gives `document` the blocks of `file` it asks for until its page is
    /// complete, and returns the blocks it asked for.
    fn settle(document: &mut Document, file: &[u8]) -> Vec<u64> {
        let mut read = Vec::new();
        while let Some(index) = document.advance(&latin1) {
            assert!(read.len() < 1_000, "the page keeps asking for blocks");
            read.push(index);
            let start = usize::try_from(index).unwrap() * BLOCK_BYTES;
            let end = (start + BLOCK_BYTES).min(file.len());
            document.receive(index, file.get(start..end).unwrap_or_default().to_vec());
        }
        read
    }

    fn open(file: &[u8], wrap: bool, columns: usize, height: usize) -> Document {
        let first = file[..file.len().min(BLOCK_BYTES)].to_vec();
        let mut document = Document::new(file.len() as u64, first, wrap);
        document.set_area(columns, height);
        settle(&mut document, file);
        document
    }

    fn texts(document: &Document) -> Vec<String> {
        document
            .rows
            .iter()
            .map(|row| row.text().to_owned())
            .collect()
    }

    #[test]
    fn a_page_that_needs_more_blocks_than_are_kept_reads_each_once() {
        // 120 lines of 10,000 characters: about 19 blocks for one page.
        let file = format!("{}\n", "x".repeat(10_000)).repeat(120).into_bytes();
        let first = file[..BLOCK_BYTES].to_vec();
        let mut document = Document::new(file.len() as u64, first, false);
        document.set_area(80, 120);
        let read = settle(&mut document, &file);
        assert!(read.windows(2).all(|pair| pair[0] < pair[1]), "{read:?}");
        assert_eq!(document.rows.len(), 120);
        assert!(document.end_shown());
    }

    #[test]
    fn up_returns_through_wrapped_rows_to_where_down_came_from() {
        let file = format!("{}\n{}\n{}", "a".repeat(25), "b".repeat(5), "c".repeat(30));
        let mut document = open(file.as_bytes(), true, 10, 3);
        let first = texts(&document);
        assert_eq!(first, ["a".repeat(10), "a".repeat(10), "a".repeat(5)]);
        // Seven rows, three shown: four moves reach the last page, and a
        // fifth moves no further.
        for _ in 0..5 {
            document.line_down();
            settle(&mut document, file.as_bytes());
        }
        assert_eq!(texts(&document), vec!["c".repeat(10); 3]);
        assert!(document.end_shown());
        for _ in 0..4 {
            document.line_up();
            settle(&mut document, file.as_bytes());
        }
        assert_eq!(texts(&document), first);
        assert_eq!(document.top, document.start_row());
    }

    #[test]
    fn end_and_a_jump_near_the_end_show_a_full_last_page() {
        let file: String = (0..10).map(|n| format!("line{n}\r\n")).collect();
        let last = ["line6", "line7", "line8", "line9"];
        let mut document = open(file.as_bytes(), true, 20, 4);
        document.end();
        settle(&mut document, file.as_bytes());
        assert_eq!(texts(&document), last);
        document.home();
        let len = file.len() as u64;
        document.jump(viewer::scroll_position(len, len - 3));
        settle(&mut document, file.as_bytes());
        assert_eq!(texts(&document), last);
        // A file that the area holds whole disables the scroll bar.
        let small = open(b"one\ntwo", true, 20, 4);
        let bar = small.scroll_bar();
        assert!(bar.page > bar.max.unsigned_abs());
    }

    #[test]
    fn a_file_shorter_than_its_size_ends_where_its_bytes_end() {
        // The size said 200,000 bytes, but only 65,546 can be read.
        let file = "x\n".repeat(32_773).into_bytes();
        let first = file[..BLOCK_BYTES].to_vec();
        let mut document = Document::new(200_000, first, true);
        document.set_area(20, 3);
        settle(&mut document, &file);
        document.end();
        settle(&mut document, &file);
        assert_eq!(document.blocks.len, file.len() as u64);
        assert_eq!(texts(&document), ["x"; 3]);
        assert!(document.end_shown());
    }

    /// Reload of a file that a program still writes shows its new end when
    /// the old end showed, as a log is read.
    #[test]
    fn reload_at_the_end_shows_the_new_end() {
        let old: String = (0..3).map(|n| format!("line{n}\n")).collect();
        let document = open(old.as_bytes(), true, 20, 4);
        let at = document.reopening();
        let grown: String = (0..10).map(|n| format!("line{n}\n")).collect();
        let mut reloaded = Document::new(grown.len() as u64, grown.clone().into_bytes(), true);
        reloaded.set_area(20, 4);
        reloaded.reopen(at);
        settle(&mut reloaded, grown.as_bytes());
        assert_eq!(texts(&reloaded), ["line6", "line7", "line8", "line9"]);
    }

    /// The clipboard gets the text a person pastes: no byte order mark in
    /// Text, the rows as shown in Hex, and nothing for a file whose text
    /// could pass the limit, before a byte of it is read.
    #[test]
    fn copy_gives_the_shown_text_of_a_file_within_the_limit() {
        let file = b"\xEF\xBB\xBFone\r\ntwo\n";
        let mut document = open(file, true, 80, 4);
        document.select_all();
        let copy = document.copy().unwrap().expect("a small file copies");
        assert_eq!(copy.text(file, &latin1), "one\r\ntwo\n");
        document.set_mode(Mode::Hex);
        let copy = document.copy().unwrap().expect("a small file copies");
        let bytes = [b'a'; 17];
        assert_eq!(
            copy.text(&bytes, &latin1),
            format!(
                "{}\r\n{}",
                viewer::hex_row(17, 0, &bytes),
                viewer::hex_row(17, 16, &bytes[16..])
            )
        );
        let mut large = Document::new(COPY_UNITS + 1, vec![b'x'; BLOCK_BYTES], true);
        large.select_all();
        assert_eq!(
            large.copy().unwrap().map(|_| ()),
            Err(TooLarge::Text(COPY_UNITS))
        );
    }

    /// A drag over wrapped rows shows the selection on each row, with the
    /// line end it holds, and copies the text it shows, tab included.
    #[test]
    fn a_drag_over_wrapped_rows_copies_what_it_shows() {
        // Rows of 8: "abcdefgh", "ij" and the tab, "k", "second l".
        let file = b"abcdefghij\tk\r\nsecond line\n";
        let mut document = open(file, true, 8, 4);
        document.press(0, 3, false);
        document.drag(3, 3);
        let selected: Vec<_> = document
            .shown()
            .into_iter()
            .map(|row| row.selected[0].clone())
            .collect();
        assert_eq!(selected, [3..8, 0..8, 0..2, 0..3]);
        let copy = document.copy().unwrap().expect("a small selection copies");
        let read = &file[copy.bytes.start as usize..copy.bytes.end as usize];
        assert_eq!(copy.text(read, &latin1), "defghij\tk\r\nsec");
    }

    /// Shift with the arrows selects from where the caret was, through
    /// wrapped rows and lines, and a caret below the page scrolls it, so the
    /// caret never leaves the screen.
    #[test]
    fn shift_with_the_arrows_selects_and_the_page_follows_the_caret() {
        let file = b"abcdefghij\nxy\nlast\n";
        let mut document = open(file, true, 8, 2);
        document.go(Motion::Right, true);
        settle(&mut document, file);
        document.go(Motion::Down, true);
        settle(&mut document, file);
        // From the text's start, past "a", down to the same column of the
        // wrapped row "ij", before "j".
        let copy = document.copy().unwrap().expect("a small selection copies");
        let read = &file[copy.bytes.start as usize..copy.bytes.end as usize];
        assert_eq!(copy.text(read, &latin1), "abcdefghi");
        document.go(Motion::Down, true);
        settle(&mut document, file);
        assert_eq!(texts(&document), ["ij", "xy"]);
        assert_eq!(document.caret_cell(), Some((1, 1)));
        let copy = document.copy().unwrap().expect("a small selection copies");
        let read = &file[copy.bytes.start as usize..copy.bytes.end as usize];
        assert_eq!(copy.text(read, &latin1), "abcdefghij\nx");
    }

    #[test]
    fn each_key_has_one_meaning() {
        for (index, (keystroke, _)) in KEYS.iter().enumerate() {
            assert!(
                !KEYS[..index].iter().any(|(each, _)| each == keystroke),
                "{keystroke:?} has two meanings"
            );
        }
    }

    #[test]
    fn cancelled_move_does_not_request_its_block_again_after_layout() {
        let mut document = Document::new((BLOCK_BYTES * 3) as u64, vec![0; BLOCK_BYTES], false);
        document.set_area(80, 3);
        assert_eq!(document.advance(&latin1), None);
        document.end();
        assert_eq!(document.advance(&latin1), Some(2));
        document.cancel();
        document.set_area(80, 5);
        assert_eq!(document.advance(&latin1), None);
        document.end();
        assert_eq!(document.advance(&latin1), Some(2));
    }

    /// An event that comes while a move of the caret, Up or a jump waits
    /// for a block leaves a consistent page once the blocks arrive: nothing
    /// panics, the rows have the width and wrapping of now, a waiting move
    /// of the caret leaves the caret and the selection as the event set
    /// them, and a caret never shows on a page that then moves away.
    #[test]
    fn events_during_a_wait_leave_a_consistent_page() {
        // 12,000 lines of 99 characters, 19 blocks: the end and the middle
        // are not among the blocks the first page reads.
        let file = format!("{}\n", "x".repeat(99)).repeat(12_000).into_bytes();
        let start = TextPoint { line: 0, char: 0 };
        // The second row of the first line, wrapped at 40 columns.
        let click = TextPoint { line: 0, char: 45 };
        type Step = fn(&mut Document);
        let waits: [Step; 3] = [
            |d: &mut Document| {
                d.toggle_caret();
                d.go(Motion::FileEnd, false);
            },
            Document::end,
            |d: &mut Document| d.jump(viewer::scroll_position(d.blocks.len, d.blocks.len / 2)),
        ];
        // Each event, and where it leaves the caret of the move.
        let events: [(Step, Option<TextPoint>); 6] = [
            (Document::toggle_caret, None),
            (|d: &mut Document| d.set_wrap(false), Some(start)),
            (|d: &mut Document| d.set_area(30, 4), Some(start)),
            (|d: &mut Document| d.press(1, 5, false), Some(click)),
            (Document::cancel, Some(start)),
            (
                |d: &mut Document| d.jump(viewer::scroll_position(d.blocks.len, d.blocks.len / 3)),
                Some(start),
            ),
        ];
        for (index, wait) in waits.iter().enumerate() {
            for (event, caret) in &events {
                let mut document = open(&file, true, 40, 5);
                wait(&mut document);
                assert!(document.advance(&latin1).is_some(), "the task waits");
                event(&mut document);
                document.resume();
                settle(&mut document, &file);
                assert!(document.settled());
                let wrapped = document.wrap && document.mode == Mode::Text;
                for row in &document.rows {
                    if wrapped {
                        assert_eq!(row.line.columns, document.columns);
                    } else {
                        assert_eq!(row.line.rows.len(), 1);
                    }
                }
                if index == 0 {
                    assert_eq!(document.caret.map(|caret| caret.point), *caret);
                    let selection = if *caret == Some(click) {
                        Selection::Text {
                            anchor: click,
                            focus: click,
                        }
                    } else {
                        Selection::None
                    };
                    assert_eq!(document.selection, selection);
                } else {
                    assert!(document.caret.is_none(), "F6 waits for the page");
                }
            }
        }
    }
}
