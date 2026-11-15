//! A batch rename (`Request::Rename`): Multi-Rename's renames and its Undo.
//! This module decides, on every OS, the name each entry gets and the order
//! of the renames; `windows` reads the disk for it and renames.
//!
//! The order keeps every rename from replacing an entry: an entry takes a
//! name only after the entry that holds it has moved away, so a shift such
//! as `1→2`, `2→3` runs from the end, and a circle, such as a swap, moves
//! one entry to a temporary name in its folder first. An entry inside a
//! folder that the batch renames goes first, so the path it was prepared
//! with still leads to it.

#[cfg(windows)]
pub(crate) mod windows;

use std::collections::{HashMap, VecDeque};

use crate::names::{Names, fold, numbered_name};

const BACKSLASH: u16 = b'\\' as u16;

/// What a batch rename does with a new name that an entry outside the batch
/// holds, or one that the batch does not move away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InUse {
    /// The entry gets the first free numbered name: Multi-Rename's batch.
    Number,
    /// The entry fails and keeps its name: Undo's batch.
    Fail,
}

/// An entry of the batch, as prepare read it.
pub(crate) struct Entry {
    /// Its final path (D12), folded, so two spellings of one entry are one.
    pub(crate) from: Vec<u16>,
    /// The folder of its new path, an index into the batch's folders.
    pub(crate) folder: usize,
    /// Its new name in that folder, as asked.
    pub(crate) name: Vec<u16>,
    pub(crate) is_folder: bool,
}

/// A folder that entries move into.
pub(crate) struct Folder {
    /// Its final path, folded, with the names of the folders below it that
    /// do not exist yet.
    pub(crate) key: Vec<u16>,
    /// Its names when prepare listed it; none for a folder that does not
    /// exist.
    pub(crate) names: Names,
}

/// The name an entry gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Name(Vec<u16>),
    /// Its name is in use, and the batch fails such an entry.
    InUse,
    /// A second spelling of an entry the batch already holds.
    Duplicate,
}

/// One rename of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The entry moves to its temporary name, so another entry can take its
    /// name.
    Temporary(usize),
    /// The entry moves to its new name.
    Final(usize),
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub(crate) decisions: Vec<Decision>,
    /// The temporary name of each entry that needs one, in its own folder.
    pub(crate) temporaries: Vec<Option<Vec<u16>>>,
    pub(crate) steps: Vec<Step>,
}

