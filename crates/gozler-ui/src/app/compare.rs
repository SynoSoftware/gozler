//! The compare windows (DESIGN.md: Compare by content): each Compare by
//! content opens one, a top-level window that no window owns, which
//! compares two files on its worker and shows them in RichEdit controls.
//! `core::compare` and `core::compare_window` decide what shows, and a
//! [`Session`] holds each window's state; this module passes the session
//! what happens, runs the effects it returns, and gives the controls what
//! the worker of [`super::work::compare`] built.

use super::dialogs::ViewerFind;
pub(super) use super::work::compare::Source;
use super::work::compare::{Job, Look, Options, Outcome, Progress, Saved, retire};
use crate::core::captions::DialogCaptions;
use crate::core::clipboard::folder_of;
use crate::core::compare::rtf::{Rtf, Style};
use crate::core::compare::{self, Matching};
use crate::core::compare_window::session::{
    Answer, Caret, Effect, Event, Fresh, Request, RowOptions, SaveFile, SavedFile, Session, Settled,
};
use crate::core::compare_window::{
    CompareCommand, CompareKey, CompareSizes, DEFAULT_ZOOM, KeyFocus, MenuState, Note, compare_key,
    compare_layout, context_menu, differences_text, menu_bar, menu_states, note_text, title,
    zoomed,
};
use crate::core::ids::{ControlId, DialogId, IconId};
use crate::core::layout::{Px, Size, WHEEL_DELTA, image_button_width, status_last_part};
use crate::core::panel::Side;
use crate::core::settings::{CompareState, Placement};
use crate::core::text::{self, Strings};
use crate::core::viewer::Mode;
use crate::core::viewer::find::Query;
use crate::win::compare::{RichText, SIDE_SCROLLED, palette, rtf_font};
use crate::win::controls::{Button, CheckBox, Edit, ProgressBar, StatusBar};
use crate::win::dialog::{DialogEnd, choose, error_box, info_box, run_modal};
use crate::win::gdi::{Font, TextMeter};
use crate::win::locale::{WindowsNumbers, code_page_encoded};
use crate::win::menu::{Menu, replace_menu_bar};
use crate::win::notify::Command;
use crate::win::shell::{FileType, accept_files, pick_file, save_dialog};
use crate::win::viewer::caption_height;
use crate::win::wake::OpenWaker;
use crate::win::window::{self, Message, WindowProc};
use crate::win::{self, Hwnd};
use std::cell::{Cell, OnceCell, RefCell};
use std::os::windows::ffi::OsStrExt;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use windows::Win32::Foundation::LRESULT;

const WINDOW_CLASS: &str = "gozler.ui.compare";
/// Shows the window once its comparison has run this long, so identical
/// files that compare faster show only their message.
const SHOW_TIMER: usize = 1;
const SHOW_DELAY: u32 = 300;
const PROGRESS_TIMER: usize = 2;
/// Aligns the rows again once the person has stopped typing this long.
const REALIGN_TIMER: usize = 3;
const REALIGN_DELAY: u32 = 300;
/// The rows above a difference that going to it keeps in view.
const CONTEXT_ROWS: usize = 3;

/// The open compare windows, as the main window or the process of
/// `--compare` holds them, and what they remember in `ui.json`.
pub(super) struct CompareWindows {
    shared: Rc<Shared>,
    /// The window class is registered, which the first window does.
    registered: Cell<bool>,
}

struct Shared {
    open: RefCell<Vec<Rc<CompareWindow>>>,
    state: Cell<CompareState>,
    changed: Cell<bool>,
    query: RefCell<Option<Query>>,
    /// The process of `--compare`, which ends when its window closes.
    alone: bool,
    exiting: Cell<ExitWait>,
    /// Windows with changes gave Windows a reason to hold the sign-out,
    /// which they take back once no window has changes.
    holding: Cell<bool>,
    sign_out_question: Cell<SignOutQuestion>,
}

/// Whether Exit waits for the saves it asked for, and what resumes it once
/// they end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExitWait {
    Idle,
    /// The main window's next deferred message, which each window posts.
    Deferred,
    /// The caller of [`CompareWindows::take_exit_wait`], which waits itself.
    Caller,
}

/// What a compare window is for, which decides whether an outcome that
/// ends its first comparison shows only a message and closes it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// It only runs the comparison tool, and never shows.
    Tool,
    /// Its first comparison runs. Identical files, or no two files to
    /// compare, show only a message.
    First,
    /// A comparison or its failure showed in it, so it stays.
    Shown,
}

/// Where the windows' question about a held sign-out is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignOutQuestion {
    Idle,
    /// It waits for the message that asks it.
    Posted,
    Asking,
}

impl CompareWindows {
    pub(super) fn new(state: CompareState, alone: bool) -> CompareWindows {
        CompareWindows {
            shared: Rc::new(Shared {
                open: RefCell::new(Vec::new()),
                state: Cell::new(state),
                changed: Cell::new(false),
                query: RefCell::new(None),
                alone,
                exiting: Cell::new(ExitWait::Idle),
                holding: Cell::new(false),
                sign_out_question: Cell::new(SignOutQuestion::Idle),
            }),
            registered: Cell::new(false),
        }
    }

    /// Opens a compare window of the two files of `source`, hidden until
    /// it shows them. With `tool`, the comparison tool's command, the
    /// window only runs the tool on them and closes. Closing it activates
    /// `main`, which opened it, while `main` shows.
    pub(super) fn open(
        &self,
        source: Source,
        tool: Option<String>,
        strings: Strings,
        main: Option<Hwnd>,
    ) -> windows::core::Result<()> {
        if !self.registered.get() {
            window::register_class::<CompareWindow>(
                WINDOW_CLASS,
                IconId::Gozler.id(),
                window::Background::Window,
            )?;
            self.registered.set(true);
        }
        let state = self.shared.state.get();
        let comparison = Rc::new(CompareWindow::new(
            Rc::downgrade(&self.shared),
            main,
            self.placement(state.placement),
            strings,
            state,
        ));
        let hwnd = window::create_window(
            WINDOW_CLASS,
            Rc::clone(&comparison),
            comparison.placement.get(),
        )?;
        if let Err(error) = comparison.start(hwnd, source, tool) {
            hwnd.destroy();
            return Err(error);
        }
        self.shared.open.borrow_mut().push(comparison);
        Ok(())
    }

    /// The state that `ui.json` saves.
    pub(super) fn state(&self) -> CompareState {
        self.shared.state.get()
    }

    /// Takes the saved-state transfer posted by a window's change.
    pub(super) fn take_state(&self) -> Option<CompareState> {
        self.shared.changed.replace(false).then(|| self.state())
    }

    /// Asks, for each window with changes, whether to save them, as Exit
    /// does before it closes the windows. `true` when Exit can go on now;
    /// `false` when the person cancelled, or while the saves run, after
    /// which [`Self::take_exit`] says to exit again.
    pub(super) fn settle_for_exit(&self) -> bool {
        self.shared.settle_for_exit()
    }

    /// Whether the saves that Exit waited for have ended, so Exit goes on.
    pub(super) fn take_exit(&self) -> bool {
        if self.shared.exiting.get() != ExitWait::Deferred || self.saving() {
            return false;
        }
        self.shared.exiting.set(ExitWait::Idle);
        true
    }

    /// Whether Exit waits for the saves it asked for; a Cancel or a failed
    /// save ends the wait.
    pub(super) fn exit_waits(&self) -> bool {
        self.shared.exiting.get() != ExitWait::Idle
    }

    /// Whether Exit waits for saves, which then no longer resume it: the
    /// caller waits for them itself.
    pub(super) fn take_exit_wait(&self) -> bool {
        let waits = self.exit_waits();
        if waits {
            self.shared.exiting.set(ExitWait::Caller);
        }
        waits
    }

    /// Whether a window saves.
    pub(super) fn saving(&self) -> bool {
        self.opened().iter().any(|each| each.saving())
    }

    /// Closes every window, as a sign-out does; each leaves
    /// its state first.
    pub(super) fn close_all(&self) {
        let open = std::mem::take(&mut *self.shared.open.borrow_mut());
        for comparison in open.into_iter().rev() {
            if let Some(hwnd) = comparison.hwnd.get() {
                hwnd.destroy();
            }
        }
    }

    /// Words every window in the language of `strings`.
    pub(super) fn set_strings(&self, strings: &Strings) {
        for comparison in self.opened() {
            comparison.strings.replace(strings.clone());
            if let Some(hwnd) = comparison.hwnd.get() {
                comparison.apply_strings(hwnd);
            }
        }
    }

    /// Shows or hides the icons of every window's menus.
    pub(super) fn apply_glyphs(&self) {
        for comparison in self.opened() {
            if let Some(hwnd) = comparison.hwnd.get() {
                comparison.apply_menus(hwnd);
            }
        }
    }

    /// Where a new window opens, as a new Viewer does: at `saved`, or below
    /// and to the right of the last one opened by its caption's height.
    fn placement(&self, saved: Option<Placement>) -> Option<Placement> {
        let last = self.opened().last().and_then(|each| each.hwnd.get());
        let Some((last, hwnd)) = last.and_then(|hwnd| Some((hwnd.placement()?, hwnd))) else {
            return saved;
        };
        let size = saved.unwrap_or(last);
        let step = caption_height(hwnd.dpi()).0;
        let (left, top) = (last.left + step, last.top + step);
        Some(Placement {
            left,
            top,
            right: left + (size.right - size.left),
            bottom: top + (size.bottom - size.top),
            maximized: false,
        })
    }

    fn opened(&self) -> Vec<Rc<CompareWindow>> {
        self.shared.opened()
    }
}

impl Shared {
    fn opened(&self) -> Vec<Rc<CompareWindow>> {
        self.open.borrow().clone()
    }

