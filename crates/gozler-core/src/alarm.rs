//! The wear alarm: the limits a window sets on how much is written, and the
//! list of the limits crossed that the service sends it ([`Warnings`]).
//!
//! The files and folders are judged from each volume's table of writes
//! (`index::alarms`). This module holds what no volume holds: each
//! program's bytes over the last hour ([`Programs`]), from the write
//! counters the service samples every 5 minutes, and each disk's bytes over
//! the last week ([`DiskWeeks`]), from the lifetime total the disk reports.
//! Every time comes in as an argument, a FILETIME; nothing here reads a
//! clock or the system.

use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::index::writes::{STEP_TICKS, STEPS};
use crate::proto::{Alarm, Alarmed, WearLimits};

/// The bytes a typical user writes in a day: "A typical client usage of 20
/// GB writes per day is assumed." (Intel X18-M/X25-M SATA SSD Product
/// Manual, order 319765-002US, section 3.5.4, July 2008). Other sources
/// give 5 to 40 GB; this one is a drive maker's written assumption for
/// client drives and the middle of that range. The figure is a rough
/// estimate, far coarser than the 7 % between 10^9 and 2^30 bytes, so a GB
/// here is 2^30 bytes, as the window shows sizes.
pub const TYPICAL_DAY: u64 = 20 << 30;

/// One day and one week, in FILETIME ticks of 100 ns.
pub const DAY_TICKS: i64 = 24 * 60 * 60 * 10_000_000;
pub const WEEK_TICKS: i64 = 7 * DAY_TICKS;

/// The most crossings a list of [`Warnings`] keeps.
const WARNINGS: usize = 64;

/// The most programs [`Programs`] keeps; a program past them is not judged
/// until one leaves the hour.
const MOST_PROGRAMS: usize = 4096;

/// The daily samples [`DiskWeeks`] keeps for each disk: one more than a
/// week, so the sample from 7 days ago is still there.
const SAMPLES: usize = 8;

/// The most disks [`DiskWeeks`] keeps.
const MOST_DISKS: usize = 64;

/// The week's bytes that `factor` times a typical user writes.
pub fn week_allowance(factor: u32) -> u64 {
    u64::from(factor) * 7 * TYPICAL_DAY
}

/// One limit of the wear alarm as the windows watch it: each window's
/// value, lowest first, each once. A subject is read against the highest
/// value it reaches, so a window's low limit cannot hide another window's
/// higher one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Levels(Vec<u64>);

impl Levels {
    pub fn is_off(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds `limit` when it is on.
    pub fn add(&mut self, limit: Option<u64>) {
        if let Some(limit) = limit
            && let Err(at) = self.0.binary_search(&limit)
        {
            self.0.insert(at, limit);
        }
    }

    /// The highest limit that `value` reaches; `None` while it is under
    /// every limit.
    pub fn reached(&self, value: u64) -> Option<u64> {
        let reached = self.0.partition_point(|&limit| limit <= value);
        self.0.get(reached.checked_sub(1)?).copied()
    }

    /// `value` read against the highest limit it reaches; `None` while it
    /// is under every limit.
    pub fn reading(&self, value: u64, at_least: bool, time: i64) -> Option<Reading> {
        let limit = self.reached(value)?;
        Some(Reading {
            value,
            at_least,
            limit,
            time,
        })
    }
}

/// The limits that the windows watch, each in the unit it is checked in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watched {
    pub file_writes: Levels,
    pub file_written: Levels,
    pub folder_written: Levels,
    pub program_written: Levels,
    /// In bytes a week ([`week_allowance`]).
    pub disk_week: Levels,
}

impl Watched {
    /// Adds the limits of one window's request.
    pub fn add(&mut self, limits: &WearLimits) {
        self.file_writes.add(limits.file_writes.map(u64::from));
        self.file_written.add(limits.file_written);
        self.folder_written.add(limits.folder_written);
        self.program_written.add(limits.program_written);
        self.disk_week.add(limits.disk_week.map(week_allowance));
    }
}

/// What a check read against a limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The writes or bytes read.
    pub value: u64,
    /// `value` is a lower bound.
    pub at_least: bool,
    /// The limit `value` is compared with.
    pub limit: u64,
    /// When it was read, a FILETIME.
    pub time: i64,
}

/// A limit that `subject` crossed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning<S> {
    pub subject: S,
    /// The reading that crossed the limit.
    pub reading: Reading,
    /// The service's sequence number of the crossing.
    pub number: u64,
    /// The subject is still over the limit, so it does not warn again.
    over: bool,
}

