//! The compare windows (DESIGN.md: Compare by content): each Compare by
//! content opens one, a top-level window that no window owns, which
//! compares two files on its worker and shows them in RichEdit controls.
//! `core::compare` and `core::compare_window` decide what shows, and a
//! [`Session`] holds each window's state; this module passes the session
//! what happens, runs the effects it returns, and gives the controls what
//! the worker of [`super::work::compare`] built.

use super::dialogs::ViewerFind;
use super::work::CopyNotice;
pub(super) use super::work::compare::Source;
use super::work::compare::{
    Find, Found, Job, Look, Options, Outcome, Progress, Rendering, Saved, Searched, Unsaved, retire,
};
use crate::core::captions::{DialogCaptions, button_glyph};
use crate::core::clipboard::folder_of;
use crate::core::compare::rtf::{self, Rtf, Style};
use crate::core::compare::{self, Chosen, Matching, Needle};
use crate::core::compare_window::session::{
    Answer, Caret, Effect, Event, Fresh, Request, RowOptions, SaveFile, SavedFile, Session, Settled,
};
use crate::core::compare_window::{
    CompareCommand, CompareKey, CompareSizes, DEFAULT_ZOOM, KeyFocus, MenuState, Note,
    command_state, compare_key, compare_layout, context_menu, differences_text, encoding_menu,
    encodings_enabled, encodings_text, menu_states, note_text, number_row, title, zoomed,
};
use crate::core::ids::{ControlId, DialogId, IconId};
use crate::core::layout::{Dpi, Px, Rect, Size, WHEEL_DELTA, image_button_width};
use crate::core::panel::Side;
use crate::core::settings::{CompareState, Placement};
use crate::core::text::{self, Numbers as _, Strings};
use crate::core::viewer::Mode;
use crate::core::viewer::find::Query;
use crate::win::compare::{RichText, SIDE_SCROLLED, palette, rtf_font};
use crate::win::controls::{
    Button, CheckBox, Edit, Label, ProgressBar, StatusBar, Tooltip, set_accessible_name,
};
use crate::win::dialog::{DialogEnd, choose, error_box, info_box, run_modal};
use crate::win::gdi::{Font, TextMeter};
use crate::win::locale::WindowsNumbers;
use crate::win::menu::Menu;
use crate::win::notify::Command;
use crate::win::shell::{FileType, accept_files, pick_file, save_dialog};
use crate::win::viewer::caption_height;
use crate::win::wake::OpenWaker;
use crate::win::window::{self, Message, WindowProc};
use crate::win::{self, Hwnd};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::os::windows::ffi::OsStrExt;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::LRESULT;

const WINDOW_CLASS: &str = "gozler.ui.compare";
/// Shows the window once its comparison has run this long, so identical
/// files that compare faster show only their message.
const SHOW_TIMER: usize = 1;
const SHOW_DELAY: u32 = crate::core::work::BUSY.as_millis() as u32;
const PROGRESS_TIMER: usize = 2;
/// Aligns the rows again once the person has stopped typing this long.
const REALIGN_TIMER: usize = 3;
const REALIGN_DELAY: u32 = 300;
/// Tries the clipboard again while another program holds it.
const CLIPBOARD_TIMER: usize = 4;
/// Streams the next slices of rows into the sides, after the messages that
/// wait, such as input and painting.
const LOAD_TIMER: usize = 5;
/// How long one message streams slices of rows before the window answers
/// again.
const LOAD_BUDGET: Duration = Duration::from_millis(30);
/// The rows above a difference that going to it keeps in view.
const CONTEXT_ROWS: usize = 3;
/// The digits of the extra lines box: those of [`compare::MAX_EXTRA_LINES`].
const EXTRA_DIGITS: usize = compare::MAX_EXTRA_LINES.ilog10() as usize + 1;
/// The width and height, in DIPs, of a window when none was moved or closed
/// before.
const DEFAULT_SIZE: (i32, i32) = (1040, 600);

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

