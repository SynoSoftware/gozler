//! A batch rename's disk work. Prepare reads each entry without following
//! links (D12) and lists each new path's folder once, and `super::plan`
//! decides the names and the order; the run renames without replacement, so
//! a name that another program took since prepare is numbered, or fails
//! Undo's entry, and is never replaced.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gozler_winfs::{Access, Existing, Reparse, create_folder, open_entry, path_attributes, rename};

use super::{BACKSLASH, Decision, Entry, Folder, InUse, Step, plan};
use crate::conflicts::Run;
use crate::copy::is_folder_path;
use crate::identity::{Identity, PathTrie};
use crate::listing::{LISTING_WORDS, join};
use crate::names::{Holder, Names, fold, numbered_name};
use crate::operation::{
    Found, Listed, Pause, Prepared, Progress, Reason, Refusal, Rename, Renamed, Request, Total,
};
use crate::prepare::{Preparation, Ready, Work};
use crate::receive::{failure, open_destination, plain, verbatim};
use crate::win32::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND,
    ERROR_PATH_NOT_FOUND, FILE_ATTRIBUTE_DIRECTORY,
};

/// `ERROR_NOT_SAME_DEVICE`: a new path on another volume.
const ERROR_NOT_SAME_DEVICE: u32 = 17;
/// `ERROR_INVALID_NAME`: a new name that names no entry, or one above the
/// root.
const ERROR_INVALID_NAME: u32 = 123;
/// How many names the run tries for one entry whose names others keep
/// taking.
const NAME_TRIES: u32 = 1_000;

/// A prepared batch rename.
pub(crate) struct Batch {
    entries: Vec<Moving>,
    folders: Vec<Target>,
    steps: Vec<Step>,
    in_use: InUse,
    /// The entries that Continue found at their new paths.
    done: Vec<Moved>,
}

/// An entry the run renames.
struct Moving {
    /// Its index in the request.
    entry: usize,
    /// As the window gave it.
    path: Vec<u16>,
    /// Its path with `\\?\`, whose name it takes back when its rename
    /// fails at a temporary name.
    original: Vec<u16>,
    /// Where it is: its path, or its temporary name.
    at: Vec<u16>,
    at_temporary: bool,
    /// The path of its temporary name, when the plan moves it there.
    temporary: Option<Vec<u16>>,
    folder: usize,
    /// The name it asked for, which a name taken since prepare is numbered
    /// from, and the name the plan gave it.
    asked: Vec<u16>,
    name: Vec<u16>,
    is_folder: bool,
    failed: bool,
}

/// A folder that entries move into.
struct Target {
    /// The deepest folder of its path that exists, with `\\?\`.
    opened: Vec<u16>,
    /// The names of the folders below `opened` that do not exist yet, the
    /// top one first, which the run creates before the first entry moves in.
    missing: Vec<Vec<u16>>,
    /// The folder, with `\\?\`.
    path: Vec<u16>,
    /// Its names, with the names the batch gives, for a number the run
    /// picks itself.
    names: Names,
}

/// A rename the run made, or that Continue found made, as `\\?\` paths.
struct Moved {
    entry: usize,
    from: Vec<u16>,
    to: Vec<u16>,
}

/// Where an entry of a continued batch is.
enum Located {
    At { path: Vec<u16>, temporary: bool },
    Done(Vec<u16>),
}

