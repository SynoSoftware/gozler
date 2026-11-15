//! The small dialogs that Multi-Rename opens: Range…, Edit names and
//! Save….

use crate::core::captions::DialogItem;
use crate::core::ids::ControlId as C;
use crate::core::multi_rename;
use crate::core::text::{Numbers, Strings};
use crate::win::Hwnd;
use crate::win::controls::{Balloon, CheckBox, Edit};
use crate::win::dialog::{self, DialogEnd, DialogMessage, DialogProc, Opened};
use crate::win::locale::WindowsNumbers;
use std::cell::{Cell, OnceCell, RefCell};

/// Range…: the person selects characters of a name, and OK names them.
pub(super) struct PickRange {
    text: String,
    /// The selection OK read, in UTF-16 units, and Count from the end.
    chosen: Cell<Option<((usize, usize), bool)>>,
}

impl PickRange {
    pub(super) fn new(text: String) -> PickRange {
        PickRange {
            text,
            chosen: Cell::new(None),
        }
    }

    /// The placeholder of the letter `letter` that names the characters OK
    /// read; `None` when none were selected.
    pub(super) fn range(&self, letter: char) -> Option<String> {
        let (selected, from_end) = self.chosen.get()?;
        multi_rename::range(letter, &self.text, selected, from_end)
    }
}

impl DialogProc for PickRange {
    fn init(&self, dialog: Hwnd) -> Opened {
        if let Some(name) = dialog::item(dialog, C::PickRangeName.id()) {
            name.set_text(&self.text);
        }
        Opened {
            focus: C::PickRangeName.id(),
            fonts: Vec::new(),
        }
    }

    fn message(&self, dialog: Hwnd, message: DialogMessage) -> bool {
        match message {
            DialogMessage::Command { id } if id == DialogItem::Ok.id() => {
                if let Some(name) = Edit::of_dialog(dialog, C::PickRangeName.id()) {
                    let from_end = CheckBox::of_dialog(dialog, C::PickRangeFromEnd.id())
                        .is_some_and(CheckBox::is_checked);
                    self.chosen.set(Some((name.selection(), from_end)));
                }
                // OK ends the dialog.
                false
            }
            _ => false,
        }
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
