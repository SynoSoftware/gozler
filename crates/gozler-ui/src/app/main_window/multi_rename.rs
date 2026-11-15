//! Multi-Rename (DESIGN.md: Multi-Rename): a dialog that shows each chosen
//! entry's new name while the person writes the rule, and starts one batch
//! rename. The work owner computes the names; the engine renames and
//! decides again from the disk. The window keeps the fields and the
//! batches that Undo reverses until Exit.

mod asks;

use super::MainWindow;
use crate::app::file_operations::FileOperations;
use crate::core::captions::{DialogCaptions, DialogItem, Guard, Question, QuestionBody};
use crate::core::commands::{Key, Keystroke, Mark, MenuNode};
use crate::core::fetch::List;
use crate::core::ids::{ControlId as C, DialogId};
use crate::core::layout::{Dpi, Px, Rect, Size};
use crate::core::multi_rename::{
    self, Case, Column, Context, Counter, Counts, Digits, Failed, Fields, Invalid, Known, Outcome,
    Problem, Row, Rule, RuleError, Source, mask::MaskError,
};
use crate::core::panel::{self, CommandPlace, Side};
use crate::core::settings::{DialogSize, History, Mode, MultiRenameState, SavedRename};
use crate::core::text::{Numbers, SizeFormat, Strings};
use crate::core::work::{Replacement, Retirement};
use crate::win::controls::{CheckBox, ComboBox, DropDownList, ListColumn, ListView, Tooltip};
use crate::win::dialog::{self, DialogEnd, DialogMessage, DialogProc, Opened};
use crate::win::locale::WindowsNumbers;
use crate::win::menu::Menu;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use crate::win::{self, Hwnd, window};
use asks::{EditNames, PickRange, SaveName};
use gozler_client::CancelToken;
use gozler_fileops::{InUse, Listed, OperationId, Reason, Rename, Renamed, Request, State};
use jiff::tz::TimeZone;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::Arc;

/// The batches Undo reverses.
const UNDO_BATCHES: usize = 20;
/// The texts that each box's history keeps.
const HISTORY: usize = 20;
/// The timer that computes the names again after a field changed, once
/// the control holds its new text.
const UPDATE: usize = 1;
/// The timer that reads the running batch.
const POLL: usize = 2;
/// The timer that runs Save… or Delete… once Saved renames' list closed.
const SAVED_ACTION: usize = 3;
/// The entries one read of dates taken reads, so the status line counts.
const DATES_CHUNK: usize = 64;

/// What Multi-Rename keeps while the window runs.
#[derive(Default)]
pub(super) struct Session {
    /// The fields as the person left them.
    fields: Fields,
    /// The batches that run, whose renames Undo waits for.
    running: Vec<OperationId>,
    /// The renames of the last batches, newest first.
    batches: VecDeque<Vec<Renamed>>,
}

impl Session {
    /// Keeps the renames of the running batch `id`, which ended with
    /// `outcome`; a batch that renamed nothing is not kept.
    fn ended(&mut self, id: OperationId, outcome: &gozler_fileops::Outcome) {
        let Some(at) = self.running.iter().position(|running| *running == id) else {
            return;
        };
        self.running.swap_remove(at);
        if !outcome.renamed.is_empty() {
            self.batches.push_front(outcome.renamed.clone());
            self.batches.truncate(UNDO_BATCHES);
        }
    }
}

/// The folder panel whose selection follows the renamed entries.
struct Follow {
    side: Side,
    folder: String,
    /// The path of the entry it focused.
    focused: Option<Vec<u16>>,
}

impl MainWindow {
    /// Multi-Rename on the selected entries of `list`, or its focused one.
    pub(super) fn multi_rename_selected(&self, list: List) {
        let place = match list {
            List::Panel(side) => Some(self.model.borrow().panels().command_place(side)),
            List::Search => None,
        };
        self.prepare_selected(list, super::PathAction::MultiRename { place });
    }

    /// Keeps the renames of each Multi-Rename batch that ended, for Undo.
    pub(super) fn keep_ended_renames(&self) {
        let running = self.multi_rename.borrow().running.clone();
        for id in running {
            self.batch_outcome(id);
        }
    }

    /// The outcome of the batch rename `id` once it ended. A Multi-Rename
    /// batch's renames are then kept for Undo.
    fn batch_outcome(&self, id: OperationId) -> Option<Arc<gozler_fileops::Outcome>> {
        let operations = self.file_operations.get()?;
        let outcome = match operations.operation(id).map(|operation| operation.state) {
            Some(State::Ended(outcome)) => outcome,
            Some(_) => return None,
            None => self.model.borrow().operation_outcome(id)?,
        };
        self.multi_rename.borrow_mut().ended(id, &outcome);
        Some(outcome)
    }

    /// Opens Multi-Rename on `rows`, which the panel of `place` or Search
    /// chose; `known` names the entries of a folder panel's folder.
    pub(super) fn multi_rename(
        &self,
        hwnd: Hwnd,
        place: Option<CommandPlace>,
        rows: Vec<Row>,
        known: Option<Known>,
    ) {
        let Some(operations) = self.file_operations.get() else {
            self.retire_chosen(super::Chosen::Rows { rows, known });
            return;
        };
        if rows.is_empty() {
            return;
        }
        let (strings, sizes, kept, follow) = {
            let model = self.model.borrow();
            let panels = model.panels();
            let follow = place
                .map(|place| place.side)
                .filter(|&side| model.mode() == Mode::Commander && panels.search(side).is_none())
                .map(|side| Follow {
                    side,
                    folder: panels.panel(side).folder().to_owned(),
                    focused: panels.focused(side).map(|entry| entry.path),
                });
            (
                model.strings().clone(),
                model.choices().size_format,
                model.multi_rename().clone(),
                follow,
            )
        };
        let folders = rows.windows(2).any(|pair| !pair[0].shares_folder(&pair[1]));
        let state = MultiRename {
            main: self,
            operations,
            numbers: self.numbers.borrow().clone(),
            strings,
            sizes,
            zone: gozler_winfs::local_time_zone(),
            known: known.map(Arc::new),
            folders,
            kept: RefCell::new(kept),
            state: RefCell::new(Shown::new(rows)),
            work: RefCell::new(None),
            list: OnceCell::new(),
            layout: OnceCell::new(),
            dpi: Cell::new(hwnd.dpi()),
        };
        let captions = DialogCaptions::new(DialogId::MultiRename, &state.strings);
        if let Err(error) = dialog::run_modal(hwnd, DialogId::MultiRename, &captions, &state) {
            self.log_error(format!("Multi-Rename did not open: {error}"));
        }
        let MultiRename {
            kept, state: shown, ..
        } = state;
        let shown = shown.into_inner();
        self.multi_rename.borrow_mut().fields = shown.fields.clone();
        if let Some(follow) = follow
            && let Some(names) = shown.following(&follow)
        {
            self.follow_renames(hwnd, &follow, names);
        }
        let kept = kept.into_inner();
        if self.model.borrow().multi_rename() != &kept {
            self.model.borrow_mut().set_multi_rename(kept);
            self.note_saved_state(hwnd);
        }
        let items = shown.rows.len();
        let mut retired = Retirement::default();
        retired.push(shown, items);
        self.created().work.borrow_mut().retire(retired);
    }

    /// Selects the renamed entries of `follow`'s folder under their new
    /// names, once the panel shows them, if it still shows that folder.
    fn follow_renames(&self, hwnd: Hwnd, follow: &Follow, names: Following) {
        let same = {
            let model = self.model.borrow();
            let panels = model.panels();
            model.mode() == Mode::Commander
                && panels.search(follow.side).is_none()
                && panel::same_folder(panels.panel(follow.side).folder(), &follow.folder)
        };
        if !same {
            return;
        }
        self.panel(follow.side).list.deselect_all();
        let update = self.model.borrow_mut().panels_mut().await_names(
            follow.side,
            &names.names,
            names.focus.as_deref(),
            names.operation,
            names.ended,
        );
        if let Some(update) = update {
            let panels = &self.controls().panels;
            panels.apply_panel_update(self, hwnd, follow.side, update);
        }
        self.start_list_passes();
    }
}

