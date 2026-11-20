//! The system image list's index of each extension's icon, which the shell
//! worker asks Windows for once per extension (DESIGN.md: Results list);
//! Windows' own icons for a folder, the panels' `..` and a file whose icon is
//! not known yet; and Windows' link arrow over the icon of a junction or a
//! symbolic link.

use crate::win::gdi::{ImageList, StockIcon};
use gozler_core::proto::ResultItem;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

pub(super) struct Icons {
    file: Option<i32>,
    folder: Option<i32>,
    up: Option<i32>,
    link_arrow: Option<u32>,
    /// The icon of each extension that a row showed; `None` while the shell
    /// worker looks it up.
    extensions: RefCell<HashMap<String, Option<i32>>>,
    /// The extensions to look up, which the window gives its work when it
    /// wakes ([`Self::take_unknown`]).
    unknown: RefCell<Vec<String>>,
    /// Whether each reparse point that a result row showed is a link, which
    /// the index does not record; `None` while its check runs.
    links: RefCell<HashMap<String, Option<bool>>>,
    /// The reparse points to check, which the window gives its work when it
    /// wakes ([`Self::take_unchecked`]).
    unchecked: RefCell<Vec<String>>,
}

impl Icons {
    /// Asks Windows for its own icons, before any list paints.
    pub(super) fn new() -> Self {
        let folder = ImageList::stock_index(StockIcon::Folder);
        Self {
            file: ImageList::stock_index(StockIcon::File),
            folder,
            up: ImageList::stock_index(StockIcon::OpenFolder).or(folder),
            link_arrow: ImageList::system_small().and_then(ImageList::link_overlay),
            extensions: RefCell::default(),
            unknown: RefCell::default(),
            links: RefCell::default(),
            unchecked: RefCell::default(),
        }
    }

    /// The icon of an item named `name`. A file whose extension's icon is
    /// not known yet shows Windows' icon of a file of no known type until
    /// the shell worker answers, which waits for [`Self::take_unknown`].
    pub(super) fn index(&self, name: &str, is_folder: bool) -> Option<i32> {
        if is_folder {
            return self.folder;
        }
        let extension = extension(name);
        if extension.is_empty() {
            return self.file;
        }
        let known = self.extensions.borrow().get(&extension).copied();
        match known {
            Some(index) => index.or(self.file),
            None => {
                self.extensions.borrow_mut().insert(extension.clone(), None);
                self.unknown.borrow_mut().push(extension);
                self.file
            }
        }
    }

    /// Whether a row waits for its extension's icon.
    pub(super) fn has_unknown(&self) -> bool {
        !self.unknown.borrow().is_empty()
    }

    pub(super) fn take_unknown(&self) -> Vec<String> {
        self.unknown.take()
    }

    /// Records the icons that the shell worker found for extensions, and
    /// gives those whose rows now show another icon. An extension that
    /// Windows gave no icon is asked again by the next row that shows it.
    pub(super) fn learn_icons(&self, found: Vec<(String, Option<i32>)>) -> Changed {
        let mut extensions = self.extensions.borrow_mut();
        let mut changed = HashSet::new();
        for (extension, index) in found {
            match index {
                Some(index) => {
                    if Some(index) != self.file {
                        changed.insert(extension.clone());
                    }
                    extensions.insert(extension, Some(index));
                }
                None => {
                    extensions.remove(&extension);
                }
            }
        }
        Changed(changed)
    }

    /// Forgets the extensions still waiting for their icon, after the work
    /// gave up on their lookups, so the next row that shows one asks again.
    pub(super) fn forget_unanswered(&self) {
        self.extensions
            .borrow_mut()
            .retain(|_, index| index.is_some());
        self.unknown.borrow_mut().clear();
    }

    /// The icon of a panel's `..` row: Windows' open folder (DESIGN.md: A
    /// panel); a folder's when the shell does not say.
    pub(super) fn up(&self) -> Option<i32> {
        self.up
    }

    /// The overlay over a link's icon: Windows' link arrow, or none when
    /// Windows has none.
    pub(super) fn link_arrow(&self) -> Option<u32> {
        self.link_arrow
    }

    /// Whether the result row `item` is a junction or a symbolic link. A
    /// reparse point not checked yet counts as neither until its check,
    /// which waits for [`Self::take_unchecked`], answers.
    pub(super) fn is_link(&self, item: &ResultItem) -> bool {
        let reparse = (item.attributes)
            .is_some_and(|attributes| attributes.bits() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0);
        if !reparse || item.is_gone() {
            return false;
        }
        let known = self.links.borrow().get(&item.path).copied();
        match known {
            Some(link) => link == Some(true),
            None => {
                self.links.borrow_mut().insert(item.path.clone(), None);
                self.unchecked.borrow_mut().push(item.path.clone());
                false
            }
        }
    }

    /// Whether a result row waits for its reparse point's check.
    pub(super) fn has_unchecked(&self) -> bool {
        !self.unchecked.borrow().is_empty()
    }

    pub(super) fn take_unchecked(&self) -> Vec<String> {
        self.unchecked.take()
    }

    /// Records which checked reparse points are links.
    pub(super) fn learn_links(&self, checked: Vec<(String, bool)>) {
        let mut links = self.links.borrow_mut();
        for (path, link) in checked {
            links.insert(path, Some(link));
        }
    }
}

/// The extensions whose rows show another icon after
/// [`Icons::learn_icons`].
pub(super) struct Changed(HashSet<String>);

impl Changed {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the icon of an item named `name` is one of them.
    pub(super) fn shows(&self, name: &str, is_folder: bool) -> bool {
        !is_folder && self.0.contains(&extension(name))
    }
}

/// The extension of `name`, lowercased, by which its icon is kept; empty
/// for a name without one.
fn extension(name: &str) -> String {
    gozler_core::query::extension(name.as_bytes(), false)
        .map(|extension| String::from_utf8_lossy(extension).to_lowercase())
        .unwrap_or_default()
}
