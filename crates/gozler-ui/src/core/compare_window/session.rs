//! The state of one compare window (DESIGN.md: Compare by content): the
//! comparison that shows, the job of the window's worker, edit mode's
//! changes and the saves. [`Session::on`] is its only transition: the
//! window passes each event and runs the effects that it returns.
//!
//! The rules it keeps:
//! - One job runs at a time, and [`Job`] says which, so "Comparing…" and
//!   what the sides take follow from it.
//! - The rows that show map rows to lines only while they were built for
//!   the lines as they are, in the sides' order, with the options chosen.
//!   A command that reads rows runs only then, which [`Fresh`] proves; it
//!   waits for such rows otherwise.
//! - Typing is read back through panes that match the controls and the
//!   lines, and the sides take no typing without them.
//! - A comparison's options describe what shows only once it succeeds.
//! - The history holds exactly the changes that are not on the disk: each
//!   save forgets only the changes it wrote.
//! - The window closes, or compares other files, only after the person
//!   answered about every change.

use super::{CompareCommand, Note};
use crate::core::compare::{self, History, Matching, Pane, Refused, ShownRows, Text};
use crate::core::panel::Side;
use crate::core::text;
use crate::core::viewer::{Encoding, Mode};
use crate::core::work::Retirement;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

/// How the rows are built from the lines or the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowOptions {
    pub(crate) matching: Matching,
    /// Show only differences, with its extra lines.
    pub(crate) only_differences: Option<u32>,
    /// Counts the changes of the system colours, which the rows' marks
    /// are drawn in.
    colours: u32,
}

impl RowOptions {
    /// Whether rows of `mode` built with `self` are those of `other`: case
    /// and spaces do not apply to Hex.
    fn builds_as(self, other: RowOptions, mode: Mode) -> bool {
        self.only_differences == other.only_differences
            && self.colours == other.colours
            && (mode == Mode::Hex || self.matching == other.matching)
    }
}

/// What a comparison reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    /// The two files; `None` while the worker chooses them from the
    /// panels' selections, as a window's first comparison does.
    pub(crate) paths: Option<[Vec<u16>; 2]>,
    /// View > Hex.
    pub(crate) hex: bool,
    /// The encodings that the person chose; `None` detects one.
    pub(crate) encodings: [Option<Encoding>; 2],
    pub(crate) rows: RowOptions,
}

/// The rows that show, and the RTF of the sides and of their line numbers.
#[derive(Debug, Default)]
pub(crate) struct View {
    pub(crate) rows: ShownRows,
    pub(crate) differences: usize,
    pub(crate) sides: [String; 2],
    pub(crate) numbers: [String; 2],
}

/// What a Hex comparison found, from which other options build the rows
/// that show without reading the files again.
#[derive(Debug)]
pub(crate) struct HexFound {
    /// Why the files are compared as Hex, whichever view was chosen.
    pub(crate) note: Option<Note>,
    /// The hex rows that differ, in order.
    pub(crate) differing: Vec<u64>,
    /// Whether `differing` holds every row that differs.
    pub(crate) complete: bool,
    /// The runs of rows that differ.
    pub(crate) differences: usize,
}

/// What the worker read of two files.
#[derive(Debug)]
pub(crate) enum Content {
    /// Each side's text. Edit mode changes the session's copy, so a job that
    /// holds the text it was given keeps it.
    Text([Arc<Text>; 2]),
    Hex(Arc<HexFound>),
}

/// Two files compared, as the worker gives them.
#[derive(Debug)]
pub(crate) struct Compared {
    pub(crate) paths: [Vec<u16>; 2],
    pub(crate) sizes: [u64; 2],
    pub(crate) content: Content,
    pub(crate) view: View,
    /// Whether the files hold the same bytes, which the options that
    /// compare lines as equal do not say.
    pub(crate) identical: bool,
}

/// The comparison that shows.
#[derive(Debug)]
pub(crate) struct Showing {
    paths: [Vec<u16>; 2],
    /// View > Hex, as its comparison asked.
    hex: bool,
    /// The encodings that its comparison asked for.
    chosen: [Option<Encoding>; 2],
    sizes: [u64; 2],
    content: Content,
    view: View,
    /// The options that the rows of `view` were built with.
    rows: RowOptions,
    /// The lines that Resync from here paired.
    anchors: Vec<(usize, usize)>,
    /// The [`Session`]'s generation that the rows of `view` were built for.
    generation: u64,
}

impl Showing {
    pub(crate) fn paths(&self) -> &[Vec<u16>; 2] {
        &self.paths
    }

    pub(crate) fn sizes(&self) -> [u64; 2] {
        self.sizes
    }

    pub(crate) fn view(&self) -> &View {
        &self.view
    }

    pub(crate) fn mode(&self) -> Mode {
        match self.content {
            Content::Text(_) => Mode::Text,
            Content::Hex(_) => Mode::Hex,
        }
    }

    /// `side`'s text, in Text.
    pub(crate) fn text(&self, side: Side) -> Option<&Text> {
        match &self.content {
            Content::Text(texts) => Some(&texts[side.index()]),
            Content::Hex(_) => None,
        }
    }

    /// Why the files are compared as Hex.
    pub(crate) fn note(&self) -> Option<Note> {
        match &self.content {
            Content::Text(_) => None,
            Content::Hex(found) => found.note,
        }
    }

    /// How the lines that show were matched.
    pub(crate) fn matching(&self) -> Matching {
        self.rows.matching
    }

    /// Each side's encoding, in Text.
    pub(crate) fn encodings(&self) -> [Option<Encoding>; 2] {
        Side::BOTH.map(|side| self.text(side).map(|text| text.form.encoding))
    }

    pub(crate) fn editable(&self, side: Side) -> bool {
        self.text(side).is_some_and(|text| text.editable)
    }

    /// The name of `side`'s file.
    pub(crate) fn name(&self, side: Side) -> String {
        text::name_of(&String::from_utf16_lossy(&self.paths[side.index()])).to_owned()
    }

    fn swap(&mut self) {
        self.paths.swap(0, 1);
        self.chosen.swap(0, 1);
        self.sizes.swap(0, 1);
        if let Content::Text(texts) = &mut self.content {
            texts.swap(0, 1);
        }
        self.view.rows = std::mem::take(&mut self.view.rows).swapped();
        self.view.sides.swap(0, 1);
        self.view.numbers.swap(0, 1);
        for anchor in &mut self.anchors {
            *anchor = (anchor.1, anchor.0);
        }
    }

    /// How many values dropping it frees, which decides where it drops.
    fn items(&self) -> usize {
        let content = match &self.content {
            Content::Text([left, right]) => left.lines.len() + right.lines.len(),
            Content::Hex(found) => found.differing.len(),
        };
        self.view.rows.rows.len() + content
    }
}