/// The names a folder panel awaits after the dialog closes.
struct Following {
    names: Vec<String>,
    focus: Option<String>,
    operation: OperationId,
    ended: bool,
}

struct MultiRename<'a> {
    main: &'a MainWindow,
    operations: &'a FileOperations,
    strings: Strings,
    numbers: WindowsNumbers,
    sizes: SizeFormat,
    zone: TimeZone,
    known: Option<Arc<Known>>,
    /// The entries come from more than one folder, so the list shows the
    /// Folder column.
    folders: bool,
    /// The saved renames, the histories and the size, which the window
    /// saves when the dialog closes.
    kept: RefCell<MultiRenameState>,
    state: RefCell<Shown>,
    work: RefCell<Option<Work>>,
    list: OnceCell<ListView>,
    layout: OnceCell<Layout>,
    dpi: Cell<Dpi>,
}

/// What the dialog shows.
struct Shown {
    fields: Fields,
    rows: Arc<Vec<Row>>,
    /// Each row's new name, as the list shows it.
    outcomes: Vec<Outcome>,
    names: Names,
    /// The names come from Edit names, not from the fields.
    edited: bool,
    dates: Dates,
    sorted: Option<(Column, bool)>,
    batch: Option<Batch>,
    /// What the status line says about the batch that ended last, until
    /// the person changes something.
    finished: Option<Finished>,
    /// The entries the dialog renamed: the path each had when the dialog
    /// opened, and the path it got.
    given: Vec<(Vec<u16>, String)>,
    /// The batch the dialog started last.
    last: Option<OperationId>,
    /// The saved rename chosen last, which Delete… deletes.
    chosen: Option<String>,
}

impl Shown {
    fn new(rows: Vec<Row>) -> Shown {
        Shown {
            fields: Fields::default(),
            rows: Arc::new(rows),
            outcomes: Vec::new(),
            names: Names::Updating,
            edited: false,
            dates: Dates::Unread,
            sorted: None,
            batch: None,
            finished: None,
            given: Vec::new(),
            last: None,
            chosen: None,
        }
    }

    /// Whether the names shown belong to what the dialog shows now.
    fn current(&self) -> bool {
        matches!(self.names, Names::Current) && self.outcomes.len() == self.rows.len()
    }

    fn can_rename(&self) -> bool {
        self.batch.is_none() && self.current() && Counts::of(&self.outcomes).renamed > 0
    }

    /// Whether the person can sort, move and remove rows: not while a batch
    /// runs, nor while dates taken are read, which are read in the rows'
    /// order.
    fn can_edit_list(&self) -> bool {
        self.batch.is_none() && !matches!(self.dates, Dates::Reading(_))
    }

    /// What the panel of `follow` selects after the dialog closes: the new
    /// names that are in its folder.
    fn following(&self, follow: &Follow) -> Option<Following> {
        let mut given = self.given.clone();
        if let Some(batch) = self.batch.as_ref().filter(|batch| !batch.undo) {
            for &index in &batch.submitted {
                let row = &self.rows[index];
                if let Some(name) = self.outcomes[index].renames() {
                    given.push((row.entry.path.clone(), row.folder() + name));
                }
            }
        }
        let in_folder = |path: &str| {
            panel::same_folder(&gozler_core::winpath::parent_of(path), &follow.folder)
                .then(|| panel::last_name(path).to_owned())
        };
        let focus = (given.iter())
            .find(|(old, _)| Some(old) == follow.focused.as_ref())
            .and_then(|(_, path)| in_folder(path));
        let names: Vec<String> = given
            .iter()
            .filter_map(|(_, path)| in_folder(path))
            .collect();
        let operation = self.last?;
        (!names.is_empty()).then_some(Following {
            names,
            focus,
            operation,
            ended: self.batch.is_none(),
        })
    }
}

/// Where the names the list shows stand.
enum Names {
    Current,
    /// The work owner computes them, or dates taken are read first; the
    /// list shows the earlier names.
    Updating,
    /// The fields give no names.
    Failed(RuleError),
}

/// Dates taken, which are read once, the first time a mask uses `[T4]`.
enum Dates {
    Unread,
    /// The dates of the first rows, in the rows' order.
    Reading(Vec<Option<i64>>),
    Read,
}

struct Batch {
    id: OperationId,
    /// Undo's batch, whose entries are not the list's rows.
    undo: bool,
    /// The entries of its request.
    entries: u64,
    /// The index of each row the batch renames, in the request's order.
    submitted: Vec<usize>,
}

struct Finished {
    undo: bool,
    done: usize,
    failed: usize,
}

/// The work owner's tasks, which deliver to the dialog.
struct Work {
    delivery: OpenWaker,
    names: Task<Naming, Vec<Outcome>>,
    dates: Task<Vec<Vec<u16>>, Vec<Option<i64>>>,
}

/// What the names are computed from.
struct Naming {
    rows: Arc<Vec<Row>>,
    /// `None` when the names come from Edit names.
    rule: Option<Rule>,
    known: Option<Arc<Known>>,
    zone: TimeZone,
    numbers: WindowsNumbers,
}

fn name_rows(naming: Naming, _cancel: &CancelToken) -> Vec<Outcome> {
    let source = match &naming.rule {
        Some(rule) => Source::Rule(rule),
        None => Source::Edited,
    };
    let context = Context {
        zone: &naming.zone,
        formats: &naming.numbers,
    };
    multi_rename::preview(&naming.rows, &source, naming.known.as_deref(), &context)
}

fn read_dates(paths: Vec<Vec<u16>>, _worker: &OffThread, cancel: &CancelToken) -> Vec<Option<i64>> {
    win::shell::dates_taken(&paths, || cancel.is_cancelled())
}

/// The places that a resize moves, as the dialog opened.
struct Layout {
    dpi: Dpi,
    client: Size,
    minimum: Size,
    controls: Vec<(u16, Hwnd, Rect)>,
}

/// The list's columns, in their order, with each one's share of the width.
const COLUMNS: [(Column, i32); 6] = [
    (Column::Name, 22),
    (Column::NewName, 22),
    (Column::Note, 18),
    (Column::Size, 10),
    (Column::Modified, 14),
    (Column::Folder, 14),
];

/// The boxes with a history, which Rename remembers.
const HISTORIES: [C; 4] = [
    C::MultiRenameName,
    C::MultiRenameExtension,
    C::MultiRenameSearch,
    C::MultiRenameReplace,
];

/// The placeholder buttons: the box each inserts into, its placeholder
/// (`None` for Range…, which asks), and its tooltip.
const INSERTS: [(C, C, Option<&str>, &str); 8] = [
    (
        C::MultiRenameInsertName,
        C::MultiRenameName,
        Some("[N]"),
        "multi_rename.tip_name",
    ),
    (
        C::MultiRenameNameRange,
        C::MultiRenameName,
        None,
        "multi_rename.tip_range",
    ),
    (
        C::MultiRenameNameCounter,
        C::MultiRenameName,
        Some("[C]"),
        "multi_rename.tip_counter",
    ),
    (
        C::MultiRenameInsertDate,
        C::MultiRenameName,
        Some("[YMD]"),
        "multi_rename.tip_date",
    ),
    (
        C::MultiRenameInsertTime,
        C::MultiRenameName,
        Some("[hms]"),
        "multi_rename.tip_time",
    ),
    (
        C::MultiRenameInsertExtension,
        C::MultiRenameExtension,
        Some("[E]"),
        "multi_rename.tip_extension",
    ),
    (
        C::MultiRenameExtensionRange,
        C::MultiRenameExtension,
        None,
        "multi_rename.tip_range",
    ),
    (
        C::MultiRenameExtensionCounter,
        C::MultiRenameExtension,
        Some("[C]"),
        "multi_rename.tip_counter",
    ),
];