impl<S> Warning<S> {
    /// The crossing as a reply sends it, with `what` crossed the limit.
    pub fn alarm(&self, what: Alarmed) -> Alarm {
        Alarm {
            number: self.number,
            what,
            value: self.reading.value,
            at_least: self.reading.at_least,
            limit: self.reading.limit,
            time: self.reading.time,
        }
    }
}

/// The limits crossed, each subject's latest, in a list of fixed size. A
/// subject warns once when it crosses a limit, again when it crosses a
/// higher one that a window watches, and again only after [`Self::rearm`]
/// finds it under the limit it crossed. A full list makes room by dropping
/// its weakest crossing: one under its limit first, then the one with the
/// lowest limit, then the one numbered lowest. It drops that one only when
/// it is under its limit or its limit is lower than the new crossing's;
/// otherwise it drops the new crossing. So many subjects over a low limit
/// cannot keep a higher limit from warning, and a subject dropped while
/// over its limit does not warn again at that limit while the list is
/// full. [`Self::dropped`] counts each crossing dropped or refused, so a
/// subject refused at each check counts at each check.
pub struct Warnings<S> {
    list: Vec<Warning<S>>,
    /// How many crossings the list dropped.
    dropped: u64,
    /// The service's sequence counter. A list without one records nothing,
    /// so an index that was never installed in the service warns about
    /// nothing.
    numbers: Option<&'static AtomicU64>,
}

impl<S: PartialEq> Warnings<S> {
    /// A list that holds no memory and records nothing.
    pub const fn none() -> Self {
        Warnings {
            list: Vec::new(),
            dropped: 0,
            numbers: None,
        }
    }

    /// Allocates the list at its full size. Allocate outside the index's
    /// write lock.
    pub fn allocate() -> Self {
        Warnings {
            list: Vec::with_capacity(WARNINGS),
            ..Self::none()
        }
    }

    /// Gives the list the service's sequence counter, from which it numbers
    /// every crossing from now on.
    pub fn number_from(&mut self, numbers: &'static AtomicU64) {
        self.numbers = Some(numbers);
    }

    /// Records `reading` of `subject`, read against the highest limit it
    /// reaches ([`Levels::reading`]), with the counter's next number, unless
    /// the subject is already over a limit as high. Answers whether it
    /// recorded a crossing.
    pub fn check(&mut self, subject: S, reading: Reading) -> bool {
        let Some(numbers) = self.numbers else {
            return false;
        };
        let at = self.list.iter().position(|w| w.subject == subject);
        if let Some(at) = at {
            let stored = &self.list[at];
            if stored.over && stored.reading.limit >= reading.limit {
                return false;
            }
        } else if self.list.len() == WARNINGS {
            let weakest = (0..self.list.len())
                .min_by_key(|&at| {
                    let w = &self.list[at];
                    (w.over, w.reading.limit, w.number)
                })
                .expect("a full list holds warnings");
            self.dropped += 1;
            let w = &self.list[weakest];
            if w.over && w.reading.limit >= reading.limit {
                return false;
            }
            self.list.swap_remove(weakest);
        }
        let warning = Warning {
            subject,
            reading,
            number: numbers.fetch_add(1, Ordering::AcqRel) + 1,
            over: true,
        };
        match at {
            Some(at) => self.list[at] = warning,
            None => self.list.push(warning),
        }
        true
    }

    /// Lets each subject that `value` now reads under every limit it
    /// crossed warn again. `value` answers a subject's value now and the
    /// limits watched for it, or `None` for a subject it did not read,
    /// which stays as it was. A subject under the highest limit it crossed
    /// but still at a lower watched one stays over that lower one, so it
    /// warns again at the higher limit, and not at the lower one.
    pub fn rearm<'a>(&mut self, mut value: impl FnMut(&S) -> Option<(u64, &'a Levels)>) {
        for warning in self.list.iter_mut().filter(|w| w.over) {
            let Some((value, levels)) = value(&warning.subject) else {
                continue;
            };
            if value >= warning.reading.limit {
                continue;
            }
            match levels.reached(value) {
                Some(lower) => warning.reading.limit = lower,
                None => warning.over = false,
            }
        }
    }

    /// Whether some subject is over its limit, so [`Self::rearm`] has work.
    pub fn any_over(&self) -> bool {
        self.list.iter().any(|w| w.over)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Warning<S>> {
        self.list.iter()
    }

    /// How many crossings the list dropped since it was made.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// A process as the service samples it.
#[derive(Clone, Copy, Debug)]
pub struct Process<'a> {
    pub id: u32,
    /// When the process started, a FILETIME. With `id`, it tells the
    /// process from a later one that reuses its id.
    pub created: i64,
    /// The full path of its program.
    pub image: &'a str,
    /// The bytes it asked to write since it started, to any handle: files,
    /// pipes and devices (`WriteTransferCount`).
    pub written: u64,
}

