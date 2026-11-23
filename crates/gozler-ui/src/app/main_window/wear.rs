//! The copy warning: while "Warn me when a folder is copied into itself" is
//! on, the first window holds one wear request, also while it is hidden, and
//! shows the folders of each reply in the SSD wear window. That window has no
//! owner and a taskbar button of its own, so it shows while the main window
//! is hidden in the notification area; it shows without taking the focus and
//! flashes its button. Show shows the selected folder in Commander.

use super::{Hwnd, Instance, MainWindow, Timer};
use crate::app::work::wear::Stop;
use crate::core::captions::DialogCaptions;
use crate::core::ids::{ControlId, DialogId, IconId};
use crate::core::startup::Request as StartupRequest;
use crate::core::text::{Numbers, Strings};
use crate::core::wear::Warnings;
use crate::win::controls::ReportList;
use crate::win::dialog::{self, DialogMessage, DialogProc, Opened};
use crate::win::window;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

/// How long a failed wear request waits before it is sent again.
const RETRY_MS: u32 = 5 * 60 * 1_000;

pub(in crate::app) struct Wear {
    shared: Rc<Shared>,
    service: RefCell<Service>,
}

/// Whether the service takes the wear request.
enum Service {
    /// It takes it, or has not answered yet.
    Accepts,
    /// It is older than the window and refused it. `version` is the one
    /// status named then, or `None` when status had named none; a status
    /// reply that names another version starts the watch again, and the
    /// first that names one when there is none only records it.
    Refuses { version: Option<String> },
}

/// What the main window and the open SSD wear window both reach.
#[derive(Default)]
struct Shared {
    warnings: RefCell<Warnings>,
    dialog: Cell<Option<Hwnd>>,
    /// Each row's cells as the list shows them.
    shown: RefCell<Vec<[String; 2]>>,
}

impl Wear {
    pub(super) fn new() -> Self {
        Self {
            shared: Rc::default(),
            service: RefCell::new(Service::Accepts),
        }
    }

    /// Whether Settings shows "Needs a newer Gozler service".
    pub(in crate::app) fn refused(&self) -> bool {
        matches!(*self.service.borrow(), Service::Refuses { .. })
    }

    /// Opens the window, hidden, with the list's headers in `strings`.
    fn open(&self, strings: &Strings, main: Hwnd) -> windows::core::Result<Hwnd> {
        let state = Rc::new(WearDialog {
            shared: Rc::clone(&self.shared),
            main,
        });
        let captions = DialogCaptions::new(DialogId::Wear, strings);
        let dialog = dialog::create_modeless(DialogId::Wear, captions, state)?;
        Self::set_headers(dialog, strings);
        window::own_keys(dialog, true);
        self.shared.dialog.set(Some(dialog));
        Ok(dialog)
    }

    fn set_headers(dialog: Hwnd, strings: &Strings) {
        if let Some(list) = ReportList::of_dialog(dialog, ControlId::WearList.id()) {
            list.set_header(0, strings.text("wear.folder"));
            list.set_header(1, strings.text("wear.copies_column"));
        }
    }

    /// Words the open window in the language of `strings`.
    pub(in crate::app) fn set_strings(&self, strings: &Strings) {
        let Some(dialog) = self.shared.dialog.get() else {
            return;
        };
        let captions = DialogCaptions::new(DialogId::Wear, strings);
        dialog.set_text(&captions.title);
        for (id, text) in &captions.items {
            if let Some(control) = dialog::item(dialog, *id) {
                control.set_text(text);
            }
        }
        Self::set_headers(dialog, strings);
    }

    /// The folder of the selected row, which Show shows.
    fn selected(&self) -> Option<String> {
        let dialog = self.shared.dialog.get()?;
        let row = ReportList::of_dialog(dialog, ControlId::WearList.id())?.selected()?;
        let warnings = self.shared.warnings.borrow();
        warnings.rows().get(row).map(|row| row.top.clone())
    }

    /// Closes the window, as Exit does.
    pub(super) fn close(&self) {
        if let Some(dialog) = self.shared.dialog.get() {
            dialog.destroy();
        }
    }
}

impl MainWindow {
    /// Starts the watch, or stops it, as the setting says; only the first
    /// window watches. Turning it on again starts from now, so no folder
    /// copied while it was off warns.
    pub(super) fn follow_wear(&self, hwnd: Hwnd) {
        let on = self.instance() == Instance::First && self.model.borrow().choices().warn_repeats;
        hwnd.kill_timer(Timer::WearRetry.id());
        let before = self.wear.service.replace(Service::Accepts);
        if matches!(before, Service::Refuses { .. }) {
            self.settings.post_settings_reply();
        }
        let mut work = self.created().work.borrow_mut();
        if on {
            work.wear.start();
        } else {
            work.wear.stop();
        }
    }

