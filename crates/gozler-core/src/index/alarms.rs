//! The wear alarm's limits on files and folders ([`crate::alarm`]). A
//! journal batch checks each row of the table of writes that it added to:
//! a name's writes when the batch's writes land (`FileAlarms::check_writes`),
//! and a name's or a folder's known bytes when the size step adds them
//! (`FileAlarms::check_written`). Each check reads the row's rolling hour,
//! so a row crosses its limit at the write that takes its last hour over it.

use std::sync::atomic::AtomicU64;

use super::writes::{Marks, PendingWrite, Values, Writes};
use super::{SeenBy, VolumeIndex};
use crate::alarm::{Levels, Warning, Warnings, Watched};
use crate::proto::{Alarm, Alarmed};
use crate::types::Frn;

/// A row of the table of writes, and which of its limits it crossed. A
/// name's row is one subject whichever file it holds, so a program that
/// saves a file by replacing it does not warn at each save.
#[derive(Clone, Copy, Debug)]
pub enum Crossed {
    /// [`WearLimits::file_writes`]: the key of the name's row
    /// ([`PendingWrite::name`]), and the file the row held when it crossed,
    /// which names the crossing once the row has left the hour.
    ///
    /// [`WearLimits::file_writes`]: crate::proto::WearLimits::file_writes
    FileWrites { name: u64, file: Frn },
    /// [`WearLimits::file_written`], as `FileWrites`.
    ///
    /// [`WearLimits::file_written`]: crate::proto::WearLimits::file_written
    FileWritten { name: u64, file: Frn },
    /// [`WearLimits::folder_written`]; the folder.
    ///
    /// [`WearLimits::folder_written`]: crate::proto::WearLimits::folder_written
    FolderWritten(Frn),
}

