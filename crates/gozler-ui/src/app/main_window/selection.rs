//! Captures native selection in bounded turns, before commands or folder
//! candidates consume its indices. The control remains the selection owner.

use super::*;
use crate::core::commands::SelectKey;
use crate::core::panel::{Place, QuickAction, Row};
use crate::core::selection::changes::{Changes, Write};
use crate::core::selection::{
    CaptureKey, Capturing, Movement, RowsChange, marked, moved, passed, push_rows, row_count,
};
use crate::core::work::INLINE_ITEMS;
use std::collections::LinkedList;

#[derive(Default)]
pub(super) struct NativeSelection {
    capture: Capturing,
    reading: Option<NativeRead>,
    applying: bool,
    writing: bool,
    update: Option<NativeUpdate>,
    action: Option<SelectionAction>,
    matching: Option<MatchingSelection>,
    /// The last run of `Shift` with a cursor key ([`MainWindow::select_key`]).
    shift_run: Option<ShiftRun>,
    restore_focus: bool,
    unhighlight: Option<Unhighlight>,
    unhighlighting: bool,
    /// The rows that Clear selection cleared in Search's list, which
    /// `Num +` selects again while the list's key is still the one here.
    cleared: Option<(CaptureKey, Arc<Vec<Range<u64>>>)>,
}

struct MatchingSelection {
    since: Instant,
    phase: MatchingPhase,
}

enum MatchingPhase {
    Preparing {
        mask: gozler_core::query::NameMask,
        select: bool,
        folders: bool,
    },
    Running {
        id: Id,
        key: CaptureKey,
    },
}

#[derive(Clone, Copy)]
struct ShiftRun {
    /// The row where the run's last key left the focus.
    focus: u64,
    /// Whether the run selects its rows or clears them.
    mark: bool,
    /// The [`win::window::shift_presses`] that the run's keys came in.
    press: u32,
}

#[derive(Clone, Copy)]
struct Unhighlight {
    key: CaptureKey,
    after: Option<u64>,
    /// The row before which the walk over the selected rows stops.
    end: u64,
}

/// Who chose the place that [`MainWindow::show_native_place`] shows.
#[derive(Clone, Copy)]
pub(super) enum Chosen {
    /// A change of the rows: a panel's model holds the place already, and
    /// `keep_action` keeps a selection command that waits.
    Remap { keep_action: bool },
    /// A key of the person: the list tells its model when the writes end,
    /// as it tells a click.
    Key,
}

/// Where [`MainWindow::show_native_place`] leaves the list's scroll.
#[derive(Clone, Copy)]
pub(super) enum Scroll {
    /// Another folder, or a new order: the focused row scrolls into view.
    ToFocus,
    /// The same rows changed, which stay where the person sees them.
    Keep,
    /// The rows took a new order while the person looked at the focused
    /// entry: it stays `offset` rows below the top row, where it showed.
    FocusAt { offset: u64 },
}

struct NativeUpdate {
    key: CaptureKey,
    selected: RowSelection,
    shown: Option<Arc<Vec<Range<u64>>>>,
    changes: Option<Changes>,
    focused: Option<u64>,
    anchor: Option<u64>,
    scroll: Scroll,
    chosen: Chosen,
}

#[derive(Clone, Copy)]
pub(super) struct CommandContext {
    pub(super) mode: Mode,
    pub(super) list: List,
    pub(super) focus: Focus,
    key: CaptureKey,
}

#[derive(Clone, Copy)]
enum SelectionAction {
    Command(MenuItem, CommandContext),
    SearchMenu(Option<(i32, i32)>, CommandContext),
    PanelMenu(Side, Option<(i32, i32)>, CommandContext),
    /// A key of `run` over the rows `start..end`, which waits for the
    /// list's selection ([`MainWindow::mark_rows`]).
    Mark {
        start: u64,
        end: u64,
        run: ShiftRun,
        context: CommandContext,
    },
}

impl SelectionAction {
    fn context(self) -> CommandContext {
        match self {
            Self::Command(_, context)
            | Self::SearchMenu(_, context)
            | Self::PanelMenu(_, _, context)
            | Self::Mark { context, .. } => context,
        }
    }

    /// The action after a remap gave the list `key`. A run of `Shift` ends,
    /// because it names its rows by position, which the remap moved.
    fn remapped(self, key: CaptureKey) -> Option<Self> {
        let mut context = self.context();
        context.key = key;
        match self {
            Self::Command(item, _) => Some(Self::Command(item, context)),
            Self::SearchMenu(point, _) => Some(Self::SearchMenu(point, context)),
            Self::PanelMenu(side, point, _) => Some(Self::PanelMenu(side, point, context)),
            Self::Mark { .. } => None,
        }
    }
}

struct NativeRead {
    key: CaptureKey,
    count: u64,
    expected: u64,
    after: Option<u64>,
    chunks: LinkedList<Vec<Range<u64>>>,
    current: Vec<Range<u64>>,
    items: usize,
    read: u64,
}

