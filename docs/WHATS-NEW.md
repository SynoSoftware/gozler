# What's new

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
