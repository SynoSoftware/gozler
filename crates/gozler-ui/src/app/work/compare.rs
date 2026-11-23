//! The compare window's worker (DESIGN.md: Compare by content): it chooses
//! the two files, reads and compares them, builds the RTF of the sides'
//! rows that changed, runs the comparison tool, saves edited files, and
//! runs Find. The window's thread never reads a file.

use crate::core::compare::rtf::{self, Font, Palette, Patch, Style, row_rtf};
use crate::core::compare::{
    self, Chosen, LINES_LIMIT, Matching, Needle, SHOWN_LIMIT, Shown, ShownRows, Text,
};
use crate::core::compare_window::session::{
    AlignInput, Compared, Content, Fresh, HexFound, SaveFile, View,
};
use crate::core::compare_window::{Note, fold_text};
use crate::core::open::{Entries, compare_command};
use crate::core::panel::Side;
use crate::core::text::Strings;
use crate::core::viewer::{Encoding, HEX_ROW_BYTES, Mode, choose_mode, hex_row, offset_digits};
use crate::core::work::{Replacement, Retirement};
use crate::win::compare::{SaveFailure, exists, open_shared, save_file};
use crate::win::locale::{WindowsNumbers, code_page_encoded, code_page_text};
use crate::win::shell::Session;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task, spawn};
use gozler_client::CancelToken;
use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read as _};
use std::ops::Range;
use std::os::windows::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The files a Hex comparison reads at a time, from each side.
const HEX_CHUNK: usize = 1 << 20;

/// Which two files to compare.
pub(in crate::app) enum Source {
    Files([Vec<u16>; 2]),
    /// The panels' entries, of which Total Commander's four rules choose
    /// two: the active panel's chosen entries, whether it has a selection,
    /// the other panel's selected entries and its folder.
    Chosen {
        source: Entries,
        selected: bool,
        target: Option<Entries>,
        folder: Option<String>,
    },
}

impl Source {
    fn chosen(self, worker: &OffThread) -> Chosen {
        match self {
            Source::Files(files) => Chosen::Both(files),
            Source::Chosen {
                source,
                selected,
                target,
                folder,
            } => compare::choose_files(
                &source.resolve(),
                selected,
                &target.map(Entries::resolve).unwrap_or_default(),
                folder.as_deref(),
                &|path| exists(path, worker),
            ),
        }
    }
}

/// How the rows look: the colours, the font, and the words of the folds.
#[derive(Clone)]
pub(in crate::app) struct Look {
    pub(in crate::app) palette: Palette,
    pub(in crate::app) font: Font,
    pub(in crate::app) strings: Strings,
    pub(in crate::app) numbers: WindowsNumbers,
}

/// How the rows are built and how they look.
#[derive(Clone)]
pub(in crate::app) struct Options {
    pub(in crate::app) matching: Matching,
    /// Show only differences, with its extra lines.
    pub(in crate::app) only_differences: Option<u32>,
    pub(in crate::app) look: Look,
    /// The fingerprints of the rows that each side's control shows, which
    /// the rows built patch.
    pub(in crate::app) shown: [Arc<Vec<u64>>; 2],
}