/// The controls that a batch that runs disables, besides the list and
/// the buttons.
const FIELDS: [C; 22] = [
    C::MultiRenameName,
    C::MultiRenameInsertName,
    C::MultiRenameNameRange,
    C::MultiRenameNameCounter,
    C::MultiRenameInsertDate,
    C::MultiRenameInsertTime,
    C::MultiRenameMore,
    C::MultiRenameExtension,
    C::MultiRenameInsertExtension,
    C::MultiRenameExtensionRange,
    C::MultiRenameExtensionCounter,
    C::MultiRenameCase,
    C::MultiRenameSearch,
    C::MultiRenameReplace,
    C::MultiRenameMatchCase,
    C::MultiRenameFirstOnly,
    C::MultiRenameExtensionToo,
    C::MultiRenameRegex,
    C::MultiRenameWholeName,
    C::MultiRenameStart,
    C::MultiRenameStep,
    C::MultiRenameDigits,
];

/// The Digits list's choices: 1 to 10, then Auto.
const DIGITS: usize = 10;

impl MultiRename<'_> {
    fn text(dialog: Hwnd, id: C) -> String {
        dialog::item(dialog, id.id()).map_or_else(String::new, Hwnd::text)
    }

    fn checked(dialog: Hwnd, id: C) -> bool {
        CheckBox::of_dialog(dialog, id.id()).is_some_and(CheckBox::is_checked)
    }

    fn columns(&self) -> &'static [(Column, i32)] {
        match self.folders {
            true => &COLUMNS,
            false => &COLUMNS[..COLUMNS.len() - 1],
        }
    }

    /// The fields as the dialog shows them.
    fn read_fields(dialog: Hwnd) -> Fields {
        let number = |id: C, default: i64| Self::text(dialog, id).trim().parse().unwrap_or(default);
        let defaults = Counter::default();
        let digits = DropDownList::of_dialog(dialog, C::MultiRenameDigits.id())
            .and_then(DropDownList::selected)
            .map_or(defaults.digits, |index| match index {
                DIGITS.. => Digits::Auto,
                index => Digits::Fixed(index as u8 + 1),
            });
        let case = DropDownList::of_dialog(dialog, C::MultiRenameCase.id())
            .and_then(DropDownList::selected)
            .and_then(|index| Case::ALL.get(index).copied())
            .unwrap_or_default();
        Fields {
            name: Self::text(dialog, C::MultiRenameName),
            extension: Self::text(dialog, C::MultiRenameExtension),
            search: Self::text(dialog, C::MultiRenameSearch),
            replace: Self::text(dialog, C::MultiRenameReplace),
            match_case: Self::checked(dialog, C::MultiRenameMatchCase),
            first_only: Self::checked(dialog, C::MultiRenameFirstOnly),
            extension_too: Self::checked(dialog, C::MultiRenameExtensionToo),
            regex: Self::checked(dialog, C::MultiRenameRegex),
            whole_name: Self::checked(dialog, C::MultiRenameWholeName),
            case,
            counter: Counter {
                start: number(C::MultiRenameStart, defaults.start),
                step: number(C::MultiRenameStep, defaults.step),
                digits,
            },
        }
    }

    /// Shows `fields` in the dialog's controls.
    fn show_fields(dialog: Hwnd, fields: &Fields) {
        for (id, text) in [
            (C::MultiRenameName, &fields.name),
            (C::MultiRenameExtension, &fields.extension),
            (C::MultiRenameSearch, &fields.search),
            (C::MultiRenameReplace, &fields.replace),
            (C::MultiRenameStart, &fields.counter.start.to_string()),
            (C::MultiRenameStep, &fields.counter.step.to_string()),
        ] {
            if let Some(control) = dialog::item(dialog, id.id())
                && control.text() != *text
            {
                control.set_text(text);
            }
        }
        for (id, checked) in [
            (C::MultiRenameMatchCase, fields.match_case),
            (C::MultiRenameFirstOnly, fields.first_only),
            (C::MultiRenameExtensionToo, fields.extension_too),
            (C::MultiRenameRegex, fields.regex),
            (C::MultiRenameWholeName, fields.whole_name),
        ] {
            if let Some(check) = CheckBox::of_dialog(dialog, id.id()) {
                check.set_checked(checked);
            }
        }
        if let Some(case) = DropDownList::of_dialog(dialog, C::MultiRenameCase.id()) {
            let index = Case::ALL.iter().position(|case| *case == fields.case);
            case.select(index.unwrap_or(0));
        }
        if let Some(digits) = DropDownList::of_dialog(dialog, C::MultiRenameDigits.id()) {
            digits.select(match fields.counter.digits {
                Digits::Fixed(count) => usize::from(count.clamp(1, DIGITS as u8)) - 1,
                Digits::Auto => DIGITS,
            });
        }
    }

    /// The Saved renames list: (Default), each saved rename, Save… and
    /// Delete….
    fn show_saved(&self, dialog: Hwnd) {
        let Some(list) = DropDownList::of_dialog(dialog, C::MultiRenameSaved.id()) else {
            return;
        };
        let (items, selected) = {
            let kept = self.kept.borrow();
            let items: Vec<String> = std::iter::once(self.strings.text("multi_rename.default"))
                .chain(kept.saved.iter().map(|saved| saved.name.as_str()))
                .chain([
                    self.strings.text("multi_rename.save"),
                    self.strings.text("multi_rename.delete"),
                ])
                .map(str::to_owned)
                .collect();
            let state = self.state.borrow();
            let selected = (state.chosen.as_ref())
                .and_then(|name| kept.saved.iter().position(|saved| saved.name == *name))
                .map_or(0, |index| index + 1);
            (items, selected)
        };
        list.set_items(&items, selected);
    }

    /// Gives each box with a history its texts.
    fn show_histories(&self, dialog: Hwnd) {
        let kept = self.kept.borrow();
        for id in HISTORIES {
            if let Some(combo) = ComboBox::of_dialog(dialog, id.id()) {
                combo.set_items(history(&kept.history, id));
            }
        }
    }

    /// Something the names depend on changed: the names are computed again
    /// once the control holds its new value, and a failed row is named
    /// again.
    fn changed(&self, dialog: Hwnd) {
        {
            let mut state = self.state.borrow_mut();
            state.finished = None;
            // `Enter` comes before the timer, so Rename refuses the names
            // shown until they belong to the change.
            state.names = Names::Updating;
            if state.rows.iter().any(|row| row.failed.is_some()) {
                for row in Arc::make_mut(&mut state.rows) {
                    row.failed = None;
                }
            }
        }
        if let Some(work) = self.work.borrow_mut().as_mut() {
            // Names computed before the change no longer fit the rows.
            work.names.cancel();
        }
        dialog.set_timer(UPDATE, 1);
    }

    /// Computes the names from what the dialog shows now.
    fn update(&self, dialog: Hwnd) {
        dialog.kill_timer(UPDATE);
        let fields = Self::read_fields(dialog);
        let rule = {
            let mut state = self.state.borrow_mut();
            if state.batch.is_some() {
                return;
            }
            state.fields = fields.clone();
            if state.edited {
                Ok(None)
            } else {
                Rule::new(&fields).map(Some)
            }
        };
        let rule = match rule {
            Ok(rule) => rule,
            Err(error) => {
                self.state.borrow_mut().names = Names::Failed(error);
                self.show(dialog);
                return;
            }
        };
        let reads_dates = rule.as_ref().is_some_and(Rule::reads_date_taken);
        let unread = matches!(self.state.borrow().dates, Dates::Unread);
        if reads_dates && unread {
            self.state.borrow_mut().dates = Dates::Reading(Vec::new());
            self.read_more_dates();
        }
        let waits = reads_dates && !matches!(self.state.borrow().dates, Dates::Read);
        self.state.borrow_mut().names = Names::Updating;
        if !waits {
            let naming = Naming {
                rows: Arc::clone(&self.state.borrow().rows),
                rule,
                known: self.known.clone(),
                zone: self.zone.clone(),
                numbers: self.numbers.clone(),
            };
            if let Some(work) = self.work.borrow_mut().as_mut() {
                work.names.replace(naming);
            }
            // A short list's names are ready at once.
            self.deliver(dialog);
        }
        self.show(dialog);
    }

    /// Reads the dates taken of the next rows.
    fn read_more_dates(&self) {
        let paths: Vec<Vec<u16>> = {
            let state = self.state.borrow();
            let Dates::Reading(read) = &state.dates else {
                return;
            };
            (state.rows[read.len()..].iter())
                .take(DATES_CHUNK)
                .map(|row| row.entry.path.clone())
                .collect()
        };
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.dates.replace(paths);
        }
    }

    /// Takes what the work owner delivered.
    fn deliver(&self, dialog: Hwnd) {
        let (names, dates, retired) = {
            let mut work = self.work.borrow_mut();
            let Some(work) = work.as_mut() else {
                return;
            };
            work.delivery.woken();
            let names = work.names.take();
            let dates = work.dates.take();
            let mut retired = work.names.take_retired();
            retired.append(work.dates.take_retired());
            (names, dates, retired)
        };
        if !retired.is_empty() {
            self.main.created().work.borrow_mut().retire(retired);
        }
        if let Some((_, outcomes)) = names {
            let mut state = self.state.borrow_mut();
            if outcomes.len() == state.rows.len() && matches!(state.names, Names::Updating) {
                state.outcomes = outcomes;
                state.names = Names::Current;
            }
        }
        if let Some((_, dates)) = dates {
            self.dates_read(dialog, dates);
        }
        self.show(dialog);
    }

    /// Keeps the dates of the rows read last, and reads on or names the
    /// rows once every row has its date.
    fn dates_read(&self, dialog: Hwnd, dates: Vec<Option<i64>>) {
        let done = {
            let mut state = self.state.borrow_mut();
            let total = state.rows.len();
            let Dates::Reading(read) = &mut state.dates else {
                return;
            };
            read.extend(dates);
            if read.len() < total {
                false
            } else {
                let read = std::mem::take(read);
                for (row, taken) in Arc::make_mut(&mut state.rows).iter_mut().zip(read) {
                    row.taken = taken;
                }
                state.dates = Dates::Read;
                true
            }
        };
        if done {
            self.update(dialog);
        } else {
            self.read_more_dates();
        }
    }

    /// Shows the list, the status line and which controls work.
    fn show(&self, dialog: Hwnd) {
        let (count, status, can_rename, running, edited, can_edit) = {
            let state = self.state.borrow();
            (
                state.rows.len() as u64,
                self.status(&state),
                state.can_rename(),
                state.batch.is_some(),
                state.edited,
                state.can_edit_list(),
            )
        };
        if let Some(list) = self.list.get() {
            list.set_count(count);
        }
        if let Some(control) = dialog::item(dialog, C::MultiRenameStatus.id())
            && control.text() != status
        {
            control.set_text(&status);
        }
        let regex = Self::checked(dialog, C::MultiRenameRegex);
        for id in FIELDS {
            if let Some(control) = dialog::item(dialog, id.id()) {
                let shown = !running && !edited;
                control.set_enabled(shown && (id != C::MultiRenameWholeName || regex));
            }
        }
        let undo = !running && !self.main.multi_rename.borrow().batches.is_empty();
        for (id, enabled) in [
            (C::MultiRenameSaved.id(), !running),
            (C::MultiRenameList.id(), !running),
            (C::MultiRenameEdit.id(), !running && count > 0 && can_edit),
            (C::MultiRenameUndo.id(), undo),
            (DialogItem::Ok.id(), can_rename),
        ] {
            if let Some(control) = dialog::item(dialog, id) {
                control.set_enabled(enabled);
            }
        }
    }

    /// The status line: what Rename will do, or what is under way.
    fn status(&self, state: &Shown) -> String {
        let numbers = &self.numbers;
        let count = |key: &str, count: usize, args: &[&str]| {
            self.strings.count(key, count as u64, numbers, args)
        };
        if let Some(batch) = &state.batch {
            let (done, total) = match self.operations.operation(batch.id).map(|batch| batch.state) {
                Some(State::Running(running)) => (
                    running.items,
                    running.total.map_or(batch.entries, |total| total.items),
                ),
                _ => (0, batch.entries),
            };
            return self.strings.format(
                "multi_rename.renaming",
                &[&numbers.integer(done), &numbers.integer(total)],
            );
        }
        if let Names::Failed(error) = &state.names {
            return match error {
                RuleError::Mask(MaskError::Unknown(placeholder)) => self
                    .strings
                    .format("multi_rename.unknown_placeholder", &[placeholder]),
                RuleError::Mask(MaskError::Unclosed) => {
                    self.strings.text("multi_rename.unclosed").to_owned()
                }
                RuleError::Regex(reason) => {
                    self.strings.format("multi_rename.invalid_regex", &[reason])
                }
            };
        }
        if let Dates::Reading(read) = &state.dates
            && matches!(state.names, Names::Updating)
        {
            return self.strings.format(
                "multi_rename.reading_dates",
                &[
                    &numbers.integer(read.len() as u64),
                    &numbers.integer(state.rows.len() as u64),
                ],
            );
        }
        if let Some(finished) = &state.finished {
            let (done, failed) = match finished.undo {
                true => ("multi_rename.undone", "multi_rename.not_undone"),
                false => ("multi_rename.renamed", "multi_rename.not_renamed"),
            };
            let mut text = count(done, finished.done, &[]);
            if finished.failed > 0 {
                text = format!("{text} {}", count(failed, finished.failed, &[]));
            }
            return text;
        }
        if !state.current() {
            return self.strings.text("multi_rename.updating").to_owned();
        }
        let counts = Counts::of(&state.outcomes);
        let renamed = numbers.integer(counts.renamed as u64);
        let mut parts = vec![count(
            "multi_rename.get_new_names",
            counts.rows,
            &[&renamed],
        )];
        if counts.numbered > 0 {
            parts.push(count("multi_rename.numbered_count", counts.numbered, &[]));
        }
        if counts.kept > 0 {
            parts.push(count("multi_rename.kept_count", counts.kept, &[]));
        }
        if state.edited {
            parts.push(self.strings.text("multi_rename.from_edit_names").to_owned());
        }
        parts.join(" ")
    }

    /// The Note column's text of `outcome`.
    fn note(&self, outcome: &Outcome) -> String {
        let text = |key: &str| self.strings.text(key).to_owned();
        match outcome {
            Outcome::New {
                numbered: false, ..
            } => String::new(),
            Outcome::New { numbered: true, .. } => text("multi_rename.numbered"),
            Outcome::NoChange => text("multi_rename.no_change"),
            Outcome::Kept(Problem::NoDateTaken) => text("multi_rename.no_date_taken"),
            Outcome::Kept(Problem::NoDateModified) => text("multi_rename.no_date_modified"),
            Outcome::Kept(Problem::Invalid(invalid)) => {
                let reason = match invalid {
                    Invalid::Characters => text("name.characters"),
                    Invalid::Reserved(name) => self.strings.format("name.reserved", &[name]),
                    Invalid::TooLong => text("multi_rename.too_long"),
                    Invalid::Empty => text("multi_rename.empty"),
                    Invalid::AboveDrive => text("multi_rename.above_drive"),
                    Invalid::Drive => text("multi_rename.starts_with_drive"),
                };
                self.strings.format("multi_rename.not_valid", &[&reason])
            }
            Outcome::Failed(failed) => {
                let reason =
                    win::error_text(&std::io::Error::from_raw_os_error(failed.error as i32));
                self.strings.format("multi_rename.failed", &[&reason])
            }
        }
    }

    /// Rename: one batch with the rows that get a new name, in the list's
    /// order.
    fn rename(&self, dialog: Hwnd) {
        let (entries, submitted) = {
            let state = self.state.borrow();
            if !state.can_rename() {
                return;
            }
            let mut entries = Vec::new();
            let mut submitted = Vec::new();
            for (index, (row, outcome)) in state.rows.iter().zip(&state.outcomes).enumerate() {
                if let Some(name) = outcome.renames() {
                    let path = PathBuf::from(OsString::from_wide(&row.entry.path));
                    entries.push(Rename::new(path, PathBuf::from(name)));
                    submitted.push(index);
                }
            }
            (entries, submitted)
        };
        self.remember_texts(dialog);
        let request = Request::Rename {
            entries,
            in_use: InUse::Number,
        };
        let Some(id) = self.main.start_operation(request, None) else {
            return;
        };
        self.main.multi_rename.borrow_mut().running.push(id);
        self.start(
            dialog,
            Batch {
                id,
                undo: false,
                entries: submitted.len() as u64,
                submitted,
            },
        );
    }

    /// The batch runs: the fields, the list and Rename wait for it, and
    /// Close keeps the focus, because the control that had it is disabled.
    fn start(&self, dialog: Hwnd, batch: Batch) {
        {
            let mut state = self.state.borrow_mut();
            state.finished = None;
            state.last = (!batch.undo).then_some(batch.id).or(state.last);
            state.batch = Some(batch);
        }
        dialog.set_timer(POLL, 200);
        self.show(dialog);
        if let Some(close) = dialog::item(dialog, DialogItem::Cancel.id()) {
            close.focus();
        }
    }

    /// Puts the texts of the boxes with a history first in their
    /// histories.
    fn remember_texts(&self, dialog: Hwnd) {
        {
            let mut kept = self.kept.borrow_mut();
            for id in HISTORIES {
                let text = Self::text(dialog, id);
                if text.is_empty() {
                    continue;
                }
                let texts = history_mut(&mut kept.history, id);
                texts.retain(|kept| *kept != text);
                texts.insert(0, text);
                texts.truncate(HISTORY);
            }
        }
        self.show_histories(dialog);
    }

    /// Reads the running batch, and takes its end.
    fn poll(&self, dialog: Hwnd) {
        let Some(id) = self.state.borrow().batch.as_ref().map(|batch| batch.id) else {
            dialog.kill_timer(POLL);
            return;
        };
        self.operations.request_refresh();
        match self.main.batch_outcome(id) {
            Some(outcome) => self.batch_ended(dialog, &outcome.renamed, &outcome.listed),
            // A batch that left the queue without an outcome ends with
            // nothing known of it.
            None if !self.operations.is_pending(id) && self.operations.operation(id).is_none() => {
                self.batch_ended(dialog, &[], &[]);
            }
            None => self.show(dialog),
        }
    }

    /// The batch ended with `renamed` and the failures `listed`: the
    /// renamed rows leave the list, and each failed row stays with its new
    /// name and Windows' reason, so Rename tries it again.
    fn batch_ended(&self, dialog: Hwnd, renamed: &[Renamed], listed: &[Listed]) {
        dialog.kill_timer(POLL);
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let Some(batch) = state.batch.take() else {
                return;
            };
            if !batch.undo {
                let mut gone = vec![false; state.rows.len()];
                for each in renamed {
                    if let Some(&index) = batch.submitted.get(each.entry) {
                        gone[index] = true;
                        let to = each.to.to_string_lossy().into_owned();
                        state.given.push((state.rows[index].entry.path.clone(), to));
                    }
                }
                let by_path: HashMap<&[u16], usize> = (batch.submitted.iter())
                    .map(|&index| (state.rows[index].entry.path.as_slice(), index))
                    .collect();
                let failures: Vec<(usize, Failed)> = (listed.iter())
                    .filter_map(|listed| {
                        let Reason::Failed(error) = listed.reason else {
                            return None;
                        };
                        let path: Vec<u16> = listed.path.as_os_str().encode_wide().collect();
                        let &index = by_path.get(path.as_slice())?;
                        let name = state.outcomes[index].name(&state.rows[index]).to_owned();
                        Some((index, Failed { name, error }))
                    })
                    .collect();
                drop(by_path);
                let rows = Arc::make_mut(&mut state.rows);
                for (index, failed) in failures {
                    rows[index].failed = Some(failed);
                }
                let mut keep = gone.iter().map(|gone| !gone);
                rows.retain(|_| keep.next().unwrap_or(true));
                let mut keep = gone.iter().map(|gone| !gone);
                state.outcomes.retain(|_| keep.next().unwrap_or(true));
            }
            state.finished = Some(Finished {
                undo: batch.undo,
                done: renamed.len(),
                failed: listed.len(),
            });
        }
        self.update(dialog);
        if let Some(name) = dialog::item(dialog, C::MultiRenameName.id()) {
            name.focus();
        }
    }

    /// Undo: asks, then renames the entries of the last batch back.
    fn undo(&self, dialog: Hwnd) {
        self.main.keep_ended_renames();
        let question = {
            let session = self.main.multi_rename.borrow();
            let Some(batch) = session.batches.front() else {
                return;
            };
            let name = |path: &std::path::Path| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            let instruction = match batch.as_slice() {
                [one] => self.strings.format(
                    "multi_rename.undo_entry",
                    &[&name(&one.from), &name(&one.to)],
                ),
                all => self.strings.format(
                    "multi_rename.undo_entries",
                    &[&self.numbers.integer(all.len() as u64)],
                ),
            };
            Question {
                title: self.strings.text("multi_rename.undo_title").to_owned(),
                instruction,
                body: QuestionBody::Text(self.strings.text("multi_rename.undo_note").to_owned()),
                footer: None,
                confirm: self.strings.text("multi_rename.undo_button").to_owned(),
                cancel: self.strings.text("button.cancel").to_owned(),
                guard: Guard::None,
                elevates: false,
            }
        };
        if !dialog::ask(dialog, &question) {
            return;
        }
        let Some(batch) = self.main.multi_rename.borrow_mut().batches.pop_front() else {
            return;
        };
        let request = Request::Rename {
            entries: multi_rename::undo(&batch),
            in_use: InUse::Fail,
        };
        if let Some(id) = self.main.start_operation(request, None) {
            self.start(
                dialog,
                Batch {
                    id,
                    undo: true,
                    entries: batch.len() as u64,
                    submitted: Vec::new(),
                },
            );
        }
    }

    /// Inserts `text` at the caret of the box `id`, in place of its
    /// selected text, and gives the box the focus.
    fn insert(dialog: Hwnd, id: C, text: &str) {
        if let Some(combo) = ComboBox::of_dialog(dialog, id.id()) {
            combo.insert(text);
        }
    }

    /// Range…: the person selects characters of the focused row's name or
    /// extension, and their range goes into the box `id`.
    fn pick_range(&self, dialog: Hwnd, id: C) {
        let (letter, text) = {
            let state = self.state.borrow();
            let focused = self.list.get().and_then(|list| list.focused()).unwrap_or(0);
            let Some(row) = state.rows.get(focused as usize) else {
                return;
            };
            match id {
                C::MultiRenameName => ('N', row.stem().to_owned()),
                _ => ('E', row.extension().to_owned()),
            }
        };
        let pick = PickRange::new(text);
        let captions = DialogCaptions::new(DialogId::PickRange, &self.strings);
        let ended = dialog::run_modal(dialog, DialogId::PickRange, &captions, &pick);
        if matches!(ended, Ok(DialogEnd::Ok))
            && let Some(range) = pick.range(letter)
        {
            Self::insert(dialog, id, &range);
        }
    }

    /// More: a menu of the other placeholders, which go into the name box.
    fn more(&self, dialog: Hwnd) {
        let Some(button) = dialog::item(dialog, C::MultiRenameMore.id()) else {
            return;
        };
        let mut items = Vec::new();
        let mut placeholders = Vec::new();
        for (index, group) in multi_rename::MORE.iter().enumerate() {
            if index > 0 {
                items.push(MenuNode::Separator);
            }
            for &(placeholder, key) in *group {
                placeholders.push(placeholder);
                items.push(MenuNode::Item {
                    id: placeholders.len() as u16,
                    text: format!("{}\t{placeholder}", self.strings.text(key)),
                    mark: Mark::None,
                    glyph: None,
                });
            }
        }
        let rect = button.screen_rect();
        let point = (rect.x.0, rect.bottom().0);
        if let Some(id) = Menu::track_popup(dialog, &items, point, &[], None)
            && let Some(placeholder) = placeholders.get(usize::from(id).wrapping_sub(1))
        {
            Self::insert(dialog, C::MultiRenameName, placeholder);
        }
    }

    /// Edit…: the person types or pastes the new names.
    fn edit_names(&self, dialog: Hwnd) {
        let (text, rows) = {
            let state = self.state.borrow();
            let names: Vec<&str> = (state.rows.iter().zip(&state.outcomes))
                .map(|(row, outcome)| outcome.name(row))
                .collect();
            (names.join("\r\n"), state.rows.len())
        };
        let edit = EditNames::new(&self.strings, &self.numbers, text, rows);
        let captions = DialogCaptions::new(DialogId::EditNames, &self.strings);
        let ended = dialog::run_modal(dialog, DialogId::EditNames, &captions, &edit);
        let Some(names) = edit.names().filter(|_| matches!(ended, Ok(DialogEnd::Ok))) else {
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            for (row, name) in Arc::make_mut(&mut state.rows).iter_mut().zip(names) {
                row.edited = Some(name);
            }
            state.edited = true;
        }
        self.changed(dialog);
    }

    /// The item that Saved renames shows: (Default), each saved rename,
    /// Save… and Delete….
    fn saved_item(dialog: Hwnd) -> Option<usize> {
        DropDownList::of_dialog(dialog, C::MultiRenameSaved.id()).and_then(DropDownList::selected)
    }

    /// A selection in Saved renames: (Default) or a saved rename fills the
    /// fields at once. Save… and Delete… wait for [`Self::run_saved`], so
    /// the arrow keys pass them by.
    fn select_saved(&self, dialog: Hwnd) {
        let Some(index) = Self::saved_item(dialog) else {
            return;
        };
        if index == 0 {
            self.apply(dialog, None, Fields::default());
            return;
        }
        let chosen = self.kept.borrow().saved.get(index - 1).cloned();
        if let Some(chosen) = chosen {
            self.apply(dialog, Some(chosen.name), chosen.fields);
        }
    }

    /// Save… or Delete…, which the person chose in the open list of Saved
    /// renames, once the list has closed.
    fn run_saved(&self, dialog: Hwnd) {
        dialog.kill_timer(SAVED_ACTION);
        let Some(index) = Self::saved_item(dialog) else {
            return;
        };
        let saved = self.kept.borrow().saved.len();
        if index == saved + 1 {
            self.save(dialog);
        } else if index == saved + 2 {
            self.delete_saved(dialog);
        }
    }

    /// Shows `fields`, from the saved rename `chosen` or the defaults, and
    /// returns to the fields from Edit names.
    fn apply(&self, dialog: Hwnd, chosen: Option<String>, fields: Fields) {
        {
            let mut state = self.state.borrow_mut();
            state.chosen = chosen;
            state.edited = false;
        }
        Self::show_fields(dialog, &fields);
        self.show_saved(dialog);
        self.changed(dialog);
    }

    /// Save…: keeps the fields under a name.
    fn save(&self, dialog: Hwnd) {
        let chosen = self.state.borrow().chosen.clone();
        let ask = SaveName::new(chosen.unwrap_or_default());
        let captions = DialogCaptions::new(DialogId::SaveRename, &self.strings);
        let ended = dialog::run_modal(dialog, DialogId::SaveRename, &captions, &ask);
        if let Some(name) = ask.name().filter(|_| matches!(ended, Ok(DialogEnd::Ok))) {
            let exists = self
                .kept
                .borrow()
                .saved
                .iter()
                .any(|saved| saved.name == name);
            let replace = !exists || {
                let question = Question {
                    title: self.strings.text("multi_rename.save_title").to_owned(),
                    instruction: self
                        .strings
                        .format("multi_rename.replace_question", &[&name]),
                    body: QuestionBody::Text(String::new()),
                    footer: None,
                    confirm: self.strings.text("multi_rename.replace_button").to_owned(),
                    cancel: self.strings.text("button.cancel").to_owned(),
                    guard: Guard::None,
                    elevates: false,
                };
                dialog::ask(dialog, &question)
            };
            if replace {
                let fields = Self::read_fields(dialog);
                let mut kept = self.kept.borrow_mut();
                kept.saved.retain(|saved| saved.name != name);
                kept.saved.push(SavedRename {
                    name: name.clone(),
                    fields,
                });
                drop(kept);
                self.state.borrow_mut().chosen = Some(name);
            }
        }
        self.show_saved(dialog);
    }

    /// Delete…: removes the saved rename chosen last.
    fn delete_saved(&self, dialog: Hwnd) {
        let chosen = self.state.borrow().chosen.clone();
        if let Some(name) = chosen {
            let question = Question {
                title: self.strings.text("multi_rename.delete_title").to_owned(),
                instruction: self
                    .strings
                    .format("multi_rename.delete_question", &[&name]),
                body: QuestionBody::Text(String::new()),
                footer: None,
                confirm: self.strings.text("multi_rename.delete_button").to_owned(),
                cancel: self.strings.text("button.cancel").to_owned(),
                guard: Guard::None,
                elevates: false,
            };
            if dialog::ask(dialog, &question) {
                self.kept
                    .borrow_mut()
                    .saved
                    .retain(|saved| saved.name != name);
                self.state.borrow_mut().chosen = None;
            }
        }
        self.show_saved(dialog);
    }

    /// Sorts the list by the column at `index`; again reverses the order.
    fn sort(&self, index: usize) {
        let Some(&(column, _)) = self.columns().get(index) else {
            return;
        };
        let descending = {
            let mut state = self.state.borrow_mut();
            if !state.can_edit_list() {
                return;
            }
            let descending = state.sorted == Some((column, false));
            let order = multi_rename::sorted(&state.rows, &state.outcomes, column, descending);
            let rows = Arc::make_mut(&mut state.rows);
            *rows = in_order(std::mem::take(rows), &order);
            if state.outcomes.len() == order.len() {
                state.outcomes = in_order(std::mem::take(&mut state.outcomes), &order);
            }
            state.sorted = Some((column, descending));
            descending
        };
        if let Some(list) = self.list.get() {
            list.show_sort(self.columns().len(), Some((index, !descending)));
        }
    }

    /// Moves the focused row up or down one place.
    fn move_row(&self, down: bool) -> bool {
        let Some(list) = self.list.get() else {
            return false;
        };
        let Some(row) = list.focused().map(|row| row as usize) else {
            return false;
        };
        let to = {
            let mut state = self.state.borrow_mut();
            let to = match down {
                true => row + 1,
                false => row.wrapping_sub(1),
            };
            if !state.can_edit_list() || to >= state.rows.len() {
                return true;
            }
            Arc::make_mut(&mut state.rows).swap(row, to);
            if state.outcomes.len() > to.max(row) {
                state.outcomes.swap(row, to);
            }
            state.sorted = None;
            to
        };
        list.show_sort(self.columns().len(), None);
        list.select_only(Some(to as u64));
        list.ensure_visible(to as u64);
        true
    }

    /// Removes the selected rows from the list.
    fn remove_rows(&self) {
        let Some(list) = self.list.get() else {
            return;
        };
        let mut selected = Vec::new();
        let mut after = None;
        while let Some(row) = list.next_selected(after) {
            selected.push(row as usize);
            after = Some(row);
        }
        let Some(&first) = selected.first() else {
            return;
        };
        let left = {
            let mut state = self.state.borrow_mut();
            if !state.can_edit_list() {
                return;
            }
            let mut removed = vec![false; state.rows.len()];
            for &row in &selected {
                removed[row] = true;
            }
            let mut keep = removed.iter().map(|removed| !removed);
            Arc::make_mut(&mut state.rows).retain(|_| keep.next().unwrap_or(true));
            if state.outcomes.len() == removed.len() {
                let mut keep = removed.iter().map(|removed| !removed);
                state.outcomes.retain(|_| keep.next().unwrap_or(true));
            }
            state.rows.len()
        };
        list.deselect_all();
        list.set_count(left as u64);
        if left > 0 {
            list.select_only(Some(first.min(left - 1) as u64));
        }
    }

    /// The list's context menu: Remove from list.
    fn list_menu(&self, dialog: Hwnd, at: Option<(i32, i32)>) {
        let Some(list) = self.list.get() else {
            return;
        };
        let Some(point) = at.or_else(|| list.focused_row_point()) else {
            return;
        };
        let items = [MenuNode::Item {
            id: 1,
            text: self.strings.text("multi_rename.remove").to_owned(),
            mark: Mark::None,
            glyph: None,
        }];
        let enabled = list.selected_count() > 0 && self.state.borrow().can_edit_list();
        if Menu::track_popup(dialog, &items, point, &[(1, enabled, false)], None) == Some(1) {
            self.remove_rows();
            self.changed(dialog);
        }
    }

    /// `Shift+Delete` in the open list of a box with a history removes the
    /// highlighted text from it.
    fn forget_text(&self, dialog: Hwnd, target: Hwnd) -> bool {
        for id in HISTORIES {
            let Some(combo) = ComboBox::of_dialog(dialog, id.id()) else {
                continue;
            };
            if target.parent() != combo.hwnd() || !combo.is_open() {
                continue;
            }
            let Some(index) = combo.highlighted() else {
                return true;
            };
            let mut kept = self.kept.borrow_mut();
            let texts = history_mut(&mut kept.history, id);
            if index < texts.len() {
                texts.remove(index);
                drop(kept);
                combo.remove(index);
            }
            return true;
        }
        false
    }

    /// A key pressed in the dialog, before the control has it.
    fn key(&self, dialog: Hwnd, target: Hwnd, keystroke: Keystroke) -> bool {
        let in_list = self.list.get().is_some_and(|list| list.hwnd() == target);
        match keystroke {
            Keystroke {
                key: Key::Function(2),
                ctrl: false,
                alt: false,
                shift: false,
            } => {
                if let Some(saved) = DropDownList::of_dialog(dialog, C::MultiRenameSaved.id()) {
                    saved.hwnd().focus();
                    saved.open();
                }
                true
            }
            Keystroke {
                key: Key::Delete,
                ctrl: false,
                alt: false,
                shift: false,
            } if in_list => {
                self.remove_rows();
                self.changed(dialog);
                true
            }
            Keystroke {
                key: key @ (Key::Up | Key::Down),
                ctrl: false,
                alt: true,
                shift: false,
            } if in_list => {
                let moved = self.move_row(key == Key::Down);
                self.changed(dialog);
                moved
            }
            Keystroke {
                key: Key::Delete,
                ctrl: false,
                alt: false,
                shift: true,
            } => self.forget_text(dialog, target),
            _ => false,
        }
    }

    /// Gives the list's columns their share of its width.
    fn fit_columns(&self) {
        let Some(list) = self.list.get() else {
            return;
        };
        let width = list.hwnd().client_size().width.0;
        let shares: i32 = self.columns().iter().map(|(_, share)| share).sum();
        list.set_column_widths(
            (self.columns().iter().enumerate())
                .map(|(index, (_, share))| (index, Px(width * share / shares))),
        );
    }
}

