//! The small dialogs that Multi-Rename opens: Range…, Edit names, Save…,
//! Regular expressions and Result list.

use crate::app::main_window::MainWindow;
use crate::core::captions::DialogItem;
use crate::core::ids::ControlId as C;
use crate::core::multi_rename::{self, RangeForm};
use crate::core::text::{Numbers, Strings};
use crate::win::Hwnd;
use crate::win::controls::{Balloon, CheckBox, Edit};
use crate::win::dialog::{self, DialogEnd, DialogMessage, DialogProc, Opened};
use crate::win::gdi::Font;
use crate::win::locale::WindowsNumbers;
use std::cell::{OnceCell, RefCell};

/// The timer that shows the selection's placeholder, because an edit
/// control says nothing when its selection changes.
const PREVIEW: usize = 1;

/// Range…: the person selects characters of a name, and OK names them.
pub(super) struct PickRange {
    text: String,
    /// The letter of the placeholder, such as `N`; empty for the full path.
    letter: &'static str,
    /// The placeholder the preview shows, which OK inserts.
    chosen: RefCell<Option<String>>,
}

impl PickRange {
    pub(super) fn new(text: String, letter: &'static str) -> PickRange {
        PickRange {
            text,
            letter,
            chosen: RefCell::new(None),
        }
    }

    /// The placeholder that names the characters OK read; `None` when none
    /// were selected.
    pub(super) fn range(&self) -> Option<String> {
        self.chosen.take()
    }

    /// The placeholder of the selection and the check boxes, in the
    /// preview at the lower left, as Total Commander shows it.
    fn preview(&self, dialog: Hwnd) {
        let checked =
            |id: C| CheckBox::of_dialog(dialog, id.id()).is_some_and(CheckBox::is_checked);
        let form = RangeForm {
            first_from_end: checked(C::PickRangeFirstFromEnd),
            last_from_end: checked(C::PickRangeLastFromEnd),
            length: checked(C::PickRangeLength),
        };
        let range = Edit::of_dialog(dialog, C::PickRangeName.id())
            .and_then(|name| multi_rename::range(self.letter, &self.text, name.selection(), form));
        if *self.chosen.borrow() == range {
            return;
        }
        if let Some(preview) = dialog::item(dialog, C::PickRangePreview.id()) {
            preview.set_text(range.as_deref().unwrap_or_default());
        }
        dialog::set_ok_enabled(dialog, range.is_some());
        *self.chosen.borrow_mut() = range;
    }
}

impl DialogProc for PickRange {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(name) = dialog::item(dialog, C::PickRangeName.id()) {
            name.set_text(&self.text);
        }
        dialog::set_ok_enabled(dialog, false);
        dialog.set_timer(PREVIEW, 100);
        Opened {
            focus: C::PickRangeName.id(),
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        match message {
            DialogMessage::Timer { id: PREVIEW } => {
                self.preview(dialog);
                true
            }
            DialogMessage::Command { id } if id == DialogItem::Ok.id() => {
                self.preview(dialog);
                // OK ends the dialog.
                false
            }
            // A check box.
            DialogMessage::Command { id } if id != DialogItem::Cancel.id() => {
                self.preview(dialog);
                true
            }
            _ => false,
        }
    }

    fn closed(&self, dialog: Hwnd) {
        dialog.kill_timer(PREVIEW);
    }
}

/// Edit names: the rows' new names, one per line, which OK gives back
/// only when there is one for each row.
pub(super) struct EditNames<'a> {
    strings: &'a Strings,
    numbers: &'a WindowsNumbers,
    text: String,
    rows: usize,
    balloon: OnceCell<Balloon>,
    names: RefCell<Option<Vec<String>>>,
}

impl<'a> EditNames<'a> {
    pub(super) fn new(
        strings: &'a Strings,
        numbers: &'a WindowsNumbers,
        text: String,
        rows: usize,
    ) -> EditNames<'a> {
        EditNames {
            strings,
            numbers,
            text,
            rows,
            balloon: OnceCell::new(),
            names: RefCell::new(None),
        }
    }

    /// The names OK read, one for each row in the list's order.
    pub(super) fn names(&self) -> Option<Vec<String>> {
        self.names.take()
    }
}

