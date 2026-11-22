//! Per-panel row calculation, counting, quick search, mask selection, and
//! the selection's lists in a file or on the clipboard.

use crate::core::commands::CommandId;
use crate::core::open::Entries;
use crate::core::panel::{
    Calculated, Calculation, Counted, Counting, Finding, Found, Selected, Selecting, Side, decoded,
    listed_paths, listing,
};
use crate::core::work::{Id, Replacement, Retirement, Waiting};
use crate::win::clipboard::{self, Contents};
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use gozler_client::CancelToken;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Save selection to file…: the chosen entries, and the file that gets
/// their names, or their full paths when `names` is false.
pub(in crate::app) struct Save {
    pub(in crate::app) entries: Entries,
    pub(in crate::app) file: PathBuf,
    pub(in crate::app) names: bool,
}

impl Save {
    fn items(&self) -> usize {
        self.entries.items()
    }
}

pub(in crate::app) struct Saved {
    pub(in crate::app) file: PathBuf,
    pub(in crate::app) result: io::Result<()>,
}

/// Where Load selection reads its names.
pub(in crate::app) enum Source {
    File(PathBuf),
    Clipboard,
}

impl Source {
    fn command(&self) -> CommandId {
        match self {
            Source::File(_) => CommandId::LoadSelectionFromFile,
            Source::Clipboard => CommandId::LoadSelectionFromClipboard,
        }
    }
}

/// Load selection from file… or from the clipboard.
pub(in crate::app) struct Load {
    pub(in crate::app) source: Source,
    /// The panel's folder, whose entry a line that is only a name names.
    pub(in crate::app) folder: String,
}

pub(in crate::app) struct Loaded {
    /// The command that loads, which titles its messages.
    pub(in crate::app) command: CommandId,
    pub(in crate::app) paths: Result<Arc<[PathBuf]>, LoadFailure>,
}

pub(in crate::app) enum LoadFailure {
    /// Windows could not read the file.
    Unreadable(PathBuf, io::Error),
    /// The file has no line with a name.
    NoNamesInFile(PathBuf),
    /// The clipboard holds no text and no files, or no line with a name.
    NoNamesOnClipboard,
    /// Another program kept the clipboard open through every retry.
    ClipboardBusy,
}

/// The largest file Load selection reads: far more than the names of any
/// folder take, and little enough to allocate.
const LIST_LIMIT: u64 = 64 << 20;

pub(in crate::app) struct Work {
    calculations: [Task<Calculation, Calculated>; 2],
    counts: [Task<Counting, Counted>; 2],
    finding: [Task<Finding, Found>; 2],
    selecting: [Task<Selecting, Selected>; 2],
    saving: [Task<Save, Saved>; 2],
    loading: [Task<Load, Loaded>; 2],
    retired: Retirement,
}

impl Work {
    pub(super) fn new(waker: OpenWaker) -> Self {
        Self {
            calculations: std::array::from_fn(|_| {
                Task::pass(
                    waker.clone(),
                    Replacement::Cancel,
                    Calculation::items,
                    |input, _| input.run(),
                )
            }),
            counts: std::array::from_fn(|_| {
                Task::pass(
                    waker.clone(),
                    Replacement::Cancel,
                    Counting::items,
                    |input, _| input.run(),
                )
            }),
            finding: std::array::from_fn(|_| {
                Task::pass(
                    waker.clone(),
                    Replacement::Finish,
                    Finding::items,
                    |input, _| input.run(),
                )
            }),
            selecting: std::array::from_fn(|_| {
                Task::pass(
                    waker.clone(),
                    Replacement::Cancel,
                    Selecting::items,
                    |input, _| input.run(),
                )
            }),
            saving: std::array::from_fn(|_| {
                Task::new(waker.clone(), Replacement::Cancel, save).with_size(Save::items)
            }),
            loading: std::array::from_fn(|_| Task::new(waker.clone(), Replacement::Cancel, load)),
            retired: Retirement::default(),
        }
    }

    pub(in crate::app) fn save(&mut self, side: Side, input: Save) {
        let task = &mut self.saving[side.index()];
        task.replace(input);
        self.retired.append(task.take_retired());
    }

    pub(in crate::app) fn load(&mut self, side: Side, input: Load) {
        let task = &mut self.loading[side.index()];
        task.replace(input);
        self.retired.append(task.take_retired());
    }

    pub(in crate::app) fn take_saved(&mut self) -> Vec<Saved> {
        let mut answers = Vec::new();
        for task in &mut self.saving {
            if let Some((_, answer)) = task.take() {
                answers.push(answer);
            }
            self.retired.append(task.take_retired());
        }
        answers
    }

    pub(in crate::app) fn take_loaded(&mut self) -> Vec<(Side, Loaded)> {
        let mut answers = Vec::new();
        for side in Side::BOTH {
            let task = &mut self.loading[side.index()];
            if let Some((_, answer)) = task.take() {
                answers.push((side, answer));
            }
            self.retired.append(task.take_retired());
        }
        answers
    }

    /// Whether `side`'s selection list has taken long to save or load.
    pub(super) fn listing_slow(&self, side: Side, now: Instant) -> bool {
        let slow = Some(Waiting::Slow);
        self.saving[side.index()].waiting(now) == slow
            || self.loading[side.index()].waiting(now) == slow
    }

    pub(super) fn cancel_listing(&mut self, side: Side) {
        self.saving[side.index()].cancel();
        self.loading[side.index()].cancel();
    }

    pub(in crate::app) fn calculate(&mut self, side: Side, input: Calculation) {
        let task = &mut self.calculations[side.index()];
        task.replace(input);
        let retired = task.take_retired();
        self.retired.append(retired);
    }

