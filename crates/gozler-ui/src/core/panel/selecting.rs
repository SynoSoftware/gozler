use super::*;
use crate::core::commands::CommandId;
use crate::core::selection::{Change, Kinds, push_rows};
use gozler_core::fold::fold_into;
use gozler_core::query::{MaskScratch, NameMask};
use gozler_core::wtf8;
use gozler_fileops::{End as Ended, Outcome, Reason};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The entries of a panel's folder that a selection command matches
/// (DESIGN.md: Selecting entries).
pub(crate) enum Matcher {
    Mask(NameMask),
    Every,
    /// The names with this extension, folded; `None` for the names without
    /// one, folders included.
    Extension(Option<Vec<u8>>),
    /// The names whose part before the extension is this one, folded.
    Stem(Vec<u8>),
    /// This name, folded.
    Name(Vec<u8>),
    /// The entries of the panel's folder that `paths` names.
    Paths(Paths),
}

/// Full paths that a selection command selects where the panel shows their
/// folder.
#[derive(Clone, Debug)]
pub(crate) enum Paths {
    /// The sources of a copy, a move or a delete.
    Sources(Arc<Request>),
    /// What Save selection saved.
    Saved(Arc<[PathBuf]>),
    /// The sources that an operation did not finish: each one at or above a
    /// path its outcome lists, or every one when it did not complete.
    Unfinished(Arc<Request>, Arc<Outcome>),
    /// What Load selection from file… or from the clipboard read, with
    /// that command.
    Loaded(Arc<[PathBuf]>, CommandId),
}

impl Paths {
    fn all(&self) -> &[PathBuf] {
        let request = match self {
            Paths::Saved(paths) | Paths::Loaded(paths, _) => return paths,
            Paths::Sources(request) | Paths::Unfinished(request, _) => request,
        };
        match &**request {
            Request::Copy { sources, .. }
            | Request::Move { sources, .. }
            | Request::Delete { sources } => sources,
            Request::NewFolder { .. } | Request::Synchronize { .. } | Request::Rename { .. } => &[],
        }
    }

    /// The paths, each as its folder and its folded name.
    fn named(&self) -> Vec<(String, Vec<u8>)> {
        let all = self.all();
        let unfinished = match self {
            Paths::Unfinished(_, outcome) => Some(unfinished(all, outcome)),
            Paths::Sources(_) | Paths::Saved(_) | Paths::Loaded(..) => None,
        };
        (all.iter().enumerate())
            .filter(|(at, _)| unfinished.as_ref().is_none_or(|kept| kept.contains(at)))
            .map(|(_, path)| {
                let path = path.to_string_lossy();
                let mut name = Vec::new();
                fold_into(last_name(&path).as_bytes(), &mut name);
                (parent_of(&path), name)
            })
            .collect()
    }
}

/// The positions in `sources` that `outcome` did not finish.
fn unfinished(sources: &[PathBuf], outcome: &Outcome) -> HashSet<usize> {
    if !matches!(outcome.end, Ended::Complete) {
        return (0..sources.len()).collect();
    }
    let key = |path: &str| fold(path.trim_end_matches('\\').as_bytes());
    let positions: HashMap<Vec<u8>, usize> = (sources.iter().enumerate())
        .map(|(at, path)| (key(&path.to_string_lossy()), at))
        .collect();
    let mut kept = HashSet::new();
    let failed = (outcome.listed.iter()).filter(|listed| {
        !matches!(
            listed.reason,
            Reason::RecoveryFailed(_) | Reason::CleanupFailed(_)
        )
    });
    for listed in failed {
        let path = listed.path.to_string_lossy();
        let mut at: &str = &path;
        loop {
            if let Some(source) = positions.get(&key(at)) {
                kept.insert(*source);
                break;
            }
            match at.trim_end_matches('\\').rsplit_once('\\') {
                Some((parent, _)) if !parent.is_empty() => at = parent,
                _ => break,
            }
        }
    }
    kept
}

