//! The Viewer windows (DESIGN.md: The Viewer): each `F3` in a panel or in
//! Search opens one, a top-level window that no window owns, of the focused
//! file or of the chosen files, which `N` and `P` walk through.
//! `core::viewer_window` decides what a Viewer shows; this module gives it
//! the blocks that the reader of [`super::work::viewers`] admits, and asks
//! for the next block that the page or a move waits for, one at a time.

use super::dialogs::ViewerFind;
use super::work::viewers::Reader;
use crate::core::captions::{self, DialogCaptions};
use crate::core::clipboard::Publication;
use crate::core::commands::Keystroke;
use crate::core::ids::{ControlId, DialogId, IconId};
use crate::core::layout::{
    Px, Rect, Size, WHEEL_DELTA, image_button_width, status_last_part, viewer_message,
    viewer_message_width,
};
use crate::core::open::Entries;
use crate::core::settings::{Placement, ViewerState};
use crate::core::text::{self, Strings};
use crate::core::viewer::Mode;
use crate::core::viewer::find::Query;
use crate::core::viewer_window::{
    DEFAULT_ZOOM, Document, MenuState, Reopening, ScrollBar, Shown, TextAt, TooLarge,
    ViewerCommand, ViewerKey, file_text, menu_bar, menu_states, place_samples, text_at, text_inset,
    text_room, title, viewer_key, waiting_layout, zoomed,
};
use crate::core::work::{Waiting, waiting};
use crate::win::clipboard;
use crate::win::controls::{Button, Label, StatusBar};
use crate::win::dialog::{DialogEnd, error_box, info_box, run_modal};
use crate::win::folder::Removal;
use crate::win::gdi::{Font, TextMeter};
use crate::win::locale::{WindowsNumbers, code_page_text};
use crate::win::menu::{Menu, replace_menu_bar};
use crate::win::notify::Command;
use crate::win::viewer::{
    AREA_CLICKED, AREA_DOUBLE_CLICKED, AREA_FOCUSED, AREA_PAINT, AreaText, ViewerEvent,
    caption_height, create_area, paint_area, register_area, set_area_scroll, set_caret,
};
use crate::win::wake::OpenWaker;
use crate::win::window::{self, Message, WindowProc};
use crate::win::{self, Hwnd};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;
use windows::Win32::Foundation::LRESULT;

const WINDOW_CLASS: &str = "gozler.ui.viewer";
const READ_TIMER: usize = 1;
/// Scrolls toward the pointer while a drag holds it outside the area.
const SCROLL_TIMER: usize = 2;
/// Shows "Reading…" once a file with no page before it has been opening
/// for [`crate::core::work::BUSY`], so a quick open shows only the page.
const READING_TIMER: usize = 3;

/// What a Viewer shows.
pub(super) enum Files {
    /// One file, at its full path in UTF-16, which holds every name a
    /// folder can hold.
    One(Vec<u16>),
    /// The files among the chosen entries, `focused` first when it is one
    /// of them, and `focused`, a file, when none is.
    Chosen {
        entries: Entries,
        focused: Option<Vec<u16>>,
    },
}

/// The open Viewers, as the main window holds them, and what they remember
/// in `ui.json`.
pub(super) struct Viewers {
    shared: Rc<Shared>,
    /// The window classes are registered, which the first Viewer does.
    registered: Cell<bool>,
}

/// What every Viewer reaches: the open ones, which Exit closes, the state
/// the last one left, which the next one opens with, and what Find looked
/// for last.
struct Shared {
    open: RefCell<Vec<Rc<Viewer>>>,
    state: Cell<ViewerState>,
    changed: Cell<bool>,
    query: RefCell<Option<Query>>,
}

impl Viewers {
    pub(super) fn new(state: ViewerState) -> Viewers {
        Viewers {
            shared: Rc::new(Shared {
                open: RefCell::new(Vec::new()),
                state: Cell::new(state),
                changed: Cell::new(false),
                query: RefCell::new(None),
            }),
            registered: Cell::new(false),
        }
    }

    /// Opens a Viewer of `files` in front, worded with `strings`. Closing
    /// it activates `main`, which opened it, while `main` shows.
    pub(super) fn open(
        &self,
        files: Files,
        strings: Strings,
        main: Hwnd,
    ) -> windows::core::Result<()> {
        if !self.registered.get() {
            window::register_class::<Viewer>(
                WINDOW_CLASS,
                IconId::Gozler.id(),
                window::Background::Window,
            )?;
            register_area()?;
            self.registered.set(true);
        }
        let state = self.shared.state.get();
        let placement = self.placement(state.placement);
        let viewer = Rc::new(Viewer {
            hwnd: Cell::new(None),
            shared: Rc::downgrade(&self.shared),
            files: RefCell::new(Vec::new()),
            index: Cell::new(0),
            name: RefCell::new(String::new()),
            main,
            strings: RefCell::new(strings),
            numbers: RefCell::new(WindowsNumbers::new()),
            controls: OnceCell::new(),
            fonts: RefCell::new(None),
            shown: RefCell::new(Shown::Reading),
            reader: RefCell::new(None),
            waker: OnceCell::new(),
            resume: Cell::new(None),
            opening: Cell::new(None),
            reading_since: Cell::new(Instant::now()),
            slow: Cell::new(false),
            read_timer: Cell::new(false),
            wrap: Cell::new(state.wrap),
            zoom: Cell::new(state.zoom),
            zoom_wheel: Cell::new(0),
            focused: Cell::new(None),
            dragging: Cell::new(false),
            pressed: Cell::new(None),
            asked: RefCell::new(Vec::new()),
        });
        let hwnd = window::create_window(WINDOW_CLASS, Rc::clone(&viewer), placement)?;
        if let Err(error) = viewer.start(hwnd, files) {
            hwnd.destroy();
            return Err(error);
        }
        window::show(hwnd, placement);
        viewer.layout(hwnd);
        viewer.controls().area.focus();
        self.shared.open.borrow_mut().push(viewer);
        Ok(())
    }

    /// The state that `ui.json` saves: the last Viewer's size and place,
    /// Wrap long lines and the text size.
    pub(super) fn state(&self) -> ViewerState {
        self.shared.state.get()
    }

    /// Takes the saved-state transfer posted by a Viewer change.
    pub(super) fn take_state(&self) -> Option<ViewerState> {
        self.shared.changed.replace(false).then(|| self.state())
    }

