//! The one writer of a result set, page by page, for `gozler`'s stdout and
//! `-export-*` files and for the window's Export, so both write the same
//! bytes for the same rows.

use super::{TableOptions, TableWriter, format_date_local};
use crate::proto::{QueryResults, ResultItem};
use crate::query::MatchSums;
use jiff::tz::TimeZone;
use serde::Serialize;
use std::io::{self, Write};

#[cfg(test)]
mod corpus;

/// How a [`ResultWriter`] writes the rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultFormat {
    /// Plain text, CSV, TSV or TXT, through [`TableWriter`].
    Table(TableOptions),
    /// One JSON object at the end: every page, folded with
    /// [`QueryResults::add_page`].
    Json,
}

/// Writes one result set as its pages arrive, so a large export never waits
/// for or holds the whole set. JSON is the exception: it is one object, so it
/// is written when the last page has arrived.
pub struct ResultWriter<W: Write> {
    out: W,
    rows: Rows,
}

enum Rows {
    Table(TableWriter),
    Json {
        pages: Option<QueryResults>,
        time_zone: TimeZone,
    },
}

impl<W: Write> ResultWriter<W> {
    /// Dates are local time in `time_zone`.
    pub fn new(format: ResultFormat, time_zone: TimeZone, out: W) -> Self {
        let rows = match format {
            ResultFormat::Table(options) => Rows::Table(TableWriter::new(options, time_zone)),
            ResultFormat::Json => Rows::Json {
                pages: None,
                time_zone,
            },
        };
        Self { out, rows }
    }

    /// Writes one page, the first with the byte order mark and the header
    /// that the format asks for.
    ///
    /// # Errors
    ///
    /// The error of writing to `out`.
    pub fn page(&mut self, page: QueryResults) -> io::Result<()> {
        match &mut self.rows {
            Rows::Table(table) => {
                let mut bytes = Vec::new();
                table.write_page(&page.items, &mut bytes);
                self.out.write_all(&bytes)
            }
            Rows::Json { pages, .. } => {
                match pages {
                    None => *pages = Some(page),
                    Some(all) => all.add_page(page),
                }
                Ok(())
            }
        }
    }

    /// Writes what the pages left to write, and flushes.
    ///
    /// # Errors
    ///
    /// The error of writing to `out`.
    pub fn finish(mut self) -> io::Result<()> {
        if let Rows::Json {
            pages: Some(all),
            time_zone,
        } = &self.rows
        {
            let json = serde_json::to_string_pretty(&JsonResults::new(all, time_zone))
                .expect("QueryResults holds only numbers, strings and flags, which serialize");
            writeln!(self.out, "{json}")?;
        }
        self.out.flush()
    }
}

// Stdout's `more` describes omitted matches; the pipe's describes frames
// still to come. Dates are readable here without changing the wire format.
#[derive(Serialize)]
struct JsonResults<'a> {
    total: u64,
    items: Vec<JsonItem<'a>>,
    more: bool,
    volumes_incomplete: bool,
    metadata_pending: bool,
    generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sums: Option<&'a MatchSums>,
}

#[derive(Serialize)]
struct JsonItem<'a> {
    #[serde(flatten)]
    item: &'a ResultItem,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created: Option<String>,
}

impl<'a> JsonResults<'a> {
    fn new(all: &'a QueryResults, time_zone: &TimeZone) -> Self {
        let date = |filetime: Option<i64>| {
            filetime
                .map(|value| format_date_local(value, time_zone))
                .filter(|text| !text.is_empty())
        };
        Self {
            total: all.total,
            items: all
                .items
                .iter()
                .map(|item| JsonItem {
                    item,
                    mtime: date(item.mtime_ft),
                    created: date(item.created_ft),
                })
                .collect(),
            more: all.more || all.total > all.items.len() as u64,
            volumes_incomplete: all.volumes_incomplete,
            metadata_pending: all.metadata_pending,
            generation: all.generation,
            sums: all.sums.as_ref(),
        }
    }
}
