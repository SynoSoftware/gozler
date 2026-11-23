use super::status::{JobCaption, StatusWidest, fix_status};
use super::{CaptionTable, DialogItem, MessageButton, failure_text};
use crate::core::fetch::{Beside, DaemonFailure, Local, ProgramFile, Programs};
use crate::core::glyph::Glyph;
use crate::core::ids::ControlId;
use crate::core::model::{Job, Model};
use crate::core::results::{Fix, ListButton};
use crate::core::settings::SettingsPage;
use crate::core::text::{self, Numbers, Strings};
use gozler_core::index::CREATED_ENTRY_BYTES;
use gozler_core::proto::Disk;
use gozler_core::types::{IndexProgress, VolumePhase};

/// The groups of the Settings pages: each title, the static icon before it
/// and the icon it shows.
pub(crate) const SETTINGS_GROUPS: &[(ControlId, ControlId, Glyph)] = &[
    (
        ControlId::GeneralDisplay,
        ControlId::GeneralDisplayIcon,
        Glyph::Globe,
    ),
    (
        ControlId::GeneralFormats,
        ControlId::GeneralFormatsIcon,
        Glyph::BulletedList,
    ),
    (
        ControlId::GeneralStarting,
        ControlId::GeneralStartingIcon,
        Glyph::PowerButton,
    ),
    (
        ControlId::SearchBoxGroup,
        ControlId::SearchBoxGroupIcon,
        Glyph::Search,
    ),
    (
        ControlId::SearchResultsGroup,
        ControlId::SearchResultsGroupIcon,
        Glyph::OpenFile,
    ),
    (
        ControlId::ContextMenuItemGroup,
        ControlId::ContextMenuItemGroupIcon,
        Glyph::BulletedList,
    ),
    (
        ControlId::ContextMenuOpenGroup,
        ControlId::ContextMenuOpenGroupIcon,
        Glyph::OpenFile,
    ),
    (
        ControlId::KeyboardShowGroup,
        ControlId::KeyboardShowGroupIcon,
        Glyph::BackToWindow,
    ),
    (
        ControlId::KeyboardCommandsGroup,
        ControlId::KeyboardCommandsGroupIcon,
        Glyph::KeyboardShortcut,
    ),
    (
        ControlId::IndexDrivesGroup,
        ControlId::IndexDrivesGroupIcon,
        Glyph::HardDrive,
    ),
    (
        ControlId::IndexHoldsGroup,
        ControlId::IndexHoldsGroupIcon,
        Glyph::Library,
    ),
    (
        ControlId::IndexServiceGroup,
        ControlId::IndexServiceGroupIcon,
        Glyph::Processing,
    ),
    (
        ControlId::SsdWearDisksGroup,
        ControlId::SsdWearDisksGroupIcon,
        Glyph::HardDrive,
    ),
    (
        ControlId::VisibilityGroup,
        ControlId::VisibilityGroupIcon,
        Glyph::People,
    ),
    (
        ControlId::IncludeFoldersGroup,
        ControlId::IncludeFoldersGroupIcon,
        Glyph::Folder,
    ),
    (
        ControlId::IncludeNamesGroup,
        ControlId::IncludeNamesGroupIcon,
        Glyph::Rename,
    ),
    (
        ControlId::SpeedCopyingGroup,
        ControlId::SpeedCopyingGroupIcon,
        Glyph::Copy,
    ),
    (
        ControlId::SpeedRingGroup,
        ControlId::SpeedRingGroupIcon,
        Glyph::Sync,
    ),
    (
        ControlId::SpeedDeleteGroup,
        ControlId::SpeedDeleteGroupIcon,
        Glyph::Delete,
    ),
];

/// What Settings > Index shows (DESIGN.md: The Drives and service page):
/// each drive by its mount prefix and label, its state and its entry count,
/// the service's state and memory, the gozlerd.exe files, the failure's fix,
/// and whether the service has a report to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IndexCaptions {
    /// Each drive's name, state and entry count, as the list's columns.
    pub(crate) drives: Vec<[String; 3]>,
    /// What the drive list's empty area says, or empty.
    pub(crate) no_drives: String,
    pub(crate) service: String,
    pub(crate) service_program: ProgramLines,
    /// The gozlerd.exe beside the window.
    pub(crate) beside: ProgramLines,
    /// Why Use local is not offered, or empty.
    pub(crate) note: String,
    /// The note under Dates created, with the memory it costs once the
    /// service has told its entries.
    pub(crate) dates_created_note: String,
    /// The Use local button, which shows before the fix's.
    pub(crate) use_local: Option<MessageButton>,
    pub(crate) fix: Option<MessageButton>,
    pub(crate) has_report: bool,
}