    /// Closes every Viewer, as Exit does; each leaves its state first.
    pub(super) fn close_all(&self) {
        let open = std::mem::take(&mut *self.shared.open.borrow_mut());
        // The first Viewer closes last, so the saved place stays its own,
        // not one that each later Viewer moved a caption further.
        for viewer in open.into_iter().rev() {
            if let Some(hwnd) = viewer.hwnd.get() {
                hwnd.destroy();
            }
        }
    }

    /// Words every Viewer in the language of `strings`.
    pub(super) fn set_strings(&self, strings: &Strings) {
        for viewer in self.opened() {
            viewer.strings.replace(strings.clone());
            if let Some(hwnd) = viewer.hwnd.get() {
                viewer.apply_strings(hwnd);
                viewer.layout(hwnd);
            }
        }
    }

    /// Shows or hides the icons of every Viewer's menus and Retry, after
    /// icons were shown or hidden.
    pub(super) fn apply_glyphs(&self) {
        for viewer in self.opened() {
            if let Some(hwnd) = viewer.hwnd.get() {
                viewer.apply_glyphs(hwnd);
            }
        }
    }

    /// Where a new Viewer opens: at `saved`, the size and place the last
    /// Viewer left, or, while a Viewer is open, at the saved size below and
    /// to the right of the last one opened by its caption's height, and not
    /// maximized, so the two never cover each other exactly.
    fn placement(&self, saved: Option<Placement>) -> Option<Placement> {
        let last = self.opened().last().and_then(|viewer| viewer.hwnd.get());
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

    /// A copy of the list, so a Viewer that closes meanwhile can leave it.
    fn opened(&self) -> Vec<Rc<Viewer>> {
        self.shared.open.borrow().clone()
    }
}

/// A Viewer's controls, created in Tab order. The message and Retry come
/// before the area, so they lie above it.
struct Controls {
    message: Label,
    retry: Button,
    area: Hwnd,
    status: StatusBar,
}

struct Fonts {
    ui: Font,
    /// Consolas at the Viewer's text size, in which the area draws the
    /// file.
    text: Font,
    char_width: Px,
    line_height: Px,
}

struct Viewer {
    hwnd: Cell<Option<Hwnd>>,
    shared: Weak<Shared>,
    /// The files the Viewer walks through, empty until the reader found
    /// them among the chosen entries, and the one that shows.
    files: RefCell<Vec<Vec<u16>>>,
    index: Cell<usize>,
    /// The shown file's name.
    name: RefCell<String>,
    main: Hwnd,
    strings: RefCell<Strings>,
    numbers: RefCell<WindowsNumbers>,
    controls: OnceCell<Controls>,
    fonts: RefCell<Option<Fonts>>,
    shown: RefCell<Shown>,
    reader: RefCell<Option<Reader>>,
    /// Wakes the window from the reading thread, which outlives it, until
    /// `WM_DESTROY` closes it.
    waker: OnceCell<OpenWaker>,
    /// Where the next opening of the file shows it again, after a removal
    /// or a failure. Reload shows the place of the page it keeps instead.
    resume: Cell<Option<Reopening>>,
    /// While `N` or `P` opens another file, its index. The page, the title
    /// and every command stay with the file at `index` until it opened, as
    /// Reload keeps its page (DESIGN.md: The Viewer).
    opening: Cell<Option<usize>>,
    /// When the file of [`Shown::Reading`] started to open.
    reading_since: Cell<Instant>,
    slow: Cell<bool>,
    read_timer: Cell<bool>,
    wrap: Cell<bool>,
    /// The text size in percent.
    zoom: Cell<u16>,
    /// The wheel's turn with Ctrl that is less than a notch, which a
    /// touchpad sends in parts.
    zoom_wheel: Cell<i32>,
    /// The control that had the focus when the window was deactivated.
    focused: Cell<Option<Hwnd>>,
    /// A drag over the text runs, and the window holds the mouse until the
    /// button goes up.
    dragging: Cell<bool>,
    /// The key last pressed in this window. A held key's repeats follow
    /// its first press, which a key held since before the window opened
    /// did not send here.
    pressed: Cell<Option<Keystroke>>,
    /// Copy and Find that wait, in the order asked, until the reader can
    /// start them: after the file opened, or after the Copy or Find that
    /// runs. Each reads the page as it shows when it starts.
    asked: RefCell<Vec<Asked>>,
}

/// Copy or Find, as the person asked for it.
enum Asked {
    Find {
        query: Query,
        backward: bool,
    },
    /// With the clipboard as it was then.
    Copy(Publication),
}

impl Viewer {
    fn controls(&self) -> &Controls {
        self.controls.get().expect("start creates the controls")
    }

    /// Everything the new window needs before it shows: its keys, its
    /// controls, menus and texts, and the reading of its files.
    fn start(&self, hwnd: Hwnd, files: Files) -> windows::core::Result<()> {
        self.hwnd.set(Some(hwnd));
        window::own_keys(hwnd, true);
        let controls = Controls {
            message: Label::create_centred(hwnd, ControlId::ViewerMessage.id())?,
            retry: Button::create(hwnd, ControlId::ViewerRetry.id())?,
            area: create_area(hwnd, ControlId::ViewerArea.id())?,
            status: StatusBar::create(hwnd, ControlId::ViewerStatus.id(), true)?,
        };
        let _ = self.controls.set(controls);
        self.apply_fonts(hwnd);
        self.apply_strings(hwnd);
        let _ = self.waker.set(OpenWaker::new(hwnd));
        self.reader.replace(Some(Reader::new(
            self.waker
                .get()
                .expect("start sets the wake endpoint")
                .clone(),
        )));
        // The area starts as Shown::Reading, for the file or the selection.
        self.wait_to_say_reading(hwnd);
        match files {
            Files::One(path) => {
                self.files.replace(vec![path]);
                self.show_file(hwnd);
            }
            Files::Chosen { entries, focused } => {
                if let Some(reader) = self.reader.borrow_mut().as_mut() {
                    reader.files(entries, focused);
                }
            }
        }
        Ok(())
    }

    /// The shown file's full path; `None` before the reader found the
    /// files.
    fn path(&self) -> Option<Vec<u16>> {
        self.files.borrow().get(self.index.get()).cloned()
    }