/// How much of the two files a comparison has read.
#[derive(Default)]
pub(in crate::app) struct Progress {
    done: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    /// The bytes read, and the bytes to read.
    pub(in crate::app) fn read(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    fn start(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    fn add(&self, bytes: usize) {
        self.done.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

pub(in crate::app) enum Job {
    /// Runs the comparison tool `command` for the two files.
    Tool { source: Source, command: String },
    /// Chooses the files of a window's first comparison from the panels'
    /// selections, which the window then compares.
    Choose(Source),
    /// Reads the two files and compares them, as Hex with `hex`, each side
    /// in its chosen encoding or, with `None`, the one it detects.
    Compare {
        paths: [Vec<u16>; 2],
        hex: bool,
        encodings: [Option<Encoding>; 2],
        options: Options,
        progress: Arc<Progress>,
    },
    /// Builds the rows again from what a comparison read, as edits, Undo,
    /// Resync from here or the options left it; `generation` is the
    /// session's, which the rows are built for.
    Align {
        generation: u64,
        input: AlignInput,
        options: Options,
    },
}

pub(in crate::app) enum Outcome {
    ToolStarted,
    ToolFailed {
        program: String,
        error: io::Error,
    },
    /// The files that [`Job::Choose`] chose; for [`Job::Tool`], only when
    /// it found fewer than two.
    Chosen(Chosen),
    Failed {
        path: Vec<u16>,
        error: io::Error,
    },
    Cancelled,
    Compared(Box<Compared>, Rendering),
    Realigned {
        generation: u64,
        view: View,
        rendering: Rendering,
    },
}

/// The sides' rows as their controls take them: each side's patch, `None`
/// when its control shows the rows already, and the fingerprints of the
/// rows that its control then shows.
pub(in crate::app) struct Rendering {
    pub(in crate::app) patches: [Option<Patch>; 2],
    pub(in crate::app) prints: [Vec<u64>; 2],
}

/// Each file that Save wrote, or why not.
pub(in crate::app) type Saved = Vec<(Side, Vec<u16>, Result<(), Unsaved>)>;

/// Why Save did not write a file.
pub(in crate::app) enum Unsaved {
    /// The file's code page cannot hold a character of its text: the first
    /// one, with the index of its line, when a single character fails.
    Unencodable {
        encoding: Encoding,
        first: Option<(usize, char)>,
    },
    /// Another file of the same save could not be encoded, so none was
    /// written.
    Withheld,
    Write(SaveFailure),
}

/// What Find reads besides the rows that show.
pub(in crate::app) enum Searched {
    /// Each side's text, whose lines the rows show.
    Text([Arc<Text>; 2]),
    /// The files, of `sizes` bytes, whose hex rows the rows show.
    Hex {
        paths: [Vec<u16>; 2],
        sizes: [u64; 2],
    },
}

/// Find, from `side`'s `row` at `column`, through the rows that `fresh`
/// proves to be the rows that show.
pub(in crate::app) struct Find {
    pub(in crate::app) needle: Needle,
    pub(in crate::app) backward: bool,
    pub(in crate::app) side: Side,
    pub(in crate::app) row: usize,
    pub(in crate::app) column: usize,
    pub(in crate::app) rows: Arc<ShownRows>,
    pub(in crate::app) searched: Searched,
    /// The words of the rows that Show only differences hides.
    pub(in crate::app) look: Look,
    pub(in crate::app) fresh: Fresh,
}

pub(in crate::app) enum Found {
    /// `columns` of `side`'s `row`, in the rows that `fresh` proved.
    Match {
        fresh: Fresh,
        side: Side,
        row: usize,
        columns: Range<usize>,
    },
    Nothing,
    /// A Hex file could not be read for its rows.
    Failed {
        path: Vec<u16>,
        error: io::Error,
    },
    Cancelled,
}

pub(in crate::app) struct Work {
    task: Task<Job, Outcome>,
    saving: Task<Vec<SaveFile>, Saved>,
    finding: Task<Find, Found>,
    /// Publishes the text that the window copies.
    pub(in crate::app) clipboard: super::clipboard::Work,
}

impl Work {
    pub(in crate::app) fn new(waker: OpenWaker) -> Work {
        Work {
            // The shell's thread starts the comparison tool.
            task: Task::shell(waker.clone(), Replacement::Cancel, run),
            // A save that started always ends.
            saving: Task::new(waker.clone(), Replacement::Finish, save),
            finding: Task::new(waker.clone(), Replacement::Cancel, search),
            clipboard: super::clipboard::Work::new(waker),
        }
    }

    /// Starts `find` in place of the Find that runs.
    pub(in crate::app) fn find(&mut self, find: Find) {
        self.finding.replace(find);
        retire(self.finding.take_retired());
    }

    pub(in crate::app) fn take_found(&mut self) -> Option<Found> {
        let found = self.finding.take().map(|(_, found)| found);
        retire(self.finding.take_retired());
        found
    }

    /// Starts `job` in place of the one that runs.
    pub(in crate::app) fn start(&mut self, job: Job) {
        self.task.replace(job);
        retire(self.task.take_retired());
    }

    pub(in crate::app) fn cancel(&mut self) {
        self.task.cancel();
        retire(self.task.take_retired());
    }

    pub(in crate::app) fn take(&mut self) -> Option<Outcome> {
        let outcome = self.task.take().map(|(_, outcome)| outcome);
        retire(self.task.take_retired());
        outcome
    }

    pub(in crate::app) fn save(&mut self, files: Vec<SaveFile>) {
        self.saving.submit(files, |mut earlier, mut later| {
            earlier.append(&mut later);
            earlier
        });
    }

    pub(in crate::app) fn take_saved(&mut self) -> Option<Saved> {
        self.saving.take().map(|(_, saved)| saved)
    }

    /// Ends the tasks as the window closes, and hands over an outcome
    /// that nobody took, which can hold whole files.
    pub(in crate::app) fn close(mut self) -> Retirement {
        self.task.cancel();
        self.finding.cancel();
        let mut retired = self.task.close_delivery();
        retired.append(self.saving.close_delivery());
        retired.append(self.finding.close_delivery());
        retired.append(self.clipboard.close());
        retired
    }
}

/// Drops what the tasks and the window no longer need off the window's
/// thread, because it can hold whole files.
pub(in crate::app) fn retire(retired: Retirement) {
    if !retired.is_empty() {
        spawn(move |_| drop(retired));
    }
}

fn run(job: Job, session: &Session, worker: &OffThread, cancel: &CancelToken) -> Outcome {
    match job {
        Job::Tool { source, command } => {
            let files = match source.chosen(worker) {
                Chosen::Both(files) => files,
                fewer => return Outcome::Chosen(fewer),
            };
            let [first, second] = files.map(|path| String::from_utf16_lossy(&path));
            let Some(line) = compare_command(&command, [&first, &second]) else {
                return Outcome::ToolStarted;
            };
            match session.run_command(&line.program, &line.parameters) {
                Ok(()) => Outcome::ToolStarted,
                Err(error) => Outcome::ToolFailed {
                    program: line.program,
                    error,
                },
            }
        }
        Job::Choose(source) => Outcome::Chosen(source.chosen(worker)),
        Job::Compare {
            paths,
            hex,
            encodings,
            options,
            progress,
        } => compare_files(paths, hex, encodings, &options, &progress, worker, cancel),
        Job::Align {
            generation,
            input: AlignInput::Text { texts, anchors },
            options,
        } => {
            let lines = [texts[0].lines.as_slice(), texts[1].lines.as_slice()];
            match realign(lines, &anchors, &options, cancel) {
                Some((view, rendering)) => Outcome::Realigned {
                    generation,
                    view,
                    rendering,
                },
                None => Outcome::Cancelled,
            }
        }
        Job::Align {
            generation,
            input:
                AlignInput::Hex {
                    paths,
                    sizes,
                    found,
                },
            options,
        } => match rebuild_hex(&paths, sizes, &found, &options, worker, cancel) {
            Ok(Some((view, rendering))) => Outcome::Realigned {
                generation,
                view,
                rendering,
            },
            Ok(None) => Outcome::Cancelled,
            Err((side, error)) => Outcome::Failed {
                path: paths[side].clone(),
                error,
            },
        },
    }
}

fn compare_files(
    paths: [Vec<u16>; 2],
    hex: bool,
    encodings: [Option<Encoding>; 2],
    options: &Options,
    progress: &Progress,
    worker: &OffThread,
    cancel: &CancelToken,
) -> Outcome {
    let mut files = Vec::with_capacity(2);
    for path in &paths {
        let opened = open_shared(path, worker).and_then(|file| Ok((file.metadata()?.len(), file)));
        match opened {
            Ok(opened) => files.push(opened),
            Err(error) => {
                return Outcome::Failed {
                    path: path.clone(),
                    error,
                };
            }
        }
    }
    let sizes = [files[0].0, files[1].0];
    let files = [&files[0].1, &files[1].1];
    progress.start(sizes[0] + sizes[1]);
    if sizes.iter().any(|size| *size > LINES_LIMIT) {
        return compare_hex(
            paths,
            files,
            sizes,
            Some(Note::LargeBytes),
            options,
            progress,
            cancel,
        );
    }
    let mut contents = [Vec::new(), Vec::new()];
    for side in 0..2 {
        match read_all(files[side], sizes[side], progress, cancel) {
            Ok(Some(bytes)) => contents[side] = bytes,
            Ok(None) => return Outcome::Cancelled,
            Err(error) if error.kind() == io::ErrorKind::FileTooLarge => {
                // The file grew, and Hex compares up to the lengths.
                let sizes = [0, 1].map(|side| {
                    files[side]
                        .metadata()
                        .map_or(sizes[side], |meta| meta.len())
                });
                return compare_hex(
                    paths,
                    files,
                    sizes,
                    Some(Note::LargeBytes),
                    options,
                    progress,
                    cancel,
                );
            }
            Err(error) => {
                return Outcome::Failed {
                    path: paths[side].clone(),
                    error,
                };
            }
        }
    }
    // A file that is not text says so also when the person chose Hex,
    // because View > Text could not compare it. A side whose encoding the
    // person chose is text, as in the Viewer, such as UTF-16 without a
    // byte order mark.
    if (contents.iter().zip(encodings))
        .any(|(bytes, chosen)| chosen.is_none() && choose_mode(bytes) == Mode::Hex)
    {
        let note = Some(Note::NotText);
        return compare_hex(paths, files, sizes, note, options, progress, cancel);
    }
    if hex {
        return compare_hex(paths, files, sizes, None, options, progress, cancel);
    }
    let identical = contents[0] == contents[1];
    let texts = [0, 1].map(|side| {
        compare::read_text(
            &contents[side],
            encodings[side],
            &code_page_text,
            &code_page_encoded,
        )
    });
    drop(contents);
    let [Some(left), Some(right)] = texts else {
        return compare_hex(
            paths,
            files,
            sizes,
            Some(Note::ManyLines),
            options,
            progress,
            cancel,
        );
    };
    let lines = [left.lines.as_slice(), right.lines.as_slice()];
    let Some((view, rendering)) = realign(lines, &[], options, cancel) else {
        return Outcome::Cancelled;
    };
    let compared = Compared {
        paths,
        sizes,
        content: Content::Text([Arc::new(left), Arc::new(right)]),
        view,
        identical,
    };
    Outcome::Compared(Box::new(compared), rendering)
}

/// Reads `file`, of `size` bytes, whole; `None` when cancelled. A file that
/// grew meanwhile is read to its end, or, past [`LINES_LIMIT`], fails with
/// [`io::ErrorKind::FileTooLarge`].
fn read_all(
    mut file: &File,
    size: u64,
    progress: &Progress,
    cancel: &CancelToken,
) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut chunk = vec![0u8; HEX_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let read = file.read(&mut chunk)?;
        if read == 0 {
            return Ok(Some(bytes));
        }
        if bytes.len() as u64 + read as u64 > LINES_LIMIT {
            return Err(io::Error::from(io::ErrorKind::FileTooLarge));
        }
        bytes.extend_from_slice(&chunk[..read]);
        progress.add(read);
    }
}

/// Fills `buffer` from `offset` of `file` as far as its end; the bytes
/// read.
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.seek_read(&mut buffer[filled..], offset + filled as u64) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// A file that failed to read, as the index of its side, and why.
type ReadFailure = (usize, io::Error);

/// Compares the files as Hex: the rows of 16 bytes at the same offsets,
/// streamed a chunk at a time, then the rows that show read again.
fn compare_hex(
    paths: [Vec<u16>; 2],
    files: [&File; 2],
    sizes: [u64; 2],
    note: Option<Note>,
    options: &Options,
    progress: &Progress,
    cancel: &CancelToken,
) -> Outcome {
    let failed = |(side, error): ReadFailure| Outcome::Failed {
        path: paths[side].clone(),
        error,
    };
    let found = match scan_hex(files, sizes, note, progress, cancel) {
        Ok(Some(found)) => found,
        Ok(None) => return Outcome::Cancelled,
        Err(failure) => return failed(failure),
    };
    let (view, rendering) = match hex_view(files, sizes, &found, options, cancel) {
        Ok(Some(built)) => built,
        Ok(None) => return Outcome::Cancelled,
        Err(failure) => return failed(failure),
    };
    let compared = Compared {
        // Hex compares every byte, and a longer file's rest differs.
        identical: found.differences == 0,
        paths,
        sizes,
        content: Content::Hex(Arc::new(found)),
        view,
    };
    Outcome::Compared(Box::new(compared), rendering)
}

/// Builds the rows of a Hex comparison again for other options, reading
/// only the rows that show; `None` when cancelled.
fn rebuild_hex(
    paths: &[Vec<u16>; 2],
    sizes: [u64; 2],
    found: &HexFound,
    options: &Options,
    worker: &OffThread,
    cancel: &CancelToken,
) -> Result<Option<(View, Rendering)>, ReadFailure> {
    let files = open_both(paths, worker)?;
    hex_view([&files[0], &files[1]], sizes, found, options, cancel)
}

fn open_both(paths: &[Vec<u16>; 2], worker: &OffThread) -> Result<Vec<File>, ReadFailure> {
    let mut files = Vec::with_capacity(2);
    for (side, path) in paths.iter().enumerate() {
        files.push(open_shared(path, worker).map_err(|error| (side, error))?);
    }
    Ok(files)
}

/// Finds the hex rows where the files differ, reading both a chunk at a
/// time; `None` when cancelled.
fn scan_hex(
    files: [&File; 2],
    sizes: [u64; 2],
    note: Option<Note>,
    progress: &Progress,
    cancel: &CancelToken,
) -> Result<Option<HexFound>, ReadFailure> {
    let row_bytes = HEX_ROW_BYTES as u64;
    progress.start(sizes[0] + sizes[1]);
    let mut differing = Vec::new();
    let mut complete = true;
    let mut differences = 0;
    let mut previous = false;
    let mut buffers = [vec![0u8; HEX_CHUNK], vec![0u8; HEX_CHUNK]];
    let end = sizes[0].max(sizes[1]);
    let mut offset = 0u64;
    while offset < end {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let mut lengths = [0usize; 2];
        for side in 0..2 {
            lengths[side] =
                read_at(files[side], &mut buffers[side], offset).map_err(|error| (side, error))?;
        }
        progress.add(lengths[0] + lengths[1]);
        let longest = lengths[0].max(lengths[1]);
        if longest == 0 {
            break;
        }
        for start in (0..longest).step_by(HEX_ROW_BYTES) {
            let row = |side: usize| {
                let length = lengths[side];
                &buffers[side][start.min(length)..(start + HEX_ROW_BYTES).min(length)]
            };
            let differs = row(0) != row(1);
            if differs {
                if !previous {
                    differences += 1;
                }
                if differing.len() < SHOWN_LIMIT {
                    differing.push((offset + start as u64) / row_bytes);
                } else {
                    complete = false;
                }
            }
            previous = differs;
        }
        offset += longest as u64;
    }
    Ok(Some(HexFound {
        note,
        differing,
        complete,
        differences,
    }))
}

/// The rows of a Hex comparison that `options` show of what `found` holds,
/// with their bytes read again; `None` when cancelled.
fn hex_view(
    files: [&File; 2],
    sizes: [u64; 2],
    found: &HexFound,
    options: &Options,
    cancel: &CancelToken,
) -> Result<Option<(View, Rendering)>, ReadFailure> {
    let row_bytes = HEX_ROW_BYTES as u64;
    let rows = sizes.map(|size| size.div_ceil(row_bytes));
    let shown = compare::hex_shown_rows(
        rows,
        &found.differing,
        found.complete,
        options.only_differences,
        SHOWN_LIMIT,
    );
    let look = &options.look;
    let mut sides = [0, 1].map(|_| Vec::with_capacity(shown.rows.len()));
    let mut readers = [0, 1].map(|side| RowReader::new(files[side]));
    let length = sizes[0].max(sizes[1]);
    let digits = offset_digits(length);
    for each in &shown.rows {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        match each {
            Shown::Row {
                left,
                right,
                differs,
            } => {
                let mut bytes: [Option<Vec<u8>>; 2] = [None, None];
                for (side, row) in [(0, left), (1, right)] {
                    if let Some(row) = row {
                        let read = readers[side].row(*row).map_err(|error| (side, error))?;
                        bytes[side] = Some(read);
                    }
                }
                let marks = if *differs {
                    compare::hex_marks(bytes[0].as_deref(), bytes[1].as_deref(), digits)
                } else {
                    [Vec::new(), Vec::new()]
                };
                for side in 0..2 {
                    let row = [left, right][side];
                    let text = match (row, &bytes[side]) {
                        (Some(row), Some(bytes)) => hex_row(length, row * row_bytes, bytes),
                        _ => String::new(),
                    };
                    sides[side].push(row_rtf(&text, Style::Text, &marks[side]));
                }
            }
            Shown::Fold { .. } => {
                let text = fold_text(&look.strings, &look.numbers, each.hidden().unwrap_or(0));
                let row = row_rtf(&text, Style::Muted, &[]);
                sides[0].push(row.clone());
                sides[1].push(row);
            }
        }
    }
    let view = View {
        rows: Arc::new(shown),
        differences: found.differences,
    };
    Ok(Some((view, render(&sides, options))))
}

/// The patches that turn what the sides' controls show into `sides`, the
/// RTF of each side's rows.
fn render(sides: &[Vec<String>; 2], options: &Options) -> Rendering {
    let look = &options.look;
    let [left, right] =
        [0, 1].map(|side| rtf::patch(&options.shown[side], &sides[side], &look.palette, look.font));
    Rendering {
        patches: [left.1, right.1],
        prints: [left.0, right.0],
    }
}

/// Reads hex rows of a file through a block that it keeps, because the
/// rows that show come in order and close together.
struct RowReader<'a> {
    file: &'a File,
    block: Option<u64>,
    bytes: Vec<u8>,
}

impl<'a> RowReader<'a> {
    const BLOCK: u64 = 64 << 10;

    fn new(file: &'a File) -> RowReader<'a> {
        RowReader {
            file,
            block: None,
            bytes: Vec::new(),
        }
    }

    /// The bytes of hex row `row`; fewer at the file's end.
    fn row(&mut self, row: u64) -> io::Result<Vec<u8>> {
        let offset = row.saturating_mul(HEX_ROW_BYTES as u64);
        let block = offset / Self::BLOCK;
        if self.block != Some(block) {
            self.bytes.resize(Self::BLOCK as usize, 0);
            let read = read_at(self.file, &mut self.bytes, block * Self::BLOCK)?;
            self.bytes.truncate(read);
            self.block = Some(block);
        }
        let start = ((offset % Self::BLOCK) as usize).min(self.bytes.len());
        let end = (start + HEX_ROW_BYTES).min(self.bytes.len());
        Ok(self.bytes[start..end].to_vec())
    }
}

/// Matches `lines`, with the pairs of `anchors`, and builds the rows that
/// show; `None` when cancelled.
fn realign(
    lines: [&[String]; 2],
    anchors: &[(usize, usize)],
    options: &Options,
    cancel: &CancelToken,
) -> Option<(View, Rendering)> {
    let rows = compare::match_lines(lines[0], lines[1], options.matching, anchors, &|| {
        cancel.is_cancelled()
    })?;
    let differences = compare::count_differences(rows.iter().map(|row| row.differs));
    let shown = compare::shown_rows(&rows, options.only_differences, SHOWN_LIMIT);
    drop(rows);
    let look = &options.look;
    let mut sides = [0, 1].map(|_| Vec::with_capacity(shown.rows.len()));
    for each in &shown.rows {
        if cancel.is_cancelled() {
            return None;
        }
        match each {
            Shown::Row {
                left,
                right,
                differs,
            } => {
                let line = |side: usize, row: &Option<u64>| {
                    row.and_then(|row| lines[side].get(usize::try_from(row).ok()?))
                        .map(String::as_str)
                };
                let texts = [line(0, left), line(1, right)];
                let marks = if *differs {
                    compare::row_marks(texts, options.matching)
                } else {
                    [Vec::new(), Vec::new()]
                };
                for side in 0..2 {
                    let text = texts[side].unwrap_or("");
                    sides[side].push(row_rtf(text, Style::Text, &marks[side]));
                }
            }
            Shown::Fold { .. } => {
                let text = fold_text(&look.strings, &look.numbers, each.hidden().unwrap_or(0));
                let row = row_rtf(&text, Style::Muted, &[]);
                sides[0].push(row.clone());
                sides[1].push(row);
            }
        }
    }
    let view = View {
        rows: Arc::new(shown),
        differences,
    };
    Some((view, render(&sides, options)))
}

/// Find's search through the rows that show, on the window's worker,
/// because a side can hold 32 MB.
fn search(find: Find, worker: &OffThread, cancel: &CancelToken) -> Found {
    let Find {
        needle,
        backward,
        side,
        row,
        column,
        rows,
        searched,
        look,
        fresh,
    } = find;
    let files = match &searched {
        Searched::Hex { paths, .. } => match open_both(paths, worker) {
            Ok(files) => files,
            Err((side, error)) => {
                return Found::Failed {
                    path: paths[side].clone(),
                    error,
                };
            }
        },
        Searched::Text(_) => Vec::new(),
    };
    let mut readers: Vec<RowReader<'_>> = files.iter().map(RowReader::new).collect();
    let shown = &rows.rows;
    for (each_side, each_row, at) in compare::find_order(side, row, column, backward, shown.len()) {
        if cancel.is_cancelled() {
            return Found::Cancelled;
        }
        let Some(each) = shown.get(each_row) else {
            continue;
        };
        let text: Cow<'_, str> = match (each.line(each_side), &searched) {
            (None, _) => match each.hidden() {
                Some(hidden) => Cow::Owned(fold_text(&look.strings, &look.numbers, hidden)),
                // A row that the side does not have is empty.
                None => continue,
            },
            (Some(line), Searched::Text(texts)) => {
                let lines = &texts[each_side.index()].lines;
                match usize::try_from(line).ok().and_then(|line| lines.get(line)) {
                    Some(line) => Cow::Borrowed(line.as_str()),
                    None => continue,
                }
            }
            (Some(line), Searched::Hex { paths, sizes }) => {
                let index = each_side.index();
                let bytes = match readers[index].row(line) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return Found::Failed {
                            path: paths[index].clone(),
                            error,
                        };
                    }
                };
                let length = sizes[0].max(sizes[1]);
                Cow::Owned(hex_row(length, line * HEX_ROW_BYTES as u64, &bytes))
            }
        };
        if let Some(columns) = needle.find(&text, at, backward) {
            return Found::Match {
                fresh,
                side: each_side,
                row: each_row,
                columns,
            };
        }
    }
    Found::Nothing
}

