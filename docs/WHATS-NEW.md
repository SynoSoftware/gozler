# What's new

## Gozler 0.6.2

### Built especially for TinyTorrent's Library

This release gives TinyTorrent's Library the search options it needs to find media on your PC. Library can leave out folders you hide, include folders you choose inside them, and search with your Windows permissions.

#### Find what's wearing out your SSD

- **See what's writing.** Turn on **Writes** and **Written** under **View > Columns** in Search or Commander. They show how often a file was written and how many bytes are known to have been written in the last hour. A folder's write totals cover the files directly inside it. The window shows `≥` when it knows only part of the amount and leaves the cell blank when it knows no byte count.
- **Get a warning when an SSD writes much more than usual.** Gozler starts with alerts for 3,600 writes to one file per hour and for a whole drive writing ten times a typical user's weekly data. Turn on hourly data limits for files, folders or programs in **File > Settings > SSD wear** to investigate where writes come from. Alerts appear without taking focus, including while Gozler runs in the notification area.
- **Catch runaway folder copies.** The same window warns when a folder is repeatedly copied into itself. Search for `folder: repeats:4` to find existing folders whose paths end in four or more repeats of the same group of names.
- **See what the disk reports.** **Settings > SSD wear** lists the estimated lifetime used and total data written for disks that report those figures.

#### See current sizes, dates and writes

- **Keep the rows you're looking at up to date.** Visible Search rows refresh their values every three seconds. If you sort by **Writes** or **Written**, a new heavy writer can rise to the top by itself. Folder panels refresh visible folder sizes and file counts even while the folder is busy. The service also tracks growth while a program keeps a file open for writing.
- **Get folder totals without a scan.** The **Size**, **Files** and **Folders** columns show how much a folder holds, including its subfolders. The inspector and `gozler -get-folder-size` can answer from the index. On the command line, `--files` and `--folders` add the counts to results.
- **Choose each panel's columns.** Right-click a panel header or use **View > Columns** to choose what that panel shows, including dates, attributes and writes.

#### Select and select again in Commander

- **Select again.** `Num /` selects again what the panel's last copy, move or delete acted on. **Edit > Save selection** keeps the current selection for it, so a selection built for a later copy survives another command. When some entries are gone, a message says how many: "Selected 18 of 20. 2 entries aren't in this folder."
- **Try unfinished entries again.** When a copy, move or delete ends, the entries it did not finish stay selected, so `F5`, `F6` or `F8` tries them again.
- **Invert, and select by the focused entry.** `Num *` inverts the selection. `Alt+Num +` and `Alt+Num -` select or clear every entry with the focused entry's extension. **Edit > By focused entry** also selects by its name or its folder, and **Edit > Files or folders only** selects only files or only folders, for example to move every subfolder.
- **Write masks as in Total Commander.** In Select files… (`Num +`), spaces separate masks, double quotes keep a space inside one, the masks after `|` are left out, and a mask that starts with `<` is a regular expression. `*.jpg>*.cr2` selects the `.jpg` files and each `.cr2` file with the same name. The box keeps your last 20 masks, and a mask saved by an earlier version keeps its meaning.
- **Keep a selection in a file.** **Edit > Selection list** saves the selected names to a text file, and selects the names in a file or on the clipboard again. Total Commander reads these files too.

#### Search from the command line

- **Find the busiest writers.** Run `gozler --writes -sort written -n 20` to show the items with the most known bytes written. Use `writes:` and `written:` to filter, and `--status` to see when each drive began counting writes.
- **Leave out folders.** `--leave-out DIR` leaves a folder and everything in it out of a search, and `--leave-in DIR` brings a folder inside it back. For example, `gozler --leave-out C:\Windows --leave-in C:\Windows\Media ext:wav` leaves only the Media folder in scope under `C:\Windows`.
- **Search with your own permissions.** `--my-permissions` finds only the entries in folders you can open, even if you're allowed to see every file.
- **Search large folders faster.** A `depth:` limit lets the search skip folders below that depth. Searches also spend less time waiting when the index grows.
- **Read clearer help.** The command-line help and error messages use shorter, plainer wording. An invalid option reports an error even when `--help` or `--version` is also present.

#### Setup

- **Choose your settings in fewer steps.** Choose the install folder, including another drive, and see the required space. On the **Choose settings** page, **Standard install (recommended)** turns on weekly SSD write alerts, starting at sign-in, keeping running in the notification area and **Win+S** for Search. Standard shows only that summary. Select **Advanced install** to see and change each checkbox, or to let your account see every file name. Alerts come only while Gozler runs. Win+S no longer opens Windows Search, and Win+Shift+S stops working. Choose other shortcuts in **File > Settings > Keyboard**.
- **Review your settings during an update.** Updates keep the installed folder and always show the checkboxes with your saved choices when the installed version is 0.6.2 or later. If Setup can't read them, or you're updating an older version, it skips that page and keeps your settings. Choices you don't change stay as they are, including another Search shortcut unless you select Win+S. Uninstall removes your settings, so installing again starts fresh. Change visibility later in **Settings > Visibility**.
- **Read the release notes in Setup.** During an update, select **What's new** on the folder page, the **Choose settings** page or the last page to read these notes in a window you can resize or maximize. **Launch Gozler** is a separate choice on the last page.
- **Install without an agreement screen.** Gozler's own code now uses the Mozilla Public License 2.0. The download includes both that license and the MIT notice for code from goz.