    /// Opens the file at `index` from its start.
    fn show_file(&self, hwnd: Hwnd) {
        self.name_file(hwnd);
        self.read_file(hwnd);
    }

    /// Names the file at `index` in the title and the status bar.
    fn name_file(&self, hwnd: Hwnd) {
        let name = self
            .path()
            .map(|path| text::name_of(&String::from_utf16_lossy(&path)).to_owned());
        self.name.replace(name.unwrap_or_default());
        self.apply_title(hwnd);
    }

    /// Opens the file again from its start, or from `resume`. The area
    /// says "Reading…" only once the open has taken
    /// [`crate::core::work::BUSY`].
    fn read_file(&self, hwnd: Hwnd) {
        let Some(path) = self.path() else {
            return;
        };
        if let Some(reader) = self.reader.borrow_mut().as_mut() {
            reader.open(path);
        } else {
            return;
        }
        // This open replaces one of `N` or `P`.
        self.opening.set(None);
        self.slow.set(false);
        // An area that already waits, for the files of a selection, keeps
        // its wait, so "Reading…" that shows does not go and come back.
        if !matches!(*self.shown.borrow(), Shown::Reading) {
            self.wait_to_say_reading(hwnd);
        }
        self.set_shown(hwnd, Shown::Reading);
    }

    fn wait_to_say_reading(&self, hwnd: Hwnd) {
        self.reading_since.set(Instant::now());
        hwnd.set_timer(READING_TIMER, crate::core::work::BUSY.as_millis() as u32);
    }

    /// `F2`: opens the file again and shows it where the page shows it once
    /// it opened, and its new end when the page showed the end. The page
    /// stays readable until then, and after Cancel, because the reader keeps
    /// the old file open until the new one opened.
    fn reload(&self, hwnd: Hwnd) {
        let shows = self.shown.borrow().document().is_some();
        match self.path() {
            Some(path) if shows => {
                if let Some(reader) = self.reader.borrow_mut().as_mut() {
                    reader.open(path);
                }
                // This open replaces one of `N` or `P`.
                self.opening.set(None);
            }
            _ => self.read_file(hwnd),
        }
    }

    /// The file that `N` and `P` step from, which the menus' Next file and
    /// Previous file enable from: the one that opens, so held keys advance
    /// on a slow share, else the one that shows.
    fn stepped_from(&self) -> usize {
        self.opening.get().unwrap_or(self.index.get())
    }

    /// `N` and `P`: shows the next or the previous of the files. The page,
    /// its title and its commands stay with the file that shows until the
    /// next one opened, as Reload keeps them, so the area never shows
    /// "Reading…" for the moment an open takes ([`Self::read_events`]).
    fn step_file(&self, hwnd: Hwnd, forward: bool) {
        let count = self.files.borrow().len();
        let from = self.stepped_from();
        let next = if forward {
            from + 1
        } else {
            let Some(previous) = from.checked_sub(1) else {
                return;
            };
            previous
        };
        if next >= count {
            return;
        }
        let shows = self.shown.borrow().document().is_some();
        // Back to the file that shows: its page stays where it is.
        if shows && self.opening.get().is_some() && next == self.index.get() {
            self.cancel_read(hwnd);
            return;
        }
        if !shows {
            self.index.set(next);
            self.resume.set(None);
            self.show_file(hwnd);
            return;
        }
        // Copy and Find that wait would act on the next file once it opened.
        self.asked.borrow_mut().clear();
        let path = self.files.borrow().get(next).cloned();
        if let (Some(path), Some(reader)) = (path, self.reader.borrow_mut().as_mut()) {
            reader.open(path);
            self.opening.set(Some(next));
        }
    }

    /// Keeps the page's place for the next open of its file.
    fn remember_place(&self) {
        if let Some(document) = self.shown.borrow().document() {
            self.resume.set(Some(document.reopening()));
        }
    }

    fn set_shown(&self, hwnd: Hwnd, shown: Shown) {
        // Copy and Find wait for a file that shows or opens, and no other.
        if !matches!(shown, Shown::File(_) | Shown::Reading) {
            self.asked.borrow_mut().clear();
        }
        self.shown.replace(shown);
        self.show_message();
        self.layout(hwnd);
    }

    /// Creates the fonts for the window's DPI and text size and gives them
    /// to the controls.
    fn apply_fonts(&self, hwnd: Hwnd) {
        let Some(c) = self.controls.get() else {
            return;
        };
        let Ok(ui) = Font::message(hwnd.dpi()) else {
            return;
        };
        for control in [c.message.hwnd(), c.retry.hwnd(), c.status.hwnd()] {
            control.set_font(&ui);
        }
        // At the height of the message font, which the message now has.
        let text =
            Font::monospace_of(c.message.hwnd()).and_then(|font| font.scaled(self.zoom.get()));
        let Ok(text) = text else {
            return;
        };
        let (char_width, line_height) = {
            let meter = TextMeter::new(&text);
            (meter.width("0"), meter.height())
        };
        // The controls hold the old fonts until they get the new ones.
        self.fonts.replace(Some(Fonts {
            ui,
            text,
            char_width,
            line_height,
        }));
    }

    /// The title, the menus and Retry in the window's language.
    fn apply_strings(&self, hwnd: Hwnd) {
        if self.controls.get().is_none() {
            return;
        }
        self.apply_title(hwnd);
        self.apply_glyphs(hwnd);
        self.show_message();
        self.show();
    }

    fn apply_title(&self, hwnd: Hwnd) {
        let file = (self.index.get(), self.files.borrow().len());
        hwnd.set_text(&title(
            &self.strings.borrow(),
            &*self.numbers.borrow(),
            &self.name.borrow(),
            file,
        ));
    }

    /// Gives the menus and Retry their icons, at the window's DPI, in the
    /// system colours of now and only while icons show.
    fn apply_glyphs(&self, hwnd: Hwnd) {
        let _ = replace_menu_bar(hwnd, &menu_bar(&self.strings.borrow()));
        if let Some(c) = self.controls.get() {
            c.retry
                .set_glyph(captions::button_glyph(self.message_button()));
        }
    }