    /// [`CompareWindows::settle_for_exit`]: each window with changes answers
    /// in turn, and a Cancel in one stops Exit in all. The windows take
    /// typing while a question is open, so they are asked again until none
    /// has changes, and Exit never closes a window with changes.
    fn settle_for_exit(&self) -> bool {
        loop {
            let (mut asked, mut saving) = (false, false);
            for comparison in self.opened() {
                let Some(hwnd) = comparison.hwnd.get() else {
                    continue;
                };
                if !comparison.has_changes() {
                    continue;
                }
                asked = true;
                hwnd.bring_to_front();
                match comparison.settle(hwnd, Event::Exit) {
                    Settled::Cancelled => {
                        self.stop_exit();
                        return false;
                    }
                    // The open question's answer decides, also for an
                    // earlier Exit that asked it and goes on after it.
                    Settled::Asking => return false,
                    Settled::Saving => saving = true,
                    Settled::Done => {}
                }
            }
            if saving {
                self.exiting.set(ExitWait::Deferred);
                return false;
            }
            if !asked {
                self.exiting.set(ExitWait::Idle);
                return true;
            }
        }
    }

    /// Wakes `main` while Exit waits for it to go on, after a window's
    /// saves ended or a window closed.
    fn resume_exit(&self, main: Option<Hwnd>) {
        if self.exiting.get() == ExitWait::Deferred
            && let Some(main) = main
        {
            main.post_deferred();
        }
    }

    /// Exit no longer goes on, and no window waits to close for it.
    fn stop_exit(&self) {
        self.exiting.set(ExitWait::Idle);
        for comparison in self.opened() {
            if let Some(hwnd) = comparison.hwnd.get() {
                comparison.send(hwnd, Event::ExitCancelled);
            }
        }
    }

    /// Holds the sign-out while a window has changes, as Notepad
    /// does: Windows names each such window, and each asks Save's question
    /// once this message has returned. Whether to hold it.
    fn hold_sign_out(&self) -> bool {
        let changed: Vec<(Rc<CompareWindow>, Hwnd)> = (self.opened().into_iter())
            .filter_map(|comparison| {
                let hwnd = comparison.hwnd.get()?;
                comparison.has_changes().then_some((comparison, hwnd))
            })
            .collect();
        let Some((_, first)) = changed.first() else {
            return false;
        };
        // Every sign-out holds and asks again, because Windows
        // need not say that one it held went on.
        for (comparison, hwnd) in &changed {
            hwnd.block_shutdown(comparison.strings.borrow().text("compare.unsaved"));
        }
        self.holding.set(true);
        if self.sign_out_question.get() == SignOutQuestion::Idle {
            self.sign_out_question.set(SignOutQuestion::Posted);
            first.post_deferred();
        }
        true
    }

    /// The question of each window with changes, for a held sign-out; a
    /// window that the answer settles closes.
    fn settle_sign_out(&self) {
        if self.sign_out_question.get() != SignOutQuestion::Posted {
            return;
        }
        self.sign_out_question.set(SignOutQuestion::Asking);
        for comparison in self.opened() {
            let Some(hwnd) = comparison.hwnd.get() else {
                continue;
            };
            if !comparison.has_changes() {
                continue;
            }
            hwnd.bring_to_front();
            if matches!(
                comparison.settle(hwnd, Event::Close),
                Settled::Cancelled | Settled::Asking
            ) {
                break;
            }
        }
        self.sign_out_question.set(SignOutQuestion::Idle);
    }

    /// Lets the sign-out go on once no window has changes, or `always`,
    /// when Windows says that the sign-out stopped.
    fn release_sign_out(&self, always: bool) {
        if !self.holding.get() || (!always && self.opened().iter().any(|each| each.has_changes())) {
            return;
        }
        self.holding.set(false);
        if self.sign_out_question.get() == SignOutQuestion::Posted {
            self.sign_out_question.set(SignOutQuestion::Idle);
        }
        for comparison in self.opened() {
            if let Some(hwnd) = comparison.hwnd.get() {
                hwnd.allow_shutdown();
            }
        }
    }
}

fn name_of(path: &[u16]) -> String {
    text::name_of(&String::from_utf16_lossy(path)).to_owned()
}

/// Where the caret goes after the rows show again.
enum Place {
    FirstDifference,
    /// On `side`'s `line`, or its row while the line has none, at
    /// `column`, with the row as far from the top as it was.
    Line {
        side: Side,
        line: Option<u64>,
        row: usize,
        column: usize,
        first_visible: usize,
        x: i32,
    },
}

/// A window's controls, created in Tab order.
struct Controls {
    paths: [Edit; 2],
    browse: [Button; 2],
    compare: Button,
    /// Case sensitive, Ignore repeated spaces and Show only differences.
    checks: [CheckBox; 3],
    extra: Edit,
    cancel: Button,
    progress: ProgressBar,
    sides: [RichText; 2],
    numbers: [RichText; 2],
    list: RichText,
    status: StatusBar,
}

impl Controls {
    fn create(hwnd: Hwnd) -> windows::core::Result<Controls> {
        let left_path = Edit::create(hwnd, ControlId::CompareLeftPath.id())?;
        let left_browse = Button::create(hwnd, ControlId::CompareLeftBrowse.id())?;
        let right_path = Edit::create(hwnd, ControlId::CompareRightPath.id())?;
        let right_browse = Button::create(hwnd, ControlId::CompareRightBrowse.id())?;
        let compare = Button::create(hwnd, ControlId::CompareRun.id())?;
        let checks = [
            CheckBox::create(hwnd, ControlId::CompareCase.id())?,
            CheckBox::create(hwnd, ControlId::CompareSpaces.id())?,
            CheckBox::create(hwnd, ControlId::CompareOnly.id())?,
        ];
        let extra = Edit::create(hwnd, ControlId::CompareExtra.id())?;
        let cancel = Button::create(hwnd, ControlId::CompareCancel.id())?;
        let progress = ProgressBar::create(hwnd, ControlId::CompareProgress.id())?;
        let sides = [
            RichText::create_side(hwnd, ControlId::CompareLeft.id())?,
            RichText::create_side(hwnd, ControlId::CompareRight.id())?,
        ];
        let numbers = [
            RichText::create_numbers(hwnd, ControlId::CompareLeftNumbers.id(), sides[0])?,
            RichText::create_numbers(hwnd, ControlId::CompareRightNumbers.id(), sides[1])?,
        ];
        let list = RichText::create_list(hwnd, ControlId::CompareList.id())?;
        let status = StatusBar::create(hwnd, ControlId::CompareStatus.id(), true)?;
        Ok(Controls {
            paths: [left_path, right_path],
            browse: [left_browse, right_browse],
            compare,
            checks,
            extra,
            cancel,
            progress,
            sides,
            numbers,
            list,
            status,
        })
    }

    fn rich(&self) -> [RichText; 5] {
        [
            self.sides[0],
            self.sides[1],
            self.numbers[0],
            self.numbers[1],
            self.list,
        ]
    }

    /// The side that `hwnd` is.
    fn side_of(&self, hwnd: Hwnd) -> Option<Side> {
        Side::BOTH
            .into_iter()
            .find(|side| self.sides[side.index()].hwnd() == hwnd)
    }
}

struct Fonts {
    ui: Font,
    /// The width of a character and the height of a row of the sides'
    /// text at their zoom.
    char_width: Px,
    row_height: Px,
}

struct CompareWindow {
    hwnd: Cell<Option<Hwnd>>,
    shared: Weak<Shared>,
    main: Option<Hwnd>,
    placement: Cell<Option<Placement>>,
    strings: RefCell<Strings>,
    numbers: RefCell<WindowsNumbers>,
    controls: OnceCell<Controls>,
    fonts: RefCell<Option<Fonts>>,
    work: RefCell<Option<super::work::compare::Work>>,
    /// Wakes the window from its worker, which outlives it, until
    /// `WM_DESTROY` closes it.
    waker: OnceCell<OpenWaker>,
    progress: RefCell<Arc<Progress>>,
    state: Cell<CompareState>,
    /// The comparison that shows, the worker's job, the changes and the
    /// saves. No borrow of it is held while an effect runs, because
    /// effects run dialogs whose loops pass the window more events.
    session: RefCell<Session>,
    stage: Cell<Stage>,
    /// The panels' entries of the window's first comparison, which its
    /// worker chooses the files from, until the worker takes them.
    chosen: RefCell<Option<Source>>,
    /// Why no comparison shows.
    failure: RefCell<Option<String>>,
    /// The progress bar and Cancel show, as they do while a comparison
    /// runs.
    progress_shown: Cell<bool>,
    /// Where the last question about changes left the window.
    settled: Cell<Option<Settled>>,
    /// Each side's last scroll position, which tells which one moved.
    scroll: Cell<[(i32, i32); 2]>,
    /// The wheel's turn with Ctrl that is less than a notch.
    zoom_wheel: Cell<i32>,
    /// The control that had the focus when the window was deactivated.
    focused: Cell<Option<Hwnd>>,
    /// The side the person works in, which the commands act on.
    active: Cell<Side>,
}

impl CompareWindow {
    fn new(
        shared: Weak<Shared>,
        main: Option<Hwnd>,
        placement: Option<Placement>,
        strings: Strings,
        state: CompareState,
    ) -> CompareWindow {
        let session = Session::new(matching_of(state), only_differences_of(state));
        CompareWindow {
            hwnd: Cell::new(None),
            shared,
            main,
            placement: Cell::new(placement),
            strings: RefCell::new(strings),
            numbers: RefCell::new(WindowsNumbers::new()),
            controls: OnceCell::new(),
            fonts: RefCell::new(None),
            work: RefCell::new(None),
            waker: OnceCell::new(),
            progress: RefCell::new(Arc::new(Progress::default())),
            state: Cell::new(state),
            session: RefCell::new(session),
            stage: Cell::new(Stage::First),
            chosen: RefCell::new(None),
            failure: RefCell::new(None),
            progress_shown: Cell::new(false),
            settled: Cell::new(None),
            scroll: Cell::new([(0, 0); 2]),
            zoom_wheel: Cell::new(0),
            focused: Cell::new(None),
            active: Cell::new(Side::Left),
        }
    }

