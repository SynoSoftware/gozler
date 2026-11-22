//! es.exe-compatible command-line parsing, the request a command line sends,
//! gozler's exit codes, the warnings a finished search ends with, and the
//! help.
//!
//! A pure translation of an argv slice into a [`Command`] (what to search,
//! how to sort, what to output) or an [`EsFatal`] carrying the process
//! [`ExitCode`]. The switch-matching rules are lifted from the voidtools es
//! source: case-sensitive names with every interior dash optional, and
//! `/`-prefixed fallthrough to search text.
//!
//! gozler-native extensions live behind a `--` prefix (`--json`, `--status`,
//! `--insecure-no-server-check`) so they can never collide with es's
//! single-dash switch namespace.
//!
//! The help is generated from the tables the parsers read: the switches and
//! sort keys in `argv`, the search functions in `query::parse`, and the exit
//! codes in [`ExitCode`]. A later switch, function or sort key is one row of
//! its table, and the help lists it.

/// The command people type to search: the one place its name is written for
/// text, so renaming the product changes this line.
pub const CLI_PROGRAM: &str = "gozler";

/// The service's program, which people type in `gozlerd see-every-file`.
pub const SERVICE_PROGRAM: &str = "gozlerd";

mod answer;
mod argv;
#[cfg(test)]
mod corpus;
mod exit;
mod help;
mod request;

pub use answer::{Answer, Warning};
pub use argv::{
    Command, Count, DEFAULT_LIMIT, EsFatal, EsPlan, HelpScreen, Limit, Report, ScopeFolder,
    parse_argv,
};
pub use exit::ExitCode;
pub use help::{help, search_syntax, set_linked, version};
pub use request::build_search;
