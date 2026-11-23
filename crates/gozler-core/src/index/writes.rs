//! How often each file and folder was written in the last hour, and how many
//! bytes those writes put on the disk: its **Writes** and its **Written**.
//!
//! [`Writes`] is a table of a fixed size for each volume, which the journal
//! tail fills from the records it reads (`docs/DESIGN.md`, "Writes"). A row
//! belongs to a name in a folder, keyed by the hash of the folder's file ID
//! and the folded name, or to a folder, keyed by the hash of its own file ID,
//! so a save through a temporary file keeps its count with the name. Each row
//! keeps 12 steps of 5 minutes, so "the last hour" is between 55 and 60
//! minutes. A count belongs to a write only when the record has `CLOSE`, so
//! one open counts once; the bytes come from the size the tail reads after
//! the batch (`Writes::add_sizes`).
//!
//! Every time comes in as an argument; the table has no clock of its own.

use std::hash::Hasher;

use crate::fold::fold_accents_into;
use crate::index::{ComponentBytes, SeenBy, VolumeIndex};
use crate::proto::WritesStatus;
use crate::types::{EntryIdx, Frn};
use crate::usn::ParsedUsnRecord;
use crate::usn::record::{
    USN_REASON_CLOSE, USN_REASON_DATA_EXTEND, USN_REASON_DATA_OVERWRITE,
    USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME,
};

/// The steps that make the last hour.
pub const STEPS: usize = 12;

/// One step, in FILETIME ticks of 100 ns: 5 minutes.
pub const STEP_TICKS: i64 = 5 * 60 * 10_000_000;

/// A table has one row for each this many entries of its volume, within
/// [`LEAST_ROWS`] and [`MOST_ROWS`].
const ENTRIES_PER_ROW: usize = 16;
const LEAST_ROWS: usize = 4096;
const MOST_ROWS: usize = 65536;

/// Each list of a rename that replaced a name holds this many keys.
const PAIRS: usize = 16;

const DATA: u32 = USN_REASON_DATA_EXTEND | USN_REASON_DATA_OVERWRITE | USN_REASON_DATA_TRUNCATION;

/// What a step of a row says of how its file was written, and whether its
/// Written is exact: the kinds of write and the two marks, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Marks(pub u8);

impl Marks {
    /// Data added to the end (`USN_REASON_DATA_EXTEND`).
    pub const ADDED: Marks = Marks(1);
    /// Data cut off and written again (`USN_REASON_DATA_TRUNCATION`).
    pub const REPLACED: Marks = Marks(2);
    /// Data written over in place (`USN_REASON_DATA_OVERWRITE`).
    pub const WRITTEN_OVER: Marks = Marks(4);
    /// Written holds the size after a write that replaced data or wrote over
    /// part of it, the most that write can have put on the disk.
    pub const UP_TO: Marks = Marks(8);
    /// Written misses the bytes of a file whose size could not be read.
    pub const AT_LEAST: Marks = Marks(16);

    /// The kinds of write that a record's reason bits say.
    fn of(reason: u32) -> Marks {
        let mut marks = Marks::default();
        for (bit, kind) in [
            (USN_REASON_DATA_EXTEND, Marks::ADDED),
            (USN_REASON_DATA_TRUNCATION, Marks::REPLACED),
            (USN_REASON_DATA_OVERWRITE, Marks::WRITTEN_OVER),
        ] {
            if reason & bit != 0 {
                marks.0 |= kind.0;
            }
        }
        marks
    }

    pub fn contains(self, other: Marks) -> bool {
        self.0 & other.0 == other.0
    }

    /// Which bound a Written with these marks shows; "at least" wins when
    /// it has both (rule 4).
    pub fn bound(self) -> Bound {
        if self.contains(Marks::AT_LEAST) {
            Bound::AtLeast
        } else if self.contains(Marks::UP_TO) {
            Bound::UpTo
        } else {
            Bound::Exact
        }
    }

    fn holds_any(self, other: Marks) -> bool {
        self.0 & other.0 != 0
    }

