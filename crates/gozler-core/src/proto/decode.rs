//! Reading a request from a client of this build or of a newer one.
//!
//! A request first decodes into the types. Only when that fails does
//! [`decode_request`] read it again one part at a time, so the daemon runs
//! what it can read of a newer client's request: a query term or a [`Want`]
//! that does not decode is left out and named, and any other part that does
//! not decode refuses the request with the part's path. Each name comes from
//! the place in the JSON where the decode failed, so this module keeps no
//! second list of the names the daemon knows.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::{Request, Returns, Rows, Want};
use crate::query::engine::MAX_QUERY_TERMS;
use crate::query::{ParsedQuery, QueryError, Test};
use crate::types::SortSpec;

/// A request as the daemon reads it.
#[derive(Debug, PartialEq)]
pub struct Decoded {
    pub request: Request,
    /// The names of the query terms and wants left out of `request`, each
    /// once, in the order the request held them; empty for a request that
    /// decoded whole.
    pub skipped: Vec<String>,
}

/// Why the daemon cannot run a request.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The frame is not JSON, or not a JSON object with a `t` field, or a
    /// query term in it names no test, which no daemon reads.
    #[error("malformed request: {0}")]
    Malformed(String),
    /// A part that only a newer daemon reads; carries the part's path, such
    /// as `Search.returns` or the kind of an unknown request.
    #[error("{0}")]
    Unknown(String),
    /// The query holds more terms than a query may
    /// ([`QueryError::TooManyTerms`]).
    #[error(transparent)]
    Query(#[from] QueryError),
}

