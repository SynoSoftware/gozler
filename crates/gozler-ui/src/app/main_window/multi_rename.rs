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
use crate::core::glyph::Glyph;
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
use crate::win::controls::{
    Button, CheckBox, ComboBox, DropDownList, ListColumn, ListView, Tooltip, UpDown,
    set_accessible_name,
};
use crate::win::dialog::{self, DialogEnd, DialogMessage, DialogProc, Opened};
use crate::win::locale::WindowsNumbers;
use crate::win::menu::Menu;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use crate::win::{self, Hwnd, clipboard, window};
use asks::{EditNames, PickRange, RegularExpressions, ResultList, SaveName};
use gozler_client::CancelToken;
use gozler_fileops::{InUse, Listed, OperationId, Reason, Rename, Renamed, Request, State};
use jiff::Timestamp;
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
/// The timer that opens Edit names with a names file once the dialog is
/// free.
const LOADED: usize = 4;
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
        let opened = Timestamp::now();
        // Windows compares the names of environment variables without
        // regard to case.
        let variables = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .map(|(name, value)| (name.to_uppercase(), value))
            .collect();
        let state = MultiRename {
            main: self,
            window: hwnd,
            operations,
            numbers: self.numbers.borrow().clone(),
            strings,
            sizes,
            zone: gozler_winfs::local_time_zone(),
            known: known.map(Arc::new),
            opened,
            variables: Arc::new(variables),
            kept: RefCell::new(kept),
            state: RefCell::new(Shown::new(rows, opened)),
            work: RefCell::new(None),
            list: OnceCell::new(),
            layout: OnceCell::new(),
            dpi: Cell::new(hwnd.dpi()),
            status_width: Cell::new(Px(0)),
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
    /// The main window, which owns the clipboard that Result list copies to.
    window: Hwnd,
    operations: &'a FileOperations,
    strings: Strings,
    numbers: WindowsNumbers,
    sizes: SizeFormat,
    zone: TimeZone,
    known: Option<Arc<Known>>,
    /// When the dialog opened, which `[T2]` reads.
    opened: Timestamp,
    /// The environment variables, read once when the dialog opens, by
    /// their names in uppercase, which `[%x%]` reads.
    variables: Arc<HashMap<String, String>>,
    /// The saved renames, the histories and the size, which the window
    /// saves when the dialog closes.
    kept: RefCell<MultiRenameState>,
    state: RefCell<Shown>,
    work: RefCell<Option<Work>>,
    list: OnceCell<ListView>,
    layout: OnceCell<Layout>,
    dpi: Cell<Dpi>,
    /// The status text's width in the template, which the dialog keeps
    /// when the bottom row's buttons grow for a longer caption.
    status_width: Cell<Px>,
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
    /// When the fields last changed, which `[T3]` reads.
    changed: Timestamp,
    clipboard: ClipboardText,
    /// The step that the fields name: 1, and one more for each Next step.
    step: usize,
    /// Result list's lines: what the batch that ended last did.
    results: Vec<String>,
    /// Why Load names from file… could not read the chosen file, until a
    /// field changes.
    load_failed: Option<String>,
    /// The names file that Load names from file… read, with its path,
    /// until Edit names can open with it.
    loaded: Option<(PathBuf, std::io::Result<String>)>,
}