#### Fixed

- `Num -` left a matching folder selected when Settings > General > Selection left folders out, so the next Delete removed that folder with the rest. It now clears matching folders too.
- The window sometimes stayed behind other windows, with only its taskbar button flashing, when a shortcut, the notification area icon or a second start opened it.
- The file list no longer flashes grey during a quick selection change, and brief jobs finish without flashing a progress bar.
- Saving a large file in Compare no longer holds up the window while it prepares the save.
- Gozler saves navigation state less often and leaves diagnostic frame and stall logs to debug builds.

The installer upgrades Gozler 0.6.1 in place. Keep `gozler.exe`, `gozlerd.exe` and `gozler-ui.exe` from the same release.

[Full changelog](https://github.com/SynoSoftware/gozler/compare/v0.6.1...v0.6.2)

## Gozler 0.6.1

### Multi-Rename, Compare and faster search

#### Rename many files at once

- **Multi-Rename.** Select entries in Search or Commander and press Ctrl+M. Build the new names with masks, a counter, search and replace, and case changes, or edit the names as text. The list shows every new name before anything changes, and Undo puts the old names back.
- **Work as in Total Commander.** The dialog is laid out like Total Commander's Multi-Rename Tool, with the same fields and access keys. **Next step** (F5) applies another rule to the names the step before made, and one Undo reverses every step. **Result list** shows what the last rename changed and what failed.

#### Compare two files

- **Compare by content.** Select two files and choose **File > Compare by content…** (Ctrl+Shift+C). The files show side by side, with the changed lines and words marked. Go from difference to difference, or turn on Edit mode to change either side and save it. Typing and Find stay quick in large files. The buttons and options are laid out as in Total Commander's compare window.
- **Use it from Git.** `gozler-ui --compare <a> <b>` opens only the compare window, so Git and other tools can use it as their diff program. To compare with another program instead, set **Comparison tool** in **File > Settings > Context menu**.

#### Work without waiting

- **Open folders at once.** In a panel sorted by Size, going into a folder or up from one is now instant. The folder sizes fill in afterwards, and the panel sorts again while the entry you're on stays on the same row of the screen. Before, the panel waited for the sizes, and showed "Reading…" when that took long.
- **Select large ranges.** Shift+End, Shift+Home and other Shift selections over a large list no longer hold the window.
- **See new file types without a pause.** The first time a file type shows, its icon fills in a moment later instead of holding the window.
- **Copy while another program holds the clipboard.** Copy from the search box, the inspector, the service report and the compare window no longer waits for that program.

#### Find exactly what you're looking for

These work in Search and on the command line.

- **Get results faster.** Searches for several names, such as `report|notes` or `*.pdf|*.jpg`, searches with a regular expression or **Match path**, and searches in a large folder are faster. A search you stop waiting for, such as during a fast scroll, stops.
- **Ignore accents.** `cafe` finds Café, `strasse` finds Straße and `kirmizi` finds kırmızı.
- **Find folders by what's in them.** `child-count:` counts the entries directly in a folder, and `empty:` finds empty folders.
- **Write dates the short way.** `dm:7d` finds items changed in the last 7 days. `dm:2025` finds a year and `dm:2025-06` a month.

#### Keep working through updates

- **Fewer version mismatches.** From this release on, most updates no longer stop a newer `gozler` or window from working with the service that's running. If you search for something that only a newer service knows, the service searches without it and tells you what it skipped, such as `child:readme`. The results might then be incomplete, so `gozler` exits with code 2.

#### Read files in the Viewer

- **View several files.** Press F3 with several files selected, and go through them with **Next file** and **Previous file**.
- **Find, select and zoom.** F7 finds text or hex bytes. Select text with the mouse, as in Notepad, and zoom with Ctrl+wheel. The **Encoding** menu reads the text in another code page. Total Commander's Lister keys also work, and Alt+F3 opens the file in the **External viewer** set in **File > Settings > Context menu**.

#### Open and delete what Windows protects

- **Open a protected folder.** When Windows refuses a folder for lack of permission, select **Continue** to open it as administrator, as File Explorer offers.
- **Delete what Windows denied.** A delete now goes on past each item that Windows refuses. Select **Retry** in the report to delete what's left as administrator.
- **See how much a delete removed.** The status bar shows the size, as in "Deleted 3 items (4.2 GB)".

#### See links

- **Link arrows.** Junctions and symbolic links show File Explorer's link arrow, so a link no longer looks like the folder or file it points to. When a link's target is gone, Gozler names the missing target.

#### Select with the mouse as in Total Commander

- **Clear the selection with a click.** In Commander, a click on a row that isn't selected clears the selection and moves to that row. Ctrl+click and Shift+click still add to it.

#### Read the list at a glance

- **Auto fit.** The columns now fill the list's width by themselves. Right-click a column header and turn off **Auto fit** to keep your own widths.
- **Two-digit dates.** Days, months and hours always show two digits, such as 01/26/2026 07:58 PM, so the dates line up in a column.

#### Use Gozler from scripts and AI agents

- **Identify each file.** `--file-id` adds each result's volume and file ID to `--json` output. A file keeps both when it's renamed or moved on its drive, so a script can tell a renamed file from a new one. See the [JSON guide](https://github.com/SynoSoftware/gozler/blob/main/docs/JSON.md#know-a-file-again-after-a-rename).
- **See where the service's memory goes.** `--status` now splits gozlerd's memory into the indexes, the kept results and the rest.
- **Read the help faster.** Each option shows one name, with its other names below it, so the help fits 80 columns.

#### Fixed

- **Safely Remove Hardware** failed for any indexed disk. Gozler now lets go of the disk.
- A folder's **Date modified** didn't change when a file in it was created, deleted or renamed.
- With **Adjust for daylight saving time automatically** off in Windows, every date was an hour off in summer.
- A drive whose change journal couldn't be read for a moment, such as a full or waking disk, stayed out of date until the service restarted.
- A search you stopped waiting for, or a sort of results you'd left, could make the service drop the results you were scrolling, so they had to be searched again.
- `foo| /ad` found foo or any folder, because a `/a` switch joined the end of the search text. The switches now apply to the whole search.
- The service kept the memory of its last search while idle, and the index kept memory it no longer used after it shrank. Both now go back to Windows.
- Windows sometimes reported that the service didn't respond while it stopped.
- The service log could grow without limit when programs failed to connect again and again.
- The service restarted about once a minute while another program held its pipe name. It now waits for the name.

The installer upgrades Gozler 0.5.9 in place. Keep `gozler.exe`, `gozlerd.exe` and `gozler-ui.exe` from the same release, because the service and its programs must match.

[Full changelog](https://github.com/SynoSoftware/gozler/compare/v0.5.9...v0.6.1)

## Gozler 0.5.9

### Search more precisely, plus updater fixes

#### Find exactly what you're looking for

These work in Search and on the command line.

- **Group terms.** `<a b>|<c d>` finds items that match both a and b, or both c and d. `!<a b>` leaves out a group.
- **Search by calendar period.** `dm:thisweek` and `dm:lastmonth` find items changed in that period, and `dm:<30days` finds items older than 30 days.
- **Find missing sizes and dates.** `size:unknown` and `dm:unknown` find items whose size or date isn't known yet.
- **Filter by contents, name or length.** `child:readme` finds folders that directly contain an item with readme in its name. `wfn:` matches the whole name, and `namelen:` and `fullpathlen:` compare lengths.

#### Use Gozler from scripts and AI agents

- **Show results in the window.** `gozler-ui --search <query>` opens Search with your query, and `gozler-ui --reveal <file>` shows the file in Commander.
- **Get readable dates in JSON.** Results include local `mtime` and `created` dates. `--json -n 0` returns only the total and whether it's complete.
- **Get help that fits one screen.** `-h` shows a short help, and `--help` shows the full reference.
- **Check the service version.** `--server-version` shows the version of the running service.

To learn about the JSON fields, limits and completeness checks, see the [JSON guide](https://github.com/SynoSoftware/gozler/blob/main/docs/JSON.md).

#### Switch drives when one isn't ready

If a panel's drive is removed or stops responding, select **Choose drive** next to **Retry** to pick another drive.

#### Fixed

- **Help > Update** didn't work until Gozler had checked for updates automatically, and it didn't tell you the result. It's now **Help > Check for updates…**, and it tells you about a new version, that Gozler is up to date, or that it couldn't check.
- Update and service repair messages, such as "Gozler is up to date.", stayed in the status bar. They now leave after 5 seconds, like other messages about finished jobs.
- The window no longer stops responding when a screen reader or another accessibility tool reads a long result list.
- **Show in Commander** now stays in Search when Windows can't open the folder, and tells you why.
- **Installed apps** in Windows Settings now shows Gozler's icon instead of a generic one.

The installer upgrades Gozler 0.5.8 in place. Keep `gozler.exe`, `gozlerd.exe` and `gozler-ui.exe` from the same release.

[Full changelog](https://github.com/SynoSoftware/gozler/compare/v0.5.8...v0.5.9)
