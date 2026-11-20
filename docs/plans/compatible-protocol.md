# The pipe protocol grows without a new version

Today every change to a request or a reply raises `PROTO_VERSION`, and the
client and the daemon must have the same version. So each change stops a
newly built window or `gozler` from talking to the installed service until
the service is installed again, which needs an administrator. Protocol 2
came on 18 October 2026 and protocol 21 on 14 November 2026: 20 versions in
27 days.

After this plan, an addition keeps the version. A daemon runs the parts of a
newer client's request that it can read and tells the client what it
skipped, so the client shows what is known and says what is missing. Only a
break raises the version, and breaks are collected so they come rarely.

The owner's target has two parts, and they pull against each other: fewer
breaks, and little legacy code. A break now and then is accepted, so this
plan adds no code whose only use is to avoid a rare break: no revision
numbers, no feature checks in the client, no old layouts, and no fallback
values in the types that the engine and the window use.

## Schedule

**Scheduled:** next protocol change. Stage 1 is itself one break (protocol
22), so it lands before or together with the next change that would raise
the version. It touches `gozler-core`'s `proto` module and query engine,
the daemon's request dispatch, `gozler-client` and how the CLI and the
window report a skipped part. No other plan changes those parts. Not
started.

One implementer does stage 1 and fixes the review's findings; the reviewer
reads and reports.

## Words

**Addition:** a change that a peer one step older can read safely, by the
rules below.

**Break:** every other change to a request, a reply or a results page. It
raises `PROTO_VERSION`.

**Skipped part:** a query term or a requested value that the daemon could
not read, so it answered without it. (This is not `Test::Unknown`, the
existing term that finds entries whose size or date is unknown.)

## What the history shows

| What the version change added | Versions | An addition under this plan? |
|---|---|---|
| Query terms (`parent:`, regex, whole word, word start, `dm:`, length, child, depth, child count, …) | 3, 4, 5, 6, 7, 13, 18, 19, 21 | Yes |
| Requested values (attributes, sums, date created, identity) | 6, 8, 13, 20 | Yes |
| Request kinds (Settings saved, Count included, result snapshots) | 8, 11, 12 | Yes |
| Reply fields (progress, descriptors, `caller_sees`, links, memory) | 14, 15, 16, 17, 21 | Yes |
| An Include setting, which `CountIncluded` sends (`dates_created`) | 13 | No |
| A layout or a meaning replaced: the client parses the query (2), each folder sent once (9), the sums block (10) | 2, 9, 10 | No |

16 of the 20 were additions only. Two of them would still have stopped the
window, because the window's default moved to the new feature: word-start
matching (7) and result snapshots (11). See "Accepted limits".

## Rules for an addition

These rules replace "each change raises `PROTO_VERSION`" in
`docs/DESIGN.md` ("Wire protocol") when stage 1 lands. They rest on one
idea: **a peer may ignore what only adds, and must skip, and report, what
changes the answer.**

1. **A new reply field is an `Option`,** so a missing field reads as
   unknown and the client shows what it knows. `#[serde(default)]` is right
   only where the default is what an older daemon means. A count such as
   `unread_folders` is not such a field: a default of 0 would say that
   every folder is read. An old client already ignores a field it does not
   know.
2. **A new filter is a new `Test` variant,** or a new field of an existing
   test that is sent only when it differs from its default. The daemon
   skips a term it cannot read (stage 1, item 2), and the answer gets
   smaller, never wider. A filter that is a new field of another request
   type is refused by an older daemon (rule 5), because every request type
   except `Hello` and `Status` refuses an unknown field (stage 1, item 3).
3. **A new requested value is a new `Want` variant** (stage 1, item 1). The
   results page sends its field only when the request asked for it, behind
   an item flag bit, or behind a page flag bit for a value of the whole page
   such as sums and identities. So an old client never meets a flag bit it
   does not know. The item flags have 2 free bits and the page flags 3; a
   value past those is a layout break.
4. **A new request field whose absence costs only work,** such as
   `Status { links_held }`, which lets the daemon leave out a table the
   client already has, is an addition. An older daemon ignores it and
   answers in full. This holds only for `Hello` and `Status`; every other
   request type refuses an unknown field (rule 5).
5. **A new request kind, a new `Returns` variant, a new sort key or a new
   field of a request type other than `Hello` and `Status`** is an
   addition. An older daemon refuses that request with
   `ProtoError::UnknownRequest` and the part it could not read (stage 1,
   item 4). A sort key is refused, not skipped, because a search with a
   limit sorted another way returns other rows; a new field is refused
   because ignoring it could change the rows, as a second sort key in
   `SortSpec` would.
6. **A new reply value that only a new request can cause,** such as a
   `ProtoError` variant for a new request kind, is an addition.