pub(crate) struct Selecting {
    rows: Arc<FolderRows>,
    selected: Arc<Vec<Range<u64>>>,
    folder: String,
    epoch: u64,
    revision: u64,
    up: u64,
    matcher: Matcher,
    change: Change,
    kinds: Kinds,
}

pub(crate) struct Selected {
    input: Selecting,
    selection: RowSelection,
    /// The paths a rule of paths names, and how many of them the folder
    /// shows.
    named: Option<(u64, u64)>,
}

impl Selecting {
    pub(super) fn new(panel: &Panel, matcher: Matcher, change: Change, kinds: Kinds) -> Self {
        Self {
            rows: Arc::clone(&panel.rows),
            selected: Arc::clone(&panel.selected),
            folder: panel.folder.clone(),
            epoch: panel.epoch,
            revision: panel.interaction_revision,
            up: panel.up_rows(),
            matcher,
            change,
            kinds,
        }
    }

    pub(crate) fn items(&self) -> usize {
        let paths = match &self.matcher {
            Matcher::Paths(paths) => paths.all().len(),
            _ => 0,
        };
        (self.rows.items())
            .saturating_add(self.selected.len())
            .saturating_add(paths)
    }

    pub(crate) fn run(self) -> Selected {
        let mut scratch = MaskScratch::default();
        let mut name = Vec::new();
        let mut folded = Vec::new();
        // The folded names a rule of paths or a related mask looks for.
        let (names, named) = match &self.matcher {
            Matcher::Paths(paths) => {
                // A list can name one entry twice, as a name and as a full
                // path, and the folder shows it once.
                let distinct: HashSet<(Vec<u8>, Vec<u8>)> = (paths.named().into_iter())
                    .map(|(folder, name)| (folder_key(&folder), name))
                    .collect();
                let shown = folder_key(&self.folder);
                let here: HashSet<Vec<u8>> = (distinct.iter())
                    .filter(|(folder, _)| *folder == shown)
                    .map(|(_, name)| name.clone())
                    .collect();
                (here, Some(distinct.len() as u64))
            }
            // `*.jpg>*.cr2`: the names without extension of the matched
            // entries, which a related entry must have.
            Matcher::Mask(mask) if mask.has_related() => {
                let mut stems = HashSet::new();
                for row in 0..self.rows.count(self.up) {
                    if let Some(entry) = self.rows.entry(row, self.up) {
                        name.clear();
                        wtf8::from_utf16(&entry.name, &mut name);
                        if mask.matches(&name, &mut scratch) {
                            stems.insert(fold(stem_of(&name, entry.is_folder())));
                        }
                    }
                }
                (stems, None)
            }
            _ => (HashSet::new(), None),
        };
        let mut was = self.selected.iter().peekable();
        let mut selected = Vec::new();
        let mut found = 0;
        for row in 0..self.rows.count(self.up) {
            while was.next_if(|rows| rows.end <= row).is_some() {}
            let mut is = was.peek().is_some_and(|rows| rows.contains(&row));
            if let Some(entry) = self.rows.entry(row, self.up) {
                let folder = entry.is_folder();
                if !matches!(self.matcher, Matcher::Every) {
                    name.clear();
                    wtf8::from_utf16(&entry.name, &mut name);
                }
                let matched = match &self.matcher {
                    Matcher::Mask(mask) => {
                        mask.matches(&name, &mut scratch)
                            || (mask.relates(&name, &mut scratch) && {
                                folded.clear();
                                fold_into(stem_of(&name, folder), &mut folded);
                                names.contains(&folded)
                            })
                    }
                    Matcher::Every => true,
                    Matcher::Extension(extension) => {
                        let own = gozler_core::query::extension(&name, folder);
                        match (own, extension) {
                            (Some(own), Some(extension)) => {
                                folded.clear();
                                fold_into(own, &mut folded);
                                folded == *extension
                            }
                            (None, None) => true,
                            _ => false,
                        }
                    }
                    Matcher::Stem(stem) => {
                        folded.clear();
                        fold_into(stem_of(&name, folder), &mut folded);
                        folded == *stem
                    }
                    Matcher::Name(wanted) => {
                        folded.clear();
                        fold_into(&name, &mut folded);
                        folded == *wanted
                    }
                    Matcher::Paths(_) => {
                        folded.clear();
                        fold_into(&name, &mut folded);
                        names.contains(&folded)
                    }
                };
                if matched {
                    found += 1;
                }
                if self.kinds.takes(folder) {
                    is = match self.change {
                        Change::Select if matched => true,
                        Change::Clear if matched => false,
                        Change::Invert if matched => !is,
                        Change::Replace => matched,
                        Change::Select | Change::Clear | Change::Invert => is,
                    };
                }
            }
            if is {
                push_rows(&mut selected, row..row + 1);
            }
        }
        Selected {
            input: self,
            selection: selected.into(),
            named: named.map(|total| (total, found)),
        }
    }
}