/// A gozlerd.exe file's path and build, each empty while unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramLines {
    pub(crate) path: String,
    pub(crate) build: String,
}

impl ProgramLines {
    fn of(strings: &Strings, numbers: &dyn Numbers, file: &ProgramFile) -> ProgramLines {
        let modified = numbers.date_time_seconds(file.modified);
        // Without the time in parentheses, which `ProgramFile::modified`
        // replaces.
        let number = file
            .version
            .as_deref()
            .and_then(|version| version.split_whitespace().next());
        let build = match number {
            Some(number) => strings.format("settings_index.version_modified", &[number, &modified]),
            None => strings.format("settings_index.modified", &[&modified]),
        };
        ProgramLines {
            path: file.path.display().to_string(),
            build,
        }
    }

    /// The service's program and the gozlerd.exe beside the window.
    fn both(strings: &Strings, numbers: &dyn Numbers, programs: &Programs) -> [ProgramLines; 2] {
        let service = programs
            .service
            .as_ref()
            .map(|file| ProgramLines::of(strings, numbers, file))
            .unwrap_or_default();
        let beside = match &programs.beside {
            Beside::Unknown => ProgramLines::default(),
            Beside::SameAsService(path) => ProgramLines {
                path: path.display().to_string(),
                build: strings.text("settings_index.same_as_service").to_owned(),
            },
            Beside::Other(file) => ProgramLines::of(strings, numbers, file),
        };
        [service, beside]
    }
}

impl IndexCaptions {
    /// `label` gives the volume label of a mount prefix, such as "Windows"
    /// for `C:\`, which only Windows knows.
    pub(crate) fn new(
        model: &Model,
        numbers: &dyn Numbers,
        label: &dyn Fn(&str) -> Option<String>,
    ) -> IndexCaptions {
        let strings = model.strings();
        let (status, failure) = model.service();
        let programs = model.programs();
        let [service_program, beside] = programs
            .map(|programs| ProgramLines::both(strings, numbers, programs))
            .unwrap_or_default();
        let note = match programs.map(|programs| &programs.local) {
            Some(Local::Mismatched) => strings.text("fixing.use_local_not_paired").to_owned(),
            Some(Local::NotOffered | Local::Offered(_)) | None => String::new(),
        };
        let files = IndexCaptions {
            service_program,
            beside,
            note,
            dates_created_note: strings.text("settings_index.dates_created_note").to_owned(),
            ..IndexCaptions::default()
        };
        if let Some(failure) = failure {
            let fix_button = |fix: Fix| MessageButton::new(model, &ListButton::Fix(fix));
            return IndexCaptions {
                no_drives: strings.text("settings_index.no_drives").to_owned(),
                service: failure_text(strings, failure),
                use_local: model.use_local_fix().map(fix_button),
                fix: model.service_fix().map(fix_button),
                ..files
            };
        }
        let Some(status) = status else {
            return IndexCaptions {
                service: strings.text("settings_index.asking").to_owned(),
                ..files
            };
        };
        let name = |prefix: &str| match label(prefix).filter(|label| !label.is_empty()) {
            Some(label) => format!("{prefix} {label}"),
            None => prefix.to_owned(),
        };
        let indexed = status.volumes.iter().map(|volume| {
            let state = match volume.phase {
                VolumePhase::Live if volume.metadata_pending => "settings_index.live_pending",
                VolumePhase::Live => "settings_index.live",
                VolumePhase::Snapshot => "settings_index.snapshot",
                VolumePhase::Bootstrapping | VolumePhase::Rescanning => "settings_index.indexing",
                VolumePhase::Offline => "settings_index.offline",
                VolumePhase::Failed { .. } => "settings_index.failed",
            };
            let mut state = strings.text(state).to_owned();
            let mut entries = volume.entries;
            if matches!(
                volume.phase,
                VolumePhase::Bootstrapping | VolumePhase::Rescanning
            ) {
                match volume.progress {
                    Some(IndexProgress::Scanning {
                        entries: found,
                        percent,
                    }) => {
                        entries = found;
                        if let Some(percent) = percent {
                            state = strings.format(
                                "status.progress_percent",
                                &[&state, &numbers.integer(u64::from(percent))],
                            );
                        }
                    }
                    Some(IndexProgress::Preparing { entries: found }) => {
                        entries = found;
                        state = strings.text("operation.preparing").to_owned();
                    }
                    None => {}
                }
            }
            [
                name(volume.mounted().display_prefix()),
                state,
                numbers.integer(entries),
            ]
        });
        // A drive the Include settings leave out has no entries.
        let left_out = status.left_out.iter().map(|volume| {
            [
                name(volume.mounted().display_prefix()),
                strings.text("settings_index.left_out").to_owned(),
                String::new(),
            ]
        });
        let drives = indexed.chain(left_out).collect();
        let service = match model.service_working_set() {
            None => strings.text("settings_index.running").to_owned(),
            Some(bytes) => strings.format(
                "settings_index.running_memory",
                &[&text::bytes(strings, numbers, bytes)],
            ),
        };
        let entries: u64 = status.volumes.iter().map(|volume| volume.entries).sum();
        let dates_created_note = if entries == 0 {
            files.dates_created_note.clone()
        } else {
            strings.format(
                "settings_index.dates_created_note_size",
                &[&text::bytes(
                    strings,
                    numbers,
                    entries * CREATED_ENTRY_BYTES,
                )],
            )
        };
        IndexCaptions {
            drives,
            dates_created_note,
            service,
            use_local: model
                .use_local_fix()
                .map(|fix| MessageButton::new(model, &ListButton::Fix(fix))),
            has_report: true,
            ..files
        }
    }
}