impl NativeRead {
    fn push(&mut self, row: u64) {
        if self.current.len() == INLINE_ITEMS {
            self.chunks.push_back(std::mem::replace(
                &mut self.current,
                Vec::with_capacity(INLINE_ITEMS),
            ));
        }
        if push_rows(&mut self.current, row..row + 1) {
            self.items += 1;
        }
        self.after = Some(row);
        self.read += 1;
    }
}

impl MainWindow {
    pub(super) fn matching_selection_pending(&self, list: List) -> bool {
        self.native_selection(list).borrow().matching.is_some()
    }

    pub(super) fn selection_command_disabled(
        &self,
        list: List,
        item: MenuItem,
        focus: Focus,
    ) -> bool {
        (self.selection_command_pending(list) || self.matching_selection_pending(list))
            && (item.reads_selection(focus) || item.writes_selection(focus))
    }

    pub(super) fn matching_selection_slow(&self, list: List) -> bool {
        self.native_selection(list)
            .borrow()
            .matching
            .as_ref()
            .is_some_and(|matching| matching.since.elapsed() >= crate::core::work::SLOW)
    }

    pub(super) fn start_matching_selection(
        &self,
        side: Side,
        mask: gozler_core::query::NameMask,
        select: bool,
        folders: bool,
    ) {
        let list = List::Panel(side);
        self.native_selection(list).borrow_mut().matching = Some(MatchingSelection {
            since: Instant::now(),
            phase: MatchingPhase::Preparing {
                mask,
                select,
                folders,
            },
        });
        self.set_selection_enabled(list, false);
        self.continue_matching_selection(side);
        self.controls().key_bar.update_key_bar(self);
    }

    pub(super) fn continue_matching_selection(&self, side: Side) {
        let list = List::Panel(side);
        let preparing = self
            .native_selection(list)
            .borrow()
            .matching
            .as_ref()
            .is_some_and(|matching| matches!(matching.phase, MatchingPhase::Preparing { .. }));
        if !preparing
            || !self.read_target_selection(self.list_control(list).hwnd().parent(), list)
            || self.model.borrow().panels().panel(side).rows_pending()
        {
            return;
        }
        let key = self.native_key(list);
        let matching = self
            .native_selection(list)
            .borrow_mut()
            .matching
            .take_if(|matching| matches!(matching.phase, MatchingPhase::Preparing { .. }));
        let Some(MatchingSelection {
            since,
            phase:
                MatchingPhase::Preparing {
                    mask,
                    select,
                    folders,
                },
        }) = matching
        else {
            return;
        };
        let input = self
            .model
            .borrow()
            .panels()
            .select_matching(side, mask, select, folders);
        if let Some(input) = input {
            let id = self.created().work.borrow_mut().panels.select(side, input);
            self.native_selection(list).borrow_mut().matching = Some(MatchingSelection {
                since,
                phase: MatchingPhase::Running { id, key },
            });
        } else {
            self.set_selection_enabled(list, true);
        }
    }

    pub(super) fn matching_selection_current(&self, side: Side, id: Id) -> bool {
        let list = List::Panel(side);
        let key = self.native_key(list);
        let commander = self.model.borrow().mode() == Mode::Commander;
        self.native_selection(list)
            .borrow()
            .matching
            .as_ref()
            .is_some_and(|matching| {
                commander && matches!(matching.phase, MatchingPhase::Running { id: current, key: captured }
                    if current == id && captured == key)
            })
    }

    // Replacement transfers the gate to native application before calling
    // Windows. Explicit cancellation restores the existing selection instead.
    fn end_matching_selection(&self, list: List) -> bool {
        let matching = self.native_selection(list).borrow_mut().matching.take();
        if matching.is_none() {
            return false;
        }
        if let List::Panel(side) = list {
            self.created()
                .work
                .borrow_mut()
                .panels
                .cancel_matching(side);
        }
        true
    }

    pub(super) fn cancel_matching_selection(&self, list: List) {
        let ended = self.end_matching_selection(list);
        if ended
            && !self.native_update_active(list)
            && !self.native_deselection_pending(list)
            && !self.selection_command_pending(list)
        {
            self.set_selection_enabled(list, true);
        }
        if ended {
            self.controls().key_bar.update_key_bar(self);
        }
    }

    pub(super) fn reject_matching_selection(
        &self,
        side: Side,
        id: Id,
        answer: crate::core::panel::Selected,
    ) {
        let list = List::Panel(side);
        let current = self.native_selection(list).borrow().matching.as_ref().is_some_and(|matching| {
            matches!(matching.phase, MatchingPhase::Running { id: current, .. } if current == id)
        });
        if current {
            self.cancel_matching_selection(list);
        }
        self.retire_selection(answer);
    }