    fn show_message(&self) {
        let Some(c) = self.controls.get() else {
            return;
        };
        let shown = self.shown.borrow();
        let quiet = matches!(*shown, Shown::Reading)
            && self.reading_since.get().elapsed() < crate::core::work::BUSY;
        let message = if self.slow.get() {
            // The wait is for the file that opens.
            let opens = self.opening.get().unwrap_or(self.index.get());
            let path = (self.files.borrow().get(opens)).map(|path| String::from_utf16_lossy(path));
            Some((waiting(&self.strings.borrow(), path.as_deref()), true))
        } else if quiet {
            None
        } else {
            shown.message(&self.strings.borrow(), &self.name.borrow())
        };
        drop(shown);
        let (text, retry) = message.clone().unwrap_or_default();
        let key = self.message_button();
        let caption = self.strings.borrow().text(key).to_owned();
        c.retry.hwnd().set_text(&caption);
        c.retry.set_glyph(captions::button_glyph(key));
        c.message.hwnd().set_text(&text);
        c.message.hwnd().set_visible(message.is_some());
        if !retry && Hwnd::focused() == Some(c.retry.hwnd()) {
            c.area.focus();
        }
        c.retry.hwnd().set_visible(retry);
    }

    fn message_button(&self) -> &'static str {
        if self.slow.get() {
            "button.cancel"
        } else {
            "panel.retry"
        }
    }

    /// Places the controls, and fits the page to the area.
    fn layout(&self, hwnd: Hwnd) {
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let Some(c) = self.controls.get() else {
            return;
        };
        let size = hwnd.client_size();
        // Minimized: the page stays as it was.
        if size.width.0 == 0 || size.height.0 == 0 {
            self.poll_read(hwnd);
            return;
        }
        let dpi = hwnd.dpi();
        let status_height = c.status.fit();
        let area = Rect {
            x: Px(0),
            y: Px(0),
            width: size.width,
            height: (size.height - status_height).max(Px(0)),
        };
        let (area, message, retry, place_width, char_width, line_height) = {
            let fonts = self.fonts.borrow();
            let Some(fonts) = fonts.as_ref() else {
                return;
            };
            let meter = TextMeter::new(&fonts.ui);
            let strings = self.strings.borrow();
            let widest = place_samples(&strings, &*self.numbers.borrow())
                .iter()
                .map(|sample| meter.width(sample))
                .max()
                .unwrap_or_default();
            let text = c.message.hwnd().text();
            let button_width =
                image_button_width(meter.width(strings.text(self.message_button())), dpi);
            let (mut message, mut retry) = viewer_message(
                area,
                meter.wrapped_height(&text, viewer_message_width(area, dpi)),
                meter.height(),
                button_width,
                dpi,
            );
            let mut page = area;
            if self.slow.get() && self.shown.borrow().document().is_some() {
                let button = Size {
                    width: button_width,
                    height: retry.height,
                };
                let (_, room, _) = waiting_layout(area, Px(0), button, dpi);
                (page, message, retry) =
                    waiting_layout(area, meter.wrapped_height(&text, room.width), button, dpi);
            }
            (
                page,
                message,
                retry,
                status_last_part(widest, dpi),
                fonts.char_width,
                fonts.line_height,
            )
        };
        win::move_children(&[
            (c.area, area),
            (c.message.hwnd(), message),
            (c.retry.hwnd(), retry),
        ]);
        c.status.set_parts(&[(size.width - place_width).max(Px(0))]);
        let (columns, rows) = text_room(c.area.client_size(), char_width, line_height, dpi);
        if let Some(document) = self.shown.borrow_mut().document_mut() {
            document.set_area(columns, rows);
        }
        self.pump();
    }

    /// Goes on with the page as far as the kept blocks allow, asks for the
    /// block it waits for, and shows the page.
    fn pump(&self) {
        let waits = self
            .shown
            .borrow_mut()
            .document_mut()
            .and_then(|document| document.advance(&code_page_text));
        if let Some(index) = waits
            && let Some(reader) = self.reader.borrow_mut().as_mut()
        {
            reader.read(index);
        }
        self.show();
        if let Some(hwnd) = self.hwnd.get() {
            self.poll_read(hwnd);
        }
    }

    fn poll_read(&self, hwnd: Hwnd) {
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let state = self
            .reader
            .borrow()
            .as_ref()
            .and_then(|reader| reader.waiting(Instant::now()));
        let pending = state.is_some();
        if self.read_timer.replace(pending) != pending {
            if pending {
                hwnd.set_timer(READ_TIMER, 250);
            } else {
                hwnd.kill_timer(READ_TIMER);
            }
        }
        let slow = state == Some(Waiting::Slow);
        if self.slow.replace(slow) != slow {
            self.show_message();
            self.layout(hwnd);
        }
    }

    /// Cancels the pending read, and Copy and Find that wait for it;
    /// `false` when none is pending, or while the Viewer looks for its
    /// files, without which it has nothing to show.
    fn cancel_read(&self, hwnd: Hwnd) -> bool {
        if self.files.borrow().is_empty()
            || !self
                .reader
                .borrow_mut()
                .as_mut()
                .is_some_and(Reader::cancel)
        {
            return false;
        }
        self.asked.borrow_mut().clear();
        self.slow.set(false);
        // A cancelled `N` or `P` stays on the file whose page shows.
        self.opening.set(None);
        {
            let mut shown = self.shown.borrow_mut();
            if let Some(document) = shown.document_mut() {
                document.cancel();
            } else {
                *shown = Shown::Cancelled;
            }
        }
        self.show_message();
        self.layout(hwnd);
        true
    }

    /// Shows the page, its scroll bar, its caret and the status bar.
    fn show(&self) {
        let Some(c) = self.controls.get() else {
            return;
        };
        let shown = self.shown.borrow();
        let document = shown.document();
        let strings = self.strings.borrow();
        let numbers = self.numbers.borrow();
        let name = self.name.borrow();
        set_area_scroll(
            c.area,
            document.map_or(ScrollBar::NONE, Document::scroll_bar),
        );
        c.status
            .set_text(0, &file_text(&strings, &*numbers, &name, document));
        let place = document.map(|document| document.place(&strings, &*numbers));
        c.status.set_text(1, &place.unwrap_or_default());
        c.area.invalidate();
        if let (Some(hwnd), Some(fonts)) = (self.hwnd.get(), self.fonts.borrow().as_ref()) {
            let (left, top) = text_inset(hwnd.dpi());
            let at = |count: usize, unit: Px| {
                Px(i32::try_from(count)
                    .unwrap_or(i32::MAX)
                    .saturating_mul(unit.0))
            };
            let caret = document
                .and_then(Document::caret_cell)
                .map(|(row, column)| {
                    (
                        left + at(column, fonts.char_width),
                        top + at(row, fonts.line_height),
                    )
                });
            set_caret(c.area, caret, fonts.line_height, hwnd.dpi());
        }
    }