    fn with(self, other: Marks) -> Marks {
        Marks(self.0 | other.0)
    }
}

/// How a Written relates to the bytes that the writes put on the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    Exact,
    /// The most the writes can have put on the disk.
    UpTo,
    /// Some of the writes' bytes are missing.
    AtLeast,
}

/// The values of one row over the last hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Values {
    /// The file last counted under the name, or the folder's own file ID.
    pub frn: Frn,
    pub writes: u32,
    /// In bytes, each write's rounded up to a whole KiB.
    pub written: u64,
    /// In how many of the hour's steps the row had a write: 0 to 12.
    pub steps: u8,
    /// The kinds and marks of every step together. A step whose Written
    /// stopped at its highest value marks it "at least".
    pub marks: Marks,
    /// A step's count stopped at its highest value, so `writes` is a lower
    /// bound.
    pub capped: bool,
}

impl Values {
    /// The values of an entry without a row: no write.
    fn none(frn: Frn) -> Values {
        Values {
            frn,
            writes: 0,
            written: 0,
            steps: 0,
            marks: Marks::default(),
            capped: false,
        }
    }
}

/// How far a table's values may be below the true ones, which a search
/// reads once: an entry's values are the least each may be, and these give
/// the most (rule 13).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Low {
    /// The table counted for less than the hour, so no value has an upper
    /// bound.
    pub open: bool,
    /// "May be low by", in writes and in bytes.
    pub writes: u64,
    pub bytes: u64,
}

impl Low {
    /// The most the writes of `values` may be; `u64::MAX` for no bound.
    pub fn most_writes(&self, values: &Values) -> u64 {
        if self.open || values.capped {
            u64::MAX
        } else {
            u64::from(values.writes).saturating_add(self.writes)
        }
    }

    /// The most the Written of `values` may be; `u64::MAX` for no bound.
    pub fn most_written(&self, values: &Values) -> u64 {
        if self.open || values.marks.contains(Marks::AT_LEAST) {
            u64::MAX
        } else {
            values.written.saturating_add(self.bytes)
        }
    }
}

/// A write that the size step settles: one for each record of a file with
/// `USN_REASON_FILE_CREATE` or a data bit, with or without `CLOSE`, and one
/// for each rename that rule 1 counts. It names its rows by key and file ID,
/// never by an entry slot, because the settle takes several write holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingWrite {
    pub frn: Frn,
    /// The folder that holds the name.
    pub folder: Frn,
    /// The key of the name's row.
    pub name: u64,
    /// The step the write was counted in.
    pub step: i64,
    /// The kinds of write of the record.
    pub marks: Marks,
    /// The open created the file, so the name has no row of its own.
    pub created: bool,
    /// The record has `CLOSE`, so the write was counted.
    pub closed: bool,
    /// A rename that replaced another file under the name.
    pub renamed: bool,
}

/// Which row of a pending write a value goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Of {
    /// The name's row, which counts the file.
    Name,
    /// The row of the folder that holds the name.
    Folder,
}

impl PendingWrite {
    /// The key of its row `of`, and the file that row is made for.
    fn row(&self, of: Of) -> (u64, Frn) {
        match of {
            Of::Name => (self.name, self.frn),
            Of::Folder => (folder_key(self.folder), self.folder),
        }
    }
}

/// One row: a name in a folder, or a folder. A write reads and changes
/// only the fields before `written`, which fill one cache line, so it
/// waits for one read of memory.
#[derive(Clone)]
#[repr(C, align(64))]
struct Row {
    key: u64,
    /// The batch that last counted in the row, which a drop pass leaves.
    batch: u32,
    marks: [u8; STEPS],
    /// For each step, its writes, stopping at the highest value.
    counts: [u16; STEPS],
    frn: Frn,
    /// For each step, its Written in KiB, stopping at the highest value.
    written: [u32; STEPS],
}

impl Row {
    fn new(key: u64, frn: Frn, batch: u32) -> Row {
        Row {
            frn,
            key,
            counts: [0; STEPS],
            written: [0; STEPS],
            marks: [0; STEPS],
            batch,
        }
    }

