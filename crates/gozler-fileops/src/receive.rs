//! What an operation receives: the selected entries, each read from the
//! disk with its identity (D12), and the destination.

use std::cell::Cell;
use std::collections::HashMap;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gozler_winfs::{Access, AttributeTag, Entry, EntryStat, Reparse, open_entry};

use crate::identity::{Identity, PathTrie, outermost};
use crate::listing::{BACKSLASH, LISTING_WORDS, list, read_names};
use crate::names::Names;
use crate::operation::{Listed, Pause, Reason, Refusal};
use crate::win32::{FILE_ATTRIBUTE_DIRECTORY, is_gone};

/// Windows' "The directory name is invalid", for a destination that is a
/// file.
const ERROR_DIRECTORY: u32 = 267;

/// From this many selected entries in one folder, they are read from one
/// listing of the folder instead of one open each, which takes about 0.2 ms:
/// 20 s for 100,000 files (#138).
const LISTED_FROM: usize = 16;

/// The listing stops after this many entries for each name it looks for, so
/// a few names in a large folder cost no more than their opens.
const LISTED_PER_NAME: usize = 32;

/// A selected entry that exists and whose identity was read.
#[derive(Debug)]
pub(crate) struct Selected {
    /// The path as the window gave it, for the result.
    pub(crate) path: Vec<u16>,
    /// The path to open: with the `\\?\` or `\\?\UNC\` prefix.
    pub(crate) verbatim: Vec<u16>,
    pub(crate) identity: Identity,
    /// What it was when it was received, for prepare; the walks read what
    /// it is again when they reach it.
    pub(crate) tag: AttributeTag,
    pub(crate) stat: EntryStat,
}