/// A part of a request at a JSON pointer, and whether a value decodes as it.
type Part = (&'static str, fn(&Value) -> bool);

/// The parts of a search that refuse it when they do not decode, the
/// narrowest first, so the refusal names the part that failed.
const SEARCH_PARTS: [Part; 6] = [
    ("/returns/Rows/sort", decodes::<SortSpec>),
    ("/returns/ResultSnapshot/rows/sort", decodes::<SortSpec>),
    ("/returns/Rows", decodes::<Rows>),
    ("/returns/ResultSnapshot/rows", decodes::<Rows>),
    ("/returns", decodes::<Returns>),
    ("/query", decodes::<ParsedQuery>),
];

/// As [`SEARCH_PARTS`], for a sort of a result snapshot.
const SORT_PARTS: [Part; 2] = [
    ("/rows/sort", decodes::<SortSpec>),
    ("/rows", decodes::<Rows>),
];

/// Decodes one request frame payload, leaving out each query term and want
/// that does not decode.
///
/// # Errors
///
/// [`DecodeError::Malformed`] for a frame that is not a JSON object with a
/// `t` field or that holds a query term with no test,
/// [`DecodeError::Query`] for a query with more terms than a
/// query may hold, and [`DecodeError::Unknown`] for any other part that does
/// not decode.
pub fn decode_request(frame: &[u8]) -> Result<Decoded, DecodeError> {
    if let Ok(request) = serde_json::from_slice(frame) {
        return Ok(Decoded {
            request,
            skipped: Vec::new(),
        });
    }
    let mut request: Value =
        serde_json::from_slice(frame).map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let kind = (request.get("t").and_then(Value::as_str))
        .ok_or_else(|| DecodeError::Malformed("no request kind".into()))?
        .to_owned();
    let mut skipped = Vec::new();
    if kind == "Search"
        && let Some(clauses) = request
            .pointer_mut("/query/clauses")
            .and_then(Value::as_array_mut)
    {
        // Counted before any term is left out, so a request cannot fill
        // `skipped` with thousands of names; the compile counts the kept
        // terms exactly.
        let terms: usize = (clauses.iter())
            .map(|clause| {
                let terms = clause.get("terms").and_then(Value::as_array);
                terms.map_or(0, Vec::len).max(1)
            })
            .sum();
        if terms > MAX_QUERY_TERMS {
            return Err(QueryError::TooManyTerms(terms).into());
        }
        let mut malformed = false;
        let clauses = clauses.iter_mut();
        for terms in clauses.filter_map(|clause| clause.get_mut("terms")?.as_array_mut()) {
            // A term whose test decodes stays, also when the term fails,
            // such as one with a field `Term` does not know, for the decode
            // below to refuse. A skipped term takes its negation with it, so
            // the answer only gets smaller.
            terms.retain(|term| {
                let Some(test) = term.get("test") else {
                    malformed = true;
                    return true;
                };
                if decodes::<Test>(test) {
                    return true;
                }
                match variant(test) {
                    Some(name) => {
                        note(name, &mut skipped);
                        false
                    }
                    None => {
                        malformed = true;
                        true
                    }
                }
            });
        }
        if malformed {
            return Err(DecodeError::Malformed("a query term names no test".into()));
        }
    }
    let wants_at: &[&str] = match kind.as_str() {
        "Search" => &["/returns/Rows/wants", "/returns/ResultSnapshot/rows/wants"],
        "ResultSnapshotRows" => &["/wants"],
        "SortResultSnapshot" => &["/rows/wants"],
        _ => &[],
    };
    for at in wants_at {
        if let Some(wants) = request.pointer_mut(at).and_then(Value::as_array_mut) {
            wants.retain(|want| !skip_want(want, &mut skipped));
        }
    }
    match Request::deserialize(&request) {
        Ok(request) => Ok(Decoded { request, skipped }),
        Err(_) => Err(DecodeError::Unknown(failed_part(&kind, &request))),
    }
}

/// Whether `want` fails to decode as a [`Want`] and names the variant it
/// holds; then that name goes on `skipped`, once. The writes want goes as
/// `--writes`, the option that asks for it, because the name `Writes` is
/// also the name of the query test, which [`typed_parts`] would expand.
fn skip_want(want: &Value, skipped: &mut Vec<String>) -> bool {
    let Some(name) = Some(want)
        .filter(|want| !decodes::<Want>(want))
        .and_then(variant)
    else {
        return false;
    };
    note(if name == "Writes" { "--writes" } else { name }, skipped);
    true
}

/// Puts `name` on `skipped`, once.
fn note(name: &str, skipped: &mut Vec<String>) {
    if !skipped.iter().any(|known| known == name) {
        skipped.push(name.to_owned());
    }
}

/// The parts of `query` that a reply skipped ([`Decoded::skipped`]), as the
/// person typed them, for a message: a part that names a query term's test
/// becomes each such term of `query`, typed ([`Test::typed`]), and a part
/// that names no term, a want, keeps its name, or the option that asks for
/// it (`skip_want`).
pub fn typed_parts(query: &ParsedQuery, skipped: &[String]) -> Vec<String> {
    let mut parts = Vec::new();
    for part in skipped {
        let terms = query.clauses.iter().flat_map(|clause| &clause.terms);
        let mut named = terms
            .filter(|term| {
                let test = serde_json::to_value(&term.test).expect("a test serializes");
                variant(&test) == Some(part.as_str())
            })
            .map(|term| {
                format!(
                    "{}{}",
                    if term.negated { "!" } else { "" },
                    term.test.typed()
                )
            })
            .peekable();
        if named.peek().is_none() {
            note(part, &mut parts);
        }
        for typed in named {
            note(&typed, &mut parts);
        }
    }
    parts
}

/// The name of the variant that a serde enum value holds: a unit variant is
/// its name, and every other variant is an object whose one key is its name.
fn variant(value: &Value) -> Option<&str> {
    match value {
        Value::String(name) => Some(name.as_str()),
        Value::Object(fields) if fields.len() == 1 => fields.keys().next().map(String::as_str),
        _ => None,
    }
}

fn decodes<T: DeserializeOwned>(value: &Value) -> bool {
    T::deserialize(value).is_ok()
}

/// The path of the part of `request`, a request of `kind`, that fails to
/// decode: the narrowest part that fails, or the kind itself.
fn failed_part(kind: &str, request: &Value) -> String {
    let parts: &[Part] = match kind {
        "Search" => &SEARCH_PARTS,
        "SortResultSnapshot" => &SORT_PARTS,
        _ => &[],
    };
    let failed = (parts.iter())
        .find(|(at, decodes)| request.pointer(at).is_some_and(|part| !decodes(part)))
        .map_or("", |(at, _)| *at);
    format!("{kind}{}", failed.replace('/', "."))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message names a skipped term as the person typed it, not by its
    /// wire name, which the person never sees; a want keeps its name.
    #[test]
    fn a_skipped_part_is_named_as_typed() {
        let query =
            crate::query::parse_query("report !child:readme ext:md;txt").expect("the query parses");
        let skipped = [
            "Child".to_owned(),
            "Extension".to_owned(),
            "Preview".to_owned(),
        ];
        assert_eq!(
            typed_parts(&query, &skipped),
            ["!child:readme", "ext:md;txt", "Preview"]
        );
    }

    #[test]
    fn decode_rejects_malformed_json() {
        assert!(matches!(
            decode_request(b"not json"),
            Err(DecodeError::Malformed(_))
        ));
    }
}
