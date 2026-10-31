# CLI gaps worth considering for AI use

Reviewed 2026-10-31 against the working tree and the six official Everything
references below. This is a decision aid, not an implementation plan. No new
behavior is authorized by this document.

The owner subsequently authorized seven additions identified here. Their
implementation is now in the working tree and recorded below; final verification
belongs to the lead's integration work, not this research pass.

**There are useful index-only gaps. They were not all refused.** The earlier
review established that basic AI file discovery works; it did not establish
that every useful, inexpensive improvement was implemented. Below, **absent**
means no decision against it was found; **deferred** and **preserved** refer to
recorded product decisions. Recommendations are the reviewer's assessment.

All costs below are estimates from code, not benchmarks. Adding no disk reads
does not imply cheap execution: a whole-index duplicate-grouping pass, repeated subtree
walks, or building every full path can still be expensive.

## What this work added or improved

| Item | Current result | What changed |
| --- | --- | --- |
| `-full-path`, `-s`, `-w` | Full-path column, ascending Path sort, whole-word matching | New spellings of existing operations, without a second implementation |
| `--server-version` | Connected service identity; JSON string with `--json` | New diagnostic, separate from the existing offline client `--version` |
| `wfn:` and its whole-name aliases | Whole-name wildcard or anchored-regex search | New search function; existing name terms retain their meaning |
| Calendar date words | This/last/next week, month and year; tomorrow; week durations | Extended the existing timestamp resolver; regional week start is respected |
| Recent durations | `dm:Nminutes`, `dm:Nhours`, including the existing `last` prefix; also available to `dc:` | Uses existing timestamps; no event history or new metadata collection |
| Unknown metadata | `size:unknown`, `dm:unknown`, `dc:unknown` | Explicit missing-metadata tests; `dc:` still requires Dates created to be enabled |
| Name/path aliases | `name:` shares `nopath:`; `full-path:` shares `path:` | Familiar spellings, with one implementation and unchanged matching rules |
| Length diagnostics | `name-length:` / `namelen:` and `full-path-length:` / `fullpathlen:`, with numeric comparisons and ranges | Counts UTF-16 code units, including the mount prefix for full paths; not bytes or Unicode scalars |
| Exact extension spelling | `ext:*.rs` means the existing `ext:rs` | Normalizes only the leading `*.` prefix; general extension wildcard patterns such as `ext:c*` remain absent |
| Immediate-child search | `child:text` finds folders with an immediate visible child matching the name test | Uses index child chains; substring matching or whole-name `*`/`?` wildcards, without a disk walk |
| Query grouping | `<...>`, including negation and `\|` combinations | Bounded to depth 32 and the existing 256-term limit; OR still binds more tightly than AND |
| `--json` | Readable local dates beside FILETIME, creation dates when available, truthful `more` | Existing option improved; not a new JSON protocol or new metadata collection |
| `--json -n 0` | Count and coverage without result rows | Existing capability made discoverable, not a new option |
| `gozler-ui --search` and match/sort modifiers | Explicit temporary Search request; `--location` supplies scope | New automation entry, sharing normal Search behavior and saved-preference ownership |
| `gozler-ui --reveal`, `--new-window` | Visible unmarked Commander cursor; optional separate window | New window integration; no new indexing |
| Help and pipelines | Adaptive detailed help with approved semantic columns, corrected scope/attribute explanations; closed readers stop output work | Presentation/documentation and error-propagation fixes; help verification is owned separately from this research |