/// The text that `gozler --status` prints for the service's latest status,
/// with its times in `tz`, which Show report shows; `None` while the service
/// does not answer.
pub(crate) fn service_report(model: &Model, tz: &jiff::tz::TimeZone) -> Option<String> {
    let (Some(status), None) = model.service() else {
        return None;
    };
    let mut report = Vec::new();
    gozler_core::output::write_status(status, tz, &mut report)
        .expect("writing to memory cannot fail");
    Some(String::from_utf8_lossy(&report).into_owned())
}

/// The Settings pages, in their order, whose name and captions hold every
/// word of `search` at the start of one of their words, ignoring case, so
/// "upd" finds "Check for updates" and "dates" does not (DESIGN.md:
/// Settings dialog). An empty search keeps every page.
pub(crate) fn settings_pages_matching(strings: &Strings, search: &str) -> Vec<SettingsPage> {
    let terms = words(search);
    SettingsPage::ALL
        .into_iter()
        .filter(|page| {
            let table = CaptionTable::of(page.dialog());
            let page_words: Vec<String> = std::iter::once(page.label())
                .chain(table.keyed.iter().chain(table.plain).map(|(_, key)| *key))
                .flat_map(|key| words(strings.text(key)))
                .collect();
            terms.iter().all(|term| starts_a_word(&page_words, term))
        })
        .collect()
}

/// Each control of `page` with a caption, and whether that caption holds a
/// word of `search` at the start of one of its words, so the dialog shows
/// what the search found in bold (DESIGN.md: Settings dialog).
pub(crate) fn settings_page_hits(
    strings: &Strings,
    page: SettingsPage,
    search: &str,
) -> Vec<(ControlId, bool)> {
    let terms = words(search);
    let table = CaptionTable::of(page.dialog());
    table
        .keyed
        .iter()
        .chain(table.plain)
        .filter_map(|(item, key)| match item {
            DialogItem::Control(control) => {
                let caption = words(strings.text(key));
                let hit = terms.iter().any(|term| starts_a_word(&caption, term));
                Some((*control, hit))
            }
            DialogItem::Ok | DialogItem::Cancel => None,
        })
        .collect()
}