impl Shown {
    fn new(rows: Vec<Row>, opened: Timestamp) -> Shown {
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
            changed: opened,
            clipboard: ClipboardText::Unread,
            step: 1,
            results: Vec::new(),
            load_failed: None,
            loaded: None,
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

/// The clipboard's text, which a mask with `[X]` waits for; read again
/// after each change of a field.
enum ClipboardText {
    Unread,
    Reading,
    /// `None`: the clipboard holds no text.
    Read(Option<Arc<str>>),
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
    clipboard: Task<(), Option<String>>,
    file: Task<PathBuf, (PathBuf, std::io::Result<String>)>,
}

/// What the names are computed from.
struct Naming {
    rows: Arc<Vec<Row>>,
    /// `None` when the names come from Edit names.
    rule: Option<Rule>,
    known: Option<Arc<Known>>,
    zone: TimeZone,
    numbers: WindowsNumbers,
    opened: Timestamp,
    changed: Timestamp,
    variables: Arc<HashMap<String, String>>,
    clipboard: Option<Arc<str>>,
}

fn name_rows(naming: Naming, _cancel: &CancelToken) -> Vec<Outcome> {
    let source = match &naming.rule {
        Some(rule) => Source::Rule(rule),
        None => Source::Edited,
    };
    let context = Context {
        zone: &naming.zone,
        formats: &naming.numbers,
        opened: naming.opened,
        changed: naming.changed,
        variables: &naming.variables,
        clipboard: naming.clipboard.as_deref(),
    };
    multi_rename::preview(&naming.rows, &source, naming.known.as_deref(), &context)
}

fn read_dates(paths: Vec<Vec<u16>>, _worker: &OffThread, cancel: &CancelToken) -> Vec<Option<i64>> {
    win::shell::dates_taken(&paths, || cancel.is_cancelled())
}

fn read_clipboard(_: (), worker: &OffThread, _cancel: &CancelToken) -> Option<String> {
    clipboard::text(worker)
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
    (Column::OldName, 22),
    (Column::Extension, 7),
    (Column::NewName, 25),
    (Column::Size, 10),
    (Column::Modified, 16),
    (Column::Location, 20),
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
const FIELDS: [C; 25] = [
    C::MultiRenameName,
    C::MultiRenameInsertName,
    C::MultiRenamePlus,
    C::MultiRenameInsertDate,
    C::MultiRenameNameRange,
    C::MultiRenameRanges,
    C::MultiRenameInsertTime,
    C::MultiRenameNameCounter,
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
    C::MultiRenameStartSpin,
    C::MultiRenameStep,
    C::MultiRenameStepSpin,
    C::MultiRenameDigits,
];

/// The symbol check boxes, each with the tooltip that is also its name for
/// screen readers.
const SYMBOLS: [(C, &str); 5] = [
    (C::MultiRenameMatchCase, "multi_rename.tip_match_case"),
    (C::MultiRenameFirstOnly, "multi_rename.tip_first_only"),
    (C::MultiRenameExtensionToo, "multi_rename.tip_extension_too"),
    (C::MultiRenameRegex, "multi_rename.tip_regex"),
    (C::MultiRenameWholeName, "multi_rename.tip_whole_name"),
];

/// The buttons that keep their template's width and place: Total
/// Commander's grid of placeholders, `?`, and the two icon buttons. The
/// dialog gives the bottom row's other buttons one width.
const FIXED: [C; 15] = [
    C::MultiRenameInsertName,
    C::MultiRenamePlus,
    C::MultiRenameInsertDate,
    C::MultiRenameNameRange,
    C::MultiRenameRanges,
    C::MultiRenameInsertTime,
    C::MultiRenameNameCounter,
    C::MultiRenameInsertExtension,
    C::MultiRenameExtensionRange,
    C::MultiRenameExtensionCounter,
    C::MultiRenameRegexHelp,
    C::MultiRenameNames,
    C::MultiRenameNextStep,
    C::MultiRenameStartSpin,
    C::MultiRenameStepSpin,
];

/// The Digits list's choices: 1 to 10, then the automatic `a`.
const DIGITS: usize = 10;

impl MultiRename<'_> {
    fn text(dialog: Hwnd, id: C) -> String {
        dialog::item(dialog, id.id()).map_or_else(String::new, Hwnd::text)
    }

    fn checked(dialog: Hwnd, id: C) -> bool {
        CheckBox::of_dialog(dialog, id.id()).is_some_and(CheckBox::is_checked)
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

    /// The Saved renames list: (Default), each saved rename in alphabetical
    /// order, as Total Commander lists them, Save… and Delete…. While no
    /// saved rename is chosen, the closed list shows its cue, "F2 Saved
    /// renames".
    fn show_saved(&self, dialog: Hwnd) {
        let Some(list) = DropDownList::of_dialog(dialog, C::MultiRenameSaved.id()) else {
            return;
        };
        let (items, selected) = {
            let mut kept = self.kept.borrow_mut();
            kept.saved
                .sort_by_cached_key(|saved| saved.name.to_lowercase());
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
                .map(|index| index + 1);
            (items, selected)
        };
        match selected {
            Some(selected) => list.set_items(&items, selected),
            None => {
                list.set_items(&items, 0);
                list.select_none();
            }
        }
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
            state.clipboard = ClipboardText::Unread;
            state.load_failed = None;
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
            if state.fields != fields {
                state.changed = Timestamp::now();
                state.fields = fields.clone();
            }
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
        // The program that owns the clipboard can make a read wait, so the
        // work owner reads it, and the names wait for it.
        let reads_clipboard = rule.as_ref().is_some_and(Rule::reads_clipboard);
        if reads_clipboard && matches!(self.state.borrow().clipboard, ClipboardText::Unread) {
            self.state.borrow_mut().clipboard = ClipboardText::Reading;
            if let Some(work) = self.work.borrow_mut().as_mut() {
                work.clipboard.replace(());
            }
        }
        let clipboard = match &self.state.borrow().clipboard {
            ClipboardText::Read(text) => Some(text.clone()),
            ClipboardText::Unread | ClipboardText::Reading => None,
        };
        let waits = (reads_dates && !matches!(self.state.borrow().dates, Dates::Read))
            || (reads_clipboard && clipboard.is_none());
        self.state.borrow_mut().names = Names::Updating;
        if !waits {
            let state = self.state.borrow();
            let naming = Naming {
                rows: Arc::clone(&state.rows),
                rule,
                known: self.known.clone(),
                zone: self.zone.clone(),
                numbers: self.numbers.clone(),
                opened: self.opened,
                changed: state.changed,
                variables: Arc::clone(&self.variables),
                clipboard: clipboard.flatten(),
            };
            drop(state);
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
        let (names, dates, clipboard, file, retired) = {
            let mut work = self.work.borrow_mut();
            let Some(work) = work.as_mut() else {
                return;
            };
            work.delivery.woken();
            let names = work.names.take();
            let dates = work.dates.take();
            let clipboard = work.clipboard.take();
            let file = work.file.take();
            let mut retired = work.names.take_retired();
            retired.append(work.dates.take_retired());
            retired.append(work.clipboard.take_retired());
            retired.append(work.file.take_retired());
            (names, dates, clipboard, file, retired)
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
        if let Some((_, text)) = clipboard {
            let reading = {
                let mut state = self.state.borrow_mut();
                let reading = matches!(state.clipboard, ClipboardText::Reading);
                if reading {
                    state.clipboard = ClipboardText::Read(text.map(Arc::from));
                }
                reading
            };
            if reading {
                self.update(dialog);
            }
        }
        self.show(dialog);
        // Edit names runs its own message loop, so it opens after the
        // other deliveries are taken.
        if let Some((_, loaded)) = file {
            self.state.borrow_mut().loaded = Some(loaded);
            self.open_loaded(dialog);
        }
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
        let (count, status, can_rename, running, edited, can_edit, results) = {
            let state = self.state.borrow();
            (
                state.rows.len() as u64,
                self.status(&state),
                state.can_rename(),
                state.batch.is_some(),
                state.edited,
                state.can_edit_list(),
                !state.results.is_empty(),
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
            (C::MultiRenameNames.id(), !running && count > 0 && can_edit),
            // The names Next step takes are the ones shown.
            (C::MultiRenameNextStep.id(), can_rename),
            (C::MultiRenameUndo.id(), undo),
            (C::MultiRenameResultList.id(), !running && results),
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
            let mut parts: Vec<String> = state.load_failed.iter().cloned().collect();
            parts.push(count(done, finished.done, &[]));
            if finished.failed > 0 {
                parts.push(count(failed, finished.failed, &[]));
            }
            return parts.join(" ");
        }
        if !state.current() {
            return self.strings.text("multi_rename.updating").to_owned();
        }
        let counts = Counts::of(&state.outcomes);
        let renamed = numbers.integer(counts.renamed as u64);
        let mut parts: Vec<String> = state.load_failed.iter().cloned().collect();
        if state.step > 1 {
            let step = numbers.integer(state.step as u64);
            parts.push(self.strings.format("multi_rename.step_status", &[&step]));
        }
        parts.push(count(
            "multi_rename.get_new_names",
            counts.rows,
            &[&renamed],
        ));
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

    /// Why the row of `outcome` keeps its name or failed, which New name
    /// shows in place of a name that cannot be used, where Total Commander
    /// shows `<Error!>`, and the row's tooltip gives for a failed rename.
    /// `None` for a row with nothing to say.
    fn reason(&self, outcome: &Outcome) -> Option<String> {
        let text = |key: &str| self.strings.text(key).to_owned();
        Some(match outcome {
            Outcome::New { .. } | Outcome::NoChange => return None,
            Outcome::Kept(Problem::NoDateTaken) => text("multi_rename.no_date_taken"),
            Outcome::Kept(Problem::NoDateModified) => text("multi_rename.no_date_modified"),
            Outcome::Kept(Problem::NoClipboardText) => text("multi_rename.no_clipboard_text"),
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
        })
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
                // Start! ends the steps; a failed row keeps its new name.
                for row in rows.iter_mut() {
                    row.step = None;
                }
                state.step = 1;
            }
            state.finished = Some(Finished {
                undo: batch.undo,
                done: renamed.len(),
                failed: listed.len(),
            });
            state.results = (renamed.iter())
                .map(|each| format!("{} → {}", each.from.display(), each.to.display()))
                .chain(listed.iter().filter_map(|listed| {
                    let Reason::Failed(error) = listed.reason else {
                        return None;
                    };
                    let reason = win::error_text(&std::io::Error::from_raw_os_error(error as i32));
                    let failed = self.strings.format("multi_rename.failed", &[&reason]);
                    Some(format!("{}: {failed}", listed.path.display()))
                }))
                .collect();
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

    /// Range: the person selects characters of the focused row's text that
    /// the placeholder starting with `letter` reads, and their range goes
    /// into the box `target`.
    fn pick_range(&self, dialog: Hwnd, target: C, letter: &'static str) {
        let text = {
            let state = self.state.borrow();
            let focused = self.list.get().and_then(|list| list.focused()).unwrap_or(0);
            let Some(row) = state.rows.get(focused as usize) else {
                return;
            };
            row.range_text(letter)
        };
        let pick = PickRange::new(text, letter);
        let captions = DialogCaptions::new(DialogId::PickRange, &self.strings);
        let ended = dialog::run_modal(dialog, DialogId::PickRange, &captions, &pick);
        if matches!(ended, Ok(DialogEnd::Ok))
            && let Some(range) = pick.range()
        {
            Self::insert(dialog, target, &range);
        }
    }

    /// Opens `items` below the button `button` and returns the chosen
    /// item's ID.
    fn menu_below(dialog: Hwnd, button: C, items: &[MenuNode]) -> Option<u16> {
        let button = dialog::item(dialog, button.id())?;
        let rect = button.screen_rect();
        Menu::track_popup(dialog, items, (rect.x.0, rect.bottom().0), &[], None)
    }

    /// `+`: a menu of the placeholders that have no button, which go into
    /// the name box; `[X]` only while the clipboard holds text.
    fn plus(&self, dialog: Hwnd) {
        let clipboard = clipboard::holds_text();
        let mut items = Vec::new();
        let mut placeholders = Vec::new();
        for group in multi_rename::MORE {
            let group: Vec<&(&str, &str)> = (group.iter())
                .filter(|(placeholder, _)| clipboard || *placeholder != "[X]")
                .collect();
            if group.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(MenuNode::Separator);
            }
            for &&(placeholder, key) in &group {
                placeholders.push(placeholder);
                items.push(MenuNode::Item {
                    id: placeholders.len() as u16,
                    text: format!("{}\t{placeholder}", self.strings.text(key)),
                    mark: Mark::None,
                    glyph: None,
                });
            }
        }
        if let Some(id) = Self::menu_below(dialog, C::MultiRenamePlus, &items)
            && let Some(placeholder) = placeholders.get(usize::from(id).wrapping_sub(1))
        {
            Self::insert(dialog, C::MultiRenameName, placeholder);
        }
    }

    /// `#`: a menu of the texts whose ranges no button inserts; the one
    /// chosen opens Range.
    fn ranges(&self, dialog: Hwnd) {
        let items: Vec<MenuNode> = (multi_rename::RANGES.iter().enumerate())
            .map(|(index, &(_, key))| MenuNode::Item {
                id: index as u16 + 1,
                text: self.strings.text(key).to_owned(),
                mark: Mark::None,
                glyph: None,
            })
            .collect();
        if let Some(id) = Self::menu_below(dialog, C::MultiRenameRanges, &items)
            && let Some(&(letter, _)) = multi_rename::RANGES.get(usize::from(id).wrapping_sub(1))
        {
            self.pick_range(dialog, C::MultiRenameName, letter);
        }
    }

    /// The names menu (`F10`): Load names from file… and Edit names….
    fn names_menu(&self, dialog: Hwnd) {
        let (load, edit) = (1, 2);
        let items = [
            MenuNode::Item {
                id: load,
                text: self.strings.text("multi_rename.load_names").to_owned(),
                mark: Mark::None,
                glyph: None,
            },
            MenuNode::Item {
                id: edit,
                text: self.strings.text("multi_rename.edit_names").to_owned(),
                mark: Mark::None,
                glyph: None,
            },
        ];
        match Self::menu_below(dialog, C::MultiRenameNames, &items) {
            Some(id) if id == load => self.load_names(dialog),
            Some(id) if id == edit => self.edit_names(dialog, None),
            _ => {}
        }
    }

    /// Load names from file…: the work owner reads the chosen file, and
    /// Edit names opens with its lines.
    fn load_names(&self, dialog: Hwnd) {
        let title = self.strings.text("multi_rename.load_title");
        let all = self.strings.text("multi_rename.all_files");
        let Some(path) = win::shell::pick_file(dialog, title, all, "*.*") else {
            return;
        };
        self.state.borrow_mut().load_failed = None;
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.file.replace(PathBuf::from(path));
        }
    }

    /// Opens Edit names with the file that Load names from file… read, once
    /// the dialog is free: a batch, a dialog open over this one, a menu or
    /// a drag runs its own loop, inside which Edit names must not open.
    /// Until then a timer tries again.
    fn open_loaded(&self, dialog: Hwnd) {
        let free =
            dialog.is_enabled() && !window::input_held() && self.state.borrow().batch.is_none();
        if !free {
            dialog.set_timer(LOADED, 100);
            return;
        }
        dialog.kill_timer(LOADED);
        let loaded = self.state.borrow_mut().loaded.take();
        if let Some((path, read)) = loaded {
            self.names_loaded(dialog, path, read);
        }
    }

    /// The file that Load names from file… read: its names go into Edit
    /// names, or the status line says why it could not be read.
    fn names_loaded(&self, dialog: Hwnd, path: PathBuf, read: std::io::Result<String>) {
        match read {
            Ok(text) => self.edit_names(dialog, Some(text)),
            Err(error) => {
                let failed = self.strings.format(
                    "multi_rename.load_failed",
                    &[
                        &path
                            .file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy(),
                        &win::error_text(&error),
                    ],
                );
                self.state.borrow_mut().load_failed = Some(failed);
                self.show(dialog);
            }
        }
    }

    /// Edit names: the person types or pastes the new names, which start as
    /// `text`, a file's lines, or as the names the list shows.
    fn edit_names(&self, dialog: Hwnd, text: Option<String>) {
        let (text, rows) = {
            let state = self.state.borrow();
            let text = text.unwrap_or_else(|| {
                let names: Vec<&str> = (state.rows.iter().zip(&state.outcomes))
                    .map(|(row, outcome)| outcome.name(row))
                    .collect();
                names.join("\r\n")
            });
            (text, state.rows.len())
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

    /// Next step (`F5`): the masks read the names shown, and the fields go
    /// back to their defaults for the next rule. Start! renames from the
    /// names on disk to the last step's names in one batch.
    fn next_step(&self, dialog: Hwnd) {
        {
            let mut state = self.state.borrow_mut();
            if !state.can_rename() {
                return;
            }
            let state = &mut *state;
            multi_rename::next_step(
                Arc::make_mut(&mut state.rows).as_mut_slice(),
                &state.outcomes,
            );
            state.step += 1;
        }
        self.apply(dialog, None, Fields::default());
    }

    /// `?`: the syntax of Search for and Replace with, with Regular
    /// expression on.
    fn regex_help(&self, dialog: Hwnd) {
        let help = RegularExpressions::new(self.strings.text("multi_rename.regex_text"));
        let captions = DialogCaptions::new(DialogId::RegularExpressions, &self.strings);
        let _ = dialog::run_modal(dialog, DialogId::RegularExpressions, &captions, &help);
    }

    /// Result list: what the batch that ended last did, one entry a line.
    fn result_list(&self, dialog: Hwnd) {
        let text = self.state.borrow().results.join("\r\n");
        let list = ResultList::new(text, self.main, self.window);
        let captions = DialogCaptions::new(DialogId::ResultList, &self.strings);
        let _ = dialog::run_modal(dialog, DialogId::ResultList, &captions, &list);
    }

    /// A drag of the row `row`: the row follows the mouse through the list,
    /// as `Alt+Up` and `Alt+Down` move it.
    fn drag_row(&self, dialog: Hwnd, row: u64) {
        let Some(&list) = self.list.get() else {
            return;
        };
        if !self.state.borrow().can_edit_list() {
            return;
        }
        let mut at = row as usize;
        let mut moved = false;
        list.follow_row_drag(|over| {
            let over = over as usize;
            if over != at && self.move_row(at, over) {
                at = over;
                moved = true;
            }
        });
        if moved {
            self.changed(dialog);
        }
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
        let Some(&(column, _)) = COLUMNS.get(index) else {
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
            list.show_sort(COLUMNS.len(), Some((index, !descending)));
        }
    }

    /// Moves the row at `from` to `to`, where it then shows focused and
    /// selected; `false` when the list cannot change now or `to` is past
    /// its end.
    fn move_row(&self, from: usize, to: usize) -> bool {
        let Some(list) = self.list.get() else {
            return false;
        };
        {
            let mut state = self.state.borrow_mut();
            let count = state.rows.len();
            if !state.can_edit_list() || from >= count || to >= count {
                return false;
            }
            let rows = Arc::make_mut(&mut state.rows);
            let row = rows.remove(from);
            rows.insert(to, row);
            if state.outcomes.len() == count {
                let outcome = state.outcomes.remove(from);
                state.outcomes.insert(to, outcome);
            }
            state.sorted = None;
            // The counter follows the list's order, so the names shown no
            // longer fit; a drag computes them again when it ends.
            state.names = Names::Updating;
        }
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.names.cancel();
        }
        list.show_sort(COLUMNS.len(), None);
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
                let Some(row) = self.list.get().and_then(|list| list.focused()) else {
                    return false;
                };
                let row = row as usize;
                let to = match key {
                    Key::Down => row + 1,
                    _ => row.wrapping_sub(1),
                };
                if self.move_row(row, to) {
                    self.changed(dialog);
                }
                true
            }
            Keystroke {
                key: Key::Function(5),
                ctrl: false,
                alt: false,
                shift: false,
            } => {
                self.next_step(dialog);
                true
            }
            Keystroke {
                key: Key::Function(10),
                ctrl: false,
                alt: false,
                shift: false,
            } => {
                // The key works when the names button does.
                if dialog::item(dialog, C::MultiRenameNames.id()).is_some_and(Hwnd::is_enabled) {
                    self.names_menu(dialog);
                }
                true
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
        let shares: i32 = COLUMNS.iter().map(|(_, share)| share).sum();
        list.set_column_widths(
            (COLUMNS.iter().enumerate())
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
            clipboard: Task::new(delivery.clone(), Replacement::Cancel, read_clipboard),
            file: Task::new(
                delivery.clone(),
                Replacement::Cancel,
                crate::app::work::read_names,
            ),
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
        if let Some(status) = dialog::item(dialog, C::MultiRenameStatus.id()) {
            self.status_width.set(status.rect_in(dialog).width);
        }
        // Windows' default range runs from 100 down to 0.
        for id in [C::MultiRenameStartSpin, C::MultiRenameStepSpin] {
            if let Some(spin) = UpDown::of_dialog(dialog, id.id()) {
                spin.set_range(0, i32::MAX);
            }
        }
        Self::show_fields(dialog, &fields);
        if let Some(saved) = DropDownList::of_dialog(dialog, C::MultiRenameSaved.id()) {
            saved.set_cue(self.strings.text("multi_rename.saved"));
        }
        self.show_saved(dialog);
        // The icon buttons show only their icon, as Total Commander's do;
        // the caption is their name for screen readers.
        for (id, glyph) in [
            (C::MultiRenameNames, Glyph::Edit),
            (C::MultiRenameNextStep, Glyph::Redo),
        ] {
            if let Some(button) = Button::of_dialog(dialog, id.id()) {
                button.set_lone_icon(Some(glyph));
            }
        }
        for (id, tip) in SYMBOLS {
            if let Some(control) = dialog::item(dialog, id.id()) {
                set_accessible_name(control, Some(self.strings.text(tip)));
            }
        }
        let tips: Vec<(C, &str)> = (INSERTS.iter())
            .map(|&(button, _, _, tip)| (button, tip))
            .chain(SYMBOLS)
            .chain([
                (C::MultiRenamePlus, "multi_rename.tip_plus"),
                (C::MultiRenameRanges, "multi_rename.tip_ranges"),
                (C::MultiRenameRegexHelp, "multi_rename.tip_regex_help"),
                (C::MultiRenameNames, "multi_rename.tip_names"),
                (C::MultiRenameNextStep, "multi_rename.tip_next_step"),
                (C::MultiRenameDigits, "multi_rename.tip_digits"),
            ])
            .collect();
        let controls: Vec<Hwnd> = (tips.iter())
            .filter_map(|(id, _)| dialog::item(dialog, id.id()))
            .collect();
        if let Ok(tooltip) = Tooltip::create(dialog, &controls) {
            for (id, tip) in tips {
                if let Some(control) = dialog::item(dialog, id.id()) {
                    tooltip.set(control, self.strings.text(tip));
                }
            }
        }
        if let Some(list) = ListView::of_dialog(dialog, C::MultiRenameList.id()) {
            list.show_info_tips();
            for (index, (column, _)) in COLUMNS.iter().enumerate() {
                let (header, right_aligned) = match column {
                    Column::OldName => ("multi_rename.old_name", false),
                    Column::Extension => ("multi_rename.extension_header", false),
                    Column::NewName => ("multi_rename.new_name", false),
                    Column::Size => ("header.size", true),
                    Column::Modified => ("multi_rename.date_header", false),
                    Column::Location => ("multi_rename.location", false),
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
            DialogMessage::Command { id } if id == C::MultiRenameNames.id() => {
                self.names_menu(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenamePlus.id() => {
                self.plus(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameRanges.id() => {
                self.ranges(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameRegexHelp.id() => {
                self.regex_help(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameNextStep.id() => {
                self.next_step(dialog);
                true
            }
            DialogMessage::Command { id } if id == C::MultiRenameResultList.id() => {
                self.result_list(dialog);
                true
            }
            // Close ends the dialog; a batch that runs goes on.
            DialogMessage::Command { id } if id == DialogItem::Cancel.id() => false,
            DialogMessage::Command { id } => {
                match INSERTS.iter().find(|(button, ..)| button.id() == id) {
                    Some(&(_, target, Some(placeholder), _)) => {
                        Self::insert(dialog, target, placeholder);
                    }
                    Some(&(_, target, None, _)) => {
                        let letter = match target {
                            C::MultiRenameName => "N",
                            _ => "E",
                        };
                        self.pick_range(dialog, target, letter);
                    }
                    // A check box.
                    None => self.changed(dialog),
                }
                true
            }
            DialogMessage::RowDragged { id, row } if id == C::MultiRenameList.id() => {
                self.drag_row(dialog, row);
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
            DialogMessage::Timer { id: LOADED } => {
                self.open_loaded(dialog);
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
        Some(match COLUMNS.get(column)?.0 {
            Column::OldName => entry.name.clone(),
            Column::Extension => entry.extension().to_owned(),
            Column::NewName => match outcome? {
                kept @ Outcome::Kept(_) => self.reason(kept)?,
                outcome => outcome.name(entry).to_owned(),
            },
            Column::Size => match (entry.entry.is_folder, entry.entry.size) {
                (true, _) => self.strings.text("panel.folder").to_owned(),
                (false, Some(size)) => self.sizes.text(&self.strings, &self.numbers, size),
                (false, None) => String::new(),
            },
            Column::Modified => (entry.entry.modified)
                .map(|modified| self.numbers.date_time(modified))
                .unwrap_or_default(),
            Column::Location => entry.folder(),
        })
    }

    fn info_tip(&self, id: u16, row: usize) -> Option<String> {
        if id != C::MultiRenameList.id() {
            return None;
        }
        match self.state.borrow().outcomes.get(row)? {
            failed @ Outcome::Failed(_) => self.reason(failed),
            _ => None,
        }
    }

    fn keeps_width(&self, id: u16) -> bool {
        FIXED.iter().any(|control| control.id() == id)
    }

    fn buttons_sized(&self, dialog: Hwnd, _width: Px) {
        // The next step button sits left of Start!, which size_buttons
        // moved; the status text ends at it.
        let place = |id: u16| dialog::item(dialog, id).map(|item| (item, item.rect_in(dialog)));
        let mut taken = Px(0);
        if let (Some((next, rect)), Some((_, ok)), Some((status, text))) = (
            place(C::MultiRenameNextStep.id()),
            place(DialogItem::Ok.id()),
            place(C::MultiRenameStatus.id()),
        ) {
            let gap = Px((ok.x - text.right() - rect.width).0 / 2);
            let x = ok.x - gap - rect.width;
            let width = x - gap - text.x;
            // The dialog grows by what wider buttons took from the counts.
            taken = (self.status_width.get() - width).max(Px(0));
            win::move_children(&[(next, Rect { x, ..rect }), (status, Rect { width, ..text })]);
        }
        let controls = [
            C::MultiRenameList,
            C::MultiRenameStatus,
            C::MultiRenameNextStep,
            C::MultiRenameUndo,
            C::MultiRenameResultList,
        ]
        .into_iter()
        .map(C::id)
        .chain([DialogItem::Ok.id(), DialogItem::Cancel.id()])
        .filter_map(|id| dialog::item(dialog, id).map(|item| (id, item, item.rect_in(dialog))))
        .collect();
        let outer = dialog.screen_rect();
        let dpi = dialog.dpi();
        let minimum = Size {
            width: outer.width + taken,
            height: outer.height,
        };
        let _ = self.layout.set(Layout {
            dpi,
            client: dialog.client_size(),
            minimum,
            controls,
        });
        let saved = self.kept.borrow().size;
        let (width, height) = match saved {
            Some(size) => (size.width.to_px(dpi), size.height.to_px(dpi)),
            None => (outer.width, outer.height),
        };
        let (width, height) = (width.max(minimum.width), height.max(minimum.height));
        if (width, height) != (outer.width, outer.height) {
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
        for (id, _) in SYMBOLS {
            if let Some(control) = dialog::item(dialog, id.id()) {
                set_accessible_name(control, None);
            }
        }
        dialog.kill_timer(UPDATE);
        dialog.kill_timer(POLL);
        dialog.kill_timer(SAVED_ACTION);
        dialog.kill_timer(LOADED);
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
            work.clipboard.cancel();
            work.file.cancel();
            let mut retired = work.names.close_delivery();
            retired.append(work.dates.close_delivery());
            retired.append(work.clipboard.close_delivery());
            retired.append(work.file.close_delivery());
            self.main.created().work.borrow_mut().retire(retired);
        }
    }
}