    pub(in crate::app) fn take_calculated(&mut self) -> Vec<(Side, Calculated)> {
        let mut retired = Retirement::default();
        let mut answers = Vec::new();
        for side in Side::BOTH {
            let task = &mut self.calculations[side.index()];
            if let Some((_, answer)) = task.take() {
                answers.push((side, answer));
            }
            retired.append(task.take_retired());
        }
        self.retired.append(retired);
        answers
    }

    pub(in crate::app) fn count(&mut self, side: Side, input: Counting) {
        let task = &mut self.counts[side.index()];
        task.replace(input);
        let retired = task.take_retired();
        self.retired.append(retired);
    }

    pub(in crate::app) fn select(&mut self, side: Side, input: Selecting) -> Id {
        let task = &mut self.selecting[side.index()];
        let id = task.replace(input);
        self.retired.append(task.take_retired());
        id
    }

    pub(in crate::app) fn take_selected(&mut self) -> Vec<(Side, Id, Selected)> {
        let mut answers = Vec::new();
        for side in Side::BOTH {
            let task = &mut self.selecting[side.index()];
            if let Some((id, answer)) = task.take() {
                answers.push((side, id, answer));
            }
            self.retired.append(task.take_retired());
        }
        answers
    }

    pub(in crate::app) fn cancel_matching(&mut self, side: Side) {
        self.selecting[side.index()].cancel();
    }

    pub(in crate::app) fn find(&mut self, side: Side, input: Finding) {
        let task = &mut self.finding[side.index()];
        task.replace(input);
        let retired = task.take_retired();
        self.retired.append(retired);
    }

    pub(in crate::app) fn take_found(&mut self) -> Vec<(Side, Found)> {
        let mut retired = Retirement::default();
        let mut answers = Vec::new();
        for side in Side::BOTH {
            let task = &mut self.finding[side.index()];
            if let Some((_, answer)) = task.take() {
                answers.push((side, answer));
            }
            retired.append(task.take_retired());
        }
        self.retired.append(retired);
        answers
    }

    pub(in crate::app) fn take_counts(&mut self) -> Vec<(Side, Counted)> {
        let mut retired = Retirement::default();
        let mut answers = Vec::new();
        for side in Side::BOTH {
            let task = &mut self.counts[side.index()];
            if let Some((_, answer)) = task.take() {
                answers.push((side, answer));
            }
            retired.append(task.take_retired());
        }
        self.retired.append(retired);
        answers
    }

    pub(in crate::app) fn waiting(&self, side: Side, now: Instant) -> Option<Waiting> {
        self.calculations[side.index()].waiting(now)
    }

    pub(in crate::app) fn cancel(&mut self, side: Side) {
        self.calculations[side.index()].cancel();
    }

    pub(super) fn tick(&mut self, now: Instant) -> bool {
        let mut active = false;
        for task in &mut self.calculations {
            active |= task.tick(now).is_some();
        }
        for task in &mut self.counts {
            active |= task.tick(now).is_some();
        }
        for task in &mut self.finding {
            active |= task.tick(now).is_some();
        }
        for task in &mut self.selecting {
            active |= task.tick(now).is_some();
        }
        for task in &mut self.saving {
            active |= task.tick(now).is_some();
        }
        for task in &mut self.loading {
            active |= task.tick(now).is_some();
        }
        active
    }

    pub(super) fn take_retired(&mut self) -> Retirement {
        for task in &mut self.calculations {
            self.retired.append(task.take_retired());
        }
        for task in &mut self.counts {
            self.retired.append(task.take_retired());
        }
        for task in &mut self.finding {
            self.retired.append(task.take_retired());
        }
        for task in &mut self.selecting {
            self.retired.append(task.take_retired());
        }
        for task in &mut self.saving {
            self.retired.append(task.take_retired());
        }
        for task in &mut self.loading {
            self.retired.append(task.take_retired());
        }
        std::mem::take(&mut self.retired)
    }

    pub(super) fn close(&mut self) -> Retirement {
        for task in &mut self.calculations {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        for task in &mut self.counts {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        for task in &mut self.finding {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        for task in &mut self.selecting {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        for task in &mut self.saving {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        for task in &mut self.loading {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        self.take_retired()
    }
}

fn save(input: Save, _worker: &OffThread, cancel: &CancelToken) -> Saved {
    let paths = super::operations::paths_of(input.entries, cancel);
    let result = if cancel.is_cancelled() {
        Ok(())
    } else {
        std::fs::write(&input.file, listing(&paths, input.names))
    };
    Saved {
        file: input.file,
        result,
    }
}

fn load(input: Load, worker: &OffThread, _cancel: &CancelToken) -> Loaded {
    let command = input.source.command();
    let listed = |text: &str, no_names: LoadFailure| {
        let paths = listed_paths(text, &input.folder);
        if paths.is_empty() {
            Err(no_names)
        } else {
            Ok(paths.into())
        }
    };
    let paths = match input.source {
        Source::File(file) => match super::shell::read_capped(&file, LIST_LIMIT) {
            Ok(bytes) => listed(&decoded(&bytes), LoadFailure::NoNamesInFile(file)),
            Err(error) => Err(LoadFailure::Unreadable(file, error)),
        },
        Source::Clipboard => match clipboard::read(worker) {
            Contents::Text(text) => listed(&text, LoadFailure::NoNamesOnClipboard),
            Contents::Files(paths) if !paths.is_empty() => Ok(paths.into()),
            Contents::Files(_) | Contents::Empty => Err(LoadFailure::NoNamesOnClipboard),
            Contents::Busy => Err(LoadFailure::ClipboardBusy),
        },
    };
    Loaded { command, paths }
}
