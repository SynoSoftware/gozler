# Use Gozler from scripts and AI agents

Use `--json` for search results and index status. Capture stdout, stderr and
the process exit code separately. Stdout contains UTF-8 JSON; warnings and
errors go to stderr. A failed command may produce no JSON.

Run `gozler --help` for the option and search-syntax reference. Running
`gozler` without arguments shows the overview; it does not search.

## Find a bounded set of files

```text
gozler --json -path "C:\repo" -n 50 ext:rs
gozler --json -path "C:\repo" -n 50 wfn:Cargo.toml
gozler --json -path "C:\repo" -n 10 -sort date-modified report
```

`-path` takes the folder literally, including spaces, and searches below it.
Use `-parent` instead to list only its direct children. Without a folder
scope, Gozler searches every indexed volume the caller can access.

The default limit is 20. Choose a limit for the task, then narrow the query
if `more` is true. `-n all` returns every match and holds the JSON result in
memory before writing it. Search terms match in any order; use the phrase
quoting examples in `--help` when spaces belong to one term. A bare `--`
is search text, not an end-of-options marker.

Search uses the existing index of fixed NTFS and ReFS drives. It does not
scan file contents or search unindexed network and removable drives.

Use indexed metadata to narrow an agent's task:

```text
gozler --json -path "C:\repo" dm:10minutes
gozler --json -path "C:\repo" child:Cargo.toml
gozler --json -path "C:\repo" "<ext:*.rs dm:2hours>|wfn:Cargo.toml"
gozler --json -path "C:\repo" "full-path-length:>260"
gozler --json -path "C:\repo" size:unknown
```

`child:` finds folders with an immediate visible child; it does not descend
or read files. Lengths count UTF-16 code units, and full-path lengths include
the mount prefix. `name:` and `full-path:` explicitly choose what to match.
Use `dm:unknown` for missing modified dates, and negate an `unknown` predicate
to require known metadata. Creation-date queries still require **Dates created**.

## Count before fetching rows

```text
gozler --json -path "C:\repo" -n 0 ext:rs
gozler -get-result-count -path "C:\repo" ext:rs
gozler -get-total-size -path "C:\repo" ext:rs
```

The first command returns the search object with an empty `items` array
and the match count in `total`. If anything matches, `more` is true. The
exit code is 10 for a complete answer or 2 if matches may be missing.

The count switches return one integer, even with `--json`. Counts cover
every match regardless of `-n`; size totals sum matching files in bytes,
without adding folders a second time. Check the exit code for completeness.

## Read search results

| Field | Meaning |
| --- | --- |
| `total` | Matching entries, including those omitted by the result limit |
| `items` | The returned files and folders |
| `more` | Matching rows were omitted |
| `volumes_incomplete` | A volume in scope is not ready; matches may be missing |
| `metadata_pending` | A size and date pass failed or stopped; some metadata is unknown |
| `generation` | The index generation reported for this result |

Each item contains `path` and `is_dir`. Optional fields are omitted when
unknown or not requested; absence is different from zero.

| Item field | Meaning |
| --- | --- |
| `path` | Full path |
| `is_dir` | Whether the entry is a folder |
| `size` | Bytes; for a folder, the size of files below it |
| `mtime`, `created` | Local dates as `YYYY-MM-DDTHH:MM:SS`, without a UTC offset |
| `mtime_ft`, `created_ft` | Integer Windows FILETIME ticks: 100 ns since 1601-01-01 UTC |
| `attributes` | Windows attribute bits, requested with `-attributes` |
| `path_u16` | Exact UTF-16 code units when `path` had to replace an unpaired surrogate |

Creation dates depend on the **Dates created** indexing setting. Folder
sizes are omitted for callers whose results are filtered by permissions.
FILETIME values exceed JavaScript's safe integer range; use an integer parser
that preserves their precision when exact timestamps matter.

Ignore fields your consumer does not need, including additional fields added
in later releases. Pass returned paths to file APIs as literal paths rather
than interpolating them into shell commands. Use `path_u16` when present if
the operation needs the exact Windows name.

## Check completeness and service status

| Exit code | What the caller can do |
| --- | --- |
| 0 | Use the answer; an empty result is valid |
| 2 | Use the available answer, but do not claim it is complete |
| 10 | Use the returned rows; narrow the query or raise the limit to get more |
| 9 | No matches, only when `-no-result-error` was requested |
| 1, 3–8 | The command failed; read stderr |

Code 2 takes precedence over code 10. Check `more` as well when the answer
may be incomplete. `metadata_pending` can also accompany code 0 when the
query does not filter or total the missing metadata; the rows are present,
but their sizes and dates may be unknown.

```text
gozler --json --status
gozler --json -path "C:\repo" -timeout 5000 -n 50 ext:rs
```

Status returns an index-status object, not a search-result object. Its
`volumes` describe indexed volumes and their phases, `include` and `left_out`
describe indexing coverage, and `caller_sees` describes the caller's access.
Status exits 2 if any volume is incomplete or its size and date pass failed.

Use `gozler --server-version` to read the running service's name and version
without searching or requesting status. With `--json`, it returns a JSON
string. `gozler --version` still reports the command-line program's version
without connecting to the service. If `--status` and `--server-version` are
both given, the last one wins. Search options do not affect either report.

`-timeout` waits up to the given milliseconds for the volumes in scope to
be ready, then searches anyway. It is not an execution deadline. If an agent
needs a deadline for the whole command, set it in the process runner.

## Show files in the window

```text
gozler-ui --search "ext:rs" --location "C:\repo" --match-mode anywhere --match-case off --match-path off
gozler-ui --reveal "C:\repo\Cargo.toml"
```

The first command prepares Search for the person to use. Explicit match and
sort choices apply to that search without changing saved settings. Search
in the window matches name terms in order; the CLI matches them in any order.

`--reveal` opens the file's folder in Commander mode and puts the cursor on
the file without marking it selected. Add `--new-window` to open a separate
window and leave the current window as it is. These options belong to
`gozler-ui`, not the search CLI. See [Window lifetime](DESIGN.md#window-lifetime)
for the complete handoff contract.