    /// Paints the area; `false` before the window has its controls.
    fn paint(&self, hwnd: Hwnd) -> bool {
        let Some(c) = self.controls.get() else {
            return false;
        };
        let fonts = self.fonts.borrow();
        let Some(fonts) = fonts.as_ref() else {
            return false;
        };
        let shown = self.shown.borrow();
        let rows = shown.document().map(Document::shown).unwrap_or_default();
        paint_area(
            c.area,
            &AreaText {
                font: &fonts.text,
                rows: &rows,
                line_height: fonts.line_height,
                char_width: fonts.char_width,
                inset: text_inset(hwnd.dpi()),
            },
        );
        true
    }

    /// Applies what the reading thread found.
    fn read_events(&self, hwnd: Hwnd) {
        let registration = self
            .reader
            .borrow_mut()
            .as_mut()
            .and_then(Reader::take_registration);
        if let Some(request) = registration {
            super::work::viewers::register(request, hwnd);
        }
        if self.hwnd.get() != Some(hwnd) {
            return;
        }
        let event = self.reader.borrow_mut().as_mut().and_then(Reader::take);
        if let Some(event) = event {
            let was_slow = self.slow.replace(false);
            // After Copy or Find, the page asks again for the block whose
            // read it replaced.
            let go_on = || {
                if was_slow {
                    self.show_message();
                    self.layout(hwnd);
                } else {
                    self.pump();
                }
            };
            match event {
                ViewerEvent::Files { paths, first } => {
                    let found = !paths.is_empty();
                    self.files.replace(paths);
                    self.index.set(first);
                    if found {
                        self.show_file(hwnd);
                    } else {
                        self.set_shown(hwnd, Shown::NoFiles);
                    }
                }
                ViewerEvent::Opened { len, first } => {
                    // Reload keeps a page while the file opens, at whose
                    // place the file shows; `N` and `P` keep the page of
                    // another file, and show the new one from its start.
                    let resume = self.resume.take();
                    let at = match self.opening.take() {
                        Some(next) => {
                            // The place kept is the page's, of another file.
                            self.index.set(next);
                            self.name_file(hwnd);
                            None
                        }
                        None => (self.shown.borrow().document())
                            .map(Document::reopening)
                            .or(resume),
                    };
                    let mut document = Document::new(len, first, self.wrap.get());
                    if let Some(at) = at {
                        document.reopen(at);
                    }
                    self.set_shown(hwnd, Shown::File(Box::new(document)));
                }
                ViewerEvent::Block { index, bytes } => {
                    if let Some(document) = self.shown.borrow_mut().document_mut() {
                        document.receive(index, bytes);
                    }
                    go_on();
                }
                ViewerEvent::Copied(copied) => {
                    go_on();
                    if let Err(failure) = copied {
                        let text = failure.message(&self.strings.borrow());
                        self.error_box(hwnd, &text);
                    }
                }
                ViewerEvent::Found {
                    encoding,
                    looked_for,
                    found,
                } => {
                    // An answer of another encoding has other places in the
                    // text, and says nothing about the text that shows.
                    let missing = match self.shown.borrow_mut().document_mut() {
                        Some(document) if document.encoding() == encoding => match found {
                            Some(found) => {
                                document.show_found(found);
                                false
                            }
                            None => true,
                        },
                        _ => false,
                    };
                    go_on();
                    if missing {
                        self.not_found(hwnd, &looked_for);
                    }
                }
                ViewerEvent::Failed { opened, reason } => {
                    // The file that `N` or `P` opens could not open: the
                    // message names it, and Retry opens it from its start.
                    // The open replaced every read of the page kept
                    // meanwhile, so this failure is the open's.
                    if let Some(next) = self.opening.take() {
                        self.index.set(next);
                        self.resume.set(None);
                        self.name_file(hwnd);
                    } else {
                        self.remember_place();
                    }
                    self.set_shown(hwnd, Shown::Failed { opened, reason });
                }
            }
            self.start_asked(hwnd);
        }
        self.poll_read(hwnd);
    }

    /// Windows asks `removal` of the file's drive (DESIGN.md: The Viewer);
    /// `None` when `notification` is not this Viewer's.
    fn removal(&self, hwnd: Hwnd, notification: isize, removal: Removal) -> Option<LRESULT> {
        let ours = self
            .reader
            .borrow()
            .as_ref()
            .is_some_and(|reader| reader.is_registration(notification));
        if !ours {
            return None;
        }
        let mut allowed = true;
        match removal {
            // The release ends an open of `N` or `P` too; the page shown is
            // the file that opens again.
            Removal::Query => {
                self.remember_place();
                self.opening.set(None);
                if let Some(reader) = self.reader.borrow_mut().as_mut() {
                    allowed = reader.query_remove();
                }
                self.set_shown(hwnd, Shown::Removing);
            }
            // Another program kept the drive, so the file can show again.
            Removal::Failed => {
                let reopen = self
                    .reader
                    .borrow()
                    .as_ref()
                    .is_some_and(Reader::removal_failed);
                if reopen {
                    self.read_file(hwnd);
                }
            }
            // A drive pulled out without asking sends only this.
            Removal::Complete => {
                self.remember_place();
                self.opening.set(None);
                if let Some(reader) = self.reader.borrow_mut().as_mut() {
                    reader.release(false);
                }
                self.set_shown(hwnd, Shown::Removed);
            }
        }
        Some(LRESULT(if allowed {
            1
        } else {
            windows::Win32::UI::WindowsAndMessaging::BROADCAST_QUERY_DENY as isize
        }))
    }

    fn on_key(&self, hwnd: Hwnd, key: ViewerKey) {
        match key {
            ViewerKey::Cancel => {
                if !self.cancel_read(hwnd) {
                    self.close(hwnd);
                }
            }
            ViewerKey::Command(command) => self.on_command(hwnd, command),
            ViewerKey::Move { motion, extend } => {
                if let Some(document) = self.shown.borrow_mut().document_mut() {
                    document.resume();
                    document.go(motion, extend);
                }
                self.pump();
            }
        }
    }