/// The bytes each program wrote in each step of the last hour, by the full
/// path of the program, summed over its processes. A program keeps its row
/// after its processes exit, until its steps leave the hour.
#[derive(Default)]
pub struct Programs {
    /// Each process at the last sample, with its counter then.
    seen: HashMap<(u32, i64), u64>,
    rows: HashMap<String, [u64; STEPS]>,
    /// The step the last hour ends in: a FILETIME divided by
    /// [`STEP_TICKS`].
    step: i64,
    /// When the last sample was taken; `None` before the first.
    sampled: Option<i64>,
}

impl Programs {
    /// Adds what each of `processes` wrote since the last sample to its
    /// program's step of `now`. A process that the last sample did not see
    /// and that started before it gives only its counter, as the base of
    /// the next sample: its counter holds the bytes of its whole life. A
    /// process that ran only between two samples is missed.
    pub fn sample<'a>(&mut self, now: i64, processes: impl IntoIterator<Item = Process<'a>>) {
        let step = now.div_euclid(STEP_TICKS);
        self.move_to(step);
        let slot = step.rem_euclid(STEPS as i64) as usize;
        let mut seen = HashMap::with_capacity(self.seen.len());
        for process in processes {
            let id = (process.id, process.created);
            let before = match self.seen.get(&id) {
                Some(&before) => Some(before),
                None if self.sampled.is_some_and(|at| process.created >= at) => Some(0),
                None => None,
            };
            seen.insert(id, process.written);
            let Some(grew) = before.map(|before| process.written.saturating_sub(before)) else {
                continue;
            };
            if grew == 0 {
                continue;
            }
            if let Some(row) = self.rows.get_mut(process.image) {
                row[slot] = row[slot].saturating_add(grew);
            } else if self.rows.len() < MOST_PROGRAMS {
                let mut row = [0; STEPS];
                row[slot] = grew;
                self.rows.insert(process.image.to_owned(), row);
            }
        }
        self.seen = seen;
        self.sampled = Some(now);
    }

    /// Ends the hour in `step`, emptying each step it leaves, and drops the
    /// programs that wrote nothing in the hour. A step more than an hour
    /// before the last one means that the clock was set back, so every
    /// program's hour starts again.
    fn move_to(&mut self, step: i64) {
        if step > self.step {
            for passed in 1..=(step - self.step).min(STEPS as i64) {
                let slot = (self.step + passed).rem_euclid(STEPS as i64) as usize;
                for row in self.rows.values_mut() {
                    row[slot] = 0;
                }
            }
            self.step = step;
            self.rows
                .retain(|_, row| row.iter().any(|&bytes| bytes > 0));
        } else if step <= self.step - STEPS as i64 {
            self.rows.clear();
            self.step = step;
        }
    }

    /// The bytes `program` wrote in the last hour; `None` when it wrote none.
    pub fn written(&self, program: &str) -> Option<u64> {
        self.rows.get(program).map(|row| row.iter().sum())
    }

    /// Each program and the bytes it wrote in the last hour.
    pub fn hour(&self) -> impl Iterator<Item = (&str, u64)> {
        self.rows
            .iter()
            .map(|(program, row)| (program.as_str(), row.iter().sum()))
    }
}

/// The bytes a disk wrote over a week or less.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Week {
    pub written: u64,
    /// When the sample that `written` counts from was taken, a FILETIME at
    /// most [`WEEK_TICKS`] before the reading.
    pub since: i64,
}

