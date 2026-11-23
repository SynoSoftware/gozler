//! The rows of the SSD wear window: the folders that the service saw copied
//! into themselves, and the files, folders, programs and disks that crossed
//! a limit of the wear alarm. Each warns at most once a day while Gozler
//! runs, so one that keeps going updates its row instead of warning again.

use super::captions::with_mounts;
use super::startup::{Request, Search};
use super::text::{self, Numbers, Strings};
use gozler_core::alarm::DAY_TICKS;
use gozler_core::proto::{Alarm, Alarmed, RepeatingFolder};
use gozler_core::types::{SortDir, SortKey};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a row's subject that warned stays quiet.
const QUIET: Duration = Duration::from_secs(24 * 60 * 60);

/// What the window lists.
pub(crate) enum Row {
    /// A folder copied into itself: the highest folder that the repeat
    /// reaches, and how many times its path repeats.
    Copies { top: String, copies: u32 },
    /// A limit of the wear alarm crossed.
    Alarm(Alarm),
}

impl Row {
    /// What the row warns about, as the service names it; a file or folder
    /// that is gone has no name, so its crossing's number stands in.
    fn key(&self) -> String {
        let alarm = match self {
            Row::Copies { top, .. } => return format!("copies|{}", top.to_lowercase()),
            Row::Alarm(alarm) => alarm,
        };
        let (kind, name) = match &alarm.what {
            Alarmed::FileWrites { path } => ("file writes", path.clone()),
            Alarmed::FileWritten { path } => ("file written", path.clone()),
            Alarmed::FolderWritten { path } => ("folder written", path.clone()),
            Alarmed::ProgramWritten { path } => ("program written", Some(path.clone())),
            Alarmed::DiskWeek { model, mounts, .. } => {
                let model = model.as_deref().unwrap_or("");
                ("disk week", Some(format!("{model}|{}", mounts.join("|"))))
            }
        };
        match name {
            Some(name) => format!("{kind}|{}", name.to_lowercase()),
            None => format!("{kind}#{}", alarm.number),
        }
    }

    /// What Show opens: a file, a folder or a program in Commander, in its
    /// folder, a volume's root folder itself, and for
    /// a disk, a search of its drives sorted by Written, most first, which
    /// shows what writes there now; the main window also shows the Writes
    /// and Written columns for it. `None` for a file or folder that is
    /// gone, and for a program the service named only by its file name.
    pub(crate) fn show(&self) -> Option<Request> {
        let alarm = match self {
            Row::Copies { top, .. } => return Some(Request::Reveal(top.clone())),
            Row::Alarm(alarm) => alarm,
        };
        match &alarm.what {
            Alarmed::FileWrites { path } | Alarmed::FileWritten { path } => {
                path.clone().map(Request::Reveal)
            }
            // A volume's root, which the service names by its mount point,
            // has no folder to show it in, so it opens itself.
            Alarmed::FolderWritten { path } => path.clone().map(|path| {
                if path.ends_with('\\') {
                    Request::Open(path)
                } else {
                    Request::Reveal(path)
                }
            }),
            Alarmed::ProgramWritten { path } => {
                path.contains('\\').then(|| Request::Reveal(path.clone()))
            }
            Alarmed::DiskWeek { mounts, .. } => {
                let drives: Vec<String> = (mounts.iter())
                    .map(|mount| format!("startwith:\"{mount}\""))
                    .collect();
                (!drives.is_empty()).then(|| {
                    Request::Search(Search {
                        text: drives.join(" | "),
                        sort: Some(SortKey::Written),
                        direction: Some(SortDir::Desc),
                        ..Search::default()
                    })
                })
            }
        }
    }