    /// EnableWindow clears a disabled list's focus; only restore focus that
    /// this update displaced, while the person has not focused another control.
    fn set_selection_enabled(&self, list: List, enabled: bool) {
        let control = self.list_control(list).hwnd();
        if !enabled && Hwnd::focused() == Some(control) {
            self.native_selection(list).borrow_mut().restore_focus = true;
        }
        control.set_enabled(enabled);
        if enabled && control.is_enabled() {
            let restore =
                std::mem::take(&mut self.native_selection(list).borrow_mut().restore_focus);
            if restore
                && Hwnd::focused().is_none()
                && control.is_visible()
                && control.parent().is_active()
            {
                control.focus();
            }
        }
    }

    fn schedule_native_selection(&self, hwnd: Hwnd) {
        if self.shutdown.borrow().is_none() && !self.selection_timer.replace(true) {
            // Posted continuations outrank input and paint in GetMessage. A
            // timer gives both a turn before the next bounded native slice.
            hwnd.set_timer(Timer::NativeSelection.id(), 1);
        }
    }

    pub(super) fn continue_native_selections(&self, hwnd: Hwnd) {
        hwnd.kill_timer(Timer::NativeSelection.id());
        self.selection_timer.set(false);
        if !self.update_live() {
            return;
        }
        for list in List::ALL {
            self.apply_native_place(list);
            if !self.update_live() {
                return;
            }
        }
        self.apply_taken_deselections(hwnd);
        for list in List::ALL {
            if !self.update_live() {
                return;
            }
            if self.native_selection(list).borrow().reading.is_some() {
                let count = self.list_control(list).count();
                let key = self.native_key(list);
                self.read_native_selection(list, count, key);
            }
        }
        if self.update_live() {
            self.controls().search.read_list_selection(self, hwnd);
        }
        if self.update_live() {
            self.controls().panels.read_panel_selection(self);
        }
        if self.update_live() {
            self.resume_selection_actions(hwnd);
        }
    }

    pub(super) fn command_context(&self) -> CommandContext {
        let (mode, list) = {
            let model = self.model.borrow();
            (model.mode(), model.active_list())
        };
        CommandContext {
            mode,
            list,
            focus: self.focus(),
            key: self.native_key(list),
        }
    }

    pub(super) fn selection_context_current(&self, context: CommandContext) -> bool {
        let mode = self.model.borrow().mode();
        context.mode == mode && context.key.same_list(self.native_key(context.list))
    }

    pub(super) fn read_target_selection(&self, hwnd: Hwnd, list: List) -> bool {
        match list {
            List::Search => self.controls().search.read_list_selection(self, hwnd),
            List::Panel(side) => self.controls().panels.read_selection(self, side, true),
        }
    }

    fn defer_selection_action(&self, action: SelectionAction) {
        let list = action.context().list;
        let accepted = {
            let mut native = self.native_selection(list).borrow_mut();
            if native.action.is_some() {
                false
            } else {
                native.action = Some(action);
                true
            }
        };
        if accepted {
            self.set_selection_enabled(list, false);
        }
    }

    pub(super) fn prepare_selection_command(
        &self,
        hwnd: Hwnd,
        item: MenuItem,
        context: CommandContext,
    ) -> bool {
        if self
            .native_selection(context.list)
            .borrow()
            .action
            .is_some()
        {
            return false;
        }
        if self.read_target_selection(hwnd, context.list) && !self.selection_waiting(context.list) {
            return true;
        }
        self.defer_selection_action(SelectionAction::Command(item, context));
        false
    }

    pub(super) fn resume_selection_actions(&self, hwnd: Hwnd) {
        for list in List::ALL {
            let action = self.native_selection(list).borrow().action;
            let Some(action) = action else {
                continue;
            };
            let context = action.context();
            let current = self.selection_context_current(context);
            if current {
                // A run of `Shift` needs the selected rows, not their files.
                let ready = match action {
                    SelectionAction::Mark { .. } => self.selected_rows(list).is_some(),
                    _ => self.read_target_selection(hwnd, list) && !self.selection_waiting(list),
                };
                if !ready {
                    continue;
                }
            }
            self.native_selection(list).borrow_mut().action = None;
            if !self.native_update_active(list)
                && !self.native_deselection_pending(list)
                && !self.matching_selection_pending(list)
            {
                self.set_selection_enabled(list, true);
            }
            if current {
                match action {
                    SelectionAction::Command(item, context) => {
                        self.run_command(hwnd, item, context)
                    }
                    SelectionAction::SearchMenu(point, context) => {
                        self.row_menu_ready(hwnd, point, context)
                    }
                    SelectionAction::PanelMenu(side, point, context) => self
                        .controls()
                        .panels
                        .panel_menu_ready(self, hwnd, side, point, context),
                    SelectionAction::Mark {
                        start, end, run, ..
                    } => self.mark_rows(list, start..end, run),
                }
            }
            if self.shutdown.borrow().is_some() {
                break;
            }
        }
    }