/// Each disk's lifetime total of bytes written, once a day for the last 8
/// days, which the service keeps across restarts. A disk is known by
/// [`disk_key`].
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskWeeks {
    disks: Vec<DiskSamples>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DiskSamples {
    key: String,
    /// Oldest first.
    samples: Vec<Sample>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Sample {
    time: i64,
    written: u64,
}

impl DiskWeeks {
    /// The samples saved as [`Self::to_json`] wrote them, past the most
    /// disks and samples cut off. A sample before 1601, which no FILETIME
    /// is, is dropped, so [`Self::add`] can subtract any sample's time from
    /// the current one.
    ///
    /// # Errors
    ///
    /// The text is not the JSON of the samples.
    pub fn from_json(json: &str) -> Result<DiskWeeks, serde_json::Error> {
        let mut weeks: DiskWeeks = serde_json::from_str(json)?;
        weeks.disks.truncate(MOST_DISKS);
        for disk in &mut weeks.disks {
            disk.samples.retain(|sample| sample.time >= 0);
            disk.samples.sort_unstable_by_key(|sample| sample.time);
            let extra = disk.samples.len().saturating_sub(SAMPLES);
            disk.samples.drain(..extra);
        }
        Ok(weeks)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("the samples are plain numbers and strings")
    }

    /// Reads that the disk of `key` has written `written` bytes over its
    /// life at `now`, and answers what it wrote since the oldest sample at
    /// most a week old, and whether the samples changed so that they need
    /// saving. A day after the newest sample, `now` becomes a sample.
    ///
    /// The week can only count low: it starts at most a week ago, and a
    /// disk with a shorter history counts from its first sample. A total
    /// below a sample, from another disk with the same key or a reset
    /// counter, drops the disk's samples, and so does a sample after `now`,
    /// from a clock set back.
    pub fn add(&mut self, key: &str, now: i64, written: u64) -> (Week, bool) {
        let at = match self.disks.iter().position(|disk| disk.key == key) {
            Some(at) => at,
            None => {
                if self.disks.len() == MOST_DISKS {
                    let stalest = (0..self.disks.len())
                        .min_by_key(|&at| self.disks[at].samples.last().map(|s| s.time))
                        .expect("a full list holds disks");
                    self.disks.swap_remove(stalest);
                }
                self.disks.push(DiskSamples {
                    key: key.to_owned(),
                    samples: Vec::with_capacity(SAMPLES),
                });
                self.disks.len() - 1
            }
        };
        let samples = &mut self.disks[at].samples;
        let count = samples.len();
        if samples.iter().any(|s| s.written > written || s.time > now) {
            samples.clear();
        }
        let due = samples
            .last()
            .is_none_or(|last| now - last.time >= DAY_TICKS);
        if due {
            if samples.len() == SAMPLES {
                samples.remove(0);
            }
            samples.push(Sample { time: now, written });
        }
        let oldest = samples
            .iter()
            .find(|s| now - s.time <= WEEK_TICKS)
            .expect("the newest sample is at most a day old");
        let week = Week {
            written: written - oldest.written,
            since: oldest.time,
        };
        (week, due || samples.len() != count)
    }
}

/// The key a disk's samples are kept under: a hash of its serial number,
/// so the saved samples do not hold the serial. Another hash function in a
/// later build only starts every disk's history again, which counts low.
pub fn disk_key(serial: &str) -> String {
    let mut hasher = rustc_hash::FxHasher::default();
    hasher.write(serial.as_bytes());
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    static NUMBERS: AtomicU64 = AtomicU64::new(0);

    /// A list that numbers its crossings.
    fn list<S: PartialEq>() -> Warnings<S> {
        let mut warnings = Warnings::allocate();
        warnings.number_from(&NUMBERS);
        warnings
    }

    fn levels(limits: &[u64]) -> Levels {
        let mut levels = Levels::default();
        for &limit in limits {
            levels.add(Some(limit));
        }
        levels
    }

    /// A window that watches 1 write an hour must not keep another window
    /// at 3,600 from warning. Read against 1 alone, a file would warn once
    /// at 1, never again while it stays over, and the reply to the window
    /// at 3,600 leaves a crossing read at 1 out. A file that falls back
    /// under 3,600 but stays over 1 must warn again at 3,600 only; warned
    /// again at 1, it would warn each time it went over 3,600 and back.
    #[test]
    fn a_low_limit_of_another_window_does_not_silence_a_higher_one() {
        let mut warnings = list();
        let watched = levels(&[1, 3_600]);
        let at = |value| watched.reading(value, false, 0).expect("over the lowest");
        assert!(warnings.check("nvlog.bin", at(1)));
        assert!(!warnings.check("nvlog.bin", at(2_000)));
        assert!(warnings.check("nvlog.bin", at(3_600)));
        assert!(!warnings.check("nvlog.bin", at(5_000)));
        let crossed: Vec<u64> = warnings.iter().map(|w| w.reading.limit).collect();
        assert_eq!(crossed, [3_600]);
        warnings.rearm(|_| Some((2_000, &watched)));
        assert!(!warnings.check("nvlog.bin", at(2_000)));
        assert!(warnings.check("nvlog.bin", at(3_600)));
        // A list full of files over 1 takes a file at 3,600 in place of one
        // of them, and refuses another file at 1.
        let mut full = list();
        for file in 0..WARNINGS as u32 {
            assert!(full.check(file, at(1)));
        }
        assert!(full.check(1_000, at(3_600)));
        assert!(!full.check(1_001, at(1)));
    }

    /// A window that raises its limit from 2 GB to 5 GB must warn when the
    /// file reaches 5 GB, although the file stayed over 2 GB all along.
    #[test]
    fn a_raised_limit_warns_at_its_new_value() {
        let mut warnings = list();
        let before = levels(&[2 * GB]);
        let crossed = before.reading(2 * GB, false, 0).expect("at the limit");
        assert!(warnings.check("cache.db", crossed));
        let after = levels(&[5 * GB]);
        assert_eq!(after.reading(3 * GB, false, 0), None);
        let crossed = after.reading(5 * GB, false, 0).expect("at the new limit");
        assert!(warnings.check("cache.db", crossed));
    }

    /// A disk that one sample could not read must keep its warning. Taken
    /// as under its limit, it would warn again with a new number at the
    /// next sample that reads it.
    #[test]
    fn a_disk_missing_from_a_sample_does_not_warn_again() {
        let mut warnings = list();
        let watched = levels(&[70 * GB]);
        let over = || watched.reading(80 * GB, false, 0).expect("over the limit");
        assert!(warnings.check("disk", over()));
        warnings.rearm(|_| None);
        assert!(!warnings.check("disk", over()));
    }

    /// A write counter read twice in one step must give only what it grew,
    /// and the bytes must leave the hour 12 steps later; a slot error would
    /// warn on bytes written long ago or count a process's whole life.
    #[test]
    fn a_program_crosses_its_limit_only_on_bytes_of_the_last_hour() {
        let mut programs = Programs::default();
        let image = r"C:\nv\container.exe";
        let process = |written| Process {
            id: 40,
            created: 0,
            image,
            written,
        };
        let step = |n: i64| 1_000 * STEP_TICKS + n * STEP_TICKS;
        // The first sample is the base: 50 GB of the process's past life.
        programs.sample(step(0), [process(50 * GB)]);
        assert_eq!(programs.written(image), None);
        // 1 GB in each of the next 12 steps: 12 GB in the hour.
        for n in 1..=12 {
            programs.sample(step(n), [process(50 * GB + n as u64 * GB)]);
        }
        assert_eq!(programs.written(image), Some(12 * GB));
        // A process that starts after the last sample counts from 0.
        let other = Process {
            id: 41,
            created: step(12) + 1,
            image,
            written: 2 * GB,
        };
        programs.sample(step(13), [process(63 * GB), other]);
        assert_eq!(programs.written(image), Some(14 * GB));
        // Two steps with no writes push two steps out of the hour.
        programs.sample(step(15), [process(63 * GB), other]);
        assert_eq!(programs.written(image), Some(12 * GB));
    }

    /// The week must start at most 7 days back, so a burst written 7.5
    /// days ago does not count, and a short history counts from its first
    /// sample. A window that reached the sample 8 days back would warn on
    /// bytes the person wrote before the week.
    #[test]
    fn the_week_never_counts_bytes_from_before_the_week() {
        let mut weeks = DiskWeeks::default();
        let day = |d: f64| (d * DAY_TICKS as f64) as i64;
        weeks.add("disk", day(0.0), 0);
        // 100 GB at day 0.5, then 10 GB a day, read daily.
        for d in 1..=7 {
            weeks.add("disk", day(d as f64), 100 * GB + (d - 1) * 10 * GB);
        }
        // At day 7, the day-0 sample is exactly a week old and still counts.
        let (at_seven, _) = weeks.add("disk", day(7.0), 160 * GB);
        assert_eq!(at_seven.written, 160 * GB);
        // At day 7.6 the burst is past the week: the week starts at day 1.
        let (week, _) = weeks.add("disk", day(7.6), 166 * GB);
        assert_eq!(
            week,
            Week {
                written: 66 * GB,
                since: day(1.0)
            }
        );
        // A new disk under the key, with a lower total, starts again.
        let (fresh, saved) = weeks.add("disk", day(7.7), 5 * GB);
        assert_eq!(fresh.written, 0);
        assert!(saved);
    }
}