    fn is_empty(&self) -> bool {
        self.counts.iter().all(|&n| n == 0) && self.written.iter().all(|&n| n == 0)
    }

    /// The order a drop pass drops rows in: the least Written first, then
    /// the fewest writes.
    fn score(&self) -> u64 {
        let written: u64 = self.written.iter().map(|&kib| u64::from(kib)).sum();
        let writes: u64 = self.counts.iter().map(|&n| u64::from(n)).sum();
        // Written is below 2^36 KiB and writes below 2^20.
        written << 20 | writes
    }

    fn values(&self) -> Values {
        let mut marks = Marks(self.marks.iter().fold(0, |all, &m| all | m));
        if self.written.contains(&u32::MAX) {
            marks = marks.with(Marks::AT_LEAST);
        }
        Values {
            frn: self.frn,
            writes: self.counts.iter().map(|&n| u32::from(n)).sum(),
            written: self.written.iter().map(|&kib| u64::from(kib) * 1024).sum(),
            steps: self.counts.iter().filter(|&&n| n > 0).count() as u8,
            marks,
            capped: self.counts.contains(&u16::MAX),
        }
    }
}

/// A rename that waits in its batch for the delete of the name it took.
#[derive(Clone, Copy)]
struct Renamed {
    key: u64,
    frn: Frn,
    folder: Frn,
    step: i64,
    /// Its record also wrote data, which counted for the name already.
    counted: bool,
}

/// A delete of a file's name that waits in its batch for another file's
/// rename to that name.
#[derive(Clone, Copy)]
struct Deleted {
    key: u64,
    frn: Frn,
}

/// The values of a volume's files and folders over the last hour.
pub struct Writes {
    /// The rows in use, at most the table's size.
    rows: Vec<Row>,
    /// Open addressing over `rows`: a row's place plus 1, or 0 for no row.
    /// Empty for a table that keeps nothing.
    slots: Vec<u32>,
    /// The step the last hour ends in: a FILETIME divided by
    /// [`STEP_TICKS`]. It only moves forward, but for a clock set back.
    step: i64,
    /// From when the values are complete; `None` until a build installs the
    /// table.
    counted_since: Option<i64>,
    /// What the dropped rows took with them, for each step: "may be low by".
    low_writes: [u32; STEPS],
    low_kib: [u64; STEPS],
    /// The batch being counted, which [`Row::batch`] compares with.
    batch: u32,
    /// A drop pass of this batch found no row to drop, because the batch
    /// counted in every row, so the batch runs no other.
    full: bool,
    /// This batch's deletes of a file's name.
    deleted: Vec<Deleted>,
    /// This batch's renames, waiting for the delete of their name.
    renames: Vec<Renamed>,
    /// A drop pass's order, one value for each row.
    scores: Vec<u64>,
}

impl Writes {
    /// A table of no rows, which counts nothing: the table of an index that
    /// no build installed, such as a test's or a bench's.
    pub const fn none() -> Writes {
        Writes {
            rows: Vec::new(),
            slots: Vec::new(),
            step: 0,
            counted_since: None,
            low_writes: [0; STEPS],
            low_kib: [0; STEPS],
            batch: 0,
            full: false,
            deleted: Vec::new(),
            renames: Vec::new(),
            scores: Vec::new(),
        }
    }

    /// Allocates the table of a volume of `entries` entries at its full
    /// size, one row for each sixteenth, so no batch grows it. Allocate
    /// outside the index's write lock.
    pub fn for_entries(entries: usize) -> Writes {
        let rows = (entries / ENTRIES_PER_ROW).clamp(LEAST_ROWS, MOST_ROWS);
        Writes {
            rows: Vec::with_capacity(rows),
            slots: vec![0; (2 * rows).next_power_of_two()],
            deleted: Vec::with_capacity(PAIRS),
            renames: Vec::with_capacity(PAIRS),
            scores: Vec::with_capacity(rows),
            ..Writes::none()
        }
    }