    pub(super) fn forget_selection_action(&self, list: List) {
        self.native_selection(list).borrow_mut().action = None;
    }

    pub(super) fn native_selection_waiting(&self, list: List) -> bool {
        let count = self.list_control(list).count();
        let native = self.native_selection(list).borrow();
        native.reading.is_some() || native.capture.selected(count).is_none()
    }

    fn selection_waiting(&self, list: List) -> bool {
        if self.native_deselection_pending(list) || self.matching_selection_pending(list) {
            return true;
        }
        let model = self.model.borrow();
        model.list(list).is_some_and(|shown| {
            shown.is_keeping()
                || matches!(
                    shown.selection(),
                    Selection::Reading { .. } | Selection::Filtering { .. }
                )
        }) || matches!(list, List::Panel(side) if model.panels().panel(side).is_calculating())
    }
    fn native_selection(&self, list: List) -> &RefCell<NativeSelection> {
        let index = List::ALL
            .iter()
            .position(|each| *each == list)
            .expect("ALL holds every list");
        &self.told[index]
    }

    fn retire_native_selection(&self, list: List) {
        let retired = self
            .native_selection(list)
            .borrow_mut()
            .capture
            .take_retired();
        self.created().work.borrow_mut().retire(retired);
    }

    fn native_key(&self, list: List) -> CaptureKey {
        let count = self.list_control(list).count();
        self.native_selection(list).borrow().capture.key(count)
    }

    pub(super) fn selection_changed(&self, list: List, change: RowsChange) -> bool {
        let mut native = self.native_selection(list).borrow_mut();
        if native.applying {
            return false;
        }
        if !native.unhighlighting
            && let Some(pending) = native.unhighlight.as_mut()
        {
            pending.after = None;
        }
        native.capture.changed(change);
        drop(native);
        self.cancel_matching_selection(list);
        self.retire_native_selection(list);
        true
    }

    pub(super) fn gate_changed_selections(&self) {
        let mut model = self.model.borrow_mut();
        for side in Side::BOTH {
            if self.panel(side).changed.get() == panels::ListChanged::Selection {
                model.panels_mut().native_selection_pending(side, true);
            }
        }
    }

    pub(super) fn accept_told_selections(&self, hwnd: Hwnd) {
        let answers = self.created().work.borrow_mut().lists.take_told();
        for (list, answer) in answers {
            self.native_selection(list)
                .borrow_mut()
                .capture
                .accept(answer);
            self.retire_native_selection(list);
            hwnd.post_deferred();
        }
    }

    pub(super) fn seed_native_selection(&self, list: List, selected: RowSelection) {
        self.end_matching_selection(list);
        let count = self.list_control(list).count();
        let mut retired = Retirement::default();
        let input = {
            let mut native = self.native_selection(list).borrow_mut();
            retired.push(native.reading.take(), INLINE_ITEMS + 1);
            retired.push(native.update.take(), INLINE_ITEMS + 1);
            native.applying = false;
            native.action = None;
            native.unhighlight = None;
            native.capture.seed(count, selected)
        };
        self.created().work.borrow_mut().retire(retired);
        self.created().work.borrow_mut().lists.tell(list, input);
        self.retire_native_selection(list);
        if let List::Panel(side) = list {
            self.model
                .borrow_mut()
                .panels_mut()
                .native_selection_pending(side, false);
            self.panel(side).changed.set(panels::ListChanged::Nothing);
        } else {
            self.controls().search.selection_pending.set(false);
        }
        self.set_selection_enabled(list, true);
    }

    pub(super) fn selected_rows(&self, list: List) -> Option<RowSelection> {
        let waiting = {
            let native = self.native_selection(list).borrow();
            native.applying || native.reading.is_some()
        };
        if waiting {
            return None;
        }
        let control = self.list_control(list);
        let hwnd = control.hwnd().parent();
        for _ in 0..2 {
            let count = control.count();
            let key = self.native_selection(list).borrow().capture.key(count);
            let selected = self.native_selection(list).borrow().capture.selected(count);
            if let Some(selected) = selected {
                let first = control.next_selected(None);
                let confirmed = selected.count == control.selected_count()
                    && selected.positions.first().map(|range| range.start) == first
                    && selected
                        .positions
                        .last()
                        .is_none_or(|range| control.row_state(range.end - 1).1);
                if key != self.native_key(list) {
                    break;
                }
                if confirmed {
                    return Some(selected);
                }
                self.native_selection(list)
                    .borrow_mut()
                    .capture
                    .invalidate();
            }
            let input = self.native_selection(list).borrow_mut().capture.take(count);
            if let Some(input) = input {
                self.created().work.borrow_mut().lists.tell(list, input);
                self.accept_told_selections(hwnd);
                continue;
            }
            if !self.native_selection(list).borrow().capture.needs_native() {
                break;
            }
            self.read_native_selection(list, count, key);
            let selected = self.native_selection(list).borrow().capture.selected(count);
            self.retire_native_selection(list);
            return selected;
        }
        self.retire_native_selection(list);
        None
    }