pub(crate) fn prepare(
    asked: &[Rename],
    in_use: InUse,
    preparing: &Preparation<'_>,
) -> Result<Ready, Refusal> {
    let continuing = matches!(preparing.rule.run, Run::Continue { .. });
    let mut failures = Vec::new();
    let mut done = Vec::new();
    let mut planned = Vec::new();
    let mut entries = Vec::new();
    // Parallel to `entries`: the asked rename and the entry's identity.
    let mut read = Vec::new();
    let mut targets = Vec::new();
    let mut folders = Vec::new();
    // Parallel to `targets`: each folder's volume, folded.
    let mut volumes: Vec<Vec<u16>> = Vec::new();
    let mut by_path: HashMap<Vec<u16>, Result<usize, u32>> = HashMap::new();
    let mut page = vec![0u64; LISTING_WORDS];
    for (index, rename) in asked.iter().enumerate() {
        if let Some(pause) = preparing.pause {
            pause.wait(preparing.cancel);
        }
        if preparing.cancel.load(Ordering::Relaxed) {
            return Err(Refusal::SourceUnavailable);
        }
        let path: Vec<u16> = rename.path.as_os_str().encode_wide().collect();
        let name: Vec<u16> = rename.name.as_os_str().encode_wide().collect();
        let Some((folder_path, new_name)) = new_path(&path, &name) else {
            failures.push(failure(&path, ERROR_INVALID_NAME));
            continue;
        };
        let original = verbatim(&path);
        let (at, at_temporary) = match (&rename.found, continuing) {
            (Some(found), true) => {
                let target = join(&verbatim(&folder_path), &new_name);
                match locate(&original, found, &target) {
                    Some(Located::At {
                        path: at,
                        temporary,
                    }) => (at, temporary),
                    Some(Located::Done(to)) => {
                        done.push(Moved {
                            entry: index,
                            from: original,
                            to,
                        });
                        continue;
                    }
                    None => {
                        failures.push(failure(&path, ERROR_FILE_NOT_FOUND));
                        continue;
                    }
                }
            }
            _ => (original.clone(), false),
        };
        let opened = open_entry(&at, Access::Inspect, Reparse::Open).and_then(|entry| {
            Ok((
                entry.final_path()?,
                entry.identity()?,
                entry.attribute_tag()?,
            ))
        });
        let (identity, file, tag) = match opened {
            Ok((identity, file, tag)) => (Identity::new(identity), file, tag),
            Err(error) => {
                failures.push(failure(&path, error.code));
                continue;
            }
        };
        let folder = *by_path.entry(fold(&folder_path)).or_insert_with(|| {
            let (target, folder, volume) = open_folder(&folder_path, &mut page, preparing)?;
            targets.push(target);
            folders.push(folder);
            volumes.push(volume);
            Ok(targets.len() - 1)
        });
        let folder = match folder {
            Ok(folder) if fold(identity.volume()) == volumes[folder] => folder,
            Ok(_) => {
                failures.push(failure(&path, ERROR_NOT_SAME_DEVICE));
                continue;
            }
            Err(code) => {
                failures.push(failure(&path, code));
                continue;
            }
        };
        let is_folder = tag.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        planned.push(Entry {
            from: fold(identity.units()),
            folder,
            name: new_name.clone(),
            is_folder,
        });
        entries.push(Moving {
            entry: index,
            path,
            original,
            at,
            at_temporary,
            temporary: None,
            folder,
            asked: new_name.clone(),
            name: new_name,
            is_folder,
            failed: false,
        });
        read.push((rename, file));
    }
    let plan = plan(&planned, &folders, in_use);
    for (target, folder) in targets.iter_mut().zip(folders) {
        target.names = folder.names;
    }
    let mut resolved = Vec::new();
    let mut renaming = 0;
    for (index, decision) in plan.decisions.into_iter().enumerate() {
        let entry = &mut entries[index];
        match decision {
            Decision::Name(name) => {
                let (rename, file) = &read[index];
                let temporary = &plan.temporaries[index];
                entry.temporary = (temporary.as_ref()).map(|name| sibling(&entry.original, name));
                // An entry that Continue found at a temporary name keeps it
                // on record until it moves on.
                let kept = (rename.found.as_ref()).and_then(|found| found.temporary.clone());
                resolved.push(Rename {
                    path: rename.path.clone(),
                    name: renamed_to(&rename.name, &name),
                    found: Some(Found {
                        volume: file.volume_serial,
                        file: file.file_id,
                        temporary: (temporary.as_deref()).map(OsString::from_wide).or(kept),
                    }),
                });
                targets[entry.folder].names.insert(&name, Holder::Ours);
                entry.name = name;
                renaming += 1;
            }
            Decision::InUse => {
                entry.failed = true;
                failures.push(failure(&entry.path, ERROR_ALREADY_EXISTS));
            }
            Decision::Duplicate => entry.failed = true,
        }
    }
    let items = (renaming + done.len() + failures.len()) as u64;
    Ok(Ready {
        prepared: Prepared {
            entries: renaming + done.len(),
            total: Some(Total { items, bytes: 0 }),
        },
        work: vec![(
            crate::waits::Plan::default(),
            Work::Renames(Box::new(Batch {
                entries,
                folders: targets,
                steps: plan.steps,
                in_use,
                done,
            })),
        )],
        failures,
        resolved: Some(Request::Rename {
            entries: resolved,
            in_use,
        }),
    })
}