    /// Everything the new window needs before it compares: its keys, its
    /// controls, menus and texts, and the first job of its worker.
    fn start(&self, hwnd: Hwnd, source: Source, tool: Option<String>) -> windows::core::Result<()> {
        self.hwnd.set(Some(hwnd));
        window::own_keys(hwnd, true);
        let _ = self.controls.set(Controls::create(hwnd)?);
        accept_files(hwnd);
        let waker = OpenWaker::new(hwnd);
        self.work
            .replace(Some(super::work::compare::Work::new(waker.clone())));
        let _ = self.waker.set(waker);
        self.apply_fonts(hwnd);
        self.apply_state();
        self.apply_strings(hwnd);
        match tool {
            Some(command) => {
                self.stage.set(Stage::Tool);
                self.start_job(Job::Tool { source, command });
            }
            None => {
                let event = match source {
                    Source::Files(paths) => Event::Compare(paths),
                    chosen @ Source::Chosen { .. } => {
                        self.chosen.replace(Some(chosen));
                        Event::Choose
                    }
                };
                self.send(hwnd, event);
                hwnd.set_timer(SHOW_TIMER, SHOW_DELAY);
            }
        }
        Ok(())
    }

    fn controls(&self) -> Option<&Controls> {
        self.controls.get()
    }

