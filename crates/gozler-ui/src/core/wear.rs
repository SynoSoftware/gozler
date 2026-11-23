//! The rows of the SSD wear window: the folders that the service saw copied
//! into themselves. Each folder
//! warns at most once a day while Gozler runs, so a copy that keeps going
//! updates its row instead of warning again.

use gozler_core::proto::RepeatingFolder;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a folder that warned stays quiet.
const QUIET: Duration = Duration::from_secs(24 * 60 * 60);

/// A folder copied into itself, as the window lists it.
pub(crate) struct Row {
    /// The highest folder that the repeat reaches.
    pub(crate) top: String,
    pub(crate) copies: u32,
}

#[derive(Default)]
pub(crate) struct Warnings {
    /// The rows the window shows, in the order they came; empty while it
    /// is closed.
    rows: Vec<Row>,
    /// When each folder last got a row, by its path in lower case.
    warned: HashMap<String, Instant>,
}

impl Warnings {
    /// Takes the folders of a reply at `now`: a folder that has a row gets
    /// its new copies, and one that got no row in the last day gets one.
    /// Answers whether a row changed or came, so the window flashes again.
    pub(crate) fn take(&mut self, events: &[RepeatingFolder], now: Instant) -> bool {
        let mut changed = false;
        for event in events {
            let key = event.top.to_lowercase();
            if let Some(row) = self
                .rows
                .iter_mut()
                .find(|row| row.top.to_lowercase() == key)
            {
                changed |= row.copies != event.copies;
                row.copies = event.copies;
            } else if self
                .warned
                .get(&key)
                .is_none_or(|warned| now.duration_since(*warned) >= QUIET)
            {
                self.warned.insert(key, now);
                self.rows.push(Row {
                    top: event.top.clone(),
                    copies: event.copies,
                });
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The window closed: its rows go, and the folders stay quiet.
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
    }

    /// The window did not open: its rows go, and their folders may warn
    /// again, because nobody saw them.
    pub(crate) fn forget(&mut self) {
        for row in self.rows.drain(..) {
            self.warned.remove(&row.top.to_lowercase());
        }
    }
}
