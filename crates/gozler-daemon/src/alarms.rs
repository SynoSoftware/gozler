//! The wear alarm's checks that no volume holds ([`gozler_core::alarm`]):
//! what each program wrote in the last hour, from the processes' write
//! counters, and what each disk wrote in the last week, from the lifetime
//! total it reports. Two samplers, one for the programs and one for the
//! disks, each sample every 5 minutes while a window watches their limit
//! ([`crate::wear::watched`]), on the blocking pool, and each waits for its
//! last sample. So a disk query that hangs stops only the disks' samples,
//! and holds one thread, which a `DiskHealth` request that comes meanwhile
//! waits on too ([`crate::disks::read_shared`]).
//!
//! The disks' daily samples are kept in the registry beside the Include
//! settings, where only administrators and the system can write, so no
//! user can plant a file for the service to write through.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::Duration;

use gozler_core::alarm::{
    DiskWeeks, Levels, Process, Programs, Warning, Warnings, Watched, disk_key, week_allowance,
};
use gozler_core::include::SETTINGS_KEY;
use gozler_core::proto::{Alarm, Alarmed, WearLimits};
use gozler_winfs::ProcessWrites;

use crate::disks::DiskRead;
use crate::wear::NUMBERS;

/// How often the counters are read: one step of the table of writes.
const EVERY: Duration = Duration::from_secs(5 * 60);

/// The value of [`SETTINGS_KEY`] that holds the disks' daily samples as
/// JSON ([`DiskWeeks`]).
const SAMPLES_VALUE: &str = "DiskWrites";

/// What the samples found, shared by the task that takes them and the held
/// wear requests that report them.
struct State {
    /// `None` while no window watches the program limit, so a new watch
    /// starts from a base instead of from counters read long ago.
    programs: Option<Programs>,
    /// Programs that crossed the limit, by full path.
    program_warnings: Warnings<String>,
    /// Read from the registry at the first disk sample.
    weeks: Option<DiskWeeks>,
    /// Disks that crossed the limit, by [`disk_key`].
    disk_warnings: Warnings<String>,
    /// What a reply names each disk by, by [`disk_key`].
    disks: HashMap<String, DiskName>,
}

struct DiskName {
    model: Option<String>,
    mounts: Vec<String>,
    /// When the week that crossed the limit started, a FILETIME.
    since: i64,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    let mut program_warnings = Warnings::allocate();
    program_warnings.number_from(&NUMBERS);
    let mut disk_warnings = Warnings::allocate();
    disk_warnings.number_from(&NUMBERS);
    Mutex::new(State {
        programs: None,
        program_warnings,
        weeks: None,
        disk_warnings,
        disks: HashMap::new(),
    })
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Samples the program and the disk counters every 5 minutes, for as long
/// as the pipe server's runtime runs.
pub(crate) async fn sample_every_step() {
    tokio::join!(every_step(sample_programs), every_step(sample_disks));
}

/// Runs `sample` with the watched limits every 5 minutes. The next sample
/// waits for the last one.
async fn every_step(sample: fn(&Watched)) {
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let watched = crate::wear::watched();
        if tokio::task::spawn_blocking(move || sample(&watched))
            .await
            .is_err()
        {
            tracing::error!("the wear alarm's sample panicked");
        }
    }
}