    fn read_native_selection(&self, list: List, count: u64, key: CaptureKey) {
        let control = self.list_control(list);
        let mut read = self.native_selection(list).borrow_mut().reading.take();
        if read.as_ref().is_some_and(|read| read.key != key) {
            self.retire_selection(read.take());
        }
        if read.is_none() {
            let expected = control.selected_count();
            let first = control.next_selected(None);
            if expected == 0
                || first.is_some_and(|first| first <= count && expected == count - first)
            {
                let positions = first
                    .filter(|_| expected != 0)
                    .into_iter()
                    .map(|first| first..count)
                    .collect();
                if key == self.native_key(list) {
                    let input = self
                        .native_selection(list)
                        .borrow_mut()
                        .capture
                        .known(count, RowSelection::new(positions, expected));
                    self.created().work.borrow_mut().lists.tell(list, input);
                    self.accept_told_selections(control.hwnd().parent());
                }
                return;
            }
            read = Some(NativeRead {
                key,
                count,
                expected,
                after: None,
                chunks: LinkedList::new(),
                current: Vec::with_capacity(INLINE_ITEMS),
                items: 0,
                read: 0,
            });
        }
        let mut read = read.expect("a scattered selection has a native cursor");
        let mut finished = false;
        for _ in 0..INLINE_ITEMS {
            let next = control.next_selected(read.after);
            if self.native_key(list) != key {
                break;
            }
            match next {
                Some(row) if row < read.count => read.push(row),
                _ => {
                    finished = true;
                    break;
                }
            }
        }
        if finished && read.read == read.expected && control.selected_count() == read.expected {
            read.chunks.push_back(std::mem::take(&mut read.current));
            let input =
                self.native_selection(list)
                    .borrow_mut()
                    .capture
                    .read(key, read.chunks, read.items);
            if let Some(input) = input {
                self.created().work.borrow_mut().lists.tell(list, input);
                self.accept_told_selections(control.hwnd().parent());
            }
        } else if !finished && self.native_key(list) == key {
            self.native_selection(list).borrow_mut().reading = Some(read);
            self.schedule_native_selection(control.hwnd().parent());
        } else {
            self.retire_selection(read);
            self.schedule_native_selection(control.hwnd().parent());
        }
    }

    pub(super) fn selection_command_pending(&self, list: List) -> bool {
        self.native_selection(list).borrow().action.is_some()
    }

    pub(super) fn await_search_menu(
        &self,
        hwnd: Hwnd,
        point: Option<(i32, i32)>,
    ) -> Option<CommandContext> {
        let context = self.command_context();
        if self.selection_command_pending(context.list)
            || self.matching_selection_pending(context.list)
        {
            return None;
        }
        if self.read_target_selection(hwnd, context.list) && !self.selection_waiting(context.list) {
            return Some(context);
        }
        self.defer_selection_action(SelectionAction::SearchMenu(point, context));
        None
    }

    pub(super) fn await_panel_menu(
        &self,
        hwnd: Hwnd,
        side: Side,
        point: Option<(i32, i32)>,
    ) -> Option<CommandContext> {
        let mut context = self.command_context();
        context.list = List::Panel(side);
        context.key = self.native_key(context.list);
        if self.selection_command_pending(context.list)
            || self.matching_selection_pending(context.list)
        {
            return None;
        }
        if self.read_target_selection(hwnd, context.list) && !self.selection_waiting(context.list) {
            return Some(context);
        }
        self.defer_selection_action(SelectionAction::PanelMenu(side, point, context));
        None
    }

    pub(super) fn native_update_active(&self, list: List) -> bool {
        self.native_selection(list).borrow().applying
    }