    /// Takes the wear request's answer: shows the folders of a reply,
    /// sends a failed request again after [`RETRY_MS`], and stops the
    /// watch that an older service refuses.
    pub(super) fn take_wear(&self, hwnd: Hwnd) {
        let answer = self.created().work.borrow_mut().wear.take();
        match answer {
            None => {}
            Some(Ok(reply)) => self.warn(hwnd, &reply.events),
            Some(Err(Stop::Failed)) => hwnd.set_timer(Timer::WearRetry.id(), RETRY_MS),
            Some(Err(Stop::Refused)) => {
                self.created().work.borrow_mut().wear.stop();
                let version = self.model.borrow().service_version().map(str::to_owned);
                self.wear.service.replace(Service::Refuses { version });
                self.settings.post_settings_reply();
            }
        }
    }

    /// Starts the watch that a service refused again once a status reply
    /// names another service, such as one that an update installed. The
    /// first version that status names after a refusal that had none is
    /// the refusing service's own, so it starts nothing.
    pub(super) fn follow_service(&self, hwnd: Hwnd) {
        let changed = {
            let mut service = self.wear.service.borrow_mut();
            let model = self.model.borrow();
            match (&mut *service, model.service_version()) {
                (Service::Refuses { version }, Some(now)) => match version {
                    Some(before) => before != now,
                    None => {
                        *version = Some(now.to_owned());
                        false
                    }
                },
                _ => false,
            }
        };
        if changed {
            self.follow_wear(hwnd);
        }
    }

    pub(super) fn retry_wear(&self, hwnd: Hwnd) {
        hwnd.kill_timer(Timer::WearRetry.id());
        self.created().work.borrow_mut().wear.retry();
    }

    /// Shows the folders of `events` that warn: in the open window, in
    /// place, or in a new one. Either flashes the window's taskbar button.
    fn warn(&self, hwnd: Hwnd, events: &[gozler_core::proto::RepeatingFolder]) {
        let shared = &self.wear.shared;
        let changed = shared.warnings.borrow_mut().take(events, Instant::now());
        if !changed {
            return;
        }
        let strings = self.model.borrow().strings().clone();
        let cells: Vec<[String; 2]> = {
            let numbers = self.numbers.borrow();
            let warnings = shared.warnings.borrow();
            let rows = warnings.rows().iter();
            rows.map(|row| [row.top.clone(), numbers.integer(row.copies.into())])
                .collect()
        };
        let dialog = match shared.dialog.get() {
            Some(dialog) => dialog,
            None => match self.wear.open(&strings, hwnd) {
                Ok(dialog) => dialog,
                Err(error) => {
                    // Its folders warn again only when the service updates
                    // their event.
                    shared.warnings.borrow_mut().forget();
                    self.log_error(format!("The SSD wear window did not open: {error}"));
                    return;
                }
            },
        };
        if let Some(list) = ReportList::of_dialog(dialog, ControlId::WearList.id()) {
            let shown = shared.shown.replace(cells.clone());
            list.update_rows(&shown, &cells);
        }
        dialog.show_and_flash();
    }

    /// Show, in the SSD wear window: the selected folder in Commander, in
    /// its parent folder, with the main window in front.
    pub(super) fn show_repeat(&self, hwnd: Hwnd) {
        if let Some(top) = self.wear.selected() {
            self.request(hwnd, StartupRequest::Reveal(top));
        }
    }
}

struct WearDialog {
    shared: Rc<Shared>,
    main: Hwnd,
}

impl DialogProc for WearDialog {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(list) = ReportList::of_dialog(dialog, ControlId::WearList.id()) {
            // The headers come from the caller, in the language of the
            // moment.
            list.set_columns(&[("", 75, false), ("", 25, true)]);
        }
        window::set_icons(dialog, IconId::Gozler.id());
        Opened {
            // No control: focusing one would activate the window, which
            // shows without taking the focus. Windows focuses the first
            // control when the person activates the window.
            focus: 0,
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        let show = ControlId::WearShow.id();
        let list = ControlId::WearList.id();
        match message {
            DialogMessage::Command { id } if id == show => {
                self.main.post(window::WEAR_SHOW);
            }
            DialogMessage::DoubleClicked { id } if id == list => {
                self.main.post(window::WEAR_SHOW);
            }
            DialogMessage::Edited { id } if id == list => {
                let selected = ReportList::of_dialog(dialog, list)
                    .and_then(ReportList::selected)
                    .is_some();
                if let Some(button) = dialog::item(dialog, show) {
                    button.set_enabled(selected);
                }
            }
            // Close, Esc and the title bar's close button.
            DialogMessage::Command { id: 2 } => dialog.destroy(),
            _ => return false,
        }
        true
    }

    fn closed(&self, dialog: Hwnd) {
        window::own_keys(dialog, false);
        self.shared.dialog.set(None);
        self.shared.warnings.borrow_mut().clear();
        self.shared.shown.borrow_mut().clear();
    }
}