impl PartialEq for Crossed {
    /// The same row and limit; a name's file does not count.
    fn eq(&self, other: &Crossed) -> bool {
        match (*self, *other) {
            (Crossed::FileWrites { name: a, .. }, Crossed::FileWrites { name: b, .. })
            | (Crossed::FileWritten { name: a, .. }, Crossed::FileWritten { name: b, .. }) => {
                a == b
            }
            (Crossed::FolderWritten(a), Crossed::FolderWritten(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Crossed {}

/// A volume's files and folders that crossed a limit of the wear alarm.
pub struct FileAlarms(Warnings<Crossed>);

impl FileAlarms {
    /// Allocates the list at its full size. Allocate outside the index's
    /// write lock.
    pub fn allocate() -> Self {
        FileAlarms(Warnings::allocate())
    }

    /// Gives the list the service's sequence counter; until then it records
    /// nothing.
    pub fn number_from(&mut self, numbers: &'static AtomicU64) {
        self.0.number_from(numbers);
    }

    /// How many crossings the list dropped.
    pub fn dropped(&self) -> u64 {
        self.0.dropped()
    }

    /// Checks the names whose writes `pending` added against `limits`, of
    /// writes in an hour. `time` is a FILETIME.
    pub(crate) fn check_writes(
        &mut self,
        writes: &Writes,
        pending: &[PendingWrite],
        limits: &Levels,
        time: i64,
    ) {
        for p in pending
            .iter()
            .filter(|p| p.renamed || (p.closed && !p.created))
        {
            if let Some(values) = writes.of_name_key(p.name)
                && let Some(reading) = limits.reading(u64::from(values.writes), values.capped, time)
            {
                let crossed = Crossed::FileWrites {
                    name: p.name,
                    file: values.frn,
                };
                self.0.check(crossed, reading);
            }
        }
    }

    /// Checks the names and folders whose bytes the size step of `pending`
    /// added against `limits`. Only known bytes count: a write whose bytes
    /// are unknown adds none. `time` is a FILETIME.
    pub(crate) fn check_written(
        &mut self,
        writes: &Writes,
        pending: &[PendingWrite],
        limits: &Watched,
        time: i64,
    ) {
        let (files, folders) = (&limits.file_written, &limits.folder_written);
        if files.is_off() && folders.is_off() {
            return;
        }
        let reading = |limits: &Levels, values: Values| {
            let at_least = values.marks.contains(Marks::AT_LEAST);
            limits.reading(values.written, at_least, time)
        };
        for p in pending {
            // Rows as `Writes::add_sizes` adds to them: a name the write did
            // not create, and the folder of a write that is not a rename.
            if !files.is_off()
                && (p.renamed || !p.created)
                && let Some(values) = writes.of_name_key(p.name)
                && let Some(reading) = reading(files, values)
            {
                let crossed = Crossed::FileWritten {
                    name: p.name,
                    file: values.frn,
                };
                self.0.check(crossed, reading);
            }
            if !folders.is_off()
                && !p.renamed
                && let Some(values) = writes.of_folder(p.folder)
                && let Some(reading) = reading(folders, values)
            {
                self.0.check(Crossed::FolderWritten(p.folder), reading);
            }
        }
    }

    /// Lets each row that `writes` now holds under the limits of `limits`
    /// that it crossed warn again, such as after the hour moved past its
    /// writes. A row that left the table is under every limit.
    pub(crate) fn rearm(&mut self, writes: &Writes, limits: &Watched) {
        if !self.0.any_over() {
            return;
        }
        self.0.rearm(|crossed| {
            let (value, levels) = match *crossed {
                Crossed::FileWrites { name, .. } => (
                    writes.of_name_key(name).map(|v| u64::from(v.writes)),
                    &limits.file_writes,
                ),
                Crossed::FileWritten { name, .. } => (
                    writes.of_name_key(name).map(|v| v.written),
                    &limits.file_written,
                ),
                Crossed::FolderWritten(folder) => (
                    writes.of_folder(folder).map(|v| v.written),
                    &limits.folder_written,
                ),
            };
            Some((value.unwrap_or(0), levels))
        });
    }

    /// The crossings that `wanted` keeps, as a reply sends them, with the
    /// full paths under `mount_prefix` that their files and folders have
    /// now. A name's path is its row's file while the row holds the hour,
    /// and then the file the row held when it crossed.
    /// With `may_list`, a filtered user's answer for a folder's descriptor,
    /// only the files in folders the user may list and the folders the user
    /// may list are given; `None` gives every crossing, with no path for a
    /// file or folder that is gone.
    pub fn reported(
        &self,
        index: &VolumeIndex,
        mount_prefix: &str,
        wanted: impl Fn(&Warning<Crossed>) -> bool,
        may_list: Option<&(impl Fn(&[u8]) -> bool + ?Sized)>,
    ) -> Vec<Alarm> {
        let mut kept = self.0.iter().filter(|&w| wanted(w)).peekable();
        if kept.peek().is_none() {
            return Vec::new();
        }
        let seen_by = index.seen_by(may_list);
        let mut buf = Vec::new();
        kept.filter_map(|warning| {
            let frn = match warning.subject {
                Crossed::FileWrites { name, file } | Crossed::FileWritten { name, file } => {
                    let row = index.writes().of_name_key(name);
                    Some(row.map_or(file, |values| values.frn))
                }
                // Its bytes are the writes of the files in it, which a user
                // who may not list it does not see.
                Crossed::FolderWritten(folder) => {
                    let head = index.head_of(folder);
                    head.is_some_and(|head| index.sees_into(head, &seen_by))
                        .then_some(folder)
                }
            };
            let path = frn.and_then(|frn| index.seen_path(frn, mount_prefix, &seen_by, &mut buf));
            if path.is_none() && matches!(seen_by, SeenBy::Filtered(_)) {
                return None;
            }
            Some(warning.alarm(match warning.subject {
                Crossed::FileWrites { .. } => Alarmed::FileWrites { path },
                Crossed::FileWritten { .. } => Alarmed::FileWritten { path },
                Crossed::FolderWritten(_) => Alarmed::FolderWritten { path },
            }))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Crossed;
    use crate::alarm::Watched;
    use crate::index::settle::Batch;
    use crate::index::writes::{STEP_TICKS, Writes};
    use crate::index::{NTFS_ROOT_FRN, VolumeIndex};
    use crate::proto::WearLimits;
    use crate::types::Frn;
    use crate::usn::record::{ParsedUsnRecord, USN_REASON_CLOSE, USN_REASON_DATA_OVERWRITE};
    use std::sync::atomic::AtomicU64;

    static NUMBERS: AtomicU64 = AtomicU64::new(0);

    /// A file's writes cross the limit at the write that takes its rolling
    /// hour to the limit, warn once while the hour stays over it, and warn
    /// again only after the hour moved past enough of them. A check that
    /// warned at each write would flood the window; one that never let the
    /// file warn again would hide a fault that comes back.
    #[test]
    fn a_file_warns_once_when_its_hour_crosses_the_limit_and_again_after_it_fell_under() {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        *index.writes_mut() = Writes::for_entries(0);
        index.alarms_mut().number_from(&NUMBERS);
        let start = 1_000 * STEP_TICKS;
        let mut limits = Watched::default();
        limits.add(&WearLimits {
            file_writes: Some(3),
            ..WearLimits::default()
        });
        let write = |index: &mut VolumeIndex, time: i64| {
            let record = ParsedUsnRecord {
                frn: Frn(100),
                parent_frn: NTFS_ROOT_FRN,
                reason: USN_REASON_DATA_OVERWRITE | USN_REASON_CLOSE,
                attributes: 0,
                name: b"nvlog.bin".to_vec(),
                time,
            };
            let records = [record];
            let batch = Batch {
                limits: limits.clone(),
                ..Batch::of(&records)
            };
            index.apply_batch(&batch);
        };
        let crossings = |index: &VolumeIndex| {
            let files = |w: &crate::alarm::Warning<Crossed>| {
                matches!(w.subject, Crossed::FileWrites { .. })
            };
            index.alarms().0.iter().filter(|&w| files(w)).count()
        };
        // Two writes in the first step, under the limit.
        write(&mut index, start);
        write(&mut index, start + 1);
        assert_eq!(crossings(&index), 0);
        // The third, an hour's step later, crosses it once.
        write(&mut index, start + 11 * STEP_TICKS);
        let first = index.alarms().0.iter().map(|w| w.number).max();
        assert!(first.is_some());
        write(&mut index, start + 11 * STEP_TICKS + 1);
        assert_eq!(index.alarms().0.iter().map(|w| w.number).max(), first);
        // The hour moves past the first step's two writes: 2 left, under
        // the limit. The next write takes it to the limit, and it warns
        // again.
        index.move_hour(start + 12 * STEP_TICKS, &limits);
        write(&mut index, start + 12 * STEP_TICKS + 1);
        let second = index.alarms().0.iter().map(|w| w.number).max();
        assert!(second > first);
        assert_eq!(crossings(&index), 1);
    }
}