7. **Everything else is a break:** a field removed, renamed or given
   another type; a field or a value whose meaning changes; a new variant in
   an enum of a request outside a query term, such as `SortDir`, or of a
   reply, such as `VolumePhase`; a new Include setting or Include setting
   value; and a change to the
   results page other than a new requested value. A break raises
   `PROTO_VERSION` by one. When the owner agrees, wait and put several
   breaks into one version.

## Stage 1: protocol 22

This stage is the last break that additions need. A daemon of version 21
or older cannot skip what it cannot read, so the skipping must be in the
daemon before the rules above work.

1. **One list of wanted values.** `Rows` and `SnapshotRowsRequest` each
   hold the same five `want_*` booleans, and `Rows` has a sixth,
   `want_sums`. Replace them with one `Want` enum and a list of its values:
   size, date modified, date created, attributes, identity and sums. A
   result snapshot read already sends the search's sums on each page
   (`server.rs`, `stream_snapshot`), so sums keep that meaning there.
2. **Decoding skips what it cannot read.** `gozler_core::proto` first
   decodes a request into the types, as today. Only when that fails does it
   decode the request again through a `serde_json::Value`, one part at a
   time: the kind, the `Returns`, the sort, each strict struct of item 3,
   and each query term and each want on its own. A term or a want that
   does not decode is left out and its name goes on the list of skipped
   parts; a term goes with its negation. Any other part that does not
   decode refuses the request (item 4). Each name comes from the place in
   the JSON where the decode failed, never from serde's error text, which
   belongs to a dependency, and never from a second list of the names the
   daemon knows: a term's or a want's name is the variant name it carries,
   and another part's name is its path in the request, such as
   `Search.returns` or `Search.returns.rows.sort`. A request that is not a
   JSON object with a `t` field is malformed (`BadQuery`). Before it leaves
   any term out, this step refuses a query whose clauses hold more terms
   than the engine's `MAX_QUERY_TERMS`, as the compile does
   (`QueryError::TooManyTerms`), so a request cannot fill the skipped list
   with thousands of names. That is a check where untrusted input enters;
   the compile still counts the kept terms exactly. The engine's and the
   window's types (`Test`, `Term`, `SortKey`) get no extra variant, so no
   code that matches on them changes. The daemon, the
   golden corpus (`escompat/corpus.rs`) and the engine tests all decode
   through this one function. A request from a client of the daemon's own
   build decodes in the first step, so it costs nothing more. The second
   step holds a node for each JSON value of the request, at most tens of
   MB for a 1 MiB request, and frees them when the request is decoded.
3. **`deny_unknown_fields`** goes on `Test`'s struct variants and the
   structs it holds, so a known term with a field the daemon does not know
   fails to decode and is skipped. It also goes on every other request
   type except `Hello` and `Status`: `SearchRequest`, `Rows`, `SortSpec`,
   `ParsedQuery`, `Clause`, `Term`, `SnapshotRowsRequest`,
   `SortSnapshotRequest` and the struct variant `Returns::ResultSnapshot`.
   An unknown field there refuses the request (item 4) instead of changing
   the answer without a report. The window also saves a `SortSpec` in
   `ui.json` and reads it with `or_default`, so a field that a newer window
   saved resets the sort to the default, as an unknown sort key already
   does.
4. **An unknown request kind, `Returns` variant, sort key, or field of the
   types in item 3 gets `ProtoError::UnknownRequest`, whose `message` is the
   path of the part (item 2).** A new code is needed because
   `Unsupported` already answers an access refusal:
   a client the daemon cannot identify, and a filtered user who asks for
   the Include count (`server.rs`, `unsupported`). Today the daemon answers
   every request it cannot decode with `BadQuery` "malformed request"
   (`server.rs`, `refusal`), which `gozler` reports as a wrong query. The
   refusal belongs to one request, not to the service: the service still
   answers `Status`. So `gozler` exits it as it exits `ClientError::TooOld`.
   The window keeps the code that `ClientError::Refused` carries, which it
   drops today (`app/work/requests.rs`, `failure`), and shows this refusal
   as a new `DaemonFailure` value beside `Refused`. Like `Refused`, it does
   not recover (`DaemonFailure::recovers`), so it starts no status poll. Its
   own text says that a newer gozlerd runs this search and shows the part
   as detail. It is not `DaemonFailure::TooOld`: that failure starts a
   status poll that searches again when the service answers, which would
   send the same request every poll, and it asks GitHub for the latest
   release.
