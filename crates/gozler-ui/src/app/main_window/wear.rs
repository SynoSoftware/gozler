//! The SSD wear warnings: while a warning of Settings > SSD wear is on, the
//! first window holds one wear request, also while it is hidden, and shows
//! the folders copied into themselves and the limits crossed of each reply
//! in the SSD wear window. That window has no owner and a taskbar button of
//! its own, so it shows while the main window is hidden in the notification
//! area; it shows without taking the focus and flashes its button. Show
//! shows the selected row's file, folder or program in Commander, or
//! searches a disk's drives for what writes there.

use super::{Hwnd, Instance, MainWindow, Timer};
use crate::app::work::wear::Stop;
use crate::core::captions::DialogCaptions;
use crate::core::ids::{ControlId, DialogId, IconId};
use crate::core::model::Column;
use crate::core::startup::Request as StartupRequest;
use crate::core::text::Strings;
use crate::core::wear::Warnings;
use crate::win::controls::ReportList;
use crate::win::dialog::{self, DialogMessage, DialogProc, Opened};
use crate::win::window;
use gozler_core::index::repeats::EVENT_COPIES;
use gozler_core::proto::WearReply;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

/// How long a failed wear request waits before it is sent again.
const RETRY_MS: u32 = 5 * 60 * 1_000;

/// The list's columns: Name, Kind, Amount and Limit, each with its share of
/// the list's width, and the numbers right-aligned. The headers come from
/// the caller, in the language of the moment.
const COLUMNS: [(&str, i32, bool); 4] = [
    ("", 46, false),
    ("", 16, false),
    ("", 19, true),
    ("", 19, true),
];

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
    shown: RefCell<Vec<[String; 4]>>,
}

impl Shared {
    /// What Show opens for the list's selected row; `None` while no row is
    /// selected, or the row names nothing that opens.
    fn selected(&self, dialog: Hwnd) -> Option<StartupRequest> {
        let row = ReportList::of_dialog(dialog, ControlId::WearList.id())?.selected()?;
        self.warnings.borrow().rows().get(row)?.show()
    }
}

impl Wear {
    pub(super) fn new() -> Self {
        Self {
            shared: Rc::default(),
            service: RefCell::new(Service::Accepts),
        }
    }

    /// Whether Settings shows that the warnings need a newer service.
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
            for (column, key) in ["wear.name", "wear.kind", "wear.amount", "wear.limit"]
                .into_iter()
                .enumerate()
            {
                list.set_header(column, strings.text(key));
            }
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

    /// Closes the window, as Exit does.
    pub(super) fn close(&self) {
        if let Some(dialog) = self.shared.dialog.get() {
            dialog.destroy();
        }
    }
}

impl MainWindow {
    /// Starts the watch, or stops it, as the settings say; only the first
    /// window watches. A change starts it again from now, so nothing from
    /// while it was off, or watched other limits, warns. With the copy
    /// warning off, the request asks for more copies than any folder has.
    pub(super) fn follow_wear(&self, hwnd: Hwnd) {
        let (on, copies, limits) = {
            let model = self.model.borrow();
            let choices = model.choices();
            let copies = if choices.warn_repeats {
                EVENT_COPIES
            } else {
                u32::MAX
            };
            let on = self.instance() == Instance::First && choices.watches_wear();
            (on, copies, choices.wear_alarm.limits())
        };
        hwnd.kill_timer(Timer::WearRetry.id());
        let before = self.wear.service.replace(Service::Accepts);
        if matches!(before, Service::Refuses { .. }) {
            self.settings.post_settings_reply();
        }
        let mut work = self.created().work.borrow_mut();
        if on {
            work.wear.start(copies, limits);
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
            Some(Ok(reply)) => self.warn(hwnd, &reply),
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

    /// Shows the folders and the limits crossed of `reply` that warn: in
    /// the open window, in place, or in a new one. Either flashes the
    /// window's taskbar button.
    fn warn(&self, hwnd: Hwnd, reply: &WearReply) {
        let shared = &self.wear.shared;
        let changed =
            (shared.warnings.borrow_mut()).take(&reply.events, &reply.alarms, Instant::now());
        if !changed {
            return;
        }
        let strings = self.model.borrow().strings().clone();
        let cells: Vec<[String; 4]> = {
            let numbers = self.numbers.borrow();
            let warnings = shared.warnings.borrow();
            let rows = warnings.rows().iter();
            rows.map(|row| row.cells(&strings, &*numbers)).collect()
        };
        let (dialog, opened) = match shared.dialog.get() {
            Some(dialog) => (dialog, false),
            None => match self.wear.open(&strings, hwnd) {
                Ok(dialog) => (dialog, true),
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
            // Again on the list's final size, which the dialog's layout
            // after `init` can change, before the window shows.
            if opened {
                list.fit_columns(COLUMNS.map(|(_, share, _)| share));
            }
        }
        dialog.show_and_flash();
    }

    /// Show, in the SSD wear window, with the main window in front: the
    /// selected file, folder or program in Commander, in its folder, or a
    /// search of the selected disk's drives sorted by Written, with the
    /// Writes and Written columns shown, as View > Columns shows them, so
    /// the amounts show beside the order.
    pub(super) fn show_warning(&self, hwnd: Hwnd) {
        let shared = &self.wear.shared;
        let Some(request) = shared
            .dialog
            .get()
            .and_then(|dialog| shared.selected(dialog))
        else {
            return;
        };
        if matches!(request, StartupRequest::Search(_)) {
            for column in [Column::Writes, Column::Written] {
                let shown = self.model.borrow().columns().contains(&column);
                if !shown {
                    let accepted = self.model.borrow_mut().toggle_column(column);
                    self.apply(hwnd, accepted);
                }
            }
        }
        self.request(hwnd, request);
    }
}

struct WearDialog {
    shared: Rc<Shared>,
    main: Hwnd,
}

impl DialogProc for WearDialog {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(list) = ReportList::of_dialog(dialog, ControlId::WearList.id()) {
            list.set_columns(&COLUMNS);
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
            // Show is on while the selected row names what it opens.
            DialogMessage::Edited { id } if id == list => {
                let opens = self.shared.selected(dialog).is_some();
                if let Some(button) = dialog::item(dialog, show) {
                    button.set_enabled(opens);
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