    fn on_command(&self, hwnd: Hwnd, command: ViewerCommand) {
        if let Some(document) = self.shown.borrow_mut().document_mut() {
            document.resume();
        }
        match command {
            ViewerCommand::NextFile => self.step_file(hwnd, true),
            ViewerCommand::PreviousFile => self.step_file(hwnd, false),
            ViewerCommand::Reload => self.reload(hwnd),
            ViewerCommand::Close => return self.close(hwnd),
            ViewerCommand::Copy => {
                let publication = Publication::new(clipboard::sequence());
                self.ask(hwnd, Asked::Copy(publication));
            }
            ViewerCommand::SelectAll => self.change_document(Document::select_all),
            ViewerCommand::Find => {
                if let Some(query) = self.ask_query(hwnd) {
                    let backward = false;
                    self.ask(hwnd, Asked::Find { query, backward });
                }
            }
            ViewerCommand::FindNext => self.find_again(hwnd, false),
            ViewerCommand::FindPrevious => self.find_again(hwnd, true),
            ViewerCommand::Caret => self.change_document(Document::toggle_caret),
            ViewerCommand::Text => self.change_document(|d| d.set_mode(Mode::Text)),
            ViewerCommand::Hex => self.change_document(|d| d.set_mode(Mode::Hex)),
            ViewerCommand::Wrap => self.toggle_wrap(),
            ViewerCommand::ZoomIn => self.set_zoom(hwnd, zoomed(self.zoom.get(), 1)),
            ViewerCommand::ZoomOut => self.set_zoom(hwnd, zoomed(self.zoom.get(), -1)),
            ViewerCommand::ZoomReset => self.set_zoom(hwnd, DEFAULT_ZOOM),
            ViewerCommand::Encoding(encoding) => {
                self.change_document(|d| d.choose_encoding(encoding));
            }
        }
        self.pump();
    }

    /// Has `change` change the shown file, while one shows.
    fn change_document(&self, change: impl FnOnce(&mut Document)) {
        if let Some(document) = self.shown.borrow_mut().document_mut() {
            change(document);
        }
    }

    /// Keeps a change to the Viewer's state for the next Viewer, which the
    /// main window saves.
    fn save_state(&self, change: impl FnOnce(&mut ViewerState)) {
        if let Some(shared) = self.shared.upgrade() {
            let mut state = shared.state.get();
            change(&mut state);
            shared.state.set(state);
            shared.changed.set(true);
            self.main.post_deferred();
        }
    }

    /// Wrap long lines on or off, which the next Viewer opens with; in Hex
    /// it does nothing, as its menu item is disabled there.
    fn toggle_wrap(&self) {
        let mode = self.shown.borrow().document().map(Document::mode);
        if mode == Some(Mode::Hex) {
            return;
        }
        let wrap = !self.wrap.get();
        self.wrap.set(wrap);
        self.change_document(|d| d.set_wrap(wrap));
        self.save_state(|state| state.wrap = wrap);
    }

    /// Shows the text at `zoom` percent, which the next Viewer opens with.
    fn set_zoom(&self, hwnd: Hwnd, zoom: u16) {
        if self.zoom.replace(zoom) == zoom {
            return;
        }
        self.apply_fonts(hwnd);
        self.layout(hwnd);
        self.save_state(|state| state.zoom = zoom);
    }

    /// The Find dialog, which shows what Find looked for last; what the
    /// person chose, or `None` for Cancel.
    fn ask_query(&self, hwnd: Hwnd) -> Option<Query> {
        let shared = self.shared.upgrade()?;
        let last = shared.query.borrow().clone().unwrap_or_default();
        let dialog = ViewerFind::new(last);
        let captions = DialogCaptions::new(DialogId::ViewerFind, &self.strings.borrow());
        if run_modal(hwnd, DialogId::ViewerFind, &captions, &dialog).ok()? != DialogEnd::Ok {
            return None;
        }
        let query = dialog.chosen();
        shared.query.replace(Some(query.clone()));
        Some(query)
    }

    /// `F3` and `Shift+F3`: looks for what Find looked for last, or asks
    /// first when it looked for nothing yet.
    fn find_again(&self, hwnd: Hwnd, backward: bool) {
        let last = self
            .shared
            .upgrade()
            .and_then(|shared| shared.query.borrow().clone());
        if let Some(query) = last.or_else(|| self.ask_query(hwnd)) {
            self.ask(hwnd, Asked::Find { query, backward });
        }
    }

    /// Keeps `asked` until the reader can start it, after the Copy and Find
    /// that wait already; a newer Copy, or a newer Find, replaces one that
    /// waits.
    fn ask(&self, hwnd: Hwnd, asked: Asked) {
        // While `N` or `P` opens another file, the reader reads that one,
        // so Copy and Find of the page that shows do nothing.
        if self.opening.get().is_some() {
            return;
        }
        {
            let mut waiting = self.asked.borrow_mut();
            let kind = std::mem::discriminant(&asked);
            waiting.retain(|each| std::mem::discriminant(each) != kind);
            waiting.push(asked);
        }
        self.start_asked(hwnd);
    }

    /// Starts the Copy and Find that wait, in turn, while the reader can
    /// start them. One that finds no file, or nothing selected, does
    /// nothing.
    fn start_asked(&self, hwnd: Hwnd) {
        loop {
            let free = self
                .reader
                .borrow()
                .as_ref()
                .is_some_and(Reader::can_copy_or_find);
            if !free {
                return;
            }
            let asked = {
                let mut waiting = self.asked.borrow_mut();
                if waiting.is_empty() {
                    return;
                }
                waiting.remove(0)
            };
            match asked {
                Asked::Find { query, backward } => self.find(query, backward),
                Asked::Copy(publication) => self.copy(hwnd, publication),
            }
        }
    }

    /// Has the reader look for `query` from the selected match, or from
    /// the page. It replaces a block read in flight.
    fn find(&self, query: Query, backward: bool) {
        let at = self
            .shown
            .borrow()
            .document()
            .map(|document| document.find_in(backward));
        if let (Some(at), Some(reader)) = (at, self.reader.borrow_mut().as_mut()) {
            reader.find(query, at);
        }
    }