/// What the window's worker does.
#[derive(Debug)]
enum Job {
    Idle,
    /// The rows align again once the person stops typing, when the
    /// window's timer says so.
    Due,
    /// The worker builds the rows for `generation` with `rows`.
    Aligning {
        generation: u64,
        rows: RowOptions,
    },
    /// The worker reads and compares files.
    Comparing(Request),
}

/// What follows the question about changes, and the saves it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Then {
    Stay,
    Close,
    /// Settles the changes for Exit, which closes every window once each
    /// has settled, so a Cancel in a later window leaves this one open.
    Exit,
    Compare(Request),
}

impl Then {
    /// What follows the saves when `later` is asked for while `self`
    /// waits for them: closing outlasts the rest, and the person's Close
    /// outlasts Exit's.
    fn and(self, later: Then) -> Then {
        match (self, later) {
            (Then::Close, _) | (_, Then::Close) => Then::Close,
            (Then::Exit, _) | (_, Then::Exit) => Then::Exit,
            (earlier, Then::Stay) => earlier,
            (_, later) => later,
        }
    }
}

/// The question that is open.
#[derive(Debug)]
struct Asking {
    then: Then,
    sides: Vec<Side>,
    /// The last change of each side that the saves wrote when it was
    /// asked; the question is about the changes after them.
    marks: [Option<u64>; 2],
}

/// The person's answer to Save's question.
#[derive(Debug)]
pub(crate) enum Answer {
    Cancel,
    /// The sides asked about lose their changes, unless the question came
    /// from Save.
    DontSave,
    /// Writes `files`; the other sides asked about lose their changes as
    /// with Don't save.
    Save(Vec<SaveFile>),
}

/// A side's file that Save writes.
#[derive(Debug)]
pub(crate) struct SaveFile {
    pub(crate) side: Side,
    pub(crate) path: Vec<u16>,
    pub(crate) bytes: Vec<u8>,
}

/// A file that a save wrote, or failed to write.
#[derive(Debug)]
pub(crate) struct SavedFile {
    pub(crate) side: Side,
    pub(crate) path: Vec<u16>,
    pub(crate) saved: bool,
}

/// Where the question about changes left the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    Done,
    /// What was asked for follows the saves that run.
    Saving,
    Cancelled,
    /// An earlier request's question is still open and decides; this
    /// request changed nothing.
    Asking,
}

/// Where the caret goes when the rows show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Caret {
    FirstDifference,
    /// On the line it was on.
    Kept,
}

/// The rows that the worker builds again.
#[derive(Debug)]
pub(crate) struct Align {
    /// The [`Session`]'s generation, which the rows are built for.
    pub(crate) generation: u64,
    pub(crate) rows: RowOptions,
    pub(crate) input: AlignInput,
}

#[derive(Debug)]
pub(crate) enum AlignInput {
    /// The lines, with the lines that Resync from here paired.
    Text {
        texts: [Arc<Text>; 2],
        anchors: Vec<(usize, usize)>,
    },
    /// The files, of `sizes` bytes, and what their Hex comparison found.
    Hex {
        paths: [Vec<u16>; 2],
        sizes: [u64; 2],
        found: Arc<HexFound>,
    },
}

/// Proves that the rows that show map rows to the lines as they are, for
/// the command that the effect carrying it runs; only [`Session::on`]
/// gives one.
#[derive(Debug)]
pub(crate) struct Fresh(u64);

/// What the window does for the session.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Reads and compares the files of the request, in place of the job
    /// that runs.
    Compare(Request),
    /// Builds the rows again, in place of the job that runs.
    Align(Align),
    /// Stops the job that runs.
    Cancel,
    /// Starts the timer after which the rows align again.
    Wait,
    /// Gives the controls the rows that show.
    Present(Caret),
    /// Reads both sides' text back as [`Event::Panes`].
    ReadPanes(Fresh),
    /// Runs a command that reads the rows.
    Run(CompareCommand, Fresh),
    /// The files changed sides, so the path boxes swap too.
    Swapped,
    /// Asks Save's question about the changes of these sides, and answers
    /// with [`Event::Answered`].
    Ask(Vec<Side>),
    /// Writes the files, and answers with [`Event::Saved`].
    Save(Vec<SaveFile>),
    /// Save as gave the side another file.
    Renamed(Side),
    /// Show only differences, with its extra lines, went back to what the
    /// rows that show were built with, which the window's controls and
    /// saved state show too.
    OnlyDifferences(Option<u32>),
    Close,
    /// Where the question about changes left the window.
    Settled(Settled),
    /// Exit no longer goes on: the person cancelled, or a save it waited
    /// for failed.
    ExitStopped,
    /// Values that may be large, which the window drops off its thread.
    Retire(Retirement),
}

/// What happened to the window.
#[derive(Debug)]
pub(crate) enum Event {
    /// The window's first comparison, of the files that the panels'
    /// selections choose.
    Choose,
    /// Compare, or `Enter` in a path box.
    Compare([Vec<u16>; 2]),
    /// View > Text or Hex.
    View {
        hex: bool,
    },
    Encoding(Side, Encoding),
    /// The check boxes or the extra lines changed.
    Options {
        matching: Matching,
        only_differences: Option<u32>,
    },
    /// The system colours changed.
    Colours,
    /// `Esc` or Cancel while a comparison runs.
    Cancel,
    /// The person typed in `side`, whose control now holds `text` with the
    /// caret at `caret`.
    Edited {
        side: Side,
        text: Vec<u16>,
        caret: usize,
    },
    /// The timer after typing.
    Timer,
    Undo,
    /// A command that reads the rows.
    Command(CompareCommand),
    /// Copy to right or left of `rows`.
    Copy {
        fresh: Fresh,
        to: Side,
        rows: Range<usize>,
    },
    /// Resync from here, with each side's caret in `rows`.
    Resync {
        fresh: Fresh,
        rows: [usize; 2],
    },
    Swap(Fresh),
    EditMode(Fresh),
    /// Both sides' text, as [`Effect::ReadPanes`] asked.
    Panes {
        fresh: Fresh,
        texts: [Vec<u16>; 2],
    },
    Compared(Box<Compared>),
    Realigned {
        generation: u64,
        view: View,
    },
    /// The job ended without rows: the files could not be read or chosen.
    Failed,
    Save,
    Close,
    /// Exit closes the window, after asking about its changes.
    Exit,
    Answered(Answer),
    Saved(Vec<SavedFile>),
    /// Exit stopped in another window.
    ExitCancelled,
}

/// The last change of each side that a save writes.
#[derive(Clone, Copy, Debug, Default)]
struct Batch {
    marks: [Option<u64>; 2],
    /// The sides swapped since the worker took the files, so its results
    /// name the other side.
    flipped: bool,
}

#[derive(Debug, Default)]
struct Saves {
    running: Option<Batch>,
    /// Saves asked for while another runs, which start in turn.
    waiting: VecDeque<(Batch, Vec<SaveFile>)>,
}

