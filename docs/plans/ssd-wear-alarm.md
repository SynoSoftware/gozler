# SSD wear alarm

A program that writes the same file over and over, or keeps filling one
folder with files, wears out an SSD and slows the PC. In November 2026 two
GPU drivers did this on many PCs. NVIDIA's display container
(`NVDisplay.Container.exe`, drivers 621.14 and 621.24) kept rewriting
`C:\ProgramData\NVIDIA Corporation\nvtopps\nct\nvlog.nvlgstg`: reports give 4
to 30 GB an hour on an idle PC, and say the writes grow as the file grows
([Windows Latest, 2026-11-20](https://www.windowslatest.com/2026/11/20/first-amd-now-nvidia-your-gpu-driver-could-be-quietly-wearing-out-your-windows-11-pcs-ssd-and-nvidia-hasnt-even-admitted-it/)).
AMD's Adrenalin software filled an `EEUDumps` folder until AMD fixed it in
26.11.1. No tool warns about this. Resource Monitor shows it only while a
person looks.

The owner's criterion is not "bytes per process per hour". Useful work also
writes many bytes: on the owner's PC, `C:` took 1,494 GB in the 274 hours
since its last start, 5.5 GB an hour on average (Windows' own counter, read
2026-11-22). What is bad is the same file written over again and again, and
one folder that gets files again and again.

gozlerd already reads every record of each volume's change journal. NTFS
writes a record each time a file is created, deleted or renamed, and each time
a program writes a file's data and closes the file. Today gozlerd uses the
records only to keep the index current, and then drops them.

After this plan, gozlerd keeps two values for each file and folder over the
last hour: **Writes**, how many times a program wrote it and closed it, and
**Written**, how many bytes those writes put on the disk. A person finds the
busiest ones with `gozler -sort written` or the window's Writes and Written
columns. The window warns, also while it runs in the notification area, when
a file or folder is written again and again and Written passes the person's
limit, and when a folder is copied into itself again and again, as a broken
test or build step does.

This plan is the "concrete incremental-monitoring task" that
`docs/research/cli-gap-comparison.md` ("Recent changes") waited for. It does
not add a recent-changes list: it keeps counts, not events.

## Schedule

**Scheduled:** approved on 2026-11-22, after a Fable review found no
substantial issue. It starts when the work on that day's open issues
(#165, #166 and the others) has finished with the index and engine files.
It touches the journal
record, the index, the tail, the query engine, the wire protocol,
`gozler-cli` and `gozler-ui`'s results list, settings and tray. Issue #161
(buffers that grow under the index's write lock) touches the same lock; this
plan adds no growth of an index buffer under it, because the table, the two
lists of rule 1 and the event list of stage 4 have a fixed size (stage 1,
items 2 and 3; stage 4, item 5). Not started.

"Before stage 1" comes first: the owner reads the journal's own records for
five actions, in a few minutes and with no Gozler build, because rule 1 and
stage 4, item 2 rest on them. Each stage is its own commit. Stage 4 needs
none of stages 1 to 3 and can come first. One implementer keeps its context
through the stages and fixes the review's findings; the reviewer reads and
reports. No stage waits for the owner: the limits start with the defaults
below, and the "After install" list holds what only an installed build or
another PC can show.

## Owner decisions

The plan is written with each recommendation. Each decision changes only the
part named, so the owner can decide while stages 1 to 4 are built.

1. **How the warning shows** (stage 5, item 4). Recommended: Gozler's own
   dialog, shown without taking the focus, with its taskbar button flashing
   until the person looks. The other choices: a Windows notification, as the
   "Gozler is still running" notice uses, which is lost when the person
   turned notifications off, as the owner has; or only another tray icon,
   which is easy to miss.
2. **The ignore list** (stage 5, item 6). Recommended: keep it. Build output
   and `.git\index` are written again and again by design, and without Ignore
   the only answer to such a warning is to turn the setting off.
3. **The default limits:** 1 GB an hour of Written for a file and 2 GB an
   hour for a folder, each with at least 5 writes that fall in at least 3 of
   the hour's 12 steps of 5 minutes, and 4 copies for a folder that copies
   into itself. They are provisional. 1 GB an hour all year is about 9 TB,
   1.5 % of the 600 TB that a 1 TB consumer SSD is rated for, and NVIDIA's
   file was reported at 4 GB an hour or more. The 3 steps are what makes
   "again and again" true over time: one copy of many files falls in 1 or 2
   steps and does not warn, and a fault that runs all hour falls in 12. A
   copy into one folder that lasts longer than 10 minutes still warns.
   "After install" lists the day of `-sort written` on the owner's PC that
   sets the final numbers.
4. **The setting starts off** (stage 5, item 1). Recommended: off in this
   plan. The limits are provisional, and a warning about normal work on
   every PC costs more trust than a warning that a person has to turn on.
   When "After install" has set the limits, the owner decides whether a new
   install starts with it on. The other choice, on from the start, makes the
   window ask the service every 5 minutes on every PC and shows the first
   wrong warning to people who never asked for the watch.

Settled by the owner (2026-11-22): appends count as writes, and the bytes
they add count in Written; the limits read Written, not writes times size.

## What a person sees, and what the warning reads

| Value | For a file | For a folder |
| --- | --- | --- |
| **Writes** | times a program wrote it and closed it, in the last hour | the writes of the files directly in it, including files created and deleted since |
| **Written** | the bytes those writes put on the disk: exact for data added to the end, "up to" when a write replaced data, because the journal does not say how much | the Written of the files directly in it, added up |
| **How written** | in the inspector only: Added to the end, Replaced, Written over in place, or a mix | – |
| **Steps with a write** | in how many of the hour's 12 steps of 5 minutes it had a write, in the inspector and in `--json` | the same, for the writes of the files directly in it |

The warning reads these values, with one rule for files and folders:
**at least 5 writes, in at least 3 of the 12 steps, and Written over the
limit.** "At least 5 writes in at least 3 steps" is the owner's criterion,
the same file or folder again and again: the writes are more than 5 minutes
apart from the first to the last. Written is the wear. A transcript appended
300 times an hour has many writes and a few MB, so it stays far under the
limit. A log that grows by GBs an hour warns. A `cache.bin` of 100 MB
created again every minute warns through its folder.

What the three parts leave out, and what they do not:

- **One copy does not warn.** Copying 10 files of 300 MB into one folder
  gives the folder 10 writes and 3 GB of Written, over both limits, but all
  in 1 or 2 steps. Without the steps, every copy, download and install of 5
  files and 2 GB would warn, also a copy made in Commander.
- **A long copy does.** A copy into one folder that takes longer than 10
  minutes has writes in 3 steps and warns. The person chooses Ignore for a
  folder where that is normal, and "After install" sets the number of steps
  from what the owner's PC shows.
- **A large file that is closed a few times can warn.** A write over part of
  a file adds the file's whole size, "up to" (rule 4). A torrent client that
  writes pieces into a 50 GB file and closes its handle 5 times over the
  hour reads "up to 250 GB", though it wrote far less. The journal cannot
  tell, the warning shows "up to", and the Ignore list answers it: the
  person ignores the download folder once.

These would confuse, and the plan leaves them out:

- **Opens.** An open that changes nothing writes no record, and it puts
  nothing on the disk.
- **Journal records as a count.** NTFS adds a record each time an open gains
  a reason bit, so one save can make three records. Only the close counts.
- **One count for every kind of write,** with no bytes. 300 appends of 1 KB
  and 300 rewrites of 100 MB would read the same.
- **Writes times size for data added to the end.** 300 appends to a 30 MB
  log would read 9 GB; the growth is the true figure.
- **A column for each kind** (appends, replaces, writes over). A person
  judges how often and how much; the kind helps only to understand one file,
  so it is in the inspector.
- **Changes of dates, attributes and permissions.** They write a few bytes
  of metadata, and an antivirus or a backup makes them on thousands of
  files.
- **An exact figure and a bound in one number without saying so.** Written
  reads "up to" whenever a write in its hour replaced data.

## Words

**Write:** one time a program changed a file's data and closed the file:
added to its end, replaced it or wrote over part of it. A save through a
temporary file is one write of the name it ends under.

**Writes:** for a file, its writes in the last hour. For a folder, the
writes of the files directly in it, also of files created and deleted since.
A folder's writes do not include the folders below it.

**Written:** the bytes a file's writes put on the disk in the last hour, as
far as gozlerd can tell: the growth for data added to the end, and the size
after the write for data replaced or written over, which is the most that
write can have put there. For a folder, the Written of the files directly in
it.

**Steps with a write:** in how many of the 12 steps of 5 minutes that make
the last hour a file or a folder had at least one write: 0 to 12. It is read
from the 12 counts that a row already keeps (rule 6), so nothing more is
stored. `CONTEXT.md` already has "Step" for Multi-Rename, so its entry there
is written out in full, as here.

**Counted since:** the time from which a volume's writes are complete: the
time its last build started to read the journal. Less than an hour after
that, the values of the hour may be low.

**Repeating folder:** a folder whose path ends in the same group of folder
names again and again, such as `a\a\a\a` or `a\b\c\a\b\c\a\b\c\a\b\c`: what
a step that copies a folder into itself leaves. The group can have any
number of names.

These go into `CONTEXT.md` with stage 2.

## What the journal can and cannot tell

These limits decide what the plan promises.

- **A file held open is seen at each new reason bit, and at close.** NTFS adds a record only when a reason
  bit is added to an open's set, and at close (Microsoft's documentation of
  change journal records; `gozler_core::usn::ops`'s module comment says
  "every sub-operation appends a record", which is looser than this). A
  program that opens a file once and writes it all day, as a database, a
  virtual disk or the page file does, has one write, when it closes the file,
  and its writes stay 0 until then. Its Written shows its growth: the size
  step reads the size whenever a new reason bit makes a record (at most once
  for each bit in one open), and adds the growth (rule 4). Of the data the
  program writes over in place, Written shows nothing. So writes find the
  programs that open, write and close a file again and again. **Whether NVIDIA's container closes `nvlog.nvlgstg`
  after each write is not known**, and no PC of the owner's has the fault.
  The first item of "After install" checks it on a PC with an affected
  driver, and anyone with such a PC can answer it today with the command of
  "Before stage 1", with no Gozler build. If the container holds the file open, this plan does not warn about
  that file, because its writes stay 0, and "Ideas considered" names what
  would.
- **A record names the file, not the program.** The journal has no process
  ID. The folder usually names the program: `nvtopps` is NVIDIA's. Resource
  Monitor's Disk tab names the process, and the warning says so.
- **A record has no byte count, but the size has.** gozlerd already reads
  each changed file's size after a batch (`UsnOp::StatDirty`), and the index
  holds the size before. Data added to the end is exactly the growth. A write
  that replaces data or writes over part of it put at most the file's size
  on the disk; a program that changed 4 KB of a 1 GB file wrote 4 KB, so
  Written says "up to" for it.
- **Preallocation over-states growth.** A program that sets a file's end
  first (`SetEndOfFile`) makes a record with `USN_REASON_DATA_EXTEND` and
  writes no data, so the growth counts bytes that nobody wrote. Downloaders,
  torrent clients included, do this. Written keeps such growth unmarked,
  because the journal cannot tell it from data written. A folder that
  receives a preallocated download shows its size as Written.
- **A record says how the data changed.** Its reason bits tell data written
  over (`USN_REASON_DATA_OVERWRITE`), data cut off
  (`USN_REASON_DATA_TRUNCATION`) and data added to the end
  (`USN_REASON_DATA_EXTEND`) apart, which rules 1 and 4 need. Microsoft
  documents the bits; which of them a save sets is checked once, in "Before
  stage 1".
- **A file can be gone before its size is read.** A temporary file deleted
  in the same batch has a write but no size. Its bytes are missing, and
  Written says "at least" for that hour.
- **A record is read once, as it is written.** gozlerd reads the journal
  forward and keeps no record, and after a rebuild it starts at a new place
  (`tail.rs`, `rescan`). "Counted since" says from when the counts are
  complete.

## Rules

1. **What counts as a write.** Only a record with `USN_REASON_CLOSE`
   counts, so one open counts once. For a file's record:
   - with `USN_REASON_FILE_CREATE`: one for its folder, also when the file
     was deleted in the same open. The name gets no row, because a build
     creates thousands of new names an hour;
   - without it, with any of `USN_REASON_DATA_EXTEND`,
     `USN_REASON_DATA_OVERWRITE` or `USN_REASON_DATA_TRUNCATION`: one for
     its name and one for its folder;
   - with `USN_REASON_RENAME_NEW_NAME`: one for its new name only when the
     rename replaced another file under that name (see below), and nothing
     for the folder, because a rename moves no data and moving 10,000 files
     into a folder is not wear. Any other rename, a move or a Multi-Rename,
     counts nothing and takes no row;
   - with no data bit, such as a change of dates, attributes or
     permissions: nothing.

   A record that meets two of these counts once for its name and once for
   its folder at most. A folder's own records count nothing.

   **A rename replaced a name** when one batch holds both a file's delete
   record for a name in a folder and another file's rename record, with
   `CLOSE`, to that name in that folder, in either order. The journal does
   not say that a rename replaced a file, and the index cannot answer it
   cheaply: it finds a folder by its folder and name (`dir_children`), but a
   file only through the chain of every entry on the volume with that name
   (`link_same_name`, `volume.rs`), which holds tens of thousands of entries
   for `package.json`. So the test reads no index. `Writes` owns two lists
   of 16 places each, emptied when a batch starts:
   - **deleted names:** a file's record with `USN_REASON_FILE_DELETE` puts
     the row key of the record's folder and name there;
   - **renames:** a file's record with `USN_REASON_RENAME_NEW_NAME` and
     `CLOSE` puts its row key, its file ID and its step there.

   Each of the two records first looks in the other list. A rename that
   finds its key among the deleted names counts at once. A delete that finds
   its key among the renames counts that rename then, for the file ID and
   the step the list kept. Either takes the key out of both lists, so a pair
   counts once. Nothing is carried from the first `RENAME_NEW_NAME` record,
   which has no `CLOSE`, to the `CLOSE` record: only the `CLOSE` record
   enters the test, so one open counts once (reason bits accumulate, so the
   `CLOSE` record still holds the rename bit).

   The cost under the write lock: each file's delete record and each such
   rename record folds its name into the buffer that `Writes` reuses, hashes
   it, compares up to 16 keys and stores one. Nothing is allocated, and the
   lists never grow. Deleting 500,000 files costs 500,000 folds and hashes
   of a name over all its batches; tens of milliseconds in total is
   suspected from that count, not measured. A list that is full
   drops its oldest key.

   What this misses, each as a count that is too low: a pair split across
   two batches (the tail reads every 50 ms); a pair with more than 16 other
   deletes or renames between its two records; and a save that renames the
   old file away before it renames the new one in, as `ReplaceFile` does,
   because no record deletes the name. It also counts `del x` followed by
   `ren y x`, which is right: the name got another file. "Before stage 1"
   reads the records of a save through a temporary file and decides two
   things. If the delete record does not carry the replaced name and its
   folder, the deleted-names list takes its key from the entry that the
   index holds for the deleted file, before the delete applies. If the
   tested program renames the old file away, a file's record with
   `USN_REASON_RENAME_OLD_NAME` and without `USN_REASON_RENAME_NEW_NAME`,
   which is the one record that carries the old name, also puts that name's
   key in the deleted-names list, so a name that one file left and another took
   counts too; otherwise that variant stays out.
2. **A count belongs to a name in a folder, not to a file ID.** A save
   through a temporary file gives the name a new file each time (create
   `x.tmp`, write it, rename it over `x`), so counts by file ID would show
   400 files with one write each. The key is the folder's file ID and the
   folded name. So a file that is renamed leaves its count with the old name
   and shows 1; a folder that is renamed or moved keeps its files' counts and
   its own; and each hard link has the count of its own name. A row is an
   entry's row only when the row's file ID equals the entry's file ID, so a
   file deleted and created again under its name does not show the old
   file's row. A counted write or a counted rename sets the name's row's file
   ID to the file it counted. A folder's row holds the folder's own file ID
   from the time it is made, and a write of a file in the folder never
   changes it, so the search that visits the table's rows (stage 2, item 3)
   finds the folder by it. The index stores names folded by `fold_accents`, and
   the key uses those stored bytes, so `résumé.txt` and `resume.txt` in one
   folder share a row. This is an accepted limit.
3. **A deleted file's writes stay in its folder** until they leave the hour.
   A program that creates and deletes temporary files all day shows in their
   folder. The name's own count also stays, so a file that is replaced again
   and again by a rename over it keeps one count. A file that is deleted and
   created again under its name shows only in its folder, because rule 1
   counts a create for the folder only and the name's row keeps the old file
   ID (rule 2); its folder's Written still shows
   each new copy's bytes, so the folder warns.
4. **Written comes from the size.** The size step is the write hold where
   the tail stores the sizes it read after a batch. It adds to the rows that
   the batch counted, and it can make or update a row itself:
   - **Growth.** For each size change that the step applies to a file whose
     batch carries a data bit, when the size grew, it adds the growth,
     exact: to the folder's row always, and to the name's row unless the open
     created the file (rule 1 gives a new name no row). For a file that the
     open created, the growth is its size, so a create adds the new file's
     size to its folder. This holds with or without a `CLOSE` in the batch,
     because the tail reads every 50 ms, and a write that lasts longer than
     one read moves the size before its `CLOSE`. So the step can make or
     update a row that has Written but no writes yet. Only a `CLOSE` adds a
     write;
   - **A write that replaced data or wrote over part of it** (a counted
     close with `USN_REASON_DATA_OVERWRITE` or `USN_REASON_DATA_TRUNCATION`):
     also the size after it, marked "up to". It is an upper bound: it may add
     again bytes that growth already added. So one save that cuts a file to
     0 and writes it again adds its size once when the cut and the close
     reach the size step together, and twice when a read of the tail falls
     between them and sees the size 0. The same save can so read 1 or 2
     times its size, by timing, and a Written near the limit can pass it or
     not. This is accepted: the value is marked "up to" in both cases, and a
     fault that the warning is for passes the limit many times over;
   - **A rename that rule 1 counts:** the size that the index holds for the
     renamed file after this batch's size step, marked "up to", to the name's
     row, with no new read. A rename adds nothing to the folder's writes or
     Written;
   - **A file gone before its size was read** (deleted in the batch, or this
     batch's read of it failed): nothing, marked "at least". The size that the index held
     before the delete is not used, because it says nothing about the write;
   - **A size before that is unknown** (the size pass failed or stopped, or
     an earlier read of the file failed): the write adds the size after, marked "up to";
   - **A walk of a missed folder that fails** ends the settle before the
     size step (`settle_batch` returns `WalkFailed`, and the tail rebuilds):
     the settle first marks the batch's rows "at least" in one write hold,
     because the rebuild keeps the table (rule 8). A settle that is
     cancelled marks nothing: the service is exiting and nobody reads the
     table again (`settle_batch`: "applies nothing more").

   Two writes of one name in one batch add their size once for each write
   that replaced data, and the growth once. A folder's Written adds what its
   files' writes added. Each row keeps, for each step, which of the three
   kinds of write it had (added, replaced, written over) and the two marks,
   so the inspector can say how the file was written and Written can say
   "up to" or "at least". A Written with both marks reads "at least": its
   value is then the lower bound of Written as this rule defines it (rule
   13).
5. **Counts are in 12 steps of 5 minutes,** so "the last hour" is between 55
   and 60 minutes. The table's step moves forward only, by the time stamp of
   the record that gozlerd counts, and by the tail's clock while the journal
   is quiet. A record whose time is before the table's step counts in the
   table's step. Each move of the step empties the oldest step of every row
   under the index's write lock, so a count changes only under that lock, as
   a size does. A search reads no clock, and its sort keys cannot change
   while it sorts. When a record's time or the tail's clock is more than an
   hour before the table's step, the clock was set back: the table empties
   and "counted since" moves to that time, as after a rebuild. Without this,
   a clock set forward and back again would hold the step in the future, and
   every write until then would count in one step.
6. **The table has a fixed size:** one row for each sixteenth of the volume's
   entries, at least 4,096 and at most 65,536 rows of about 112 bytes (the
   key, the file ID, and for each of the 12 steps a 16-bit count, a 32-bit
   Written in KiB and a byte of kinds and marks, each count stopping at its
   highest value), so about 450 KiB to 7 MiB a volume, beside an index of
   194.6 MB for the owner's `C:`. Each write's bytes round up to a whole
   KiB, because a disk writes at least a page, so an append of 10 bytes
   counts 1 KiB. A step's Written stops at 4 TiB. A row is a name or a
   folder with at least one write in the hour. Every indexed
   volume has the table, whether or not anybody asks, so `gozler` answers at
   once and the service needs no setting. A rebuild sizes the table again
   from the new entry count (rule 8).
7. **A full table drops the least written rows.** One pass drops the eighth
   of the rows with the least Written, then the fewest writes, and adds the
   highest count and the highest Written it dropped to **may be low by**,
   for an hour, in the same 12 steps. It adds, not keeps the highest,
   because a name that is dropped, written again and dropped again loses a
   count at each pass. The pass costs one read of the table, and runs at most
   once for each eighth of the table that fills. While "may be low by" is
   `D` writes and `B` bytes, every value on that volume is between what the
   table says and that much more. A pass never drops a row that the current
   batch touched (stage 1, item 3).
8. **A rebuild keeps the values** and moves "counted since" to the time the
   rebuild took its journal cursor, because the records between the old place
   and the new cursor are never read. `Building::finish` moves the table into
   the new index inside the write hold that installs it. The table is sized
   again from the new entry count (rule 6). A smaller table keeps the rows
   with the most Written and adds what it drops to "may be low by" (rule 7).
9. **Nothing is saved, and nothing is read back.** After gozlerd starts, the
   writes of the hour before are missing, and "counted since" says so. This
   follows "Saving the index between runs" in `docs/DESIGN.md`'s "Not
   planned".
10. **Where the Include settings leave something out, nothing is counted.**
   `apply_batch` counts a record after its screen, so a record that the
   screen drops counts nothing: the index has no entry there to show. A
   person who turns on "Exclude hidden files and folders" leaves out
   `C:\ProgramData`, where NVIDIA's file is, and stage 5 tells them.
11. **A filtered user sees the values of the files they see, and of the
    folders they may list.** A folder's values count only the files directly
    in it, so they show activity in that folder. For a folder that the
    caller may not list, the values are unknown, even when the folder entry
    itself is visible because its parent is listable (`CONTEXT.md`, "Visible
    entry"): rule 13 then applies, so no `writes:` or `written:` test
    matches it, a sort puts it last, and no value is sent.
12. **A volume without a journal (`snapshot`) has no values.** Its writes
    and Written are unknown, not 0.
13. **An unknown or uncertain value never gives a wrong match.** Written is
    defined as rule 4 makes it, so "up to" is part of its meaning, not an
    uncertainty: `written:>1gb` compares that number. A `writes:` or
    `written:` test, negated or not, never matches an unknown value. While
    "may be low by" is `D` writes and `B` bytes, a test matches an entry only
    when it holds for every value from the table's to that much more, fails
    it when it holds for none, and otherwise treats the value as unknown, as
    a size test treats a folder whose size is a lower bound (`holds_from`).
    While a volume was counted for less than an hour, for a row with a step
    that stopped at its highest value, and for a Written marked "at least",
    the value is a lower bound with no upper bound, and a test matches only
    when it holds for every value from the table's up. So `writes:>100`
    still finds a file with 400, and `writes:0` lists no file that may have
    had a write. The steps with a write are a lower bound in the same
    states. No search term reads them; the warning's rule does (stage 5,
    item 2), and a number that is too low can only make it warn later, never
    wrongly.

## Before stage 1

Rule 1 and stage 4, item 2 rest on what NTFS writes to the journal for five
actions. Windows' own `fsutil` prints each record with its reason, so the
owner checks them before any code is written: a few minutes on the owner's
PC, in an administrator Command Prompt (`cmd.exe`, not PowerShell, because
the commands below use its syntax), with no Gozler build. A result that
differs from the "Expected" column changes the rule named beside it before
stage 1 starts, not after an install.

1. `mkdir C:\weartest\a\sub`, put a file of about 1 MB in it as
   `C:\weartest\weartest.txt`, and run `git init C:\weartest\repo`.
2. Start the reader, which prints every record written from now on:
   `fsutil usn readJournal C: wait tail csv > "%TEMP%\weartest.csv"`
3. In a second terminal or in the programs named, do the five actions of the
   table, a few seconds apart.
4. Stop the reader with `Ctrl+C` and open `%TEMP%\weartest.csv`. A record
   holds the file's name, not its path, so find each action by its file name
   and read the records around it.

| Action | Read | Expected | Decides |
| --- | --- | --- | --- |
| Open `weartest.txt` in Notepad, change it, save it twice | the reasons of the two records with Close | a data reason (data overwrite, data extend or data truncation) with Close, and no file create | rule 1, a write of a name; rule 4, the kinds |
| `echo x>>C:\weartest\weartest.txt` | the reasons of the record with Close | data extend and Close, no other data reason | rule 4, growth is exact for data added to the end |
| In `C:\weartest\repo`, `git add` a file twice, which saves `index` through `index.lock`; and one save from the editor the owner uses | the records of `index` and `index.lock`: which file is deleted, which is renamed, in what order, and the name and parent file ID in the delete record | a delete record that carries the name `index` and the folder's ID, and a rename record of the other file to `index`, with Close, next to each other | rule 1, a rename that replaced a name: the two lists as written; or the delete key from the index's entry; or the `RENAME_OLD_NAME` variant when the old file is renamed away |
| `robocopy C:\weartest\a C:\weartest\a\a /e` | the reasons of the record with Close of each new folder `a` and `sub` | file create and Close in one record, with or without other reasons | stage 4, item 2: the test for both bits; other reasons here show why "no other bit" would have failed |
| Copy the folder `a` in File Explorer and paste it into `a\a` | the same | the same | the same |

On a ReFS volume, such as a Dev Drive, the same five actions with that
drive's letter show whether ReFS writes the same reasons; the plan assumes
it does. The check of NVIDIA's file stays in "After install", because no PC
of the owner's has the fault; anyone with an affected driver can answer it
with the reader of step 2 and a look for `nvlog.nvlgstg`: records with
Close every few seconds mean that the plan finds the file, and one data
record with no Close after it means that the container holds the file open.

## Stage 1: gozlerd counts writes

1. **The record keeps its time.** `ParsedUsnRecord` gains the record's time
   stamp (`RawV2`'s and `RawV3`'s `_timestamp`, which the parsers now drop),
   because rule 5 needs it. It is the time NTFS wrote the record, not a time
   of the file.
2. **`gozler_core::index::Writes`** owns the values: the rows, their steps,
   the fixed size, "may be low by" and "counted since". It is plain Rust, so
   CI tests it on Ubuntu. A row holds its key (64 bits, the hash of the
   folder's file ID and of the folded name as the index stores it, or of a
   folder's own file ID), the file ID of the file last counted under the name
   (rule 2; for a folder's row, the folder's own file ID), and for each step
   the count, the Written and the kinds and marks of rule 4. The steps with
   a write are the steps whose count is not 0. `Writes` also owns the buffer
   that a name is folded into and the two lists of 16 places of rule 1. Two
   names whose keys are equal
   would share a row; with 64 bits and 65,536 rows that does not happen in
   practice. `Writes` takes every time as an argument and has no clock of
   its own.
3. **The table is a field of `VolumeIndex`,** so a search reads it under the
   read hold it already has, with no second lock and no lock order.
   `apply_batch` counts each record after its screen, in the loop where the
   record's file, folder, name and reason are in hand. What each record
   costs under the write lock:
   - a file's record with `USN_REASON_FILE_CREATE` or a data bit, with or
     without `CLOSE`: one fold of its name into the buffer that `Writes`
     owns and reuses, one hash, at most two row updates when it has `CLOSE`
     (rule 1), and one hand-off value (below);
   - a file's delete record, and a file's rename record with `CLOSE`: one
     fold, one hash and the two lists of rule 1;
   - every other record: one test of its reason bits.

   Nothing walks up the folders, and nothing reads the index by name. The
   table is allocated at its full size when a build ends, outside the write
   lock, so a batch never grows it (issue #161).

   The hand-off from `apply_batch` to the size step (`settle_batch`, where
   it stores the sizes it read) travels in `ApplyOutcome`, as a `Vec` beside
   `needs_stat`. It holds one value for each file's record with
   `USN_REASON_FILE_CREATE` or a data bit, whether or not the record has
   `CLOSE`, because rule 4 adds growth before the close, and one for each
   rename that rule 1 counts. Each value holds the file ID, the name's row
   key and the folder's row key, never an entry slot, because the settle
   takes up to four write holds; the step; the kinds; and whether the open
   created the file. The `Vec` grows under the write lock as `needs_stat`
   does: by at most one value of about 40 bytes for each record of the
   batch, and a batch is one read of at most 512 KiB (`READ_BUFFER_BYTES`),
   so it stays under a few hundred KiB. That is the only allocation the
   counting adds under the lock; the fold, the rows and the lists allocate
   nothing. One open can put several values for one file in a batch, because
   reason bits accumulate; the settle sorts the values by file ID outside
   the write lock, and the size step adds a file's growth once, to the rows
   of its last value. The size step adds Written by rule 4 in its write
   hold, and reads no new size. A walk that fails marks the batch's rows "at least"
   (rule 4). A drop pass (rule 7) never drops a row that the current batch
   touched.
4. **The tail moves the step while the journal is quiet.** When a read
   brings no records and the clock has passed the table's step, the tail
   moves the step in one write hold, which reads the table once. Without
   this, an idle volume would show the writes of an hour that ended long
   ago.
5. **Status.** `VolumeStatus` gains `writes: Option<WritesStatus>`: "counted
   since", the rows in use, the table's size and "may be low by". It is the
   one place a client reads these facts. `--status` prints one line for each
   volume, such as `Writes        counted since 10:42, 1,204 of 65,536 rows`,
   with `, values may be up to 3 writes and 40 MB low` while "may be low by"
   is not 0.

Tests, each with literal times, sizes and counts:

- Rule 1, in one test: a record with only `DATA_EXTEND` counts for the name
  and the folder; a create counts for the folder only; a record with only a
  date change counts nothing; a close after a rename counts for the new
  name only when it replaced another file under that name, in either order
  of the delete and the rename, and a move counts nothing and takes no row;
  a record without `CLOSE` counts nothing.
- Rule 2: three saves of `x` through `x.tmp`, each with a new file ID, give
  `x` three writes. It watches a return to counts by file ID.
- Rule 4, in one test: an append from 10 MB to 12 MB adds 2 MB, exact; a
  write over a 30 MB file adds "up to" 30 MB; a create of 5 MB adds 5 MB to
  the folder; a file gone before its size adds nothing and marks "at
  least"; growth read before the `CLOSE` adds its Written to the folder
  and the name with no write yet. It watches a return to writes times size
  for appends, and a loss of the growth that a long write moves before its
  close.
- Rule 5: a write leaves after 12 steps, a record with an older time counts
  in the table's step, and a record more than an hour older empties the
  table and moves "counted since".
- Rule 7: a full table drops its least written rows and reports "may be low
  by", a name dropped twice in the hour stays within that bound, and a row
  that the current batch touched is not dropped.
- Rule 8: a rebuild keeps the values and moves "counted since"; into a
  smaller table it keeps the rows with the most Written and adds the rest to
  "may be low by".

## Stage 2: searches, sorts and the command line

1. **`writes:` and `written:`** are search functions. `writes:` takes the
   comparisons and ranges of `child-count:` (`count_range`): `writes:>100`,
   `writes:10..50`, `writes:0`. `written:` takes those of `size:`:
   `written:>1gb`, `written:10mb..1gb`. They are new `Test::Writes { range }`
   and `Test::Written { range }`, additions by rule 2 of "Additions and
   breaks". Both follow rule 13.
2. **`-sort writes` and `-sort written`** are new `SortKey::Writes` and
   `SortKey::Written`, most first, additions by rule 5. `key_records` reads
   the value under the read hold that reads the other keys, and an unknown
   value sorts last, as an unknown size does. The list holds files and
   folders together; `file:` or `folder:` keeps one kind.
3. **The scan pays only when it is asked.** A search that tests or sorts by
   either value looks each candidate up in the table: one read of the
   parent's file ID, one hash of the stored folded bytes of its name
   (`folded_bytes_of`), one table lookup and one file ID compare. The new
   checks get a cost rank like `Check::Child` (`query/engine.rs`), so the
   cheap name tests run first. A search that only asks for the values looks up each row it
   sends, not each candidate. Every other search runs nothing more for a
   candidate, decided once for the search by the loop it chooses, as
   AGENTS.md asks of `Scan::visit`. A search with a clause that holds only
   `writes:` or `written:` terms that need at least one write or one byte,
   such as `writes:>=5 file:`, visits only the table's rows: it finds the
   entries of the file ID each row holds, keeps the one whose folder and name
   give the row's key, and passes it through `Scan::evaluate` like the other
   scan paths, so the scope, visibility and the other tests still apply. The
   key check matters because a file that was renamed, or has hard links, has
   entries under other names, whose counts are other rows. Both paths use
   one rule: a row is an entry's row only when the row's file ID equals the
   entry's file ID (rule 2). So `writes:>=5 file:` and `writes:>=5|size:>1gb`
   give the same answer for a name that was deleted and created again. A row whose file is gone has no entry and is left
   out. An entry without a row never matches such a clause under rule 13,
   so the table's rows are every candidate.
4. **The values travel when they are asked for.** A new `Want::Writes` sends
   each item's writes as a `u32`, its Written as a `u64`, its steps with a
   write as one byte (0 to 12) and one byte of kinds and marks: 14 bytes,
   behind item flag bit 7, by rule 3. Issue #165's per-folder file and
   folder counts already take bit 6 (`IFLAG_HAS_COUNTS`, `proto/binary.rs`),
   so bit 7 is the last free item flag bit. The steps travel inside this
   value, so they take no flag bit of their own. An unknown value sends
   nothing.
5. **One flag says that values may be low,** as `metadata_pending` says of
   sizes: a volume in scope has no table, was counted for less than an hour,
   or has "may be low by" above 0. A results page carries it behind the next
   free page flag bit, bit 5 today, when the request asks for the values or its query tests or sorts by
   them (the daemon knows its query), so stage 3, item 5 works with the
   columns hidden. A totals reply carries
   it as a new `Option` field (rule 1) when the query tests them, so
   `-get-result-count writes:>100` can warn too. After this plan no item
   flag bit is free and two page flag bits are; the next requested value of
   an item is a layout break (rule 3). `PROTO_VERSION` stays. The times and the
   numbers behind the flag are in status only (stage 1, item 5).
6. **`gozler`.** `--writes` adds two columns, Writes and Written (gozler's
   own options start with `--`, because es has none). Written prints bytes
   as the Size column does, after `<=` when it is "up to" and `>=` when it is
   "at least" (also when it has both marks, rule 4), so a script can tell
   them apart. `--json` with `--writes` adds `writes`, `written`,
   `written_is` (`exact`, `up_to` or `at_least`) and `write_steps` (0 to 12)
   to each item. Only `--writes` prints the columns and the fields: a search
   that tests or sorts by either value without it prints what it printed
   before and sends no `Want::Writes`, so its rows cost no lookup; the flag
   still reaches it, because the daemon sets the flag from the query (item
   5).
   When the flag is set, `gozler` warns
   `gozler: warning: some writes may be counted low; run gozler --status`,
   because the flag also covers a full table and a drive without a journal,
   not only a drive counted for less than an hour.
   It exits 2 when the query tests either value, because the answer may be
   short, and 0 when the rows only show them or are sorted by them, because
   every row is there. This is the rule for sizes (`docs/DESIGN.md`, "Sizes
   and dates").
7. **Help and design.** `SEARCH_FUNCTIONS`, `SORT_KEYS` and the option table
   gain their rows, so the help and the window's Search syntax dialog list
   them. `docs/JSON.md` gains the four fields. `docs/DESIGN.md` gains a section on
   writes, from this plan's "Words", "What the journal can and cannot tell"
   and "Rules", and `CONTEXT.md` the words. Rule 3 of "Additions and breaks"
   then says that no item flag bit is free and 2 page flag bits are.

Tests:

- The exact bytes of a results page with writes and with the flag, because
  the page's bytes are the contract.
- The CSV and plain output of `--writes`, with an exact, an "up to" and an
  "at least" Written.
- Rule 13, in one test with "may be low by" 3 writes: `writes:>100` matches
  a file with 400; `writes:0` does not match a file without a row;
  `!writes:0` does not match it either. And on a volume counted for less
  than an hour, `writes:0` and `!writes:>=5` do not match a file without a
  row.
- Stage 2, item 3: for a name deleted and created again, `writes:>=5 file:`
  and `writes:>=5|size:>1gb` give the same answer. It watches the two search
  paths disagreeing.
- Rule 11: for a folder that a filtered user may not list, but whose entry
  they see, `writes:>=1 folder:` does not match it and no value is sent.
  It watches a leak of activity in a folder the user cannot open.
- The exit code: 2 for a `writes:` test with the flag, 0 for `--writes` with
  the flag.

## Stage 3: the window shows writes

1. **Writes and Written columns** in Search's results list, hidden by
   default, as Date created is. Writes shows the count, Written the bytes in
   the Size column's unit, with "up to" or "at least" before it when rule 4
   marked it ("at least" when it has both marks), and an empty cell for none
   or unknown, so a list sorted by either reads from the busiest down. A
   first click on either header sorts most first, and View > Sort by gains
   both. The inspector line (item 2) and the help explain the values, because
   the results list has no header tooltips.
2. **The inspector** gains one line for one entry with writes, "Writes",
   such as "412 in the last hour, in 11 of its 12 five-minute periods, up to
   1.9 GB, replaced". The second part is the steps with a write, which is
   what the warning's rule reads, and the last part names the kinds of rule
   4: added to the end, replaced, written over in place, or several of them. It is where a person learns how a file was
   written; no column holds it.
3. **The window asks for the values** while either column or the inspector
   line is shown, as it asks for Date created only while that column is
   shown (`fetch.rs`, `want_created`): 14 bytes a row and one lookup a row
   sent.
4. **A row shows the values of the moment its chunk was read,** as it shows
   the size of that moment. The order is the search's, and `F5` searches
   again (see "Not planned" in `docs/DESIGN.md`).
5. **The status bar says when values may be low.** While the list shows
   either column, is sorted by one, or its search tests one, and a page
   carried the flag, the right group shows "Some writes may be counted
   low", as it shows the index state when that affects the answer.
6. **Strings** in `en.json`, `es.json`, `nl.json` and `tr.json`.

Screenshot for the owner: the results list sorted by Written, with Writes,
Size and Date modified beside it, and the inspector of one file.

## Stage 4: folders copied into themselves

A test or build step that copies a folder into a folder inside it, again and
again, leaves `a\a\a\a`, `a\b\a\b\a\b` or `a\b\c\a\b\c\a\b\c\a\b\c`. In
general, a copy of `S` into `S\p1\…\pm` makes the path below `S` repeat a
group of `m` names (the contents of `S` were copied) or `m + 1` names (`S`
itself was copied), so the group can have any length. It fills the disk, slows every walk of the tree, and the counts of stages 1 to 3
do not show it, because no file in it is written twice. The tail warns when
such a copy happens, and never about an old tree. A search term finds old
trees by hand, and never warns.

1. **The test.** One function owns the rule "the path ends in the same group
   of names N times", for a group of any length. The tail's detector (item 2)
   and `repeats:` (item 7) both call it. It works in two passes over the
   folder's chain of names, read upward from the folder to the root: `r[0]`
   is the folder's own name, `r[1]` its parent's, and so on to `r[d - 1]`.
   - **Pass 1, the walk.** It reads the chain into a buffer, never more than
     `MAX_PATH_HOPS` names, as `VolumeIndex::depth` does (`volume.rs`). In
     the same pass it counts how often `r[0]` occurs. A path that repeats a
     group N times holds the folder's own name N times, so a folder whose
     name occurs fewer than N times has 1 copy, and the test stops there. On
     almost every folder, this one walk of `d` parent reads and `d` name
     comparisons is the whole cost.
   - **Pass 2, the Z-function.** Only a folder that passes runs the
     Z-function (Gusfield's Z-algorithm) over the chain. `z[k]` is how many
     names, from the folder upward, equal the name `k` levels above them. It
     gives every group length `k` at once, in one pass of at most `2d` name
     comparisons, so no group length costs a walk of its own. The group of
     length `k` repeats `1 + z[k] / k` whole times (integer division), and
     its run reaches `k + z[k]` names up. Among the `k` from 1 to `d / N`
     that give at least N copies, the test answers the one whose run
     reaches highest, and the smallest `k` on a tie, with its copies. The
     tie rule picks the group itself, not a group made of two of its
     copies: `a\a\a\a\a\a\a\a` is `a` 8 times, not `a\a` 4 times, because
     both runs reach 8 names. The height rule keeps one answer for one
     tree when the group itself holds N equal names in a row: for `x`
     copied into `x\a\a\a\a`, the group `a\a\a\a\x` reaches higher than the
     `a` 4 times inside each copy, so every create answers the group of 5.
     `repeats:` with a range answers the most copies any `k` gives.
   - **Memory.** The caller owns and reuses both buffers, the chain and the
     `z` values, each at most `MAX_PATH_HOPS` long (about 32 KiB together),
     so the test allocates nothing.
   - **The top of the run,** which the detector needs: the highest folder
     that the repeat reaches, `r[k + z[k] - 1]`. It includes the part of a
     group above the whole groups. In `r\a\b\a\b\a\b\a\b` the top is `r\a`,
     whether the create is the fourth `b` or a fifth `a` below it, so every
     create of one tree gives the same top.

   The test needs no state in the index: the chain is read when the test
   runs, so the index keeps no byte per folder for it. Names compare by name
   id first, then by `folded_bytes_of`, so a name that the journal added
   since the last rank merge still compares right, and case is ignored.
   This comparison is an equivalence (each name has one folded form), which
   is what the Z-function needs. Names never compare by
   `NameStore::name_key`: for a name without a rank it runs a binary
   search, and it gives two different unranked names the same odd key
   (`store.rs`). A parent that the index does not hold is a placeholder
   under lost+found (`resolve_or_placeholder_parent`), so such a chain ends
   at lost+found, and the test runs on the names it read.
2. **Detection in the tail.** In `apply_batch`'s `UsnOp::Upsert` arm, right
   after `upsert_link`, the tail checks a folder's record whose reasons hold
   both `USN_REASON_FILE_CREATE` and `USN_REASON_CLOSE`, whatever other bits
   they hold, tested with `&` as `usn::ops` tests every bit. Reason bits
   accumulate, so a create makes a record with `FILE_CREATE` and later one
   with `FILE_CREATE|CLOSE`. One open writes one `CLOSE` record, and only
   the open that created the folder has `FILE_CREATE`, so the record with
   both counts each create once. A tool that sets the new folder's dates,
   attributes or permissions on the same handle adds bits such as
   `USN_REASON_BASIC_INFO_CHANGE` to that record, and a test for "no other
   bit" would miss its folders; "Before stage 1" reads what robocopy and
   File Explorer write. The tail runs the test of item 1 on the new folder
   (`head_of`, `entries.parent`), with the buffer that the event list owns.
   The walk costs `d` parent reads for each folder created, with `d` the
   folder's depth, about 10 on the owner's `C:`; a copy that unpacks 10,000
   folders adds about 100,000 reads, a few milliseconds suspected, not
   measured, spread over its batches. Only a folder whose name occurs N times
   on its path runs pass 2. A create inside a copy that is already an event
   still runs the test, because the test gives the top that finds the event.
   A copy that has reached 300 levels costs 300 reads a folder, still
   microseconds, and the event makes it visible long before.
3. **Replay.** `apply_batch` does not know that it replays the changes made
   during a build, so the tail tells it. Today the tail keeps the end of a
   replay only for a volume built by a walk (`ReadPosition::replay_until`,
   `tail.rs`), so an NTFS volume has none. `ReadPosition` gains
   `build_end: Option<i64>`: the journal's next USN when the build ended,
   which `ReadPosition::at` reads for every filesystem as `walk_end` does.
   It becomes `None` when a read starts at or past it or finds nothing new,
   as `replay_until` does, so a journal that cannot say its next USN ends
   the replay at the first quiet read. It is a field of its own, because
   `replay_until` also starts again after a walk of a missed folder and
   decides `replaying_walk`. `Batch` (`index/settle.rs`) gains
   `detects_repeats`, true when `build_end` is `None`, beside
   `replaying_walk`.
   `apply_batch` takes the `Batch` in place of its records, so the flag
   reaches the `Upsert` arm with no new state in the index; its callers,
   `settle_batch` and the tests, pass a `Batch`. So a copy made during a
   build is not reported, and an event always uses the state of the moment.
4. **Intended limits.** The tail does not check a folder moved in (only its
   top is a `RENAME_NEW_NAME` record), a folder that the index missed (the
   walk inserts it with `insert_listed` in its own hold, `settle.rs`), and a
   create under a left-out folder (`Screen::Drop` drops it, `volume.rs`, as
   rule 10 says for counts).
5. **The event.** An event is one tree: it is identified by the top of the
   run (item 1) and the group length `k`, which this plan calls the period.
   When a create gives at least the daemon's
   constant number of copies, 4, the tail looks for the event with that top
   and period in the list. When there is one, it updates the event's
   copies, deepest folder and sequence number. When there is none, it makes
   one: the top, the deepest folder so far, the period, the copies, the time
   and a sequence number. "At least", not "exactly", so a tree that passed 4
   copies while no event could be made (during the replay of a build, before
   the service started, or after the list dropped its event) gets an event
   at its next create. The window's "copies" box has this constant as its
   lowest value, because the daemon cannot read `ui.json`. The list is a field of `VolumeIndex` with
   a fixed size of 64 events, written in the same hold as `apply_batch`, and
   moved through `Building::finish` (`volume_state.rs`) like the writes
   table. It stores file IDs; the reply resolves the paths and drops an event
   whose folder is gone. When the list is full, it drops the oldest event and
   counts how many it dropped.

   **The sequence number** is one counter for the whole service, so one
   number orders the events of every volume. gozlerd owns it: a `static`
   `AtomicU64` in the daemon, at 0 when the service starts, because there is
   one per run of the service. `Building::finish` gives the installed index's
   list a `&'static AtomicU64` to it, beside the old list it moves in, so
   `VolumeState::new` keeps its four parameters. An index that a build makes
   (`bootstrap.rs`) has a list without the counter until `Building::finish`
   installs it, and `apply_batch` runs only on an installed index, so no
   event is ever made without a number. A list without a counter records no
   event, so the tests and benches that call `apply_batch` on an index that
   was never installed are unchanged; the detector's test in `gozler-core`
   gives its index a counter the way `Building::finish` does. So `apply_batch` reaches the counter
   through the list it already writes, with no new parameter, and
   `gozler-core` needs only `std`. A new event and an updated event each take the next number (add 1,
   then read), so numbers start at 1 and never repeat within one run of the
   service. gozlerd also makes a **run id** when it starts: 64 random bits
   that are not 0 (`BCryptGenRandom`, as the result snapshot ids use). The
   counter and the events are not saved (rule 9), so the run id is what
   tells a client that the numbers started again.
6. **Travel,** as folder links travel (`Request::Status { links_held }` and
   the reply's `links: Option<Links>`, `proto/types.rs`). A `Status` request
   gains `repeats_after: Option<RepeatsAfter>`, with the fields `run` and
   `number`, an addition by rule 4 of "Additions and breaks"; `None` means
   that the client does not use it. The reply gains `repeats: Option<...>`,
   an addition by rule 1 of "Additions and breaks", with the service's run
   id, the counter's value now (the highest number given so far), the events
   to send and how many events were dropped. "After" is the service's
   sequence number, not a time, because the clock can be set back (rule 5).
   Which events the reply holds:
   - **`run` is 0:** none. This is a client's first check. It learns the run
     id and the highest number, and warns about nothing older.
   - **`run` is the service's run id:** the events whose number is above
     `number`.
   - **any other `run`:** every event the service holds. The service
     started again after that client's last check, so each of its events is
     newer than what the client has seen. Without the run id, a client that
     had seen number 12 would miss the new service's events 1 to 12.

   The client then keeps the reply's run id and highest number for its next
   check. It keeps the highest number, not the highest among the events it
   got, because a filtered user does not get every event. The service reads
   the counter before it reads any volume's list: an event takes its number
   inside its volume's write hold, so every event up to that value is in a
   list by the time the reply reads it under the read hold, and an event
   numbered later is sent again at the next check, where the window updates
   the row it already shows (stage 5, item 4). A
   filtered user gets only the events whose top's parent they may
   list, as `folder_links` filters by `may_list` (`volume.rs`).
7. **`repeats:`** stays as a search function, for looking by hand: `repeats:4`
   finds each folder whose path ends in the same group of folder names, of
   any length, at least 4 times in a row. It calls the test of item 1. It takes
   the comparisons and ranges of `child-count:`, so `repeats:>=4` and
   `repeats:4` mean the same, and `repeats:` alone means `repeats:>=2`. The
   prefilter of item 1 uses the lowest copies the range accepts, and 2 for a
   range that includes 1, such as `repeats:<4`: a folder that fails it has
   1 copy, and the range decides whether that matches. It
   matches folders only. It is a new `Test::Repeats { range }`, an addition
   by rule 2 of "Additions and breaks". It reads only the index, so it works
   on every volume and finds a tree made before gozlerd started. It never
   warns. Every folder below the first repetition matches too, because its
   path still ends in the copies above it, plus its own. It costs nothing
   unless a search holds it. On the owner's `C:` with 274,208 folders and a
   depth of about 10, pass 1 costs about 2.7 million parent reads and name
   comparisons; a `folder:` term before it skips every file. With
   `repeats:>=2`, pass 2 also runs on every folder whose name occurs twice
   on its path, such as each nested `node_modules`; it is linear in the
   depth too. A few milliseconds is
   suspected from that count, not measured. Stage 4 adds a criterion bench
   on a generated tree; a bench run is a build, which the owner starts, so
   the help, `docs/DESIGN.md` and the stage's report call the cost suspected
   until the owner has run it.
8. **Help and design:** the help, the Search syntax dialog and a paragraph in
   `docs/DESIGN.md`'s "Search", with the rule of item 1.

Tests:

- The test of item 1, over a fixture tree: the fourth copy of `a`, of `a\b`,
  of `a\b\c` and of a group of 7 names matches, and the third does not;
  `a` 8 times answers period 1 with 8 copies, not period 2; names that
  differ in case match; `src\src` is 2 copies; a folder named like its
  grandparent but not its parent (`a\b\a\c`) is 1 copy; the top of
  `r\a\b\a\b\a\b\a\b` is `r\a` from the fourth `b` and from a fifth `a`;
  `x` copied 4 times into `x\a\a\a\a` answers the group of 5 at each fourth
  `a`, not `a` 4 times. It watches the Z-function's bounds, the copies
  arithmetic and the choice of `k`, which an off-by-one breaks without any
  other test failing. A proptest compares the test with a plain check of
  every `k`, with the same choice rule, over random chains of 3 different
  names, where repeats are common.
- The detector: four creates make one event, a fifth updates it (copies 5)
  instead of making a second event and gives it a higher number; nine
  creates of `a\b\a\b…` leave one event for period 2, with the top `a`; a
  tree already at 6 copies with no event gets one at its next create; a
  create whose record also holds `USN_REASON_BASIC_INFO_CHANGE` counts; and
  a create during the replay makes none.
- Which events a `Status` reply holds: none for run 0, the events above
  `number` for the service's own run id, and every event for another run
  id. It watches a window that misses the events of a service that started
  again, which no other test and no compiler sees.

## Stage 5: Watch SSD wear

1. **The setting.** Settings > General gains a section, "SSD wear", in
   `ui.json`:

   | Control | Default |
   | --- | --- |
   | Check box: "Warn me when a program keeps writing the same files" | off |
   | Number box: "A file, written in an hour (GB):" | 1 |
   | Number box: "A folder, written in an hour (GB):" | 2 |
   | Number box: "A folder copied into itself, copies:" | 4 |
   | List: "Ignored files and folders", with Remove | empty |

   The check box starts off (owner decision 4). The two GB boxes' tooltip
   says "Bytes written in the last hour, for a file or folder written at
   least 5 times over more than 5 minutes", with the two 5s from the
   constants that the check uses: the least writes, and the length of a
   step. The copies box's lowest value is the daemon's
   constant (stage 4, item 5). The number boxes, the list and Remove are
   disabled while the check box is off, and Remove while no row is selected.
2. **The rules.** A file or a folder warns when it has at least 5 writes in
   the last hour, at least 3 steps with a write, and a Written that reaches
   its limit (owner decision 3). The 5 and the 3 are constants beside the
   function, not settings. The steps come with each row (stage 2, item 4);
   while values may be low they are a lower bound, so the rule can warn
   later but never wrongly (rule 13). A fault that starts now so warns
   at the first check after its third step begins, 5 to 15 minutes later. A
   folder copied into itself warns when its event (stage 4, item 5) has at
   least the number of copies set. One function in `gozler-ui`'s `core`
   decides all three from the rows and events of the check, the limits, the
   ignore list and the paths already warned, so unit tests cover it without
   a window.
3. **The check.** While the setting is on, the first window (the one with
   the tray icon) asks gozlerd every 5 minutes, through `app::work`, also
   while it is hidden: the search `writes:>=5 written:>=` the smaller limit,
   asking for the values (`Want::Writes`), and `Status` with `repeats_after`
   set to the run id and the highest number of the last reply (stage 4, item
   6). The first check after the window starts, or after the setting is
   turned on, sends run 0: it gets no events, keeps the reply's run id and
   highest number, and so warns about no copy made before it. The window
   keeps the two values in memory only. The search has no row limit: the
   table's size bounds it, and a limit would cut before the check's own
   rules, so ignored and already warned rows would fill the cut and hide the
   rest. It visits only the table's rows (stage 2, item 3), at most 65,536,
   so milliseconds are suspected, not measured. A row or an event under an ignored path is left out. A new
   event warns as "Copied into itself" with its copies. The window polls
   `Status` only while waiting can change the answer (`core/model.rs`), so
   this request adds the only periodic status traffic while the setting is
   on. A check that fails, because the service does not answer, shows nothing; the next one runs 5
   minutes later, and the window's own notice says what is wrong with the
   service when the person opens it.
4. **The warning** (owner decision 1) is one window of its own with a
   taskbar button (`WS_EX_APPWINDOW`, no owner, as a Viewer has), so it can
   show while the main window is hidden in the tray. It shows without taking
   the focus (`SW_SHOWNOACTIVATE`), and its taskbar button flashes until the
   person brings it to the front (`FlashWindowEx` with `FLASHW_TRAY` and
   `FLASHW_TIMERNOFG`). Its title is "SSD wear". Windows draws all of it:
   - a line of text: "These were written again and again in the last hour.
     The folder usually names the program. Resource Monitor's Disk tab shows
     the process.";
   - a list view in report mode with the columns Name, Folder, Writes and
     Written, where Written reads as in the results list. A folder copied
     into itself shows the top of its run (stage 4, item 1), "Copied into
     itself" under Writes and its copies under Written, such as "4 copies";
   - the buttons Show, Ignore and Close. Show runs Show in Commander for the
     selected row. Ignore adds the selected row's path to the ignore list and
     removes the row. Show and Ignore are disabled while no row is selected.

   While the window is open, a later check adds its rows in place, updates
   the copies of a row it already shows, and flashes the button again;
   nothing is cleared and filled again.
5. **Each path warns at most once a day,** while Gozler runs, so a file that
   stays busy is not reported every 5 minutes.
6. **The ignore list** (owner decision 2). A folder ignores everything below
   it. The check compares full paths in `core`, without case, so it adds no
   term to its searches.
7. **What the watch cannot see is said where the person turns it on.** When
   Apply turns the setting on:
   - while "Start when I sign in to Windows" or "Keep running in the
     notification area after I close the window" is off, a question says
     that Gozler warns only while it runs and asks whether to turn them on,
     with Yes and No, and builds the two captions from the check boxes'
     own strings;
   - while the Include settings leave anything out, the section shows "Not
     watched: what Include leaves out.";
   - for a filtered user (`Status`'s `caller_sees`), the section shows "Only
     the folders you can open are watched."

   The two lines show only in those states, so the section has no note that
   is shown just in case.
8. **Strings** in the four catalogues.

Tests, for the function of item 2, in one test for each: a file with 4
writes and 3 GB does not warn and one with 5 writes in 3 steps does; a
folder with 10 writes and 3 GB in 2 steps does not warn, which watches a
return to a warning for every copy; a file of 5 writes under the limit does
not warn; a folder over its limit warns; a row under an
ignored folder does not warn; a path warned less than a day ago does not warn
again.

Screenshots for the owner: the settings section with the setting on, and the
warning with one file and one folder.

## After install

- **On a PC with an affected NVIDIA driver (621.14 or 621.24):** whether
  `nvlog.nvlgstg` shows writes, and how Written reads. Writes and a Written
  near what Resource Monitor shows means the plan finds it. Few writes and a
  low Written while Resource Monitor shows GBs means the container holds the
  file open, and the next step is in "Ideas considered".
- **The counts, once, with the installed build.** "Before stage 1" already
  read the journal's records for these actions, so this checks only that
  the values follow them: save a 1 MB file twice from Notepad and see Writes
  go 1, 2 and Written read "up to"; add a line with `echo x>>file` and see
  Writes go up by one and Written by 1 KiB, exact (rule 6 rounds up); save a
  file from a program that saves through a temporary file and see the name's
  Writes go up by one.
- **On a ReFS volume, such as a Dev Drive:** the same three steps, when
  "Before stage 1" was not run there.
- **A folder copied into itself:** run `robocopy a a\a /e` four times in a
  test folder while Gozler runs. The warning appears once and updates. An old
  copied tree made before the service started never warns, and
  `gozler folder: repeats:4` finds it.
- **The limits:** `gozler -sort written --writes --json -n 30` a few times
  over a day on the owner's PC, and the defaults of owner decision 3, the
  least steps with a write included (`write_steps`), set from what it shows.
  Then the owner decides whether the setting starts on (owner decision 4).
- **One copy does not warn:** copy 10 large files into one folder while the
  setting is on, and see no warning.
- **The warning** with Windows notifications turned off and the window
  hidden in the tray.
- **The 5-minute check** while the window is hidden, with Task Manager open:
  no CPU between checks.

## Ideas considered

Kept:

- **Count by name, not by file ID.** It is what makes "the same file again
  and again" true for programs that save through a temporary file.
- **Two values, Writes and Written, and a limit that needs both, spread
  over at least 3 of the hour's 12 steps.** Writes over several steps is
  "again and again"; Written is the wear. The steps cost nothing to keep,
  because each row already holds a count for each step, and they keep one
  copy of many files from warning. An append counts as a write,
  and its bytes are the growth, exact, so a transcript appended all day
  stays under the limit and a log that grows by GBs warns.
- **The folder's own values.** They find the folder that fills with dumps,
  and the folder where temporary files are created and deleted all day.
- **Detection of a copy into itself in the tail, and `repeats:` for looking
  by hand.** A folder create happens once for each folder, so the tail
  checks it then: it warns only when a copy happens, and never about an old
  tree. One walk up the new folder's path and the Z-function find a repeat
  of any group length, in time linear in the folder's depth, which is the
  least a test without a state for each folder can cost, because it must
  read the path. `repeats:` costs nothing until it is asked for, and finds a tree
  made before gozlerd started; it never warns.

Not in this plan, and why:

- **A folder's count summed up to the root, as its size is.** `C:\`,
  `Users` and `AppData` would lead every list and every warning, each record
  would walk up its folders under the write lock, and a filtered user could
  be shown none of it.
- **Reading the last hour of the journal when gozlerd starts.** It reads up
  to 1 GiB a volume at each start, to fill an hour that matters once, after
  an update. After Windows starts there is no hour before. "Counted since"
  already gives the partial answer.
- **A limit on the bytes written to a drive** (`IOCTL_DISK_PERFORMANCE` on
  the volume handle that gozlerd holds, or the drive's SMART host writes). It
  is exact and it also sees a file that is held open. But the owner's `C:`
  takes 5.5 GB an hour in normal use, inside the range reported for NVIDIA's
  fault, so no limit separates the fault from the work. SMART also gives only
  a lifetime total, in units that differ between vendors.
- **The process that writes** (a kernel file trace with ETW, or each
  process's write counter). The trace is what Resource Monitor uses: it
  names the process, the file and the bytes, also for a file held open. It
  costs an event for every file operation on the PC while it runs, and a
  trace reader in the elevated service. The counters are cheap, but they
  also count pipes and the network, and they name no file. If "After
  install" shows that NVIDIA's container holds its file open, a short trace
  that the person starts from the warning is the next plan, because nothing
  the journal holds can show such a file.
- **Writes times size for every write.** 300 appends to a 30 MB log would
  read 9 GB. The growth is the true figure, and gozlerd reads the size
  anyway.
- **A column for each kind of write, opens, and changes of dates,
  attributes and permissions.** See "What a person sees, and what the
  warning reads".
- **Comparing what was written with what was there,** to find the same data
  written again. gozlerd would have to read every written file, which is the
  load this feature warns about.
- **A test for each group length, 1 to 3.** It walks once for each length
  and misses a copy into a folder 3 or more levels below its source. The
  Z-function gives every length in one pass.
- **A copy into a new name each time,** such as a script that copies `out`
  into `out\run_2` and then into `out\run_3`. The path holds no repeated
  group (`out\run_3\run_2\run_1`), so only a comparison of whole trees could
  find it, and that needs a hash of each folder's names below it, kept
  current at every change. The bytes it writes still show as the folder's
  Written (stage 1).
- **A state kept for each folder, so a create costs one step, not a walk.**
  It costs bytes for every folder, and the walk of `d` parents already costs
  microseconds.
- **A learned baseline of what is normal on this PC.** It needs days of
  saved counts, and it would learn a fault that was there on the first day
  as normal. Ignore gives the person the same result with one click, and
  they see what they ignore.
- **A list of the changes themselves** (Everything's Recent changes). This
  plan keeps counts only.
- **Opens.** An open that changes nothing writes no record, so gozlerd
  cannot count it, and it does not wear the disk.