    fn start_job(&self, job: Job) {
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.start(job);
        }
    }

    fn saving(&self) -> bool {
        self.session.borrow().saving()
    }

    fn has_changes(&self) -> bool {
        self.session.borrow().has_changes()
    }

    /// Passes `event` to the session and runs what follows.
    fn send(&self, hwnd: Hwnd, event: Event) {
        self.send_at(hwnd, event, None);
    }

    /// [`Self::send`], where rows that show again put the caret at `place`.
    fn send_at(&self, hwnd: Hwnd, event: Event, place: Option<Place>) {
        let effects = self.session.borrow_mut().on(event);
        self.run(hwnd, effects, place);
    }

    /// Runs the session's effects in order, until one closes the window.
    fn run(&self, hwnd: Hwnd, effects: Vec<Effect>, mut place: Option<Place>) {
        for effect in effects {
            if self.hwnd.get() != Some(hwnd) {
                return;
            }
            match effect {
                Effect::Compare(request) => self.start_compare(hwnd, request),
                Effect::Align(align) => self.start_job(Job::Align {
                    generation: align.generation,
                    input: align.input,
                    options: self.options(align.rows),
                }),
                Effect::Cancel => {
                    if let Some(work) = self.work.borrow_mut().as_mut() {
                        work.cancel();
                    }
                }
                Effect::Wait => hwnd.set_timer(REALIGN_TIMER, REALIGN_DELAY),
                Effect::Present(caret) => {
                    let place = match caret {
                        Caret::FirstDifference => Place::FirstDifference,
                        Caret::Kept => place.take().unwrap_or(Place::FirstDifference),
                    };
                    self.present(hwnd, place);
                }
                Effect::ReadPanes(fresh) => {
                    if let Some(c) = self.controls() {
                        let texts = c.sides.map(RichText::text);
                        self.send(hwnd, Event::Panes { fresh, texts });
                    }
                }
                Effect::Run(command, fresh) => self.run_command(hwnd, command, fresh),
                Effect::Swapped => self.swap_boxes(),
                Effect::Ask(sides) => {
                    let answer = self.ask(hwnd, &sides);
                    self.send(hwnd, Event::Answered(answer));
                }
                Effect::Save(files) => {
                    if let Some(work) = self.work.borrow_mut().as_mut() {
                        work.save(files);
                    }
                }
                Effect::Renamed(side) => {
                    let path = (self.session.borrow().showing())
                        .map(|showing| String::from_utf16_lossy(&showing.paths()[side.index()]));
                    if let (Some(path), Some(c)) = (path, self.controls()) {
                        c.paths[side.index()].hwnd().set_text(&path);
                    }
                    self.apply_title(hwnd);
                }
                Effect::OnlyDifferences(only) => {
                    self.save_state(|state| {
                        state.only_differences = only.is_some();
                        if let Some(lines) = only {
                            state.extra_lines = lines;
                        }
                    });
                    // The extra lines box tells of its change, which then
                    // matches the state.
                    if let Some(c) = self.controls() {
                        let state = self.state.get();
                        c.checks[2].set_checked(state.only_differences);
                        c.extra.hwnd().set_text(&state.extra_lines.to_string());
                    }
                }
                Effect::Close => self.close_now(hwnd),
                Effect::Settled(settled) => self.settled.set(Some(settled)),
                Effect::ExitStopped => {
                    if let Some(shared) = self.shared.upgrade() {
                        shared.stop_exit();
                    }
                }
                Effect::Retire(retired) => retire(retired),
            }
        }
        self.refresh(hwnd);
        if let Some(shared) = self.shared.upgrade() {
            shared.release_sign_out(false);
        }
    }

    /// The parts of the window that follow the session: the progress, the
    /// sides that take typing and the status bar.
    fn refresh(&self, hwnd: Hwnd) {
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let comparing = self.session.borrow().comparing();
        if self.progress_shown.replace(comparing) != comparing {
            if !comparing {
                hwnd.kill_timer(PROGRESS_TIMER);
            }
            self.layout(hwnd);
        }
        self.apply_read_only();
        self.show_status();
    }

    /// Asks Save's question for `event`, Close or Exit; where it left the
    /// window.
    fn settle(&self, hwnd: Hwnd, event: Event) -> Settled {
        self.settled.set(None);
        self.send(hwnd, event);
        // A window that closed ran its Close before it could say so.
        self.settled.take().unwrap_or(Settled::Done)
    }

    /// How the worker builds the rows of `rows`.
    fn options(&self, rows: RowOptions) -> Options {
        Options {
            matching: rows.matching,
            only_differences: rows.only_differences,
            look: Look {
                palette: palette(),
                font: rtf_font(),
                strings: self.strings.borrow().clone(),
                numbers: self.numbers.borrow().clone(),
            },
        }
    }

    /// Reads and compares the files of `request`, with the progress.
    fn start_compare(&self, hwnd: Hwnd, request: Request) {
        let source = match request.paths {
            Some(paths) => {
                // A comparison that fails still shows which files it read,
                // so Compare can try them again.
                if let Some(c) = self.controls() {
                    for (path_box, path) in c.paths.iter().zip(&paths) {
                        path_box.hwnd().set_text(&String::from_utf16_lossy(path));
                    }
                }
                Source::Files(paths)
            }
            None => match self.chosen.take() {
                Some(chosen) => chosen,
                None => return,
            },
        };
        let progress = Arc::new(Progress::default());
        self.progress.replace(Arc::clone(&progress));
        self.start_job(Job::Compare {
            source,
            hex: request.hex,
            encodings: request.encodings,
            options: self.options(request.rows),
            progress,
        });
        hwnd.set_timer(PROGRESS_TIMER, 100);
    }

    /// Esc: stops the comparison that runs; `false` when none runs.
    fn cancel_compare(&self, hwnd: Hwnd) -> bool {
        let (comparing, showing) = {
            let session = self.session.borrow();
            (session.comparing(), session.showing().is_some())
        };
        if !comparing {
            return false;
        }
        // The window stays, so a later Compare of identical files keeps it.
        self.stage.set(Stage::Shown);
        if !showing {
            let cancelled = self.strings.borrow().text("work.cancelled").to_owned();
            self.failure.replace(Some(cancelled));
        }
        self.send(hwnd, Event::Cancel);
        true
    }

    /// Shows the window, after its comparison ended or ran long.
    fn show_window(&self, hwnd: Hwnd) {
        hwnd.kill_timer(SHOW_TIMER);
        if hwnd.is_visible() {
            return;
        }
        window::show(hwnd, self.placement.get());
        self.layout(hwnd);
        if let Some(c) = self.controls() {
            c.sides[0].hwnd().focus();
        }
    }

    /// Applies what the worker did.
    fn receive(&self, hwnd: Hwnd) {
        let outcome = self.work.borrow_mut().as_mut().and_then(|work| work.take());
        if let Some(outcome) = outcome {
            self.on_outcome(hwnd, outcome);
        }
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let saved = self
            .work
            .borrow_mut()
            .as_mut()
            .and_then(|work| work.take_saved());
        if let Some(saved) = saved {
            self.on_saved(hwnd, saved);
        }
    }

    fn on_outcome(&self, hwnd: Hwnd, outcome: Outcome) {
        let title = self.strings.borrow().text("compare.window").to_owned();
        match outcome {
            Outcome::ToolStarted => hwnd.destroy(),
            Outcome::ToolFailed { program, error } => {
                let text = self
                    .strings
                    .borrow()
                    .format("compare.tool_failed", &[&program, &win::error_text(&error)]);
                error_box(self.main.or(Some(hwnd)), &text, &title);
                hwnd.destroy();
            }
            Outcome::Unchosen => {
                let text = self.strings.borrow().text("compare.choose_two").to_owned();
                if self.stage.get() != Stage::Shown {
                    hwnd.kill_timer(SHOW_TIMER);
                    info_box(hwnd, &text, &title);
                    hwnd.destroy();
                } else {
                    self.fail(hwnd, text);
                }
            }
            Outcome::Failed { path, error } => {
                let path = String::from_utf16_lossy(&path);
                let text = self
                    .strings
                    .borrow()
                    .format("viewer.cannot_read", &[&path, &win::error_text(&error)]);
                self.fail(hwnd, text);
            }
            // Only a job that was cancelled or replaced returns it, and the
            // worker drops the outcome of such a job.
            Outcome::Cancelled => {}
            Outcome::Compared(compared) => {
                if self.stage.replace(Stage::Shown) == Stage::First && compared.identical {
                    let names = [0, 1].map(|side| name_of(&compared.paths[side]));
                    let text = self
                        .strings
                        .borrow()
                        .format("compare.identical", &[&names[0], &names[1]]);
                    hwnd.kill_timer(SHOW_TIMER);
                    info_box(hwnd, &text, &title);
                    hwnd.destroy();
                    return;
                }
                if let Some(c) = self.controls() {
                    for (path_box, path) in c.paths.iter().zip(&compared.paths) {
                        path_box.hwnd().set_text(&String::from_utf16_lossy(path));
                    }
                }
                self.failure.replace(None);
                self.send(hwnd, Event::Compared(compared));
                self.show_window(hwnd);
            }
            Outcome::Realigned { generation, view } => {
                // Rows built again replace a rebuild that failed before.
                self.failure.replace(None);
                let place = self.capture_place();
                self.send_at(hwnd, Event::Realigned { generation, view }, place);
            }
        }
    }

    /// Shows why the files were not compared, in the window.
    fn fail(&self, hwnd: Hwnd, text: String) {
        self.stage.set(Stage::Shown);
        self.failure.replace(Some(text));
        self.send(hwnd, Event::Failed);
        self.show_window(hwnd);
    }

    /// Gives the controls the comparison's RTF and places the caret.
    fn present(&self, hwnd: Hwnd, place: Place) {
        let Some(c) = self.controls() else {
            return;
        };
        {
            let session = self.session.borrow();
            let Some(showing) = session.showing() else {
                return;
            };
            let view = showing.view();
            for side in 0..2 {
                c.sides[side].set_rtf(&view.sides[side]);
                c.numbers[side].set_rtf(&view.numbers[side]);
            }
        }
        self.layout(hwnd);
        self.place(place);
        self.apply_title(hwnd);
        self.show_list();
    }

    /// Where the caret is, which [`Self::place`] restores.
    fn capture_place(&self) -> Option<Place> {
        let c = self.controls()?;
        let side = self.active.get();
        let control = c.sides[side.index()];
        let caret = control.selection().start;
        let row = control.row_of(caret);
        let column = caret - control.row_start(row).unwrap_or(caret).min(caret);
        let line = (self.session.borrow().showing())
            .and_then(|showing| showing.view().rows.rows.get(row)?.line(side));
        Some(Place::Line {
            side,
            line,
            row,
            column,
            first_visible: control.first_visible_row(),
            x: control.scroll_position().0,
        })
    }

    fn place(&self, place: Place) {
        let Some(c) = self.controls() else {
            return;
        };
        match place {
            Place::FirstDifference => {
                let first = {
                    let session = self.session.borrow();
                    let rows = (session.showing())
                        .map_or(&[][..], |showing| showing.view().rows.rows.as_slice());
                    match rows.first() {
                        Some(row) if row.differs() => Some(0),
                        _ => compare::next_difference(rows, 0),
                    }
                };
                self.go_to_row(first.unwrap_or(0));
            }
            Place::Line {
                side,
                line,
                row,
                column,
                first_visible,
                x,
            } => {
                let new_row = {
                    let session = self.session.borrow();
                    let rows = (session.showing())
                        .map_or(&[][..], |showing| showing.view().rows.rows.as_slice());
                    let found = line.and_then(|line| {
                        rows.iter().position(|each| each.line(side) == Some(line))
                    });
                    found.unwrap_or(row).min(rows.len().saturating_sub(1))
                };
                let control = c.sides[side.index()];
                let start = control.row_start(new_row).unwrap_or(0);
                let length = control.row_length(new_row);
                control.select(start + column.min(length)..start + column.min(length));
                // The row stays as far from the top as it was.
                let top = (first_visible + new_row).saturating_sub(row);
                control.scroll_rows(top as isize - control.first_visible_row() as isize);
                control.set_scroll_position((x, control.scroll_position().1));
                self.sync_from(side);
            }
        }
    }

    /// Places the caret of both sides at the start of `row`, with a few
    /// rows above it in view.
    fn go_to_row(&self, row: usize) {
        let Some(c) = self.controls() else {
            return;
        };
        let active = self.active.get();
        for side in [active.other(), active] {
            let control = c.sides[side.index()];
            if let Some(start) = control.row_start(row) {
                control.select(start..start);
            }
        }
        let control = c.sides[active.index()];
        let top = row.saturating_sub(CONTEXT_ROWS);
        control.scroll_rows(top as isize - control.first_visible_row() as isize);
        control.set_scroll_position((0, control.scroll_position().1));
        self.sync_from(active);
        self.show_list();
    }

    /// Scrolls the other side and both line numbers to `side`'s place.
    fn sync_from(&self, side: Side) {
        let Some(c) = self.controls() else {
            return;
        };
        let other = side.other();
        let position = c.sides[side.index()].scroll_position();
        c.sides[other.index()].set_scroll_position(position);
        let mut positions = [(0, 0); 2];
        positions[side.index()] = position;
        positions[other.index()] = c.sides[other.index()].scroll_position();
        for each in Side::BOTH {
            let y = positions[each.index()].1;
            c.numbers[each.index()].set_scroll_position((0, y));
        }
        self.scroll.set(positions);
    }

    /// A side scrolled, by any means: the other follows.
    fn follow_scroll(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let now = c.sides.map(RichText::scroll_position);
        let last = self.scroll.get();
        if now == last {
            return;
        }
        let moved = if now[0] != last[0] {
            Side::Left
        } else {
            Side::Right
        };
        self.sync_from(moved);
    }

    /// Creates the fonts for the window's DPI and text size and gives the
    /// window's font to its controls.
    fn apply_fonts(&self, hwnd: Hwnd) {
        let Some(c) = self.controls() else {
            return;
        };
        let Ok(ui) = Font::message(hwnd.dpi()) else {
            return;
        };
        let controls = [
            c.paths[0].hwnd(),
            c.paths[1].hwnd(),
            c.browse[0].hwnd(),
            c.browse[1].hwnd(),
            c.compare.hwnd(),
            c.checks[0].hwnd(),
            c.checks[1].hwnd(),
            c.checks[2].hwnd(),
            c.extra.hwnd(),
            c.cancel.hwnd(),
            c.status.hwnd(),
        ];
        for control in controls {
            control.set_font(&ui);
        }
        // Consolas at the height of the message font, as the sides show
        // it at their zoom.
        let text = Font::monospace_of(c.paths[0].hwnd())
            .and_then(|font| font.scaled(self.state.get().zoom));
        let (char_width, row_height) = match &text {
            Ok(text) => {
                let meter = TextMeter::new(text);
                (meter.width("0"), meter.height())
            }
            Err(_) => (Px(8), Px(16)),
        };
        self.fonts.replace(Some(Fonts {
            ui,
            char_width,
            row_height,
        }));
    }

    /// The check boxes, the extra lines and the zoom as the state says.
    fn apply_state(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let state = self.state.get();
        c.checks[0].set_checked(state.case_sensitive);
        c.checks[1].set_checked(state.ignore_spaces);
        c.checks[2].set_checked(state.only_differences);
        c.extra.hwnd().set_text(&state.extra_lines.to_string());
        for control in c.rich() {
            control.set_zoom(state.zoom);
        }
    }

    /// The menus, the captions and the title in the window's language.
    fn apply_strings(&self, hwnd: Hwnd) {
        let Some(c) = self.controls() else {
            return;
        };
        {
            let strings = self.strings.borrow();
            let captions = [
                (c.browse[0].hwnd(), "compare.browse"),
                (c.browse[1].hwnd(), "compare.browse_right"),
                (c.compare.hwnd(), "compare.compare"),
                (c.checks[0].hwnd(), "compare.case_sensitive"),
                (c.checks[1].hwnd(), "compare.ignore_spaces"),
                (c.checks[2].hwnd(), "compare.only_differences"),
                (c.cancel.hwnd(), "button.cancel"),
            ];
            for (control, key) in captions {
                control.set_text(strings.text(key));
            }
        }
        self.apply_menus(hwnd);
        self.apply_title(hwnd);
        self.show_status();
        self.layout(hwnd);
    }

    /// The menu bar, at the window's DPI and with icons while they show.
    fn apply_menus(&self, hwnd: Hwnd) {
        let _ = replace_menu_bar(hwnd, &menu_bar(&self.strings.borrow()));
    }

    fn title_text(&self) -> String {
        let names = match self.session.borrow().showing() {
            Some(showing) => Side::BOTH.map(|side| showing.name(side)),
            None => [String::new(), String::new()],
        };
        title(&self.strings.borrow(), [&names[0], &names[1]])
    }

    fn apply_title(&self, hwnd: Hwnd) {
        hwnd.set_text(&self.title_text());
    }

    /// The status bar: what runs, or why nothing shows, or the differences
    /// with the notes, then each side's encoding.
    fn show_status(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let strings = self.strings.borrow();
        let numbers = self.numbers.borrow();
        let session = self.session.borrow();
        let mut parts = [String::new(), String::new(), String::new()];
        if session.comparing() {
            parts[0] = strings.text("compare.comparing").to_owned();
        } else if let Some(failure) = self.failure.borrow().as_ref() {
            parts[0] = failure.clone();
        } else if let Some(showing) = session.showing() {
            let view = showing.view();
            let mut texts = vec![differences_text(&strings, &*numbers, view.differences)];
            let notes = [
                showing.note(),
                view.rows.forced.then_some(Note::OnlyDifferences),
                view.rows.truncated.then_some(Note::Truncated),
                (showing.mode() == Mode::Text
                    && showing
                        .sizes()
                        .iter()
                        .any(|size| *size > compare::EDIT_LIMIT))
                .then_some(Note::EditLarge),
            ];
            texts.extend(
                notes
                    .into_iter()
                    .flatten()
                    .map(|note| note_text(&strings, &*numbers, note)),
            );
            parts[0] = texts.join(" · ");
            let hex = text::without_access_key(strings.text("viewer.hex"));
            for (side, encoding) in showing.encodings().into_iter().enumerate() {
                parts[side + 1] =
                    encoding.map_or_else(|| hex.clone(), |encoding| encoding.name().to_owned());
            }
        }
        for (index, part) in parts.iter().enumerate() {
            c.status.set_text(index, part);
        }
    }

    /// The two-line list: the caret's row from each side, one above the
    /// other, with the parts that differ marked.
    fn show_list(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        if !self.state.get().two_line_list {
            return;
        }
        let side = c.sides[self.active.get().index()];
        let row = side.row_of(side.selection().start);
        let texts = c.sides.map(|side| side.row_text(row));
        let marks = self.row_marks(row, &texts);
        let mut rtf = Rtf::new(&palette(), rtf_font());
        for index in 0..2 {
            rtf.row(&texts[index], Style::Text, &marks[index]);
        }
        c.list.set_rtf(&rtf.finish());
    }

    /// The parts of `row` that differ, as the sides mark them; `texts` are
    /// the two sides' text of the row, which the list shows.
    fn row_marks(&self, row: usize, texts: &[String; 2]) -> [Vec<std::ops::Range<usize>>; 2] {
        let session = self.session.borrow();
        let Some(showing) = session.showing() else {
            return [Vec::new(), Vec::new()];
        };
        let Some(shown) = (showing.view().rows.rows.get(row)).filter(|shown| shown.differs())
        else {
            return [Vec::new(), Vec::new()];
        };
        match showing.mode() {
            // The whole lines, as in the worker, because `texts` stop at the
            // list's limit.
            Mode::Text => compare::row_marks(
                Side::BOTH.map(|side| {
                    (shown.line(side))
                        .and_then(|line| showing.text(side)?.lines.get(usize::try_from(line).ok()?))
                        .map(String::as_str)
                }),
                showing.matching(),
            ),
            Mode::Hex => {
                let [left, right] = showing.sizes();
                let digits = crate::core::viewer::offset_digits(left.max(right));
                let bytes = Side::BOTH.map(|side| {
                    (shown.line(side)).map(|_| compare::hex_row_bytes(&texts[side.index()], digits))
                });
                compare::hex_marks(bytes[0].as_deref(), bytes[1].as_deref(), digits)
            }
        }
    }

    /// What the layout measures.
    fn sizes(&self, hwnd: Hwnd) -> Option<CompareSizes> {
        let fonts = self.fonts.borrow();
        let fonts = fonts.as_ref()?;
        let dpi = hwnd.dpi();
        let meter = TextMeter::new(&fonts.ui);
        let strings = self.strings.borrow();
        let caption = |key: &str| meter.width(&text::without_access_key(strings.text(key)));
        let button = |key: &str| image_button_width(caption(key), dpi);
        let digits = match self.session.borrow().showing() {
            Some(showing) if showing.mode() == Mode::Text => Side::BOTH.map(|side| {
                let lines = showing.text(side).map_or(0, |text| text.lines.len());
                lines.max(1).to_string().len() + 2
            }),
            Some(_) => [1, 1],
            None => [3, 3],
        };
        let chars = |count: usize| Px(fonts.char_width.0 * i32::try_from(count).unwrap_or(0));
        Some(CompareSizes {
            line: Px(meter.height().0 * 3 / 2 + 2),
            browse: button("compare.browse").max(button("compare.browse_right")),
            compare: button("compare.compare"),
            cancel: button("button.cancel"),
            checks: [
                caption("compare.case_sensitive"),
                caption("compare.ignore_spaces"),
                caption("compare.only_differences"),
            ],
            extra: meter.width("999"),
            row: fonts.row_height,
            numbers: digits.map(chars),
        })
    }

    /// Places the controls.
    fn layout(&self, hwnd: Hwnd) {
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let Some(c) = self.controls() else {
            return;
        };
        let size = hwnd.client_size();
        if size.width.0 == 0 || size.height.0 == 0 {
            return;
        }
        let Some(sizes) = self.sizes(hwnd) else {
            return;
        };
        let dpi = hwnd.dpi();
        let status_height = c.status.fit();
        let area = Size {
            width: size.width,
            height: (size.height - status_height).max(Px(0)),
        };
        let state = self.state.get();
        let layout = compare_layout(
            area,
            &sizes,
            state.above_each_other,
            state.two_line_list,
            dpi,
        );
        let running = self.session.borrow().comparing();
        win::place_children(
            &[
                (c.paths[0].hwnd(), layout.paths[0]),
                (c.browse[0].hwnd(), layout.browse[0]),
                (c.paths[1].hwnd(), layout.paths[1]),
                (c.browse[1].hwnd(), layout.browse[1]),
                (c.compare.hwnd(), layout.compare),
                (c.checks[0].hwnd(), layout.checks[0]),
                (c.checks[1].hwnd(), layout.checks[1]),
                (c.checks[2].hwnd(), layout.checks[2]),
                (c.extra.hwnd(), layout.extra),
                (c.progress.hwnd(), layout.progress),
                (c.cancel.hwnd(), layout.cancel),
                (c.sides[0].hwnd(), layout.sides[0]),
                (c.sides[1].hwnd(), layout.sides[1]),
                (c.numbers[0].hwnd(), layout.numbers[0]),
                (c.numbers[1].hwnd(), layout.numbers[1]),
                (c.list.hwnd(), layout.list),
            ],
            &[
                (c.progress.hwnd(), running),
                (c.cancel.hwnd(), running),
                (c.list.hwnd(), state.two_line_list),
            ],
        );
        let encoding = {
            let fonts = self.fonts.borrow();
            fonts.as_ref().map_or(Px(0), |fonts| {
                let meter = TextMeter::new(&fonts.ui);
                status_last_part(meter.width("Windows-1252 UTF-16 LE"), dpi)
            })
        };
        let width = size.width;
        c.status
            .set_parts(&[(width - encoding - encoding).max(Px(0)), encoding]);
        self.sync_from(self.active.get());
    }

    /// Lets the person type in each side that takes typing now.
    fn apply_read_only(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let typing = {
            let session = self.session.borrow();
            Side::BOTH.map(|side| session.typing(side))
        };
        for side in Side::BOTH {
            c.sides[side.index()].set_read_only(!typing[side.index()]);
        }
    }

    /// The person typed, pasted or deleted in `side`.
    fn on_edited(&self, hwnd: Hwnd, side: Side) {
        let Some(c) = self.controls() else {
            return;
        };
        if !self.session.borrow().typing(side) {
            return;
        }
        let control = c.sides[side.index()];
        let (text, caret) = (control.text(), control.selection().start);
        self.send(hwnd, Event::Edited { side, text, caret });
        self.show_list();
    }

    /// Runs a command that reads the rows, which are fresh.
    fn run_command(&self, hwnd: Hwnd, command: CompareCommand, fresh: Fresh) {
        use CompareCommand as C;
        match command {
            C::CopyToRight => self.copy_to(hwnd, Side::Right, fresh),
            C::CopyToLeft => self.copy_to(hwnd, Side::Left, fresh),
            C::Resync => self.resync(hwnd, fresh),
            C::Swap => self.swap(hwnd, fresh),
            C::EditMode => self.send(hwnd, Event::EditMode(fresh)),
            C::NextDifference => self.go_to_difference(true),
            C::PreviousDifference => self.go_to_difference(false),
            C::NextInLine => self.go_to_mark(true),
            C::PreviousInLine => self.go_to_mark(false),
            _ => {}
        }
    }

    /// Copy to right and Copy to left: the selected rows, or the difference
    /// with the caret, replace the lines they pair with on side `to`.
    fn copy_to(&self, hwnd: Hwnd, to: Side, fresh: Fresh) {
        let Some(c) = self.controls() else {
            return;
        };
        let control = c.sides[self.active.get().index()];
        let selection = control.selection();
        let first = control.row_of(selection.start);
        let rows = if selection.is_empty() {
            let session = self.session.borrow();
            let shown =
                (session.showing()).map_or(&[][..], |showing| showing.view().rows.rows.as_slice());
            compare::difference_at(shown, first)
        } else {
            first..control.row_of(selection.end - 1).max(first) + 1
        };
        self.send(hwnd, Event::Copy { fresh, to, rows });
    }

    /// Resync from here: pairs the caret's line of each side, and matches
    /// the lines before and after them again.
    fn resync(&self, hwnd: Hwnd, fresh: Fresh) {
        let Some(c) = self.controls() else {
            return;
        };
        let rows = c.sides.map(|side| side.row_of(side.selection().start));
        self.send(hwnd, Event::Resync { fresh, rows });
    }

    /// Next and Previous difference.
    fn go_to_difference(&self, forward: bool) {
        let Some(c) = self.controls() else {
            return;
        };
        let control = c.sides[self.active.get().index()];
        let row = control.row_of(control.selection().start);
        let target = {
            let session = self.session.borrow();
            let Some(showing) = session.showing() else {
                return;
            };
            let rows = &showing.view().rows.rows;
            if forward {
                compare::next_difference(rows, row)
            } else {
                compare::previous_difference(rows, row)
            }
        };
        if let Some(target) = target {
            self.go_to_row(target);
        }
    }

    /// `F2` and `Shift+F2`: the next or previous marked part of the
    /// caret's row, and past its end the one of the next row that differs.
    fn go_to_mark(&self, forward: bool) {
        let Some(c) = self.controls() else {
            return;
        };
        let side = self.active.get();
        let control = c.sides[side.index()];
        let selection = control.selection();
        let row = control.row_of(selection.start);
        let column = selection.start - control.row_start(row).unwrap_or(0).min(selection.start);
        let count =
            (self.session.borrow().showing()).map_or(0, |showing| showing.view().rows.rows.len());
        let marks_of = |row: usize| {
            // Rows that do not differ have no marks, and reading their text
            // first would cost every row the search passes.
            let differs = (self.session.borrow().showing())
                .and_then(|showing| {
                    showing
                        .view()
                        .rows
                        .rows
                        .get(row)
                        .map(|shown| shown.differs())
                })
                .unwrap_or(false);
            if !differs {
                return Vec::new();
            }
            let texts = c.sides.map(|side| side.row_text(row));
            let [left, right] = self.row_marks(row, &texts);
            match side {
                Side::Left => left,
                Side::Right => right,
            }
        };
        let in_row = marks_of(row).into_iter().filter(|mark| {
            if forward {
                mark.start > column || (selection.is_empty() && mark.start == column)
            } else {
                mark.start < column
            }
        });
        let mut found = if forward {
            in_row.min_by_key(|mark| mark.start)
        } else {
            in_row.max_by_key(|mark| mark.start)
        }
        .map(|mark| (row, mark));
        let mut next = row;
        while found.is_none() {
            next = match (forward, next) {
                (true, next) if next + 1 < count => next + 1,
                (false, next) if next > 0 => next - 1,
                _ => break,
            };
            let marks = marks_of(next);
            let mark = if forward { marks.first() } else { marks.last() };
            found = mark.map(|mark| (next, mark.clone()));
        }
        let Some((row, mark)) = found else {
            return;
        };
        let start = control.row_start(row).unwrap_or(0);
        control.select(start + mark.start..start + mark.end);
        self.sync_from(side);
        self.show_list();
    }

    /// The control with the focus, when it is a side or the two-line list.
    fn focused_text(&self) -> Option<RichText> {
        let c = self.controls()?;
        let focused = Hwnd::focused()?;
        [c.sides[0], c.sides[1], c.list]
            .into_iter()
            .find(|control| control.hwnd() == focused)
    }

    /// Where the focus is, which decides the keys the window takes.
    fn key_focus(&self, target: Hwnd) -> KeyFocus {
        let Some(c) = self.controls() else {
            return KeyFocus::Other;
        };
        if [c.sides[0], c.sides[1], c.list]
            .iter()
            .any(|control| control.hwnd() == target)
        {
            KeyFocus::Text {
                editing: self.session.borrow().editing(),
            }
        } else if [c.paths[0], c.paths[1], c.extra]
            .iter()
            .any(|edit| edit.hwnd() == target)
        {
            KeyFocus::Box
        } else {
            KeyFocus::Other
        }
    }

    fn on_key(&self, hwnd: Hwnd, key: CompareKey, target: Hwnd) {
        let Some(c) = self.controls() else {
            return;
        };
        match key {
            CompareKey::Cancel => {
                if !self.cancel_compare(hwnd) {
                    self.close(hwnd);
                }
            }
            CompareKey::Command(command) => self.on_command(hwnd, command),
            CompareKey::SelectLine => {
                if let Some(control) = [c.sides[0], c.sides[1], c.list]
                    .into_iter()
                    .find(|control| control.hwnd() == target)
                {
                    let row = control.row_of(control.selection().start);
                    let start = control.row_start(row).unwrap_or(0);
                    let length = control.row_length(row);
                    control.select(start..start + length);
                }
            }
            CompareKey::NextPane { forward } => {
                let mut panes = vec![c.sides[0].hwnd(), c.sides[1].hwnd()];
                if self.state.get().two_line_list {
                    panes.push(c.list.hwnd());
                }
                let at = panes.iter().position(|pane| *pane == target).unwrap_or(0);
                let next = if forward {
                    (at + 1) % panes.len()
                } else {
                    (at + panes.len() - 1) % panes.len()
                };
                panes[next].focus();
            }
            CompareKey::ContextMenu => self.context_menu(hwnd, None),
        }
    }

    fn on_command(&self, hwnd: Hwnd, command: CompareCommand) {
        use CompareCommand as C;
        let Some(c) = self.controls() else {
            return;
        };
        let active = c.sides[self.active.get().index()];
        match command {
            C::Compare => self.compare_boxes(hwnd),
            C::Save => self.send(hwnd, Event::Save),
            C::Close => self.close(hwnd),
            C::Undo => self.send(hwnd, Event::Undo),
            C::Copy => match self.focused_text() {
                Some(control) => control.copy(),
                None => {
                    if let Some(edit) = [c.paths[0], c.paths[1], c.extra]
                        .into_iter()
                        .find(|edit| Some(edit.hwnd()) == Hwnd::focused())
                    {
                        edit.copy();
                    }
                }
            },
            C::Paste => {
                if self.session.borrow().editing() {
                    active.paste_text();
                }
            }
            C::SelectAll => self.focused_text().unwrap_or(active).select_all(),
            C::Find => {
                if let Some(query) = self.ask_query(hwnd) {
                    self.find(hwnd, &query, false);
                }
            }
            C::FindNext | C::FindPrevious => {
                let last = self
                    .shared
                    .upgrade()
                    .and_then(|shared| shared.query.borrow().clone());
                if let Some(query) = last.or_else(|| self.ask_query(hwnd)) {
                    self.find(hwnd, &query, command == C::FindPrevious);
                }
            }
            C::EditMode
            | C::CopyToRight
            | C::CopyToLeft
            | C::Resync
            | C::Swap
            | C::NextDifference
            | C::PreviousDifference
            | C::NextInLine
            | C::PreviousInLine => self.send(hwnd, Event::Command(command)),
            C::Text => self.send(hwnd, Event::View { hex: false }),
            C::Hex => self.send(hwnd, Event::View { hex: true }),
            C::OnlyDifferences => {
                self.save_state(|state| state.only_differences = !state.only_differences);
                c.checks[2].set_checked(self.state.get().only_differences);
                self.send_options(hwnd);
            }
            C::TwoLineList => {
                self.save_state(|state| state.two_line_list = !state.two_line_list);
                self.layout(hwnd);
                self.show_list();
            }
            C::AboveEachOther => {
                self.save_state(|state| state.above_each_other = !state.above_each_other);
                self.layout(hwnd);
            }
            C::ZoomIn => self.set_zoom(hwnd, zoomed(self.state.get().zoom, 1)),
            C::ZoomOut => self.set_zoom(hwnd, zoomed(self.state.get().zoom, -1)),
            C::ZoomReset => self.set_zoom(hwnd, DEFAULT_ZOOM),
            C::Encoding(side, encoding) => self.send(hwnd, Event::Encoding(side, encoding)),
        }
    }

    /// Compare, or `Enter` in a box: compares the files that the path boxes
    /// name, after asking about changes.
    fn compare_boxes(&self, hwnd: Hwnd) {
        let Some(c) = self.controls() else {
            return;
        };
        // File Explorer's Copy as path puts the path in quotes, so a pasted
        // one opens too.
        let paths = c.paths.map(|path| {
            let text = path.text();
            let text = text.trim();
            let text = (text.strip_prefix('"'))
                .and_then(|text| text.strip_suffix('"'))
                .unwrap_or(text);
            text.encode_utf16().collect::<Vec<u16>>()
        });
        if paths.iter().any(Vec::is_empty) {
            let (text, title) = {
                let strings = self.strings.borrow();
                (
                    strings.text("compare.choose_two").to_owned(),
                    strings.text("compare.window").to_owned(),
                )
            };
            info_box(hwnd, &text, &title);
            return;
        }
        self.send(hwnd, Event::Compare(paths));
    }

    /// The check boxes and the extra lines, as the state holds them.
    fn send_options(&self, hwnd: Hwnd) {
        let state = self.state.get();
        self.send(
            hwnd,
            Event::Options {
                matching: matching_of(state),
                only_differences: only_differences_of(state),
            },
        );
    }

    /// Swap sides: the files change places, with their changes, and the
    /// caret stays on its line, which moves to the other side.
    fn swap(&self, hwnd: Hwnd, fresh: Fresh) {
        let place = match self.capture_place() {
            Some(Place::Line {
                side,
                line,
                row,
                column,
                first_visible,
                x,
            }) => Place::Line {
                side: side.other(),
                line,
                row,
                column,
                first_visible,
                x,
            },
            _ => Place::FirstDifference,
        };
        self.send_at(hwnd, Event::Swap(fresh), Some(place));
    }

    /// The path boxes and the side the person works in follow the files.
    fn swap_boxes(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        self.active.set(self.active.get().other());
        let texts = c.paths.map(|path| path.text());
        for side in 0..2 {
            c.paths[side].hwnd().set_text(&texts[1 - side]);
        }
    }

    /// Shows the text at `zoom` percent, which the next window opens with.
    fn set_zoom(&self, hwnd: Hwnd, zoom: u16) {
        if self.state.get().zoom == zoom {
            return;
        }
        self.save_state(|state| state.zoom = zoom);
        if let Some(c) = self.controls() {
            for control in c.rich() {
                control.set_zoom(zoom);
            }
        }
        self.apply_fonts(hwnd);
        self.layout(hwnd);
    }

    /// Keeps a change of the state for the next window, which the main
    /// window saves.
    fn save_state(&self, change: impl FnOnce(&mut CompareState)) {
        let mut state = self.state.get();
        change(&mut state);
        self.state.set(state);
        if let Some(shared) = self.shared.upgrade() {
            shared.state.set(state);
            shared.changed.set(true);
        }
        if let Some(main) = self.main {
            main.post_deferred();
        }
    }

    /// Asks one question about `changed`, the sides with changes, as Total
    /// Commander does: Save, and Save as…, for each changed side and, when
    /// both changed, for both; Don't save; Cancel.
    fn ask(&self, hwnd: Hwnd, changed: &[Side]) -> Answer {
        let groups: Vec<Vec<Side>> = match changed {
            [_, _] => vec![vec![Side::Left], vec![Side::Right], changed.to_vec()],
            _ => vec![changed.to_vec()],
        };
        let title = text::without_access_key(self.strings.borrow().text("compare.save"));
        let (question, labels, cancel) = {
            let session = self.session.borrow();
            let strings = self.strings.borrow();
            let names: Vec<String> = (changed.iter())
                .map(|side| {
                    (session.showing())
                        .map(|showing| showing.name(*side))
                        .unwrap_or_default()
                })
                .collect();
            let question = match names.as_slice() {
                [left, right] => strings.format("compare.save_question_both", &[left, right]),
                [one, ..] => strings.format("compare.save_question", &[one]),
                [] => String::new(),
            };
            let mut labels = Vec::new();
            for group in &groups {
                let key = match group.as_slice() {
                    [Side::Left] => "compare.save_left",
                    [Side::Right] => "compare.save_right",
                    _ => "compare.save_both",
                };
                labels.push(strings.text(key).to_owned());
                labels.push(strings.text(&format!("{key}_as")).to_owned());
            }
            labels.push(strings.text("compare.dont_save").to_owned());
            (question, labels, strings.text("button.cancel").to_owned())
        };
        let choices: Vec<&str> = labels.iter().map(String::as_str).collect();
        let Some(index) = choose(hwnd, &title, &question, &choices, &cancel) else {
            return Answer::Cancel;
        };
        let Some(saved) = groups.get(index / 2) else {
            return Answer::DontSave;
        };
        let mut files = Vec::new();
        for &side in saved {
            let path = if index % 2 == 1 {
                match self.save_as(hwnd, side) {
                    Some(path) => path,
                    None => return Answer::Cancel,
                }
            } else {
                let session = self.session.borrow();
                let Some(showing) = session.showing() else {
                    continue;
                };
                showing.paths()[side.index()].clone()
            };
            match self.encoded(side) {
                Ok(bytes) => files.push(SaveFile { side, path, bytes }),
                Err(text) => {
                    error_box(Some(hwnd), &text, &title);
                    return Answer::Cancel;
                }
            }
        }
        Answer::Save(files)
    }

    /// The bytes that Save writes for `side`, or why it cannot.
    fn encoded(&self, side: Side) -> Result<Vec<u8>, String> {
        let session = self.session.borrow();
        let showing = session.showing();
        let Some((showing, text)) =
            showing.and_then(|showing| Some((showing, showing.text(side)?)))
        else {
            return Err(String::new());
        };
        compare::encoded(&text.lines, text.form, &code_page_encoded).ok_or_else(|| {
            self.strings.borrow().format(
                "compare.cannot_encode",
                &[&showing.name(side), text.form.encoding.name()],
            )
        })
    }

    /// Save as… for `side`: the file the person chose. The dialog opens in
    /// the side's folder, as Notepad's does.
    fn save_as(&self, hwnd: Hwnd, side: Side) -> Option<Vec<u16>> {
        let (title, all, folder, name) = {
            let strings = self.strings.borrow();
            let session = self.session.borrow();
            let showing = session.showing();
            (
                text::without_access_key(strings.text("compare.save_as")),
                strings.text("compare.all_files").to_owned(),
                showing.map(|showing| {
                    String::from_utf16_lossy(folder_of(&showing.paths()[side.index()]))
                }),
                showing
                    .map(|showing| showing.name(side))
                    .unwrap_or_default(),
            )
        };
        let types = [FileType {
            name: &all,
            extension: "*",
        }];
        let (path, _) = save_dialog(hwnd, &title, folder.as_deref(), &name, &types)?;
        Some(path.as_os_str().encode_wide().collect())
    }

    /// Tells the session what the save wrote, then says what it did not.
    fn on_saved(&self, hwnd: Hwnd, saved: Saved) {
        let mut failures = Vec::new();
        let files = (saved.into_iter())
            .map(|(side, path, result)| {
                let written = result.is_ok();
                if let Err(failure) = result {
                    failures.push((path.clone(), failure));
                }
                SavedFile {
                    side,
                    path,
                    saved: written,
                }
            })
            .collect();
        self.send(hwnd, Event::Saved(files));
        if let Some(shared) = self.shared.upgrade() {
            shared.resume_exit(self.main);
        }
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let title = text::without_access_key(self.strings.borrow().text("compare.save"));
        for (path, failure) in failures {
            let text = {
                let strings = self.strings.borrow();
                let text = strings.format(
                    "compare.save_failed",
                    &[&name_of(&path), &win::error_text(&failure.error)],
                );
                match failure.kept.as_ref().and_then(|kept| kept.file_name()) {
                    Some(kept) => format!(
                        "{text} {}",
                        strings.format("compare.save_kept", &[&kept.to_string_lossy()])
                    ),
                    None => text,
                }
            };
            error_box(Some(hwnd), &text, &title);
        }
    }

    /// The Find dialog, without Hex bytes; what the person chose, or
    /// `None` for Cancel.
    fn ask_query(&self, hwnd: Hwnd) -> Option<Query> {
        let shared = self.shared.upgrade()?;
        let last = shared.query.borrow().clone().unwrap_or_default();
        let dialog = ViewerFind::without_hex(last);
        let captions = DialogCaptions::new(DialogId::ViewerFind, &self.strings.borrow());
        if run_modal(hwnd, DialogId::ViewerFind, &captions, &dialog).ok()? != DialogEnd::Ok {
            return None;
        }
        let query = dialog.chosen();
        shared.query.replace(Some(query.clone()));
        Some(query)
    }

    /// Looks for `query` from the selection of the side with the caret,
    /// then in the other side.
    fn find(&self, hwnd: Hwnd, query: &Query, backward: bool) {
        let Some(c) = self.controls() else {
            return;
        };
        let side = self.active.get();
        let control = c.sides[side.index()];
        let selection = control.selection();
        let from = if backward {
            selection.start
        } else {
            selection.end
        };
        let search = |control: RichText, from: usize| {
            control.find(
                &query.text,
                from,
                backward,
                query.match_case,
                query.whole_words,
            )
        };
        let other = c.sides[side.other().index()];
        let other_from = if backward { other.length() } else { 0 };
        let found = search(control, from)
            .map(|range| (side, range))
            .or_else(|| search(other, other_from).map(|range| (side.other(), range)));
        match found {
            Some((side, range)) => {
                let control = c.sides[side.index()];
                self.active.set(side);
                control.hwnd().focus();
                control.select(range);
                self.sync_from(side);
                self.show_list();
            }
            None => {
                let (text, title) = {
                    let strings = self.strings.borrow();
                    (
                        strings.format("viewer.not_found", &[&query.text]),
                        strings.text("viewer.find_title").to_owned(),
                    )
                };
                info_box(hwnd, &text, &title);
            }
        }
    }

    fn menu_state(&self) -> MenuState {
        let focused = Hwnd::focused();
        let c = self.controls();
        let selected = self
            .focused_text()
            .is_some_and(|control| !control.selection().is_empty())
            || c.is_some_and(|c| {
                [c.paths[0], c.paths[1], c.extra]
                    .into_iter()
                    .any(|edit| Some(edit.hwnd()) == focused && edit.has_selected_text())
            });
        let session = self.session.borrow();
        let showing = session.showing();
        let state = self.state.get();
        MenuState {
            shown: showing.map(|showing| (showing.mode(), showing.encodings())),
            note: showing.and_then(|showing| showing.note()),
            editing: session.editing(),
            can_edit: session.can_edit(),
            changed: session.has_changes(),
            can_undo: session.can_undo(),
            selected,
            side: c.and_then(|c| focused.and_then(|focused| c.side_of(focused))),
            editable: Side::BOTH.map(|side| showing.is_some_and(|showing| showing.editable(side))),
            only_differences: state.only_differences,
            two_line_list: state.two_line_list,
            above_each_other: state.above_each_other,
            zoom: state.zoom,
        }
    }

    /// The context menu of the side with the caret, at `point` in screen
    /// coordinates, or near the side's corner for the keyboard.
    fn context_menu(&self, hwnd: Hwnd, point: Option<(i32, i32)>) {
        let Some(c) = self.controls() else {
            return;
        };
        let side = c.sides[self.active.get().index()];
        let corner = side.hwnd().screen_rect();
        let point = point.unwrap_or((corner.x.0 + 16, corner.y.0 + 16));
        let nodes = context_menu(&self.strings.borrow());
        let states = menu_states(self.menu_state());
        // The menu runs its own message loop, so nothing is borrowed here.
        let chosen = Menu::track_popup(hwnd, &nodes, point, &states, None);
        if let Some(command) = chosen.and_then(CompareCommand::from_id) {
            self.on_command(hwnd, command);
        }
    }

    /// Esc, File > Close, Alt+F4: asks about changes first.
    fn close(&self, hwnd: Hwnd) {
        self.send(hwnd, Event::Close);
    }

    /// Activates the main window, which opened this one, as a Viewer does.
    fn close_now(&self, hwnd: Hwnd) {
        if let Some(main) = self.main.filter(|main| main.is_on_screen()) {
            main.bring_to_front();
        }
        hwnd.destroy();
    }

    /// Keeps the shown window's size and place for the next window.
    fn remember_placement(&self, hwnd: Hwnd) {
        if !hwnd.is_visible() {
            return;
        }
        if let Some(placement) = hwnd.placement() {
            self.placement.set(Some(placement));
            self.save_state(|state| state.placement = Some(placement));
        }
    }

    /// A file or two that File Explorer dropped: one sets the path of the
    /// side it fell on, two set both.
    fn drop_files(&self, hwnd: Hwnd, files: Vec<Vec<u16>>, point: (i32, i32)) {
        let Some(c) = self.controls() else {
            return;
        };
        let right = c.sides[1].hwnd().rect_in(hwnd);
        let side = if self.state.get().above_each_other {
            if point.1 >= right.y.0 { 1 } else { 0 }
        } else if point.0 >= right.x.0 {
            1
        } else {
            0
        };
        match files.as_slice() {
            [] => {}
            [one] => c.paths[side]
                .hwnd()
                .set_text(&String::from_utf16_lossy(one)),
            [first, second, ..] => {
                c.paths[0].hwnd().set_text(&String::from_utf16_lossy(first));
                c.paths[1]
                    .hwnd()
                    .set_text(&String::from_utf16_lossy(second));
            }
        }
    }

    /// Browse…: the file the person chose goes in the side's path box.
    fn browse(&self, hwnd: Hwnd, side: usize) {
        let (title, all) = {
            let strings = self.strings.borrow();
            (
                strings.text("compare.browse_title").to_owned(),
                strings.text("compare.all_files").to_owned(),
            )
        };
        if let Some(path) = pick_file(hwnd, &title, &all, "*.*")
            && let Some(c) = self.controls()
        {
            c.paths[side].hwnd().set_text(&path);
        }
    }

    /// A control of the window notified it.
    fn on_control(&self, hwnd: Hwnd, event: crate::win::notify::ControlEvent) {
        let Some(c) = self.controls() else {
            return;
        };
        for side in Side::BOTH {
            if c.sides[side.index()].changed(event) {
                return self.on_edited(hwnd, side);
            }
            if c.browse[side.index()].clicked(event) {
                return self.browse(hwnd, side.index());
            }
        }
        if c.compare.clicked(event) {
            self.compare_boxes(hwnd);
        } else if c.cancel.clicked(event) {
            self.cancel_compare(hwnd);
        } else if c.checks[0].clicked(event) {
            let checked = c.checks[0].is_checked();
            self.save_state(|state| state.case_sensitive = checked);
            self.send_options(hwnd);
        } else if c.checks[1].clicked(event) {
            let checked = c.checks[1].is_checked();
            self.save_state(|state| state.ignore_spaces = checked);
            self.send_options(hwnd);
        } else if c.checks[2].clicked(event) {
            let checked = c.checks[2].is_checked();
            self.save_state(|state| state.only_differences = checked);
            self.send_options(hwnd);
        } else if c.extra.changed(event) {
            let Ok(lines) = c.extra.text().trim().parse::<u32>() else {
                return;
            };
            let lines = lines.min(compare::MAX_EXTRA_LINES);
            if lines == self.state.get().extra_lines {
                return;
            }
            self.save_state(|state| state.extra_lines = lines);
            self.send_options(hwnd);
        }
    }

    /// The colours of the marks follow the system's, so the rows show
    /// again in the new ones.
    fn apply_colors(&self, hwnd: Hwnd) {
        self.apply_menus(hwnd);
        self.show_list();
        self.send(hwnd, Event::Colours);
    }
}