/// Decides each entry's name and the order of the renames. A name that is
/// in use is numbered, or fails with [`InUse::Fail`]; a name that another
/// entry of the batch gives up is free, as is the entry's own name in
/// another case.
pub(crate) fn plan(entries: &[Entry], folders: &[Folder], in_use: InUse) -> Plan {
    let mut holders: HashMap<&[u16], usize> = HashMap::new();
    let mut decisions: Vec<Decision> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| match holders.entry(&entry.from) {
            std::collections::hash_map::Entry::Occupied(_) => Decision::Duplicate,
            std::collections::hash_map::Entry::Vacant(free) => {
                free.insert(index);
                Decision::Name(entry.name.clone())
            }
        })
        .collect();
    let key = |entry: &Entry, name: &[u16]| {
        let mut key = folders[entry.folder].key.clone();
        key.push(BACKSLASH);
        key.extend(fold(name));
        key
    };
    let mut claimed: HashMap<Vec<u16>, usize> = HashMap::new();
    let listed = |entry: &Entry, name: &[u16]| folders[entry.folder].names.get(name).is_some();
    for (index, entry) in entries.iter().enumerate() {
        let Decision::Name(name) = &decisions[index] else {
            continue;
        };
        let wanted = key(entry, name);
        let free = |key: &Vec<u16>, name: &[u16], claimed: &HashMap<Vec<u16>, usize>| {
            !claimed.contains_key(key)
                && (holders.contains_key(key.as_slice()) || !listed(entry, name))
        };
        if free(&wanted, name, &claimed) {
            claimed.insert(wanted, index);
            continue;
        }
        decisions[index] = match in_use {
            InUse::Fail => Decision::InUse,
            InUse::Number => {
                let (name, key) = (2..)
                    .map(|number| {
                        let name = numbered_name(&entry.name, entry.is_folder, number);
                        let key = key(entry, &name);
                        (name, key)
                    })
                    .find(|(name, key)| free(key, name, &claimed))
                    .expect("a folder cannot hold every numbered name");
                claimed.insert(key, index);
                Decision::Name(name)
            }
        };
    }
    let mut order = Order::new(entries, &decisions, &holders, claimed);
    let mut temporaries: Vec<Option<Vec<u16>>> = vec![None; entries.len()];
    let mut used: HashMap<&[u16], u32> = HashMap::new();
    while let Some(stuck) = order.run() {
        match stuck {
            Stuck::Circle(index) => {
                let entry = &entries[index];
                let folder = parent(&entry.from);
                let names = folders.iter().find(|f| f.key == folder).map(|f| &f.names);
                let next = used.entry(folder).or_insert(0);
                let temporary = loop {
                    *next += 1;
                    let name: Vec<u16> = format!("~gozler-{next}").encode_utf16().collect();
                    let mut path = folder.to_vec();
                    path.push(BACKSLASH);
                    path.extend(fold(&name));
                    if !holders.contains_key(path.as_slice())
                        && names.is_none_or(|names| names.get(&name).is_none())
                    {
                        break name;
                    }
                };
                temporaries[index] = Some(temporary);
                order.moved_to_temporary(index);
            }
            Stuck::Blocked(index) => {
                let entry = &entries[index];
                decisions[index] = match in_use {
                    InUse::Fail => Decision::InUse,
                    InUse::Number => {
                        // The entry that holds the name cannot move first,
                        // so a name that nothing holds now is taken.
                        let (name, key) = (2..)
                            .map(|number| {
                                let name = numbered_name(&entry.name, entry.is_folder, number);
                                let key = key(entry, &name);
                                (name, key)
                            })
                            .find(|(name, key)| {
                                !order.claims(key)
                                    && !holders.contains_key(key.as_slice())
                                    && !listed(entry, name)
                            })
                            .expect("a folder cannot hold every numbered name");
                        order.claim(key, index);
                        Decision::Name(name)
                    }
                };
                order.unblock(index, matches!(decisions[index], Decision::InUse));
            }
        }
    }
    Plan {
        decisions,
        temporaries,
        steps: order.steps,
    }
}

/// The folder part of a folded path.
fn parent(path: &[u16]) -> &[u16] {
    let at = path
        .iter()
        .rposition(|&unit| unit == BACKSLASH)
        .unwrap_or(0);
    &path[..at]
}

/// Why no entry can be renamed now.
enum Stuck {
    /// The entry is in a circle of entries that wait for each other's names
    /// and has nothing below it to rename first; it moves to a temporary
    /// name.
    Circle(usize),
    /// The entry waits for a name that an entry holds which cannot move
    /// first; it gets another name.
    Blocked(usize),
}

/// The run order, found as a topological order of the waits: an entry
/// waits for the entry that holds its new name (`blocker`) and for the
/// entries of the batch below it (`below`).
struct Order {
    blocker: Vec<Option<usize>>,
    waiters: Vec<Vec<usize>>,
    below: Vec<usize>,
    above: Vec<Vec<usize>>,
    /// Entries that hold a temporary name.
    at_temporary: Vec<bool>,
    /// Entries that are ordered, or that the batch does not rename.
    finished: Vec<bool>,
    pending: usize,
    ready: VecDeque<usize>,
    claimed: HashMap<Vec<u16>, usize>,
    steps: Vec<Step>,
}