    /// Says that the Find that looked for `looked_for` found nothing; a
    /// newer Find may already have replaced the last query.
    fn not_found(&self, hwnd: Hwnd, looked_for: &str) {
        let (text, title) = {
            let strings = self.strings.borrow();
            (
                strings.format("viewer.not_found", &[looked_for]),
                strings.text("viewer.find_title").to_owned(),
            )
        };
        info_box(hwnd, &text, &title);
    }

    /// Puts the selected text on the clipboard. The reader reads the bytes
    /// it needs and publishes the text, so the window never waits for the
    /// disk or the clipboard (DESIGN.md: The Viewer). `publication` is the
    /// clipboard as it was when the person asked.
    fn copy(&self, hwnd: Hwnd, publication: Publication) {
        let Some(copy) = self.shown.borrow().document().and_then(Document::copy) else {
            return;
        };
        match copy {
            Ok(copy) => {
                if let Some(reader) = self.reader.borrow_mut().as_mut() {
                    reader.copy(copy, publication);
                }
            }
            Err(too_large) => {
                let text = {
                    let strings = self.strings.borrow();
                    let name = self.name.borrow();
                    let size = |limit| text::bytes(&strings, &*self.numbers.borrow(), limit);
                    match too_large {
                        TooLarge::Text(limit) => {
                            strings.format("viewer.copy_too_large_text", &[&name, &size(limit)])
                        }
                        TooLarge::Hex(limit) => {
                            strings.format("viewer.copy_too_large_hex", &[&name, &size(limit)])
                        }
                        TooLarge::Selection(limit) => {
                            strings.format("viewer.copy_too_large_selection", &[&size(limit)])
                        }
                    }
                };
                self.error_box(hwnd, &text);
            }
        }
    }

    /// Where the pointer is on the area's text; `None` before the window
    /// has its controls.
    fn pointer(&self, hwnd: Hwnd) -> Option<TextAt> {
        let point = self.controls.get()?.area.cursor_point()?;
        let fonts = self.fonts.borrow();
        let fonts = fonts.as_ref()?;
        Some(text_at(
            point,
            fonts.char_width,
            fonts.line_height,
            hwnd.dpi(),
        ))
    }

    /// Has `select` change the shown file's selection at the pointer, and
    /// shows the page again when it says the selection changed.
    fn select(&self, hwnd: Hwnd, select: impl FnOnce(&mut Document, TextAt) -> bool) {
        let Some(at) = self.pointer(hwnd) else {
            return;
        };
        let changed = self
            .shown
            .borrow_mut()
            .document_mut()
            .is_some_and(|document| select(document, at));
        if changed {
            self.show();
        }
    }

    /// The left button went down on the area: the selection starts there,
    /// or reaches there with Shift, and a drag over text follows the
    /// pointer, with the mouse held until the button goes up.
    fn press(&self, hwnd: Hwnd) {
        self.select(hwnd, |document, at| {
            document.press(at.row, at.place, window::shift_held());
            self.dragging.set(document.selects_text());
            true
        });
        if self.dragging.get() {
            hwnd.capture_mouse();
            hwnd.set_timer(SCROLL_TIMER, 50);
        }
    }

    fn drag(&self, hwnd: Hwnd) {
        self.select(hwnd, |document, at| document.drag(at.row, at.place));
    }

    /// While a drag holds the pointer beyond an edge of the area, scrolls
    /// one row or column toward it, as an edit control does, and selects
    /// to the pointer on the text that scrolled in.
    fn autoscroll(&self, hwnd: Hwnd) {
        let area = self.controls().area;
        let Some((x, y)) = area.cursor_point() else {
            return;
        };
        let size = area.client_size();
        let (above, below) = (y < 0, y >= size.height.0);
        let (before, after) = (x < 0, x >= size.width.0);
        if !(above || below || before || after) {
            return;
        }
        if let Some(document) = self.shown.borrow_mut().document_mut() {
            document.resume();
            if above {
                document.line_up();
            } else if below {
                document.line_down();
            }
            if before {
                document.scroll_left();
            } else if after {
                document.scroll_right();
            }
        }
        self.pump();
        self.drag(hwnd);
    }

    fn select_word(&self, hwnd: Hwnd) {
        self.select(hwnd, |document, at| {
            document.select_word(at.row, at.cell);
            true
        });
    }

    /// A message box about Copy, which its title names.
    fn error_box(&self, hwnd: Hwnd, text: &str) {
        let title = text::without_access_key(self.strings.borrow().text("command.copy"));
        error_box(Some(hwnd), text, &title);
    }

    /// Activates the main window, which keeps its focused row, so `F3`,
    /// `Esc`, `Down`, `F3` walks through the files; Windows would
    /// otherwise activate whichever window was behind the Viewer. A main
    /// window the person minimized stays minimized.
    fn close(&self, hwnd: Hwnd) {
        if self.main.is_on_screen() {
            self.main.bring_to_front();
        }
        hwnd.destroy();
    }

    /// Keeps the shown window's size and place for the next Viewer.
    fn remember_placement(&self, hwnd: Hwnd) {
        if !hwnd.is_visible() {
            return;
        }
        if let Some(placement) = hwnd.placement() {
            self.save_state(|state| state.placement = Some(placement));
        }
    }
}

