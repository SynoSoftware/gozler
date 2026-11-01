//! The system image list's index for each extension, asked of Windows once
//! per extension (DESIGN.md: Results list), and of the panels' `..`, and
//! Windows' link arrow over the icon of a junction or a symbolic link.

use crate::win::gdi::ImageList;
use gozler_core::proto::ResultItem;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

/// The extension, lowercased, or `None` for a folder; a file without an
/// extension is the empty extension.
type Kind = Option<String>;

#[derive(Default)]
pub(super) struct Icons {
    kinds: RefCell<HashMap<Kind, i32>>,
    up: Cell<Option<i32>>,
    link_arrow: Cell<Option<Option<u32>>>,
    /// Whether each reparse point that a result row showed is a link, which
    /// the index does not record; `None` while its check runs.
    links: RefCell<HashMap<String, Option<bool>>>,
    /// The reparse points to check, which the window gives its work when it
    /// wakes ([`Self::take_unchecked`]).
    unchecked: RefCell<Vec<String>>,
}

impl Icons {
    /// The icon of an item named `name`. A new extension asks Windows with
    /// no borrow held, because the shell call can dispatch messages (DESIGN.md:
    /// window rule 1).
    pub(super) fn index(&self, name: &str, is_folder: bool) -> i32 {
        let kind: Kind = (!is_folder).then(|| {
            gozler_core::query::extension(name.as_bytes(), false)
                .map(|ext| String::from_utf8_lossy(ext).to_lowercase())
                .unwrap_or_default()
        });
        if let Some(&index) = self.kinds.borrow().get(&kind) {
            return index;
        }
        let sample = match &kind {
            None => "folder".to_owned(),
            Some(ext) if ext.is_empty() => "file".to_owned(),
            Some(ext) => format!("file.{ext}"),
        };
        let index = ImageList::system_index(&sample, is_folder);
        self.kinds.borrow_mut().insert(kind, index);
        index
    }

    /// The icon of a panel's `..` row: Windows' open folder (DESIGN.md: A
    /// panel); a folder's when the shell does not say.
    pub(super) fn up(&self) -> i32 {
        if let Some(index) = self.up.get() {
            return index;
        }
        let index = ImageList::open_folder_index().unwrap_or_else(|| self.index("", true));
        self.up.set(Some(index));
        index
    }

    /// The overlay over a link's icon: Windows' link arrow, or none when
    /// Windows has none. Asks Windows the first time, with no borrow held,
    /// as [`Self::index`] does.
    pub(super) fn link_arrow(&self) -> Option<u32> {
        if let Some(overlay) = self.link_arrow.get() {
            return overlay;
        }
        let overlay = ImageList::system_small().and_then(ImageList::link_overlay);
        self.link_arrow.set(Some(overlay));
        overlay
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