/// What shows of the comparison that runs, so a quick one, such as after
/// a check box, shows only its result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Running {
    /// No comparison runs, or it has run less than
    /// [`crate::core::work::BUSY`].
    Hidden,
    /// The progress bar and "Comparing…".
    Progress,
    /// Cancel too, once the comparison has run [`crate::core::work::SLOW`].
    Cancel,
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

    /// Shows or hides the icons of every window's buttons.
    pub(super) fn apply_glyphs(&self) {
        for comparison in self.opened() {
            comparison.apply_glyphs();
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

/// What a path box holds: its text without the spaces around it, and
/// without the quotes that File Explorer's Copy as path puts around it.
fn box_path(text: &str) -> &str {
    let text = text.trim();
    (text.strip_prefix('"'))
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
}

/// What the window says in the sides' place while no comparison shows; in
/// the status bar, while rows show, why they could not be built again.
enum Notice {
    /// `name`, on `side`, could not be read, for Windows' `reason`.
    Failed {
        side: Side,
        name: String,
        reason: String,
    },
    Cancelled,
    /// One file was found, `name`, and the person chooses the other.
    ChooseSecond {
        name: String,
    },
}

impl Notice {
    fn text(&self, strings: &Strings) -> String {
        match self {
            Notice::Failed { name, reason, .. } => {
                strings.format("viewer.cannot_read", &[name, reason])
            }
            Notice::Cancelled => strings.text("work.cancelled").to_owned(),
            Notice::ChooseSecond { name } => strings.format("compare.choose_second", &[name]),
        }
    }
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

/// Rows that still stream into the sides, some at each turn of the message
/// loop, so a large comparison never holds the window.
struct Loading {
    /// Each side's slices still to stream. Only a patch of all the rows
    /// has more than one, so each one goes below all the rows the side
    /// holds and replaces its last row.
    slices: [VecDeque<String>; 2],
    /// Each side whose patch replaced all its rows.
    whole: [bool; 2],
    /// The sides took patches, whose rows are checked once all are in.
    patched: bool,
    /// Where the caret goes once the rows around it are in, unless the
    /// person moves it first.
    place: Option<Place>,
    /// The commands that read the rows, in order, which wait until all are
    /// in and then go to the session again, as if given then.
    commands: VecDeque<CompareCommand>,
}

/// A window's controls, created in Tab order.
struct Controls {
    paths: [Edit; 2],
    browse: [Button; 2],
    compare: Button,
    next: Button,
    previous: Button,
    find: Button,
    find_next: Button,
    /// Binary, Case sensitive, Ignore repeated spaces and Ignore frequent
    /// lines.
    options: [CheckBox; 4],
    edit_mode: CheckBox,
    only: CheckBox,
    extra_label: Label,
    extra: Edit,
    above: CheckBox,
    save: Button,
    /// Each side's encoding, which opens the encodings' menu.
    encodings: Button,
    cancel: Button,
    progress: ProgressBar,
    /// Why no comparison shows, in the sides' place.
    message: Label,
    /// Each side's text and its line numbers. Swap sides swaps the
    /// controls, which keep their text, so nothing streams again.
    sides: Cell<[RichText; 2]>,
    numbers: Cell<[RichText; 2]>,
    list: RichText,
    status: StatusBar,
    /// The buttons' tooltips, which name their commands' keys.
    tips: Tooltip,
}

impl Controls {
    fn create(hwnd: Hwnd) -> windows::core::Result<Controls> {
        let left_path = Edit::create(hwnd, ControlId::CompareLeftPath.id())?;
        let left_browse = Button::create(hwnd, ControlId::CompareLeftBrowse.id())?;
        let right_path = Edit::create(hwnd, ControlId::CompareRightPath.id())?;
        let right_browse = Button::create(hwnd, ControlId::CompareRightBrowse.id())?;
        let compare = Button::create(hwnd, ControlId::CompareRun.id())?;
        let next = Button::create(hwnd, ControlId::CompareNext.id())?;
        let previous = Button::create(hwnd, ControlId::ComparePrevious.id())?;
        let find = Button::create(hwnd, ControlId::CompareFind.id())?;
        let find_next = Button::create(hwnd, ControlId::CompareFindNext.id())?;
        let options = [
            CheckBox::create(hwnd, ControlId::CompareBinary.id())?,
            CheckBox::create(hwnd, ControlId::CompareCase.id())?,
            CheckBox::create(hwnd, ControlId::CompareSpaces.id())?,
            CheckBox::create(hwnd, ControlId::CompareFrequent.id())?,
        ];
        let edit_mode = CheckBox::create(hwnd, ControlId::CompareEditMode.id())?;
        let only = CheckBox::create(hwnd, ControlId::CompareOnly.id())?;
        let extra_label = Label::create(hwnd, ControlId::CompareExtraLabel.id())?;
        let extra = Edit::create_number(hwnd, ControlId::CompareExtra.id(), EXTRA_DIGITS)?;
        let above = CheckBox::create(hwnd, ControlId::CompareAbove.id())?;
        let save = Button::create(hwnd, ControlId::CompareSave.id())?;
        let encodings = Button::create(hwnd, ControlId::CompareEncodings.id())?;
        let cancel = Button::create(hwnd, ControlId::CompareCancel.id())?;
        let progress = ProgressBar::create(hwnd, ControlId::CompareProgress.id())?;
        let message = Label::create_centred(hwnd, ControlId::CompareMessage.id())?;
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
        let tips = Tooltip::create(
            hwnd,
            &[
                next.hwnd(),
                previous.hwnd(),
                find.hwnd(),
                find_next.hwnd(),
                save.hwnd(),
                encodings.hwnd(),
            ],
        )?;
        Ok(Controls {
            paths: [left_path, right_path],
            browse: [left_browse, right_browse],
            compare,
            next,
            previous,
            find,
            find_next,
            options,
            edit_mode,
            only,
            extra_label,
            extra,
            above,
            save,
            encodings,
            cancel,
            progress,
            message,
            sides: Cell::new(sides),
            numbers: Cell::new(numbers),
            list,
            status,
            tips,
        })
    }

    fn sides(&self) -> [RichText; 2] {
        self.sides.get()
    }

    fn numbers(&self) -> [RichText; 2] {
        self.numbers.get()
    }

    /// The check boxes, the extra lines' label and the message, which show
    /// on the window's background.
    fn on_background(&self) -> [Hwnd; 9] {
        [
            self.options[0].hwnd(),
            self.options[1].hwnd(),
            self.options[2].hwnd(),
            self.options[3].hwnd(),
            self.edit_mode.hwnd(),
            self.only.hwnd(),
            self.above.hwnd(),
            self.extra_label.hwnd(),
            self.message.hwnd(),
        ]
    }

    fn rich(&self) -> [RichText; 5] {
        [
            self.sides()[0],
            self.sides()[1],
            self.numbers()[0],
            self.numbers()[1],
            self.list,
        ]
    }

    /// The side that `hwnd` is.
    fn side_of(&self, hwnd: Hwnd) -> Option<Side> {
        Side::BOTH
            .into_iter()
            .find(|side| self.sides()[side.index()].hwnd() == hwnd)
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
    notice: RefCell<Option<Notice>>,
    running: Cell<Running>,
    /// When the comparison that runs started, which one that replaces it
    /// keeps; `None` once none runs (`refresh`).
    compare_started: Cell<Option<Instant>>,
    /// The notice shows in the sides' place, because no comparison shows.
    message_shown: Cell<bool>,
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
    /// The fingerprints of the rows that each side's control shows, which
    /// the worker's rows patch (`rtf::patch`).
    prints: RefCell<[Arc<Vec<u64>>; 2]>,
    /// The rows of each side whose line numbers show the rows of now. The
    /// line numbers show only around the rows in view, because inserting a
    /// line numbers every row below it again.
    numbered: Cell<[(usize, usize); 2]>,
    /// The patches of the worker's rows, until the session shows them.
    rendering: RefCell<Option<Rendering>>,
    loading: RefCell<Option<Loading>>,
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
            notice: RefCell::new(None),
            running: Cell::new(Running::Hidden),
            compare_started: Cell::new(None),
            message_shown: Cell::new(false),
            settled: Cell::new(None),
            scroll: Cell::new([(0, 0); 2]),
            zoom_wheel: Cell::new(0),
            focused: Cell::new(None),
            active: Cell::new(Side::Left),
            prints: RefCell::new([Arc::default(), Arc::default()]),
            numbered: Cell::new([(0, 0); 2]),
            rendering: RefCell::new(None),
            loading: RefCell::new(None),
        }
    }

    /// Everything the new window needs before it compares: its keys, its
    /// size, controls and texts, and the first job of its worker.
    fn start(&self, hwnd: Hwnd, source: Source, tool: Option<String>) -> windows::core::Result<()> {
        self.hwnd.set(Some(hwnd));
        if self.placement.get().is_none() {
            // Windows' default size fills most of a large screen, while two
            // files read side by side in less, as in Total Commander.
            let at = hwnd.screen_rect();
            let size =
                |dip: i32| Px(dip).rescale(Dpi::new(96).expect("96 is not zero"), hwnd.dpi());
            hwnd.set_rect(Rect {
                width: size(DEFAULT_SIZE.0),
                height: size(DEFAULT_SIZE.1),
                ..at
            });
        }
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
                match source {
                    Source::Files(paths) => self.send(hwnd, Event::Compare(paths)),
                    chosen @ Source::Chosen { .. } => self.start_job(Job::Choose(chosen)),
                }
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
                Effect::Run(command, fresh) => self.run_command(hwnd, command, fresh),
                Effect::Swapped => {
                    self.swap_boxes();
                    self.swap_sides(hwnd);
                }
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
                        .map(|showing| showing.paths()[side.index()].clone());
                    if let Some(path) = path {
                        self.set_path(side, &path);
                    }
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
                        c.only.set_checked(state.only_differences);
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

    /// The parts of the window that follow the session and the saved
    /// state: the progress, the message, the sides that take typing, the
    /// controls and the menus' commands, the title and the status bar.
    fn refresh(&self, hwnd: Hwnd) {
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let (comparing, shows) = {
            let session = self.session.borrow();
            (session.comparing(), session.showing().is_some())
        };
        let message = !shows && self.notice.borrow().is_some();
        let ran = (self.compare_started.get())
            .filter(|_| comparing)
            .map(|started| started.elapsed());
        let running = match ran {
            Some(ran) if ran >= crate::core::work::SLOW => Running::Cancel,
            Some(ran) if ran >= crate::core::work::BUSY => Running::Progress,
            _ => Running::Hidden,
        };
        let progress_changed = self.running.replace(running) != running;
        if !comparing {
            hwnd.kill_timer(PROGRESS_TIMER);
            self.compare_started.set(None);
        }
        let message_changed = self.message_shown.replace(message) != message;
        self.show_message();
        let encodings_changed = self.apply_encodings();
        if progress_changed || message_changed || encodings_changed {
            self.layout(hwnd);
        }
        self.apply_read_only();
        self.apply_controls();
        self.apply_title(hwnd);
        self.show_status();
    }

    /// The controls that show the session and the saved state: the extra
    /// lines, which apply while Show only differences is on, and the
    /// commands.
    fn apply_controls(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let only = self.state.get().only_differences;
        c.extra_label.hwnd().set_enabled(only);
        c.extra.hwnd().set_enabled(only);
        self.apply_commands();
    }

    /// The buttons and the check boxes of commands, as their commands are;
    /// Binary is Hex while checked and Text while not.
    fn apply_commands(&self) {
        use CompareCommand as C;
        let Some(c) = self.controls() else {
            return;
        };
        let state = self.menu_state();
        let enabled = |command| command_state(state, command).0;
        for (button, command) in [
            (c.compare, C::Compare),
            (c.next, C::NextDifference),
            (c.previous, C::PreviousDifference),
            (c.find, C::Find),
            (c.find_next, C::FindNext),
            (c.save, C::Save),
        ] {
            button.hwnd().set_enabled(enabled(command));
        }
        c.encodings.hwnd().set_enabled(encodings_enabled(state));
        // Windows checks a box at the click, before the session decides.
        let hex = command_state(state, C::Hex).1;
        let binary = if hex { C::Text } else { C::Hex };
        let checks = [
            (c.options[0], (enabled(binary), hex)),
            (c.edit_mode, command_state(state, C::EditMode)),
            (c.only, command_state(state, C::OnlyDifferences)),
            (c.above, command_state(state, C::AboveEachOther)),
        ];
        for (check, (enabled, checked)) in checks {
            check.set_checked(checked);
            check.hwnd().set_enabled(enabled);
        }
        // A disabled button takes no keys, so the focus goes on in the side.
        let focused = Hwnd::focused();
        if [
            c.next.hwnd(),
            c.previous.hwnd(),
            c.find.hwnd(),
            c.find_next.hwnd(),
            c.save.hwnd(),
            c.encodings.hwnd(),
            c.options[0].hwnd(),
            c.edit_mode.hwnd(),
        ]
        .into_iter()
        .any(|button| Some(button) == focused && !button.is_enabled())
            && let Some(target) = self.focus_target()
        {
            target.focus();
        }
    }

    /// The encodings button's caption, each side's encoding; `true` when it
    /// changed, which changes the button's width.
    fn apply_encodings(&self) -> bool {
        let Some(c) = self.controls() else {
            return false;
        };
        let encodings = (self.session.borrow().showing()).map(|showing| showing.encodings());
        let caption = encodings_text(&self.strings.borrow(), encodings);
        let changed = c.encodings.hwnd().text() != caption;
        if changed {
            c.encodings.hwnd().set_text(&caption);
        }
        changed
    }

    /// Where the focus goes when the window shows or is activated: the side
    /// the person works in while a comparison shows; else the path box of
    /// the file that could not be read, or the first empty one.
    fn focus_target(&self) -> Option<Hwnd> {
        let c = self.controls()?;
        if self.session.borrow().showing().is_some() {
            return Some(c.sides()[self.active.get().index()].hwnd());
        }
        let side = match self.notice.borrow().as_ref() {
            Some(Notice::Failed { side, .. }) => *side,
            _ => Side::BOTH
                .into_iter()
                .find(|side| box_path(&c.paths[side.index()].text()).is_empty())
                .unwrap_or(Side::Left),
        };
        Some(c.paths[side.index()].hwnd())
    }

    /// The message in the sides' place, while no comparison shows.
    fn show_message(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let text = (self.notice.borrow().as_ref())
            .map(|notice| notice.text(&self.strings.borrow()))
            .unwrap_or_default();
        c.message.hwnd().set_text(&text);
    }

    /// Puts `path` in `side`'s path box, scrolled to its end, so the box
    /// shows the file's name.
    fn set_path(&self, side: Side, path: &[u16]) {
        let Some(c) = self.controls() else {
            return;
        };
        let path_box = c.paths[side.index()];
        path_box.hwnd().set_text(&String::from_utf16_lossy(path));
        path_box.select(path.len(), path.len());
    }

    /// The two files that the path boxes name, or `None` while one is
    /// empty.
    fn box_paths(&self) -> Option<[Vec<u16>; 2]> {
        let c = self.controls()?;
        let paths = c.paths.map(|path_box| {
            box_path(&path_box.text())
                .encode_utf16()
                .collect::<Vec<u16>>()
        });
        paths.iter().all(|path| !path.is_empty()).then_some(paths)
    }

    /// Asks Save's question for `event`, Close or Exit; where it left the
    /// window.
    fn settle(&self, hwnd: Hwnd, event: Event) -> Settled {
        self.settled.set(None);
        self.send(hwnd, event);
        // A window that closed ran its Close before it could say so.
        self.settled.take().unwrap_or(Settled::Done)
    }

    /// How the worker builds the rows of `rows`, as patches of the rows
    /// that the sides show now.
    fn options(&self, rows: RowOptions) -> Options {
        Options {
            matching: rows.matching,
            only_differences: rows.only_differences,
            look: self.look(),
            shown: self.prints.borrow().clone(),
        }
    }

    fn look(&self) -> Look {
        Look {
            palette: palette(),
            font: rtf_font(),
            strings: self.strings.borrow().clone(),
            numbers: self.numbers.borrow().clone(),
        }
    }

    /// Reads and compares the files of `request`, with the progress.
    fn start_compare(&self, hwnd: Hwnd, request: Request) {
        // A comparison that fails or is cancelled still shows which files
        // it read, so Compare can try them again.
        for side in Side::BOTH {
            self.set_path(side, &request.paths[side.index()]);
        }
        let progress = Arc::new(Progress::default());
        self.progress.replace(Arc::clone(&progress));
        self.start_job(Job::Compare {
            paths: request.paths,
            hex: request.hex,
            encodings: request.encodings,
            options: self.options(request.rows),
            progress,
        });
        // A comparison that replaces one that runs keeps its start, so the
        // progress that shows already stays; `refresh` clears the start
        // once none runs.
        if self.compare_started.get().is_none() {
            self.compare_started.set(Some(Instant::now()));
        }
        hwnd.set_timer(PROGRESS_TIMER, 100);
    }

    /// Esc: stops the comparison that runs; `false` when none runs.
    fn cancel_compare(&self, hwnd: Hwnd) -> bool {
        if !self.session.borrow().comparing() {
            return false;
        }
        // The window stays, so a later Compare of identical files keeps it.
        self.stage.set(Stage::Shown);
        let shown = (self.session.borrow().showing()).map(|showing| showing.paths().clone());
        match shown {
            // The window returns to the files that show.
            Some(paths) => {
                for side in Side::BOTH {
                    self.set_path(side, &paths[side.index()]);
                }
            }
            None => {
                self.notice.replace(Some(Notice::Cancelled));
            }
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
        self.scroll_paths(Side::BOTH);
        if let Some(target) = self.focus_target() {
            target.focus();
        }
    }

    /// Scrolls `sides`' path boxes back to their carets, which show the
    /// file's name after a path that the window put there. A text box that
    /// was hidden or changed size shows the start of its text.
    fn scroll_paths(&self, sides: impl IntoIterator<Item = Side>) {
        let Some(c) = self.controls() else {
            return;
        };
        for side in sides {
            let path_box = c.paths[side.index()];
            let (start, end) = path_box.selection();
            path_box.select(start, end);
        }
    }

    /// Puts the focus in `side`'s path box with its text selected, so the
    /// person can correct the file that could not be read.
    fn select_box(&self, side: Side) {
        if let Some(c) = self.controls() {
            let path_box = c.paths[side.index()];
            path_box.hwnd().focus();
            path_box.select_to_end();
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
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let found = (self.work.borrow_mut().as_mut()).and_then(|work| work.take_found());
        if let Some(found) = found {
            self.on_found(hwnd, found);
        }
        let copied = self
            .work
            .borrow_mut()
            .as_mut()
            .and_then(|work| work.clipboard.take());
        match copied {
            Some(CopyNotice::Retry) => hwnd.set_timer(
                CLIPBOARD_TIMER,
                crate::core::clipboard::RETRY_AFTER.as_millis() as u32,
            ),
            Some(CopyNotice::Failed(_, failure, _)) => {
                let (text, title) = {
                    let strings = self.strings.borrow();
                    (
                        failure.message(&strings),
                        strings.text("compare.window").to_owned(),
                    )
                };
                error_box(self.main.or(Some(hwnd)), &text, &title);
            }
            Some(CopyNotice::Finished) | None => {}
        }
    }

    /// Puts the text that a box selected on the clipboard; nothing happens
    /// while none is selected. The window only reads the selection.
    fn copy_text(&self, text: Option<String>) {
        let Some(text) = text else { return };
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.clipboard.copy_text(text);
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
            Outcome::Chosen(chosen) => match (chosen, self.stage.get()) {
                (Chosen::Both(paths), Stage::First) => self.send(hwnd, Event::Compare(paths)),
                // The window keeps the one file while the person chooses
                // the other.
                (Chosen::First(path), Stage::First) => {
                    self.stage.set(Stage::Shown);
                    self.set_path(Side::Left, &path);
                    let name = name_of(&path);
                    self.notice.replace(Some(Notice::ChooseSecond { name }));
                    self.refresh(hwnd);
                    self.show_window(hwnd);
                }
                // The comparison tool needs two files.
                _ => {
                    let text = self.strings.borrow().text("compare.choose_two").to_owned();
                    hwnd.kill_timer(SHOW_TIMER);
                    info_box(hwnd, &text, &title);
                    hwnd.destroy();
                }
            },
            Outcome::Failed { path, error } => {
                let (comparing, shows) = {
                    let session = self.session.borrow();
                    (session.comparing(), session.showing().is_some())
                };
                let side = self.controls().and_then(|c| {
                    Side::BOTH.into_iter().find(|side| {
                        let text = c.paths[side.index()].text();
                        box_path(&text).encode_utf16().eq(path.iter().copied())
                    })
                });
                let notice = Notice::Failed {
                    side: side.unwrap_or(Side::Left),
                    name: name_of(&path),
                    reason: win::error_text(&error),
                };
                // The window stays, so a later Compare of identical files
                // keeps it.
                self.stage.set(Stage::Shown);
                if comparing && shows {
                    // The person stays with the comparison that shows, and
                    // is told why.
                    let text = notice.text(&self.strings.borrow());
                    self.send(hwnd, Event::Failed);
                    error_box(Some(hwnd), &text, &title);
                } else {
                    // No comparison shows, or rows of the one that shows
                    // could not be built again, which the status bar says.
                    self.notice.replace(Some(notice));
                    self.send(hwnd, Event::Failed);
                    self.show_window(hwnd);
                }
                if comparing && let Some(side) = side {
                    self.select_box(side);
                }
            }
            // Only a job that was cancelled or replaced returns it, and the
            // worker drops the outcome of such a job.
            Outcome::Cancelled => {}
            Outcome::Compared(compared, rendering) => {
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
                for side in Side::BOTH {
                    self.set_path(side, &compared.paths[side.index()]);
                }
                self.notice.replace(None);
                // The session shows the rows, or drops them as stale, and
                // the patches go with them.
                self.rendering.replace(Some(rendering));
                self.send(hwnd, Event::Compared(compared));
                self.rendering.replace(None);
                self.show_window(hwnd);
            }
            Outcome::Realigned {
                generation,
                view,
                rendering,
            } => {
                // Rows built again replace a rebuild that failed before.
                self.notice.replace(None);
                let place = self.capture_place();
                self.rendering.replace(Some(rendering));
                self.send_at(hwnd, Event::Realigned { generation, view }, place);
                self.rendering.replace(None);
            }
        }
    }

    /// Streams the first slice of each of the worker's patches into its
    /// side, and the rest through [`Self::load`], which numbers the rows in
    /// view and places the caret.
    fn present(&self, hwnd: Hwnd, place: Place) {
        let Some(c) = self.controls() else {
            return;
        };
        if self.session.borrow().showing().is_none() {
            return;
        }
        hwnd.kill_timer(LOAD_TIMER);
        // A side that the worker gives no new rows goes on with the slices
        // of an earlier load, which are still its rows.
        let mut loading = match self.loading.take() {
            Some(earlier) => Loading {
                place: Some(place),
                ..earlier
            },
            None => Loading {
                slices: [VecDeque::new(), VecDeque::new()],
                whole: [false; 2],
                patched: false,
                place: Some(place),
                commands: VecDeque::new(),
            },
        };
        let mut unknown = [false; 2];
        // Swap sides gives no patches: the controls keep their rows.
        if let Some(Rendering { patches, prints }) = self.rendering.take() {
            loading.patched = true;
            for (side, patch) in Side::BOTH.into_iter().zip(patches) {
                let Some(patch) = patch else {
                    continue;
                };
                let index = side.index();
                let control = c.sides()[index];
                let mut slices = VecDeque::from(patch.slices);
                let first = slices.pop_front().expect("a patch holds a row or more");
                if patch.whole {
                    control.set_rtf(&first);
                    loading.slices[index] = slices;
                    loading.whole[index] = true;
                } else if loading.slices[index].is_empty() {
                    // A patch that keeps rows is one slice.
                    control.replace_rows(patch.rows, &first);
                    loading.whole[index] = false;
                } else {
                    // It changes rows that have not streamed in yet, so the
                    // side streams again whole.
                    loading.slices[index].clear();
                    unknown[index] = true;
                }
            }
            self.prints.replace(prints.map(Arc::new));
        }
        for side in Side::BOTH {
            if unknown[side.index()] {
                self.forget_prints(side);
            }
        }
        let streaming = loading.slices.iter().any(|slices| !slices.is_empty());
        self.loading.replace(Some(loading));
        if unknown.contains(&true) {
            self.send(hwnd, Event::Outdated);
            if self.hwnd.get() != Some(hwnd) {
                return;
            }
        }
        if !streaming {
            self.load(hwnd);
            return;
        }
        self.show_loaded();
        hwnd.set_timer(LOAD_TIMER, 1);
        // The line numbers take the width of the rows that will show.
        self.layout(hwnd);
    }

    /// Streams the slices of rows that wait, the side with fewer rows
    /// first, for [`LOAD_BUDGET`], and the rest after the messages that
    /// wait; once all are in, ends [`Self::present`], and gives `true`.
    fn load(&self, hwnd: Hwnd) -> bool {
        let Some(c) = self.controls() else {
            return false;
        };
        let started = Instant::now();
        loop {
            let next = {
                let mut loading = self.loading.borrow_mut();
                let Some(loading) = loading.as_mut() else {
                    hwnd.kill_timer(LOAD_TIMER);
                    return false;
                };
                let side = (Side::BOTH.into_iter())
                    .filter(|side| !loading.slices[side.index()].is_empty())
                    .min_by_key(|side| c.sides()[side.index()].row_count());
                side.and_then(|side| Some((side, loading.slices[side.index()].pop_front()?)))
            };
            let Some((side, slice)) = next else {
                break;
            };
            c.sides()[side.index()].extend(&slice);
            if started.elapsed() >= LOAD_BUDGET {
                break;
            }
        }
        let done = (self.loading.borrow().as_ref())
            .is_some_and(|loading| loading.slices.iter().all(VecDeque::is_empty));
        if !done {
            self.show_loaded();
            hwnd.set_timer(LOAD_TIMER, 1);
            return false;
        }
        hwnd.kill_timer(LOAD_TIMER);
        if let Some(loading) = self.loading.take() {
            self.end_present(hwnd, loading);
        }
        true
    }

    /// The line numbers follow the rows that are in while more stream in,
    /// and the caret goes to its place once the rows around it are in.
    fn show_loaded(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let held = c.sides().map(RichText::row_count);
        self.size_numbers(held[0].max(held[1]));
        self.numbered.set([(0, 0); 2]);
        let page = self.page_rows();
        let place = (self.loading.borrow_mut().as_mut()).and_then(|loading| {
            let row = self.place_row(loading.place.as_ref()?);
            if row + page < held[0].min(held[1]) {
                loading.place.take()
            } else {
                None
            }
        });
        match place {
            Some(place) => self.place(place),
            None => self.sync_from(self.active.get()),
        }
    }

    /// Ends [`Self::present`] once all the rows are in: numbers the rows in
    /// view, places the caret, and gives the session the commands that
    /// waited for them.
    fn end_present(&self, hwnd: Hwnd, loading: Loading) {
        let Some(c) = self.controls() else {
            return;
        };
        let rows = match self.session.borrow().showing() {
            Some(showing) => showing.view().rows.rows.len().max(1),
            None => return,
        };
        let mut outdated = false;
        for side in Side::BOTH {
            let index = side.index();
            // A control that took a patch otherwise than it was built
            // for holds other rows, so its rows stream again whole. A whole
            // stream that left other rows, as a form feed in a row does,
            // would leave them again, so it sends no Outdated, which would
            // stream it again without end.
            let built = self.prints.borrow()[index].len().max(1);
            if loading.patched && c.sides()[index].row_count() != built {
                self.forget_prints(side);
                outdated |= !loading.whole[index];
            }
        }
        if outdated {
            for side in Side::BOTH {
                self.forget_prints(side);
            }
        }
        self.numbered.set([(0, 0); 2]);
        self.size_numbers(rows);
        self.layout(hwnd);
        if let Some(place) = loading.place {
            self.place(place);
        }
        self.show_list();
        if outdated {
            self.send(hwnd, Event::Outdated);
        }
        // A command that changes the lines makes the next one wait for
        // fresh rows, as when the person gives them one after the other.
        for command in loading.commands {
            if self.hwnd.get() != Some(hwnd) {
                return;
            }
            self.send(hwnd, Event::Command(command));
        }
    }

    /// `side`'s control holds rows that nobody knows, so the next rows
    /// stream into it whole.
    fn forget_prints(&self, side: Side) {
        let Some(c) = self.controls() else {
            return;
        };
        let rows = c.sides()[side.index()].row_count();
        self.prints.borrow_mut()[side.index()] = Arc::new(vec![rtf::UNKNOWN; rows]);
    }

    /// Gives each side's line numbers `rows` rows, as many as the side
    /// shows, blank until [`Self::number_rows`] numbers the rows in view.
    fn size_numbers(&self, rows: usize) {
        let Some(c) = self.controls() else {
            return;
        };
        let (palette, font) = (palette(), rtf_font());
        for numbers in c.numbers() {
            let held = numbers.row_count();
            if held == rows {
                continue;
            }
            // The last row that stays, and those after it, give way to
            // blank rows, because a patch replaces a row or more.
            let kept = held.min(rows);
            let mut blank = Rtf::new(&palette, font);
            for _ in kept - 1..rows {
                blank.row("", Style::Muted, &[]);
            }
            numbers.replace_rows(kept - 1..held, &blank.finish());
        }
    }

    /// Numbers the rows of each side from a page above those in view to a
    /// page below them, unless their line numbers show already.
    fn number_rows(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let page = self.page_rows();
        let mut numbered = self.numbered.get();
        for side in Side::BOTH {
            let first = c.sides()[side.index()].first_visible_row();
            let (start, end) = numbered[side.index()];
            if start <= first && first + page <= end {
                continue;
            }
            // While rows stream in, the line numbers hold only as many.
            let held = c.numbers()[side.index()].row_count();
            let numbers = {
                let session = self.session.borrow();
                let Some(showing) = session.showing() else {
                    return;
                };
                let rows = &showing.view().rows.rows;
                let from = first.saturating_sub(page).min(rows.len());
                let to = (first + 2 * page).min(rows.len()).min(held);
                let digits = (showing.mode() == Mode::Text).then(|| {
                    let lines = showing.text(side).map_or(0, |text| text.lines.len());
                    lines.max(1).to_string().len()
                });
                let mut rtf = Rtf::new(&palette(), rtf_font());
                for row in &rows[from..to] {
                    let (text, style) = number_row(row, side, digits);
                    rtf.row(&text, style, &[]);
                }
                (from < to).then(|| (from..to, rtf.finish()))
            };
            if let Some((rows, rtf)) = numbers {
                c.numbers()[side.index()].replace_rows(rows.clone(), &rtf);
                numbered[side.index()] = (rows.start, rows.end);
            }
        }
        self.numbered.set(numbered);
    }

    /// The rows that a side shows at a time, and one more.
    fn page_rows(&self) -> usize {
        let Some(c) = self.controls() else {
            return 1;
        };
        let fonts = self.fonts.borrow();
        let height = c.sides()[0].hwnd().client_size().height.0;
        let row = fonts.as_ref().map_or(16, |fonts| fonts.row_height.0.max(1));
        usize::try_from(height / row).unwrap_or(0) + 1
    }

    /// Where the caret is, which [`Self::place`] restores.
    fn capture_place(&self) -> Option<Place> {
        let c = self.controls()?;
        let side = self.active.get();
        let control = c.sides()[side.index()];
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

    /// The row of the rows that show where `place` puts the caret.
    fn place_row(&self, place: &Place) -> usize {
        let session = self.session.borrow();
        let Some(rows) = session.showing().map(|showing| &showing.view().rows) else {
            return 0;
        };
        match place {
            Place::FirstDifference => rows.differences().first().map_or(0, |range| range.start),
            Place::Line {
                side, line, row, ..
            } => {
                let found = line.and_then(|line| {
                    rows.rows
                        .iter()
                        .position(|each| each.line(*side) == Some(line))
                });
                found.unwrap_or(*row).min(rows.rows.len().saturating_sub(1))
            }
        }
    }

    fn place(&self, place: Place) {
        let Some(c) = self.controls() else {
            return;
        };
        let new_row = self.place_row(&place);
        match place {
            Place::FirstDifference => self.go_to_row(new_row),
            Place::Line {
                side,
                row,
                column,
                first_visible,
                x,
                ..
            } => {
                let control = c.sides()[side.index()];
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
            let control = c.sides()[side.index()];
            if let Some(start) = control.row_start(row) {
                control.select(start..start);
            }
        }
        let control = c.sides()[active.index()];
        let top = row.saturating_sub(CONTEXT_ROWS);
        control.scroll_rows(top as isize - control.first_visible_row() as isize);
        control.set_scroll_position((0, control.scroll_position().1));
        self.sync_from(active);
        self.show_list();
    }

    /// Scrolls the other side and both line numbers to `side`'s place, and
    /// numbers the rows that come into view.
    fn sync_from(&self, side: Side) {
        let Some(c) = self.controls() else {
            return;
        };
        let other = side.other();
        let position = c.sides()[side.index()].scroll_position();
        c.sides()[other.index()].set_scroll_position(position);
        let mut positions = [(0, 0); 2];
        positions[side.index()] = position;
        positions[other.index()] = c.sides()[other.index()].scroll_position();
        self.number_rows();
        for each in Side::BOTH {
            let y = positions[each.index()].1;
            c.numbers()[each.index()].set_scroll_position((0, y));
        }
        self.scroll.set(positions);
    }

    /// A side scrolled, by any means: the other follows.
    fn follow_scroll(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let now = c.sides().map(RichText::scroll_position);
        let last = self.scroll.get();
        if now == last {
            return;
        }
        // Only the person's input scrolls without telling the window, so a
        // place that waits for its rows no longer moves the view.
        if let Some(loading) = self.loading.borrow_mut().as_mut() {
            loading.place = None;
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
            c.next.hwnd(),
            c.previous.hwnd(),
            c.find.hwnd(),
            c.find_next.hwnd(),
            c.extra.hwnd(),
            c.save.hwnd(),
            c.encodings.hwnd(),
            c.cancel.hwnd(),
            c.status.hwnd(),
        ];
        for control in controls.into_iter().chain(c.on_background()) {
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

    /// The matching options, the extra lines and the zoom as the state
    /// says; the other check boxes follow their commands.
    fn apply_state(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let state = self.state.get();
        c.options[1].set_checked(state.case_sensitive);
        c.options[2].set_checked(state.ignore_spaces);
        c.options[3].set_checked(state.ignore_frequent);
        c.extra.hwnd().set_text(&state.extra_lines.to_string());
        for control in c.rich() {
            control.set_zoom(state.zoom);
        }
    }

    /// The captions, the tooltips and the title in the window's language.
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
                (c.next.hwnd(), "compare.next"),
                (c.previous.hwnd(), "compare.previous"),
                (c.find.hwnd(), "compare.find"),
                (c.find_next.hwnd(), "compare.find_next"),
                (c.options[0].hwnd(), "compare.binary"),
                (c.options[1].hwnd(), "compare.case_sensitive"),
                (c.options[2].hwnd(), "compare.ignore_spaces"),
                (c.options[3].hwnd(), "compare.ignore_frequent"),
                (c.edit_mode.hwnd(), "compare.edit_mode_box"),
                (c.only.hwnd(), "compare.only_differences"),
                (c.extra_label.hwnd(), "compare.extra_lines"),
                (c.above.hwnd(), "compare.stacked"),
                (c.save.hwnd(), "compare.save_button"),
                (c.cancel.hwnd(), "button.cancel"),
            ];
            for (control, key) in captions {
                control.set_text(strings.text(key));
            }
            // No label precedes a path box, so it has no name of its own.
            for (path, key) in c.paths.into_iter().zip(["compare.left", "compare.right"]) {
                let name = text::without_access_key(strings.text(key));
                set_accessible_name(path.hwnd(), Some(&name));
            }
            let tips = [
                (c.next, CompareCommand::NextDifference),
                (c.previous, CompareCommand::PreviousDifference),
                (c.find, CompareCommand::Find),
                (c.find_next, CompareCommand::FindNext),
                (c.save, CompareCommand::Save),
            ];
            for (button, command) in tips {
                c.tips.set(button.hwnd(), &command.tip(&strings));
            }
            let encoding = text::without_access_key(strings.text("viewer.encoding"));
            c.tips.set(c.encodings.hwnd(), &encoding);
        }
        self.apply_encodings();
        self.apply_glyphs();
        self.apply_title(hwnd);
        self.show_message();
        self.show_status();
        self.layout(hwnd);
    }

    /// The buttons' icons, at the window's DPI, in the system colours of
    /// now and only while icons show.
    fn apply_glyphs(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let buttons = [
            (c.browse[0], "compare.browse"),
            (c.browse[1], "compare.browse_right"),
            (c.compare, "compare.compare"),
            (c.next, "compare.next"),
            (c.previous, "compare.previous"),
            (c.find, "compare.find"),
            (c.save, "compare.save_button"),
            (c.cancel, "button.cancel"),
        ];
        for (button, key) in buttons {
            button.set_glyph(button_glyph(key));
        }
    }

    fn title_text(&self) -> String {
        let session = self.session.borrow();
        let names = match session.showing() {
            Some(showing) => Side::BOTH.map(|side| showing.name(side)),
            None => [String::new(), String::new()],
        };
        let changed = Side::BOTH.map(|side| session.changed(side));
        title(&self.strings.borrow(), [&names[0], &names[1]], changed)
    }

    fn apply_title(&self, hwnd: Hwnd) {
        hwnd.set_text(&self.title_text());
    }

    /// The status bar: what runs, or why the rows that show could not be
    /// built again, or the differences, the caret's among them, with the
    /// notes. While no comparison shows, the message in the sides' place
    /// says why.
    fn show_status(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let caret = self.caret_row();
        let strings = self.strings.borrow();
        let numbers = self.numbers.borrow();
        let session = self.session.borrow();
        let mut status = String::new();
        if self.running.get() != Running::Hidden {
            status = strings.text("compare.comparing").to_owned();
        } else if let Some(showing) = session.showing() {
            let view = showing.view();
            let index = view.rows.difference_index(caret);
            let mut texts = vec![match self.notice.borrow().as_ref() {
                Some(notice) => notice.text(&strings),
                None => differences_text(&strings, &*numbers, index, view.differences),
            }];
            let notes = [
                showing.note(),
                view.rows.forced.then_some(Note::OnlyDifferences),
                view.rows
                    .truncated
                    .then_some(Note::Truncated(view.rows.rows.len())),
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
            status = texts.join(" · ");
        }
        c.status.set_text(0, &status);
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
        let side = c.sides()[self.active.get().index()];
        let row = side.row_of(side.selection().start);
        let texts = c.sides().map(|side| side.row_text(row));
        let marks = self.row_marks(row, &texts);
        let mut rtf = Rtf::new(&palette(), rtf_font());
        for index in 0..2 {
            rtf.row(&texts[index], Style::Text, &marks[index]);
        }
        c.list.set_rtf(&rtf.finish());
    }

    /// The parts of `row` that differ, as the sides mark them; `texts` are
    /// the two sides' text of the row, which the list shows and only Hex
    /// reads.
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
        let encodings = self.controls().map_or(Px(0), |c| {
            image_button_width(meter.width(&c.encodings.hwnd().text()), dpi)
        });
        Some(CompareSizes {
            line: Px(meter.height().0 * 3 / 2 + 2),
            text: meter.height(),
            browse: button("compare.browse").max(button("compare.browse_right")),
            buttons: [
                button("compare.compare"),
                button("compare.next"),
                button("compare.previous"),
                button("compare.find"),
                button("compare.find_next"),
            ],
            options: [
                caption("compare.binary"),
                caption("compare.case_sensitive"),
                caption("compare.ignore_spaces"),
                caption("compare.ignore_frequent"),
            ],
            views: [
                caption("compare.edit_mode_box"),
                caption("compare.only_differences"),
                caption("compare.stacked"),
            ],
            extra_label: caption("compare.extra_lines"),
            extra: meter.width(&"9".repeat(EXTRA_DIGITS)),
            save: button("compare.save_button"),
            encodings,
            cancel: button("button.cancel"),
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
        let running = self.running.get();
        let message = self.message_shown.get();
        let resized = Side::BOTH.map(|side| {
            let path_box = c.paths[side.index()].hwnd();
            path_box.rect_in(hwnd).width != layout.paths[side.index()].width
        });
        win::place_children(
            &[
                (c.paths[0].hwnd(), layout.paths[0]),
                (c.browse[0].hwnd(), layout.browse[0]),
                (c.paths[1].hwnd(), layout.paths[1]),
                (c.browse[1].hwnd(), layout.browse[1]),
                (c.compare.hwnd(), layout.buttons[0]),
                (c.next.hwnd(), layout.buttons[1]),
                (c.previous.hwnd(), layout.buttons[2]),
                (c.find.hwnd(), layout.buttons[3]),
                (c.find_next.hwnd(), layout.buttons[4]),
                (c.options[0].hwnd(), layout.options[0]),
                (c.options[1].hwnd(), layout.options[1]),
                (c.options[2].hwnd(), layout.options[2]),
                (c.options[3].hwnd(), layout.options[3]),
                (c.edit_mode.hwnd(), layout.views[0]),
                (c.only.hwnd(), layout.views[1]),
                (c.extra_label.hwnd(), layout.extra_label),
                (c.extra.hwnd(), layout.extra),
                (c.above.hwnd(), layout.views[2]),
                (c.save.hwnd(), layout.save),
                (c.encodings.hwnd(), layout.encodings),
                (c.progress.hwnd(), layout.progress),
                (c.cancel.hwnd(), layout.cancel),
                (c.message.hwnd(), layout.message),
                (c.sides()[0].hwnd(), layout.sides[0]),
                (c.sides()[1].hwnd(), layout.sides[1]),
                (c.numbers()[0].hwnd(), layout.numbers[0]),
                (c.numbers()[1].hwnd(), layout.numbers[1]),
                (c.list.hwnd(), layout.list),
            ],
            &[
                (c.progress.hwnd(), running != Running::Hidden),
                (c.cancel.hwnd(), running == Running::Cancel),
                (c.message.hwnd(), message),
                (c.sides()[0].hwnd(), !message),
                (c.sides()[1].hwnd(), !message),
                (c.numbers()[0].hwnd(), !message),
                (c.numbers()[1].hwnd(), !message),
                (c.list.hwnd(), state.two_line_list && !message),
            ],
        );
        self.scroll_paths(Side::BOTH.into_iter().filter(|side| resized[side.index()]));
        // A control that the message hid had the focus.
        if let Some(focused) = Hwnd::focused()
            && hwnd.is_parent_of(focused)
            && !focused.is_visible()
            && let Some(target) = self.focus_target()
        {
            target.focus();
        }
        self.sync_from(self.active.get());
    }

    /// Lets the person type in each side that takes typing now, which no
    /// side does while its rows stream in.
    fn apply_read_only(&self) {
        let Some(c) = self.controls() else {
            return;
        };
        let typing = {
            let session = self.session.borrow();
            Side::BOTH.map(|side| session.typing(side))
        };
        let loading = self.loading.borrow().is_some();
        for side in Side::BOTH {
            c.sides()[side.index()].set_read_only(loading || !typing[side.index()]);
        }
    }

    /// The person typed, pasted or deleted in `side`: the window reads
    /// only the rows that the edit changed.
    fn on_edited(&self, hwnd: Hwnd, side: Side) {
        let Some(c) = self.controls() else {
            return;
        };
        if !self.session.borrow().typing(side) {
            return;
        }
        let event = match c.sides()[side.index()].edited_rows() {
            Some(rows) => {
                let mut prints = self.prints.borrow_mut();
                let prints = Arc::make_mut(&mut prints[side.index()]);
                if prints.is_empty() {
                    prints.push(rtf::UNKNOWN);
                }
                let replaced = rows.replaced(prints.len());
                let typed = std::iter::repeat_n(rtf::UNKNOWN, rows.last - rows.first + 1);
                prints.splice(replaced, typed);
                Event::Edited { side, rows }
            }
            // A change that the window could not read takes the edit back:
            // the side shows its lines again, streamed whole.
            None => {
                self.forget_prints(side);
                Event::Outdated
            }
        };
        self.send(hwnd, event);
        self.show_list();
    }

    /// Runs a command that reads the rows, which are fresh; while rows
    /// stream in, holds it for [`Self::end_present`].
    fn run_command(&self, hwnd: Hwnd, command: CompareCommand, fresh: Fresh) {
        use CompareCommand as C;
        if let Some(loading) = self.loading.borrow_mut().as_mut() {
            loading.commands.push_back(command);
            return;
        }
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
            C::FindNext => self.find(false, fresh),
            C::FindPrevious => self.find(true, fresh),
            _ => {}
        }
    }

    /// Copy to right and Copy to left: the selected rows, or the difference
    /// with the caret, replace the lines they pair with on side `to`.
    fn copy_to(&self, hwnd: Hwnd, to: Side, fresh: Fresh) {
        let Some(c) = self.controls() else {
            return;
        };
        let control = c.sides()[self.active.get().index()];
        let selection = control.selection();
        let first = control.row_of(selection.start);
        let rows = if selection.is_empty() {
            (self.session.borrow().showing()).map_or(first..first + 1, |showing| {
                showing.view().rows.difference_at(first)
            })
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
        let rows = c.sides().map(|side| side.row_of(side.selection().start));
        self.send(hwnd, Event::Resync { fresh, rows });
    }

    /// Next and Previous difference.
    fn go_to_difference(&self, forward: bool) {
        let row = self.caret_row();
        let target = (self.session.borrow().showing()).and_then(|showing| {
            let rows = &showing.view().rows;
            if forward {
                rows.next_difference(row)
            } else {
                rows.previous_difference(row)
            }
        });
        if let Some(target) = target {
            self.go_to_row(target);
        }
    }

    /// The row of the caret in the side the person works in.
    fn caret_row(&self) -> usize {
        let Some(c) = self.controls() else {
            return 0;
        };
        let control = c.sides()[self.active.get().index()];
        control.row_of(control.selection().start)
    }

    /// `F2` and `Shift+F2`: the next or previous marked part of the
    /// caret's row, and past its end the one of the next row that differs.
    fn go_to_mark(&self, forward: bool) {
        let Some(c) = self.controls() else {
            return;
        };
        let side = self.active.get();
        let control = c.sides()[side.index()];
        let selection = control.selection();
        let row = control.row_of(selection.start);
        let column = selection.start - control.row_start(row).unwrap_or(0).min(selection.start);
        let (count, hex) = (self.session.borrow().showing()).map_or((0, false), |showing| {
            (showing.view().rows.rows.len(), showing.mode() == Mode::Hex)
        });
        let marks_of = |row: usize| {
            // Only a row that differs and holds a line of `side` can have
            // marks there.
            let marked = (self.session.borrow().showing())
                .and_then(|showing| showing.view().rows.rows.get(row))
                .is_some_and(|shown| shown.differs() && shown.line(side).is_some());
            if !marked {
                return Vec::new();
            }
            // Text marks the session's lines, so only Hex reads the controls.
            let texts = if hex {
                c.sides().map(|side| side.row_text(row))
            } else {
                [String::new(), String::new()]
            };
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
        [c.sides()[0], c.sides()[1], c.list]
            .into_iter()
            .find(|control| control.hwnd() == focused)
    }

    /// Where the focus is, which decides the keys the window takes.
    fn key_focus(&self, target: Hwnd) -> KeyFocus {
        let Some(c) = self.controls() else {
            return KeyFocus::Other;
        };
        if [c.sides()[0], c.sides()[1], c.list]
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
                if let Some(control) = [c.sides()[0], c.sides()[1], c.list]
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
                let mut panes = vec![c.sides()[0].hwnd(), c.sides()[1].hwnd()];
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
        let active = c.sides()[self.active.get().index()];
        match command {
            C::Compare => self.compare_boxes(hwnd),
            C::Save => self.send(hwnd, Event::Save),
            C::Close => self.close(hwnd),
            C::Undo => self.send(hwnd, Event::Undo),
            C::Copy => match self.focused_text() {
                Some(control) => self.copy_text(control.selected_text()),
                None => {
                    if let Some(edit) = [c.paths[0], c.paths[1], c.extra]
                        .into_iter()
                        .find(|edit| Some(edit.hwnd()) == Hwnd::focused())
                    {
                        self.copy_text(edit.selected_text());
                    }
                }
            },
            C::Paste => {
                if self.session.borrow().editing() {
                    active.paste_text();
                }
            }
            C::SelectAll => self.focused_text().unwrap_or(active).select_all(),
            // Find reads the rows, so it waits for those of the latest edit;
            // it looks for the query that the dialog keeps.
            C::Find => {
                if self.ask_query(hwnd).is_some() {
                    self.send(hwnd, Event::Command(C::FindNext));
                }
            }
            C::FindNext | C::FindPrevious => {
                if self.last_query().is_some() || self.ask_query(hwnd).is_some() {
                    self.send(hwnd, Event::Command(command));
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
                self.apply_commands();
            }
            C::ZoomIn => self.set_zoom(hwnd, zoomed(self.state.get().zoom, 1)),
            C::ZoomOut => self.set_zoom(hwnd, zoomed(self.state.get().zoom, -1)),
            C::ZoomReset => self.set_zoom(hwnd, DEFAULT_ZOOM),
            C::Encoding(side, encoding) => self.send(hwnd, Event::Encoding(side, encoding)),
        }
    }

    /// Compare, or `Enter` in a box: compares the files that the path boxes
    /// name, after asking about changes. Compare is disabled, and `Enter`
    /// does nothing, while a box is empty.
    fn compare_boxes(&self, hwnd: Hwnd) {
        if let Some(paths) = self.box_paths() {
            self.send(hwnd, Event::Compare(paths));
        }
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
        let texts = c
            .paths
            .map(|path| path.text().encode_utf16().collect::<Vec<u16>>());
        for side in Side::BOTH {
            self.set_path(side, &texts[side.other().index()]);
        }
    }

    /// The sides' controls follow the files with the rows they show, so
    /// Swap sides streams nothing.
    fn swap_sides(&self, hwnd: Hwnd) {
        let Some(c) = self.controls() else {
            return;
        };
        let [left, right] = c.sides();
        c.sides.set([right, left]);
        let [left, right] = c.numbers();
        c.numbers.set([right, left]);
        self.prints.borrow_mut().swap(0, 1);
        let [left, right] = self.numbered.get();
        self.numbered.set([right, left]);
        let [left, right] = self.scroll.get();
        self.scroll.set([right, left]);
        self.layout(hwnd);
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
            let chosen = if index % 2 == 1 {
                match self.save_as(hwnd, side) {
                    Some(path) => Some(path),
                    None => return Answer::Cancel,
                }
            } else {
                None
            };
            let session = self.session.borrow();
            let Some((showing, texts)) =
                (session.showing()).and_then(|showing| Some((showing, showing.texts()?)))
            else {
                return Answer::Cancel;
            };
            // The save's worker encodes the text, so the window's thread
            // does not wait in proportion to a side.
            files.push(SaveFile {
                side,
                path: chosen.unwrap_or_else(|| showing.paths()[side.index()].clone()),
                text: Arc::clone(&texts[side.index()]),
            });
        }
        Answer::Save(files)
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
                    failures.push((side, path.clone(), failure));
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
        for (side, path, failure) in failures {
            let text = {
                let strings = self.strings.borrow();
                let name = name_of(&path);
                match failure {
                    Unsaved::Withheld => strings.format("compare.withheld", &[&name]),
                    Unsaved::Unencodable { encoding, first } => match first {
                        Some((line, character)) => strings.format(
                            "compare.cannot_encode_character",
                            &[
                                &name,
                                encoding.name(),
                                &character.to_string(),
                                &self.numbers.borrow().integer(line as u64 + 1),
                            ],
                        ),
                        None => strings.format("compare.cannot_encode", &[&name, encoding.name()]),
                    },
                    Unsaved::Write(failure) => {
                        let text = strings.format(
                            "compare.save_failed",
                            &[&name, &win::error_text(&failure.error)],
                        );
                        let next = match failure.kept.as_ref().and_then(|kept| kept.file_name()) {
                            Some(kept) => {
                                strings.format("compare.save_kept", &[&kept.to_string_lossy()])
                            }
                            // Save's question offers Save as… for the side.
                            None => {
                                let save_as = match side {
                                    Side::Left => "compare.save_left_as",
                                    Side::Right => "compare.save_right_as",
                                };
                                let choice = text::without_access_key(strings.text(save_as));
                                strings.format("compare.save_elsewhere", &[&choice])
                            }
                        };
                        format!("{text} {next}")
                    }
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

    /// The query that Find looked for last, in any compare window.
    fn last_query(&self) -> Option<Query> {
        (self.shared.upgrade()).and_then(|shared| shared.query.borrow().clone())
    }

    /// Find next, or with `backward` Find previous: the worker looks for
    /// the last query from the selection of the side with the caret, then
    /// in the other side, through the rows that `fresh` proves.
    fn find(&self, backward: bool, fresh: Fresh) {
        let Some(c) = self.controls() else {
            return;
        };
        let Some(needle) = self.last_query().as_ref().and_then(Needle::new) else {
            return;
        };
        let side = self.active.get();
        let control = c.sides()[side.index()];
        let selection = control.selection();
        let at = if backward {
            selection.start
        } else {
            selection.end
        };
        let row = control.row_of(at);
        let column = at - control.row_start(row).unwrap_or(0).min(at);
        let look = self.look();
        let find = {
            let session = self.session.borrow();
            let Some(showing) = session.showing() else {
                return;
            };
            let searched = match showing.texts() {
                Some(texts) => Searched::Text(texts.clone()),
                None => Searched::Hex {
                    paths: showing.paths().clone(),
                    sizes: showing.sizes(),
                },
            };
            Find {
                needle,
                backward,
                side,
                row,
                column,
                rows: Arc::clone(&showing.view().rows),
                searched,
                look,
                fresh,
            }
        };
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.find(find);
        }
    }

    /// Selects the match that the worker's Find found, or says why none.
    /// A match in rows that changed meanwhile shows nothing, because it was
    /// found in other text.
    fn on_found(&self, hwnd: Hwnd, found: Found) {
        let Some(c) = self.controls() else {
            return;
        };
        let title = self.strings.borrow().text("viewer.find_title").to_owned();
        match found {
            Found::Match {
                fresh,
                side,
                row,
                columns,
            } => {
                if !self.session.borrow().holds(&fresh) {
                    return;
                }
                let control = c.sides()[side.index()];
                let start = control.row_start(row).unwrap_or(0);
                self.active.set(side);
                control.hwnd().focus();
                control.select(start + columns.start..start + columns.end);
                self.sync_from(side);
                self.show_list();
            }
            Found::Nothing => {
                let query = self.last_query().unwrap_or_default();
                let text = (self.strings.borrow()).format("viewer.not_found", &[&query.text]);
                info_box(hwnd, &text, &title);
            }
            Found::Failed { path, error } => {
                let (name, reason) = (name_of(&path), win::error_text(&error));
                let text = (self.strings.borrow()).format("viewer.cannot_read", &[&name, &reason]);
                error_box(Some(hwnd), &text, &title);
            }
            Found::Cancelled => {}
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
        let can_compare = self.box_paths().is_some();
        let caret = self.caret_row();
        let session = self.session.borrow();
        let showing = session.showing();
        let rows = showing.map(|showing| &showing.view().rows);
        let state = self.state.get();
        MenuState {
            can_compare,
            shown: showing.map(|showing| (showing.mode(), showing.encodings())),
            next: rows.is_some_and(|rows| rows.next_difference(caret).is_some()),
            previous: rows.is_some_and(|rows| rows.previous_difference(caret).is_some()),
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
        let side = c.sides()[self.active.get().index()];
        let corner = side.hwnd().screen_rect();
        let point = point.unwrap_or((corner.x.0 + 16, corner.y.0 + 16));
        let nodes = context_menu(&self.strings.borrow());
        self.track_menu(hwnd, &nodes, point);
    }

    /// The encodings button's menu, below the button, as a drop-down
    /// button's menu opens.
    fn encoding_menu(&self, hwnd: Hwnd) {
        let Some(c) = self.controls() else {
            return;
        };
        let button = c.encodings.hwnd().screen_rect();
        let nodes = encoding_menu(&self.strings.borrow());
        self.track_menu(hwnd, &nodes, (button.x.0, button.bottom().0));
    }

    /// Opens a menu of `nodes` at `point`, in screen coordinates, and runs
    /// the command chosen in it.
    fn track_menu(&self, hwnd: Hwnd, nodes: &[crate::core::commands::MenuNode], point: (i32, i32)) {
        let states = menu_states(self.menu_state());
        // The menu runs its own message loop, so nothing is borrowed here.
        let chosen = Menu::track_popup(hwnd, nodes, point, &states, None);
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

    /// A file or two that File Explorer dropped: one fills the path box
    /// that is empty while the other is not, or else that of the side it
    /// fell on; two fill both.
    fn drop_files(&self, hwnd: Hwnd, files: Vec<Vec<u16>>, point: (i32, i32)) {
        let Some(c) = self.controls() else {
            return;
        };
        let right = c.sides()[1].hwnd().rect_in(hwnd);
        let fell_right = if self.state.get().above_each_other {
            point.1 >= right.y.0
        } else {
            point.0 >= right.x.0
        };
        let empty = Side::BOTH.map(|side| box_path(&c.paths[side.index()].text()).is_empty());
        let side = match empty {
            [true, false] => Side::Left,
            [false, true] => Side::Right,
            _ if fell_right => Side::Right,
            _ => Side::Left,
        };
        match files.as_slice() {
            [] => {}
            [one] => self.set_path(side, one),
            [first, second, ..] => {
                self.set_path(Side::Left, first);
                self.set_path(Side::Right, second);
            }
        }
    }

    /// Browse…: the file the person chose goes in the side's path box.
    fn browse(&self, hwnd: Hwnd, side: Side) {
        let (title, all) = {
            let strings = self.strings.borrow();
            (
                strings.text("compare.browse_title").to_owned(),
                strings.text("compare.all_files").to_owned(),
            )
        };
        if let Some(path) = pick_file(hwnd, &title, &all, "*.*") {
            self.set_path(side, &path.encode_utf16().collect::<Vec<u16>>());
        }
    }

    /// A control of the window notified it.
    fn on_control(&self, hwnd: Hwnd, event: crate::win::notify::ControlEvent) {
        let Some(c) = self.controls() else {
            return;
        };
        for side in Side::BOTH {
            if c.sides()[side.index()].changed(event) {
                return self.on_edited(hwnd, side);
            }
            if c.browse[side.index()].clicked(event) {
                return self.browse(hwnd, side);
            }
            if c.paths[side.index()].changed(event) {
                return self.apply_commands();
            }
        }
        let commands = [
            (c.compare, CompareCommand::Compare),
            (c.next, CompareCommand::NextDifference),
            (c.previous, CompareCommand::PreviousDifference),
            (c.find, CompareCommand::Find),
            (c.find_next, CompareCommand::FindNext),
            (c.save, CompareCommand::Save),
        ];
        let checks = [
            (c.edit_mode, CompareCommand::EditMode),
            (c.only, CompareCommand::OnlyDifferences),
            (c.above, CompareCommand::AboveEachOther),
        ];
        if let Some((_, command)) = commands.iter().find(|(button, _)| button.clicked(event)) {
            self.on_command(hwnd, *command);
        } else if let Some((_, command)) = checks.iter().find(|(check, _)| check.clicked(event)) {
            self.on_command(hwnd, *command);
        } else if c.options[0].clicked(event) {
            let hex = c.options[0].is_checked();
            self.on_command(
                hwnd,
                if hex {
                    CompareCommand::Hex
                } else {
                    CompareCommand::Text
                },
            );
        } else if c.encodings.clicked(event) {
            self.encoding_menu(hwnd);
        } else if c.cancel.clicked(event) {
            self.cancel_compare(hwnd);
        } else if let Some(index) = (1..4).find(|index| c.options[*index].clicked(event)) {
            let checked = c.options[index].is_checked();
            self.save_state(|state| match index {
                1 => state.case_sensitive = checked,
                2 => state.ignore_spaces = checked,
                _ => state.ignore_frequent = checked,
            });
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
        self.apply_glyphs();
        self.show_list();
        self.send(hwnd, Event::Colours);
    }
}

/// How lines and words match, as the check boxes of `state` say.
fn matching_of(state: CompareState) -> Matching {
    Matching {
        case_sensitive: state.case_sensitive,
        ignore_spaces: state.ignore_spaces,
        ignore_frequent: state.ignore_frequent,
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
                self.apply_glyphs();
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
                    .find(|side| c.sides()[side.index()].selection_changed(&notification))?;
                // While a place waits for its rows, the loading moves no
                // caret and commands wait, so this move is the person's,
                // and the place gives way to it.
                if let Some(loading) = self.loading.borrow_mut().as_mut() {
                    loading.place = None;
                }
                self.active.set(side);
                self.show_list();
                // "Difference 2 of 3", Next and Previous follow the caret.
                self.show_status();
                self.apply_commands();
                Some(LRESULT(0))
            }
            Message::CtlColorStatic { colors, control }
                if self.controls()?.on_background().contains(&control) =>
            {
                Some(colors.window_background())
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
                // The comparison has run as long as its progress waits for.
                self.refresh(hwnd);
                self.show_window(hwnd);
                Some(LRESULT(0))
            }
            Message::Timer(PROGRESS_TIMER) => {
                let (done, total) = self.progress.borrow().read();
                if let Some(c) = self.controls() {
                    c.progress.set_measured(done, total);
                }
                if self.running.get() != Running::Cancel {
                    self.refresh(hwnd);
                }
                Some(LRESULT(0))
            }
            Message::Timer(CLIPBOARD_TIMER) => {
                hwnd.kill_timer(CLIPBOARD_TIMER);
                let left = self
                    .work
                    .borrow_mut()
                    .as_mut()
                    .and_then(|work| work.clipboard.retry());
                if let Some(left) = left {
                    hwnd.set_timer(CLIPBOARD_TIMER, left.as_millis().max(1) as u32);
                }
                Some(LRESULT(0))
            }
            Message::Timer(REALIGN_TIMER) => {
                hwnd.kill_timer(REALIGN_TIMER);
                self.send(hwnd, Event::Timer);
                Some(LRESULT(0))
            }
            Message::Timer(LOAD_TIMER) => {
                if self.load(hwnd) {
                    self.refresh(hwnd);
                }
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
                let focus = (self.focused.take())
                    .filter(|focused| focused.is_visible())
                    .or_else(|| self.focus_target())?;
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
                for timer in [
                    SHOW_TIMER,
                    PROGRESS_TIMER,
                    REALIGN_TIMER,
                    CLIPBOARD_TIMER,
                    LOAD_TIMER,
                ] {
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
                if let Some(c) = self.controls() {
                    for path in c.paths {
                        set_accessible_name(path.hwnd(), None);
                    }
                }
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