impl Saves {
    /// The last change of each side that a save writes.
    fn writing(&self) -> [Option<u64>; 2] {
        let mut marks = [None, None];
        let batches = (self.running.iter()).chain(self.waiting.iter().map(|(batch, _)| batch));
        for batch in batches {
            for (mark, written) in marks.iter_mut().zip(batch.marks) {
                *mark = (*mark).max(written);
            }
        }
        marks
    }

    fn start(&mut self, marks: [Option<u64>; 2], files: Vec<SaveFile>, effects: &mut Vec<Effect>) {
        let batch = Batch {
            marks,
            flipped: false,
        };
        if self.running.is_some() {
            self.waiting.push_back((batch, files));
        } else {
            self.running = Some(batch);
            effects.push(Effect::Save(files));
        }
    }

    fn swap(&mut self) {
        if let Some(batch) = &mut self.running {
            batch.marks.swap(0, 1);
            batch.flipped = !batch.flipped;
        }
        for (batch, files) in &mut self.waiting {
            batch.marks.swap(0, 1);
            for file in files {
                file.side = file.side.other();
            }
        }
    }
}

pub(crate) struct Session {
    job: Job,
    showing: Option<Showing>,
    /// Counts the changes that the rows are built again for: of the lines,
    /// of the sides' order, of the lines that Resync paired, and an edit
    /// that the controls show and the lines do not hold.
    generation: u64,
    /// The options that the person chose.
    rows: RowOptions,
    editing: bool,
    /// Read typing back, while they match the controls and the lines.
    panes: Option<[Pane; 2]>,
    history: History,
    saves: Saves,
    asking: Option<Asking>,
    /// What follows the saves that run.
    after: Option<Then>,
    /// A command that waits for fresh rows.
    queued: Option<CompareCommand>,
    retired: Retirement,
}

impl Session {
    pub(crate) fn new(matching: Matching, only_differences: Option<u32>) -> Session {
        Session {
            job: Job::Idle,
            showing: None,
            generation: 0,
            rows: RowOptions {
                matching,
                only_differences,
                colours: 0,
            },
            editing: false,
            panes: None,
            history: History::default(),
            saves: Saves::default(),
            asking: None,
            after: None,
            queued: None,
            retired: Retirement::default(),
        }
    }

    pub(crate) fn showing(&self) -> Option<&Showing> {
        self.showing.as_ref()
    }

    /// Hands over the lines and the rows as the window closes, so they are
    /// dropped off the window's thread, because they can be whole files.
    pub(crate) fn close(&mut self) -> Retirement {
        let mut retired = std::mem::take(&mut self.retired);
        let showing = self.showing.take();
        let history = std::mem::take(&mut self.history);
        let items = showing.as_ref().map_or(0, Showing::items) + history.lines();
        retired.push((showing, history, self.panes.take()), items);
        retired
    }

    /// Whether files are read and compared, which "Comparing…" says.
    pub(crate) fn comparing(&self) -> bool {
        matches!(self.job, Job::Comparing(_))
    }

    pub(crate) fn editing(&self) -> bool {
        self.editing
    }

    /// Whether edit mode can start: the files compare as Text, and one of
    /// them writes back as it was read.
    pub(crate) fn can_edit(&self) -> bool {
        (self.showing.as_ref())
            .is_some_and(|showing| Side::BOTH.into_iter().any(|side| showing.editable(side)))
    }

    /// Whether `side` takes typing now.
    pub(crate) fn typing(&self, side: Side) -> bool {
        self.panes.is_some()
            && self.can_change()
            && (self.showing.as_ref()).is_some_and(|showing| showing.editable(side))
    }

    pub(crate) fn has_changes(&self) -> bool {
        !self.history.is_empty()
    }

    pub(crate) fn can_undo(&self) -> bool {
        self.can_change() && self.history.can_take_last(self.saves.writing())
    }

    pub(crate) fn saving(&self) -> bool {
        self.saves.running.is_some()
    }