    /// A cursor or selection key in `list`, Search's or a panel's, which
    /// both take the same way, as Total Commander does (DESIGN.md:
    /// Selecting entries). While a panel's quick search box is open, `Up`
    /// and `Down` go to its matches and Space types into it.
    pub(super) fn select_key(&self, hwnd: Hwnd, list: List, key: SelectKey) {
        // The list never sees the keys the window takes, so it does not
        // show its focus outline, which Windows hides until the keyboard is
        // used.
        hwnd.show_focus_cues();
        let control = self.list_control(list);
        if !control.hwnd().is_enabled() {
            return;
        }
        if let List::Panel(side) = list {
            let quick = match key {
                SelectKey::Move(Movement::Up) => Some(QuickAction::Step { down: false }),
                SelectKey::Move(Movement::Down) => Some(QuickAction::Step { down: true }),
                SelectKey::Toggle { down: false } => Some(QuickAction::Character(' ')),
                _ => None,
            };
            if self.controls().panels.quick_key(self, side, quick) == Some(true) {
                return;
            }
        }
        let focused = control.focused();
        let count = control.count();
        match key {
            SelectKey::Move(movement) => {
                control.set_focused(moved(focused, movement, count, control.whole_rows()));
            }
            SelectKey::Toggle { down } => {
                if let Some(row) = focused
                    && self.model.borrow().selectable(list, row)
                {
                    control.select(row, !control.row_state(row).1);
                }
                let next = focused.map_or(0, |row| row + 1);
                if down && next < count {
                    control.set_focused(Some(next));
                }
            }
            SelectKey::Mark(movement) => {
                let view = control.whole_rows();
                let Some((rows, to)) = passed(focused, movement, count, view.clone()) else {
                    control.set_focused(moved(focused, movement, count, view));
                    return;
                };
                // A run of these keys gives its rows the state opposite to
                // its first row's; it goes on while Shift stays down and the
                // focus is where the last one left it.
                let press = win::window::shift_presses();
                let run = self.native_selection(list).borrow().shift_run;
                let mark = match run {
                    Some(run) if run.press == press && focused == Some(run.focus) => run.mark,
                    _ => !control.row_state(rows.start).1,
                };
                let run = ShiftRun {
                    focus: to,
                    mark,
                    press,
                };
                self.native_selection(list).borrow_mut().shift_run = Some(run);
                // Each row written alone is a message and a notification,
                // so a longer run goes through the bounded writes.
                if rows.end - rows.start > INLINE_ITEMS as u64 {
                    control.set_focused(Some(to));
                    self.mark_rows(list, rows, run);
                    return;
                }
                for row in rows {
                    let selectable = self.model.borrow().selectable(list, row);
                    if selectable && control.row_state(row).1 != mark {
                        control.select(row, mark);
                    }
                }
                control.set_focused(Some(to));
            }
            SelectKey::Restore => {
                let cleared = self.native_selection(list).borrow_mut().cleared.take();
                if let Some((key, selected)) = cleared
                    && key == self.native_key(list)
                {
                    let before = Place {
                        focused,
                        selected: Arc::default(),
                    };
                    let place = Place { focused, selected };
                    self.show_native_place(list, &before, &place, Scroll::Keep, Chosen::Key);
                }
            }
        }
    }

    /// Gives the rows `rows` of `list` the state of `run`, `..` left out,
    /// through [`Self::show_native_place`]'s bounded writes, and then
    /// unhighlights the taken rows among them. While the list's selection
    /// is not known yet, the run waits for it as a command does.
    fn mark_rows(&self, list: List, rows: Range<u64>, run: ShiftRun) {
        let Some(selected) = self.selected_rows(list) else {
            let mut context = self.command_context();
            context.list = list;
            context.key = self.native_key(list);
            self.defer_selection_action(SelectionAction::Mark {
                start: rows.start,
                end: rows.end,
                run,
                context,
            });
            return;
        };
        let (up, taken) = {
            let model = self.model.borrow();
            let up = match list {
                List::Panel(side) => matches!(model.panels().row(side, 0), Some(Row::Up)),
                List::Search => false,
            };
            let taken = model.list(list).is_some() && !model.panels().taken().is_empty();
            (up, taken)
        };
        let rows = rows.start.max(u64::from(up))..rows.end;
        let focused = Some(run.focus);
        let place = Place {
            focused,
            selected: Arc::new(marked(&selected.positions, rows.clone(), run.mark)),
        };
        let before = Place {
            focused,
            selected: selected.positions,
        };
        self.show_native_place(list, &before, &place, Scroll::Keep, Chosen::Key);
        if run.mark && taken {
            self.request_taken_deselection(list, rows);
        }
    }

