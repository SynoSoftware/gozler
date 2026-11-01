//! Explicit requests from another program, shared by startup and window handoff.

use super::query::MatchMode;
use gozler_core::types::{SortDir, SortKey};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) const SAVE_ARGUMENT: &str = "--save-include";
pub(crate) const REBUILD_ARGUMENT: &str = "--rebuild-index";
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
        let mut result = Self::default();
        let mut search = Search::default();
        let mut has_search = false;
        let mut location = None;
        let mut reveal = None;
        let mut open = None;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let arg = arg.to_string_lossy();
            match arg.as_ref() {
                "--data-dir" => result.data_dir = args.next().map(PathBuf::from),
                "--sign-in" => result.sign_in = true,
                "--unregister-shortcut" => result.unregister_shortcut = true,
                SAVE_ARGUMENT => result.save_include = args.next().map(PathBuf::from),
                REBUILD_ARGUMENT => result.rebuild_index = true,
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
        if commander && (has_search || location.is_some())
            || reveal.is_some() && open.is_some()
            || result.sign_in && (has_search || commander || result.new_window)
        {
            return Err(Error::Conflict);
        }
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