/// Encodes every file, then writes them, unless one cannot be encoded: the
/// person asked for them together. The texts are dropped before the writes,
/// because typing while the worker holds a side's text copies it on the
/// window's thread.
fn save(files: Vec<SaveFile>, worker: &OffThread, _cancel: &CancelToken) -> Saved {
    let encoded: Vec<_> = (files.into_iter())
        .map(|file| (file.side, file.path, encode(&file.text)))
        .collect();
    let refused = encoded.iter().any(|(_, _, bytes)| bytes.is_err());
    encoded
        .into_iter()
        .map(|(side, path, bytes)| {
            let saved = match bytes {
                Ok(_) if refused => Err(Unsaved::Withheld),
                Ok(bytes) => save_file(&path, &bytes, worker).map_err(Unsaved::Write),
                Err(unencodable) => Err(unencodable),
            };
            (side, path, saved)
        })
        .collect()
}

/// The bytes that Save writes for `text`.
fn encode(text: &Text) -> Result<Vec<u8>, Unsaved> {
    compare::encoded(&text.lines, text.form, &code_page_encoded).ok_or_else(|| {
        // Only a code page can lack a character.
        let first = match text.form.encoding {
            Encoding::CodePage(page) => {
                compare::first_unencodable(&text.lines, page, &code_page_encoded)
            }
            _ => None,
        };
        Unsaved::Unencodable {
            encoding: text.form.encoding,
            first,
        }
    })
}
