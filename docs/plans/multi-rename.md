# Multi-Rename

**Scheduled:** the next feature, item 12 of
[Commander: what to add next](commander-features.md). Not started. Written
2026-11-14 from SynapseCommander's draft
`docs/specs/multi-rename.md` (2026-11-08), with Total Commander 11.58 (TC) as
the reference. When it ships, its rules move into `DESIGN.md` and the help,
and this plan is deleted.

Multi-Rename gives many entries new names in one step. The person writes a
rule, such as "the old name, a dash and a number", sees every new name before
anything changes, and starts the renames. The engine does the renames, never
replaces a file, and can reverse them.

This is TC's Multi-Rename tool (`cm_MultiRenameFiles`, `Ctrl+M`), because
Commander mode behaves as TC does (`AGENTS.md`, "The window"). A TC user's
masks work unchanged, except for the placeholders that "Left out" lists.

## Evidence

| Tag | Source |
| --- | --- |
| `[help]` | TC's help topic "Dialog box - Multi-rename tool", in `TOTALCMD.CHM`; `[help:<file>]` is another topic |
| `[history:<date>]` | an entry of TC's `HISTORY.TXT` |
| `[strings:<id>]` | TC's string table, read with REA from the resources of `TOTALCMD64.EXE` |
| `[live:<capture>]` | the sandbox TC, 2026-11-08, in `C:\SynoSoftware\SynapseCommander\artifacts\total-commander\captures\multi-rename\` |
| `[live2:<capture>]` | the sandbox TC, 2026-11-14, in `…\captures\multi-rename-gozler\` |

The captures are on the owner's machine only, because Git ignores
`artifacts`. TC's licence forbids decompiling it, so the evidence comes from
its help, its files, its resources and the running program, never from its
code. Two questions were checked live on 2026-11-14, because nothing else
answered them:

- **Undo does not survive a restart.** Two batches, then Undo, offered the
  second batch; Undo again offered the first [live2:undo1] [live2:undo2].
  After TC restarted, Undo was disabled and the name mask was `[N]` again;
  the mask history was still in the drop-down, saved in `[RenameTemplates]`
  of `wincmd.ini` [live2:r2].
- **`..\` in the mask moves the file up a folder.** `..\[N]` on
  `mrt-up\sub\c.txt` showed `..\c.txt` and moved the file to `mrt-up\c.txt`
  [live2:up3].

## Decisions

Each decision is settled. An agent made them on 2026-11-14, from the evidence
and gozler's rules, because each one is a technical or usability choice.
The owner can reverse any of them.

1. **A dialog that stays open after Rename,** as TC's does. The renamed rows
   leave the list; the rows that failed stay, so Rename tries them again;
   Undo is one key away [live:04-after-enter] [live:08-failed-in-bold].
2. **`Ctrl+M` in both modes,** and File > Multi-Rename…. Search's selection
   and a search panel's work too, so found files are renamed in one step;
   TC needs Feed to listbox first.
3. **A folder has no extension.** `[N]` is a folder's whole name and `[E]` is
   empty, as a panel's Ext column is empty for a folder. TC splits
   `My.Folder` into `My` and `Folder` unless the mask starts with `[I]`
   [help] [live]; gozler accepts `[I]`, `[I0]` and `[I1]` and ignores them,
   so TC masks keep working.
4. **A new name that is in use is numbered in the preview,** as
   `name (2).ext`, with gozler's Keep both numbering. TC asks at Start! and
   numbers only when the person ticks a box, and its preview does not show
   the numbers [strings:1404] [strings:1415] [help]. In a folder panel the
   preview shows what will happen, so no question is needed, and nothing is
   ever replaced. A name in use in a folder that the preview does not list,
   as in Search, is numbered by the engine, and the row shows the name it
   got. File Explorer numbers the same way when it renames several files.
5. **A row with a problem keeps its name, and the other rows are renamed.**
   TC skips a file that has no date taken [history:31.12.21]. The row says
   why, and the status line counts these rows.
6. **Undo reverses the last 20 batches, one per press, newest first, while
   the window runs.** TC does the same [history:09.08.16] [live2:undo2],
   and loses them at a restart [live2:r2]. Undo never replaces a file. It
   reverses Multi-Rename batches only; it is not the general Undo that the
   roadmap leaves out.
7. **A batch rename does not wait for device admission,** as Delete and
   New folder do not (D14), because it changes only names. TC renames at
   once while a copy runs. Move's same-volume rename keeps its admission.
8. **Recovery never leaves a temporary name.** Continue finishes the batch;
   Dismiss gives each entry that holds a temporary name its old name back,
   because the person never chose that name.
9. **The selection follows the renamed entries.** When the dialog closes, a
   folder panel selects the new names that are in its folder, so a second
   rule is `Esc`, `Ctrl+M`, and `F5` copies what was just renamed. This
   extends the rule that already moves the focus and the selection with an
   `F6` rename. TC's "next step" (`F5` in the tool) is left out.
10. **Names by hand use an Edit names dialog with one name per line,** in
    which the person types or pastes. TC sends the names to an external
    editor and reads them back [help].
11. **Windows words replace TC's symbols:** Match case for `^`, First only
    for `1x`, Extension too for `[E]`, Regular expression for RegEx, Whole
    name for Subst.
12. **Date taken comes from Windows' property system,** the values that File
    Explorer shows as Date taken (`System.Photo.DateTaken`) and Media created
    (`System.Media.DateEncoded`). TC reads EXIF itself [history:31.12.21]
    [history:06.05.24]; an own parser would be untrusted-bytes code to
    maintain for the same answer.
13. **Words follow gozler's search rule:** a word starts at the beginning of
    the text or after a character that is not a letter or a digit. TC added
    `\` and `/` as separators in 2021 [history:29.09.21].
14. **A range past the end of a name gives the characters that exist,** and
    an empty text when there are none. A rule that works on most names keeps
    working on short ones.

## Left out

Each item below is a placeholder or a command that can be added later
without changing anything else. They stay out of the first delivery because
none of them serves a common task that the rest cannot do.

| TC | Task | Why not now |
| --- | --- | --- |
| `[2-5]` | a range of the full path | `[P]`, `[G]` and `[B…]` with ranges name the folders |
| `[c]`, `[c2]`, `[c3]` | "File 3 of 12" | rare |
| `[T2]`, `[T3]` | the time now | rare; the date a file was renamed is not a property of the file |
| `[S]` | the size in bytes | rare |
| `[%x%]`, `[X]`, `[X+]` | an environment variable, the clipboard | rare; paste the text into the mask instead |
| `[u]` | join separated accents | rare outside files from macOS |
| `[=plugin.field]` | a content plug-in field | gozler has no plug-ins |
| `MULTIRENAME=<name>` | run a saved rename from a button | gozler has no button bar |
| Result list | a log of the renames | Undo and the Report cover the reason for it |
| `F5` next step | apply a second rule before renaming | decision 9 |
| Load names from file | names from a text file | paste them into Edit names |
| Restart as administrator | rename where Windows denies it | Move to folder… does not offer it either; a window that runs as administrator renames there |

## Tasks

Each row is a task in the person's words. "Same" means the same keys and
result as TC; the other verdicts say what differs.

| # | Task | TC | Gozler | Verdict |
| --- | --- | --- | --- | --- |
| 1 | Rename the selected entries, or the focused one | `Ctrl+M` opens the tool; with nothing selected it takes the focused entry; on `..` it says "No files selected!" [live:02-mrt-focused-only] | `Ctrl+M` opens Multi-Rename on the selected entries or the focused row; disabled on `..` | Same |
| 2 | Rename search results | Feed to listbox, then `Ctrl+M` [help:dlg_search.htm] | `Ctrl+M` in Search or in a search panel | Better: one step less |
| 3 | See every new name first | The list updates with each key; nothing changes before Start! [help] | The same, through the work owner | Same |
| 4 | Build the name from parts of the old name | Name mask, ranges, padding [help] [history:12.03.26] | "The mask" | Same |
| 5 | Pick a range by selecting it | `[N#-#]` opens a range picker [history:11.06.26] | Range… | Same |
| 6 | Change the extension apart from the name | Extension mask [help] | Extension box | Same |
| 7 | Number the files | `[C]` with start, step, digits, letters and steps every n files [help] | "The counter" | Same |
| 8 | Choose the numbering order | Sort by a column; move rows with `Shift+Up`/`Shift+Down` or a drag [help] | Sort by a column; move rows with `Alt+Up`/`Alt+Down`, as the transfer queue does | Fine: `Shift` with an arrow selects rows in gozler's lists |
| 9 | Insert the number of files | `[c]` [history:19.06.19] | Left out | Fine: rare |
| 10 | Insert the date and time | `[Y][M][D][h][m][s][v]` and more, from Date modified [help] | The same | Same |
| 11 | Name photos and videos by the date taken | `[T4]`, EXIF, HEIC, MP4; no date skips the file [history:31.12.21] [history:06.05.24] | `[T4]` from Windows' Date taken and Media created | Same |
| 12 | Correct a camera clock | `[T1+60m]` [history:28.12.21] | The same | Same |
| 13 | Insert the time now | `[T2]`, `[T3]` [help] | Left out | Fine: rare |
| 14 | Insert a folder of the path | `[P]`, `[G]`, `[B0]`…`[B9]`, `[B+0]`…`[B+9]` [history:16.03.22] | The same | Same |
| 15 | Insert the size | `[S]` [history:07.01.19] | Left out | Fine: rare |
| 16 | Insert an environment variable or the clipboard | `[%x%]`, `[X]` [history:15.05.26] | Left out | Fine: paste the text |
| 17 | Insert metadata, such as an artist | Content plug-ins [help] | Left out | Fine: no plug-ins |
| 18 | Change upper and lower case | Case list and `[U][L][F][f][n]` [help] [history:13.05.18] | The same | Same |
| 19 | Join separated accents | `[u]` [history:18.09.16] | Left out | Fine: rare |
| 20 | Replace text in every name | Search and replace, pairs apart by `\|`, `*`, `?`, case, first only, in the extension too [help] | The same, with Windows words for the options | Same |
| 21 | Replace with a regular expression | RegEx, groups, Subst., `\U \L \F` [help] [history:31.05.23] | The same | Same |
| 22 | Sort files into folders by their names | `\` in the mask; missing folders are created; `..\` goes up [help] [live2:up3] | The same | Same |
| 23 | Rename folders whose names hold dots | `[I]` [history:29.05.23] | Always so (decision 3) | Better: no switch to remember |
| 24 | Type or paste the new names | External editor or a file (`F10` menu) [help] | Edit names | Better: no editor round trip |
| 25 | Apply a second rule | `F5` loads the new names [help] | `Esc`, `Ctrl+M` on the renamed entries (decision 9) | Fine: one key more, and each step can be undone |
| 26 | Leave some files out | `Delete` removes a row [help] | The same | Same |
| 27 | Reuse a rename | `F2` saved settings; field history [help] [live2:r2] | Saved renames (`F2`) and field history | Same |
| 28 | Run a saved rename from a button | `MULTIRENAME=<name>` [history:28.02.22] | Left out | Fine: no button bar |
| 29 | Undo a batch, also later | Undo, `Alt+U`; 20 batches; asks first; lost at restart [history:09.08.16] [live2:undo2] [live2:r2] | The same | Same |
| 30 | Keep a log of the renames | Result list [help] | Left out | Fine: Undo and the Report |
| 31 | Leave the tool | Close, or `Esc` from anywhere [live] | The same | Same |
| 32 | Two new names are equal, or a new name is in use | Start! warns and offers numbering; the preview does not show it [strings:1404] [strings:1415] [history:18.03.22] | Numbered in the preview; in a folder that the preview does not list, by the engine (decision 4) | Better: no question |
| 33 | Names swap, `1.txt` ↔ `2.txt` | A temporary name [history:01.02.23] | The engine's order and one temporary name | Same |
| 34 | A name appears after the preview | Warns [history:01.02.23] | The engine numbers it; the row shows the name it got | Better: no question |
| 35 | Some renames fail | The others go on; the failed rows stay, in bold, and Start! tries them again [strings:1405] [live:08-failed-in-bold] | The same, with the reason in the row | Same |
| 36 | Only the case changes | Works, not a collision [live:03-case-only-preview] | The same | Same |
| 37 | A name is too long or has a forbidden character | `<Error!>` in the list [help] [strings:1402] | The row says why and keeps its name | Better: it says why |
| 38 | Windows denies the rename | A failure, with OK only [live:07-access-denied] | A failure, with Windows' reason | Same |
| 39 | Gozler stops in the middle | Nothing is recorded [help] | Interrupted, with Continue and Dismiss (decision 8) | Better |
| 40 | A copy runs on the same disk | Renames at once | Renames at once (decision 7) | Same |

## How it works

### Opening

- **Multi-Rename…** is a command of both modes: `Ctrl+M`, its default
  shortcut, and File > Multi-Rename…, after Move to folder…. It is enabled
  when an entry is selected or focused, as Copy to folder… is, and disabled
  in a second window, which owns no file operations.
- **It acts on the selected entries, or on the focused row,** never on `..`.
  Search's limit of 100,000 selected entries applies, so the dialog never
  holds more rows than that.
- **The dialog opens at once.** It takes each entry's full path, name, kind,
  size and date modified from the rows the list already holds, and reads
  nothing from the disk to open.
- **The fields start as the person left them** while the window runs, and as
  the defaults after Gozler starts: name `[N]`, extension `[E]`, empty search and
  replace, Extension too on and the other boxes off, Case Unchanged, counter
  start 1, step 1, digits 1 [live2:r2]. The name box has the focus, with its
  text selected [history:24.04.12].

### The dialog

The title bar says Multi-Rename. The dialog is modal and resizable; the list
takes the extra room, and the size is saved in `ui.json`. Windows draws every
control.

| Area | Controls |
| --- | --- |
| Name | an editable combo box with the name mask and its history; buttons Name, Range…, Counter, Date, Time and More |
| Extension | an editable combo box with the extension mask and its history; buttons Extension, Range…, Counter |
| Search and replace | Search for and Replace with, editable combo boxes with history; check boxes Match case, First only, Extension too, Regular expression, Whole name |
| Case | a drop-down list: Unchanged, lowercase, UPPERCASE, First letter uppercase, First Letter Of Each Word, Title case (English) |
| Counter | Start at, Step by, and Digits, a drop-down list of 1 to 10 and Auto |
| Saved renames | a drop-down list, `F2` (see "Saved renames") |
| The list | a virtual list view: Name, New name, Note, Size, Date modified, and Folder when the entries come from more than one folder |
| Bottom | the status line at the left; Edit…, Undo, Rename (the default) and Close at the right |

- **A placeholder button inserts at the caret** of the box it belongs to,
  and replaces the selected text [history:06.05.26] [history:06.02.15].
  Its caption is one word, as the button captions ruling in `AGENTS.md`
  asks, and its tooltip names the placeholder, such as "Insert [N], the name
  without its extension". The placeholder then shows in the box, so the
  person learns it. Date inserts `[YMD]` and Time `[hms]`. More opens a menu
  of every other placeholder with what it inserts.
- **Range…** opens a dialog titled Range with the focused row's name in a
  read-only edit box and a check box, Count from the end. The person selects
  characters; OK reads the selection and inserts the range, such as
  `[N3-6]`, or `[N-8-5]` when counted from the end.
- **Each check box and list** changes the new names at once, as each key in a
  box does.

### The mask

The name mask and the extension mask are text with placeholders in square
brackets. Text outside brackets goes into the new name as it is. The new name
is the name mask's result, then a dot and the extension mask's result when
that result is not empty. Placeholders are case-sensitive: `[M]` is the month
and `[m]` the minutes.

**Names and folders**

| Placeholder | Inserts |
| --- | --- |
| `[N]` | the name without its extension; a folder's whole name |
| `[E]` | the extension without its dot; empty for a folder |
| `[A]` | the whole name, with its extension |
| `[P]` | the name of the folder that holds the entry |
| `[G]` | the name of the folder above that |
| `[B0]` … `[B9]` | a folder counted up from the entry: `[B0]` is `[P]`, `[B1]` is `[G]` |
| `[B+0]` … `[B+9]` | a folder counted down from the root: `[B+0]` is the drive letter without its colon, or the server of a share; `[B+1]` the first folder |
| `[[]`, `[]]` | `[` and `]` |
| `[I]`, `[I0]`, `[I1]` | nothing (decision 3) |

**Ranges.** Each of `[N]`, `[E]`, `[A]`, `[P]` and `[G]` takes a range after
its letter; `[B…]` takes one after `_`, as `[B1_2-5]`. Positions count
characters from 1. Each Unicode character counts once, also one that Windows
stores as two UTF-16 units, such as an emoji [history:09.06.26].

| Range | Characters |
| --- | --- |
| `[N3]` | the 3rd |
| `[N-3]` | the 3rd from the end [history:12.03.26] |
| `[N2-5]` | 2 to 5 |
| `[N2,5]` | 5 characters from the 2nd |
| `[N2-]` | from the 2nd to the end |
| `[N-8,5]` | 5 characters from the 8th from the end |
| `[N-8-5]` | from the 8th from the end to the 5th from the end |
| `[N2--5]` | from the 2nd to the 5th from the end |
| `[N-5-]` | from the 5th from the end to the end |
| `[N02-9]`, `[N 2-9]` | 2 to 9, filled from the left with zeros or spaces to 8 characters when fewer exist |

A range past the end gives the characters that exist (decision 14).

**The counter**

| Placeholder | Inserts |
| --- | --- |
| `[C]` | the counter, with the Counter fields |
| `[C10+5:3]` | the counter starting at 10, stepping by 5, 3 digits; each part is optional, as `[C10]`, `[C+5]`, `[C:3]`, and the step can be `-5` |
| `[C:a]` | digits from the largest value the counter reaches in the list, as Auto does |
| `[C+1/100]` | a counter that steps once every 100 rows, such as for `[C+1/100]\[N]` |
| `[Caa+1]` | letters: `aa`, `ab` … `az`, `ba`; the start sets the width |

Every row of the list takes the next value in the list's order, including a
row that keeps its name, so one row's problem never renumbers the others. A
placeholder with its own settings ignores the Counter fields. Fewer digits
than the value needs show the whole value.

**Dates and times**

| Placeholder | Inserts |
| --- | --- |
| `[Y]`, `[y]` | the year, 4 or 2 digits |
| `[M]`, `[D]` | the month and the day, 2 digits |
| `[h]`, `[H]` | the hour, 00 to 23 or 01 to 12 |
| `[m]`, `[s]`, `[v]` | minutes and seconds, 2 digits; milliseconds, 3 digits |
| `[i]`, `[i1]` | `am` or `pm`; `a` or `p` |
| `[d]`, `[t]` | the date and the time in Windows' short formats, with `/` as `-` and `:` as `.` |
| `[YMD]`, `[hms]` | several date letters in one bracket insert one after another |
| `[T1]` | the date fields after it read Date modified; this is the default |
| `[T4]` | the date fields after it read Date taken, or Media created for a video (decision 12); an entry without one keeps its name, and its row says "No date taken" |
| `[T1+60m]`, `[T4-2h]` | the same, moved by a number of `s`, `m`, `h`, `D`, `M` or `Y` [history:28.12.21] |

Dates are local time, as the list shows them. Date taken is read once for
each entry, on the work owner, only when a mask uses `[T4]`.

**Case placeholders** change the case of the text the mask makes after them:
`[U]` uppercase, `[L]` lowercase, `[F]` the first letter of each word
uppercase and the rest lowercase, `[f]` the same with English title rules,
`[n]` back to the case the text has. Words follow decision 13. English title
rules keep these words lowercase, except as the first or the last word: a,
an, the, at, by, for, in, of, on, to, from, up, and, as, but, or, nor, yet,
so, vs.

A mask that cannot be read, such as `[Q]` or a `[` with no `]`, gives no new
names; the status line names the first error.

### Search and replace

Search and replace changes the result of the masks.

- **Search for** finds text without regard to case, or with it when Match
  case is on. It does not ignore accents, so `ä` can be replaced by `ae`.
  `*` stands for any characters and `?` for one.
- **Several pairs** are separated by `|`: `ä|ö` with `ae|oe`. A pair without
  a replacement replaces with nothing.
- **First only** replaces the first match of each pair. **Extension too**
  also searches the extension; off, only the name changes.
- **Regular expression** reads Search for in the syntax of Search's
  `regex:`, and Match case applies to it as above. Replace with takes `$1`
  to `$9` and `${name}`. `\U`, `\L` and `\F` before a group make that group
  uppercase, lowercase or first letter uppercase, and `\n` keeps its case;
  each acts on the next group only [history:31.05.23]. **Whole
  name**, available only with Regular expression, replaces the whole name
  with Replace with when the expression matches, and keeps it when it does
  not.
- A regular expression that does not parse gives no new names; the status
  line says why.

### Case

The Case list changes the new name last. Lowercase and UPPERCASE change the
whole name, the extension included. First letter uppercase, First Letter Of
Each Word and Title case (English) change the name without its extension,
because an extension such as `Jpg` is never wanted.

### Order of the steps

For each row: the masks, then search and replace, then Case, then the checks
of "Names that cannot be used", then numbering. A row whose result equals its
name keeps its name and its Note says "No change".

### Folders in a new name

A `\` in the result separates folders below the entry's folder; the missing
ones are created [help]. `..` goes up a folder [live2:up3]. A trailing `\` is
removed [history:04.10.11]. A rename stays on the entry's drive: a name that
goes above the drive's root cannot be used. A new path that reaches another
volume through a mount point is found by the engine, which fails that entry
(rule 1 of "What the engine does").

### Names that cannot be used

Each folder name in the result follows "Names the person types" in
`DESIGN.md` (`core::typed`): trailing dots and spaces are trimmed, a device
name such as `NUL` and a character that no name holds cannot be used. A name
longer than 255 characters, an empty name, a path that starts with a drive,
`\` or `\\`, and an empty folder name such as `a\\b` cannot be used either. The
row keeps its name, and its Note says why (decision 5).

### Numbering

Names are compared without regard to case, as NTFS compares them.

- **Two rows that get the same new name:** the first in the list's order
  gets it, and each later one gets the first free numbered name,
  `name (2).ext`, then `(3)`.
- **A new name that an entry outside the batch holds,** or an entry of the
  batch that keeps its name: numbered the same way. The preview knows the
  rows of a folder panel's folder; for other folders the engine finds the
  name in use when it runs.
- **Not in use:** a name that another row of the batch gives up, as in a
  swap or a shift (`1→2`, `2→3`), and a change of case only.
- The preview calls the engine's numbering function, so the two never
  number differently. A numbered row's Note says "Numbered".

### The list

- **The list starts in the order the entries had,** in the panel or in
  Search, so the counter follows what the person saw. A click on a column
  header sorts by it, and a second click reverses the order. `Alt+Up` and
  `Alt+Down` move the focused row. `Delete` removes the selected rows from
  the list, not from the disk, and the context menu calls it Remove from
  list.
- **Names are computed through the work owner.** A list of up to its inline
  size of 1,000 rows is computed at once; a larger one keeps its previous
  names while the work runs, and a newer key press cancels the older work.
  Rename is enabled only when the names shown belong to the fields shown.
- **The status line** counts what Rename will do, and never hides the known
  counts behind a problem in some rows: "9 of 12 entries get a new name. 2
  are numbered. 1 can't be renamed."

### Rename

- **Rename** is enabled when at least one row gets a new name. It submits one
  operation with the rows that get a new name, in the list's order, and
  leaves out the rows with No change or a problem.
- **While it runs,** the fields, the list and Rename are disabled, and the
  status line counts: "Renaming… 4,210 of 10,000". Close closes the dialog;
  the operation goes on.
- **The operation shows as a rename does now,** with Move's transfer
  presentation, and the batch takes its entries as a move does: they leave
  the folder panels and show struck through in the search lists until the
  lists read again. A failure goes to the operation's Report.
- **When it ends with the dialog open,** the renamed rows leave the list.
  Each row that failed stays, with "Couldn't rename: " and Windows' reason in
  its Note, and keeps the new name it had, so the counter does not number it
  again; a change to a field computes its name again. A row that the engine
  numbered shows the name it got. The status line says "Renamed 98 entries.
  2 couldn't be renamed.", and Rename tries the failed rows again.
- **When the dialog closes,** a folder panel selects the renamed entries that
  are in its folder, under their new names, and focuses the new name of the
  entry it had focused (decision 9). It waits for the names as it waits for
  the name of an `F6` rename now.

### What the engine does

The engine gets a new request: a list of entries, each with its full path
and its new name, which is relative to the entry's folder and may hold `\`
and `..`. It owns these rules; the dialog checks none of them on the disk.

1. **Prepare** opens each entry without following links, reads its identity
   (D12) and its volume, and resolves its new path. A new path on another
   volume is a listed failure for that entry.
2. **It never replaces.** A new path that holds an entry outside the batch,
   or one that the batch does not move away, gets the first free numbered
   name, as the preview does. Undo's request fails such an entry instead.
3. **Order.** An entry is renamed only after the entry that holds its new
   name has moved away, so a shift such as `1→2`, `2→3`, `3→4` runs from the
   end [history:02.12.09]. A cycle, such as a swap, moves one entry to a
   temporary name in its folder first; the engine records the name before it
   uses it. Names are compared by identity, so a change of case is a rename
   of one entry. An entry inside a folder of the batch, as Search can select
   both, is renamed before that folder, so the path it was prepared with
   still leads to it.
4. **Missing folders** of a new path are created before the entry moves into
   them, as a copy creates a missing destination.
5. **No device admission** (decision 7). The progress counts entries.
6. **Cancel** takes effect between two renames. A cycle that has started
   finishes, so no entry keeps a temporary name.
7. **The result** lists each entry with the path it got, or its failure and
   Windows' error, in the order they happened.
8. **Saved before the first change,** with each rename and each temporary
   name. After a crash, Continue renames what is left: an entry at its new
   path is done, and an entry at a temporary name goes on to its new path.
   Dismiss gives each entry at a temporary name its old name back, without
   replacing anything, and leaves the rest (decision 8).

`DESIGN.md`'s D14 then lists the batch beside Delete and New folder, and D13
needs no change, because a batch deletes nothing.

### Undo

- **The window keeps the renamed entries of the last 20 batches,** each as
  its old path and the path it got, newest first, until Exit (decision 6).
  A batch that renamed nothing is not kept [history:16.08.16].
- **Undo** (`Alt+U`) is enabled while a batch is kept. It asks first, in a
  dialog titled Undo: "Undo the last Multi-Rename of 12 entries?", or for
  one entry, "Undo renaming "a.txt" to "a-1.txt"?", with "Entries that
  changed since then keep their names." below it, and Undo, the default,
  and Cancel.
- **Undo submits a batch** that renames each entry back, the last rename
  first. An entry that is no longer at the path it got, or whose old name is
  in use, fails and is listed; nothing is numbered or replaced. The folders
  that the batch created stay [history:22.07.11]. The batch leaves the list
  of kept batches, and the Undo itself is not kept, so each press goes one
  batch further back.
- A row in the open dialog whose entry the Undo renamed fails at Rename as
  any entry that no longer exists does.

### Edit names

Edit… opens a dialog titled Edit names, with "New names, one per line:" and
a multi-line edit box that holds the rows' new names in the list's order,
with their extensions and any folders. OK gives each row the name on its
line and closes. The names then pass the checks and the numbering, as
computed names do. The fields are disabled, and the status line says "The
names come from Edit names.". Choosing (Default) or a saved rename returns to
the fields. A count of lines other than the count of rows keeps the dialog
open with a message: "There are 98 names for 100 entries. Write one name on
each line." Empty lines at the end are not counted.

### Saved renames

- **Saved renames** (`F2`) is a drop-down list: (Default), then each saved
  rename by name, then Save… and Delete…. A saved rename holds every field of
  the dialog. Choosing one fills the fields; (Default) sets the defaults of
  "Opening".
- **Save…** asks for a name in a dialog titled Save. A name in use asks
  "Replace the saved rename "Photos by date"?", with Replace and Cancel.
  **Delete…** asks "Delete the saved rename "Photos by date"?", with Delete
  and Cancel, for the saved rename chosen last.
- **History.** The name, extension, Search for and Replace with boxes each
  keep their last 20 texts, newest first. `Shift+Delete` in an open list
  removes one.
- Both are saved in `ui.json` under `multi_rename`, and a missing or damaged
  value gives an empty list.

### Keys

| Key | Does |
| --- | --- |
| `Enter` | Rename, from a box or the list; in an open drop-down list, closes the list |
| `Esc` | Close, also from a box [live]; in an open drop-down list, closes the list |
| `F2` | open Saved renames |
| `Alt+U` | Undo |
| `Alt` and an underlined letter | the field or button with that access key |
| `Delete`, `Alt+Up`, `Alt+Down` | in the list: remove rows, move the focused row |
| `Shift+Delete` | in an open history list: remove the entry |

### Messages

Every text below goes into all four catalogues, `en`, `es`, `nl` and `tr`.

| Where | Text |
| --- | --- |
| Note | "No change", "Numbered", "No date taken", "Couldn't rename: {reason}" |
| Note, a name that cannot be used | "Not valid: {reason}", with the reason of "Names the person types", or "longer than 255 characters", "empty", "goes above the drive", "starts with a drive" |
| Status line | "9 of 12 entries get a new name.", "2 are numbered.", "1 can't be renamed.", "Updating names…", "Reading dates taken… 120 of 400", "Renaming… 4,210 of 10,000", "Renamed 98 entries.", "2 couldn't be renamed.", "The names come from Edit names." |
| Placeholder button tooltip | "Insert [N], the name without its extension", one for each button |
| Status line, a mask error | "[Q] isn't a placeholder.", "A [ has no ].", "The search isn't a valid regular expression: {reason}" |
| Undo | see "Undo" |
| Edit names | see "Edit names" |
| Saved renames | see "Saved renames" |

## Delivery

One implementer does the three stages in order and keeps its context; the
reviewer reads and reports. Each stage saves the screenshots of the states it
adds, for the owner's later review.

1. **The rules,** in `gozler-ui::core`: reading the masks, computing a row's
   new name, search and replace, case, the counter, the checks and the
   numbering order. Unit tests watch the failures a plausible change brings
   back: each range form, from the end and with padding; the counter forms
   and the order of rows; a folder's name with a dot; `\` and `..`; the order
   of the steps; numbering of two equal names and of a name in use, and not
   of a swap or a change of case.
2. **The engine request,** in `gozler-fileops`. Tests in a temporary folder
   watch: a swap, a shift of four names, a folder renamed with a file inside
   it in the same batch, a name created after prepare
   (numbered, never replaced), a missing folder, Undo's failure on a name in
   use, Cancel inside a cycle, and Continue and Dismiss from a saved file that
   holds a temporary name.
3. **The window:** the command, the dialog, Range, Edit names, Saved renames,
   Undo, Date taken and the selection after the dialog closes. Code review
   proves the behavior. One native run answers what review cannot: that the
   list view and the combo boxes send the changes the preview needs, and
   that a rename of 1,000 entries in a panel keeps the window responsive.

## After install

- Rename a folder of photos and videos by Date taken, and compare the names
  with File Explorer's Date taken and Media created columns.
- Look at the dialog in dark mode, in high contrast and at 150 % scaling.