/// `items` with the item at `order[0]` first, then `order[1]`.
fn in_order<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut items: Vec<Option<T>> = items.into_iter().map(Some).collect();
    (order.iter())
        .map(|&index| items[index].take().expect("the order names each item once"))
        .collect()
}

/// The history of the box `id`.
fn history(history: &History, id: C) -> &[String] {
    match id {
        C::MultiRenameName => &history.name,
        C::MultiRenameExtension => &history.extension,
        C::MultiRenameSearch => &history.search,
        _ => &history.replace,
    }
}

fn history_mut(history: &mut History, id: C) -> &mut Vec<String> {
    match id {
        C::MultiRenameName => &mut history.name,
        C::MultiRenameExtension => &mut history.extension,
        C::MultiRenameSearch => &mut history.search,
        _ => &mut history.replace,
    }
}

impl DialogProc for MultiRename<'_> {
    fn init(&self, dialog: Hwnd) -> Opened {
        let delivery = OpenWaker::posting(dialog, dialog::REPLY);
        *self.work.borrow_mut() = Some(Work {
            names: Task::pass(
                delivery.clone(),
                Replacement::Cancel,
                |naming: &Naming| naming.rows.len(),
                name_rows,
            ),
            dates: Task::new(delivery.clone(), Replacement::Cancel, read_dates),
            delivery,
        });
        window::own_keys(dialog, true);
        let fields = self.main.multi_rename.borrow().fields.clone();
        self.show_histories(dialog);
        if let Some(case) = DropDownList::of_dialog(dialog, C::MultiRenameCase.id()) {
            let labels: Vec<&str> = Case::ALL
                .iter()
                .map(|case| self.strings.text(case.label()))
                .collect();
            case.set_items(&labels, 0);
        }
        if let Some(digits) = DropDownList::of_dialog(dialog, C::MultiRenameDigits.id()) {
            let items: Vec<String> = (1..=DIGITS as u64)
                .map(|count| self.numbers.integer(count))
                .chain([self.strings.text("multi_rename.auto").to_owned()])
                .collect();
            digits.set_items(&items, 0);
        }
        for id in [C::MultiRenameName, C::MultiRenameExtension] {
            if let Some(mask) = ComboBox::of_dialog(dialog, id.id()) {
                mask.keep_caret();
            }
        }
        Self::show_fields(dialog, &fields);
        self.show_saved(dialog);
        let buttons: Vec<Hwnd> = (INSERTS.iter())
            .map(|(button, ..)| button.id())
            .chain([C::MultiRenameMore.id()])
            .filter_map(|id| dialog::item(dialog, id))
            .collect();
        if let Ok(tips) = Tooltip::create(dialog, &buttons) {
            for (button, _, _, tip) in INSERTS {
                if let Some(control) = dialog::item(dialog, button.id()) {
                    tips.set(control, self.strings.text(tip));
                }
            }
            if let Some(more) = dialog::item(dialog, C::MultiRenameMore.id()) {
                tips.set(more, self.strings.text("multi_rename.tip_more"));
            }
        }
        if let Some(list) = ListView::of_dialog(dialog, C::MultiRenameList.id()) {
            for (index, (column, _)) in self.columns().iter().enumerate() {
                let (header, right_aligned) = match column {
                    Column::Name => ("header.name", false),
                    Column::NewName => ("multi_rename.new_name", false),
                    Column::Note => ("multi_rename.note", false),
                    Column::Size => ("header.size", true),
                    Column::Modified => ("header.date_modified", false),
                    Column::Folder => ("header.folder", false),
                };
                list.insert_column(
                    index,
                    &ListColumn {
                        header: self.strings.text(header),
                        width: Px(0),
                        right_aligned,
                    },
                );
            }
            let _ = self.list.set(list);
            self.fit_columns();
        }
        self.update(dialog);
        Opened {
            focus: C::MultiRenameName.id(),
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        match message {
            DialogMessage::Command { id } if id == DialogItem::Ok.id() => {
                self.rename(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameUndo.id() => {
                self.undo(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameEdit.id() => {
                self.edit_names(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameMore.id() => {
                self.more(dialog);
                true
            }
            // Close ends the dialog; a batch that runs goes on.
            DialogMessage::Command { id } if id == DialogItem::Cancel.id() => false,
            DialogMessage::Command { id } => {
                match INSERTS.iter().find(|(button, ..)| button.id() == id) {
                    Some(&(_, target, Some(placeholder), _)) => {
                        Self::insert(dialog, target, placeholder);
                    }
                    Some(&(_, target, None, _)) => self.pick_range(dialog, target),
                    // A check box.
                    None => self.changed(dialog),
                }
                true
            }
            DialogMessage::Edited { id } if id == C::MultiRenameSaved.id() => {
                self.select_saved(dialog);
                true
            }
            // The list is still open here, so the dialog that its item opens
            // waits until it has closed.
            DialogMessage::Chosen { id }
                if id == C::MultiRenameSaved.id()
                    && DropDownList::of_dialog(dialog, id).is_some_and(DropDownList::is_open) =>
            {
                dialog.set_timer(SAVED_ACTION, 1);
                true
            }
            DialogMessage::Edited { id } if id == C::MultiRenameList.id() => false,
            DialogMessage::Edited { .. } => {
                self.changed(dialog);
                true
            }
            DialogMessage::Key { target, keystroke } => self.key(dialog, target, keystroke),
            DialogMessage::ColumnClicked { id, column } if id == C::MultiRenameList.id() => {
                self.sort(column);
                self.changed(dialog);
                true
            }
            DialogMessage::ContextMenu { target, at }
                if self.list.get().is_some_and(|list| list.hwnd() == target) =>
            {
                self.list_menu(dialog, at);
                true
            }
            DialogMessage::Timer { id: UPDATE } => {
                self.update(dialog);
                true
            }
            DialogMessage::Timer { id: POLL } => {
                self.poll(dialog);
                true
            }
            DialogMessage::Timer { id: SAVED_ACTION } => {
                self.run_saved(dialog);
                true
            }
            DialogMessage::Reply => {
                self.deliver(dialog);
                true
            }
            _ => false,
        }
    }

    fn list_cell(&self, id: u16, row: u64, column: usize) -> Option<String> {
        if id != C::MultiRenameList.id() {
            return None;
        }
        let state = self.state.borrow();
        let index = usize::try_from(row).ok()?;
        let entry = state.rows.get(index)?;
        // While new names are computed, the list shows the earlier ones.
        let outcome = state.outcomes.get(index);
        Some(match self.columns().get(column)?.0 {
            Column::Name => entry.name.clone(),
            Column::NewName => outcome?.name(entry).to_owned(),
            Column::Note => self.note(outcome?),
            Column::Size => match (entry.entry.is_folder, entry.entry.size) {
                (true, _) => self.strings.text("panel.folder").to_owned(),
                (false, Some(size)) => self.sizes.text(&self.strings, &self.numbers, size),
                (false, None) => String::new(),
            },
            Column::Modified => (entry.entry.modified)
                .map(|modified| self.numbers.date_time(modified))
                .unwrap_or_default(),
            Column::Folder => entry.folder(),
        })
    }

    fn buttons_sized(&self, dialog: Hwnd, width: Px) {
        // The extension's buttons start at its box, as the name's do.
        let place = |id: C| dialog::item(dialog, id.id()).map(|item| (item, item.rect_in(dialog)));
        if let (Some((_, first)), Some((_, second)), Some((_, label))) = (
            place(C::MultiRenameInsertName),
            place(C::MultiRenameNameRange),
            place(C::MultiRenameExtensionLabel),
        ) {
            let gap = second.x - first.x - width;
            let moves: Vec<(Hwnd, Rect)> = [
                C::MultiRenameInsertExtension,
                C::MultiRenameExtensionRange,
                C::MultiRenameExtensionCounter,
            ]
            .into_iter()
            .filter_map(place)
            .enumerate()
            .map(|(index, (button, rect))| {
                let x = label.x + Px((width + gap).0 * index as i32);
                (button, Rect { x, ..rect })
            })
            .collect();
            win::move_children(&moves);
        }
        let controls = [
            C::MultiRenameList,
            C::MultiRenameStatus,
            C::MultiRenameEdit,
            C::MultiRenameUndo,
        ]
        .into_iter()
        .map(C::id)
        .chain([DialogItem::Ok.id(), DialogItem::Cancel.id()])
        .filter_map(|id| dialog::item(dialog, id).map(|item| (id, item, item.rect_in(dialog))))
        .collect();
        let outer = dialog.screen_rect();
        let dpi = dialog.dpi();
        let _ = self.layout.set(Layout {
            dpi,
            client: dialog.client_size(),
            minimum: Size {
                width: outer.width,
                height: outer.height,
            },
            controls,
        });
        let saved = self.kept.borrow().size;
        if let Some(size) = saved {
            let width = size.width.to_px(dpi).max(outer.width);
            let height = size.height.to_px(dpi).max(outer.height);
            dialog.set_rect(Rect {
                x: outer.x - Px((width - outer.width).0 / 2),
                y: outer.y - Px((height - outer.height).0 / 2),
                width,
                height,
            });
        }
        self.resized(dialog);
    }

    fn minimum_size(&self) -> Option<Size> {
        let layout = self.layout.get()?;
        Some(Size {
            width: layout.minimum.width.rescale(layout.dpi, self.dpi.get()),
            height: layout.minimum.height.rescale(layout.dpi, self.dpi.get()),
        })
    }

    fn resized(&self, dialog: Hwnd) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let dpi = dialog.dpi();
        self.dpi.set(dpi);
        let scale = |px: Px| px.rescale(layout.dpi, dpi);
        let size = dialog.client_size();
        let dx = size.width - scale(layout.client.width);
        let dy = size.height - scale(layout.client.height);
        let moves: Vec<(Hwnd, Rect)> = (layout.controls.iter())
            .map(|&(id, control, rect)| {
                let mut rect = Rect {
                    x: scale(rect.x),
                    y: scale(rect.y),
                    width: scale(rect.width),
                    height: scale(rect.height),
                };
                if id == C::MultiRenameList.id() {
                    rect.width = rect.width + dx;
                    rect.height = rect.height + dy;
                } else if id == C::MultiRenameStatus.id() {
                    rect.width = rect.width + dx;
                    rect.y = rect.y + dy;
                } else {
                    rect.x = rect.x + dx;
                    rect.y = rect.y + dy;
                }
                (control, rect)
            })
            .collect();
        win::move_children(&moves);
        self.fit_columns();
    }

    fn closed(&self, dialog: Hwnd) {
        window::own_keys(dialog, false);
        dialog.kill_timer(UPDATE);
        dialog.kill_timer(POLL);
        dialog.kill_timer(SAVED_ACTION);
        let fields = Self::read_fields(dialog);
        self.state.borrow_mut().fields = fields;
        let rect = dialog.screen_rect();
        let dpi = dialog.dpi();
        self.kept.borrow_mut().size = Some(DialogSize {
            width: rect.width.to_dip(dpi),
            height: rect.height.to_dip(dpi),
        });
        if let Some(mut work) = self.work.borrow_mut().take() {
            work.delivery.close();
            work.names.cancel();
            work.dates.cancel();
            let mut retired = work.names.close_delivery();
            retired.append(work.dates.close_delivery());
            self.main.created().work.borrow_mut().retire(retired);
        }
    }
}