    /// What the table holds in memory, for `--status`.
    pub(crate) fn memory(&self) -> ComponentBytes {
        ComponentBytes::of_vec(&self.rows)
            + ComponentBytes::of_vec(&self.slots)
            + ComponentBytes::of_vec(&self.scores)
            + ComponentBytes::of_vec(&self.deleted)
            + ComponentBytes::of_vec(&self.renames)
    }

    /// How many rows the table holds at most.
    fn size(&self) -> usize {
        self.rows.capacity()
    }

    pub(crate) fn is_kept(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Takes the values of `old`, the table of the index that a rebuild
    /// replaces, which keeps none. When this table is smaller, it keeps the
    /// rows with the most Written and adds the rest to "may be low by". The
    /// records between `old`'s last read and `since`, when the rebuild took
    /// its journal cursor, are never read, so the values count from then.
    pub fn keep(&mut self, old: &mut Writes, since: i64) {
        if old.rows.len() > self.size() {
            // No batch of the new index counted in any row yet.
            old.begin_batch();
            old.shed(old.rows.len() - self.size());
        }
        self.step = old.step;
        // The rows keep their batch tags, so the counter goes on from them.
        self.batch = old.batch;
        self.low_writes = old.low_writes;
        self.low_kib = old.low_kib;
        for row in old.rows.drain(..) {
            self.rows.push(row);
        }
        self.index_rows();
        old.slots.fill(0);
        self.counted_since = Some(since);
    }

    /// Starts counting a new journal batch: no row is new to it, and the
    /// lists of rule 1 are empty.
    pub(crate) fn begin_batch(&mut self) {
        self.batch = self.batch.wrapping_add(1);
        self.full = false;
        self.deleted.clear();
        self.renames.clear();
    }

    /// Counts `rec`, a record that the Include rules keep, whose name has
    /// the key `name` ([`name_keys`]), by rule 1: adds to `pending` what
    /// [`Self::add_writes`] and the size step settle for it.
    pub(crate) fn count(
        &mut self,
        rec: &ParsedUsnRecord,
        name: u64,
        pending: &mut Vec<PendingWrite>,
    ) {
        let reason = rec.reason;
        let closed = reason & USN_REASON_CLOSE != 0;
        let created = reason & USN_REASON_FILE_CREATE != 0;
        let data = reason & DATA != 0;
        let deleted = closed && reason & USN_REASON_FILE_DELETE != 0;
        let renamed = closed && reason & USN_REASON_RENAME_NEW_NAME != 0;
        if !self.is_kept() || rec.is_dir() || !(created || data || deleted || renamed) {
            return;
        }
        self.move_to(rec.time);
        let marks = Marks::of(reason);
        // `add_writes` counts a closed write for the name unless it created
        // the file.
        let name_counted = data && closed && !created;
        if created || data {
            pending.push(PendingWrite {
                frn: rec.frn,
                folder: rec.parent_frn,
                name,
                step: self.step,
                marks,
                created,
                closed,
                renamed: false,
            });
        }
        // A pair is two files: a file renamed and then deleted in one open,
        // whose record holds both bits, replaced nothing.
        if renamed {
            let rename = Renamed {
                key: name,
                frn: rec.frn,
                folder: rec.parent_frn,
                step: self.step,
                counted: name_counted,
            };
            let other = |d: &Deleted| d.key == name && d.frn != rec.frn;
            if let Some(at) = self.deleted.iter().position(other) {
                self.deleted.remove(at);
                self.count_rename(rename, pending);
            } else {
                push_within(&mut self.renames, rename);
            }
        }
        if deleted {
            // The file has left the name, so its own rename to it waits for
            // no delete any more; left waiting, it would pair with a later
            // file's delete and count a file the name no longer holds.
            self.renames.retain(|r| r.key != name || r.frn != rec.frn);
            let other = |r: &Renamed| r.key == name && r.frn != rec.frn;
            if let Some(at) = self.renames.iter().position(other) {
                let rename = self.renames.remove(at);
                self.count_rename(rename, pending);
            } else {
                let deleted = Deleted {
                    key: name,
                    frn: rec.frn,
                };
                push_within(&mut self.deleted, deleted);
            }
        }
    }

    /// Counts a rename that replaced another file under its name: a write of
    /// the name in the rename's step, and nothing for the folder, because a
    /// rename moves no data. A rename whose record also wrote data counted
    /// for the name already, and rule 1 counts a record once.
    fn count_rename(&mut self, rename: Renamed, pending: &mut Vec<PendingWrite>) {
        if rename.counted {
            return;
        }
        pending.push(PendingWrite {
            frn: rename.frn,
            folder: rename.folder,
            name: rename.key,
            step: rename.step,
            marks: Marks::default(),
            created: false,
            closed: true,
            renamed: true,
        });
    }

    /// Adds the writes that [`Self::count`] left in `pending` for this batch
    /// to their rows, in one pass after the batch's records, so the reads of
    /// the rows' memory overlap instead of each waiting for the last: a
    /// closed write for its folder, and for its name unless it created the
    /// file (a build creates thousands of new names an hour), and a rename
    /// for its name.
    pub(crate) fn add_writes(&mut self, pending: &[PendingWrite]) {
        for p in pending {
            if p.renamed {
                self.add_write(p, Of::Name, Marks::default());
            } else if p.closed {
                self.add_write(p, Of::Folder, p.marks);
                if !p.created {
                    self.add_write(p, Of::Name, p.marks);
                }
            }
        }
    }

    /// Adds one write in the step of `p` to its row `of`, made when it is
    /// new; a name's row takes the file of `p` as the file it counted.
    fn add_write(&mut self, p: &PendingWrite, of: Of, marks: Marks) {
        if !self.in_hour(p.step) {
            return;
        }
        let step = self.slot(p.step);
        let (key, frn) = p.row(of);
        let Some(at) = self.row_for(key, frn) else {
            self.low_writes[step] = self.low_writes[step].saturating_add(1);
            return;
        };
        let row = &mut self.rows[at];
        if of == Of::Name {
            row.frn = frn;
        }
        row.counts[step] = row.counts[step].saturating_add(1);
        row.marks[step] |= marks.0;
    }

    /// Adds `bytes` to the Written of the row `of` of `p` in its step, made
    /// when it is new, with `marks`.
    fn add_bytes(&mut self, p: &PendingWrite, of: Of, bytes: u64, marks: Marks) {
        if !self.in_hour(p.step) {
            return;
        }
        let slot = self.slot(p.step);
        let kib = bytes.div_ceil(1024);
        let (key, frn) = p.row(of);
        let Some(at) = self.row_for(key, frn) else {
            self.low_kib[slot] = self.low_kib[slot].saturating_add(kib);
            return;
        };
        let row = &mut self.rows[at];
        row.written[slot] = u32::try_from(u64::from(row.written[slot]) + kib).unwrap_or(u32::MAX);
        row.marks[slot] |= marks.0;
    }

    /// Marks the row of `key` in `step` with `marks`, when it has one.
    fn mark(&mut self, key: u64, step: i64, marks: Marks) {
        if !self.in_hour(step) {
            return;
        }
        let slot = self.slot(step);
        if let Some(at) = self.find(key) {
            self.rows[at].marks[slot] |= marks.0;
        }
    }

    /// Adds the bytes of this batch's writes by rule 4, from the size of each
    /// file: `sizes` answers the size the index holds before this batch's
    /// size step and the size the step read, `None` for one that is unknown
    /// or was not read. `pending` is sorted by file ID, each file's values in
    /// the order of their records.
    pub(crate) fn add_sizes(
        &mut self,
        pending: &[PendingWrite],
        sizes: impl Fn(Frn) -> (Option<u64>, Option<u64>),
    ) {
        for file in pending.chunk_by(|a, b| a.frn == b.frn) {
            let (before, read) = sizes(file[0].frn);
            let writes = || file.iter().filter(|p| !p.renamed);
            if let Some(last) = writes().next_back() {
                let created = writes().any(|p| p.created);
                match read {
                    // Gone, or the read failed: its bytes are missing.
                    None => {
                        for p in writes() {
                            self.mark(folder_key(p.folder), p.step, Marks::AT_LEAST);
                            if !p.created {
                                self.mark(p.name, p.step, Marks::AT_LEAST);
                            }
                        }
                    }
                    Some(size) => {
                        // The growth once, to the rows of the file's last value.
                        let (growth, marks) = match before {
                            Some(before) => (size.saturating_sub(before), last.marks),
                            None if created => (size, last.marks),
                            None => (size, last.marks.with(Marks::UP_TO)),
                        };
                        if growth > 0 {
                            self.add_file_bytes(last, created, growth, marks);
                        }
                        for p in writes().filter(|p| p.closed) {
                            if p.marks.holds_any(Marks::REPLACED.with(Marks::WRITTEN_OVER)) {
                                let marks = p.marks.with(Marks::UP_TO);
                                self.add_file_bytes(p, p.created, size, marks);
                            }
                        }
                    }
                }
            }
            for p in file.iter().filter(|p| p.renamed) {
                match read.or(before) {
                    Some(size) => self.add_bytes(p, Of::Name, size, Marks::UP_TO),
                    None => self.mark(p.name, p.step, Marks::AT_LEAST),
                }
            }
        }
    }

    /// Adds `bytes` of the write `p` to its folder's row, and to its name's
    /// row unless the open created the file.
    fn add_file_bytes(&mut self, p: &PendingWrite, created: bool, bytes: u64, marks: Marks) {
        self.add_bytes(p, Of::Folder, bytes, marks);
        if !created {
            self.add_bytes(p, Of::Name, bytes, marks);
        }
    }

    /// Marks the rows of `pending` "at least": a walk of a missed folder
    /// failed before the size step, so their bytes are missing.
    pub(crate) fn mark_at_least(&mut self, pending: &[PendingWrite]) {
        for p in pending {
            if !p.renamed {
                self.mark(folder_key(p.folder), p.step, Marks::AT_LEAST);
            }
            if p.renamed || !p.created {
                self.mark(p.name, p.step, Marks::AT_LEAST);
            }
        }
    }

    /// Moves the last hour to end in the step of `time`, a FILETIME: a later
    /// step empties the steps it leaves behind, and a time more than an hour
    /// before the table's step means that the clock was set back, so the
    /// table empties and counts from `time`.
    pub fn move_to(&mut self, time: i64) {
        let step = time.div_euclid(STEP_TICKS);
        if step > self.step {
            self.age(step);
        } else if self.is_set_back(step) {
            self.rows.clear();
            self.slots.fill(0);
            self.low_writes = [0; STEPS];
            self.low_kib = [0; STEPS];
            self.step = step;
            self.counted_since = Some(time);
        }
    }

    /// Whether [`Self::move_to`] would change the table at `time`, a
    /// FILETIME: a later step, or a clock set back.
    pub fn needs_move(&self, time: i64) -> bool {
        let step = time.div_euclid(STEP_TICKS);
        self.is_kept() && (step > self.step || self.is_set_back(step))
    }

    /// Whether `step` is more than an hour before the table's step.
    fn is_set_back(&self, step: i64) -> bool {
        step < self.step - STEPS as i64
    }

    /// Ends the hour in `step`, emptying each step it leaves, and drops the
    /// rows that hold nothing then.
    fn age(&mut self, step: i64) {
        let gone = (step - self.step).min(STEPS as i64);
        for passed in 1..=gone {
            let slot = self.slot(self.step + passed);
            self.low_writes[slot] = 0;
            self.low_kib[slot] = 0;
            for row in &mut self.rows {
                row.counts[slot] = 0;
                row.written[slot] = 0;
                row.marks[slot] = 0;
            }
        }
        self.step = step;
        let before = self.rows.len();
        self.rows.retain(|row| !row.is_empty());
        if self.rows.len() != before {
            self.index_rows();
        }
    }

    fn slot(&self, step: i64) -> usize {
        step.rem_euclid(STEPS as i64) as usize
    }

    fn in_hour(&self, step: i64) -> bool {
        step > self.step - STEPS as i64 && step <= self.step
    }

    /// The place of the row of `key`.
    fn find(&self, key: u64) -> Option<usize> {
        if !self.is_kept() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut at = home(key, mask);
        loop {
            match self.slots[at] {
                0 => return None,
                place if self.rows[place as usize - 1].key == key => {
                    return Some(place as usize - 1);
                }
                _ => at = (at + 1) & mask,
            }
        }
    }

    /// The place of the row of `key`, made for `frn` in this batch when the
    /// table has none; `None` when the table is full of rows this batch
    /// counted in.
    fn row_for(&mut self, key: u64, frn: Frn) -> Option<usize> {
        if let Some(at) = self.find(key) {
            self.rows[at].batch = self.batch;
            return Some(at);
        }
        if self.rows.len() == self.size() {
            if self.full {
                return None;
            }
            self.shed(self.size().div_ceil(8));
            if self.rows.len() == self.size() {
                self.full = true;
                return None;
            }
        }
        self.rows.push(Row::new(key, frn, self.batch));
        self.place(self.rows.len() - 1);
        Some(self.rows.len() - 1)
    }

    /// Drops up to `count` rows, the least written first, and adds the most
    /// that a dropped row held in each step to "may be low by". It adds,
    /// rather than keeps the highest, because a name dropped, written again
    /// and dropped again loses a count at each pass. It never drops a row
    /// that this batch counted in.
    fn shed(&mut self, count: usize) {
        let batch = self.batch;
        let score = |row: &Row| {
            if row.batch == batch {
                u64::MAX
            } else {
                row.score()
            }
        };
        self.scores.clear();
        self.scores.extend(self.rows.iter().map(score));
        let count = count.min(self.scores.len());
        if count == 0 {
            return;
        }
        let (_, &mut threshold, _) = self.scores.select_nth_unstable(count - 1);
        let below = self.scores.iter().filter(|&&s| s < threshold).count();
        let mut equal = if threshold == u64::MAX {
            0
        } else {
            count - below
        };
        let mut most_writes = [0u16; STEPS];
        let mut most_kib = [0u32; STEPS];
        self.rows.retain(|row| {
            let s = score(row);
            let drop = s < threshold || (s == threshold && equal > 0);
            if drop {
                if s == threshold {
                    equal -= 1;
                }
                for slot in 0..STEPS {
                    most_writes[slot] = most_writes[slot].max(row.counts[slot]);
                    most_kib[slot] = most_kib[slot].max(row.written[slot]);
                }
            }
            !drop
        });
        for slot in 0..STEPS {
            self.low_writes[slot] = self.low_writes[slot].saturating_add(most_writes[slot].into());
            self.low_kib[slot] = self.low_kib[slot].saturating_add(most_kib[slot].into());
        }
        self.index_rows();
    }

    /// Fills the slots again from the rows.
    fn index_rows(&mut self) {
        self.slots.fill(0);
        for at in 0..self.rows.len() {
            self.place(at);
        }
    }

    fn place(&mut self, at: usize) {
        let mask = self.slots.len() - 1;
        let mut slot = home(self.rows[at].key, mask);
        while self.slots[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.slots[slot] = u32::try_from(at + 1).expect("a table holds at most MOST_ROWS rows");
    }

    /// The values of the name `folded_name`, as the index stores it, in
    /// `folder`; `None` when it has no row.
    pub fn of_name(&self, folder: Frn, folded_name: &[u8]) -> Option<Values> {
        self.find(name_key(folder, folded_name))
            .map(|at| self.rows[at].values())
    }

    /// The values of folder `folder`: the writes of the files directly in
    /// it; `None` when it has no row.
    pub fn of_folder(&self, folder: Frn) -> Option<Values> {
        self.find(folder_key(folder))
            .map(|at| self.rows[at].values())
    }

    /// The facts a client reads in `--status`; `None` for a table that keeps
    /// nothing.
    pub fn status(&self) -> Option<WritesStatus> {
        Some(WritesStatus {
            counted_since: self.counted_since?,
            rows: self.rows.len() as u32,
            size: self.size() as u32,
            low_writes: self.low_writes(),
            low_bytes: self.low_bytes(),
        })
    }

    /// "May be low by", in writes.
    fn low_writes(&self) -> u64 {
        self.low_writes.iter().map(|&n| u64::from(n)).sum()
    }

    /// "May be low by", in bytes.
    fn low_bytes(&self) -> u64 {
        self.low_kib.iter().sum::<u64>().saturating_mul(1024)
    }

    /// The table counted for less than the hour that ends in its step, so
    /// a write before it is missing.
    pub fn is_part_of_hour(&self) -> bool {
        // A corrupt record's time can put the step's start past `i64`.
        let hour_start = (self.step - STEPS as i64 + 1).saturating_mul(STEP_TICKS);
        self.counted_since.is_none_or(|since| since > hour_start)
    }

    /// How far the values may be low, for a search to read once.
    pub fn low(&self) -> Low {
        Low {
            open: self.is_part_of_hour(),
            writes: self.low_writes(),
            bytes: self.low_bytes(),
        }
    }

    /// The values of entry `idx` of `index` as `seen_by` may read them;
    /// `None` when they are unknown: the table keeps nothing, or `idx` is a
    /// folder that `seen_by` may not list (rule 11). An entry without a row,
    /// or whose row counted another file (rule 2), had no write.
    pub fn of_entry(&self, index: &VolumeIndex, idx: EntryIdx, seen_by: &SeenBy) -> Option<Values> {
        if !self.is_kept() {
            return None;
        }
        let entry = index.entry(idx);
        if entry.is_dir() && !index.sees_into(idx, seen_by) {
            return None;
        }
        let frn = entry.frn();
        let values = self
            .find(key_of(index, idx))
            .map(|at| self.rows[at].values())
            .filter(|values| values.frn == frn)
            .unwrap_or(Values::none(frn));
        Some(values)
    }
}

/// Puts `item` at the end of a list of rule 1, dropping its oldest item when
/// the list is full, so it never grows.
fn push_within<T>(list: &mut Vec<T>, item: T) {
    if list.len() == PAIRS {
        list.remove(0);
    }
    list.push(item);
}

/// The key of each record's name in its folder, for `Writes::count`,
/// folded as the index stores names. It reads only the records, so it runs
/// before the index's write lock.
pub fn name_keys(records: &[ParsedUsnRecord]) -> Vec<u64> {
    let mut folded = Vec::new();
    records
        .iter()
        .map(|rec| {
            folded.clear();
            fold_accents_into(&rec.name, &mut folded);
            name_key(rec.parent_frn, &folded)
        })
        .collect()
}

/// The key of the name `folded`, as the index stores it, in `folder`.
fn name_key(folder: Frn, folded: &[u8]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    hasher.write_u128(folder.0);
    hasher.write_usize(folded.len());
    hasher.write(folded);
    hasher.finish()
}

/// The key of folder `folder`'s own row, which no name's key equals, because
/// no name is empty.
fn folder_key(folder: Frn) -> u64 {
    name_key(folder, &[])
}

/// The key of entry `idx`'s row: a folder's own, or a file's name in the
/// folder that holds it, as the index stores the name.
fn key_of(index: &VolumeIndex, idx: EntryIdx) -> u64 {
    let entry = index.entry(idx);
    if entry.is_dir() {
        folder_key(entry.frn())
    } else {
        name_key(index.entry(entry.parent()).frn(), index.folded_name(idx))
    }
}

/// The first slot to try for `key`: its top bits after a Fibonacci multiply,
/// because the hasher's low bits are its weakest.
fn home(key: u64, mask: usize) -> usize {
    let bits = mask.count_ones();
    (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - bits)) as usize
}

#[cfg(test)]
mod tests;
