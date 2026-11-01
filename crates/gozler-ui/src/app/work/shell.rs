//! Purpose-local shell invocations retain their STA execution and source.

use super::Surface;
use crate::core::clipboard::folder_of;
use crate::core::fetch::List;
use crate::core::open::{Commands, Entries, EntryAction, Selected};
use crate::core::work::{Id, Replacement, Retirement, Waiting};
use crate::win::folder;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use gozler_client::CancelToken;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ShellTarget {
    Repository,
    Company,
    Credits,
    Services,
}

impl ShellTarget {
    pub(in crate::app) fn address(self) -> &'static str {
        match self {
            Self::Repository => env!("CARGO_PKG_REPOSITORY"),
            Self::Company => "https://synapse.sr",
            Self::Credits => concat!(
                env!("CARGO_PKG_REPOSITORY"),
                "/blob/main/ACKNOWLEDGMENTS.md"
            ),
            Self::Services => "services.msc",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ShellPurpose {
    Target(Surface, ShellTarget),
    Entries(List, EntryAction),
    Commander(List),
    Reveal,
    Setup,
    Administrator,
}

enum Invocation {
    Target(Surface, ShellTarget),
    Setup(PathBuf),
    Entries {
        list: List,
        action: EntryAction,
        entries: Entries,
        commands: Commands,
    },
    /// Show in Commander, which waits until the entry's folder opens
    /// (DESIGN.md: Searching, "where Open goes").
    Commander {
        list: List,
        entry: Selected,
    },
    Reveal(Selected),
    /// A copy of this window that runs as administrator and shows `entry`
    /// in Commander, after Windows refused this window its folder
    /// (DESIGN.md: A panel). `data_dir` is the folder that `--data-dir`
    /// gave this window.
    Administrator {
        entry: Selected,
        data_dir: Option<PathBuf>,
    },
}

impl Invocation {
    fn items(&self) -> usize {
        match self {
            Self::Entries { entries, .. } => entries.items(),
            Self::Target(..)
            | Self::Setup(_)
            | Self::Commander { .. }
            | Self::Reveal(_)
            | Self::Administrator { .. } => 0,
        }
    }

    fn purpose(&self) -> ShellPurpose {
        match self {
            Self::Target(surface, target) => ShellPurpose::Target(*surface, *target),
            Self::Setup(_) => ShellPurpose::Setup,
            Self::Entries { list, action, .. } => ShellPurpose::Entries(*list, *action),
            Self::Commander { list, .. } => ShellPurpose::Commander(*list),
            Self::Reveal(_) => ShellPurpose::Reveal,
            Self::Administrator { .. } => ShellPurpose::Administrator,
        }
    }
}

pub(in crate::app) enum OpenTarget {
    Command(String),
    Path(Vec<u16>),
    /// A junction or a symbolic link at `link` whose `target` is gone.
    BrokenLink {
        link: Vec<u16>,
        target: String,
    },
    /// This program, started as administrator.
    Administrator,
}

pub(in crate::app) struct OpenFailure {
    pub(in crate::app) target: OpenTarget,
    pub(in crate::app) error: io::Error,
}

impl OpenFailure {
    /// The failure to open `path`, which names the target that Windows did
    /// not find when `path` is a junction or a symbolic link. Reads the
    /// link, so it runs on the shell worker's thread.
    fn of_path(path: Vec<u16>, error: io::Error) -> Self {
        let link_target = match error.kind() {
            io::ErrorKind::NotFound => folder::link_target(&path),
            _ => None,
        };
        let target = match link_target {
            Some(target) => OpenTarget::BrokenLink { link: path, target },
            None => OpenTarget::Path(path),
        };
        Self { target, error }
    }
}

#[derive(Default)]
pub(in crate::app) struct Opened {
    pub(in crate::app) failure: Option<OpenFailure>,
    pub(in crate::app) missing: Option<Vec<u16>>,
    /// The entry whose folder opened, which the window shows in Commander.
    pub(in crate::app) commander: Option<Selected>,
    /// The entry whose folder Windows refused this window, beside the
    /// failure, which a copy that runs as administrator can show.
    pub(in crate::app) denied: Option<Selected>,
}

pub(in crate::app) struct Work {
    tasks: Vec<(ShellPurpose, Task<Invocation, Opened>)>,
    setup_surface: Surface,
    retired: Retirement,
}

impl Work {
    pub(super) fn new(waker: OpenWaker) -> Self {
        Self {
            tasks: List::ALL
                .into_iter()
                .flat_map(|list| EntryAction::ALL.map(|action| ShellPurpose::Entries(list, action)))
                .chain(List::ALL.map(ShellPurpose::Commander))
                .chain(
                    [Surface::Main, Surface::Settings]
                        .into_iter()
                        .flat_map(|surface| {
                            [
                                ShellTarget::Repository,
                                ShellTarget::Company,
                                ShellTarget::Credits,
                                ShellTarget::Services,
                            ]
                            .map(|target| ShellPurpose::Target(surface, target))
                        }),
                )
                .chain([
                    ShellPurpose::Setup,
                    ShellPurpose::Reveal,
                    ShellPurpose::Administrator,
                ])
                .map(|purpose| {
                    // Windows' permission prompt cannot be taken back.
                    let replacement =
                        if matches!(purpose, ShellPurpose::Setup | ShellPurpose::Administrator) {
                            Replacement::Finish
                        } else {
                            Replacement::Cancel
                        };
                    (
                        purpose,
                        Task::shell(waker.clone(), replacement, invoke)
                            .with_size(Invocation::items),
                    )
                })
                .collect(),
            setup_surface: Surface::Main,
            retired: Retirement::default(),
        }
    }

    pub(in crate::app) fn open(&mut self, target: ShellTarget, surface: Surface) {
        self.invoke(Invocation::Target(surface, target));
    }

    pub(in crate::app) fn start_setup(&mut self, path: PathBuf, surface: Surface) {
        self.setup_surface = surface;
        self.invoke(Invocation::Setup(path));
    }

    pub(in crate::app) fn open_entries(
        &mut self,
        list: List,
        action: EntryAction,
        entries: Entries,
        commands: Commands,
    ) {
        self.invoke(Invocation::Entries {
            list,
            action,
            entries,
            commands,
        });
    }

    pub(in crate::app) fn show_in_commander(&mut self, list: List, entry: Selected) {
        self.invoke(Invocation::Commander { list, entry });
    }

    pub(in crate::app) fn reveal(&mut self, entry: Selected) {
        self.invoke(Invocation::Reveal(entry));
    }

    pub(in crate::app) fn start_as_administrator(
        &mut self,
        entry: Selected,
        data_dir: Option<PathBuf>,
    ) {
        self.invoke(Invocation::Administrator { entry, data_dir });
    }

    fn invoke(&mut self, invocation: Invocation) {
        let purpose = invocation.purpose();
        let task = &mut self
            .tasks
            .iter_mut()
            .find(|(each, _)| *each == purpose)
            .expect("every shell purpose has a slot")
            .1;
        task.replace(invocation);
        let retired = task.take_retired();
        self.retired.append(retired);
    }

    pub(in crate::app) fn take_opened(&mut self) -> Vec<(ShellPurpose, Id, Opened)> {
        let mut retired = Retirement::default();
        let opened = self
            .tasks
            .iter_mut()
            .filter_map(|(target, task)| {
                let opened = task.take();
                retired.append(task.take_retired());
                let (id, opened) = opened?;
                (*target == ShellPurpose::Setup
                    || opened.failure.is_some()
                    || opened.missing.is_some()
                    || opened.commander.is_some())
                .then_some((*target, id, opened))
            })
            .collect();
        self.retired.append(retired);
        opened
    }

    pub(in crate::app) fn is_current(&self, purpose: ShellPurpose, id: Id) -> bool {
        self.tasks
            .iter()
            .any(|(each, task)| *each == purpose && task.is_current(id))
    }

    pub(super) fn slow(&self, list: List, now: Instant) -> Option<ShellPurpose> {
        self.tasks.iter().find_map(|(purpose, task)| {
            ((matches!(purpose, ShellPurpose::Entries(source, _) | ShellPurpose::Commander(source)
                if *source == list)
                || *purpose == ShellPurpose::Reveal)
                && task.waiting(now) == Some(Waiting::Slow))
            .then_some(*purpose)
        })
    }

    pub(super) fn global_waiting(
        &self,
        surface: Surface,
        now: Instant,
    ) -> Option<(ShellPurpose, Waiting)> {
        self.tasks.iter().find_map(|(purpose, task)| {
            let owner = match purpose {
                ShellPurpose::Target(owner, _) => *owner,
                ShellPurpose::Setup => self.setup_surface,
                // Windows' permission prompt shows that it waits.
                ShellPurpose::Entries(..)
                | ShellPurpose::Commander(_)
                | ShellPurpose::Reveal
                | ShellPurpose::Administrator => {
                    return None;
                }
            };
            (owner == surface)
                .then(|| task.waiting(now).map(|waiting| (*purpose, waiting)))
                .flatten()
        })
    }

    pub(super) fn setup_waiting(&self, now: Instant) -> Option<Waiting> {
        self.tasks.iter().find_map(|(purpose, task)| {
            (*purpose == ShellPurpose::Setup)
                .then(|| task.waiting(now))
                .flatten()
        })
    }

    pub(in crate::app) fn cancel(&mut self, purpose: ShellPurpose) {
        self.tasks
            .iter_mut()
            .find(|(source, _)| *source == purpose)
            .expect("every shell purpose has a slot")
            .1
            .cancel();
    }

    pub(super) fn tick(&mut self, now: Instant) -> bool {
        self.tasks.iter_mut().fold(false, |active, (_, task)| {
            task.tick(now).is_some() || active
        })
    }

    pub(super) fn take_retired(&mut self) -> Retirement {
        for (_, task) in &mut self.tasks {
            self.retired.append(task.take_retired());
        }
        std::mem::take(&mut self.retired)
    }

    pub(super) fn close(&mut self) -> Retirement {
        for (_, task) in &mut self.tasks {
            task.cancel();
            self.retired.append(task.close_delivery());
        }
        self.take_retired()
    }
}

fn invoke(
    invocation: Invocation,
    session: &crate::win::shell::Session,
    _worker: &OffThread,
    cancel: &CancelToken,
) -> Opened {
    if cancel.is_cancelled() {
        return Opened::default();
    }
    match invocation {
        Invocation::Setup(path) => Opened {
            failure: session.start_program(&path).err().map(|error| OpenFailure {
                target: OpenTarget::Path(path.as_os_str().encode_wide().collect()),
                error,
            }),
            ..Opened::default()
        },
        Invocation::Target(_, target) => Opened {
            failure: session
                .open(target.address())
                .err()
                .map(|error| OpenFailure {
                    target: OpenTarget::Path(target.address().encode_utf16().collect()),
                    error,
                }),
            ..Opened::default()
        },
        Invocation::Administrator { entry, data_dir } => {
            let started = std::env::current_exe().and_then(|program| {
                let parameters = administrator_parameters(&entry, data_dir.as_deref());
                session.start_program_as_administrator(&program, &parameters)
            });
            Opened {
                failure: started
                    .err()
                    .filter(|error| !crate::win::shell::refused(error))
                    .map(|error| OpenFailure {
                        target: OpenTarget::Administrator,
                        error,
                    }),
                ..Opened::default()
            }
        }
        Invocation::Commander { entry, .. } | Invocation::Reveal(entry) => {
            // Only a name that is gone is missing: a file in a folder that
            // Windows refuses, such as one in C:\Windows\Prefetch, gets
            // the folder's reason below. A folder that the disk shows to be
            // a file, as `--open` may name, shows in its folder; a file
            // stays one, because Reveal shows a folder in its parent.
            let entry = match folder::is_folder(&entry.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Opened {
                        missing: Some(entry.path),
                        ..Opened::default()
                    };
                }
                Ok(false) => Selected {
                    is_folder: false,
                    ..entry
                },
                Ok(true) | Err(_) => entry,
            };
            let folder = if entry.is_folder {
                entry.path.clone()
            } else {
                folder_of(&entry.path).to_vec()
            };
            match folder::lists(&folder) {
                Ok(()) => Opened {
                    commander: Some(entry),
                    ..Opened::default()
                },
                Err(error) => Opened {
                    denied: (error.kind() == io::ErrorKind::PermissionDenied
                        && folder::on_this_pc(&folder))
                    .then_some(entry),
                    failure: Some(OpenFailure::of_path(folder, error)),
                    ..Opened::default()
                },
            }
        }
        Invocation::Entries {
            list,
            action,
            entries,
            commands,
        } => {
            let mut opened = Opened::default();
            if cancel.is_cancelled() {
                return opened;
            }
            let entries = entries
                .resolve()
                .into_iter()
                .take_while(|_| !cancel.is_cancelled());
            let entries: Vec<_> = if list == List::Search {
                entries
                    .filter(|entry| {
                        if std::fs::symlink_metadata(OsString::from_wide(&entry.path)).is_ok() {
                            true
                        } else {
                            opened.missing.get_or_insert_with(|| entry.path.clone());
                            false
                        }
                    })
                    .collect()
            } else {
                entries.collect()
            };
            opened.failure = act_on_entries(session, action, entries, &commands, cancel).err();
            opened
        }
    }
}

fn act_on_entries(
    session: &crate::win::shell::Session,
    action: EntryAction,
    entries: Vec<Selected>,
    commands: &Commands,
    cancel: &CancelToken,
) -> Result<(), OpenFailure> {
    let mut files = Vec::new();
    let mut failure = None;
    for entry in entries.into_iter().take_while(|_| !cancel.is_cancelled()) {
        if let Some(line) = commands.expand(action, &entry) {
            session
                .run_command(&line.program, &line.parameters)
                .map_err(|error| OpenFailure {
                    target: OpenTarget::Command(line.program),
                    error,
                })?;
            continue;
        }
        let result = match action {
            EntryAction::Open => session.open_entry(&entry.path),
            EntryAction::Properties => session.properties(&entry.path),
            EntryAction::Explore if entry.is_folder => session.explore(&entry.path),
            EntryAction::OpenFileLocation | EntryAction::Explore => {
                files.push(entry);
                continue;
            }
        };
        if let Err(error) = result {
            failure.get_or_insert_with(|| OpenFailure::of_path(entry.path, error));
        }
    }
    let mut folders: Vec<(Vec<u16>, Vec<Vec<u16>>)> = Vec::new();
    for Selected { path, .. } in files {
        let folder = folder_of(&path).to_vec();
        match folders.iter_mut().find(|(each, _)| *each == folder) {
            Some((_, entries)) => entries.push(path),
            None => folders.push((folder, vec![path])),
        }
    }
    for (folder, entries) in folders.into_iter().take_while(|_| !cancel.is_cancelled()) {
        if let Err(error) = session.show_in_folder(&folder, &entries) {
            failure.get_or_insert_with(|| OpenFailure::of_path(folder, error));
        }
    }
    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// The command line that has a copy of this window show `entry` in
/// Commander: `--open` for a folder and `--reveal` for a file, with this
/// window's `--data-dir`, so the copy replaces this window and no other.
fn administrator_parameters(entry: &Selected, data_dir: Option<&Path>) -> OsString {
    let mut parameters = OsString::from(if entry.is_folder {
        "--open "
    } else {
        "--reveal "
    });
    push_quoted(&mut parameters, &OsString::from_wide(&entry.path));
    if let Some(folder) = data_dir {
        parameters.push(" --data-dir ");
        push_quoted(&mut parameters, folder.as_os_str());
    }
    parameters
}

/// Adds `argument` in quotes, with the backslashes before the closing quote
/// doubled, because Windows reads `\"` as an escaped quote: `"C:\"` passes
/// `C:\`. A path holds no quote of its own.
fn push_quoted(line: &mut OsString, argument: &OsStr) {
    let units: Vec<u16> = argument.encode_wide().collect();
    let trailing = units
        .iter()
        .rev()
        .take_while(|&&unit| unit == u16::from(b'\\'))
        .count();
    line.push("\"");
    line.push(argument);
    line.push("\\".repeat(trailing));
    line.push("\"");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The copy started as administrator reads its folder from this line:
    /// an unquoted drive root would end in `\"` and lose its backslash,
    /// and a missing `--data-dir` would replace the person's own window.
    #[test]
    fn administrator_parameters_keep_a_root_and_the_data_folder() {
        let folder = Selected {
            path: r"C:\".encode_utf16().collect(),
            is_folder: true,
        };
        assert_eq!(
            administrator_parameters(&folder, Some(Path::new(r"D:\Test data"))),
            OsString::from(r#"--open "C:\\" --data-dir "D:\Test data""#)
        );
        let file = Selected {
            path: r"C:\Windows\Prefetch\a b.pf".encode_utf16().collect(),
            is_folder: false,
        };
        assert_eq!(
            administrator_parameters(&file, None),
            OsString::from(r#"--reveal "C:\Windows\Prefetch\a b.pf""#)
        );
    }
}