impl Selected {
    pub(crate) fn folder(&self) -> bool {
        self.tag.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// The folder a copy or a move writes into, opened following links: the
/// destination, or the nearest folder above it that exists, below which the
/// run creates the missing folders.
pub(crate) struct Destination {
    /// The folder that opened.
    pub(crate) verbatim: Vec<u16>,
    /// The destination's identity: the target's when the folder that opened
    /// is a link, with the missing folders' names.
    pub(crate) identity: Identity,
    /// The folder that opened, whose volume prepare checks.
    pub(crate) entry: Entry,
    /// The names of the folders that do not exist yet, the top one first.
    pub(crate) missing: Vec<Vec<u16>>,
}

impl Destination {
    /// Its names now; none while it does not exist. Each call lists it
    /// through a handle of its own, because a handle's listing goes on where
    /// its last read stopped, so a second listing of one handle would find
    /// the folder empty.
    pub(crate) fn names(&self, page: &mut [u64], stopped: impl Fn() -> bool) -> Result<Names, u32> {
        if !self.missing.is_empty() {
            return Ok(Names::default());
        }
        let folder = self
            .entry
            .reopen(Access::List)
            .map_err(|error| error.code)?;
        read_names(&folder, page, stopped)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Received {
    /// The outermost selected entries, in selection order.
    pub(crate) entries: Vec<Selected>,
    /// Selected entries that no longer exist, as the window gave them.
    pub(crate) missing: Vec<Vec<u16>>,
    /// Selected entries whose identity cannot be read. The operation does
    /// not touch them.
    pub(crate) failures: Vec<Listed>,
}

/// Reads each selected entry and the destination. An entry below another
/// selected folder, or a second spelling of one, is dropped; a destination
/// inside a selected folder is refused, as File Explorer refuses it. Stops
/// early, with what it has read, when `cancel` is set.
pub(crate) fn receive(
    sources: &[PathBuf],
    destination: Option<&Path>,
    cancel: &AtomicBool,
    pause: Option<&Pause>,
) -> Result<(Received, Option<Destination>), Refusal> {
    let mut received = Received::default();
    let paths: Vec<Vec<u16>> = (sources.iter())
        .map(|source| source.as_os_str().encode_wide().collect())
        .collect();
    let mut listed = read_listed(&paths, cancel, pause);
    let mut selected = Vec::new();
    for (path, from_listing) in paths.into_iter().zip(&mut listed) {
        if let Some(pause) = pause {
            pause.wait(cancel);
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(entry) = from_listing.take() {
            selected.push(entry);
            continue;
        }
        let verbatim = verbatim(&path);
        let read = open_entry(&verbatim, Access::Inspect, Reparse::Open)
            .and_then(|entry| Ok((entry.final_path()?, entry.attribute_tag()?, entry.stat()?)));
        match read {
            Ok((identity, tag, stat)) => selected.push(Selected {
                path,
                verbatim,
                identity: Identity::new(identity),
                tag,
                stat,
            }),
            Err(error) if is_gone(error.code) => received.missing.push(path),
            Err(error) => received.failures.push(failure(&path, error.code)),
        }
    }
    let identities: Vec<Identity> = selected.iter().map(|s| s.identity.clone()).collect();
    let (kept, trie) = outermost(&identities);
    let destination = match destination {
        Some(destination) => Some(open_destination(destination, &trie, cancel, pause)?),
        None => None,
    };
    let mut selected: Vec<Option<Selected>> = selected.into_iter().map(Some).collect();
    received.entries = kept
        .into_iter()
        .map(|i| selected[i].take().expect("each kept index is distinct"))
        .collect();
    Ok((received, destination))
}

/// The selected entries that a listing of their folder names exactly, read
/// from it, for each folder that holds at least [`LISTED_FROM`] of them:
/// the identity is the folder's final path and the listed name, which is
/// the entry's final path. An entry the listing does not name exactly, such
/// as one given by its short name, stays `None`, for its own open.
pub(crate) fn read_listed(
    paths: &[Vec<u16>],
    cancel: &AtomicBool,
    pause: Option<&Pause>,
) -> Vec<Option<Selected>> {
    let mut read: Vec<Option<Selected>> = paths.iter().map(|_| None).collect();
    let mut folders: HashMap<&[u16], HashMap<&[u16], Vec<usize>>> = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        if let Some(at) = path.iter().rposition(|&unit| unit == BACKSLASH)
            && at + 1 < path.len()
        {
            let names = folders.entry(&path[..at]).or_default();
            names.entry(&path[at + 1..]).or_default().push(index);
        }
    }
    let mut page = vec![0u64; LISTING_WORDS];
    for (folder, mut names) in folders {
        if names.values().map(Vec::len).sum::<usize>() < LISTED_FROM {
            continue;
        }
        if let Some(pause) = pause {
            pause.wait(cancel);
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut path = verbatim(folder);
        // A root opens as a folder only with its backslash.
        if is_volume_root(&path) {
            path.push(BACKSLASH);
        }
        let Ok(entry) = open_entry(&path, Access::List, Reparse::Follow) else {
            continue;
        };
        let Ok(identity) = entry.final_path().map(Identity::new) else {
            continue;
        };
        let budget = names.len() * LISTED_PER_NAME;
        let (seen, left) = (Cell::new(0), Cell::new(names.len()));
        // A folder of a million entries takes seconds, so Pause holds it
        // between pages.
        let stopped = || {
            if let Some(pause) = pause {
                pause.wait(cancel);
            }
            cancel.load(Ordering::Relaxed) || left.get() == 0 || seen.get() > budget
        };
        // A folder that cannot be listed leaves its entries to their opens.
        let _ = list(&entry, &mut page, stopped, |listed| {
            seen.set(seen.get() + 1);
            let Ok(listed) = listed else {
                return;
            };
            let Some(indices) = names.remove(listed.name.as_slice()) else {
                return;
            };
            left.set(left.get() - 1);
            for index in indices {
                read[index] = Some(Selected {
                    path: paths[index].clone(),
                    verbatim: verbatim(&paths[index]),
                    identity: identity.child(&listed.name),
                    tag: AttributeTag {
                        attributes: listed.attributes,
                        reparse_tag: listed.reparse_tag.unwrap_or(0),
                    },
                    stat: EntryStat {
                        size: listed.size,
                        created: listed.created,
                        last_write: listed.last_write,
                    },
                });
            }
        });
    }
    read
}

/// The destination is a folder the person chose, so it is opened following
/// links, and its identity is its target's. A folder that does not exist is
/// opened at the nearest folder above it that exists; any other error
/// refuses, and so does a file in its place.
pub(crate) fn open_destination(
    destination: &Path,
    sources: &PathTrie,
    cancel: &AtomicBool,
    pause: Option<&Pause>,
) -> Result<Destination, Refusal> {
    let path: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let mut verbatim = verbatim(&path);
    while verbatim.last() == Some(&BACKSLASH) && !is_volume_root(&verbatim) {
        verbatim.pop();
    }
    let mut missing = Vec::new();
    let entry = loop {
        if let Some(pause) = pause {
            pause.wait(cancel);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Refusal::SourceUnavailable);
        }
        match open_entry(&verbatim, Access::List, Reparse::Follow) {
            Ok(entry) => break entry,
            Err(error) if is_gone(error.code) => {
                let parent = (verbatim.iter().rposition(|&unit| unit == BACKSLASH))
                    .filter(|_| !is_volume_root(&verbatim));
                let Some(at) = parent else {
                    return Err(Refusal::DestinationMissing);
                };
                missing.push(verbatim.split_off(at)[1..].to_vec());
                // A root opens as a folder only with its backslash.
                if is_volume_root(&verbatim) {
                    verbatim.push(BACKSLASH);
                }
            }
            Err(error) => return Err(Refusal::DestinationUnreadable(error.code)),
        }
    };
    missing.reverse();
    let tag = entry
        .attribute_tag()
        .map_err(|error| Refusal::DestinationUnreadable(error.code))?;
    if tag.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(Refusal::DestinationUnreadable(ERROR_DIRECTORY));
    }
    let mut identity = Identity::new(
        (entry.final_path()).map_err(|error| Refusal::DestinationUnreadable(error.code))?,
    );
    for name in &missing {
        identity = identity.child(name);
    }
    if sources.covers(&identity) {
        return Err(Refusal::DestinationInsideSource);
    }
    Ok(Destination {
        verbatim,
        identity,
        entry,
        missing,
    })
}

/// Whether the verbatim path is a volume's root, which has no folder above
/// it: `\\?\C:\`, `\\?\UNC\server\share`, or `\\?\Volume{…}\`.
fn is_volume_root(verbatim: &[u16]) -> bool {
    let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let trimmed = verbatim.strip_suffix(&[BACKSLASH]).unwrap_or(verbatim);
    if let Some(share) = trimmed.strip_prefix(&units(r"\\?\UNC\")[..]) {
        return share.iter().filter(|&&unit| unit == BACKSLASH).count() <= 1;
    }
    let rest = trimmed.strip_prefix(&units(r"\\?\")[..]).unwrap_or(trimmed);
    !rest.contains(&BACKSLASH)
}

/// `path` with the prefix that lets Windows take it longer than `MAX_PATH`.
/// The prefix also turns off Windows' rewriting of the path, so a name that
/// ends with a dot or a space reaches the file system as it is; a path that
/// is not a full path then fails to open, and the entry is a failure.
pub(crate) fn verbatim(path: &[u16]) -> Vec<u16> {
    let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    if path.starts_with(&units(r"\\?\")) {
        path.to_vec()
    } else if let Some(share) = path.strip_prefix(&units(r"\\")[..]) {
        units(r"\\?\UNC\")
            .into_iter()
            .chain(share.iter().copied())
            .collect()
    } else {
        units(r"\\?\")
            .into_iter()
            .chain(path.iter().copied())
            .collect()
    }
}

/// A path that [`verbatim`] made, as the person writes it.
pub(crate) fn plain(path: &[u16]) -> PathBuf {
    let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let plain = if let Some(share) = path.strip_prefix(&units(r"\\?\UNC\")[..]) {
        units(r"\\")
            .into_iter()
            .chain(share.iter().copied())
            .collect()
    } else {
        path.strip_prefix(&units(r"\\?\")[..])
            .unwrap_or(path)
            .to_vec()
    };
    PathBuf::from(std::ffi::OsString::from_wide(&plain))
}

pub(crate) fn listed(path: &[u16], reason: Reason) -> Listed {
    Listed {
        path: PathBuf::from(std::ffi::OsString::from_wide(path)),
        reason,
    }
}

pub(crate) fn failure(path: &[u16], error: u32) -> Listed {
    listed(path, Reason::Failed(error))
}