impl WindowProc for Viewer {
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
                self.apply_glyphs(hwnd);
                self.layout(hwnd);
                Some(LRESULT(0))
            }
            Message::Other(AREA_PAINT) => Some(LRESULT(isize::from(self.paint(hwnd)))),
            Message::Other(AREA_CLICKED) => {
                self.press(hwnd);
                Some(LRESULT(0))
            }
            Message::Other(AREA_DOUBLE_CLICKED) => {
                self.select_word(hwnd);
                Some(LRESULT(0))
            }
            // The caret shows only while the area has the focus.
            Message::Other(AREA_FOCUSED) => {
                self.show();
                Some(LRESULT(0))
            }
            Message::MouseMove { .. } if self.dragging.get() => {
                self.drag(hwnd);
                Some(LRESULT(0))
            }
            Message::LeftButtonUp if self.dragging.get() => {
                window::release_mouse();
                Some(LRESULT(0))
            }
            Message::CaptureChanged => {
                if self.dragging.replace(false) {
                    hwnd.kill_timer(SCROLL_TIMER);
                }
                None
            }
            // The I-beam over text that a drag selects, the arrow in Hex.
            Message::SetCursor { over, client: true }
                if Some(over) == self.controls.get().map(|c| c.area)
                    && self
                        .shown
                        .borrow()
                        .document()
                        .is_some_and(Document::selects_text) =>
            {
                window::show_text_cursor();
                Some(LRESULT(1))
            }
            Message::Wake => {
                if let Some(waker) = self.waker.get() {
                    waker.woken();
                }
                self.read_events(hwnd);
                Some(LRESULT(0))
            }
            Message::Timer(READ_TIMER) => {
                self.poll_read(hwnd);
                Some(LRESULT(0))
            }
            Message::Timer(READING_TIMER) => {
                let ran = self.reading_since.get().elapsed();
                match crate::core::work::BUSY.checked_sub(ran) {
                    Some(left) if !left.is_zero() => {
                        hwnd.set_timer(READING_TIMER, left.as_millis().max(1) as u32);
                    }
                    _ => {
                        hwnd.kill_timer(READING_TIMER);
                        self.show_message();
                        self.layout(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            Message::Timer(SCROLL_TIMER) => {
                if self.dragging.get() {
                    self.autoscroll(hwnd);
                }
                Some(LRESULT(0))
            }
            Message::KeyDown {
                keystroke, repeat, ..
            } => {
                let key = viewer_key(keystroke)?;
                if !repeat {
                    self.pressed.set(Some(keystroke));
                }
                // A held Esc would otherwise close the Viewers behind this
                // one too, and a held F3 that opened this Viewer would ask
                // what to find, or find again, until it is released.
                let ignored = repeat
                    && match key {
                        ViewerKey::Cancel | ViewerKey::Command(ViewerCommand::Close) => true,
                        ViewerKey::Command(
                            ViewerCommand::FindNext | ViewerCommand::FindPrevious,
                        ) => self.pressed.get() != Some(keystroke),
                        ViewerKey::Command(_) | ViewerKey::Move { .. } => false,
                    };
                if !ignored {
                    self.on_key(hwnd, key);
                }
                Some(LRESULT(1))
            }
            Message::MouseWheel { delta, .. } if window::ctrl_held() => {
                let turned = self.zoom_wheel.get() + delta;
                self.zoom_wheel.set(turned % WHEEL_DELTA);
                let steps = turned / WHEEL_DELTA;
                if steps != 0 {
                    self.set_zoom(hwnd, zoomed(self.zoom.get(), steps));
                }
                Some(LRESULT(0))
            }
            Message::MouseWheel { delta, .. } => {
                if let Some(document) = self.shown.borrow_mut().document_mut() {
                    document.resume();
                    document.wheel(delta);
                }
                self.pump();
                Some(LRESULT(0))
            }
            Message::VerticalScroll { from, request } => {
                if from != self.controls.get()?.area {
                    return None;
                }
                if let Some(document) = self.shown.borrow_mut().document_mut() {
                    document.resume();
                    use crate::core::layout::ScrollRequest as R;
                    match request {
                        R::LineUp => document.line_up(),
                        R::LineDown => document.line_down(),
                        R::PageUp => document.page_up(),
                        R::PageDown => document.page_down(),
                        R::Top => document.home(),
                        R::Bottom => document.end(),
                        R::To(position) => document.jump(position.0),
                        R::Wheel(delta) => document.wheel(delta),
                    }
                }
                self.pump();
                Some(LRESULT(0))
            }
            Message::DeviceRemoval {
                removal,
                notification,
            } => self.removal(hwnd, notification, removal),
            Message::InitMenuPopup => {
                let state = {
                    let shown = self.shown.borrow();
                    let document = shown.document();
                    MenuState {
                        shown: document.map(|d| (d.mode(), d.encoding())),
                        wrap: self.wrap.get(),
                        selected: document.is_some_and(Document::selected),
                        caret: document.is_some_and(Document::has_caret),
                        can_toggle_caret: document.is_some_and(Document::can_toggle_caret),
                        file: (self.stepped_from(), self.files.borrow().len()),
                        zoom: self.zoom.get(),
                    }
                };
                if let Some(menu) = Menu::of(hwnd) {
                    for (id, enabled, checked) in menu_states(state) {
                        menu.set_state(id, enabled, checked);
                    }
                }
                Some(LRESULT(0))
            }
            Message::Command(Command::Menu(id)) => {
                self.on_command(hwnd, ViewerCommand::from_id(id)?);
                Some(LRESULT(0))
            }
            Message::Command(Command::Control(event)) => {
                let button = self.controls.get()?.retry;
                if button.clicked(event) && button.hwnd().is_visible() {
                    if !self.slow.get() {
                        self.read_file(hwnd);
                    } else if !self.cancel_read(hwnd) {
                        self.close(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            Message::CtlColorStatic { colors, control }
                if control == self.controls.get()?.message.hwnd() =>
            {
                Some(colors.window_background())
            }
            Message::Deactivate => {
                self.focused
                    .set(Hwnd::focused().filter(|focused| hwnd.is_parent_of(*focused)));
                None
            }
            Message::Activate { .. } => {
                let area = self.controls.get()?.area;
                let focus = self
                    .focused
                    .take()
                    .filter(|focused| focused.is_visible())
                    .unwrap_or(area);
                focus.focus();
                Some(LRESULT(0))
            }
            Message::SysColorChange => {
                self.apply_glyphs(hwnd);
                if let Some(c) = self.controls.get() {
                    c.area.invalidate();
                }
                None
            }
            Message::SettingChange {
                regional_format: true,
            } => {
                self.numbers.replace(WindowsNumbers::new());
                self.apply_title(hwnd);
                self.layout(hwnd);
                None
            }
            Message::Close => {
                self.close(hwnd);
                Some(LRESULT(0))
            }
            Message::Destroy => {
                self.remember_placement(hwnd);
                self.hwnd.set(None);
                hwnd.kill_timer(READ_TIMER);
                hwnd.kill_timer(SCROLL_TIMER);
                hwnd.kill_timer(READING_TIMER);
                if let Some(waker) = self.waker.get() {
                    waker.close();
                }
                self.reader.replace(None);
                window::own_keys(hwnd, false);
                if let Some(shared) = self.shared.upgrade() {
                    shared
                        .open
                        .borrow_mut()
                        .retain(|viewer| !std::ptr::eq(Rc::as_ptr(viewer), self));
                }
                None
            }
            _ => None,
        }
    }
}