5. **An emptied clause stays, and matches nothing.** A clause is an OR of
   terms and a query is an AND of clauses (`docs/DESIGN.md`, "Search"), so
   a skipped term only makes the answer smaller:

   | The skipped term is | The answer |
   |---|---|
   | alone in its clause | the clause is empty and matches nothing, so no rows |
   | negated | the same, because the negation goes with the term |
   | one of several ORed terms | the rows the other terms match |

   Every row shown is a true match. So a delete in Commander mode reaches
   only files the search really found, and no command needs a block. The
   emptied clause must stay in the query: a query with no clauses matches
   every entry. `CompiledClause::passes` already returns false for an empty
   clause, and `name_literals` gives it no literals, so the prefilter comes
   from the other clauses and cannot widen the answer. But that scan still
   visits every candidate to reject it, so `scan_with` in `engine.rs`
   returns no records at its start for a query that holds an empty clause.
   Rows, `Returns::Totals` (`run_totals`) and the sums all come from that
   scan. The scope check (`targets()` in `rows.rs`) runs before it, and
   `run_held` reads the coverage flags around it, so an empty answer still
   refuses a scope on no indexed volume and still reports an incomplete
   volume.
6. **The reply says what was skipped.** When the daemon skipped a part, a
   JSON `Response::Skipped { parts }` comes before the answer, as
   `Response::ResultSnapshot` comes before its pages. This holds for every
   `Returns` of a search, for `ResultSnapshotRows` and for
   `SortResultSnapshot`, whose `Rows` carries wants. The client's search
   calls return the skipped parts with the answer. The skipped terms of a
   result snapshot belong to that snapshot: the window keeps the notice
   while it shows the snapshot, because later reads of it decode cleanly
   and report no terms. The binary page does not change.
7. **The clients show a skipped part.** `gozler` writes one stderr line
   that names the skipped parts and says that a newer gozlerd reads them,
   and exits 2, because the matches may exist (`docs/DESIGN.md`, "Exit
   codes"). The window shows the same in one notice next to the one it
   shows for an incomplete answer. Write its text and the text of item 4's
   refusal by `docs/WRITING.md`, in all four languages. Save a screenshot of
   each for the owner to review later.
8. **Tests.** Each one watches a failure that the compiler and the existing
   tests miss:
   - A request that holds a term and a want the daemon does not know
     decodes, runs and reports both as skipped. The request is fixture
     JSON, because the client cannot make such a request.
   - A skipped term that is alone in its clause, negated or not, gives no
     rows. This watches the one rule that keeps a skipped part from
     widening the answer: an emptied clause that someone removes makes the
     query match everything.
   - A request of an unknown kind answers `UnknownRequest` with its name,
     not `BadQuery` or `Unsupported`.
9. **Docs.** Move "Rules for an addition" into `docs/DESIGN.md` ("Wire
   protocol") and replace its "Each change … raises `PROTO_VERSION`" line.
   Correct the module comment of `proto/types.rs` and the comment on
   `ParsedQuery`, which both say that each change raises the version. In
   `AGENTS.md` ("The wire protocol"), point at `docs/DESIGN.md` instead of
   this plan and remove the sentence about stage 1. Then delete this plan.

`gozlerd protocol` and `LocalService::from_protocol` do not change: the
daemon still serves only its own version, so their oldest and newest
version stay equal.

## Accepted limits

- **The window's default moves to a new feature.** It happened with
  word-start matching (7) and result snapshots (11). An older daemon then
  skips or refuses every default search of the window, and the window says
  each time that a newer gozlerd is needed. `gozler` goes on working. Code
  to fall back to the old default would be the legacy code this plan
  avoids, and an Update button in the notice would be new window work for
  a state that lasts until the next install.
- **An older client after a break says the daemon is older.** The client
  reads every refusal at `Hello` as "the daemon is older"
  (`client.rs`, `hello`), also when the daemon is newer. The setup installs
  both programs together, so only a developer's machine meets this, and
  telling the two apart would need a new failure in the CLI and the window.
- **`CountIncluded` sends `IndexSettings`.** A new Include setting changes
  the count, and the daemon cannot skip part of a setting. The settings
  change rarely, so a new one is a break.
- **Stage 1 itself is a break.** Each machine installs gozlerd 22 once.

## Not in this plan

- **JSON for results pages.** The page is binary because a large reply
  carries millions of names. On `C:\` it already carries 73 MB instead of
  251 MB of full paths, and JSON would quote each name and repeat each field
  name in every row. JSON did not cause the version changes: every request
  and every control reply is JSON already.
- **Protobuf or another schema format.** It reads unknown fields, but it
  adds a dependency to `gozler-core`'s allowlist and a code generator. It
  also cannot know that a skipped filter widens an answer, so rule 2 is
  still needed.
- **A revision number with feature checks in the client.** The skipped
  report gives the person the same answer with no check to maintain at
  each addition.
- **A fallback variant in reply enums.** Only one of the 20 version changes
  added a value to a reply enum, `ProtoError::ResultSnapshotGone`, and only
  its new request causes it (rule 6). Serde also cannot give a fallback to
  an enum whose variants hold data, such as `IndexProgress`.