impl Order {
    fn new(
        entries: &[Entry],
        decisions: &[Decision],
        holders: &HashMap<&[u16], usize>,
        claimed: HashMap<Vec<u16>, usize>,
    ) -> Order {
        let count = entries.len();
        let mut order = Order {
            blocker: vec![None; count],
            waiters: vec![Vec::new(); count],
            below: vec![0; count],
            above: vec![Vec::new(); count],
            at_temporary: vec![false; count],
            finished: decisions
                .iter()
                .map(|decision| !matches!(decision, Decision::Name(_)))
                .collect(),
            pending: 0,
            ready: VecDeque::new(),
            claimed: HashMap::new(),
            steps: Vec::new(),
        };
        for (key, &index) in &claimed {
            if let Some(&holder) = holders.get(key.as_slice())
                && holder != index
                && matches!(decisions[holder], Decision::Name(_))
            {
                order.blocker[index] = Some(holder);
                order.waiters[holder].push(index);
            }
        }
        for (index, entry) in entries.iter().enumerate() {
            if !matches!(decisions[index], Decision::Name(_)) {
                continue;
            }
            order.pending += 1;
            let mut path: &[u16] = &entry.from;
            while let Some(at) = path.iter().rposition(|&unit| unit == BACKSLASH) {
                path = &path[..at];
                if let Some(&folder) = holders.get(path)
                    && matches!(decisions[folder], Decision::Name(_))
                {
                    order.below[folder] += 1;
                    order.above[index].push(folder);
                }
            }
        }
        for index in 0..count {
            if !order.finished[index] {
                order.release(index);
            }
        }
        order.claimed = claimed;
        order
    }

    /// Queues `index` once nothing holds it back.
    fn release(&mut self, index: usize) {
        if self.blocker[index].is_none() && self.below[index] == 0 {
            self.ready.push_back(index);
        }
    }

    /// Adds the renames that can run, in order, and says why the rest
    /// cannot; `None` once every entry is ordered.
    fn run(&mut self) -> Option<Stuck> {
        while let Some(index) = self.ready.pop_front() {
            if !self.finished[index] {
                self.steps.push(Step::Final(index));
                self.done(index);
            }
        }
        if self.pending == 0 {
            return None;
        }
        let waiting = |index: usize| self.below[index] == 0 && !self.finished[index];
        let circle = (0..self.blocker.len()).find(|&index| {
            waiting(index)
                && !self.at_temporary[index]
                && self.waiters[index]
                    .iter()
                    .any(|&waiter| self.blocker[waiter] == Some(index))
        });
        Some(match circle {
            Some(index) => Stuck::Circle(index),
            None => Stuck::Blocked(
                (0..self.blocker.len())
                    .find(|&index| waiting(index) && self.blocker[index].is_some())
                    .expect("an entry with nothing below it waits for a name"),
            ),
        })
    }

    /// `index` has its new name, or has failed: what waited for it goes on.
    fn done(&mut self, index: usize) {
        self.finished[index] = true;
        self.pending -= 1;
        for waiter in std::mem::take(&mut self.waiters[index]) {
            if self.blocker[waiter] == Some(index) {
                self.blocker[waiter] = None;
                self.release(waiter);
            }
        }
        for folder in std::mem::take(&mut self.above[index]) {
            self.below[folder] -= 1;
            self.release(folder);
        }
    }

    fn moved_to_temporary(&mut self, index: usize) {
        self.steps.push(Step::Temporary(index));
        self.at_temporary[index] = true;
        for waiter in std::mem::take(&mut self.waiters[index]) {
            if self.blocker[waiter] == Some(index) {
                self.blocker[waiter] = None;
                self.release(waiter);
            }
        }
    }

    fn claims(&self, key: &[u16]) -> bool {
        self.claimed.contains_key(key)
    }

    fn claim(&mut self, key: Vec<u16>, index: usize) {
        self.claimed.insert(key, index);
    }

    /// `index` no longer waits for the name it asked: it has another, or
    /// it failed.
    fn unblock(&mut self, index: usize, failed: bool) {
        self.blocker[index] = None;
        match failed {
            true => self.done(index),
            false => self.release(index),
        }
    }
}