/// How lines and words match, as the check boxes of `state` say.
fn matching_of(state: CompareState) -> Matching {
    Matching {
        case_sensitive: state.case_sensitive,
        ignore_spaces: state.ignore_spaces,
    }
}

/// Show only differences with its extra lines, as `state` says.
fn only_differences_of(state: CompareState) -> Option<u32> {
    state.only_differences.then_some(state.extra_lines)
}

impl WindowProc for CompareWindow {
    fn message(&self, hwnd: Hwnd, message: Message<'_>) -> Option<LRESULT> {
        match message {
            Message::Size => {
                self.layout(hwnd);
                Some(LRESULT(0))
            }
            Message::WindowPosChanged => {
                self.remember_placement(hwnd);
                None
            }
            Message::DpiChanged { suggested, .. } => {
                hwnd.set_rect(suggested);
                self.apply_fonts(hwnd);
                self.apply_menus(hwnd);
                self.layout(hwnd);
                Some(LRESULT(0))
            }
            Message::Other(SIDE_SCROLLED) => {
                self.follow_scroll();
                Some(LRESULT(0))
            }
            Message::Notify(notification) => {
                let c = self.controls()?;
                let side = Side::BOTH
                    .into_iter()
                    .find(|side| c.sides[side.index()].selection_changed(&notification))?;
                self.active.set(side);
                self.show_list();
                Some(LRESULT(0))
            }
            Message::Wake => {
                if let Some(waker) = self.waker.get() {
                    waker.woken();
                }
                self.receive(hwnd);
                Some(LRESULT(0))
            }
            Message::Deferred => {
                if let Some(shared) = self.shared.upgrade() {
                    shared.settle_sign_out();
                }
                Some(LRESULT(0))
            }
            Message::Timer(SHOW_TIMER) => {
                self.show_window(hwnd);
                Some(LRESULT(0))
            }
            Message::Timer(PROGRESS_TIMER) => {
                let (done, total) = self.progress.borrow().read();
                if let Some(c) = self.controls() {
                    c.progress.set_measured(done, total);
                }
                Some(LRESULT(0))
            }
            Message::Timer(REALIGN_TIMER) => {
                hwnd.kill_timer(REALIGN_TIMER);
                self.send(hwnd, Event::Timer);
                Some(LRESULT(0))
            }
            Message::KeyDown {
                target,
                keystroke,
                repeat,
            } => {
                let key = compare_key(keystroke, self.key_focus(target))?;
                // A held Esc would otherwise close the windows behind this
                // one too.
                let held_close = matches!(
                    key,
                    CompareKey::Cancel | CompareKey::Command(CompareCommand::Close)
                );
                if !(repeat && held_close) {
                    self.on_key(hwnd, key, target);
                }
                Some(LRESULT(1))
            }
            Message::MouseWheel { delta, .. } if window::ctrl_held() => {
                let turned = self.zoom_wheel.get() + delta;
                self.zoom_wheel.set(turned % WHEEL_DELTA);
                let steps = turned / WHEEL_DELTA;
                if steps != 0 {
                    self.set_zoom(hwnd, zoomed(self.state.get().zoom, steps));
                }
                Some(LRESULT(0))
            }
            Message::ContextMenu { from, point } => {
                let side = self.controls()?.side_of(from)?;
                self.active.set(side);
                from.focus();
                self.context_menu(hwnd, point);
                Some(LRESULT(0))
            }
            Message::DropFiles { files, point } => {
                self.drop_files(hwnd, files, point);
                Some(LRESULT(0))
            }
            Message::InitMenuPopup => {
                let states = menu_states(self.menu_state());
                if let Some(menu) = Menu::of(hwnd) {
                    for (id, enabled, checked) in states {
                        menu.set_state(id, enabled, checked);
                    }
                }
                Some(LRESULT(0))
            }
            Message::Command(Command::Menu(id)) => {
                self.on_command(hwnd, CompareCommand::from_id(id)?);
                Some(LRESULT(0))
            }
            Message::Command(Command::Control(event)) => {
                self.on_control(hwnd, event);
                Some(LRESULT(0))
            }
            Message::Deactivate => {
                self.focused
                    .set(Hwnd::focused().filter(|focused| hwnd.is_parent_of(*focused)));
                None
            }
            Message::Activate { .. } => {
                let side = self.controls()?.sides[self.active.get().index()].hwnd();
                let focus = self
                    .focused
                    .take()
                    .filter(|focused| focused.is_visible())
                    .unwrap_or(side);
                focus.focus();
                Some(LRESULT(0))
            }
            Message::SysColorChange => {
                self.apply_colors(hwnd);
                None
            }
            Message::SettingChange {
                regional_format: true,
            } => {
                self.numbers.replace(WindowsNumbers::new());
                self.show_status();
                None
            }
            // Every top-level window is asked, in the main window's process
            // and in that of `--compare` alike.
            Message::QueryEndSession => {
                let hold = (self.shared.upgrade()).is_some_and(|shared| shared.hold_sign_out());
                Some(LRESULT(isize::from(!hold)))
            }
            Message::EndSession { ending: false } => {
                if let Some(shared) = self.shared.upgrade() {
                    shared.release_sign_out(true);
                }
                None
            }
            Message::Close => {
                self.close(hwnd);
                Some(LRESULT(0))
            }
            Message::Destroy => {
                self.remember_placement(hwnd);
                self.hwnd.set(None);
                for timer in [SHOW_TIMER, PROGRESS_TIMER, REALIGN_TIMER] {
                    hwnd.kill_timer(timer);
                }
                if let Some(waker) = self.waker.get() {
                    waker.close();
                }
                let mut retired = self.session.borrow_mut().close();
                if let Some(work) = self.work.replace(None) {
                    retired.append(work.close());
                }
                retire(retired);
                window::own_keys(hwnd, false);
                if let Some(shared) = self.shared.upgrade() {
                    shared
                        .open
                        .borrow_mut()
                        .retain(|each| !std::ptr::eq(Rc::as_ptr(each), self));
                    shared.release_sign_out(false);
                    if shared.alone && shared.open.borrow().is_empty() {
                        drop(window::quit_after(|_| {}));
                    }
                    shared.resume_exit(self.main);
                }
                None
            }
            _ => None,
        }
    }
}