Implementation: [option table](../../crates/gozler-core/src/escompat/argv.rs#L457),
[query functions](../../crates/gozler-core/src/query/parse.rs#L405),
[dates](../../crates/gozler-core/src/query/dates.rs#L146),
[JSON writer](../../crates/gozler-core/src/output/results.rs#L96),
[JSON guide](../JSON.md), and [window requests](../DESIGN.md#window-lifetime).

## Useful gaps using the data already indexed

The names in this table come from the official [search functions][functions],
[modifiers][modifiers], [syntax][syntax], and [CLI reference][cli]. Their
implementation cost and recommendation are Gozler-specific source analysis.

| Missing item | AI task gained / current workaround | Cost and current reason | Assessment |
| --- | --- | --- | --- |
| `end-with:`; word-end `suffix:` | Express ending tests without composing a regex. Existing regex handles either task; a wildcard handles a literal filename ending. | **Small-to-moderate.** Could reuse existing matchers, but word endings differ from filename endings. **Absent**; regex overlap, not a measured performance issue. | Low priority for AI; do not add both merely for parity. |
| `stem:` | Find related names across extensions. Current workaround is a name regex with correct file/folder handling. | **Small-to-moderate matching work**, borrowed name slicing possible; no extra index field required. **Absent**, not refused. | Useful when an agent frequently groups source/output pairs; otherwise regex is enough. |
| `parent-count:` / `depth:` | Limit how deep an agent explores a tree. Current `-parent` covers one level; deeper limits need path regex or post-filtering. | **Moderate ancestry integration.** Cache depths and define mounted/link paths; repeated parent walks per row would be wasteful. **Absent**. | Plausible value, after recent-time and metadata diagnostics. |
| `location:` / `parent-name:` | Find entries by their containing folder without accidentally matching their own basename. Current path regex or JSON post-filtering works. | **Small-to-moderate** reuse of parent/name/path owners; full-path assembly can dominate. **Absent**. | Useful only when existing `-path`, `-parent` and `path:` do not express the task clearly. |
| More date inputs: year/month shorthand, date/time parts | Simpler calendar queries. Explicit full-date ranges already cover years/months; JSON permits time-part filtering. | **Small-to-moderate parsing**, timezone/calendar boundaries need care; no new reads. **Absent**. | Lower priority than the recent-duration queries now added. |
| `-no-case`, `-no-match-path`, `-no-whole-words`, corresponding column removals | Compose reusable argument lists and override earlier choices. Current fresh CLI calls have explicit defaults; `case:0` overrides case. UI requests already have explicit on/off. | **Small argv work for simple toggles**, with last-wins rules. **Absent**; generic toggle compatibility was not implemented. | Low priority unless wrapper composition needs it. Do not copy UI switches into the search CLI accidentally. |
| `-sort attributes` | Group existing attribute values. Current JSON post-sort works only on fetched rows. | **Cheap key read, normal sorting cost; moderate contract change.** New `SortKey` affects protocol, comparators and snapshot sorting. **Absent**, not rejected for disk cost. | Low AI value compared with existing name/path/size/date sorts. |
| `extension:` | Substring/modified matching on just the extension. Current `ext:` lists or name regex cover ordinary tasks. | **Small-to-moderate predicate**, no extra reads. **Absent**. It is **not an alias of `ext:`**: their matching rules differ. | Low priority; exact extension lists already solve most agent discovery. |
| General wildcard extension patterns, such as `ext:c*` | Discover extension families without enumerating them. Current explicit extension lists or a name regex cover the task. | **Moderate matcher work**, no new index field or reads; preserve file-only extension rules and share any changed query representation across the pipe. **Absent**, not refused. | Remains lower priority after the delivered leading `*.` normalization. This is a new matching capability, not just stripping a prefix. |
| Additional numeric spellings (`==`, `!=`, hyphen ranges), date-time literals | More flexible generated filters; existing comparison/negation and `..` ranges cover many cases. Exact time-of-day bounds currently need JSON post-filtering. | **Small-to-moderate parsing**, with signed values, unknown metadata and dates requiring care. **Absent**. Existing exact bare-size meaning is preserved. | Low priority for extra spellings; full timestamp bounds have more potential than redundant operators. |

Source boundaries: the complete current [function table](../../crates/gozler-core/src/query/parse.rs#L405)
and [operand dispatcher](../../crates/gozler-core/src/query/parse.rs#L894)
distinguish delivered functions from remaining gaps, rather than relying on
help wording alone. The
[date resolver](../../crates/gozler-core/src/query/dates.rs#L146) shows the
supported duration units. [EntryView](../../crates/gozler-core/src/index/volume.rs#L2022)
holds names, parents, file IDs, attributes and optional timestamps/sizes.
[DirPaths](../../crates/gozler-core/src/query/paths.rs#L1) explains path-cache
cost; [SortKey](../../crates/gozler-core/src/types.rs#L91) has no attribute sort.
[SearchRequest](../../crates/gozler-core/src/proto/types.rs#L136) sends the
parsed query, so a new query representation is a shared contract change.
Unknown values still fail ordinary positive and negated numeric/date tests;
the explicit [unknown predicates](../../crates/gozler-core/src/query/engine.rs#L877)
now provide the intended query. Current
[`ext:` parsing](../../crates/gozler-core/src/query/parse.rs#L1026) normalizes the
leading prefix, while [extension membership](../../crates/gozler-core/src/query/engine.rs#L891)
still uses exact values rather than a general wildcard matcher.

## Index-only features that are not small switches

These do not need content reads, but the performance and correctness work is
real. They should not be grouped with the aliases above.

| Missing item | Current workaround | Cost / why it is not already included | Assessment |
| --- | --- | --- | --- |
| Per-term modifier chains, such as mixed case-sensitive and insensitive terms | Separate queries; regex inline flags can cover some name tests. | **Absent**. Current `case:` is a whole-query, last-wins directive. General chains need scoped matching settings and composition rules. | Potentially useful; add only concrete combinations, preserving existing global `case:` behavior. |
| `empty:` and child counts | `-parent` plus count/inspection for a known folder. `child:` now covers immediate-child name discovery, such as project-root queries. | **Absent**. Child chains exist, so no disk walk is necessary. Counts and emptiness still need permission-aware definitions and truthful coverage. Hidden/excluded children complicate what “empty” means. | Separate from delivered `child:` matching. **`size:0` is not an empty-folder test.** |
| Descendant/ancestor/sibling predicates, counts and `exists:` substitutions | Scoped searches and agent-side joins over returned paths. | **Absent**. Naive per-folder subtree scans approach quadratic work; joins/substitution need bounded evaluation and visibility rules. | Revisit narrowly for demonstrated tasks; not the whole relationship language. |
| `dupe:`, `distinct:`, `unique:` on names/sizes | Fetch bounded scope and group it in the caller; enlarge the limit when complete grouping is required. | **Absent**. Needs whole-candidate grouping, roughly O(N) hash state or O(N log N) sort; memory grows with candidates. Name/size duplicates do not prove equal contents. | Optional batch-analysis feature, not free because it is index-only. Content-hash variants are out of scope below. |
| `-viewport-offset` / `-viewport-count` | Narrow scope/filters and use `-n`; fetch all only when justified. | **Explicitly deferred**. Protocol offset already exists. Each independent invocation can rescan/sort, and files can change between pages; stable cross-command paging needs an explicit snapshot lifecycle. | Moderate value, moderate semantics. Do not promise stable paging by exposing an integer alone. |
| JSON field projection | Existing table column options; consume only needed JSON fields; plain output already returns paths alone. | **Absent; an AI-oriented candidate, not a claim about ES's `-add-columns`.** No new metadata collection for current fields, but projection must share existing request flags and writer; table and JSON defaults must stay stable. | Useful only if output/token volume is a demonstrated problem. Keep one JSON writer. |
| `filelist:` or a literal query-tail option | Existing OR expressions and `-r PATTERN`; a host can regex-escape an exact literal name. | **Absent**. New parsing/escaping contract. Everything's `-search*` affects command-line consumption; it does not magically make the inner query a literal filename. | Consider a narrowly defined literal interface only for a real escaping failure; preserve bare `--`. |

Delivered grouping retains [bounded distribution](../../crates/gozler-core/src/query/parse.rs#L751)
and adds a [depth limit](../../crates/gozler-core/src/query/parse.rs#L1277);
its former deferral no longer describes the authorized scope. Remaining
relationship costs are supported by
[child chains](../../crates/gozler-core/src/index/store.rs#L806),
[subtree walk](../../crates/gozler-core/src/index/volume.rs#L1860),
[visibility check](../../crates/gozler-core/src/index/volume.rs#L647), and
[row/snapshot/offset ownership](../../crates/gozler-core/src/query/rows.rs#L1).
Deferrals are recorded in [Not planned](../DESIGN.md#not-planned).

## Outside the agreed boundary: additional file/content reads

This section is the scope exclusion the owner requested. It is not a reason
to reject the index-only features above. The official [functions][functions]
and [modifiers][modifiers] expose these families; Gozler has no corresponding
query/index owner and the owner excluded content/media indexing.

| Family | Why excluded |
| --- | --- |
| `content:` and text/hex/binary/content-line searches | Open and read file contents; `rg` is the existing tool for this task. |
| `sha256:`, `md5:`, CRC and true content duplicates | Read file bytes, potentially entire files; persistent hashes add invalidation and storage. |
| Audio/video/photo/document metadata, executable resources | Require format parsing, property handlers or extra file reads: duration, dimensions, EXIF, authors, product versions, and similar properties. |
| Archive/container contents | Read and parse archive contents; names inside an archive are not filesystem entries in the current index. |
| `from-disk:` and disk-verification variants | Deliberately bypass the index and introduce per-candidate I/O/waits. |

## More metadata is a separate category from reading contents

These should not be dismissed as “would have to read the contents”:

| Item | Actual boundary and disposition |
| --- | --- |
| Accessed/changed dates | Filesystem metadata, not media/content. Accessed dates are **explicitly rejected** because Windows may not keep them updated. Changed dates are **absent**, not covered by that same rejection; retaining/updating another date costs index memory and protocol work. |
| `file-id:` / `parent-file-id:` | IDs already exist internally. Exposing/filtering them is index-only, but needs volume identity, 128-bit ReFS handling, visibility and a public contract. **Absent**; low ordinary AI value. |
| Hard-link counts/names | Current index already maintains file-ID link chains. Visible indexed names can be derived; they are not necessarily the filesystem's full link count when names are excluded or hidden. **Absent**; clarify scope rather than silently claiming a complete OS count. |
| Allocated size / streams / other filesystem properties | May be filesystem metadata, but the current public entry model does not retain these values. Need collection, refresh and storage design; do not treat them as a free output column. |
| Owner/type/MIME/cloud-state properties | Some derive from registry or security metadata, not contents; others need external providers. Existing permission descriptors decide visibility, not a public owner-name property. No new per-result shell/provider calls are justified for AI discovery. |

Evidence: [index entry fields](../../crates/gozler-core/src/index/volume.rs#L2022),
[raw listing timestamps](../../crates/gozler-core/src/layout/listing.rs#L24),
[file IDs](../../crates/gozler-core/src/types.rs#L10), and
[product exclusions](../DESIGN.md#not-planned).

## Inexpensive omissions that need no new AI capability

| Item | Actual reason / alternative |
| --- | --- |
| `-json`, `-export-json` | Cheap wrappers, but `--json` already exists and redirects normally. The prior review **recommended leaving these out**, not because of search performance, but to avoid implying ES's JSON shape and adding another output path. No owner ban was found. |
| Column colors/widths, highlighting, human size/date formats, pager/footer | Formatting work rather than disk work. JSON/CSV/TSV and existing counts already give agent-readable values. Format menus/paging are deferred; the rest were not adopted, not proven too slow. |
| `-add-columns` | ES display-column convenience, not a generic JSON field-projection contract. Gozler already has individual table-column options; another list syntax adds little AI capability. Absent, not rejected for performance. |
| Bookmarks, macros, extension-category shortcuts | Deferred product features. Agents can send explicit extension lists and queries without saved mutable definitions. Cheap aliases alone do not justify a second vocabulary. |
| Run/search history, journal/watch APIs | Need history or event retention and lifecycle contracts, although not file-content indexing. A daemon that consumes USN records does not already expose a durable client change log. |
| Saved CLI defaults, Everything instances/IPC/database controls | Existing service/client architecture and explicit invocations serve the task. Everything-specific control switches are not automatically meaningful for Gozler. |
| Character entities and parser-precedence overrides | Existing quoting and escaped regex values cover literal/operator searches. New entities and custom precedence add grammar choices; they are absent, not rejected for I/O or measured slowness. |
| Extra window geometry/view/sidebar controls | The new Search/Reveal/new-window requests cover the agreed handoff tasks. Copying Everything's UI-control surface adds no demonstrated AI discovery capability. |
| Diacritic/punctuation/whitespace, Pinyin/Soundex transforms | No disk reads, but extra comparison/normalization work and matching semantics. Accent preservation is deliberate; the others are absent rather than explicitly banned. Existing regex covers some needs. |
| `root:` and other tiny structural conveniences | The index knows its root already. This is absent, not a metadata limitation; no clear additional everyday AI task justifies prioritizing it over folder scopes. |

Preserved contracts are not gaps to fix: bare `gozler` shows the overview;
default results stay limited to 20; CLI name terms remain unordered; bare `--`
is search text; `size:1kb` stays exact 1,024 bytes; count switches stay scalar
even with `--json`; established exits and export precedence stay unchanged.
Likewise, Everything 1.5's `*` versus `**` path rule is not a mandate to change
Gozler's existing wildcard matcher, which lets `*` cross separators and
collapses repeated stars. Any change to those meanings needs the owner first.

## Decisions after this review

The seven authorized additions are now listed under delivered scope: recent
durations, unknown metadata, name/path aliases, length diagnostics, leading
`*.` extension normalization, immediate-child matching and bounded grouping.
This closes those original recommendations; they are not outstanding gaps.

Remaining candidates need a demonstrated task before more surface is added.
Depth limits, containing-folder queries and timestamp bounds can help scoped
automation, but their costs and existing workarounds remain in the tables.
`stem:`, word-end modes, general extension wildcards, extra aliases such as
`len:`, and formatting switches are lower priority. No further behavior is
authorized by this assessment.

## Recent changes: useful, but not a small date alias

Everything's [Recent Changes][recent] tracks changed entries since startup;
`rc:` / `recentchange:` searches that history, and an optional USN-journal
backfill extends coverage before startup. It also supports a live recent-change
sort. This is different from `dm:`, which tests a file's modified timestamp.

| Question | Gozler assessment |
| --- | --- |
| Does it help AI work? | Yes, for asking what changed after a task, including renames or attribute changes that a modified-time query may miss. It is less necessary for ordinary file discovery. |
| Does live tracking require reading file contents? | No. The daemon already consumes filesystem journal changes. This belongs with new index/history state, not the excluded media/content features. |
| Is the timestamp already exposed? | No. Raw USN records contain it, but `ParsedUsnRecord` deliberately drops it; `EntryTable` holds modified/optional created dates, not recent-change dates. |
| What implementation is required? | Retain a defined change timestamp, maintain it through index updates, and add query/output/sort contracts. Define restart, rescan, journal-gap and ReFS/snapshot coverage before exposing an answer as complete. |
| What adds I/O? | Backfilling older journal events is extra journal reading. It is not a file-content scan, but it adds work beyond processing the stream already needed for current indexing. |
| Is this a full incremental event API? | No. A list of currently indexed entries with last-change times cannot by itself report every deletion or every step of a rename sequence. A durable create/delete/rename feed is a separate retention/cursor contract. |
| Should we add it now? | **Defer pending a concrete incremental-monitoring task.** Useful, not bloat by definition, but not a cheap parser addition. The newly added minute/hour `dm:` durations cover recent content edits; they are not a substitute for change tracking. |

Evidence: [journal tail](../../crates/gozler-daemon/src/tail.rs#L1),
[decoded USN record](../../crates/gozler-core/src/usn/record.rs#L64),
[raw journal timestamps](../../crates/gozler-core/src/usn/record.rs#L188), and
[stored entry fields](../../crates/gozler-core/src/index/store.rs#L788).
The USN timestamp comment explicitly distinguishes journal-write time from
the file's modified time. Cost is a source-level estimate; no memory layout,
retention policy or runtime measurement is proposed here.

## Sources and verification boundary

- [Everything CLI reference][cli]: search, sort, output, export and general options.
- [Everything program options][options]: window launching/control; not the ES CLI.
- [Everything search syntax][syntax]: operators, quoting, wildcards and entities.
- [Everything modifiers][modifiers]: term control and transformation families.
- [Everything functions][functions]: properties and index-derived relationships.
- [Everything recent changes][recent]: startup history and optional journal backfill.
- Gozler authorities linked above, plus [Command line](../DESIGN.md#command-line)
  and [JSON](../JSON.md); prior audit at `target/cli-reference-pass.md` was
  checked against the current code rather than treated as an implementation record.

This is a source/documentation comparison, not a runtime or performance test.
No builds, probes, services or UI inspections were run. The check covers the
six references by feature family and the AI-relevant gaps above; it does not
promise every Everything 1.5 property, parser edge or optional spelling is
compatible. No independent review of this new report is claimed.

[cli]: https://www.voidtools.com/support/everything/command_line_interface/
[options]: https://www.voidtools.com/support/everything/command_line_options/
[syntax]: https://www.voidtools.com/support/everything/search_syntax/
[modifiers]: https://www.voidtools.com/support/everything/search_modifiers/
[functions]: https://www.voidtools.com/support/everything/search_functions/
[recent]: https://www.voidtools.com/support/everything/recent_changes/