/// UTF-8's byte-order mark, which starts a saved selection so that Total
/// Commander reads it as UTF-8.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// What Save selection to file… writes for `paths`: each one's name, or its
/// full path when `names` is false, on a line of its own, in UTF-8 with a
/// byte-order mark.
pub(crate) fn listing(paths: &[PathBuf], names: bool) -> Vec<u8> {
    let mut text = UTF8_BOM.to_vec();
    for path in paths {
        let line = match path.file_name() {
            Some(name) if names => name,
            _ => path.as_os_str(),
        };
        text.extend_from_slice(line.to_string_lossy().as_bytes());
        text.extend_from_slice(b"\r\n");
    }
    text
}

/// The text of a file that Load selection from file… reads: UTF-16 after
/// its byte-order mark, as Total Commander's Unicode save writes it, and
/// otherwise UTF-8, with or without one.
pub(crate) fn decoded(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// The entries that the lines of `text` name: a name is an entry of
/// `folder`, the panel's, and a full path is an entry of its own folder.
/// Double quotes around a line, as Copy as path writes them, are dropped,
/// and so are spaces at its end, which no name on Windows has.
pub(crate) fn listed_paths(text: &str, folder: &str) -> Vec<PathBuf> {
    (text.lines())
        .map(|line| {
            let line = line.trim_end();
            (line.strip_prefix('"'))
                .and_then(|quoted| quoted.strip_suffix('"'))
                .unwrap_or(line)
        })
        .filter(|line| !line.is_empty())
        .map(|line| Path::new(folder).join(line))
        .collect()
}

/// A name without its extension; a folder's whole name, because a folder
/// has no extension.
pub(crate) fn stem_of(name: &[u8], folder: bool) -> &[u8] {
    match gozler_core::query::extension(name, folder) {
        Some(extension) => &name[..name.len() - extension.len() - 1],
        None => name,
    }
}

impl Selected {
    /// For a rule of paths: how many paths it names, and how many of them
    /// the panel's folder shows.
    pub(crate) fn named(&self) -> Option<(u64, u64)> {
        self.named
    }

    /// The command whose message reports the count of [`Selected::named`].
    pub(crate) fn command(&self) -> CommandId {
        match &self.input.matcher {
            Matcher::Paths(Paths::Loaded(_, command)) => *command,
            _ => CommandId::RestoreSelection,
        }
    }

    /// Whether it selects again what an ended operation did not finish.
    pub(crate) fn reselects(&self) -> bool {
        matches!(self.input.matcher, Matcher::Paths(Paths::Unfinished(..)))
    }

    pub(super) fn accept(self, panel: &mut Panel) -> bool {
        let current = matches!(panel.view, View::Folder)
            && !panel.rows_pending()
            && self.input.epoch == panel.epoch
            && self.input.revision == panel.interaction_revision
            && Arc::ptr_eq(&self.input.rows, &panel.rows);
        let items = self.input.items();
        if current {
            panel.select_shared(self.selection);
            panel.quick.invalidate();
        } else {
            let count = self.selection.positions.len();
            panel.retired.push(self.selection, count);
        }
        panel.retired.push(self.input, items);
        current
    }
}