/// Reads the processes' write counters while a window watches the program
/// limit, and wakes the held wear requests when a program crossed it.
fn sample_programs(watched: &Watched) {
    let limits = &watched.program_written;
    if limits.is_off() {
        state().programs = None;
        return;
    }
    let now = gozler_winfs::system_time();
    match gozler_winfs::process_writes() {
        Ok(processes) => {
            if state().check_programs(now, &processes, limits) {
                crate::wear::wake();
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not list the processes"),
    }
}

/// Reads the disks' lifetime totals while a window watches the disk limit,
/// and wakes the held wear requests when a disk crossed it.
fn sample_disks(watched: &Watched) {
    let limits = &watched.disk_week;
    if limits.is_off() {
        return;
    }
    // Read before the state is locked: a disk query can wait.
    let read = crate::disks::read_shared();
    let now = gozler_winfs::system_time();
    match &*read {
        Ok(disks) => {
            if state().check_disks(now, disks, limits) {
                crate::wear::wake();
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not list the disks"),
    }
}

impl State {
    /// Answers whether a program crossed a limit.
    fn check_programs(&mut self, now: i64, processes: &[ProcessWrites], limits: &Levels) -> bool {
        let programs = self.programs.get_or_insert_with(Programs::default);
        programs.sample(
            now,
            processes.iter().map(|p| Process {
                id: p.id,
                created: p.created,
                image: &p.image,
                written: p.written,
            }),
        );
        // A program that wrote nothing in the hour has no row.
        self.program_warnings
            .rearm(|program| Some((programs.written(program).unwrap_or(0), limits)));
        let mut crossed = false;
        for (program, written) in programs.hour() {
            if let Some(reading) = limits.reading(written, false, now) {
                crossed |= self.program_warnings.check(program.to_owned(), reading);
            }
        }
        crossed
    }

    /// Answers whether a disk crossed a limit.
    fn check_disks(&mut self, now: i64, disks: &[DiskRead], limits: &Levels) -> bool {
        let weeks = self.weeks.get_or_insert_with(read_weeks);
        let mut changed = false;
        let mut read = HashMap::new();
        // A disk with no serial number could not be told from another put
        // in its place, and one that does not report its total has no week.
        for disk in disks {
            let (Some(serial), Some(written)) = (&disk.serial, disk.disk.written) else {
                continue;
            };
            let key = disk_key(serial);
            let (week, saved) = weeks.add(&key, now, written);
            changed |= saved;
            let name = self.disks.entry(key.clone()).or_insert(DiskName {
                model: None,
                mounts: Vec::new(),
                since: week.since,
            });
            name.model.clone_from(&disk.disk.model);
            name.mounts.clone_from(&disk.disk.mounts);
            read.insert(key, week);
        }
        if changed {
            let json = weeks.to_json();
            if let Err(e) = gozler_winfs::set_machine_strings(SETTINGS_KEY, SAMPLES_VALUE, &[json])
            {
                tracing::warn!(error = %e, "could not save the disks' daily samples");
            }
        }
        // A disk this sample could not read keeps its warning, so a read
        // that fails once does not make it warn again.
        self.disk_warnings
            .rearm(|key| read.get(key).map(|week| (week.written, limits)));
        let mut crossed = false;
        for (key, week) in read {
            let Some(reading) = limits.reading(week.written, false, now) else {
                continue;
            };
            if self.disk_warnings.check(key.clone(), reading) {
                crossed = true;
                if let Some(name) = self.disks.get_mut(&key) {
                    name.since = week.since;
                }
            }
        }
        crossed
    }
}

/// The disks' saved daily samples; none when they are missing or cannot be
/// read, which only shortens each disk's week, so it counts low.
fn read_weeks() -> DiskWeeks {
    let saved = match gozler_winfs::machine_strings(SETTINGS_KEY, SAMPLES_VALUE) {
        Ok(saved) => saved,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the disks' daily samples");
            None
        }
    };
    let Some(json) = saved.as_ref().and_then(|lines| lines.first()) else {
        return DiskWeeks::default();
    };
    DiskWeeks::from_json(json).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the disks' daily samples do not parse");
        DiskWeeks::default()
    })
}

/// The programs and disks that crossed a limit and that `wanted` keeps by
/// their number, as a reply to a request with `limits` sends them, and how
/// many crossings the lists dropped. A client that does not see every file
/// gets only a program's file name, because the service sees every user's
/// programs.
pub(crate) fn reported(
    limits: &WearLimits,
    wanted: impl Fn(u64) -> bool,
    sees_everything: bool,
) -> (Vec<Alarm>, u64) {
    let state = state();
    let mut alarms = Vec::new();
    if let Some(limit) = limits.program_written {
        let reaches = |w: &&Warning<String>| wanted(w.number) && w.reading.value >= limit;
        for warning in state.program_warnings.iter().filter(reaches) {
            let path = if sees_everything {
                warning.subject.clone()
            } else {
                let name = Path::new(&warning.subject).file_name();
                name.map_or_else(String::new, |name| name.to_string_lossy().into_owned())
            };
            alarms.push(warning.alarm(Alarmed::ProgramWritten { path }));
        }
    }
    if let Some(factor) = limits.disk_week {
        let limit = week_allowance(factor);
        let reaches = |w: &&Warning<String>| wanted(w.number) && w.reading.value >= limit;
        for warning in state.disk_warnings.iter().filter(reaches) {
            let name = state.disks.get(&warning.subject);
            let what = Alarmed::DiskWeek {
                model: name.and_then(|name| name.model.clone()),
                mounts: name.map(|name| name.mounts.clone()).unwrap_or_default(),
                since: name.map_or(warning.reading.time, |name| name.since),
            };
            alarms.push(warning.alarm(what));
        }
    }
    let dropped = state.program_warnings.dropped() + state.disk_warnings.dropped();
    (alarms, dropped)
}