/// Opens and lists the folder `path` of new paths, following links as a
/// copy's destination does. Returns it with its volume, folded.
fn open_folder(
    path: &[u16],
    page: &mut [u64],
    preparing: &Preparation<'_>,
) -> Result<(Target, Folder, Vec<u16>), u32> {
    let opened = open_destination(
        Path::new(&OsString::from_wide(path)),
        &PathTrie::default(),
        preparing.cancel,
        preparing.pause,
    );
    let destination = match opened {
        Ok(destination) => destination,
        Err(Refusal::DestinationUnreadable(code)) => return Err(code),
        Err(_) => return Err(ERROR_PATH_NOT_FOUND),
    };
    // A folder that cannot be listed holds names prepare does not know; the
    // run numbers a new name that meets one of them.
    let names = destination
        .names(page, || preparing.cancel.load(Ordering::Relaxed))
        .unwrap_or_default();
    let path = (destination.missing.iter())
        .fold(destination.verbatim.clone(), |path, name| join(&path, name));
    let volume = fold(destination.identity.volume());
    Ok((
        Target {
            opened: destination.verbatim,
            missing: destination.missing,
            path,
            names: Names::default(),
        },
        Folder {
            key: fold(destination.identity.units()),
            names,
        },
        volume,
    ))
}

/// The folder and the name of `path`'s new path: `name` read from the
/// folder that holds `path`, `..` going up a folder. `None` for a name that
/// names no entry, or goes above the root.
fn new_path(path: &[u16], name: &[u16]) -> Option<(Vec<u16>, Vec<u16>)> {
    const DOT: u16 = b'.' as u16;
    let cut = path.iter().rposition(|&unit| unit == BACKSLASH)?;
    let mut folder = path[..cut].to_vec();
    let root = root_length(&folder);
    let parts: Vec<&[u16]> = name.split(|&unit| unit == BACKSLASH).collect();
    let (last, above) = parts.split_last()?;
    for part in above {
        match *part {
            [] => return None,
            [DOT, DOT] => {
                if folder.len() <= root {
                    return None;
                }
                let at = folder.iter().rposition(|&unit| unit == BACKSLASH)?;
                folder.truncate(at);
            }
            part => {
                folder.push(BACKSLASH);
                folder.extend_from_slice(part);
            }
        }
    }
    if matches!(*last, [] | [DOT] | [DOT, DOT]) {
        return None;
    }
    // A drive's root opens as a folder only with its backslash.
    if folder.len() == root && !folder.starts_with(&[BACKSLASH, BACKSLASH]) {
        folder.push(BACKSLASH);
    }
    Some((folder, last.to_vec()))
}

/// The length of a path's root: `C:`, or `\\server\share`.
fn root_length(path: &[u16]) -> usize {
    let separators = if path.starts_with(&[BACKSLASH, BACKSLASH]) {
        4
    } else {
        1
    };
    (path.iter().enumerate())
        .filter(|&(_, &unit)| unit == BACKSLASH)
        .nth(separators - 1)
        .map_or(path.len(), |(at, _)| at)
}

/// `name` in the folder that holds `path`.
fn sibling(path: &[u16], name: &[u16]) -> Vec<u16> {
    join(split_name(path).0, name)
}

/// A path's folder and its name.
fn split_name(path: &[u16]) -> (&[u16], &[u16]) {
    match path.iter().rposition(|&unit| unit == BACKSLASH) {
        Some(cut) => (&path[..cut], &path[cut + 1..]),
        None => (&[], path),
    }
}