/// Whether one of `words` starts with `term`.
fn starts_a_word(words: &[String], term: &str) -> bool {
    words.iter().any(|word| word.starts_with(term))
}

/// The words of `text` in lower case: its runs of letters and digits. An
/// access key's `&` joins the letters around it, as in "f&iles", and a
/// link's `<a>` and `</a>` are not words.
fn words(text: &str) -> Vec<String> {
    text.replace('&', "")
        .replace("<a>", "")
        .replace("</a>", "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(crate) fn settings_status_view(strings: &Strings, changed: bool) -> &str {
    if changed {
        strings.text("settings.unsaved")
    } else {
        ""
    }
}

pub(crate) fn settings_job(
    model: &Model,
    strings: &Strings,
    numbers: &dyn Numbers,
    follow_fix: bool,
) -> JobCaption {
    if !follow_fix || !model.job_notice_visible(Job::Fix) {
        return JobCaption::default();
    }
    let (text, progress) = fix_status(strings, numbers, model.fix());
    JobCaption {
        text,
        progress,
        cancel: model.job_cancel_shown(Job::Fix),
    }
}

/// Settings > SSD wear's disk list (DESIGN.md: The SSD wear page): one row
/// for each disk that reports its wear or the bytes written to it, such as
/// "Samsung SSD 990 PRO 2TB (C:, D:)", "4% used" and "34.9 TB written". A
/// disk that reports neither has no row, and a value that a disk does not
/// report leaves its cell empty.
pub(crate) fn disk_rows(
    strings: &Strings,
    numbers: &dyn Numbers,
    disks: &[Disk],
) -> Vec<[String; 3]> {
    let reporting = disks
        .iter()
        .filter(|disk| disk.used.is_some() || disk.written.is_some());
    reporting
        .map(|disk| {
            let name = disk.model.clone().unwrap_or_else(|| {
                let number = numbers.integer(u64::from(disk.number));
                strings.format("settings_ssd_wear.disk", &[&number])
            });
            let name = with_mounts(strings, name, &disk.mounts);
            let used = disk.used.map_or_else(String::new, |used| {
                let used = numbers.integer(u64::from(used));
                strings.format("settings_ssd_wear.used", &[&used])
            });
            let written = (disk.written)
                .map_or_else(String::new, |bytes| text::bytes(strings, numbers, bytes));
            [name, used, written]
        })
        .collect()
}

/// A disk's `name` with the drives on it, as `KXG50ZNV512G (C:, D:)`; a
/// drive's `C:\` shows as `C:`, as File Explorer names a drive.
pub(crate) fn with_mounts(strings: &Strings, name: String, mounts: &[String]) -> String {
    if mounts.is_empty() {
        return name;
    }
    let mounts: Vec<&str> = (mounts.iter())
        .map(|mount| mount.strip_suffix('\\').unwrap_or(mount))
        .collect();
    let mounts = mounts.join(", ");
    strings.format("settings_ssd_wear.disk_mounts", &[&name, &mounts])
}

/// What the disk list says while it has no row: "Waiting…" while the
/// service has not answered (`health` is `None`), that no disk reports its
/// health, that an older service cannot read the request, or the service's
/// failure.
pub(crate) fn disks_empty_text(
    strings: &Strings,
    health: Option<&Result<Vec<Disk>, DaemonFailure>>,
) -> String {
    match health {
        None => crate::core::work::waiting(strings, None),
        Some(Ok(_)) => strings.text("settings_ssd_wear.no_disks").to_owned(),
        // A service that cannot read the request is older than the window.
        Some(Err(DaemonFailure::TooOld { .. } | DaemonFailure::Unreadable { .. })) => {
            strings.text("settings_ssd_wear.needs_newer").to_owned()
        }
        Some(Err(failure)) => failure_text(strings, failure),
    }
}

/// Settings' work takes the space after its fixed unsaved-changes part.
pub(crate) fn settings_status_widest(strings: &Strings) -> StatusWidest {
    let view = vec![settings_status_view(strings, true).to_owned()];
    StatusWidest {
        view,
        indexing: String::new(),
        running: Vec::new(),
        done: Vec::new(),
        panel_index: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{disk_rows, settings_page_hits, settings_pages_matching};
    use crate::core::captions::DialogCaptions;
    use crate::core::ids::ControlId;
    use crate::core::settings::SettingsPage;
    use crate::core::text::tests::{English, english};
    use crate::core::text::{LANGUAGES, Strings};
    use gozler_core::proto::Disk;

    /// The owner's rule for the SSD wear page: a disk that reports neither
    /// value has no row, and a disk that reports one value shows it with
    /// the other cell empty. A row names its drives as File Explorer does.
    #[test]
    fn the_disk_list_shows_only_disks_that_report_a_value() {
        let disk = |number, used, written| Disk {
            number,
            model: Some("SSD".into()),
            mounts: vec![r"C:\".into(), r"D:\".into()],
            bus: Some(17),
            used,
            written,
            unread: None,
        };
        let disks = [disk(0, Some(4), None), disk(1, None, None)];
        let rows = disk_rows(&english(), &English, &disks);
        let expected = ["SSD (C:, D:)", "4%", ""].map(String::from);
        assert_eq!(rows, [expected]);
    }

    /// A search that matched inside a word would keep pages the person did
    /// not mean, and one that needed the words in order would drop pages
    /// they did.
    #[test]
    fn a_settings_search_matches_the_start_of_words() {
        let strings = Strings::load(&LANGUAGES[0]);
        let pages = |search| settings_pages_matching(&strings, search);
        assert_eq!(
            pages("UPD"),
            [SettingsPage::General, SettingsPage::FileOperations]
        );
        assert!(pages("pdates").is_empty());
        assert_eq!(pages("cont ite"), [SettingsPage::ContextMenu]);
        assert_eq!(pages(" "), SettingsPage::ALL);
        let bold: Vec<ControlId> = settings_page_hits(&strings, SettingsPage::General, "upd start")
            .into_iter()
            .filter_map(|(control, hit)| hit.then_some(control))
            .collect();
        assert_eq!(
            bold,
            [
                ControlId::GeneralSignIn,
                ControlId::GeneralUpdates,
                ControlId::GeneralStarting
            ]
        );
    }

    /// The Settings buttons share one width, set before a page is made from
    /// the captions that `SettingsDialog::later_captions` finds by their
    /// icon, so a push button added to a page template without a key in
    /// `BUTTON_GLYPHS` would be cut off when its page first shows. The fix
    /// buttons' captions come from `Fix::label_groups`, and Rebuild's is
    /// listed with its shield.
    #[test]
    fn every_settings_page_button_has_an_icon() {
        let script = include_str!("../../../resources/gozler-ui.rc");
        let header = include_str!("../../../resources/ids.h");
        let number = |name: &str| {
            header.lines().find_map(|line| {
                let mut words = line.split_whitespace();
                (words.next() == Some("#define") && words.next() == Some(name))
                    .then(|| words.next()?.parse::<u16>().ok())
                    .flatten()
            })
        };
        let strings = english();
        let shielded = [
            ControlId::IndexUseLocal.id(),
            ControlId::IndexFix.id(),
            ControlId::IndexRebuild.id(),
        ];
        // Each part but the first starts with a template and ends with the
        // name of the next.
        let parts: Vec<&str> = script.split("DIALOGEX").collect();
        for page in SettingsPage::ALL {
            let template = (parts.windows(2))
                .find(|pair| {
                    let name = pair[0].split_whitespace().last().unwrap_or_default();
                    number(name) == Some(page.dialog().id())
                })
                .map(|pair| pair[1].split("\nEND").next().unwrap_or_default())
                .unwrap_or_else(|| panic!("gozler-ui.rc has the template of {page:?}"));
            let glyphs = DialogCaptions::new(page.dialog(), &strings).glyphs;
            for line in template.lines() {
                let Some(rest) = line.trim().strip_prefix("PUSHBUTTON") else {
                    continue;
                };
                let name = rest.split(',').nth(1).unwrap_or_default().trim();
                let id = number(name).unwrap_or_else(|| panic!("ids.h defines {name}"));
                assert!(
                    shielded.contains(&id) || glyphs.iter().any(|(item, _)| *item == id),
                    "{name} on {page:?} has no caption key in BUTTON_GLYPHS"
                );
            }
        }
    }
}