    /// Clear selection clears `list`'s selection. Search's list remembers
    /// it first, so `Num +` selects it again (DESIGN.md: Selecting entries).
    #[allow(
        clippy::single_range_in_vec_init,
        reason = "a selection is a list of row ranges"
    )]
    pub(super) fn clear_selection(&self, list: List) {
        let control = self.list_control(list);
        let count = control.count();
        let kept = match list {
            // More rows than `SELECTION_LIMIT` run the command before the
            // capture knows them, which for every row it need not.
            List::Search => (self.native_selection(list).borrow())
                .capture
                .selected(count)
                .map(|kept| kept.positions)
                .or_else(|| {
                    (count > 0 && control.selected_count() == count)
                        .then(|| Arc::new(vec![0..count]))
                }),
            List::Panel(_) => None,
        };
        control.deselect_all();
        // The key after the clear, so any later change forgets it.
        let key = self.native_key(list);
        self.native_selection(list).borrow_mut().cleared = kept.map(|kept| (key, kept));
    }

    pub(super) fn native_deselection_pending(&self, list: List) -> bool {
        let native = self.native_selection(list).borrow();
        native.unhighlight.is_some() || native.unhighlighting
    }

    /// Unhighlights, in bounded turns, the selected rows among `rows` whose
    /// entries a delete or a move took.
    pub(super) fn request_taken_deselection(&self, list: List, rows: Range<u64>) {
        let key = self.native_key(list);
        self.native_selection(list).borrow_mut().unhighlight = Some(Unhighlight {
            key,
            after: rows.start.checked_sub(1),
            end: rows.end,
        });
        if let List::Panel(side) = list {
            self.model
                .borrow_mut()
                .panels_mut()
                .native_selection_pending(side, true);
        }
        let control = self.list_control(list);
        self.set_selection_enabled(list, false);
        self.schedule_native_selection(control.hwnd().parent());
    }

    pub(super) fn apply_taken_deselections(&self, hwnd: Hwnd) {
        for list in List::ALL {
            if self.native_update_active(list) {
                continue;
            }
            let waiting = self.model.borrow().list(list).is_some_and(|shown| {
                shown.is_keeping() || matches!(shown.selection(), Selection::Filtering { .. })
            });
            if waiting {
                continue;
            }
            let pending = self.native_selection(list).borrow_mut().unhighlight.take();
            let Some(mut pending) = pending else {
                continue;
            };
            let current =
                || pending.key.same_list(self.native_key(list)) && self.shutdown.borrow().is_none();
            if !current() {
                continue;
            }
            self.native_selection(list).borrow_mut().unhighlighting = true;
            let control = self.list_control(list);
            let mut selected = Vec::with_capacity(INLINE_ITEMS);
            let mut finished = false;
            for _ in 0..INLINE_ITEMS {
                match control.next_selected(pending.after) {
                    Some(row) if row < pending.end && current() => {
                        selected.push(row..row + 1);
                        pending.after = Some(row);
                    }
                    _ => {
                        finished = true;
                        break;
                    }
                }
            }
            let rows = if current() {
                self.model.borrow().taken_rows(list, &selected)
            } else {
                Vec::new()
            };
            for row in rows {
                if !current() {
                    break;
                }
                control.deselect(row);
            }
            let current = current();
            let complete = {
                let mut native = self.native_selection(list).borrow_mut();
                native.unhighlighting = false;
                if current && !finished && native.unhighlight.is_none() {
                    native.unhighlight = Some(pending);
                }
                native.unhighlight.is_none()
            };
            if complete && current {
                if !self.selection_command_pending(list) && !self.matching_selection_pending(list) {
                    self.set_selection_enabled(list, true);
                }
                match list {
                    List::Search => self.controls().search.selection_pending.set(true),
                    List::Panel(side) => {
                        self.panel(side).changed.set(panels::ListChanged::Selection)
                    }
                }
            }
            if complete {
                hwnd.post_deferred();
            } else {
                self.schedule_native_selection(hwnd);
            }
        }
    }

    pub(super) fn retire_native_selections(&self, hwnd: Hwnd) {
        hwnd.kill_timer(Timer::NativeSelection.id());
        self.selection_timer.set(false);
        let mut retired = Retirement::default();
        for slot in &self.told {
            retired.push(std::mem::take(&mut *slot.borrow_mut()), INLINE_ITEMS + 1);
        }
        self.created().work.borrow_mut().retire(retired);
    }

    /// Shows `place` in `list`, which showed `before`, writing the rows
    /// that change in bounded slices. A panel's model holds a remap's
    /// `place` already; otherwise the list tells its model when the writes
    /// end, as a person's selection does.
    pub(super) fn show_native_place(
        &self,
        list: List,
        before: &Place,
        place: &Place,
        scroll: Scroll,
        chosen: Chosen,
    ) {
        self.end_matching_selection(list);
        let control = self.list_control(list);
        let (count, selected_count, anchor) = match (list, chosen) {
            (List::Panel(side), Chosen::Remap { .. }) => {
                let model = self.model.borrow();
                (
                    model.panels().panel(side).count(),
                    model.panels().selected_count(side),
                    model.panels().anchor(side),
                )
            }
            (List::Search, Chosen::Remap { keep_action: true }) => (
                control.count(),
                row_count(&place.selected),
                control.selection_mark(),
            ),
            _ => (control.count(), row_count(&place.selected), place.focused),
        };
        let mut retired = Retirement::default();
        {
            let mut native = self.native_selection(list).borrow_mut();
            retired.push(native.reading.take(), INLINE_ITEMS + 1);
            retired.push(native.update.take(), INLINE_ITEMS + 1);
            native.capture.reset();
            let shown =
                (!native.applying && native.unhighlight.is_none() && !native.unhighlighting)
                    .then(|| Arc::clone(&before.selected));
            native.unhighlight = None;
            if let Chosen::Remap { keep_action: true } = chosen {
                let key = native.capture.key(count);
                native.action = native.action.and_then(|action| action.remapped(key));
            } else {
                native.action = None;
            }
            native.applying = true;
            native.update = Some(NativeUpdate {
                key: native.capture.key(count),
                selected: RowSelection {
                    positions: Arc::clone(&place.selected),
                    count: selected_count,
                },
                shown,
                changes: None,
                focused: place.focused,
                anchor,
                scroll,
                chosen,
            });
        }
        if let List::Panel(side) = list {
            self.model
                .borrow_mut()
                .panels_mut()
                .native_selection_pending(side, true);
        }
        self.created().work.borrow_mut().retire(retired);
        if let (List::Panel(_), Chosen::Remap { .. }) = (list, chosen) {
            // The panel's rows may have changed with the place.
            control.set_count(count);
        }
        self.apply_native_place(list);
    }

    /// Drops `value`, which can hold a large selection, on a work thread,
    /// so the window never waits while its memory is freed.
    fn retire_selection<T: Send + 'static>(&self, value: T) {
        let mut retired = Retirement::default();
        retired.push(value, INLINE_ITEMS + 1);
        self.created().work.borrow_mut().retire(retired);
    }

    pub(super) fn apply_native_place(&self, list: List) {
        {
            let mut native = self.native_selection(list).borrow_mut();
            if native.writing {
                return;
            }
            native.writing = true;
        }
        self.write_native_place(list);
        let pending = {
            let mut native = self.native_selection(list).borrow_mut();
            native.writing = false;
            native.update.is_some()
        };
        if pending && self.shutdown.borrow().is_none() {
            self.schedule_native_selection(self.list_control(list).hwnd().parent());
        }
    }

    fn write_native_place(&self, list: List) {
        let update = self.native_selection(list).borrow_mut().update.take();
        let Some(mut update) = update else {
            return;
        };
        let key = update.key;
        let current = || self.native_key(list) == key && self.shutdown.borrow().is_none();
        let control = self.list_control(list);
        let mut remaining = INLINE_ITEMS;
        if update.changes.is_none() {
            let count = control.count();
            let selected_count = control.selected_count();
            update.changes = Some(Changes::new(
                update.shown.take(),
                &update.selected,
                count,
                selected_count,
            ));
        }
        let changes = update.changes.as_mut().expect("the update has a cursor");
        while remaining > 0 && current() {
            let Some(write) = changes.next(&mut remaining) else {
                break;
            };
            match write {
                Write::All(true) => control.select_all(),
                Write::All(false) => control.deselect_all(),
                Write::Row(row, selected) => control.select(row, selected),
            }
        }
        if !current() {
            self.retire_selection(update);
            return;
        }
        // Only writes left for a later slice disable the list against
        // clicks between the slices: disabling repaints the whole list, and
        // its scroll bars flash, also when nothing changed.
        if !changes.is_done() {
            self.native_selection(list).borrow_mut().update = Some(update);
            self.set_selection_enabled(list, false);
            return;
        }
        control.focus_in_place(update.focused);
        control.set_selection_mark(update.anchor);
        match (update.scroll, update.focused) {
            (Scroll::ToFocus, Some(row)) => control.ensure_visible(row),
            (Scroll::FocusAt { offset }, Some(row)) => control.show_row_at(row, offset),
            _ => {}
        }
        if !current() {
            self.retire_selection(update);
            return;
        }
        let focused = control.focused();
        let hwnd = control.hwnd().parent();
        match (list, update.chosen) {
            (List::Panel(side), Chosen::Remap { .. }) => {
                self.model
                    .borrow_mut()
                    .panels_mut()
                    .set_focused(side, focused);
                self.panel(side).changed.set(panels::ListChanged::Nothing);
            }
            // The model reads a key's place as it reads a click's, on a turn
            // of its own, because a read during the writes found the list
            // busy and left no turn waiting.
            (List::Panel(side), Chosen::Key) => {
                self.panel(side).changed.set(panels::ListChanged::Selection);
                hwnd.post_deferred();
            }
            (List::Search, _) => {
                self.controls().search.selection_pending.set(true);
                hwnd.post_deferred();
            }
        }
        let count = control.count();
        let input = {
            let mut native = self.native_selection(list).borrow_mut();
            native.applying = false;
            native.capture.known(count, update.selected.clone())
        };
        if let List::Panel(side) = list {
            self.model
                .borrow_mut()
                .panels_mut()
                .native_selection_pending(side, false);
        }
        self.created().work.borrow_mut().lists.tell(list, input);
        self.retire_native_selection(list);
        if !self.selection_command_pending(list)
            && !self.native_deselection_pending(list)
            && !self.matching_selection_pending(list)
        {
            self.set_selection_enabled(list, true);
        }
        self.retire_selection(update);
        control.redraw_visible();
        if let List::Panel(side) = list {
            self.rows_shown(list);
            self.controls().panels.show_panel(self, side);
        }
        self.controls().key_bar.update_key_bar(self);
        self.resume_selection_actions(hwnd);
        self.start_list_passes();
    }
}