    /// The row's cells: what warns, its kind, how much it wrote, and the
    /// limit it crossed. A value with an unknown part shows "≥".
    pub(crate) fn cells(&self, strings: &Strings, numbers: &dyn Numbers) -> [String; 4] {
        let alarm = match self {
            Row::Copies { top, copies } => {
                let copies = strings.count("wear.copies", u64::from(*copies), numbers, &[]);
                let kind = strings.text("wear.kind_copies").to_owned();
                return [top.clone(), kind, copies, String::new()];
            }
            Row::Alarm(alarm) => alarm,
        };
        let bytes = |n| text::bytes(strings, numbers, n);
        let gone = || strings.text("cell.gone").to_owned();
        let per_hour = |limit: String| strings.format("wear.per_hour", &[&limit]);
        let (name, kind, amount, limit) = match &alarm.what {
            Alarmed::FileWrites { path } => (
                path.clone().unwrap_or_else(gone),
                "wear.kind_file",
                strings.count("wear.writes", alarm.value, numbers, &[]),
                per_hour(numbers.integer(alarm.limit)),
            ),
            Alarmed::FileWritten { path } | Alarmed::FolderWritten { path } => (
                path.clone().unwrap_or_else(gone),
                match alarm.what {
                    Alarmed::FileWritten { .. } => "wear.kind_file",
                    _ => "wear.kind_folder",
                },
                bytes(alarm.value),
                per_hour(bytes(alarm.limit)),
            ),
            Alarmed::ProgramWritten { path } => (
                path.clone(),
                "wear.kind_program",
                bytes(alarm.value),
                per_hour(bytes(alarm.limit)),
            ),
            Alarmed::DiskWeek {
                model,
                mounts,
                since,
            } => {
                let name = model
                    .clone()
                    .unwrap_or_else(|| strings.text("wear.kind_disk").to_owned());
                // Whole days, at least one: a week is at most 7.
                let ticks = u64::try_from(alarm.time.saturating_sub(*since)).unwrap_or(0);
                let ticks = ticks.max(1);
                let days = ticks.div_ceil(DAY_TICKS as u64);
                let written = bytes(alarm.value);
                (
                    with_mounts(strings, name, mounts),
                    "wear.kind_disk",
                    strings.count("wear.days", days, numbers, &[&written]),
                    strings.format("wear.per_week", &[&bytes(alarm.limit)]),
                )
            }
        };
        let amount = if alarm.at_least {
            text::at_least(&amount)
        } else {
            amount
        };
        [name, strings.text(kind).to_owned(), amount, limit]
    }
}

#[derive(Default)]
pub(crate) struct Warnings {
    /// The rows the window shows, in the order they came; empty while it
    /// is closed.
    rows: Vec<Row>,
    /// When each row's subject last got a row, by [`Row::key`].
    warned: HashMap<String, Instant>,
}

impl Warnings {
    /// Takes the folders and the crossings of a reply at `now`: one that
    /// has a row replaces it, and one that got no row in the last day gets
    /// one. Answers whether a row changed or came, so the window flashes
    /// again.
    pub(crate) fn take(
        &mut self,
        events: &[RepeatingFolder],
        alarms: &[Alarm],
        now: Instant,
    ) -> bool {
        let copies = events.iter().map(|event| Row::Copies {
            top: event.top.clone(),
            copies: event.copies,
        });
        let alarms = alarms.iter().cloned().map(Row::Alarm);
        let mut changed = false;
        for row in copies.chain(alarms) {
            let key = row.key();
            if let Some(shown) = self.rows.iter_mut().find(|shown| shown.key() == key) {
                changed |= !same(shown, &row);
                *shown = row;
            } else if self
                .warned
                .get(&key)
                .is_none_or(|warned| now.duration_since(*warned) >= QUIET)
            {
                self.warned.insert(key, now);
                self.rows.push(row);
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The window closed: its rows go, and what they named stays quiet.
    pub(crate) fn clear(&mut self) {
        self.rows.clear();
    }

    /// The window did not open: its rows go, and what they named may warn
    /// again, because nobody saw them.
    pub(crate) fn forget(&mut self) {
        for row in self.rows.drain(..) {
            self.warned.remove(&row.key());
        }
    }
}

/// Whether two rows of one subject show the same values: all but the
/// service's number of the crossing.
fn same(a: &Row, b: &Row) -> bool {
    match (a, b) {
        (Row::Copies { copies: a, .. }, Row::Copies { copies: b, .. }) => a == b,
        (Row::Alarm(a), Row::Alarm(b)) => {
            a.what == b.what
                && a.value == b.value
                && a.at_least == b.at_least
                && a.limit == b.limit
                && a.time == b.time
        }
        _ => false,
    }
}
