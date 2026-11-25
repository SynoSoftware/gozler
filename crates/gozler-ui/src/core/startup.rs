//! Explicit requests from another program, shared by startup and window handoff.

use super::query::MatchMode;
use super::settings::{Choices, WearLine};
use super::shortcuts::ShowShortcut;
use gozler_core::types::{SortDir, SortKey};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) const SAVE_ARGUMENT: &str = "--save-include";
pub(crate) const REBUILD_ARGUMENT: &str = "--rebuild-index";
pub(crate) const DELETE_ARGUMENT: &str = "--delete-as-administrator";
pub(crate) const KEEP_ADMINISTRATOR_ARGUMENT: &str = "--keep-administrator";
/// Opens only the compare window for two files (DESIGN.md: Compare by
/// content).
pub(crate) const COMPARE_ARGUMENT: &str = "--compare";
const SETUP_ARGUMENT: &str = "--setup-settings";
const SETUP_READ_ARGUMENT: &str = "--read-setup-settings";
const CLEANUP_ARGUMENT: &str = "--remove-settings";
pub(crate) const USER_CLEANUP_ARGUMENT: &str = "--remove-user-settings";
pub(crate) const REQUEST_TAG: usize = 0x475a_0001;
pub(crate) const MAX_HANDOFF_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "request",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum Request {
    Location(String),
    Search(Search),
    Reveal(String),
    /// A folder to show in Commander, as `--open` asks.
    Open(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Search {
    pub(crate) text: String,
    pub(crate) location: Option<String>,
    pub(crate) mode: Option<MatchMode>,
    pub(crate) case: Option<bool>,
    pub(crate) path: Option<bool>,
    pub(crate) sort: Option<SortKey>,
    pub(crate) direction: Option<SortDir>,
}

impl Request {
    pub(crate) fn encode(&self) -> String {
        serde_json::to_string(self).expect("window requests contain only serializable values")
    }

    pub(crate) fn decode(text: &str) -> Option<Self> {
        if text.encode_utf16().count() * 2 > MAX_HANDOFF_BYTES || text.contains('\0') {
            return None;
        }
        let request = serde_json::from_str::<Self>(text).ok()?;
        request.validate().ok()?;
        Some(request)
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        let path = |path: &str| !path.is_empty() && !path.contains('\0');
        let invalid = match self {
            Self::Location(folder) if !path(folder) => Some("--location"),
            Self::Reveal(file) if !path(file) => Some("--reveal"),
            Self::Open(folder) if !path(folder) => Some("--open"),
            Self::Search(search) if search.text.contains('\0') => Some("--search"),
            Self::Search(search) if !search.location.as_deref().is_none_or(path) => {
                Some("--location")
            }
            _ => None,
        };
        if let Some(option) = invalid {
            return Err(Error::Value(option.into()));
        }
        if self.encode().encode_utf16().count() * 2 > MAX_HANDOFF_BYTES {
            return Err(Error::TooLong);
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Arguments {
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) sign_in: bool,
    pub(crate) request: Option<Request>,
    pub(crate) new_window: bool,
    pub(crate) save_include: Option<PathBuf>,
    pub(crate) rebuild_index: bool,
    pub(crate) unregister_shortcut: bool,
    pub(crate) setup: Option<SetupChoices>,
    /// The Search shortcut that Setup offers, which the answer compares
    /// with the saved one.
    pub(crate) setup_read: Option<ShowShortcut>,
    pub(crate) cleanup: Option<Cleanup>,
    /// The copy that a failure report's Retry starts as administrator. It
    /// is no [`Request`], because a window that runs as administrator
    /// accepts requests from every program, which could then delete any
    /// file without Windows' permission prompt.
    pub(crate) delete: Option<DeleteStart>,
    /// The two files that `--compare` names. It is no [`Request`] either,
    /// because it opens a process of its own that the first window never
    /// sees.
    pub(crate) compare: Option<[PathBuf; 2]>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Cleanup {
    Desktop,
    User,
}

#[derive(Debug, PartialEq)]
pub(crate) struct SetupChoices {
    pub(crate) sign_in: Option<bool>,
    pub(crate) keep_running: Option<bool>,
    pub(crate) disk_warning: Option<bool>,
    /// The outer `None` keeps the saved shortcut; `Some(None)` clears it.
    pub(crate) shortcut: Option<Option<ShowShortcut>>,
}

impl SetupChoices {
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self, Error> {
        let mut toggle = || match value(&mut args, SETUP_ARGUMENT)?.as_str() {
            "keep" => Ok(None),
            "0" => Ok(Some(false)),
            "1" => Ok(Some(true)),
            _ => Err(Error::Value(SETUP_ARGUMENT.into())),
        };
        let sign_in = toggle()?;
        let keep_running = toggle()?;
        let disk_warning = toggle()?;
        let shortcut = match value(&mut args, SETUP_ARGUMENT)?.as_str() {
            "keep" => None,
            "-" => Some(None),
            keys => Some(Some(
                ShowShortcut::from_name(keys).ok_or_else(|| Error::Value(SETUP_ARGUMENT.into()))?,
            )),
        };
        if args.next().is_some() {
            return Err(Error::Conflict);
        }
        Ok(Self {
            sign_in,
            keep_running,
            disk_warning,
            shortcut,
        })
    }
}

/// The exit code of `--read-setup-settings`, which `ReadSetupChoices` in
/// `installer\Setup.iss` decodes: 256 plus a bit for each saved choice.
/// Setup reads any code outside 256..=511 as a failure, so an error exit,
/// a crash or a blocked start never reads as an answer.
pub(crate) fn setup_answer(choices: &Choices, sign_in: bool, offered: ShowShortcut) -> i32 {
    let saved = choices.show_shortcut;
    256 | i32::from(sign_in)
        | i32::from(choices.keep_running) << 1
        | i32::from(choices.wear_alarm.line(WearLine::DiskWeek).on) << 2
        | i32::from(saved == Some(offered)) << 3
        | i32::from(saved.is_some_and(|key| key != offered)) << 4
}

#[derive(Debug, PartialEq)]
pub(crate) struct DeleteStart {
    /// The file that lists the items to delete.
    pub(crate) list: PathBuf,
    pub(crate) keeps_administrator: bool,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Error {
    Missing(String),
    Value(String),
    Conflict,
    SearchRequired,
    TooLong,
}

impl Error {
    pub(crate) fn message(&self, strings: &super::text::Strings) -> String {
        match self {
            Self::Missing(option) => strings.format("message.argument_missing", &[option]),
            Self::Value(option) => strings.format("message.argument_value", &[option]),
            Self::Conflict => strings.text("message.arguments_conflict").to_owned(),
            Self::SearchRequired => strings.text("message.search_required").to_owned(),
            Self::TooLong => strings.text("message.arguments_too_long").to_owned(),
        }
    }
}

impl Arguments {
    pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, Error> {
        let mut args = args.into_iter().peekable();
        if let Some(option) =
            args.next_if(|arg| arg == CLEANUP_ARGUMENT || arg == USER_CLEANUP_ARGUMENT)
        {
            let option = option.to_string_lossy();
            if args.next().is_some() {
                return Err(Error::Conflict);
            }
            return Ok(Self {
                cleanup: Some(if option == CLEANUP_ARGUMENT {
                    Cleanup::Desktop
                } else {
                    Cleanup::User
                }),
                ..Self::default()
            });
        }
        if args.next_if(|arg| arg == SETUP_READ_ARGUMENT).is_some() {
            let offered = ShowShortcut::from_name(&value(&mut args, SETUP_READ_ARGUMENT)?)
                .ok_or_else(|| Error::Value(SETUP_READ_ARGUMENT.into()))?;
            if args.next().is_some() {
                return Err(Error::Conflict);
            }
            return Ok(Self {
                setup_read: Some(offered),
                ..Self::default()
            });
        }
        if args.next_if(|arg| arg == SETUP_ARGUMENT).is_some() {
            return Ok(Self {
                setup: Some(SetupChoices::parse(args)?),
                ..Self::default()
            });
        }
        let mut result = Self::default();
        let mut search = Search::default();
        let mut has_search = false;
        let mut location = None;
        let mut reveal = None;
        let mut open = None;
        let mut delete_list = None;
        let mut keeps_administrator = false;
        while let Some(arg) = args.next() {
            let arg = arg.to_string_lossy();
            match arg.as_ref() {
                "--data-dir" => result.data_dir = args.next().map(PathBuf::from),
                "--sign-in" => result.sign_in = true,
                "--unregister-shortcut" => result.unregister_shortcut = true,
                SAVE_ARGUMENT => result.save_include = args.next().map(PathBuf::from),
                REBUILD_ARGUMENT => result.rebuild_index = true,
                DELETE_ARGUMENT => delete_list = Some(PathBuf::from(value(&mut args, &arg)?)),
                KEEP_ADMINISTRATOR_ARGUMENT => keeps_administrator = true,
                COMPARE_ARGUMENT => {
                    let left = value(&mut args, &arg)?;
                    let right = value(&mut args, &arg)?;
                    let invalid = |path: &str| path.is_empty() || path.contains('\0');
                    if invalid(&left) || invalid(&right) {
                        return Err(Error::Value(arg.into_owned()));
                    }
                    result.compare = Some([PathBuf::from(left), PathBuf::from(right)]);
                }
                "--location" => {
                    location = args
                        .next()
                        .map(|value| legacy_location(&value.to_string_lossy()))
                }
                "--new-window" => result.new_window = true,
                "--search" => {
                    search.text = value(&mut args, &arg)?;
                    has_search = true;
                }
                "--reveal" => reveal = Some(value(&mut args, &arg)?),
                "--open" => open = Some(value(&mut args, &arg)?),
                "--match-mode" => {
                    search.mode = Some(match value(&mut args, &arg)?.as_str() {
                        "start-of-words" => MatchMode::StartOfWords,
                        "start-of-name" => MatchMode::StartOfName,
                        "anywhere" => MatchMode::Anywhere,
                        "whole-words" => MatchMode::WholeWords,
                        "regex" => MatchMode::RegularExpression,
                        _ => return Err(Error::Value(arg.into_owned())),
                    });
                }
                "--match-case" | "--match-path" => {
                    let on = match value(&mut args, &arg)?.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return Err(Error::Value(arg.into_owned())),
                    };
                    if arg == "--match-case" {
                        search.case = Some(on);
                    } else {
                        search.path = Some(on);
                    }
                }
                "--sort" => {
                    search.sort = Some(match value(&mut args, &arg)?.as_str() {
                        "name" => SortKey::Name,
                        "path" => SortKey::Path,
                        "extension" => SortKey::Extension,
                        "size" => SortKey::Size,
                        "date-modified" => SortKey::DateModified,
                        "date-created" => SortKey::DateCreated,
                        _ => return Err(Error::Value(arg.into_owned())),
                    });
                }
                "--sort-direction" => {
                    search.direction = Some(match value(&mut args, &arg)?.as_str() {
                        "ascending" => SortDir::Asc,
                        "descending" => SortDir::Desc,
                        _ => return Err(Error::Value(arg.into_owned())),
                    });
                }
                _ => {}
            }
        }
        let modifiers = search.mode.is_some()
            || search.case.is_some()
            || search.path.is_some()
            || search.sort.is_some()
            || search.direction.is_some();
        if modifiers && !has_search {
            return Err(Error::SearchRequired);
        }
        let commander = reveal.is_some() || open.is_some();
        // A delete runs only in the first window, which owns the file
        // operations.
        if commander && (has_search || location.is_some())
            || reveal.is_some() && open.is_some()
            || result.sign_in && (has_search || commander || result.new_window)
            || delete_list.is_some() && (result.sign_in || result.new_window)
            || result.compare.is_some()
                && (has_search
                    || commander
                    || location.is_some()
                    || result.sign_in
                    || delete_list.is_some())
        {
            return Err(Error::Conflict);
        }
        result.delete = delete_list.map(|list| DeleteStart {
            list,
            keeps_administrator,
        });
        result.request = if has_search {
            search.location = location;
            Some(Request::Search(search))
        } else if let Some(path) = reveal {
            Some(Request::Reveal(path))
        } else if let Some(folder) = open {
            Some(Request::Open(folder))
        } else {
            location.map(Request::Location)
        };
        if let Some(request) = &result.request {
            request.validate()?;
        }
        Ok(result)
    }
}

fn value(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String, Error> {
    args.next()
        .ok_or_else(|| Error::Missing(option.to_owned()))?
        .into_string()
        .map_err(|_| Error::Value(option.to_owned()))
}

/// Restores a quoted drive root: Windows reads the final `\"` in `"C:\"`
/// as a quote and passes `C:"`, so the root needs its backslash back.
fn legacy_location(argument: &str) -> String {
    match argument.strip_suffix('"') {
        Some(drive) => format!(r"{drive}\"),
        None => argument.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Arguments, Error> {
        Arguments::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn setup_choices_are_a_standalone_startup_action() {
        let arguments = parse(&[SETUP_ARGUMENT, "1", "1", "1", "Ctrl+Alt+G"]).unwrap();
        let choices = arguments.setup.unwrap();
        assert_eq!(choices.sign_in, Some(true));
        assert_eq!(choices.keep_running, Some(true));
        assert_eq!(choices.disk_warning, Some(true));
        assert_eq!(
            serde_json::to_string(&choices.shortcut).unwrap(),
            "\"Ctrl+Alt+G\""
        );
        assert_eq!(
            parse(&[SETUP_ARGUMENT, "keep", "keep", "keep", "keep"])
                .unwrap()
                .setup,
            Some(SetupChoices {
                sign_in: None,
                keep_running: None,
                disk_warning: None,
                shortcut: None,
            })
        );
        let independent = parse(&[SETUP_ARGUMENT, "0", "0", "1", "Win+S"])
            .unwrap()
            .setup
            .unwrap();
        assert_eq!(independent.sign_in, Some(false));
        assert_eq!(independent.keep_running, Some(false));
        assert_eq!(independent.disk_warning, Some(true));
        assert_eq!(independent.shortcut.flatten().unwrap().name(), "Win+S");
        assert!(matches!(
            parse(&[SETUP_ARGUMENT, "1", "1", "1", "-", "--sign-in"]),
            Err(Error::Conflict)
        ));
    }

    #[test]
    fn removing_settings_is_standalone_and_cannot_start_another_action() {
        assert_eq!(
            parse(&[CLEANUP_ARGUMENT]).unwrap().cleanup,
            Some(Cleanup::Desktop)
        );
        assert_eq!(
            parse(&[USER_CLEANUP_ARGUMENT]).unwrap().cleanup,
            Some(Cleanup::User)
        );
        for option in [CLEANUP_ARGUMENT, USER_CLEANUP_ARGUMENT] {
            assert!(matches!(
                parse(&[option, "--sign-in"]),
                Err(Error::Conflict)
            ));
        }
    }

    #[test]
    fn reading_setup_settings_does_not_open_a_window_or_accept_other_actions() {
        let arguments = parse(&[SETUP_READ_ARGUMENT, "Win+S"]).unwrap();
        assert_eq!(
            arguments.setup_read.map(ShowShortcut::name).as_deref(),
            Some("Win+S")
        );
        assert!(arguments.request.is_none());
        assert!(matches!(
            parse(&[SETUP_READ_ARGUMENT, "Win+S", "--sign-in"]),
            Err(Error::Conflict)
        ));
        assert!(matches!(
            parse(&[SETUP_READ_ARGUMENT]),
            Err(Error::Missing(_))
        ));
        assert!(matches!(
            parse(&[SETUP_READ_ARGUMENT, r"C:\Temp\setup.ini"]),
            Err(Error::Value(_))
        ));
    }

    /// Setup decodes these bits, so a change here breaks every installer
    /// that reads them.
    #[test]
    fn setup_answer_sets_one_bit_for_each_saved_choice() {
        let win_s = ShowShortcut::from_name("Win+S").unwrap();
        let mut choices = Choices {
            keep_running: false,
            show_shortcut: None,
            ..Choices::default()
        };
        choices.wear_alarm.line_mut(WearLine::DiskWeek).on = false;
        assert_eq!(setup_answer(&choices, false, win_s), 256);
        choices.show_shortcut = Some(win_s);
        assert_eq!(setup_answer(&choices, true, win_s), 256 + 1 + 8);
        choices.keep_running = true;
        choices.wear_alarm.line_mut(WearLine::DiskWeek).on = true;
        choices.show_shortcut = ShowShortcut::from_name("Ctrl+Alt+G");
        assert_eq!(setup_answer(&choices, false, win_s), 256 + 2 + 4 + 16);
    }

    #[test]
    fn explicit_search_keeps_off_values_and_round_trips_the_handoff() {
        let arguments = parse(&[
            "--search",
            "report 2026",
            "--location",
            r"C:\docs",
            "--match-mode",
            "anywhere",
            "--match-case",
            "off",
            "--match-path",
            "on",
            "--sort",
            "size",
        ])
        .unwrap();
        let request = arguments.request.unwrap();
        assert_eq!(
            request,
            Request::Search(Search {
                text: "report 2026".into(),
                location: Some(r"C:\docs".into()),
                mode: Some(MatchMode::Anywhere),
                case: Some(false),
                path: Some(true),
                sort: Some(SortKey::Size),
                direction: None,
            })
        );
        assert_eq!(Request::decode(&request.encode()), Some(request));
        assert!(Request::decode(r#"{"request":"reveal","value":4}"#).is_none());
        assert!(Request::decode(r#"{"request":"reveal","value":"\u0000"}"#).is_none());
        assert!(Request::decode(&" ".repeat(MAX_HANDOFF_BYTES)).is_none());
    }

    /// `--compare` takes two paths and stands alone.
    #[test]
    fn compare_takes_two_files_and_nothing_else() {
        let arguments = parse(&["--compare", r"C:\a.txt", r"D:\b.txt"]).unwrap();
        assert_eq!(
            arguments.compare,
            Some([PathBuf::from(r"C:\a.txt"), PathBuf::from(r"D:\b.txt")])
        );
        assert_eq!(
            parse(&["--compare", r"C:\a.txt"]).err(),
            Some(Error::Missing("--compare".into()))
        );
        assert_eq!(
            parse(&["--compare", "a", "b", "--search", "x"]).err(),
            Some(Error::Conflict)
        );
    }

    #[test]
    fn startup_and_handoff_reject_the_same_invalid_requests() {
        for (args, request, option) in [
            (
                vec!["--location", ""],
                Request::Location(String::new()),
                "--location",
            ),
            (
                vec!["--reveal", ""],
                Request::Reveal(String::new()),
                "--reveal",
            ),
            (
                vec!["--search", "\0"],
                Request::Search(Search {
                    text: "\0".into(),
                    ..Default::default()
                }),
                "--search",
            ),
            (
                vec!["--search", "report", "--location", ""],
                Request::Search(Search {
                    text: "report".into(),
                    location: Some(String::new()),
                    ..Default::default()
                }),
                "--location",
            ),
        ] {
            assert_eq!(parse(&args).err(), Some(Error::Value(option.into())));
            assert!(Request::decode(&request.encode()).is_none());
        }
    }

    #[test]
    fn request_errors_do_not_open_an_unrequested_search() {
        assert!(parse(&[]).unwrap().request.is_none());
        assert_eq!(
            parse(&["--match-case", "off"]).err(),
            Some(Error::SearchRequired)
        );
        assert_eq!(
            parse(&["--search"]).err(),
            Some(Error::Missing("--search".into()))
        );
        assert_eq!(
            parse(&["--search", "a", "--reveal", "b"]).err(),
            Some(Error::Conflict)
        );
        assert_eq!(
            parse(&["--reveal", "a", "--open", "b"]).err(),
            Some(Error::Conflict)
        );
        assert_eq!(
            parse(&["--new-window", "--sign-in"]).err(),
            Some(Error::Conflict)
        );
        assert!(matches!(
            parse(&["--search", ""]).unwrap().request,
            Some(Request::Search(_))
        ));
        assert_eq!(
            parse(&["--location", "C:\""]).unwrap().request,
            Some(Request::Location(r"C:\".into()))
        );
    }

    proptest::proptest! {
        #[test]
        fn malformed_handoffs_never_panic(text in ".{0,2048}") {
            let _ = Request::decode(&text);
        }
    }
}