/// The asked relative name with its last name replaced by `name`.
fn renamed_to(asked: &Path, name: &[u16]) -> PathBuf {
    let name = OsString::from_wide(name);
    match asked
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Whether `path` names the entry that prepare read as `found`.
fn holds(path: &[u16], found: &Found) -> bool {
    open_entry(path, Access::Inspect, Reparse::Open)
        .and_then(|entry| entry.identity())
        .is_ok_and(|file| file.volume_serial == found.volume && file.file_id == found.file)
}

/// Where a continued entry is, by its identity: at its path, at its
/// temporary name, or at its new path `target`. `None` when it is at none
/// of them.
fn locate(original: &[u16], found: &Found, target: &[u16]) -> Option<Located> {
    if holds(original, found) {
        return Some(Located::At {
            path: original.to_vec(),
            temporary: false,
        });
    }
    if let Some(temporary) = &found.temporary {
        let path = sibling(original, &temporary.encode_wide().collect::<Vec<_>>());
        if holds(&path, found) {
            return Some(Located::At {
                path,
                temporary: true,
            });
        }
    }
    holds(target, found).then(|| Located::Done(target.to_vec()))
}

/// Renames the batch's entries in the planned order, and returns what it
/// lists and what it renamed, in the order they happened. A cancel takes
/// effect between two renames once no entry holds a temporary name.
pub(crate) fn run(
    batch: Batch,
    cancel: &AtomicBool,
    pause: &Pause,
    progress: &Progress,
) -> (Vec<Listed>, Vec<Renamed>) {
    let Batch {
        mut entries,
        mut folders,
        steps,
        in_use,
        done,
    } = batch;
    let _active = pause.enter(cancel);
    let mut record = Record::default();
    for moved in done {
        record.add(moved);
        progress.add_items(1);
    }
    let mut listed = Vec::new();
    let mut circling = entries.iter().filter(|entry| entry.at_temporary).count();
    for step in steps {
        pause.wait(cancel);
        if circling == 0 && cancel.load(Ordering::Relaxed) {
            break;
        }
        match step {
            Step::Temporary(index) => {
                let entry = &mut entries[index];
                let temporary = (entry.temporary.take())
                    .expect("the plan names a temporary for each temporary step");
                match rename(&entry.at, &temporary, Existing::Refuse) {
                    Ok(()) => {
                        record.moved(&entry.at, &temporary, entry.is_folder);
                        entry.at = temporary;
                        entry.at_temporary = true;
                        circling += 1;
                    }
                    Err(error) => {
                        entry.failed = true;
                        listed.push(failure(&entry.path, error.code));
                        progress.add_items(1);
                    }
                }
            }
            Step::Final(index) => {
                let entry = &mut entries[index];
                if entry.failed {
                    continue;
                }
                let folder = &mut folders[entry.folder];
                let renamed = folder.create().and_then(|()| {
                    let names = &mut folder.names;
                    let next = || {
                        (in_use == InUse::Number)
                            .then(|| names.keep_both(&entry.asked, entry.is_folder))
                    };
                    rename_free(&entry.at, &folder.path, entry.name.clone(), next)
                });
                match renamed {
                    Ok(to) => {
                        record.moved(&entry.at, &to, entry.is_folder);
                        record.add(Moved {
                            entry: entry.entry,
                            from: entry.original.clone(),
                            to,
                        });
                    }
                    Err(code) => {
                        let mut failed = failure(&entry.path, code);
                        // No entry keeps a temporary name.
                        if entry.at_temporary {
                            match rename_back(&entry.at, &entry.original, entry.is_folder) {
                                Ok(back) => {
                                    record.moved(&entry.at, &back, entry.is_folder);
                                    if back != entry.original {
                                        record.add(Moved {
                                            entry: entry.entry,
                                            from: entry.original.clone(),
                                            to: back.clone(),
                                        });
                                    }
                                    entry.at = back;
                                }
                                Err(code) => {
                                    failed = Listed {
                                        path: plain(&entry.at),
                                        reason: Reason::Failed(code),
                                    }
                                }
                            }
                        }
                        listed.push(failed);
                    }
                }
                if entry.at_temporary {
                    entry.at_temporary = false;
                    circling -= 1;
                }
                progress.add_items(1);
            }
        }
    }
    (listed, record.renamed())
}

/// Renames the entry at `at` back to the name of `original`, or, when
/// another entry took that name, as a swap's does, to the first free
/// numbered one. Returns the path it got.
fn rename_back(at: &[u16], original: &[u16], is_folder: bool) -> Result<Vec<u16>, u32> {
    let (folder, name) = split_name(original);
    let mut number = 1;
    let next = || {
        number += 1;
        Some(numbered_name(name, is_folder, number))
    };
    rename_free(at, folder, name.to_vec(), next)
}

/// Renames `from` to `name` in `folder`, and, while the new path is taken,
/// to each name that `next` gives instead; `next` gives `None` to fail.
/// Returns the path it got.
fn rename_free(
    from: &[u16],
    folder: &[u16],
    mut name: Vec<u16>,
    mut next: impl FnMut() -> Option<Vec<u16>>,
) -> Result<Vec<u16>, u32> {
    for _ in 0..NAME_TRIES {
        let to = join(folder, &name);
        let code = match rename(from, &to, Existing::Refuse) {
            Ok(()) => return Ok(to),
            Err(error) => error.code,
        };
        // A hidden or read-only entry in the way refuses as access denied.
        let taken = matches!(code, ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS)
            || (code == ERROR_ACCESS_DENIED && matches!(path_attributes(&to), Ok(Some(_))));
        if !taken {
            return Err(code);
        }
        match next() {
            Some(next) => name = next,
            None => return Err(code),
        }
    }
    Err(ERROR_ALREADY_EXISTS)
}

impl Target {
    /// Creates the folders of its path that do not exist yet.
    fn create(&mut self) -> Result<(), u32> {
        while let Some(name) = self.missing.first() {
            let path = join(&self.opened, name);
            match create_folder(&path, None) {
                Ok(()) => {}
                Err(error) if error.code == ERROR_ALREADY_EXISTS && is_folder_path(&path) => {}
                Err(error) => return Err(error.code),
            }
            self.opened = path;
            self.missing.remove(0);
        }
        Ok(())
    }
}

/// The renames of a run. A folder that moves moves each recorded path below
/// it, so the result names every path where the batch left it, and renaming
/// each `to` back to its `from` undoes the batch.
#[derive(Default)]
struct Record {
    moved: Vec<Moved>,
    /// Each recorded path, folded, with its rename's index and whether it is
    /// the `to`.
    by_path: BTreeSet<(Vec<u16>, usize, bool)>,
}

impl Record {
    fn add(&mut self, moved: Moved) {
        let index = self.moved.len();
        self.by_path.insert((fold(&moved.from), index, false));
        self.by_path.insert((fold(&moved.to), index, true));
        self.moved.push(moved);
    }

    /// An entry moved from `from` to `to`; for a folder, every recorded path
    /// below it moves with it.
    fn moved(&mut self, from: &[u16], to: &[u16], is_folder: bool) {
        if !is_folder {
            return;
        }
        let mut prefix = fold(from);
        prefix.push(BACKSLASH);
        let below: Vec<_> = (self.by_path.range((prefix.clone(), 0, false)..))
            .take_while(|(path, ..)| path.starts_with(&prefix))
            .cloned()
            .collect();
        for key in below {
            self.by_path.remove(&key);
            let (_, index, is_to) = key;
            let moved = &mut self.moved[index];
            let path = if is_to {
                &mut moved.to
            } else {
                &mut moved.from
            };
            // Folding keeps each unit in place, so the folder part of the
            // path is as long as `from`.
            *path = [to, &path[from.len()..]].concat();
            self.by_path.insert((fold(path), index, is_to));
        }
    }

    fn renamed(self) -> Vec<Renamed> {
        (self.moved.into_iter())
            .map(|moved| Renamed {
                entry: moved.entry,
                from: plain(&moved.from),
                to: plain(&moved.to),
            })
            .collect()
    }
}

/// Dismiss of an interrupted batch: each entry at its temporary name gets
/// its old name back, numbered when another entry took it, and nothing is
/// replaced. Returns what failed.
pub(crate) fn restore_temporaries(entries: &[Rename]) -> Vec<Listed> {
    let mut listed = Vec::new();
    for entry in entries {
        let Some(found) = &entry.found else {
            continue;
        };
        let Some(temporary) = &found.temporary else {
            continue;
        };
        let path: Vec<u16> = entry.path.as_os_str().encode_wide().collect();
        let original = verbatim(&path);
        let at = sibling(&original, &temporary.encode_wide().collect::<Vec<_>>());
        if !holds(&at, found) {
            continue;
        }
        if let Err(code) = rename_back(&at, &original, is_folder_path(&at)) {
            listed.push(Listed {
                path: plain(&at),
                reason: Reason::Failed(code),
            });
        }
    }
    listed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `..` goes up from the entry's folder but never above its root, and a
    /// drive's root keeps the backslash it opens with.
    #[test]
    fn a_new_path_is_read_from_the_entry_folder() {
        let new = |path: &str, name: &str| {
            let units = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
            new_path(&units(path), &units(name)).map(|(folder, name)| {
                (
                    String::from_utf16_lossy(&folder),
                    String::from_utf16_lossy(&name),
                )
            })
        };
        let pair = |folder: &str, name: &str| Some((folder.to_owned(), name.to_owned()));
        assert_eq!(new(r"C:\a\b.txt", r"x\c.txt"), pair(r"C:\a\x", "c.txt"));
        assert_eq!(new(r"C:\a\b.txt", r"..\c.txt"), pair(r"C:\", "c.txt"));
        assert_eq!(new(r"C:\b.txt", r"..\c.txt"), None);
        assert_eq!(new(r"\\s\share\a\b", r"..\c"), pair(r"\\s\share", "c"));
        assert_eq!(new(r"\\s\share\b", r"..\c"), None);
        assert_eq!(new(r"C:\a\b.txt", r"x\\c"), None);
    }
}