    /// The window's one transition: `event` changes the session, and the
    /// window runs the effects, in order.
    pub(crate) fn on(&mut self, event: Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            Event::Choose => {
                let request = Request {
                    paths: None,
                    hex: false,
                    encodings: [None, None],
                    rows: self.rows,
                };
                self.start_compare(request, &mut effects);
            }
            Event::Compare(paths) => {
                let base = self.request();
                let encodings = match &base {
                    Some(base) if base.paths.as_ref() == Some(&paths) => base.encodings,
                    _ => [None, None],
                };
                let request = Request {
                    paths: Some(paths),
                    hex: base.is_some_and(|base| base.hex),
                    encodings,
                    rows: self.rows,
                };
                self.settle(Then::Compare(request), &mut effects);
            }
            Event::View { hex } => {
                if let Some(mut request) = self.request() {
                    request.hex = hex;
                    self.settle(Then::Compare(request), &mut effects);
                }
            }
            Event::Encoding(side, encoding) => {
                // Reading a side again would lose its changes.
                if !self.has_changes()
                    && self.asking.is_none()
                    && let Some(mut request) = self.request()
                {
                    // A choice shows Text, also from Hex, as in the Viewer.
                    request.encodings[side.index()] = Some(encoding);
                    request.hex = false;
                    self.start_compare(request, &mut effects);
                }
            }
            Event::Options {
                matching,
                only_differences,
            } => {
                self.rows.matching = matching;
                self.rows.only_differences = only_differences;
            }
            Event::Colours => self.rows.colours = self.rows.colours.wrapping_add(1),
            Event::Cancel => {
                if self.comparing() {
                    self.job = Job::Idle;
                    effects.push(Effect::Cancel);
                }
            }
            Event::Edited { side, text, caret } => self.edited(side, text, caret, &mut effects),
            Event::Timer => {
                if matches!(self.job, Job::Due) {
                    self.job = Job::Idle;
                }
            }
            Event::Undo => self.undo(),
            Event::Command(command) => self.command(command, &mut effects),
            Event::Copy { fresh, to, rows } => {
                if self.holds(&fresh) {
                    self.copy(to, rows);
                }
            }
            Event::Resync { fresh, rows } => {
                if self.holds(&fresh) {
                    self.resync(rows);
                }
            }
            Event::Swap(fresh) => {
                if self.holds(&fresh) {
                    self.swap(&mut effects);
                }
            }
            Event::EditMode(fresh) => {
                if self.holds(&fresh) {
                    self.toggle_edit(&mut effects);
                }
            }
            Event::Panes {
                fresh,
                texts: [left, right],
            } => {
                if self.holds(&fresh)
                    && self.editing
                    && let Some(showing) = &self.showing
                {
                    let rows = &showing.view.rows.rows;
                    self.panes = Some([
                        Pane::new(rows, Side::Left, left),
                        Pane::new(rows, Side::Right, right),
                    ]);
                }
            }
            Event::Compared(compared) => self.compared(*compared, &mut effects),
            Event::Realigned { generation, view } => {
                self.realigned(generation, view, &mut effects);
            }
            Event::Failed => self.failed(&mut effects),
            Event::Save => self.settle(Then::Stay, &mut effects),
            Event::Close => self.settle(Then::Close, &mut effects),
            Event::Exit => self.settle(Then::Exit, &mut effects),
            Event::Answered(answer) => self.answered(answer, &mut effects),
            Event::Saved(files) => self.saved(files, &mut effects),
            Event::ExitCancelled => {
                if self.after == Some(Then::Exit) {
                    self.after = None;
                }
            }
        }
        self.follow_up(&mut effects);
        effects
    }

    /// What every event leads to: what waited for the saves, rows for the
    /// lines and options as they are, and the command that waited for them.
    fn follow_up(&mut self, effects: &mut Vec<Effect>) {
        if self.asking.is_none()
            && self.saves.running.is_none()
            && let Some(then) = self.after.take()
        {
            self.proceed(then, effects);
        }
        self.align_if_needed(effects);
        if self.asking.is_none()
            && self.fresh()
            && let Some(command) = self.queued.take()
        {
            effects.push(Effect::Run(command, Fresh(self.generation)));
        }
        if !self.retired.is_empty() {
            effects.push(Effect::Retire(std::mem::take(&mut self.retired)));
        }
    }

    /// Whether the rows that show were built for the lines as they are,
    /// with the options chosen.
    fn fresh(&self) -> bool {
        self.showing.as_ref().is_some_and(|showing| {
            showing.generation == self.generation
                && showing.rows.builds_as(self.rows, showing.mode())
        })
    }

    /// Whether `fresh` still proves it, because a token can be kept past
    /// the event that gave it.
    fn holds(&self, fresh: &Fresh) -> bool {
        fresh.0 == self.generation && self.fresh()
    }

    /// Whether edit mode can change the lines now.
    fn can_change(&self) -> bool {
        self.editing && self.asking.is_none() && !self.comparing()
    }

    /// The comparison that the person sees: the one that runs, or else the
    /// one that shows.
    fn request(&self) -> Option<Request> {
        if let Job::Comparing(request) = &self.job
            && request.paths.is_some()
        {
            return Some(request.clone());
        }
        self.showing.as_ref().map(|showing| Request {
            paths: Some(showing.paths.clone()),
            hex: showing.hex,
            encodings: showing.chosen,
            rows: self.rows,
        })
    }

    fn start_compare(&mut self, mut request: Request, effects: &mut Vec<Effect>) {
        request.rows = self.rows;
        self.queued = None;
        self.job = Job::Comparing(request.clone());
        effects.push(Effect::Compare(request));
    }

    /// Has the worker build the rows again when the rows that show are not
    /// fresh, unless typing or a comparison will replace them.
    fn align_if_needed(&mut self, effects: &mut Vec<Effect>) {
        if self.fresh() {
            // The options went back to those of the rows that show.
            if matches!(self.job, Job::Aligning { .. }) {
                self.job = Job::Idle;
                effects.push(Effect::Cancel);
            }
            return;
        }
        let Some(showing) = &self.showing else {
            return;
        };
        match self.job {
            Job::Comparing(_) | Job::Due => return,
            Job::Aligning { generation, rows }
                if generation == self.generation && rows.builds_as(self.rows, showing.mode()) =>
            {
                return;
            }
            Job::Idle | Job::Aligning { .. } => {}
        }
        let input = match &showing.content {
            Content::Text(texts) => AlignInput::Text {
                texts: texts.clone(),
                anchors: showing.anchors.clone(),
            },
            Content::Hex(found) => AlignInput::Hex {
                paths: showing.paths.clone(),
                sizes: showing.sizes,
                found: Arc::clone(found),
            },
        };
        self.job = Job::Aligning {
            generation: self.generation,
            rows: self.rows,
        };
        effects.push(Effect::Align(Align {
            generation: self.generation,
            rows: self.rows,
            input,
        }));
    }

    /// The controls show the rows again; edit mode reads its panes back
    /// from them.
    fn present(&mut self, caret: Caret, effects: &mut Vec<Effect>) {
        self.panes = None;
        effects.push(Effect::Present(caret));
        if self.editing && self.fresh() {
            effects.push(Effect::ReadPanes(Fresh(self.generation)));
        }
    }

    /// An edit changed the lines; it forgets Resync from here.
    fn lines_edited(&mut self) {
        self.generation += 1;
        if let Some(showing) = &mut self.showing {
            showing.anchors.clear();
        }
    }

    /// The controls no longer show the lines, so the sides take no typing
    /// until the rows show again, which starts at once.
    fn controls_outdated(&mut self) {
        self.panes = None;
        if matches!(self.job, Job::Due) {
            self.job = Job::Idle;
        }
    }

    /// `side`'s lines, as edit mode changes them.
    fn lines_mut(&mut self, side: Side) -> Option<&mut Vec<String>> {
        let Content::Text(texts) = &mut self.showing.as_mut()?.content else {
            return None;
        };
        Some(&mut Arc::make_mut(&mut texts[side.index()]).lines)
    }

    fn edited(&mut self, side: Side, text: Vec<u16>, caret: usize, effects: &mut Vec<Effect>) {
        if !self.typing(side) {
            return;
        }
        let writing = self.saves.writing();
        let (Some(showing), Some(panes)) = (self.showing.as_mut(), self.panes.as_mut()) else {
            return;
        };
        let Content::Text(texts) = &mut showing.content else {
            return;
        };
        let text_of_side = &mut texts[side.index()];
        match panes[side.index()].edited(side, &text_of_side.lines, text, caret) {
            Ok(None) => return,
            Ok(Some(change)) => {
                if change.apply(&mut Arc::make_mut(text_of_side).lines) {
                    self.history.record_typing(change, writing);
                    self.lines_edited();
                    self.job = Job::Due;
                    effects.push(Effect::Wait);
                    return;
                }
            }
            Err(Refused::Fold | Refused::Gap) => {}
        }
        // The control shows rows that the rows of the lines do not: an edit
        // that the lines do not hold, or a row without a line.
        self.generation += 1;
        self.controls_outdated();
    }

    fn undo(&mut self) {
        if !self.can_change() {
            return;
        }
        let Some(change) = self.history.take_last(self.saves.writing()) else {
            return;
        };
        if let Some(lines) = self.lines_mut(change.side) {
            change.revert(lines);
        }
        self.lines_edited();
        self.controls_outdated();
    }

    /// Runs `command` once the rows are fresh, so it reads rows that map
    /// to the lines as they are.
    fn command(&mut self, command: CompareCommand, effects: &mut Vec<Effect>) {
        if self.asking.is_some() {
            self.queued = Some(command);
            return;
        }
        if self.fresh() {
            effects.push(Effect::Run(command, Fresh(self.generation)));
            return;
        }
        // The comparison that runs replaces the rows; Swap swaps the files
        // it reads when none show yet.
        if let Job::Comparing(request) = &mut self.job {
            if command == CompareCommand::Swap
                && self.showing.is_none()
                && let Some(paths) = &mut request.paths
            {
                paths.swap(0, 1);
                request.encodings.swap(0, 1);
                effects.push(Effect::Compare(request.clone()));
                effects.push(Effect::Swapped);
            }
            return;
        }
        if self.showing.is_some() {
            self.queued = Some(command);
            if matches!(self.job, Job::Due) {
                self.job = Job::Idle;
            }
        }
    }

    /// Copy to right or Copy to left: `rows` of the side `to.other()`
    /// replace the lines they pair with on side `to`.
    fn copy(&mut self, to: Side, rows: Range<usize>) {
        if !self.can_change() {
            return;
        }
        let Some(showing) = &mut self.showing else {
            return;
        };
        let Content::Text(texts) = &mut showing.content else {
            return;
        };
        if !texts[to.index()].editable {
            return;
        }
        let change = compare::copy_change(
            &showing.view.rows.rows,
            rows,
            to.other(),
            [&texts[0].lines, &texts[1].lines],
        );
        let Some(change) = change else {
            return;
        };
        if !change.apply(&mut Arc::make_mut(&mut texts[to.index()]).lines) {
            return;
        }
        self.history.record(change);
        self.lines_edited();
        self.controls_outdated();
    }

    /// Resync from here: pairs the line of each side's row in `rows`, and
    /// matches the lines before and after them again.
    fn resync(&mut self, rows: [usize; 2]) {
        if self.comparing() {
            return;
        }
        let Some(showing) = &mut self.showing else {
            return;
        };
        if showing.mode() != Mode::Text {
            return;
        }
        let lines = Side::BOTH.map(|side| {
            let line = showing.view.rows.rows.get(rows[side.index()])?.line(side)?;
            usize::try_from(line).ok()
        });
        let [Some(left), Some(right)] = lines else {
            return;
        };
        let anchors = &mut showing.anchors;
        anchors.retain(|&(a, b)| (a < left && b < right) || (a > left && b > right));
        anchors.push((left, right));
        anchors.sort_unstable();
        self.generation += 1;
    }

    /// Swap sides: the files change places, with their changes.
    fn swap(&mut self, effects: &mut Vec<Effect>) {
        let Some(showing) = &mut self.showing else {
            return;
        };
        showing.swap();
        self.history.swap();
        self.saves.swap();
        self.generation += 1;
        showing.generation = self.generation;
        if let Job::Comparing(request) = &mut self.job
            && let Some(paths) = &mut request.paths
        {
            paths.swap(0, 1);
            request.encodings.swap(0, 1);
            effects.push(Effect::Compare(request.clone()));
        }
        effects.push(Effect::Swapped);
        self.present(Caret::Kept, effects);
    }

    fn toggle_edit(&mut self, effects: &mut Vec<Effect>) {
        if self.editing {
            self.editing = false;
            self.panes = None;
        } else if self.can_edit() {
            self.editing = true;
            effects.push(Effect::ReadPanes(Fresh(self.generation)));
        }
    }

    fn compared(&mut self, compared: Compared, effects: &mut Vec<Effect>) {
        let request = match std::mem::replace(&mut self.job, Job::Idle) {
            Job::Comparing(request) => request,
            job => {
                self.job = job;
                let items = compared.view.rows.rows.len();
                self.retired.push(compared, items);
                return;
            }
        };
        let Compared {
            paths,
            sizes,
            content,
            view,
            ..
        } = compared;
        let showing = Showing {
            paths,
            hex: request.hex,
            chosen: request.encodings,
            sizes,
            content,
            view,
            rows: request.rows,
            anchors: Vec::new(),
            generation: self.generation,
        };
        if let Some(old) = self.showing.replace(showing) {
            let items = old.items();
            self.retired.push(old, items);
        }
        let can_edit = self.can_edit();
        self.editing &= can_edit;
        self.queued = None;
        self.present(Caret::FirstDifference, effects);
    }

    fn realigned(&mut self, generation: u64, view: View, effects: &mut Vec<Effect>) {
        let built = match self.job {
            Job::Aligning {
                generation: built,
                rows,
            } if built == generation && generation == self.generation => Some(rows),
            _ => None,
        };
        let (Some(rows), Some(showing)) = (built, &mut self.showing) else {
            let items = view.rows.rows.len();
            self.retired.push(view, items);
            return;
        };
        let old = std::mem::replace(&mut showing.view, view);
        showing.generation = generation;
        showing.rows = rows;
        let items = old.rows.rows.len();
        self.retired.push(old, items);
        self.job = Job::Idle;
        self.present(Caret::Kept, effects);
    }

    fn failed(&mut self, effects: &mut Vec<Effect>) {
        match self.job {
            Job::Comparing(_) => self.job = Job::Idle,
            // Only a Hex comparison reads its files again to build its rows.
            // One that can no longer be read keeps the rows it showed, and
            // their options, so no realignment starts again.
            Job::Aligning { .. } => {
                self.job = Job::Idle;
                if let Some(showing) = &self.showing {
                    self.rows.colours = showing.rows.colours;
                    if self.rows.only_differences != showing.rows.only_differences {
                        self.rows.only_differences = showing.rows.only_differences;
                        effects.push(Effect::OnlyDifferences(self.rows.only_differences));
                    }
                }
            }
            Job::Idle | Job::Due => {}
        }
    }

    /// The sides with changes that no save writes.
    fn unsaved(&self) -> Vec<Side> {
        let writing = self.saves.writing();
        Side::BOTH
            .into_iter()
            .filter(|side| self.history.changed_after(*side, writing[side.index()]))
            .collect()
    }

    /// Asks about the changes that no save writes, then does `then`.
    fn settle(&mut self, then: Then, effects: &mut Vec<Effect>) {
        if self.asking.is_some() {
            effects.push(Effect::Settled(Settled::Asking));
            return;
        }
        let sides = self.unsaved();
        if sides.is_empty() {
            self.proceed(then, effects);
            return;
        }
        self.asking = Some(Asking {
            then,
            sides: sides.clone(),
            marks: self.saves.writing(),
        });
        effects.push(Effect::Ask(sides));
    }

    /// Does `then` once the saves end, and asks again about changes that
    /// came or stayed meanwhile.
    fn proceed(&mut self, then: Then, effects: &mut Vec<Effect>) {
        if then == Then::Stay {
            effects.push(Effect::Settled(Settled::Done));
            return;
        }
        if self.saves.running.is_some() {
            self.after = Some(match self.after.take() {
                Some(after) => after.and(then),
                None => then,
            });
            effects.push(Effect::Settled(Settled::Saving));
            return;
        }
        if !self.unsaved().is_empty() {
            self.settle(then, effects);
            return;
        }
        match then {
            Then::Stay | Then::Exit => {}
            Then::Close => effects.push(Effect::Close),
            Then::Compare(request) => self.start_compare(request, effects),
        }
        effects.push(Effect::Settled(Settled::Done));
    }

    fn answered(&mut self, answer: Answer, effects: &mut Vec<Effect>) {
        let Some(asking) = self.asking.take() else {
            return;
        };
        let saved: Vec<Side> = match &answer {
            Answer::Cancel => {
                if asking.then == Then::Exit || self.after == Some(Then::Exit) {
                    effects.push(Effect::ExitStopped);
                }
                effects.push(Effect::Settled(Settled::Cancelled));
                return;
            }
            Answer::DontSave => Vec::new(),
            Answer::Save(files) => files.iter().map(|file| file.side).collect(),
        };
        if asking.then != Then::Stay {
            for &side in asking.sides.iter().filter(|side| !saved.contains(*side)) {
                self.revert_after(side, asking.marks[side.index()]);
            }
        }
        if let Answer::Save(files) = answer {
            let marks = Side::BOTH.map(|side| {
                saved
                    .contains(&side)
                    .then(|| self.history.last(side))
                    .flatten()
            });
            self.saves.start(marks, files, effects);
        }
        // The question can come while an earlier request waits for a save.
        let then = match self.after.take() {
            Some(after) => after.and(asking.then),
            None => asking.then,
        };
        self.proceed(then, effects);
    }

    /// Don't save: `side`'s lines lose the changes after the one numbered
    /// `mark`.
    fn revert_after(&mut self, side: Side, mark: Option<u64>) {
        let changes = self.history.take_after(side, mark);
        if changes.is_empty() {
            return;
        }
        if let Some(lines) = self.lines_mut(side) {
            for change in &changes {
                change.revert(lines);
            }
        }
        self.lines_edited();
        self.controls_outdated();
    }

    fn saved(&mut self, files: Vec<SavedFile>, effects: &mut Vec<Effect>) {
        let Some(batch) = self.saves.running.take() else {
            return;
        };
        let mut failed = false;
        for file in files {
            let side = if batch.flipped {
                file.side.other()
            } else {
                file.side
            };
            if !file.saved {
                failed = true;
                continue;
            }
            if let Some(mark) = batch.marks[side.index()] {
                self.history.forget_through(side, mark);
            }
            if let Some(showing) = &mut self.showing
                && showing.paths[side.index()] != file.path
            {
                showing.paths[side.index()] = file.path;
                effects.push(Effect::Renamed(side));
            }
        }
        if let Some((batch, files)) = self.saves.waiting.pop_front() {
            self.saves.running = Some(batch);
            effects.push(Effect::Save(files));
        }
        // The window stays, with the changes that did not save.
        if failed && self.after.take() == Some(Then::Exit) {
            effects.push(Effect::ExitStopped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::compare::{
        Form, LineEnd, SHOWN_LIMIT, Shown, count_differences, difference_at, hex_shown_rows,
        match_lines, shown_rows,
    };
    use proptest::prelude::*;
    use std::collections::HashMap;

    const PLAIN: Matching = Matching {
        case_sensitive: true,
        ignore_spaces: false,
    };

    const CR: u16 = 0x0D;

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    fn lines_of(picks: &[u8]) -> Vec<String> {
        (picks.iter())
            .map(|pick| ["", "a", "b", "ab"][usize::from(*pick % 4)].to_owned())
            .collect()
    }

    fn bytes_of(lines: &[String]) -> Vec<u8> {
        (lines.iter())
            .flat_map(|line| line.bytes().chain(*b"\n"))
            .collect()
    }

    fn lines_from(bytes: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(bytes)
            .split_terminator('\n')
            .map(str::to_owned)
            .collect()
    }

    fn text_view(
        left: &[String],
        right: &[String],
        rows: RowOptions,
        anchors: &[(usize, usize)],
    ) -> View {
        let matched =
            match_lines(left, right, rows.matching, anchors, &|| false).expect("nothing cancels");
        View {
            differences: count_differences(matched.iter().map(|row| row.differs)),
            rows: shown_rows(&matched, rows.only_differences, SHOWN_LIMIT),
            ..View::default()
        }
    }

    /// The files' rows stand in for hex rows.
    fn hex_view(sizes: [u64; 2], found: &HexFound, rows: RowOptions) -> View {
        View {
            rows: hex_shown_rows(
                sizes,
                &found.differing,
                found.complete,
                rows.only_differences,
                SHOWN_LIMIT,
            ),
            differences: found.differences,
            ..View::default()
        }
    }

    #[derive(Clone, Debug)]
    enum Step {
        Type {
            side: Side,
            at: u8,
            key: u8,
        },
        Timer,
        FinishJob {
            fail: bool,
        },
        FinishSave {
            fail: bool,
        },
        Command(CompareCommand),
        Undo,
        Caret(u8),
        Options {
            case_sensitive: bool,
            only: Option<u32>,
        },
        Colours,
        Compare {
            swapped: bool,
        },
        View {
            hex: bool,
        },
        Cancel,
        Save,
        Close,
        Exit,
        Answer(u8),
    }

    fn step() -> impl Strategy<Value = Step> {
        (0u8..18, any::<u8>(), any::<u8>()).prop_map(|(kind, a, b)| {
            let side = if a % 2 == 0 { Side::Left } else { Side::Right };
            let commands = [
                CompareCommand::EditMode,
                CompareCommand::CopyToRight,
                CompareCommand::CopyToLeft,
                CompareCommand::Resync,
                CompareCommand::Swap,
                CompareCommand::NextDifference,
            ];
            match kind {
                0 | 1 => Step::Type {
                    side,
                    at: b,
                    key: a / 2,
                },
                2 => Step::Timer,
                3 => Step::FinishJob { fail: a % 6 == 0 },
                4 => Step::FinishSave { fail: a % 4 == 0 },
                5 | 6 => Step::Command(commands[usize::from(a) % commands.len()]),
                7 => Step::Undo,
                8 => Step::Caret(a),
                9 => Step::Options {
                    case_sensitive: a % 2 == 0,
                    only: [None, Some(0), Some(1)][usize::from(b % 3)],
                },
                10 => Step::Colours,
                11 => Step::Compare {
                    swapped: a % 2 == 0,
                },
                12 => Step::View { hex: a % 3 == 0 },
                13 => Step::Cancel,
                14 => Step::Save,
                15 => Step::Close,
                16 => Step::Exit,
                _ => Step::Answer(a),
            }
        })
    }

    /// The row of a control's `text` that holds the position `at`, as a
    /// RichEdit counts its rows.
    fn row_at(text: &[u16], at: usize) -> usize {
        let at = at.min(text.len());
        text[..at].iter().filter(|unit| **unit == CR).count()
    }

    enum Pending {
        Compare(Request),
        Align(Align),
    }

    /// A compare window without Windows: its controls, its worker, its
    /// questions and the disk.
    struct World {
        session: Session,
        disk: HashMap<Vec<u16>, Vec<String>>,
        controls: [Vec<u16>; 2],
        caret: usize,
        job: Option<Pending>,
        saves: VecDeque<Vec<SaveFile>>,
        question: Option<Vec<Side>>,
        closed: bool,
        named: u32,
    }

    impl World {
        fn new(left: Vec<String>, right: Vec<String>) -> World {
            let mut world = World {
                session: Session::new(PLAIN, None),
                disk: [(wide("A"), left), (wide("B"), right)]
                    .into_iter()
                    .collect(),
                controls: [Vec::new(), Vec::new()],
                caret: 0,
                job: None,
                saves: VecDeque::new(),
                question: None,
                closed: false,
                named: 0,
            };
            world.send(Event::Choose);
            world
        }

        fn send(&mut self, event: Event) {
            for effect in self.session.on(event) {
                if self.closed {
                    return;
                }
                self.apply(effect);
            }
        }

        fn apply(&mut self, effect: Effect) {
            match effect {
                Effect::Compare(request) => self.job = Some(Pending::Compare(request)),
                Effect::Align(align) => self.job = Some(Pending::Align(align)),
                Effect::Cancel => self.job = None,
                Effect::Present(_) => self.controls = Side::BOTH.map(|side| self.rendered(side)),
                Effect::ReadPanes(fresh) => {
                    let texts = self.controls.clone();
                    self.send(Event::Panes { fresh, texts });
                }
                Effect::Run(command, fresh) => {
                    assert!(self.session.fresh(), "{command:?} runs on fresh rows");
                    self.run(command, fresh);
                }
                Effect::Ask(sides) => self.question = Some(sides),
                Effect::Save(files) => self.saves.push_back(files),
                Effect::Close => {
                    assert!(
                        !self.session.has_changes() && !self.session.saving(),
                        "the window closes only after every change was answered and saved"
                    );
                    self.closed = true;
                }
                Effect::ExitStopped => self.send(Event::ExitCancelled),
                Effect::Wait
                | Effect::Swapped
                | Effect::Renamed(_)
                | Effect::OnlyDifferences(_)
                | Effect::Settled(_)
                | Effect::Retire(_) => {}
            }
        }

        /// A side's text as its control holds the rows: each row's line,
        /// with the rows separated by CR.
        fn rendered(&self, side: Side) -> Vec<u16> {
            let Some(showing) = self.session.showing() else {
                return Vec::new();
            };
            let mut text = Vec::new();
            for (index, row) in showing.view().rows.rows.iter().enumerate() {
                if index > 0 {
                    text.push(CR);
                }
                let line = match row {
                    Shown::Row { .. } => (row.line(side))
                        .and_then(|line| {
                            showing
                                .text(side)?
                                .lines
                                .get(usize::try_from(line).ok()?)
                                .cloned()
                        })
                        .unwrap_or_default(),
                    Shown::Fold { .. } => "folded".to_owned(),
                };
                text.extend(line.encode_utf16());
            }
            text
        }

        /// Runs `command` as the window does: the rows come from the caret
        /// in each control, and the difference from the rows that show.
        fn run(&mut self, command: CompareCommand, fresh: Fresh) {
            for side in Side::BOTH {
                assert_eq!(
                    self.controls[side.index()],
                    self.rendered(side),
                    "{command:?} reads the rows that the controls show"
                );
            }
            let carets = (self.controls.each_ref()).map(|text| row_at(text, self.caret));
            let rows = (self.session.showing())
                .map_or(&[][..], |showing| showing.view().rows.rows.as_slice());
            let difference = difference_at(rows, carets[0]);
            let event = match command {
                CompareCommand::CopyToRight => Event::Copy {
                    fresh,
                    to: Side::Right,
                    rows: difference,
                },
                CompareCommand::CopyToLeft => Event::Copy {
                    fresh,
                    to: Side::Left,
                    rows: difference,
                },
                CompareCommand::Resync => Event::Resync {
                    fresh,
                    rows: carets,
                },
                CompareCommand::Swap => Event::Swap(fresh),
                CompareCommand::EditMode => Event::EditMode(fresh),
                _ => return,
            };
            self.send(event);
        }

        fn compared(&self, request: &Request) -> Compared {
            let paths = (request.paths.clone()).unwrap_or_else(|| [wide("A"), wide("B")]);
            let lines = paths
                .each_ref()
                .map(|path| self.disk.get(path).cloned().unwrap_or_default());
            let sizes = lines.each_ref().map(|lines| lines.len() as u64);
            let identical = lines[0] == lines[1];
            if request.hex {
                let differs = |row: &u64| {
                    let row = usize::try_from(*row).expect("few rows");
                    lines[0].get(row) != lines[1].get(row)
                };
                let differing: Vec<u64> = (0..sizes[0].max(sizes[1])).filter(differs).collect();
                let found = HexFound {
                    note: None,
                    differences: count_differences(
                        (0..sizes[0].max(sizes[1])).map(|row| differs(&row)),
                    ),
                    differing,
                    complete: true,
                };
                return Compared {
                    view: hex_view(sizes, &found, request.rows),
                    paths,
                    sizes,
                    content: Content::Hex(Arc::new(found)),
                    identical,
                };
            }
            let view = text_view(&lines[0], &lines[1], request.rows, &[]);
            let form = Form {
                encoding: Encoding::Utf8,
                byte_order_mark: false,
                line_end: LineEnd::Lf,
                final_line_end: true,
            };
            Compared {
                paths,
                sizes,
                content: Content::Text(lines.map(|lines| {
                    Arc::new(Text {
                        lines,
                        form,
                        editable: true,
                    })
                })),
                view,
                identical,
            }
        }

        fn finish_job(&mut self, fail: bool) {
            let Some(job) = self.job.take() else {
                return;
            };
            let event = match job {
                Pending::Compare(_) if fail => Event::Failed,
                Pending::Compare(request) => Event::Compared(Box::new(self.compared(&request))),
                Pending::Align(Align {
                    input: AlignInput::Hex { .. },
                    ..
                }) if fail => Event::Failed,
                Pending::Align(Align {
                    generation,
                    rows,
                    input: AlignInput::Text { texts, anchors },
                }) => Event::Realigned {
                    generation,
                    view: text_view(&texts[0].lines, &texts[1].lines, rows, &anchors),
                },
                Pending::Align(Align {
                    generation,
                    rows,
                    input: AlignInput::Hex { sizes, found, .. },
                }) => Event::Realigned {
                    generation,
                    view: hex_view(sizes, &found, rows),
                },
            };
            self.send(event);
        }

        fn finish_save(&mut self, fail: bool) {
            let Some(files) = self.saves.pop_front() else {
                return;
            };
            let saved = files
                .into_iter()
                .map(|file| {
                    if !fail {
                        self.disk.insert(file.path.clone(), lines_from(&file.bytes));
                    }
                    SavedFile {
                        side: file.side,
                        path: file.path,
                        saved: !fail,
                    }
                })
                .collect();
            self.send(Event::Saved(saved));
        }

        fn answer(&mut self, choice: u8) {
            let Some(asked) = self.question.take() else {
                return;
            };
            let answer = match choice % 5 {
                0 => Answer::Cancel,
                1 => Answer::DontSave,
                kind => {
                    let sides = if kind == 4 { &asked[..1] } else { &asked[..] };
                    let showing = self
                        .session
                        .showing()
                        .expect("questions are about shown lines");
                    let files = (sides.iter())
                        .map(|&side| {
                            let path = if kind == 3 {
                                self.named += 1;
                                wide(&format!("N{}", self.named))
                            } else {
                                showing.paths()[side.index()].clone()
                            };
                            let text = showing.text(side).expect("changes are in Text");
                            SaveFile {
                                side,
                                path,
                                bytes: bytes_of(&text.lines),
                            }
                        })
                        .collect();
                    Answer::Save(files)
                }
            };
            self.send(Event::Answered(answer));
        }

        fn type_key(&mut self, side: Side, at: u8, key: u8) {
            if !self.session.typing(side) {
                return;
            }
            let text = &mut self.controls[side.index()];
            let at = usize::from(at) % (text.len() + 1);
            let caret = match key % 3 {
                0 => {
                    text.insert(at, u16::from(b'x'));
                    at + 1
                }
                1 => {
                    text.insert(at, CR);
                    at + 1
                }
                _ => {
                    if at < text.len() {
                        text.remove(at);
                    }
                    at
                }
            };
            let text = text.clone();
            self.send(Event::Edited { side, text, caret });
        }

        fn step(&mut self, step: Step) {
            if self.closed {
                return;
            }
            // A question is modal: only the worker, the timers and Exit
            // reach the window while it is open.
            let person = !matches!(
                step,
                Step::Timer
                    | Step::FinishJob { .. }
                    | Step::FinishSave { .. }
                    | Step::Exit
                    | Step::Answer(_)
            );
            if person && self.question.is_some() {
                return;
            }
            match step {
                Step::Type { side, at, key } => self.type_key(side, at, key),
                Step::Timer => self.send(Event::Timer),
                Step::FinishJob { fail } => self.finish_job(fail),
                Step::FinishSave { fail } => self.finish_save(fail),
                Step::Command(command) => self.send(Event::Command(command)),
                Step::Undo => self.send(Event::Undo),
                Step::Caret(row) => self.caret = usize::from(row),
                Step::Options {
                    case_sensitive,
                    only,
                } => self.send(Event::Options {
                    matching: Matching {
                        case_sensitive,
                        ignore_spaces: false,
                    },
                    only_differences: only,
                }),
                Step::Colours => self.send(Event::Colours),
                Step::Compare { swapped } => {
                    let paths = if swapped {
                        [wide("B"), wide("A")]
                    } else {
                        [wide("A"), wide("B")]
                    };
                    self.send(Event::Compare(paths));
                }
                Step::View { hex } => self.send(Event::View { hex }),
                Step::Cancel => self.send(Event::Cancel),
                Step::Save => self.send(Event::Save),
                Step::Close => self.send(Event::Close),
                // Exit asks only the windows with changes, and a Cancel in
                // any stops it in all.
                Step::Exit => {
                    if self.session.has_changes() {
                        for effect in self.session.on(Event::Exit) {
                            if !self.closed {
                                self.apply(effect);
                            }
                        }
                    }
                }
                Step::Answer(choice) => self.answer(choice),
            }
        }

        /// The rules of the module's documentation, checked after each
        /// event.
        fn check(&self) {
            if self.closed {
                return;
            }
            let session = &self.session;
            if let Job::Aligning { generation, .. } = session.job {
                assert_eq!(
                    generation, session.generation,
                    "rows are built for the lines as they are"
                );
            }
            if session.fresh() {
                assert!(
                    !matches!(session.job, Job::Due | Job::Aligning { .. }),
                    "fresh rows are not built again"
                );
                for side in Side::BOTH {
                    assert_eq!(
                        self.controls[side.index()],
                        self.rendered(side),
                        "fresh rows are the rows that the controls show"
                    );
                }
            }
            if matches!(session.job, Job::Idle) && session.showing.is_some() {
                assert!(
                    session.fresh(),
                    "with no job, the rows match the lines and the options"
                );
            }
            if session.comparing() {
                assert!(
                    session.history.is_empty(),
                    "a comparison starts only without changes"
                );
            }
            for side in Side::BOTH {
                if session.typing(side) {
                    let panes = session
                        .panes
                        .as_ref()
                        .expect("typing reads back through panes");
                    assert_eq!(
                        panes[side.index()].text(),
                        &self.controls[side.index()][..],
                        "the panes match the controls"
                    );
                    assert_eq!(
                        panes[side.index()].row_count(),
                        self.controls[side.index()]
                            .iter()
                            .filter(|unit| **unit == CR)
                            .count()
                            + 1,
                        "a pane holds a row for each of the control's rows"
                    );
                }
            }
            if let Some(showing) = &session.showing
                && let Content::Text(texts) = &showing.content
            {
                for side in Side::BOTH {
                    let mut lines = texts[side.index()].lines.clone();
                    let mut history = session.history.clone();
                    for change in history.take_after(side, None) {
                        assert!(
                            change.revert(&mut lines),
                            "the history holds the lines' changes"
                        );
                    }
                    assert_eq!(
                        Some(&lines),
                        self.disk.get(&showing.paths[side.index()]),
                        "the lines are the file with the changes the history holds"
                    );
                }
            }
        }
    }

    proptest! {
        /// Any order of the person's commands, the worker's results, the
        /// saves and the questions keeps the rules of the module: rows that
        /// map to the lines before a command reads them, panes that match
        /// the controls, a history that holds exactly what is not on the
        /// disk, and no window that closes with changes.
        #[test]
        fn any_events_keep_the_rules(
            left in proptest::collection::vec(0u8..4, 0..5),
            right in proptest::collection::vec(0u8..4, 0..5),
            steps in proptest::collection::vec(step(), 0..60),
        ) {
            let mut world = World::new(lines_of(&left), lines_of(&right));
            world.check();
            for step in steps {
                world.step(step);
                world.check();
            }
        }
    }
}