impl DialogProc for EditNames<'_> {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(text) = Edit::of_dialog(dialog, C::EditNamesText.id()) {
            text.allow_any_length();
            text.hwnd().set_text(&self.text);
        }
        if let Ok(balloon) = Balloon::create(dialog) {
            let _ = self.balloon.set(balloon);
        }
        Opened {
            focus: C::EditNamesText.id(),
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        match message {
            DialogMessage::Edited { .. } => {
                if let Some(balloon) = self.balloon.get() {
                    balloon.hide();
                }
                true
            }
            DialogMessage::Command { id } if id == DialogItem::Ok.id() => {
                let Some(text) = Edit::of_dialog(dialog, C::EditNamesText.id()) else {
                    return false;
                };
                let all = text.text();
                let mut names: Vec<String> = (all.split('\n'))
                    .map(|line| line.trim_end_matches('\r').to_owned())
                    .collect();
                // Empty lines at the end are not names.
                while names.last().is_some_and(String::is_empty) {
                    names.pop();
                }
                if names.len() == self.rows {
                    *self.names.borrow_mut() = Some(names);
                    dialog::end(dialog, DialogEnd::Ok);
                } else if let Some(balloon) = self.balloon.get() {
                    let count = |count: usize| self.numbers.integer(count as u64);
                    let message = self.strings.format(
                        "multi_rename.edit_count",
                        &[&count(names.len()), &count(self.rows)],
                    );
                    balloon.show(text.hwnd(), &message);
                }
                true
            }
            _ => false,
        }
    }
}

/// Save…: the name to keep the fields under.
pub(super) struct SaveName {
    shown: String,
    name: RefCell<Option<String>>,
}

impl SaveName {
    /// `shown` is the name the box starts with.
    pub(super) fn new(shown: String) -> SaveName {
        SaveName {
            shown,
            name: RefCell::new(None),
        }
    }

    /// The name OK read, without spaces around it.
    pub(super) fn name(&self) -> Option<String> {
        self.name.take()
    }

    fn typed(dialog: Hwnd) -> String {
        dialog::item(dialog, C::SaveRenameName.id())
            .map(|name| name.text().trim().to_owned())
            .unwrap_or_default()
    }
}

impl DialogProc for SaveName {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(name) = Edit::of_dialog(dialog, C::SaveRenameName.id()) {
            name.hwnd().set_text(&self.shown);
            name.select_all();
        }
        dialog::set_ok_enabled(dialog, !self.shown.trim().is_empty());
        Opened {
            focus: C::SaveRenameName.id(),
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        match message {
            DialogMessage::Edited { .. } => {
                dialog::set_ok_enabled(dialog, !Self::typed(dialog).is_empty());
                true
            }
            DialogMessage::Command { id } if id == DialogItem::Ok.id() => {
                *self.name.borrow_mut() = Some(Self::typed(dialog));
                // OK ends the dialog.
                false
            }
            _ => false,
        }
    }
}

/// `?`: what Search for and Replace with take when Regular expression is
/// on, as Search syntax shows the search box's.
pub(super) struct RegularExpressions<'a> {
    text: &'a str,
}

impl<'a> RegularExpressions<'a> {
    pub(super) fn new(text: &'a str) -> RegularExpressions<'a> {
        RegularExpressions { text }
    }
}

impl DialogProc for RegularExpressions<'_> {
    fn init(&self, dialog: Hwnd) -> Opened {
        let mut fonts = Vec::new();
        if let Some(text) = dialog::item(dialog, C::RegularExpressionsText.id()) {
            if let Ok(font) = Font::monospace_of(text) {
                text.set_font(&font);
                fonts.push(font);
            }
            // An edit control breaks lines only at "\r\n".
            text.set_text(&self.text.replace('\n', "\r\n"));
        }
        // Close starts with the focus, so the text is not selected and
        // Enter closes the dialog.
        Opened {
            focus: DialogItem::Cancel.id(),
            fonts,
        }
    }

    fn message(&self, _dialog: Hwnd, _message: DialogMessage) -> bool {
        false
    }
}

/// Result list: what the batch that ended last did, one entry a line, with
/// Copy and Close.
pub(super) struct ResultList<'a> {
    text: String,
    /// The main window, whose clipboard worker publishes the copy, and its
    /// window, which times the worker's retries.
    main: &'a MainWindow,
    window: Hwnd,
}

impl<'a> ResultList<'a> {
    pub(super) fn new(text: String, main: &'a MainWindow, window: Hwnd) -> ResultList<'a> {
        ResultList { text, main, window }
    }
}

impl DialogProc for ResultList<'_> {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(text) = Edit::of_dialog(dialog, C::ResultListText.id()) {
            text.allow_any_length();
            text.hwnd().set_text(&self.text);
        }
        Opened {
            focus: DialogItem::Cancel.id(),
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        match message {
            DialogMessage::Command { id } if id == C::ResultListCopy.id() => {
                // The whole list, selected so the person sees what was
                // copied, as the service's report copies.
                if let Some(text) = Edit::of_dialog(dialog, C::ResultListText.id()) {
                    text.select_all();
                    self.main.copy_text(self.window, text.selected_text());
                }
                true
            }
            _ => false,
        }
    }
}
