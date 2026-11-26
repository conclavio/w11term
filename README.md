# w11term

A Windows-native terminal in Rust. Windows Reactor hosts the window and GPU terminal surface. SSH panes connect directly through russh. Local shell profiles run Windows console programs through ConPTY.

Terminal emulation is provided by **[wezterm-term](https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/term)**, the terminal core from [WezTerm](https://wezterm.org/), by Wez Furlong and contributors. Its VT parser, screen model, scrollback/reflow, graphics protocols, and semantic prompt support are the foundation of w11term's terminal. The vendored crate retains its [original MIT license](vendor/wezterm-term/LICENSE.md); our local changes are documented in [PATCHES.md](vendor/wezterm-term/PATCHES.md).

## Run

[Download the latest Windows x64 ZIP](https://github.com/conclavio/w11term/releases/latest/download/w11term-windows-x64.zip)

The ZIP contains the GUI, `w11term-cli.exe` and the Windows App Runtime. Extract the entire ZIP into one folder and launch `w11term.exe`. The latest release page includes the SHA-256 checksum. [Latest release](https://github.com/conclavio/w11term/releases/latest) · [Previous builds](https://github.com/conclavio/w11term/releases)

Building from source requires Windows and the Rust MSVC toolchain with Visual Studio C++ Build Tools. The first build downloads Rust dependencies, a pinned WezTerm source revision, and Microsoft's runtime packages. The build stages the WinUI runtime beside the executable.

Launch the build with `cargo run --release`, `.\target\release\w11term.exe`, or
`.\launch.ps1` from the project root. Copying only `w11term.exe` into another folder
does not produce a standalone installation: keep the runtime DLLs, PRI resource
files, and runtime subfolders beside it. Move the complete staged runtime folder
when deploying elsewhere.

Startup failures write `%LOCALAPPDATA%\w11term\startup.log` (or the directory in
`W11TERM_DATA_DIR`) and report the error to stderr. An Explorer launch without a
console shows a native error dialog. A missing Windows App Runtime is a packaging
error; renaming `profiles.json` does not repair it.

```powershell
cargo run
cargo run -- --profile root
cargo run -- --profile play-6789
cargo run -- --profile play-6792
cargo run -- --profile root --tab play-6789 --tab play-6792
cargo run -- --workspace "Admin console"
cargo run -- --profile "Windows PowerShell"
cargo run -- --list-profiles
cargo run -- ssh user@example.com --identity C:\path\to\id_ed25519
cargo run -- ssh user@example.com --port 2222 --host-key SHA256:verified-fingerprint
cargo run -- ssh play@conclav.io --port 6789 --auth none
```

Without a launch target, the app opens the **local terminal lab** until you choose a startup profile. The lab’s ANSI colors, Unicode text, and kitty image are actual VT traffic processed by the same engine used for SSH. Typing in the lab exercises local echo. Use `--demo` to open the lab directly.

`profiles.json` contains the three initial targets. `root` uses the local `hostinger` key; both `play` profiles use SSH `none` authentication, without contacting the agent or offering a private key. The key is excluded by `.gitignore`. Profiles load from the current working directory, then beside the executable or up to two parent directories (for development builds), or from an explicit `--profiles C:\path\to\profiles.json`. Relative identity and user certificate paths resolve beside that configuration file. CLI SSH options can override a profile.

## Tabs and profiles

Set **Tab color** in the SSH or local profile editor to give new panes from that profile a colored dot in the tab strip. Choose from six colors or **None**. In a split tab, the focused pane supplies the color. The tab's accessible name also includes the color, and pinned tabs keep their pushpin.

Choose **More → Tab color** or use the command palette to set a fixed color for the current tab, hide its marker, or return to **Use profile tab color**. An override follows the tab across pane focus changes and window moves. Saved workspaces, duplicate layouts, and reopened layouts retain it.

To give an SSH or local PowerShell profile its own colors, open **Settings → Profiles**, edit the profile, choose **Terminal color scheme**, and save. Choose **Follow Windows** to switch between **One Half Light** and **One Half Dark** with the Windows app appearance, including while a pane is open. You can also choose either scheme directly or keep **w11term (default)**. The schemes set the pane's foreground, background, cursor and first 16 ANSI colors. Two panes can use different schemes while sharing one SSH connection. Applications can query these colors and temporarily override them with OSC. Resetting an indexed color or a default color restores that part of the pane's current scheme. [Windows Terminal scheme definitions](https://github.com/microsoft/terminal/blob/main/src/cascadia/TerminalSettingsModel/defaults.json)

To change an open pane, choose **Panes → Color scheme** or search for **Set pane color scheme** in the command palette. The current choice is marked. Existing output and scrollback take on the new defaults, and the running process keeps its connection. Colors explicitly set by an application stay in effect until the application resets them. A reconnect in the same pane keeps your choice. Newly opened panes start with their profile's saved scheme.

In an SSH or local profile editor, set **Terminal text size** from 6 to 72 to choose the starting size for new panes. Leave the field blank to use the window's text size. **Reset pane text size** returns an open pane to its profile or window default. Display scaling and **Follow Windows text size** apply to both.

Terminal applications can request the pane's light or dark preference with `CSI ? 996 n`. w11term answers `CSI ? 997 ; 2 n` for One Half Light and `CSI ? 997 ; 1 n` for the dark schemes. An application that enables `CSI ? 2031 h` also receives a report when the effective palette changes, including when a pane follows Windows from light to dark. It can stop notifications with `CSI ? 2031 l`. The preference follows the user's selection even if an editor sets its own RGB background or requests reverse screen. [Contour theme reporting](https://contour-terminal.org/vt-extensions/color-palette-update-notifications/)

In **Settings → Profiles**, use **Filter profiles** to find a profile by name or SSH address, then choose **Edit…**. Use the up and down arrow buttons to arrange SSH and local shell profiles together. Each move takes one step in the full catalog order, even while the list is filtered. The **+** menu, split submenus, command palette, and remote session browser use that order. Changes are saved immediately in `profiles.json` and appear in other open windows when you switch to them.

In the same section, set **Startup profile** to a saved SSH or local shell profile. The choice is saved in the current profile catalog and applies on the next launch without a target. Explicit profile, workspace and replay launches use their requested target. Choose **Terminal lab** to restore the default.

Use the pin button beside an SSH or local profile to keep that profile in the **+** menu. Pinned profiles follow the catalog order. Open **Other profiles** or use **Find Profile…** for the rest. The pins are saved in the current profile catalog and follow profile renames.

In an SSH or local profile editor, **Scrollback lines** sets how many rows the terminal can keep above the primary screen. The default is 10,000. Set it to zero to keep only the current screen, or enter up to 32,767 for longer build logs. A wrapped line can occupy several rows. Find and **Save scrollback as text…** use the retained rows. Open a new pane from the edited profile to use its limit. Reconnecting or restarting a shell in an existing pane keeps that pane’s limit. The JSON setting is `"scrollback_lines": 2000`.

When creating or editing an SSH profile, **SSH terminal type (TERM)** selects the terminfo name sent with each new PTY. Existing profiles use `xterm-256color`. Choose `xterm-sixel` for a host running Venti with Sixel graphics, or choose **Custom…** to enter a terminfo name supported by the host. This setting is stored as `"terminal_type": "xterm-sixel"` for the Sixel preset. Direct shells and PTY-based zmx and tmux attachments request `COLORTERM=truecolor`. A process already running inside a durable session keeps its existing environment.

Open **More → Command palette…** or press **Ctrl+Shift+K** to search actions for the current window. Search by words in any order, a profile name, or a current shortcut. Use Up and Down to highlight a result, then press Enter or click it. Disabled commands stay visible when their pane or connection requirements are unmet. Escape returns to the current pane. The palette also works from Settings and other utility tabs.

Use **Ctrl+Shift+P** or **More → Find pane…** to search every terminal pane in the window, including panes in other tabs and hidden splits. Type part of the terminal title, profile name or SSH `user@host`. Use the arrow keys or hover over a match to preview its live viewport, including terminal graphics. The preview preserves each pane’s grid size and focus. Screen readers can read the last two nonblank preview lines. Press Enter or click a result to focus it. Escape returns to the pane you were using.

Choose **More → Find in all panes…** to search terminal text across this window’s tabs and splits. Type a word or phrase to see each matching pane, its match count and an excerpt from the first match. The search includes retained scrollback and words split across soft wraps. In an alternate-screen application it searches the current screen. Use Up and Down to choose a pane. Enter opens that pane’s Find bar at the displayed match, where you can step through the other occurrences. Escape returns to your original pane and scroll position. You can assign **Find in all panes** a shortcut under **Settings → Keyboard**.

Results show the text present when you searched. Use **Refresh** to include later output. Turn on **Match case** for case-sensitive matching or **Regex** for a regular expression. Both choices carry over to the selected pane’s Find bar. Queries accept up to 4,096 UTF-8 bytes, and results show `10,000+` when a pane contains additional matches. Regex search skips logical lines larger than 64 KiB and flags panes with incomplete results.

The native WinUI tab strip sits in the title bar. Click **+** to open a profile in a new tab. The title-bar icons toggle SFTP files in an SSH pane, open **Pane layout**, open scrollback Find, and open the command palette or Attention inbox. When tabs crowd the title bar, **Terminal tools** groups SFTP, pane layout, Find, and the command palette beside the inbox. **More** (the ellipsis) includes those commands alongside Workspaces, SSH host groups, SFTP transfers, Attention inbox, and Settings.

Choose **+ → Find profile…** to search launchable profiles by name or SSH address, including the username and port. Search words can appear in any order. Press Enter to open the selected profile in a new tab or Escape to return without opening anything. Local shell profiles appear alongside SSH profiles. **Find profile…** is also available in the command palette.

To reorder the selected tab, drag it or use **Move tab left** or **Move tab right** in **More** or the command palette. Both actions can be assigned shortcuts under **Settings → Keyboard**. Moving a tab keeps the same pane focused. Numbered tab shortcuts follow the new order. Closing a tab closes its panes, and closing the last tab closes the window. Each visible pane renders independently. Background sessions continue receiving output.

Choose **More → Find tab…** or search for **Find tab** in the command palette. Search this window by tab title or position, including SSH groups and utility tabs beyond the visible strip. Use Up and Down to choose a match, Enter to switch and Escape to keep your current tab. Assign a shortcut for **Find tab** under **Settings → Keyboard**.

Builds, deployments, and other programs that emit **OSC 9;4** can report progress in the tab header and on the window’s Windows taskbar button. The header shows a percentage, **Paused**, **Error**, or **In progress**. When several panes report at once, an error takes priority, followed by paused, normal, and indeterminate progress. Within the same state the least complete operation wins. Clearing a pane’s progress reveals the next one. [Windows Terminal progress sequences](https://learn.microsoft.com/en-us/windows/terminal/tutorials/progress-bar-sequences)

Programs that emit **OSC 7501** can also show **Done**, **Needs approval**, **Needs an answer**, or **Needs sign-in** on their tab. Hover over the tab to read the reported title and message and identify the pane that sent them. Each live pane keeps up to 64 reports. A new shell prompt or process exit clears active work while leaving completed and failed results visible. A terminal reset or a fresh connection clears all reports. [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)

Use **More → Panes** to split right or below. Splitting the current host opens a new shell channel on its authenticated SSH connection when the endpoint, user, authentication method and host-key settings match. A server that declines the extra channel gets a separate login attempt. Choosing a specific profile always opens an independent login. Each pane owns its PTY and SFTP handles. Either pane can close while the other keeps running.

In **Pane layout**, choose **Find profile to split right…** or **Find profile to split below…** to search by profile name or SSH address. The same actions are in the command palette. Enter opens the selected profile beside the focused pane. Escape returns to that pane.

Choose **More → Panes → Broadcast typing to panes** or search for it in the command palette to type into the connected, writable direct SSH and local shells visible in the current tab. The bar below the panes shows the recipient count and lets you stop. Switching tabs or changing the eligible panes ends the broadcast. Typing and committed IME text are mirrored. Use the per-pane paste commands for clipboard text.

Choose **More → SSH host group** or search for it in the command palette to submit one shell command to selected SSH panes across this window’s tabs. Each target shows its profile or pane name, SSH address, and prompt state. A fresh OSC 133 or OSC 633 prompt with start and input markers reports **Ready**. The bundled Bash and Zsh integrations emit these markers. Typing into a pane uses its prompt until the shell reports a fresh one. Select one or more targets, enter the line, and choose **Send to _n_ hosts**. Each result reports whether the command reached that SSH channel. Use **Focus pane** to read its output or respond to an authentication prompt.

For an appliance SSH shell, set **SSH group prompt regex** in its profile and open a new pane from that profile. For example, `rack-[12]>>` matches the visible prompt on either rack. The regex matches the full line up to the cursor with trailing spaces removed. A fresh match reports **Ready · prompt match**, and the next match after a submission reports **Prompt returned · exit status unavailable**. Inspect the pane for command output. OSC 133 and OSC 633 take precedence when present. Choose a pattern specific to the prompt because matching output can also mark the pane ready.

Choose **Prompt setup…** on a connected host’s roster row to copy the Bash, Zsh, or Fish integration and open the pane to paste and run it. For an appliance, **Apply to pane** changes its prompt regex immediately and keeps the setting for reconnects in that pane. A blank regex disables matching. The pane waits for a freshly printed matching prompt after a change. Use the profile field to set the default for new panes.

Use **Host filter** to find SSH panes by profile, pane name, or address. **Select shown** and **Deselect shown** change the visible rows. The filter preserves hidden selections and scopes **Send** and **Interrupt selected** to the shown hosts. Prior submission results remain visible.

Turn on **Use wildcards** for a list such as `web-[01][0-9], db-??, !*-canary`. Patterns match whole hostnames, SSH addresses with or without ports, pane names, or profile names. Separate patterns with commas. Use `*` for any number of characters, `?` for one character, brackets for sets or ranges, and `!` before a pattern to exclude matching hosts. Use `*, !*-canary` to include every host except canaries. A backslash escapes a special character. Matching ignores case. An incomplete pattern keeps the target list empty until you fix it.

The command bar above the host filter shows how many selected, visible hosts are ready. The Send button shows the recipient count, and its adjacent **Send options** menu offers recording and alerts. From **Roster actions**, choose **Select only ready shown** to check visible hosts with a fresh shell marker or a configured prompt match and uncheck the other visible hosts. The selection stays in place as prompts change. Review the recipients before choosing **Send**.

Expand **Dispatch settings** to configure readiness, parallelism, pacing, expiry, and canary behavior. The collapsed header shows the active non-default settings. The adjacent **Preview send order** button shows how the current draft would run.

Turn on **Require all recipients ready** to wait for every visible, checked host to have a verified prompt before **Send** or **Send and record** becomes available. Choose when to send once they are ready. The app checks every prompt again before dispatch, and each pane reports its own delivery outcome.

Set **Parallel SSH commands** to run a submission in batches. The default is **Unlimited**. With a limit, the results show queued hosts and the next host gets a slot when an earlier command settles. A queued command still needs the original verified prompt. **Cancel queued commands** withholds the remaining sends while active commands continue.

With a parallel limit, send interval, or expiry, turn on **Stop queue on failure** to withhold the remaining commands as soon as a host settles without a confirmed zero exit status. Active commands continue. **Save roster defaults** remembers this choice for a named group.

Use **Preview send order** to see how the checked, visible hosts will be dispatched. Queues with a parallel limit, pacing, or expiry follow the current roster sort, with a chosen canary promoted to the first send. Immediate unlimited sends use tab order. The preview shows unavailable hosts and the concurrency that applies after canary approval. For these queues, select **Earlier** or **Later** to move a ready host in the next run. **Use roster order** clears the draft order. Sending uses that order for the submitted queue and starts the next draft in roster order.

Set **Time between sends** to leave a minimum gap between dispatches within each run. The first host can start immediately. Later hosts still need a free parallel slot and any required canary approval. **No delay** is the default.

Set **Expire unsent commands** to cap how long the current run can wait to dispatch. The timer starts on submission and keeps running during pauses and canary approval. When it expires, queued hosts are withheld and active commands can finish. **Never** is the default.

Use **Pause queue** to hold unsent commands while you inspect results. Active commands continue, and stop-on-failure still applies. **Resume queue** rechecks the original prompts before sending again. Use **Skip host** on a queued result to remove just that recipient. The remaining queue keeps its limits and canary approval requirement. Skips are counted separately from failures.

Choose a **Canary host** from the checked recipients to run on that shell first. It needs a fresh OSC 133 prompt and at least one other ready recipient. After a confirmed exit 0, inspect the result and select **Approve remaining hosts** to continue at the chosen concurrency. If the canary fails or its exit status is unavailable, the remaining commands are withheld. Check **Notify when canary finishes** before sending to get an Attention Inbox entry that opens the exact run when the canary succeeds or needs attention.

Use the host-state dropdown to show ready shells, disconnected panes, or another current state. The state filter combines with the name or wildcard filter and keeps checks on hidden hosts. Sending and other actions on checked hosts use the visible selection.

The live state count summarizes every host matching the name or wildcard filter. When a state filter hides hosts, the summary shows how many are visible out of the matching total.

The roster shows **Connecting** while the first shell opens and **Reconnecting** during a retry. Automatic retries show their attempt number. These states are available in the host-state filter and the copied roster.

Filter by **Connection failed** to find panes whose SSH attempt ended with an error. Each failed row shows a short diagnostic and offers **Reconnect** when the pane can restart.

Choose **Stop selected SSH attempts** to stop connecting, signing in, or retrying for the checked hosts shown by the roster. The command palette also offers **Stop pending SSH connections**. A stopped pane keeps its scrollback and can be reconnected explicitly.

Choose **Notify when logins finish** to watch the currently checked, shown SSH attempts. When all of those attempts settle, the Attention inbox gets one entry with the connected, failed, stopped, and closed counts. Select it to return to the roster. **Cancel login notification** disarms the watch.

Choose **View login results** beside a login alert for each watched host's address, original outcome, and any failure message. A later reconnect leaves the original report intact. Open a pane from the report while that pane is still available.

Use **Copy login report** or **Save login report…** to share the captured results as plain text. Saved reports use UTF-8.

Use **Review failed logins** to see which failures from the report can still be retried. Confirm to reconnect the listed panes through the roster's login queue. If a pane has already reconnected or the eligible hosts change, review the remaining hosts again.

Choose **Close ended SSH panes…** to review and close checked, visible panes whose connection has ended. Closing discards their scrollback. Panes in pinned tabs stay open, and a pane that reconnects before confirmation is kept. The command palette offers **Review close for ended SSH panes**.

Choose **Open next SSH prompt** to open the first pending host-key or sign-in prompt in the displayed roster. The button shows how many visible hosts are waiting and follows the roster filters and sort order. Select the group tab to return to the roster.

Choose **Show last command output** under **Roster display** to see the exit status and a short excerpt from each visible host’s latest completed shell capture. **Open history** opens that capture in the host’s command history. The preview uses the current SSH connection and hides during authentication and alternate-screen applications.

Choose **Show current terminal line** under **Roster display** to see the bottommost text line in each shown host’s live screen, including shells without integration. The preview masks concealed cells and hides during authentication and alternate-screen applications. When a preview is on, its menu entry changes to **Hide**.

Choose **Roster actions → Copy shown roster** to copy a plain-text status report. It includes every host shown by the current filters, its check state, SSH address, pane ID, and connection or prompt status.

Use **Sort SSH roster** to display hosts in tab order, alphabetically by label, checked first, or with hosts needing attention first. The copied roster follows the display order. Command dispatch and `POLYSH_RANK` use tab order.

In a named SSH group tab, choose **Save roster defaults** from the roster scope menu to remember its sort order, output previews, readiness requirement, parallel command limit, send interval, and queue expiry. These defaults apply when you open the group again and travel with exported group definitions.

When an SSH shell has exited or its connection attempts have ended, use **Reconnect** on its roster row or **Reconnect selected** for checked hosts shown by the filter. Each attempt starts a fresh SSH login in the same pane with its scrollback and selection intact. Previous submissions stay available, and the replacement shell needs a fresh prompt before it can receive another group command.

Choose **Disconnect selected…** to review the connected, checked shells shown by the roster filters. Confirm to end those shells and keep their panes, scrollback, and selection. A changed connection or selection requires a fresh review. Use **Reconnect** when you want to start a new shell.

From an SSH group tab, open the command palette to copy the shown SSH roster, open the next sign-in or host-key prompt, reconnect selected hosts, or review a disconnect. These commands follow the current roster filters and apply the same recipient and connection checks as the roster buttons.

Choose **Find SSH host in group…** from the command palette to search the shown hosts by pane name, title, profile, or SSH address. The picker follows the group’s live filters and shows a preview. Press Enter to focus the host or Esc to return to the roster.

After delivery, the group follows each host separately. Matching shell markers show when the command runs and give its captured output and exit status. **View full output** opens that command’s history entry while the pane still retains it. The group keeps the last eight submissions, each with an output preview per host. Use **Previous submission** and **Next submission** to revisit them while a slow command continues in the background.

Choose **Find run…** in the results or **Find group run…** in the command palette to jump to a retained submission. Search its command text or enter a run number such as `#3`. Runs appear newest first, with repeated commands listed separately. Enter opens the selected result and Escape returns to the current submission. The command draft and checked hosts stay in place.

Use **Find group runs across tabs…** in the command palette from any tab to see retained runs in the current window. Search by group tab title, command or exact run number. Active runs appear first, with their host progress updated while the picker is open. Enter opens the result in its original group tab. Escape leaves your current tab in place.

While a verified command runs, its result shows elapsed time. On completion it shows **Shell time**, the interval between the shell markers as received by w11term. Network and output buffering can affect that interval. Timing is available when the shell reports the start of command output and the command can be matched to the submission.

Choose **Live output…** on a submission to follow recent output from all its hosts, newest first. Each line identifies its source and lets you focus that pane. **Pause output** freezes the available history while the shells continue, and **Resume output** catches up to the newest retained lines. The combined feed keeps 128 lines. Selecting a host can recover lines that a burst from another host pushed out of the combined feed. Each host can retain up to 128 lines, with a shared budget of 512 additional lines per run. The view marks skipped or shortened output. It shows completed text lines from verified shell markers and the final line when the command ends. For prompt-only appliances or a full transcript, open the source pane or its command history.

**Find in live output** starts with case-insensitive literal search. Turn on **Regular expression** to match a pattern against each retained line or **Match case** to distinguish uppercase from lowercase. Use the host dropdown to narrow the results to one SSH pane. The match count uses the retained lines in the selected feed. The view keeps the filters when you pause, resume, or visit an originating pane. An invalid pattern shows a diagnostic and disables export until corrected. **Clear output filters** restores the combined feed and the default search options.

Turn on **Show surrounding lines** to include up to two retained lines before and after each match from the same host. The view labels those lines as context and shows the match count separately. Copy and save include the context in the order shown.

Choose **Oldest first** to read the output in the order w11term received it. **Show elapsed time** annotates each line with the local capture time since w11term started tracking the run. Lines from the same batch can share a time. Both options carry over when you switch runs, and copy and save use the displayed order and timestamps.

Scripts can use `w11term-cli wait-ssh-group-output` to wait for a matching line and get its host and capture time. Use `get-ssh-group-output` to read available lines in batches or `follow-ssh-group-output` to stream retained and live lines as JSON Lines. Pass a returned cursor or line ID as `--after-line` to continue in the same run. [Control protocol](CONTROL.md#ssh-group-rosters-and-runs)

While a run is active, enter a search and choose **Notify on next match**. The alert uses the search options and host filter selected when you arm it. It watches new output even when the view is paused or another tab is active. Live output and the run results both show the armed query, match options, and host scope. Choose **Cancel output alert** from either view before it fires. The first match adds the host and a short excerpt to the Attention inbox. Open the notice to return to that run's results.

Choose **View matching output** in the Attention inbox for the matched line and up to two nearby lines on each side from the same host. If that history has expired, the view shows the saved alert excerpt. From there, return to the inbox or open the original run results while they are retained.

From the command palette, choose **Find armed SSH output alerts…** to review alerts across group tabs in the current window. Search by saved query, host scope, tab title, command, or run number such as `#4`. Enter opens the run, preserving its Live output view if it is already open for that run. Alerts leave the picker when they fire, are cancelled, or the run settles.

Choose **Send options → Send with alert…** to catch a match from the start of a command. Review the command and recipients, set the search rule, then choose **Send with alert**. w11term rechecks the command and prompts and arms the alert before dispatch, including for queued and canary runs.

Turn on **Record full output** in the same review to choose a new text file before sending. The alert uses your search rule, and the recording captures all marked output and final outcomes until the run settles. Cancelling the file picker leaves the command unsent. Scripts can use `submit-ssh-group-command --record-output PATH` and wait for the writer with `wait-ssh-group-run`.

Open **Saved alert rules…** in Live output or the send-with-alert review to save a named copy of the current query and matching options. Select its name to fill in the search for a new run. The preset uses the current view's host scope, and you choose when to arm or send. Saved rules are available across windows and restarts.

In **Send with alert…**, turn on **Alert if expected output is missing** to look for a confirmation line across the selected hosts. A verified match clears the alert quietly. If the run settles without a match, the Attention inbox reports either **Expected output missing** when capture completed for all recipients or **Could not verify expected output** when delivery or capture was incomplete.

Select **Require a match from every host** when each recipient needs to produce its own confirmation. Scripts can arm the same check with `submit-ssh-group-command --expect-output`. The inbox summarizes missing and unverified hosts at settlement. Open the notice to see which hosts fall into each group. Copied results, saved text and JSON reports, and the CLI run detail include each recipient's resolved check, including successful matches.

The run selector switches among retained submissions and keeps the text search, its options, and the selected host. If the selected host is absent from a run, the view stays scoped to that host until you select another. The destination opens in live mode. **Back to results** opens the selected run, and any original recording continues on its own run.

Choose **Copy shown output** or **Save shown output…** to share a plain-text excerpt of the active view, with source labels and newest lines first. The save dialog takes a snapshot when it opens. Paused views export the frozen lines that match the current filters.

In **Live output…**, choose **Record output…** to write that submission's host-labeled output to a new UTF-8 text file in arrival order. The recording starts with the retained lines and continues while the view is paused or another tab is focused. Missing or shortened lines are marked in the file. Choose **Stop recording** to finish writing and see the saved path.

At the end of a recording, **Host outcomes** lists each checked host's delivery status and the shell result known at the time. A host still running when you stop is shown as running. For confirmed completions the file includes the exit code when the shell reported one.

After saving, choose **Show in File Explorer** to select the file or **Copy recording path** to copy its full path. If you save several recordings of the same submission, use the **Saved recordings** selector to revisit an earlier file from the results or the live output view.

Choose **Send options → Send and record…** beside the group command to attach a recorder before dispatch. Pick a new text file and w11term will recheck the command, checked hosts, and their prompts before sending. Cancelling the picker or choosing an existing file leaves the command unsent. If the checked scope changes while the file is being prepared, it records that no command was sent. The recorder saves automatically once every host finishes, rejects the send, or loses its connection, even while another tab is active. To finish sooner, choose **Stop recording** in the live output view.

The submission progress bar and counts cover every recipient in the viewed run. Use **Filter submission results** to see waiting, running, successful, or attention-needed results. **Needs attention** includes nonzero exits, commands withheld or unconfirmed, and outcomes without a verified exit status.

Use **Search result recipients** to find recipients by profile, pane, or SSH address. Search ignores case and combines with the outcome filter. It follows you through earlier and later submissions in the same tab and clears on a new run or when you open a run from Attention. The table shows how many recipients match. **Interrupt selected** and **Prepare retry** use the matching results and the live roster scope.

Choose **Notify when run finishes** from **Run actions** on a pending submission to get one entry in the Attention inbox once all its hosts settle. The alert includes success and attention counts, and selecting it takes you back to the retained submission. If another application has focus, the owning taskbar button flashes. The results show when the notification is armed. Choose **Cancel run notification** from the same menu to disarm it while the run is pending.

Use **Sort submission results** to show the longest or shortest shell times first. The sort compares live elapsed time with completed durations. Ties keep their submission order and untimed results appear last. The sort stays with the group tab as you move between submissions. Exports keep the original submission order.

Choose **Load command** from **Run actions** on a previous submission to put its exact text back in the group editor. Edit it if needed, check the current host selection, then choose **Send** to run it. **Restore draft** recovers the text you were editing before loading from history.

Press **Ctrl+R** in an SSH host group, choose **Command library → Recent commands…**, or find **Recent group commands…** in the command palette to search the last 50 distinct commands submitted from this tab, newest first. Search matches the full command without regard to case. Press Enter to load the selected command as a draft or Esc to return to your current draft. The host filter and checked hosts stay as they are. Recent commands are kept in memory for the life of the group tab.

Choose **Prepare retry** to load the exact command and select original recipients with a confirmed nonzero exit or withheld delivery. The roster and result filters scope the selection to the hosts you can see. Review the command and checked hosts, then choose **Send** when the targets have fresh prompts. **Restore draft** also restores the previous host selection and roster filter. Unconfirmed delivery and exit outcomes remain available in **Needs attention** for inspection.

Choose **Command library → Saved commands…** in the group editor to browse the same command library managed in **Settings → Saved commands**. You can also search by name in the command palette while the group is active. Choosing a command fills the draft for review. **Restore draft** is available here too.

Choose **Command library → Save as command…** to add the draft to the shared library. Enter a name and choose **Save to commands**. The form shows the exact text it will save, and sending remains a separate action.

Choose **Copy results** from **Run actions** to copy the viewed submission as plain text, including every recipient’s delivery, execution and interrupt status. The report uses full output retained in each pane and labels any preview that stands in for expired output. Large reports shorten output to fit the clipboard while retaining every host’s status.

Choose **Save results…** in the same menu to write a UTF-8 report of the viewed submission. It includes every recipient and all retained output from completed commands, even when the roster or result filters hide a host. A host still running appears with its status at the time you opened the Save dialog.

When the result filters show a nonempty subset, use **Copy shown results**, **Save shown results…**, or **Save shown results as JSON…**. These reports keep the displayed recipient order, the original recipient count, and the run's dispatch settings and queue activity. Text reports identify the subset in their heading. JSON reports include `total_recipient_count`, with the shown hosts in `recipients`.

Scripts can find windows and live groups with `w11term-cli list-ssh-groups`, discover a window's saved group names with `list-saved-ssh-groups`, and launch a saved group with `open-ssh-group`. Use `get-ssh-group-roster` to check live selection and `wait-ssh-group-ready` to wait for verified prompts. Use `submit-ssh-group-command` with explicit pane IDs to send a shell line and get a run ID. Use `get-ssh-group-run` to inspect the run, `wait-ssh-group-run` to wait for final outcomes, or `save-ssh-group-report` to export a JSON or text report with captured output. [SSH group control commands](CONTROL.md#ssh-group-rosters-and-runs)

Command results, reports, and output recordings show the dispatch settings captured for that run. These settings stay with the run when you change the next draft. **Show queue activity** lists pauses, resumes, canary decisions, and stops with elapsed time since submission. Reports and completed recordings include the most recent 32 queue events and count any earlier events omitted.

Text and JSON reports include the known per-host times when you open the Save dialog. In JSON, `execution.elapsed_ms` is the running time for a verified running command and `execution.duration_ms` is the final time for a completed command.

**Save results as JSON…** exports the same snapshot for scripts. The document has `schema_version: 1`, a `submission_id` scoped to its group tab, `kind` (`command` or `environment`), `label`, `command`, and an ordered `recipients` array. Each recipient has `pane_id`, `name`, `profile`, `address`, and `command`. A command submission puts the shared command at the top level. An environment export puts the commands on the recipients.

- Command submissions include `dispatch_policy` with the numeric `parallel_limit` (null means unlimited), `interval_ms`, effective `stop_on_failure`, and a `canary` with its `pane_id` and `label` when selected. The canary field is null for a run without one. With an expiry configured, `queue_expiry_ms` gives the time allowed for unsent commands.
- When queue events exist, `queue_activity` contains an `omitted` count and an ordered `events` array with `elapsed_ms` and `kind` for each event.
- `delivery.state` and `interrupt.state` use `pending`, `sent`, `rejected`, or `unconfirmed`. A queued command uses `queued` until dispatch and `skipped` if removed before sending. A host skip in `queue_activity` carries its `pane_id`. A rejection includes `reason`. The `interrupt` field is `null` until an interrupt is requested.
- `execution.state` uses `awaiting_markers`, `awaiting_prompt`, `running`, `prompt_returned`, `completed`, or `unavailable`. A completed execution includes `connection_generation`, `command_id`, and `exit_code`. The exit code is `null` when the shell supplied none.
- `output.state` uses `complete` or `preview` with `text`, or `unavailable` with `reason`. A run without a completed capture has `output: null`. A complete, empty capture uses an empty `text` string.

Choose **Compare results…** from **Run actions** to group the viewed submission by exact captured output and verified exit code. Smaller groups appear first. Each distinct result shows its hosts, an output preview, and where its output first differs from the largest group. If sizes tie, the earliest group in the submission is the reference. Running commands, unverified exits, and results with only a truncated preview are listed separately. Use **Refresh comparison** to include results that arrived after opening the view.

Choose **View differences** on a result to see added and removed lines against the reference group with original line numbers and both exit codes. Large differences show a bounded excerpt. Use **View full output** from the comparison to inspect a longer result in its command history.

Use **Compare with earlier run…** on a completed host result to compare the same pane and SSH address across two retained runs. Pick the earlier run and choose **Compare output**. Both sides need complete output and a reported exit code. The view shows the two run IDs, command labels, and exit codes. Choose **Save full run differences…** to export the complete line changes.

Choose **Compare runs…** from **Run actions** to see those changes across the entire group. The overview separates changes in output and exit status, hosts in one run only, and unavailable comparisons. Search by host label or SSH address and filter by classification. Choose **All hosts** to include identical results. Choose **Compare runs** again to refresh the captures. Open **View differences** on a comparable host for the detailed diff and return to the same filters when finished.

Use **Copy comparison** or **Save comparison as JSON…** to share the run labels, counts and every host classification, including hosts hidden by filters. The JSON report uses `schema_version` 1 and includes `earlier` and `current` runs, `counts` and `hosts`. Each host entry identifies its pane, profile label, SSH address and classification. Exit codes are available when both runs provided comparable captures. An unavailable comparison includes a detail message.

Choose **Use as reference** on a result group when a particular host is the expected baseline. Refresh keeps that choice while the host still belongs to a comparable group. **Use largest group** restores automatic selection.

In the diff view, choose **Save full differences…** to write a UTF-8 report of the current comparison. It includes the command, both hosts and exit codes, and changed lines at full length. If the changed sections are too large to align, the report includes both full unmatched sections in source order.

Choose **Prepare follow-up** on a result group to return to the roster with the original command in the editor and that group’s visible, matching panes checked. The button shows how many hosts are in scope when the roster excludes some members. Edit the draft if needed, then use **Send to _n_ hosts** when the intended shells are ready. **Restore draft** brings back the previous draft, roster filter, and checked hosts.

While viewing a submission, choose **Interrupt selected** to send Ctrl+C to its selected hosts that still report the matching command as running. The result shows Ctrl+C delivery and the command’s eventual exit status separately. w11term uses each pane’s current keyboard mode.

In **SSH host group**, choose **New group**, name it, and check the saved SSH profiles to include. After creating your first group, find **New group** in **Saved group actions**. **Open group** uses the current group tab. **Saved group actions → Open in new tab** and the palette command **Open SSH group** create a named group tab. Each launch starts a fresh SSH tab for every member. Unavailable profiles are reported before any member tabs open. Uncheck a pane to pause delivery to it. **Saved group actions → Edit group** changes the saved name or members, and **Delete group** removes the saved group. Group definitions live with their profiles in `profiles.json`. Renaming a profile updates its group memberships, and deleting a profile removes it from those groups.

Choose **Duplicate group…** to save the selected group under a new name. The copy keeps its profile order, recipient sets, and roster defaults. It appears in the saved groups list and can be opened separately.

Choose **Saved group actions → Export group definition…** to save a portable JSON file with the saved group name, ordered profile names, recipient sets, and roster defaults. To use the file elsewhere, open **Saved group actions → Import group definition…** and review the matching local SSH profiles and their addresses. If a profile is missing, create it locally and choose **Refresh local profiles**. Saving adds the group to the catalog for later launch.

Use **Choose local profile…** on an imported member to map it to a differently named local profile. Search by name or SSH address. Each imported member needs a distinct local profile, and the saved recipient sets follow the mapping in their original order.

Choose **Saved group actions → Open group in new window** to keep the roster and its fresh SSH panes together in a separate window. The new window uses the group’s saved roster defaults.

To open just a subset or a recipient set in its own window, use **Saved group actions → Review group launch…** and choose new panes for the included hosts. **Open reviewed group in new window** rechecks the review before connecting. A partial roster keeps the saved group’s full membership available for future launches.

Set **Parallel SSH logins** in the launch review to limit how many fresh connections authenticate at once. The default is **Unlimited**. A queued pane shows **Waiting to connect** and can be stopped from the roster. A slot becomes available after login and opening an SSH session channel, so active shells do not count against the limit.

The server has 15 seconds to open a session channel and another 15 seconds to acknowledge terminal setup. If either stage times out, the roster reports the SSH error or retry. These limits start after authentication.

Choose **Save as default** beside the login limit to use it in future reviews and when opening the saved group directly in a tab or window.

In a saved group's roster, **Reconnect selected** uses the same saved limit. Overlapping batches in the same roster share the queue. Each queued retry keeps its original pane and can be stopped from the roster.

**Parallel SSH logins** is also available when reviewing an imported host list or adding saved profiles to a roster. A named roster starts these reviews at its saved limit. Changing the value in the review applies to this launch.

When creating or editing a group, filter by profile name or SSH address. **Check matches** and **Uncheck matches** apply to every matching profile across pages. **Move up** and **Move down** change a checked profile’s position in the full group. Fresh tabs open in the saved order.

Group tabs keep their own drafts, filters, host selections, and results. A named tab shows the panes opened for that group and panes added with **Saved group actions → Import host list…**. The scope button shows **Group panes** or **All window panes**. Open it and choose **Show all window SSH panes** to include other connections. Choosing **Show group panes only** clears checks on those other panes. **New SSH group tab** opens an independent window-wide roster with an empty selection. **SSH host group** returns to the most recently used group tab.

In a named tab, choose **Manage live panes…** from the roster scope menu to add or remove existing SSH panes. Search by profile, pane name, or SSH address, then choose **Apply membership**. Newly added panes appear unchecked for command delivery. Removing a member clears its send selection and leaves its SSH session open. These changes apply to the current tab. The same menu offers **Update saved group…** when the full saved group was opened. Review proposed additions and removals based on roster membership. The preview shows skipped panes without an available saved profile or outside the window. Reopen the group if its saved membership changed elsewhere.

Choose **Saved group actions → Add saved profiles…** to search saved SSH profiles by name or address. Check profiles and review the choice of new or matching live panes. The added panes join the live roster unchecked, and the current draft, filters, and checked hosts stay in place.

Choose **Saved group actions → Review group launch…** for a saved group to pick which hosts to open. Check individual profiles or choose a **Launch recipient set** such as *Canaries*. Each included profile offers live panes that match its settings. A single match is suggested. With several matches, choose the exact pane or open a new one. **Open reviewed group** checks the included profiles, approved connections, and selected recipient set again before opening the tab. Use **Refresh review** if any of them changes. A partial launch uses just its chosen hosts in the new roster. Use **Saved group actions → Edit group** to change the underlying saved membership.

Filter the review by profile name or SSH address to find members in a large group. **Include matches** adds matching hosts, **Skip matches** excludes them, and **Include only matches** limits the launch to their available profiles across all pages. The launch totals include every checked host, even when the filter hides it.

**Use new panes for matches** and **Reuse unique matches** apply to included profiles across every matching page. A blank filter covers all included profiles. **Reuse unique matches** picks a live pane when exactly one is available and keeps the current connection choice for other profiles.

Choose **Save as recipient set…** in the launch review to name and save the included profiles before connecting. The saved set becomes the selected set in the review and is available for future launches from the command palette.

Use **Update recipient set…** to apply the current inclusion choices to an existing set. Pick the set and review its additions and removals before confirming the update.

Use **Manage sets…** to rename or delete a saved set before connecting. Deleting a set keeps the current host and connection choices in launch review.

From a terminal, search the command palette for **Review SSH group** and the group name, or **Review SSH recipient set** with the group and set names. Press Enter to open the review with the matching hosts checked. **Open reviewed group** turns that tab into the live roster. **Cancel** returns you to the tab you came from.

In a named tab, check the shown recipients you want to keep and open **Roster actions → Recipient sets…**. Save a name such as *Canaries* or *Full fleet*. From that tab, you can also search the command palette for **Apply recipient set** and the name, then press Enter. Applying a set replaces the checks, clears the filter, and returns to the named roster. Each saved profile must match exactly one pane in that roster. If a profile is missing or ambiguous, adjust the live roster before applying the set. Applying a set leaves the command ready for a separate **Send** action.

From **Roster actions**, choose **Save selection as group…** to start a new group from the shown, checked panes. The editor preselects each available saved profile once, in roster order, and reports how many selected panes lack an available profile. Review the members and name the group before saving.

Use **Saved group actions → Import host list…** to paste a fleet into the SSH group or choose **Open host list file…** to load it from disk. The editor accepts UTF-8 and BOM-marked UTF-16 files. Enter one saved profile name, host, or SSH address per line. Use `#` for comments and put a space before a comment following an `ssh://` address. Numeric groups such as `ops@rack<1,3>-web<03-01,07>.example:2222` expand left to right, preserving zero padding and the order written. A list can expand to 64 entries. [Polysh host syntax](https://github.com/innogames/polysh/blob/master/tests/test_host_syntax.py)

Host-only entries can reuse a unique saved profile or use the default SSH user from the form. New addresses use the authentication selected in the form and the usual host-key prompt. Select **Preview hosts** to see the expanded addresses, saved profile matches, duplicates, and ambiguous entries. Checked targets default to new panes. A matching open connection can be selected by pane, or **Reuse unique matches** can select the single eligible match for each checked entry. Confirm the reviewed choices to use that mix of existing and new SSH panes. Use **Save selection as group…** afterward for targets backed by saved profiles.

Choose **Save checked hosts as group…** in the import preview to keep the checked entries for future use. Enter a group name and review the suggested names for profiles created from one-time addresses. The group and new profiles save together, using the authentication settings from the import. Saving returns to the group view without opening any SSH connections.

Choose **Roster actions → Export host list…** to preview the addresses of the checked panes shown in the roster. Copy the list or save it as UTF-8 text with one `user@host:port` per line. Repeated endpoints appear once. Paste the addresses into **Saved group actions → Import host list…** or pass a hostname or IPv4 list to `polysh --hosts-file=ssh-hosts.txt`. w11term can reimport the bracketed IPv6 entries. Configure authentication and jump hosts in the receiving client. [Polysh hosts-file option](https://github.com/innogames/polysh#options)

Choose **Saved group actions → Export saved group hosts…** to share a saved group before connecting. In a launch review, **Export included hosts…** previews every included profile in saved order, including hosts hidden by the filter. Both actions offer the same address-only clipboard and file export.

Choose **Roster actions → Upload files…** to send local files by SFTP to the checked hosts shown in the roster. For one file, enter the full remote path, including the filename. For several files, enter an existing absolute remote directory. Each file keeps its local name, and the group reports an outcome for each host and file. Existing files are kept, and a collision leaves other files eligible to upload. Review the recipients and choose **Upload to _n_ hosts**. A host that reconnects during review needs a fresh review. The group offers cancellation while uploads are active.

Choose **Roster actions → Download files…** to collect files from the checked hosts shown in the roster. Enter one absolute remote file path per line, pick a local parent folder, and review the hosts. w11term skips blank lines and repeated paths. It creates a new `ssh-group-download-*` folder and names each completed file by pane ID and, for a batch of several paths, the path's position among the distinct entries. It includes the original filename where Windows permits it. The report shows each host and remote file, its status, and the local filename if it downloaded. Cancelling discards incomplete downloads and keeps completed files.

Choose **Roster actions → Export group variables…** to set Polysh-compatible environment variables in the checked, shown SSH shells. Review the command for each host and choose POSIX shell or Fish syntax. `POLYSH_NR_SHELLS` is the reviewed host count, `POLYSH_RANK` starts at zero in roster order, `POLYSH_NAME` is the SSH hostname, and `POLYSH_DISPLAY_NAME` is the roster label. Each host needs a fresh prompt. If the reviewed prompts or recipients change, review again. The variables are available to later commands in the same shell and need to be exported again after reconnecting to a fresh shell. [Polysh control commands](https://github.com/innogames/polysh#control-commands)

Under **Settings → Saved commands**, add a name and a one-line command you reuse. In **Command history**, select a completed command and choose **Save as command…** to keep its original text under a name you choose. Open the command palette and search for the name to insert the command into the focused, writable direct SSH or local shell. You can edit the line in the shell before pressing Enter. Commands are stored as plain text on this Windows account.

Choose **Panes → New tab in current directory** to start another shell in the folder the focused shell reports. The new tab opens at full width and the original pane keeps its running program. To see both shells side by side, choose **Split right in current directory** or **Split below in current directory** from the same menu. All three actions are also available in the command palette and under **Settings → Keyboard**.

In a local PowerShell pane, change to a checkout and use either action to start a separate PowerShell process there. The bundled PowerShell integration reports the folder with OSC 7. Existing prompts can also report a drive path with Windows Terminal’s OSC 9;9, for example `ESC ] 9 ; 9 ; "C:\My Projects" BEL`. Both quoted and unquoted paths work with BEL or ST termination. A reported location such as `Env:` clears the previous directory. [Windows Terminal shell integration](https://learn.microsoft.com/en-us/windows/terminal/tutorials/shell-integration)

On an ordinary SSH connection, the actions use the directory reported by the remote shell and open another shell on the same server. w11term recognizes OSC 7 file URIs and the paired OSC 1337 `RemoteHost` and `CurrentDir` reports from iTerm2 shell integration. The iTerm2 report must name the SSH login and host. If you connect through an alias and the shell reports a different hostname, set `iterm2_hostname` in the remote shell to the hostname in your SSH profile. The server must provide a POSIX shell and accept SSH exec requests. [iTerm2 Bash integration](https://iterm2.com/shell_integration/bash)

In a named WSL shell, the same actions open another shell in the distribution using the reported directory. The shell can send an OSC 7 file URI such as `file:///home/dev/my%20project`, or an OSC 9;9 path from `wslpath -w "$PWD"`. For example, follow the Bash or Zsh instructions for reporting the working directory. Duplicate and Reopen start fresh WSL processes using each pane’s saved launch directory. [WSL prompt examples](https://learn.microsoft.com/windows/terminal/tutorials/new-tab-same-directory#bash)

Choose **Panes → Copy current directory** to put the focused shell’s reported path on the Windows clipboard as plain text. It works in direct SSH, PowerShell and named WSL panes after they report a directory. WSL paths use Linux syntax, including Windows drives mounted by that distribution. Find the command in the palette or assign it a shortcut under **Settings → Keyboard**.

Choose **Panes → Open current folder in File Explorer**, or search for it in the command palette, to browse the folder reported by the focused PowerShell or named WSL shell. For Linux paths, Explorer opens the folder through that distribution’s Windows share. The action becomes available after the shell reports a supported directory. [WSL file access from Windows](https://learn.microsoft.com/en-us/windows/dev-environment/wsl-interop#access-linux-files-from-windows)

In a direct SSH or named WSL pane, choose the setup for your shell from the Panes menu or command palette. Use **Copy Bash directory setup** for Bash 5.1 or later, **Copy Zsh directory setup** for Zsh, or **Copy Fish directory setup** for Fish. Paste the setup into your shell prompt and press Enter. The Fish setup replaces Fish’s stock directory reporter so the path works when the SSH alias and shell hostname differ. For future sessions, add the setup to your shell startup file, such as `.bashrc`, `.zshrc` or `~/.config/fish/config.fish`. [Bash prompt hooks](https://www.gnu.org/software/bash/manual/html_node/Bash-Variables.html#index-PROMPT_005fCOMMAND) · [Zsh prompt hooks](https://zsh.sourceforge.io/Doc/Release/User-Contributions.html#Manipulating-Hook-Functions) · [Fish prompt events](https://fishshell.com/docs/current/language.html#event-handlers)

Choose **Copy Bash shell integration**, **Copy Zsh shell integration**, or **Copy Fish shell integration** for directory reporting, prompt navigation, command history and command completion alerts together. Paste the setup into the matching interactive shell and press Enter. For future sessions, add it after your theme in `.bashrc`, `.zshrc`, or `~/.config/fish/config.fish`. Bash 5.1 or later uses `PS0` and `PROMPT_COMMAND` with the default `promptvars` option. Its command history uses the text displayed by the shell. Zsh requires the default `PROMPT_PERCENT` setting. Fish uses its built-in markers when available and installs event hooks otherwise. If your theme or another plugin already emits semantic markers, choose the directory-only setup for that shell. [Bash interactive behavior](https://www.gnu.org/software/bash/manual/html_node/Interactive-Shell-Behavior.html) · [Zsh prompt expansion](https://zsh.sourceforge.io/Doc/Release/Prompt-Expansion.html) · [Fish prompt markers](https://fishshell.com/docs/current/language.html#feature-flags) · [OSC 133 markers](https://learn.microsoft.com/en-us/windows/terminal/tutorials/shell-integration)

Shells that already emit VS Code’s **OSC 633** markers can use the same prompt navigation, command history and directory actions. w11term reads the shell’s reported command line, exit status and working directory. The directory is resolved using the pane’s configured SSH, PowerShell or WSL backend. [VS Code shell integration sequences](https://code.visualstudio.com/docs/terminal/shell-integration#_supported-escape-sequences)

Saving the workspace remembers the starting directory of the new tab or split. Its next launch opens a shell there.

Windows PowerShell 5.1 can treat square brackets in its starting directory as a wildcard. For those paths, w11term restores the literal directory if PowerShell opens at `$PSHOME` or a matching sibling. A profile that intentionally selects that same fallback directory will also start in the requested folder.

Click a pane to focus it, drag a divider to resize, or zoom one pane and restore the layout when finished. `Alt+Shift+Arrow` moves the nearest divider on that axis by five percent of its span. Hold the chord to keep moving it. The same resize actions appear under **Panes**, and saving a workspace keeps its divider positions. The control CLI keeps targeting the same pane as focus and layout change.

**Panes → Focus previous pane** returns to the last terminal pane you used in this window, including a pane in another tab. Invoke it again to switch back. For example, you can alternate between an SSH editor and a PowerShell build. From Settings, the action returns to the terminal you just left. Find it in the command palette or assign a shortcut under **Settings → Keyboard**.

Choose **More → Pin tab** or use the command palette to keep a terminal tab on the left of the strip. A pushpin marks the tab and its close button disappears. **Close tab** and **Close pane** leave it open when the action would close the whole tab. You can still close an individual split, move the live tab to another window, or close the window itself. Choose **Unpin tab** to restore the tab’s close button. Saved workspaces remember pins.

Choose **Close other tabs** or **Close tabs to the right** from the More menu or command palette. Both preserve the selected tab and all pinned tabs. Closed terminal layouts enter the usual reopen history, with the rightmost eligible tab reopening first.

Choose **More → Duplicate tab layout** to open a fresh copy near the current tab. It keeps the tab name, pane arrangement, divider positions, focused pane, zoom and read-only input locks. Each SSH pane makes an independent login, and each local shell starts a new process. The original tab keeps its jobs and scrollback. A configured local or SSH starting directory carries over. Copies start unpinned. You can assign **Duplicate tab layout** a shortcut in Settings.

If you close a terminal tab by accident, choose **More → Reopen last closed tab layout** in the same window. The tab returns to its former position with its name, splits, focus, zoom and input locks. SSH panes log in again and local shells start new processes with empty scrollback. Each window remembers up to ten eligible layouts. Repeat the action to reopen them from newest to oldest. Closing Settings or a tab that runs a startup command, durable session or game attachment leaves that history intact. The action also appears in the command palette and can be assigned a shortcut in **Settings → Keyboard**.

Choose **More → Reopen last closed pane** to recover one ordinary SSH or local shell that you closed from a split. It starts a fresh shell with the pane name and input lock, then focuses it on the original side of the divider. The other panes keep running. If those panes moved or the original split position is gone, the shell opens in its own tab. Each window remembers one eligible split pane. The action is also available in the command palette and **Settings → Keyboard**.

Duplication works with ordinary SSH and interactive local shell launches. The menu explains when a workspace startup command, durable viewer, RustMUSH player session or local command makes a tab ineligible. If a profile has been edited or removed, the duplicate still uses the original pane’s connection settings.

Choose **More → Panes → Move tab to new window** to move the whole terminal tab, including every split, to another native window. The tab keeps its title, focused pane, split proportions and zoom. Each SSH channel and local shell keeps running with its scrollback. The last tab leaving a window closes that window after the destination takes ownership. Both windows share a process and control pipe. Use the pane’s `window_id` in `w11term-cli list` to find its current window.

Use **More → Panes → Move tab to existing window** to move the live tab back into another window in the same process. Choose a destination by its window ID and active tab title. The moved tab joins the end of the destination’s tab strip and becomes selected. The command palette also lists the available destinations.

To monitor an SSH or local shell, choose **More → Panes → Enable read-only pane input** or search for **read-only** in the command palette. The pane’s status line and picker entry show **Read-only**, and `w11term-cli list` reports `read_only: true`. Typing, IME commits, paste, and application mouse input are blocked. A control-pipe write gets a read-only error. Output, terminal query replies, resizing, selection, copying, and scrollback search continue. Use **Disable read-only pane input** to resume typing. The lock follows the live pane when it moves. Newly opened panes start unlocked. Closing panes and SFTP file operations are still available.

To trade places with a neighboring pane in the same tab, choose **More → Panes → Swap pane left**, **right**, **up**, or **down**. Focus follows your shell. Both panes keep their sessions and IDs, and the dividers stay in place. When two panes share an edge with yours, the action uses the same neighbor as directional focus. Unzoom to swap panes. All four actions are searchable in the command palette and can be assigned shortcuts under **Settings → Keyboard**.

In a split tab, the **Pane layout** toolbar menu offers **Zoom pane**. It becomes **Restore panes** while the focused pane is zoomed. The layout commands are also available under **More → Panes**.

Use **Pane layout → Pane text size** or search the command palette for **Increase pane text size** or **Decrease pane text size** to change the focused pane's font size. It follows the pane between tabs and windows and applies to previews and PNG captures. Saved workspaces, duplicated tabs, and reopened panes and tabs keep explicit adjustments. **Reset pane text size** uses the profile or window default again.

Choose **Pane layout → Rotate split** to turn the divider next to the focused pane. Side-by-side panes become stacked, with the left pane on top. Rotate again to restore the original arrangement. In a nested layout this changes only the focused pane’s immediate split and preserves both branches, their proportions and running sessions. The action is available when the split is visible and the panes fit in the new orientation. You can assign it a shortcut under **Settings → Keyboard**.

Choose **Pane layout → Balance panes** to even out the rows and columns in the current tab. For example, splitting the right half of a tab again creates three columns of different widths. **Balance panes** gives each about a third of the available width. It accounts for the dividers and each pane’s minimum size, and keeps your focused pane, running shells and split arrangement. You can also run it from the command palette or assign a shortcut under **Settings → Keyboard**.

To move a running shell beside one in another tab, choose **More → Panes → Move pane to existing tab…**. Search for the destination by title, profile or SSH address and press Enter or click it. Press an arrow key or choose **Move left**, **Move right**, **Move above** or **Move below**. Escape cancels at either step. The moved pane keeps focus, scrollback and its control ID. SSH channels and local processes keep running. The source tab closes if its last pane moves. You can assign a shortcut in Keyboard settings.

By default, a tab shows its focused pane’s label. Turn on **Settings → Use application titles for tabs** to let a shell or editor name its tab. For example, when Vim saves the shell title, sets its own and restores the original on exit, the tab header follows each change. Switching between split panes shows the focused pane’s title. The window title and tab switcher follow the same setting.

Choose **More → Rename tab…** to give a group of panes a name such as `Production logs`. That name takes precedence across the tab strip, window title, tab switcher and pane picker. Saving a workspace keeps the name. Choose **Use default** to let the focused pane supply the label again. The control pipe’s `list` reply includes the optional explicit `tab_title` alongside each pane’s own `label` and terminal `title`.

Choose **Pane layout → Rename focused pane…** to tell two shells on the same host apart. The command is also available under **More → Panes**. For example, name one `Deploy` and the other `Logs`, then type either name in the pane picker. You can still search by the shell’s title, profile or SSH address. Your pane name follows the shell when you move it and is saved with its workspace. An unnamed tab shows its focused pane’s custom name, even when application titles are enabled. **Use default** restores the usual label for that pane.

Choose **+ → Windows PowerShell · Local** to start a local shell or **More → Panes → Split right with profile** to put it beside an SSH session. PowerShell 7 appears separately when installed. Under **Settings → New local shell profile…**, choose an executable, its arguments and an optional starting directory. Each argument is a separate value, so paths with spaces need no extra quoting. The same profiles work with `--profile`, `--tab` and `w11term-cli open --profile`.

Installed Windows Subsystem for Linux distributions appear as **WSL: <distribution>** local profiles. Open them from **+**, split with them from the pane menu or command palette, or save them in a workspace. Each starts in the distribution user's home directory. w11term reads the current user's registrations when it loads the profile catalog and leaves Docker Desktop and Rancher Desktop helper distributions out of the menu. [WSL launch options](https://learn.microsoft.com/windows/wsl/basic-commands)

Drop files or folders from Explorer onto an interactive PowerShell or named WSL pane to insert their paths as quoted arguments. A drop focuses the pane and leaves the command ready for you to edit or submit. WSL uses the selected distribution’s `wslpath` for Windows drive paths and accepts Explorer paths inside that distribution. To upload a file over SSH, drop it on the SFTP file drawer. [WSL path translation](https://learn.microsoft.com/windows/dev-environment/wsl-interop#path-translation)

For current-directory splits, command history and completion alerts in an interactive local PowerShell, choose **Settings → Edit…** for that profile, check **Enable PowerShell integration**, save, and open a fresh pane. The setting defaults to off. It works with Windows PowerShell and PowerShell 7 and can also be set with `"powershell_integration": true` on a local profile. The bundled `shell-integration.ps1` runs under the shell’s normal script policy, wraps its existing prompt and PSReadLine reader for that process, and leaves the profile file and keyboard bindings alone. The checkbox is available for interactive launches with supported startup switches. Profiles that run a script or a command keep their configured invocation.

History shows 0 for success and the native exit code when PowerShell supplies a changed, nonzero `$LASTEXITCODE`. A failed cmdlet and a native program that repeats the preceding nonzero code can look identical to the shell hook. History reports 1 for that case. The hook preserves the shell’s own `$?` and `$LASTEXITCODE` values.

Saved workspaces record the local executable, arguments and directory. Opening the workspace launches a fresh process in that directory. Closing the pane terminates its local process tree. Remote SSH panes keep their own shell, input stream and SFTP connection.

On an SSH pane, open **SFTP files** from the title bar or **More → Panes**. The drawer keeps Back, Forward, Up, Refresh, Find, Upload, and Download available above the file list. Select a directory and choose **Open folder**, or select a file and choose **Download…**. **More folder actions** opens bookmarks, folder creation and upload, and folder reports. You can also drag files and folders together from Explorer onto the drawer to upload them into the displayed directory. The footer shows confirmed bytes and cancellation during a transfer. Transfers stay attached to the originating pane when you switch tabs.

Select a row and open **Selected entry actions** beside Download for commands such as Properties, Preview, Rename, and Delete. File type and the current selection determine which commands are available. The Open folder button sits beside the path.

The **Sort files** dropdown below the list shows the current sort. Expand **File options** for shell-directory following, overwrite, and timestamp or atomic upload preferences. The collapsed header shows when following or overwriting is on.

Choose **More folder actions → Filesystem space…** to check the filesystem containing the displayed remote folder. The report shows total and used capacity, space available to unprivileged users, any reserved free space, and the server’s read-only flag when set. **Refresh space** reads a new snapshot over the same SSH connection. The server must support the OpenSSH `statvfs` extension. [OpenSSH filesystem statistics](https://github.com/openssh/openssh-portable/blob/V_10_2_P1/PROTOCOL#L2206-L2262)

Choose **More folder actions → Folder contents…** to total the reported lengths of regular files in the displayed folder and its subfolders. The scan reads SFTP metadata and skips symbolic links. Sparse files count by their logical length, and each hard-linked name counts separately. The scan reports missing sizes and unreadable folders, and labels the total partial if it skips unmeasurable entries or reaches the existing scan bounds of 128 folders, 16,384 entries, or 24 nested levels. **Stop scan** keeps the partial totals, and **Measure again** starts a fresh scan.

Choose **Select entries…** to mark files or folders across pages, sorts, and filters. **Mark all matches** adds every file and folder that matches the current filter across all pages. The drawer shows how many existing marks are outside the filter. **Clear marks** clears the whole selection. **Download marked** saves the selected entries to a local folder. An existing destination, transfer error, or cancellation stops the batch and keeps items already saved. **Move marked entries…** moves them to another folder on the same SSH connection. The drawer refreshes the source listing and keeps untouched entries marked. Use **Stop after current move** to skip the remaining queued moves.

Enable **Preserve modification times** in the SFTP drawer to keep the server’s reported modification times on downloaded files and folders. This covers search results, batches, and verified or resumed downloads. Entries without a reported time keep their local write time. Scripts can select the same behavior with `--preserve-mtime`. [SFTP control commands](CONTROL.md#run-sftp-operations)

If a drawer transfer finishes while another pane is active or the drawer is closed, the **Attention inbox** gets one result for the transfer or batch. Select the result to open the originating pane’s SFTP drawer.

Open **SFTP transfers** from the More menu or command palette to see active drawer uploads and downloads across the current window. The tab shows the current item and byte progress for each transfer. Cancel a transfer from the list or use **Open SFTP drawer** to visit its pane.

**Recent transfers** keeps the last 20 results from open panes in that window, newest first. Each result shows its outcome and the original transfer message. Downloads with saved items offer an Explorer action for the saved file or folder.

Expand **Transfer item details** to see what happened to each attempted upload or download at the top level. The details show selectable Windows and remote paths, the item outcome, and any byte count reported by the transfer. A folder is one item. Large batches show the first 64 completed items and always include the item that stopped the batch, if any. The report counts any remaining items that never started.

For a completed item, **Show remote entry** returns to the originating pane and selects its current path in the SFTP drawer while the original SSH connection is available. **Show in File Explorer** beside a completed batch download selects that exact saved item. **Open download folder** still opens the batch’s destination folder.

Choose **Save as JSON…** beside **Recent transfers** to save the current window’s displayed results, newest first. The versioned report includes transfer IDs and finish times, SSH accounts and addresses, Windows and remote paths, outcomes, saved download locations, and the available item details. It captures the history when you choose Save and remains readable after the panes close.

Scripts can list, cancel, and wait for drawer transfers with `w11term-cli sftp list-transfers`, `cancel-transfer`, and `wait-transfer`. [SFTP control commands](CONTROL.md#control-drawer-transfers)

After a file or folder is saved, choose **Show download in File Explorer** in the drawer or beside its inbox result to select it. Use **Open download folder** for a batch that saved at least one item. Each inbox result keeps its own destination so you can find earlier downloads.

For a saved SSH profile, open **More folder actions → Default download folder → Choose default download folder…** and select a folder on a local Windows drive. File, directory, and marked-item download dialogs for that profile open in the chosen folder, including downloads from search results. You still choose the destination for every transfer. Select **Use Windows default folder** to clear the preference. Windows also uses its usual starting folder if the saved folder is unavailable.

Select a regular file and choose **Preview text…** to read it over the pane’s existing SSH connection. You can select and copy text from the preview. It accepts UTF-8 files up to 256 KiB, including empty files. **Close preview** returns to the listing.

Choose **Compare with local file…** from a text preview to see changed lines with their original remote and local line numbers. The comparison includes line endings, final newlines and UTF-8 byte order marks. It compares a local UTF-8 file up to 256 KiB with the remote snapshot already displayed. Choose **Return to preview** to go back to the remote file.

To compare two remote text files, open the first preview and choose **Set as comparison reference**. Browse to the second file and choose **Compare selected with reference…**, or use **Compare with reference…** in the search results. The view shows both paths and uses the captured reference text. The reference belongs to the SSH pane and expires when it reconnects. Use **Clear comparison reference** when you’re done.

To compare remote folders, open the first folder and go to **More folder actions → Compare folders**. Choose **Set folder as comparison reference…**, browse to the second folder, and choose **Compare folder with reference…** from the same menu. The report compares relative paths, entry types, and regular-file sizes and modification times. Entries with missing metadata and otherwise matching symbolic links appear under **Needs inspection**. Filter the complete result by change type and by a case-insensitive substring of the relative path. Use **Previous differences** and **Next differences** to browse the filtered results. The scan reads directory metadata and can see changes made while it runs. A stopped scan, an unreadable folder, or a scan that exceeds the limits leaves the previous reference in place. The reference lasts until you clear it, close the pane, or reconnect.

Choose **Set Windows folder as reference…** to capture a folder from the Windows file picker. Then use **Compare folder with reference…** on a remote folder. The Windows snapshot stays in w11term and uses exact relative path spelling and timestamps at SFTP’s one-second resolution. Junctions and other reparse points appear as entries without traversing their targets. Choose the Windows folder again to capture later changes.

Choose **Save comparison as JSON…** to write the captured report to a file. The versioned report includes the SSH account and endpoint, both folder paths and their origins, the comparison totals, and every difference in path order regardless of the current filters. File sizes are in bytes and modification times are Unix seconds. A missing side or unavailable metadata is represented by JSON `null`.

Choose **Show in drawer** beside a difference with a current remote entry to open its containing folder and select it. The drawer selects symbolic links themselves. If the entry is gone or its type or containing path has changed, the comparison stays open so you can scan again. When the action opens a subfolder, **Back** returns to the folder you compared.

Choose **Verify file contents…** and enter an exact relative path from the reference to compare the live files by SHA-256. This works with a Windows or remote reference and can find byte changes even when the file size and modification time match. Each file may be up to 32 MiB. The result describes the bytes read during verification, independently of the captured metadata report.

Select **Files with matching metadata** in the comparison filter to browse regular files with equal reported size and modification time. Use **Verify contents** on any row with regular files on both sides to start a SHA-256 check for that exact path. **Back to comparison** restores the filter and page.

The comparison remembers the last completed SHA-256 check for each path until you choose **Compare again** or close the report. The JSON export lists those checks separately from the metadata differences, with both digests, a result, and the completion time in Unix seconds.

Choose **Verify files on this page…** to check every row on the current page that has a regular file on both sides. The dialog shows progress and any per-file errors. **Stop page verification** keeps completed checks and leaves the remaining files unchecked.

Use the comparison filter’s **SHA-256** options to find paired files whose contents differ, whose contents match, or whose check has yet to complete. These options cover files with both matching and changed metadata and work with the relative path filter.

Choose **Preview image…** on a PNG, JPEG or GIF in the SFTP listing or search results to see the image and its original dimensions. The preview reads a regular file over the pane’s existing connection and displays a scaled still image, including the first frame of an animated GIF. It accepts files up to 8 MiB with dimensions up to 4096 pixels per side and up to 8,388,608 pixels in total.

Choose **Tail text file…** in the listing or search results to read up to 200 lines from the last 64 KiB of a regular UTF-8 file. The drawer shows the file size and indicates when it omitted earlier text. **Refresh tail** reads the path again, so it picks up appended output or a log file replaced by rotation. You can select and copy the text.

Choose **Follow tail** to refresh about once a second and keep new output in view. The last snapshot stays visible while the file is temporarily unavailable during rotation. **Pause follow** stops the automatic reads and keeps the snapshot available to select and copy. Closing the tail also stops the reads.

Choose **Properties…** on a file, folder or symbolic link to read its current metadata. The dialog shows the remote path, entry type, size in bytes, octal permissions, numeric owner and group, and modification and access times in UTC. Unreported fields are marked **Not reported**. For a symbolic link, the dialog shows the link’s own attributes and asks the server for its target path. This also works for a dangling link.

Choose **Open link target** in a symbolic link’s Properties to let the SFTP server resolve it. A directory opens in the drawer. A regular file is selected in its containing folder. If the server cannot resolve the link or list the destination, the drawer keeps the link’s Properties open and shows the error. If opening the target changes folders, **Back** returns to the source folder. **Back to search results** restores the search if you opened Properties from a result. [SFTP path resolution](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6.11)

Choose **Edit permissions…** in the Properties of a regular file or directory with a reported mode. Enter three or four octal digits and choose **Apply permissions**. Three digits are equivalent to a leading zero. The drawer rechecks the entry’s type, mode and ownership on the same SSH connection, sends only the new mode, and shows the resulting mode reported by the server. This edit applies to the selected entry. The server’s permissions policy still applies.

w11term uses OpenSSH’s `lsetstat` extension when available to avoid following the final symbolic link. Other servers use SFTP v3 `SETSTAT`, which addresses the entry by path after the recheck. If the acknowledgement is lost, reopen Properties to check the remote mode. [OpenSSH protocol](https://github.com/openssh/openssh-portable/blob/V_10_2_P1/PROTOCOL#L2304-L2324) · [SFTP v3 attributes](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6.9)

In a regular file’s properties, choose **Calculate SHA-256** and then **Copy SHA-256**. w11term reads the file over the existing SFTP channel and hashes it locally without saving a copy. Close the calculation to cancel. The digest describes the bytes read and covers the whole file. Reported size or modification-time changes during the read cause an error. **Compare with local file…** shows whether a local file matches the displayed digest and requires no second SFTP read. Choose **Use for verified download** to prefill that digest and optionally enable **Resume interrupted download** before choosing a destination.

Choose **Edit** in the preview to change the text. **Save** replaces the original file. **Save as…** creates a sibling file under a new name. Editing preserves the UTF-8 byte order mark and the line endings on unchanged lines. Each pane keeps one in-memory draft when you close its editor. Use **Resume draft** in the drawer to return to it or **Discard draft…** to remove it. After an SSH reconnect, copy any edits you need, discard the old draft, and reopen the remote file to edit against its current contents. Closing the pane ends its draft.

Choose **New text file…** to start a blank draft in the displayed folder. Enter a filename and choose **Create file** to save it. New files use UTF-8 and LF line endings, and can be empty. If the name already exists, the editor keeps the draft so you can try another name. **Resume draft** restores both the filename and text after you close the editor.

Before replacing a file, w11term compares its contents to the version you opened. If another writer changed it, your draft stays available for **Save as…**. Replacement uses the server’s OpenSSH atomic rename extension and preserves the POSIX mode, owner and group when reported. The replacement gets a new inode, which matters for hard links, ACLs and extended attributes. SFTP has no atomic compare-and-swap, so another writer can still change the file between the last check and the rename. Servers with standard SFTP v3 support **Save as…**. If the connection drops during publication, refresh the directory and preview the destination to see which version reached the server.

**Copy directory path** copies the displayed remote directory to the Windows clipboard. **Copy selected path** copies the full remote path of the selected file, folder or symbolic link. Both copy plain text, preserving spaces and Unicode. A symbolic link keeps its own path.

Choose **Download and verify…** on a file and paste its expected SHA-256 digest from a source you trust. w11term checks the downloaded bytes before saving to the chosen destination. If the digest differs, the temporary download is discarded and any existing destination is kept, even with **Replace existing files** enabled.

Enable **Resume interrupted download** to keep a partial file beside the destination after a disconnect, cancellation, or restart. Repeat the download with the same destination and digest to continue. w11term checks the SHA-256 of the entire file before publishing it. To abandon an interrupted download, delete its `.w11term-download-*.part` file after the transfer stops.

Choose **Download folder…** on a remote directory and select a Windows folder
to receive it. For example, choosing `Downloads` for `/tmp/diagnostics`
creates `Downloads\diagnostics`. The transfer preserves regular files, dotfiles
and empty directories, and reports skipped symbolic links and special entries.
It stages the tree beside the destination and publishes it after the download
finishes. An existing destination or a name Windows cannot represent stops
the download. Use **Cancel transfer** in the drawer to stop it.

Choose **Upload folder…** to copy a Windows folder into the displayed remote
directory. Choosing `C:\Projects\cockpit` while viewing `/tmp` creates
`/tmp/cockpit`, including nested files and empty directories. That name
must be available. The drawer counts skipped junctions and other reparse
points and shows confirmed progress. If you cancel or disconnect after the
remote folder is created, inspect the partial tree at the reported path.

Enable **Preserve upload timestamps** to keep the Windows access and modification times of uploaded regular files. This applies to folder contents, Explorer drops, and atomic file uploads. Scripts can use `--preserve-times`. The SFTP server must accept both timestamps. [SFTP file attributes](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-5)

An Explorer drop keeps the item order supplied by Windows. **Replace existing
files** applies to individual files in that batch. Each dropped folder needs a
new remote name. Dropping a junction or another reparse point as a top-level
item stops the batch before it starts. w11term verifies each queued item before
uploading it. On a volume that cannot identify files reliably, w11term keeps
the selected item open until its turn. If w11term cannot verify an item, an
upload fails, or you cancel, the batch stops. Earlier completed uploads stay
in place.

When a file row or drawer control has keyboard focus, use the keyboard to operate that control. For example, press **Space** on a file row to select it. Click the terminal pane, or choose **Done** in the filename filter, to resume typing into the shell while the drawer stays open.

Choose **Go to folder…** to enter a remote directory directly. An absolute path such as `/var/log` works even if the starting directory could not be listed. Relative paths start in the displayed directory. The SFTP server resolves `.`, `..` and symbolic links. Enter spaces, quotes and dollar signs literally. If the folder cannot be opened, the drawer keeps the previous listing and selection.

Use **Back** and **Forward** to revisit folders opened in the current SSH pane, including folders visited while following the shell. For example, after jumping from a project to `/var/log`, choose **Back** to return to the project. Opening a different folder from there starts a new history branch. Refreshing the listing or trying a path the server cannot open preserves your place. Reconnecting starts a fresh history.

Choose **Cancel transfer** to stop the current upload or download and skip the rest of that batch. The drawer waits for the server to acknowledge an in-flight request. A cancelled download leaves an existing local file intact. A cancelled direct upload can leave a partial file once it opens its final destination. The drawer reports the remote path and acknowledged byte count. The SSH shell and subsequent transfers remain available.

Enable **Upload files atomically** before selecting or dropping regular files to stage them next to their remote destinations. Each file is published after all writes and the close are acknowledged. Replacing an existing regular file also requires **Replace existing files** and the server's OpenSSH atomic rename extension. New files start with private permissions. Replacements keep reported ownership and ordinary mode bits, and get a new inode. After a connection loss, the reported temporary path may still exist. If the rename was already sent, refresh to check the destination before retrying.

Choose **Filter filenames…** and type part of a name to search the entire directory listing, including later pages. Matching ignores case and treats punctuation literally. Filtering uses the listing already in memory. **Done** returns keyboard input to the terminal and keeps the results visible. **Clear filter** brings back all entries. Refresh keeps your filter in the current directory, and opening another directory starts with its full listing.

Choose **Find files…** to search the displayed directory and its subdirectories by filename. The default is a case-insensitive literal search. Use **Regex** for a regular expression or **Match case** to distinguish uppercase and lowercase. Select a result to open its containing folder with the entry selected, or use **Download…** or **Download folder…** beside a regular file or folder. Direct downloads use the drawer’s destination and overwrite rules, show cancellable progress, and keep the search results in place. The search uses the pane’s SFTP channel, skips symbolic links when descending, and reports incomplete results if it reaches a limit. Cancelling the first scan keeps the matches already collected.

Use **File text contains** to search inside remote UTF-8 text files. It can be the only query or be combined with a filename and the other filters. **Regex** and **Match case** apply to both queries. Each result shows the first matching line and an excerpt. **Preview match…** opens the first current matching line with two lines of context on each side. Use **Previous match** and **Next match** to move between matching lines in the same file. **Show full file** opens the complete text, and **Back to match** returns to the selected line. **Edit** opens the full file for editing. If the file changed and all matches disappeared, the preview shows the current file with a message. This search considers regular files up to 256 KiB and attempts up to 256 file reads per scan. The result summary reports skipped files and any reached limit. Use `--contains TEXT` with `w11term-cli sftp find`. Saved searches retain the text query.

While a search runs, the drawer shows the folder being scanned and counts for inspected entries, completed folders, and matches found. Refreshing keeps the previous results visible until the new scan finishes.

Enable **Match path** to search the path relative to the search folder. Paths use `/` separators and have no leading slash. For example, combine **Regex** and **Match path** with `^src/.*\.rs$` to find Rust files under `src`. The CLI equivalent is `w11term-cli sftp --pane 1 find /repo '^src/.*\.rs$' --regex --match-path`. Saved searches keep this option.

Enter a regex in **Exclude folder paths** to skip matching folders and their contents. The regex uses paths relative to the search root and follows **Match case**. For example, `(^|/)(\.git|node_modules)$` skips those folder names at any depth. The CLI accepts the same pattern with `--exclude-dir-regex`. You can use exclusions on their own or with the other filters, and saved searches retain them.

Open **More filters** to edit folder exclusions, file sizes, and modification dates. **Hide filters** gives the results more room and keeps the selected advanced filters visible in a summary.

Use **Min size** or **Max size** to find regular files within inclusive size limits. Enter whole bytes or binary units such as `16 MiB`. Leave either bound blank for an open-ended range. Size filters apply before the result cap, and files whose size the server cannot report are omitted. The CLI offers `--min-size` and `--max-size`, and saved searches retain both limits.

Use **Modified from** or **Modified through** with `YYYY-MM-DD` dates to filter by modification time. Both dates are inclusive UTC days. To search a single day, enter it in both fields. Date filters apply before the result cap, and entries whose modification time the server cannot report are omitted. Use `--modified-from` and `--modified-through` in the CLI. Saved searches retain these dates.

Sort retained results by containing folder, filename, modification time, or file size. Sorting starts on page one, keeps marked downloads, and uses the scan’s existing metadata. Results without a reported modification time or regular file size sort last for that column. **Copy all paths** follows the displayed order across pages.

On a saved SSH profile, open **Saved searches…**, enter a name, and choose **Save current search**. The preset records the remote folder, query, filters, and sort order. Choose **Run saved search** to scan that folder again from any folder in the drawer. Saved searches belong to the profile and the verified SSH endpoint. **Remove saved search** removes the preset from the profile.

Choose **Refresh results** to scan again with the same settings. The previous results stay visible during the scan. A successful refresh keeps marks for paths that still match with the same entry type. Cancelling a refresh or encountering a scan error keeps the previous results and marks.

After revealing a match, choose **Back to search results** to return to the same query and results page without rescanning. **Find files…** starts a new search from the current folder. Closing the drawer or reconnecting ends the saved search.

The filename field takes keyboard focus when search opens or you return to the results. With that field focused, **Enter** starts the search and **Escape** returns to the file listing.

Use **Search result type** to find only files, folders, or symbolic links. Choose a type and leave the filename field empty to match any name of that type. Only matching entries count toward the 200-result limit. A file search still descends into real subfolders.

Choose **Properties…** beside a search result to inspect its current metadata without leaving the results. For regular files, you can also calculate or copy the SHA-256 digest and use it for a verified download.

Choose **Preview text…** on a regular file to read its UTF-8 contents, or choose **Edit** in the preview to make an explicit save. **Resume draft** is available from the results if you close the editor with a draft.

Choose **Rename…** beside a file, folder, or symbolic link. The dialog shows the exact source path and renames it within the same remote folder. The search refreshes after success, so the renamed entry appears if its new name still matches.

Choose **Delete…** beside a result and confirm its SSH destination, type, and full remote path. Only files, symbolic links, and empty folders can be deleted. Deleting a link leaves its target intact. After the server confirms deletion, w11term refreshes the current search and keeps marks on unrelated results.

Choose **Shell in containing folder** to open a right split on the same authenticated SSH connection with that folder as its working directory. The search stays open in the source pane.

Choose **Copy path** beside a search result for its absolute remote path, or **Copy all paths** for one path per line from every retained results page. Paths are plain text and keep their spaces, Unicode, and literal names. Copy paths containing line breaks individually.

Choose **Select results…** to mark files and folders from any results page. **Mark all results** adds every file or folder returned by the current search. **Clear marks** clears the selection without closing the results. **Download marked** asks for a new local folder name and keeps the paths relative to the search root. A marked folder includes its subtree, including any separately marked descendants. The new folder appears after the download completes. Cancellation or a transfer error discards the staged download.

**Move marked results…** reviews the selected paths and moves them to one folder on the same SSH connection. All entries keep their names. Unmark descendants of a marked folder and move entries with the same name in separate batches. While moves are queued, choose **Stop after current move** to let the active request finish and skip the rest. The batch also stops after the first failure. It reports the number of confirmed moves and refreshes the search. Confirmed entries stay moved. Entries still at their original paths keep their marks after a successful refresh.

In **Sort files**, choose **Newest first** to find a recent log or capture, or **Largest first** to find the largest regular files. Folders stay first. **Newest first** uses the reported modification time within each group. **Largest first** keeps folders alphabetical and puts files with a reported byte count ahead of other entries. **Sort by name** restores alphabetical order. Changing the sort uses the cached listing and keeps the selected filename in view, including when it moves to another page. Each pane starts with name sorting and keeps its own choice.

The list shows reported regular-file sizes beside their names in bytes or binary units such as KiB and MiB. An empty file shows **0 B**. Rows without a regular-file size have a blank size column. Use **Refresh** to update the listing after a transfer or a change on the server.

Choose **New folder** and enter a name to create a directory under the displayed remote path. After the server confirms creation, the drawer refreshes and selects the new folder. If the name already exists, the drawer shows the server’s error. **Replace existing files** applies to file transfers.

Select a file or directory and choose **Rename…** to change its name in the current remote folder. The drawer checks for an existing destination, sends a standard SFTP rename, and selects the new name after the server confirms it. Renaming a link changes the directory entry itself. If the connection drops before confirmation, refresh the parent directory to see which name the server retained.

Choose **Move to folder…** in the listing or on a search result to move a file, folder, or link under its existing name. Enter an absolute destination or a path relative to the entry’s containing folder. **Browse folders…** starts at the typed path or the entry’s containing folder. Navigate to a destination and choose **Use this folder** to fill in its path. Select **Move** to confirm. The destination folder must exist on the same SSH connection. A confirmed move refreshes the source listing or reruns the search. If the connection drops before confirmation, refresh both folders to check the outcome. A server can reject moves between filesystems.

**Copy to folder…** copies a regular file under its existing name on the same SSH connection. w11term requests mode `0600` for the new file, stages its contents, and publishes the final name after the copy completes. An existing destination name stops the copy. Servers advertising `copy-data` can copy the bytes internally. Other servers send the bytes through w11term in chunks. If the connection drops while publishing, refresh the destination to check the result. [OpenSSH copy-data](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL#L2418-L2467)

Use **Copy marked files…** in selection mode in the listing or search results to copy several regular files into one remote folder. Each file keeps its name. Copy files with the same name in separate batches. An existing destination or other error stops the batch and reports how many copies finished. **Stop after current copy** lets the active copy finish and skips the queue. Confirmed copies stay in the destination, and the originals keep their marks.

Use **Delete marked entries…** in the listing or search results to review the exact remote paths and permanently delete the selected regular files and empty folders. A nonempty folder or other server error stops the batch. **Stop after current delete** lets the active request finish and skips the remaining entries. The drawer refreshes, reports how many deletes the server confirmed, and keeps marks on untouched entries.

Choose **Rename marked entries…** in selection mode in the listing or search results. Enter literal, case-sensitive text to find and preview every changed name in its original folder. An empty replacement removes the matched text. Before renaming, w11term checks the source types and that every destination is free. Unmark entries inside a folder you are renaming. Each rename rechecks its destination, so a new collision stops the batch. **Stop after current rename** keeps confirmed renames and skips the queue. Untouched entries keep their marks after the refresh.

Choose **Duplicate as…** on a regular file to make a copy in its own folder under a new name. You can edit the suggested name before confirming. An existing name leaves both files intact. The current listing or search filter applies to the duplicate.

SFTP v3 requires the server to reject an existing destination. If another process creates the name between the check and the rename, enforcement depends on the server and filesystem. [SFTP v3 rename semantics](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6.5) · [OpenSSH implementation](https://github.com/openssh/openssh-portable/blob/V_10_2_P1/sftp-server.c#L1187-L1249)

To remove a remote file, symbolic link or empty directory, select it in the SFTP drawer and choose **Delete…**. The confirmation shows the SSH destination, entry type and full remote path. Deleting a link leaves its target intact. A nonempty directory stays in place with its contents. After confirmation, w11term checks the entry type again on the same SSH connection. If the connection drops before the server acknowledges deletion, refresh the parent directory to check the outcome.

SFTP v3 addresses entries by path. Another process can replace an entry between the final type check and the delete request. [SFTP file removal](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6.5) · [Directory removal](https://datatracker.ietf.org/doc/html/draft-ietf-secsh-filexfer-02#section-6.6)

### Share a Windows folder with Linux

Connect to a saved SSH profile, open **More → Shared folders… → Add folder…**, and choose a local NTFS directory. Check the displayed SSH account, host and fingerprint. Enter a mount name and select **Save folder**. For example, sharing `C:\Projects\sprites` as `sprites` makes its files available at `~/windows/sprites` on Linux. New and existing folders default to **Read-only**.

Select **Allow write access** when adding a folder to let programs running as that Linux account create, edit, rename and delete files in it. The saved row shows **Read/write**. To change access, choose **Forget**, then select the directory again. Writable selections exclude w11term’s private data directory and the active profile catalog.

Linux waits for Windows to commit each write. An interrupted upload can leave a partial file. If the connection drops after Windows commits a write but before the reply arrives, check the file after reconnecting. The new mount never replays pending writes. Editing file contents requires local NTFS with Windows transaction support. EFS-encrypted files and conflicting Windows opens can cause a write to fail. Renaming to an existing destination is refused. [Windows transactional file I/O](https://learn.microsoft.com/en-us/windows/win32/fileio/programming-considerations-for-transacted-fileio-)

**Unmount** stops the current export. **Mount** retries it. **Forget** revokes the saved permission. Reconnecting the same profile can remount its folders. A changed host key, account, profile identity or replaced Windows directory requires a new selection. Windows keeps the grants under `%LOCALAPPDATA%\w11term\folder-shares`. A copied profile carries only an opaque identifier. Drive roots, junctions, reparse points and multiply linked files are excluded.

**Mount on connect** is on by default for each saved folder. Clear it when adding a folder to mount it now and leave it unmounted on future connections. You can also change it on the saved row. The checkbox controls future connections. Use **Mount** or **Unmount** for the current one.

Older builds decline to load a grant file containing settings they do not recognize. Before switching back, forget writable grants and turn **Mount on connect** on for folders you want to keep in a build that predates that checkbox.

To stop sharing every folder for a profile, open **Settings**, edit the saved SSH profile and choose **Revoke all shared folders…**. Confirm the profile name. This works while the host is offline. Running exports for that profile refuse new reads and writes. Revocation waits for admitted writes to finish. Reconnecting leaves the folders unmounted until you select them again. Files already read or cached on Linux remain available there. Other profiles and open SSH shells keep working.

On a mounted row, **Copy Linux path** puts its absolute path on the Windows clipboard. **Open in SFTP drawer** returns to the originating pane and lists that path over its SSH connection. This works when the host exposes the mount at the same absolute path through SFTP, even if SFTP starts in another directory.

The remote account needs SSH exec channels, stock SSHFS 3, `fusermount3` 3.15 or newer, access to `/dev/fuse`, and ordinary Linux shell utilities including util-linux. Run `fusermount3 --version` on the host to check. Version 3.14 can leave an owner-only mount behind after SSHFS exits. w11term reports older helpers before creating the mount. The interactive SSH shell remains available. [SSHFS passive mode](https://github.com/libfuse/sshfs/blob/ba33bab77006cfb81cbd6ecc44a8fc2728d24793/sshfs.rst#L150-L154) · [libfuse owner-only unmount fix](https://github.com/libfuse/libfuse/blob/fuse-3.15.0/util/fusermount.c#L1164-L1201)

The **Inbox** button beside the tab menu shows how many bells and terminal notifications are unread across this window. The inbox keeps the newest 128 alerts, including alerts from background tabs and splits. Select a message to mark it read and focus its source pane. **Mark all read** acknowledges the remaining messages, and **Clear inbox** empties the list. Both leave the per-pane control event journals available to scripts.

SSH applications can send Kitty OSC 99 notifications to the Attention inbox, including multipart titles and bodies and action buttons. Run this in a remote shell to send a title followed by a body.

```sh
printf '\033]99;i=build:d=0;Build finished\033\\\033]99;i=build:p=body;Artifacts are ready\033\\'
```

The application can update a notification by ID, close it, or supply an expiry time. Clicking a notification can focus its pane and report the selected action to the originating connection when requested. The pane's event journal records the published message. Capability queries describe the actions, occasions, and payload types supported by the inbox. [Kitty notification protocol](https://sw.kovidgoyal.net/kitty/desktop-notifications/)

Choose **More → Panes → Open files here** to open the drawer at the directory the focused SSH shell reported. For example, the OSC 7 URI `file://server.example/home/me/my%20project` opens `/home/me/my project` on that pane’s connection. The URI must specify an absolute path and either the pane’s SSH host or an empty host. With iTerm2 shell integration, the paired report supplies the absolute path as literal text. An absent, invalid or foreign-host report opens the usual SFTP working directory.

To keep a remote directory for another session, open it in the SFTP drawer and choose **Bookmarks… → Bookmark this folder**. Select its path to reopen it over the current SSH connection, or choose **Remove** beside the path. Bookmarks belong to the saved profile, SSH account, host, port and verified host key. If a bookmarked directory has moved, the drawer shows the error and keeps the previous listing.

On a direct SSH shell, **Open shell here** opens a right-hand split in the directory displayed by the SFTP drawer. You can browse to a folder or reopen a bookmark even when the running shell does not report its working directory. The new shell reuses the authenticated SSH connection, and the original pane keeps its running program and file selection. The server must allow SSH exec and expose the selected path to the shell.

Check **Follow shell directory** under **File options** to track later directory changes from that shell. The drawer waits for active transfers and file pickers to finish, then reads the latest reported directory when visible. It ignores invalid and foreign-host reports. Opening a folder manually or revealing a file turns following off for that pane.

**Replace existing files** starts off. Downloads publish the local destination after the transfer completes. An interrupted upload can leave a partial file on the server, and the drawer reports that outcome. A new SSH connection clears the previous directory and overwrite choice.

Use **More → Workspaces…** to name and save your current terminal tabs. A workspace remembers tab order, split directions and proportions, the focused pane in each tab and which tab you were using. Under **Switch workspace**, type part of a name and select the workspace to bring its live window forward. If the window has closed, w11term opens its saved layout. Saving a workspace associates the current window with that name.

Use `w11term --switch-workspace "Admin console"` for the same behavior from a launcher. Workspaces match within the same Windows user, session, elevation level, workspace catalog and profile file. **Append in this window** adds a fresh copy of the layout to the current window. `w11term --workspace "Admin console"` always opens a new window. When multiple windows have the same workspace open, switching from one of those windows keeps its sessions selected. Switching from elsewhere lists the matching process IDs so you can choose the window yourself.

Each restored pane receives a new process-local control ID. SSH panes normally start a fresh shell. A saved durable pane can return to its original shell. Named profile references use the current profile settings. Ad-hoc SSH panes retain their connection options and key paths. Replay panes reference their original capture files. Workspaces are stored in `%LOCALAPPDATA%\w11term\workspaces.json`, or in `W11TERM_DATA_DIR` when set.

To give an ordinary SSH pane a startup command, choose **More → Workspace startup…**, enter a remote command or an absolute starting directory, and choose **Apply to pane**. Then save or update the named workspace. Reopening it sends the command as an SSH exec request on a fresh PTY. For example, set the directory to `/srv/game` and the command to `tail -f logs/server.log`. A directory alone starts an interactive shell there. Directory selection requires a POSIX-compatible remote shell. Clearing both fields restores the profile’s default shell.

The recipe belongs to that pane in the saved workspace and is bound to its SSH host, port and user. Editing the recipe leaves the running session alone. After the client sends a startup command, a lost connection leaves the pane available for inspection. Reopen the workspace explicitly to run the command again. Directory-only shells retain their normal reconnect behavior.

**Settings → Import OpenSSH config** previews aliases from your Windows SSH config. Choose **Browse…** to select a different file and preview it, or type a path and choose **Preview hosts**. Pick an alias to see its destination, identity, jump hosts and any unsupported directives. Saving creates an editable w11term profile. Import reads the selected file and leaves it intact. Routes or trust settings that cannot be represented are flagged before saving.

Choose **Create SSH group from aliases…** to review several aliases together. Filter by alias name and check the hosts to include. An alias can reuse an identically configured saved SSH profile. Edit the profile name for any new profile, including aliases whose original name is already taken. Saving creates the group and any new profiles together. Open the saved group from **SSH host groups** when you are ready to connect.

If your config starts with `Include config.d/*.conf`, aliases and `ProxyJump` hosts in those local fragments appear in the same preview. Relative include paths start at `%USERPROFILE%\.ssh`, including when you select a config elsewhere. Matching filenames are read in lexical order. An `Include` inside a `Host` or `Match` block is flagged for the aliases it could affect. Saving a profile takes a snapshot of the settings you previewed. Import supports one explicit `CertificateFile` per endpoint, with a private key file or with the Windows SSH agent.

When `IdentityFile` points to a plain OpenSSH public key on a local drive, w11term previews and saves its SHA256 fingerprint as **Specific SSH agent key**. Each jump host can use a different key. A missing or invalid `.pub` file is reported in the preview. If the same endpoint specifies `CertificateFile`, w11term uses the agent with that certificate and keeps the fingerprint as an explicit check. A `CertificateFile` without an `IdentityFile` selects its matching agent key automatically. [OpenSSH IdentityFile](https://man.openbsd.org/ssh_config#IdentityFile)

OpenSSH import reads `IdentityAgent` for each destination and jump host that uses agent authentication. It accepts local Windows named pipes and `SSH_AUTH_SOCK` or `$SSH_AUTH_SOCK`. For a host with `IdentityAgent none`, import uses an explicitly configured private `IdentityFile` when supplied and reports an unavailable profile otherwise. [OpenSSH IdentityAgent](https://man.openbsd.org/ssh_config#IdentityAgent)

Imported `ProxyJump` routes use SSH `direct-tcpip` channels. Each jump host and the final destination have their own authentication and host-key checks. The resulting SSH shell and its SFTP drawer use the destination connection. For a manually configured profile, `proxy_jump` is an ordered array of objects with `host`, `user`, optional `port`, `auth` and optional `host_key`. Each hop supports the same authentication choices as the destination.

In the SSH profile editor, expand **SSH jump hosts** to add or change the route. Use each hop’s **Actions** menu to edit, reorder, or remove it. Apply the hop edit and save the profile. New connections visit the hops from top to bottom before opening the destination shell. Each hop has its own authentication, host-key settings, connection timeout, and keepalives. For private-key authentication, use **Browse…** to select the key or enter a path. Relative paths resolve beside `profiles.json`.

Choose **Settings → Keyboard** to edit shortcuts for panes, tabs, the command palette, the pane picker and workspaces. Select **Edit…**, enter a chord such as `Ctrl+Alt+Right` or `F8`, and choose **Save shortcut**. **Clear shortcut** releases its key for the terminal. **Restore default** restores that action’s original binding. The editor identifies conflicts, and pane menus show the current bindings. Changes apply immediately and persist in the per-user settings file.

| Pane action | Default shortcut |
| --- | --- |
| Split right or below | Ctrl+Alt+Shift+Right or Down |
| Focus a neighbor | Ctrl+Alt+Arrow |
| Resize the focused split | Alt+Shift+Arrow |
| Zoom pane or restore layout | Ctrl+Alt+Shift+Z |
| Close focused pane | Ctrl+Alt+Shift+W |

The **+ → New Profile…** form supports host, user, port, agent/key-file/none/password/keyboard-interactive authentication, an optional verified host fingerprint, device-key generation, and player preferences. **Save and connect** adds the profile to the selected configuration file and opens it. New Profile refuses duplicate names. **Settings → Profiles → Edit…** or **More → Edit current profile…** opens an existing profile with its saved values. The editor supports renaming and all connection/player fields. **Save changes** updates it without opening or disconnecting a session; **Save and connect** opens the updated connection. Connection changes apply to new sessions; sound and game options update open tabs. Edits preserve unrelated fields and unchanged relative key paths, refuse name collisions, and report a conflict if the profile changed on disk while the editor was open. Saving preserves unrelated configuration fields and atomically replaces the file. Only the key file's path is stored; private key contents never go into the configuration.

From the profile editor, **Duplicate saved profile…** opens a copy of the last saved settings, including key paths and custom fields. Give the copy a name, then choose **Save copy** or **Save and connect**. **Delete profile…** asks you to confirm the saved name. Open sessions in this window keep running independently, and key files stay on disk.

To connect once, open **+ → New Profile…**, enter the SSH details and choose **Connect without saving**. The profile name can stay blank. The connection uses the usual authentication and host-key checks, and relative key paths still resolve beside the current `profiles.json`.

For a shorter route, choose **+ → Quick connect…** or search for it in the command palette. Paste an explicit address such as `play@conclav.io:6792`, `ssh://play@conclav.io:6792`, or `user@[2001:db8::1]:2222`, choose authentication, and select **Connect**. Quick Connect uses the existing host-key and authentication prompts. It leaves the profile catalog untouched.

Once a one-off SSH pane is connected, choose **Save SSH connection as profile…** from the More menu or command palette. Enter a new name to add the connection to the profile list and bind it to the current pane. The SSH session and its scrollback stay in place. The profile records the authentication method and any key file path. Passwords and keyboard-interactive answers stay in the session.

The same destinations work with `w11term ssh`. The optional `--port` flag takes precedence over a port in the address.

In a local shell profile editor, **Duplicate saved profile…** copies the saved executable, arguments, starting directory and scrollback settings. Change the name or settings and select **Save copy**. **Delete profile…** removes a custom profile after confirmation and leaves open shells running. If you customized an automatically discovered PowerShell profile, **Restore defaults…** removes its saved overrides and keeps the installed shell in the menu.

The More menu displays **On**/**Off** for fullscreen, mute, game mode, alternate-screen wheel keys, remote named sounds, and asset synchronization, using the active terminal’s effective settings. Terminal-only actions are disabled when a settings/form tab is selected.

Choose **More → Terminal inspector** to see the focused pane’s cell grid, text area in physical pixels, DPI, in-band resize subscription, primary or alternate screen, mouse and keyboard modes, bracketed paste and synchronized output. The panel follows focus across splits and tabs. It reads the local parser, so it can show a pending synchronized update while the terminal still displays the last committed frame. Close it with **×** or the More menu. To inspect a background pane, run `w11term-cli inspect --pane ID`. [Control protocol and example](CONTROL.md#live-pane-inspection)

When shell integration reports a command start, the inspector shows the running command, its starting directory when available, and a live elapsed time. The control reply supplies the full shell-reported command line and its capture ID. When the command finishes, look up that ID in command history.

Choose **Find running commands…** from More or the command palette to see active shell captures across the window, longest-running first. Search by command, starting directory, pane name, profile, or SSH address. **Enter** focuses the source pane, and **Esc** returns to the original pane. The list refreshes while open and removes completed or disconnected commands.

Choose **Show recent command output** or press **F9** to preview up to six terminal rows from the highlighted command, including its current unfinished line. The preview updates about once a second and labels omitted rows. It leaves focus in your current pane and pauses during synchronized updates. Alternate-screen applications can be viewed in their source pane.

Scripts can read the same sample with `w11term-cli get-running-output` using the command's `start_token` from `inspect` or the `command-started` event. [Running command output](CONTROL.md#running-command-output)

Highlight a result and choose **Notify when selected command finishes** to watch it while keeping your current pane. The Attention inbox opens the capture from that notification. Choose **Cancel selected command notification** to disarm it. **F8** toggles the watch for the highlighted result. The watch ends if the selected capture is replaced or its session disconnects.

For an SSH pane, the inspector shows the connection state and the current step and endpoint during login or reconnection. If the connection ends during a failed step, the inspector keeps the step and error visible. Once connected, expand **Verified SSH route** to see the destination and each jump host with its verified SHA256 host-key fingerprint and any configured host-key alias. Each peer shows the initial key exchange, host-key algorithm, cipher, and whether strict key exchange was negotiated. The route disappears when the authenticated connection ends.

For each peer, the same view shows the sign-in method the server accepted. For public-key sign-in it includes the user key fingerprint, whether it came from a key file or agent, and the issuing CA fingerprint for a user certificate. It also shows any completed password or keyboard-interactive steps after the key. Scripts can read the same details from `w11term-cli inspect`. [Control reply fields](CONTROL.md#waiting-geometry-tabs-and-events)

**SSH transport** shows the connection’s ID, and **Other panes** lists the other live panes sharing that login. A split that reuses the connection shows the same transport ID. A separate login to the same profile gets a different one. The list updates as panes connect and disconnect.

Move the pointer over the focused pane to read its column and row, counted from 1 at the top-left of the visible grid, including while viewing scrollback. **Hyperlink** shows the allowed OSC 8 target or a detected URL under that cell. Moving into the padding or another pane clears the readout. Hovering leaves keyboard focus and the link destination alone.

The **More → Settings** command opens profile management, the terminal font selector and programming-ligatures toggle. The selector includes only installed Cascadia Code, Cascadia Mono, Consolas, JetBrains Mono, Monaspace-family, and Fira Code fonts. Changes apply across the window's terminal tabs and are saved automatically to `%LOCALAPPDATA%\w11term\settings.json`. `W11TERM_SETTINGS_PATH` can override that file for portable configuration or isolated tests. Settings and New Profile tabs use native controls and do not send their typing into an SSH session. The plus menu also includes the terminal lab.

Programming ligatures default to enabled for adjacent ASCII cells with matching attributes. Monaspace also enables its programming-ligature feature sets. DirectWrite shapes connected scripts even with programming ligatures disabled. Font changes recalculate cell geometry and resize existing sessions. PNG capture uses the same font and shaping settings as the live pane. [Monaspace features](https://github.com/githubnext/monaspace#code-ligatures)

The terminal follows **Windows Settings → Accessibility → Text size** by default. Changing the slider and choosing **Apply** updates the cell grid in open panes. Terminal zoom sets the base font size, and **Reset font size** restores that base before Windows scaling is applied. Turn off **Follow Windows text size** in w11term Settings for a grid sized by terminal zoom and display DPI.

In w11term Settings, choose **Default cursor style** for a steady or blinking block, bar, or underline. The choice applies in the current window and is saved for new windows. Applications can still set their own cursor style. When they reset it, the pane uses your preference again. Blinking follows the Windows caret interval.

Set **Cursor guide** to **On** for a horizontal rule at the focused pane’s live cursor row. Choose **Follow applications** to let an editor show or hide the rule with iTerm2’s `OSC 1337;HighlightCursorLine` sequence. The guide is off by default. It stays visible while the caret blinks and disappears while browsing scrollback or when the application hides the caret. [iTerm2 cursor guide](https://iterm2.com/documentation-escape-codes.html#cursor-guide)

SSH supports OpenSSH private keys supplied with `--identity`, protected w11term device keys, the Windows OpenSSH agent (the default for `ssh user@host`), `--auth none`, `--auth password`, and `--auth keyboard-interactive`. Password and MFA challenges use native dialogs; hidden answers never enter terminal input, profiles, event logs, or recordings. A successful password remains in memory only for reconnection; one-time codes are requested again. Passphrase-protected OpenSSH key files can be unlocked directly in a masked dialog. The passphrase unlocks the file locally and is cleared after each attempt. Choose **Keep this key unlocked for reconnects** to retain the decrypted key in memory after authentication. The choice applies to that SSH session and key file. The key is released when the pane closes or its SSH worker ends. An independent pane or a changed encrypted file prompts again. Direct unlock supports OpenSSH keys with up to 1024 bcrypt rounds. The SSH agent can also supply a previously unlocked key.

Choose **Private key file** in Quick Connect or an SSH profile, then use **Browse…** to select the file through the Windows dialog. The picker starts beside the entered path when that folder is available locally, or in your OpenSSH `.ssh` folder when it exists.

Choose **Specific SSH agent key** when an SSH destination or jump host should receive a single plain key from the Windows SSH agent. Select **Load agent keys** and pick a key by its comment and SHA256 fingerprint, or enter the fingerprint yourself. A missing or rejected selection stops authentication. From the command line, use `w11term ssh user@host --agent-key SHA256:…`. The profile stores only `"auth": "agent-key"` and `"agent_fingerprint": "SHA256:…"` for this choice. [OpenSSH identity selection](https://man.openbsd.org/ssh_config#IdentitiesOnly)

Use **SSH agent pipe (optional)** in Quick Connect or a profile to choose a local agent, for example `\\.\pipe\work-agent`. Jump hosts have their own field. The key picker and authentication use the same pipe. Leave the field blank to try the process's `SSH_AUTH_SOCK` and use the Windows OpenSSH agent when it is unset. Enter `SSH_AUTH_SOCK` to require that environment variable. The CLI accepts `--agent-pipe PIPE`, and profiles store this as `agent_pipe`.

If your SSH server trusts a user CA, choose **User certificate file (optional)** beside the private key. Each destination or jump host can have its own matching OpenSSH user certificate. From the command line, use `w11term ssh user@host --identity KEY --certificate KEY-cert.pub`. In a profile, the fields are `"auth": "identity"`, `"identity"`, and `"certificate"`. w11term uses the selected certificate for that endpoint and reports a rejected or mismatched certificate as a login error. [OpenSSH user certificate configuration](https://man.openbsd.org/ssh_config#CertificateFile)

If the signing key is in your Windows SSH agent, choose **SSH agent with user certificate** and select the certificate file. w11term finds the matching key in the agent and uses it to sign. From the command line, use `w11term ssh user@host --certificate KEY-cert.pub` with the default agent authentication, or add `--agent-key SHA256:…` to require a specific signing fingerprint. The profile fields are `"auth": "agent-certificate"`, `"certificate"`, and the optional `"agent_fingerprint"`. [OpenSSH CertificateFile](https://man.openbsd.org/ssh_config#CertificateFile)

When a server accepts a key or certificate and requests a password or keyboard-interactive challenge to finish authentication, w11term opens a **Verify sign-in** dialog. This also works with agent keys and on jump hosts. If the server offers both methods, w11term tries keyboard-interactive first. It requests fresh answers for each connection, including reconnects. [OpenSSH authentication methods](https://man.openbsd.org/sshd_config#AuthenticationMethods)

The first connection to an unknown host shows its algorithm and SHA256 fingerprint. **Accept and save** writes the public key to OpenSSH `known_hosts`. Cancel stops the connection. A changed known key fails without offering to replace it. An explicitly verified `--host-key SHA256:...` pin remains an alternative. Headless probes fail on unknown hosts instead of opening a dialog.

To trust a fleet’s SSH host CA, add its public key to `%USERPROFILE%\.ssh\known_hosts`.
For example, a line beginning with `@cert-authority *.example.net ssh-ed25519`
followed by the CA’s base64 key covers that domain on port 22. For port 2222 use
`[*.example.net]:2222` as the host pattern. Each ProxyJump hop uses its own
hostname, port and trust entries.

For a matching CA entry, w11term requests an OpenSSH host certificate and
checks its signature, host principal, validity interval, critical options and
revocations. A valid certificate from the trusted CA can authorize a rotated
host key without a first-connect prompt. A profile fingerprint still pins the
individual host key. Durable sessions and folder grants retain their existing
host-key binding. The supported host certificate and CA key types are Ed25519
and ECDSA P-256, P-384 and P-521. [OpenSSH known_hosts format](https://man.openbsd.org/sshd.8#SSH_KNOWN_HOSTS_FILE_FORMAT)

When you connect through a forwarded port, set **Host key alias** to the name
on the remote server’s certificate or in your `known_hosts`. For example,
`w11term ssh ops@127.0.0.1 --port 2202 --host-key-alias build01.example.net`
dials loopback and verifies `build01.example.net`. The alias is the whole
lookup name even on a custom port. The first-connect dialog shows both the
address and the alias under which it will save the key.

The OpenSSH importer reads `HostKeyAlias` independently for the destination
and each jump. Profiles store it in the optional `host_key_alias` field.
[OpenSSH HostKeyAlias](https://man.openbsd.org/ssh_config#HostKeyAlias)

## Forward a local port

If a development server on the SSH host listens on `127.0.0.1:8000`, open the command palette in its SSH pane and choose **Forward a local port…**. Enter `8000` as the remote port. Leave the Windows port empty to have Windows assign one, or enter a specific port. Review the Windows and remote endpoints shown above **Start forwarding**. Once the forward starts, choose **Copy address** to use the Windows endpoint in your browser or TCP client. You can close the manager while the forward is running. For example, if Windows assigns port `54210`, visit `http://127.0.0.1:54210/` for an HTTP service.

Both endpoints use IPv4 loopback. Each client connection uses the pane's authenticated SSH connection, and the SSH server must allow local TCP forwarding. Use **Stop** on a forward to close its listener and connected clients. Closing the owning pane or losing its SSH connection also stops its forwards, even when another pane shares that login. You can start a fresh forward after reconnecting. [OpenSSH local forwarding](https://man.openbsd.org/ssh#L)

Choose **More → Active port forwards** or find the same action in the command palette to see listeners across the current window, including background tabs. Each row shows its SSH pane, Windows and remote endpoints, and active client count. Copy the Windows address, focus the owning pane, or stop that listener from the list.

The OpenSSH profile importer recognizes loopback port pairs such as `LocalForward 15432 127.0.0.1:5432`. The import preview lists the saved pairs and identifies rules it skips by alias and source line. In the profile's SSH pane, choose **Forward a local port… → Use saved forward… → Windows 15432 → remote 5432** to fill in both ports. Choose **Start forwarding** when you need the connection. After reconnecting, you can select and start the saved pair again. [OpenSSH LocalForward](https://man.openbsd.org/ssh_config#LocalForward)

To manage these pairs directly, open **Settings → Profiles → Edit… → Saved local forwards**. Enter the Windows and remote loopback ports and choose **Add saved forward**, or remove an existing pair. Save the profile and open a new session to use the updated list. Saving the profile leaves listeners under manual control.

## Browse existing Herdr terminals

Choose **More → Find remote sessions → Browse Herdr terminals on _profile_** for a saved Linux SSH profile with Herdr Core’s `hc` available to remote SSH commands. Select a terminal by its display name and numeric address (such as `t7`). w11term opens a full interactive viewer over that SSH login. The program receives input and resize events, and continues running after the viewer detaches. This explicit command runs `hc connect`, which can start the account’s Core daemon. Session discovery and attachment use existing terminal records. The adapter targets Core’s generation 1 framed protocol from v0.1.21. [Herdr viewer protocol](https://github.com/herdrdev/herdr-core/blob/v0.1.21/schema/README.md#attach-streams)

Reopen a saved workspace or choose **Remote session → Reattach to remote session** to request the same program again. w11term checks the SSH account and host key, persistent Core ID, terminal address and creation time, and the Linux process’s PID, boot ID and start ticks. Verification requires readable `/proc` entries and SSH exec channels. After a disconnect or a kept terminal replacing its program, the viewer shows its state. Choose an existing terminal again when its identity changes.

Herdr can repaint the screen during attachment or after a slow viewer catches up. For the lifetime of these panes, remote clipboard changes, bells, named sounds and notifications stay disabled. Local copy/paste, terminal input, rendering and SFTP remain available.

## Durable Linux shells

In the SSH profile editor, enable **Keep the remote shell running with zmx (Linux)** to let a remote PTY survive a network drop or a w11term restart. The Linux account needs a compatible zmx 1.x executable on its SSH command path and permission to run SSH exec requests. The tested version is 1.0.1. Save the workspace after the first attachment to reopen the same shell and layout together.

From a fresh workspace, choose **More → Find remote sessions → Find sessions on _profile_**. w11term authenticates to that SSH account and lists its w11term-managed zmx shells and existing tmux sessions on the account’s default socket. Each row shows the session name, backend and attached client count, plus the window count for tmux or the available command and directory for zmx. Select a row to attach in the browser tab. **Refresh** checks the host again. Discovery works with an ordinary saved SSH shell profile and keeps its usual launch behavior. You can save the selected session in a workspace or save an unselected browser to list that host again on reopen.

Type in the session search box to match a name, session ID, backend, command or directory across all pages. The search also works in **Browse Herdr terminals**. Matching ignores case and treats punctuation literally, so a path such as `/srv/build[2]` can be entered directly. With one matching result, press **Enter** in the search box to attach. With several matches, choose a row. Typing filters the current listing locally. **Refresh** queries the host again and keeps your search.

tmux attaches in its normal full-screen UI on an SSH PTY. Its prefix keys, windows and panes work as they do in a plain SSH attachment. Detach in tmux or use **Remote session → Detach from remote session**. Reattach and workspace restore use the same session ID even after a rename. If the tmux server restarts or the session ends, choose **Remote session → Find remote sessions again** to return to the list. Creation and termination of tmux sessions stay with the remote account. Stock tmux 3.4 on Linux is exercised in CI.

Use **More → Panes → Remote session** to detach, reattach, or end the remote session. Ending requires confirmation. w11term checks the SSH destination, account, host fingerprint and remote session generation before accepting input. If recovery fails, the pane shows the outcome and offers an explicit choice to start a new durable shell or a plain SSH shell. Ordinary game profiles that accept only shell requests can keep using direct SSH.

The Linux trial restored Vim's unsaved buffer, editing mode and current grid. An application using a static kitty image needs to redraw it after reattachment. [Linux trial and reproduction steps](docs/durable-linux-trial.md)

## Player profiles

Dropped transports reconnect in the same tab, with 1/2/4/8/10-second backoff. In an SSH profile, use **SSH retry window (seconds)** to set the total time for automatic retries, including login and shell setup. The default is 300 seconds. The retry window resets after the new shell stays connected for 30 seconds. An open pane keeps the policy it started with. Clear **Reconnect after a network interruption** to disable automatic retry. In JSON, set `"reconnect_window_seconds": 45` inside `player`.

Permanent authentication/trust errors and normal remote process/channel closure stop retrying. Input while disconnected is discarded; raw control writes and SFTP operations fail explicitly instead of being replayed. The latest grid is applied to the new PTY, terminal protocol state resets, and primary scrollback remains. The client returns to the server's prompt; it never sends `BOARD` or replays game commands automatically. Every SSH shell requests `COLORTERM=truecolor`.

In an SSH profile, set **SSH keepalive interval (seconds)** and **Unanswered keepalives allowed** to control when an unresponsive link is dropped. The defaults are 5 seconds and two unanswered probes. An interval of zero turns off the probes. Open a new connection to use an edited policy. In JSON, use `"keepalive": {"interval_seconds": 15, "max_unanswered": 3}`. OpenSSH import reads `ServerAliveInterval` and positive `ServerAliveCountMax` for the destination and each jump host. When just one directive is present, the other uses its OpenSSH default. [OpenSSH server-alive settings](https://man.openbsd.org/ssh_config#ServerAliveInterval)

Set **SSH connection timeout (seconds)** to limit how long a new connection waits for an endpoint. The same budget covers opening the TCP connection or jump channel and completing the SSH key exchange. The timer pauses for first-time host key approval and ends before user authentication. Leave the field blank to use w11term's default limits of 15 seconds to open the connection and 300 seconds for the handshake. In JSON, set `"connect_timeout_seconds": 20` on the destination or an entry under `proxy_jump`. OpenSSH import applies each endpoint's `ConnectTimeout`. A value of zero or `none` uses w11term's default limits. [OpenSSH connection timeout](https://man.openbsd.org/ssh_config#ConnectTimeout)

Applications that negotiate the [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) can distinguish keypad Enter and Num Lock-off navigation from the main Enter and navigation cluster. With disambiguation enabled, printable keypad digits and operators still type the text produced by the active Windows layout. Report-all mode identifies the physical keypad function, including a localized decimal key, and reports associated text when requested.

Applications that request alternate keys can also recognize shortcuts by their physical position on a standard US PC keyboard. For example, Ctrl+с on a Russian layout can report both Cyrillic `с` and the physical `c` key. The application chooses which identity to bind. Ordinary text follows the active Windows layout. [Kitty alternate key reporting](https://sw.kovidgoyal.net/kitty/keyboard-protocol/#key-codes)

Programs using xterm's `modifyOtherKeys` can select levels 0–3 with `CSI > 4 ; level m`. Level 2 distinguishes Ctrl+I from Tab, and level 3 also reports unmodified keys. With the default format, Ctrl+I at level 2 sends `CSI 27 ; 5 ; 105 ~`. Select `CSI > 4 ; 1 f` to receive `CSI 105 ; 5 u` instead. Query the active level and format with `CSI ? 4 m` and `CSI ? 4 g`, or view them in the Terminal inspector. Function, cursor and physical keypad keys retain their existing encodings. Kitty negotiation takes priority while active. [Xterm keyboard controls](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) · [Xterm modified-key examples](https://www.invisible-island.net/xterm/modified-keys-us-pc105.html)

Game mode suppresses held-key repeats while preserving the initial press and final release. Normal terminal mode sends negotiated kitty repeat event type 2 and release type 3. The More menu toggles game mode and mute for the active saved profile. **F11** or the More menu toggles borderless fullscreen. Normal window size and position are remembered per profile in `%LOCALAPPDATA%\w11term\windows.json`, clamped to the current monitor on restore. Explicit launch `--cols`/`--rows` takes precedence. The status bar displays columns × rows throughout resizing.

Alternate-screen wheel-to-arrow compatibility is disabled by default in game mode.
`player.alternate_scroll` defaults to `null` (follow game mode); set it to `true` or
`false` to override that policy. The connect form and More menu expose this setting.
Application-requested wheel reports still pass through, Shift keeps the local
override, and alternate-screen history remains the remote application's job.
The bundled `play-6789` and `play-6792` profiles explicitly disable wheel-generated
arrow keys.

The wizard generates Ed25519 device keys in process, stores them under `%LOCALAPPDATA%\w11term\keys`, encrypts private bytes with current-Windows-user DPAPI, and restricts the private file ACL to that user and SYSTEM. The machine name becomes the public comment. Only the public line can be copied by the wizard. These protected files are w11term-specific; OpenSSH cannot read them directly. For a generic host, append the copied public line to `~/.ssh/authorized_keys`.

For a Rustmush profile, select the Rustmush checkbox, generate a device key, and sign in with a password. Once at the game command prompt, choose **More → Enroll this device**. It sends `@sshkey/add`, reads a fresh `@sshkey/list`, and changes future profile authentication to the key only after verifying the exact SHA256 fingerprint. The confirmation shows that fingerprint. **Forget this device** revokes that fingerprint, verifies the server listing, and returns the profile to password authentication. The encrypted local key remains available for reenrollment. `account+character` usernames pass through unchanged. Account creation stays in the server's welcome flow. Do not concurrently drive the same pane while enrollment is running.

BEL and names-only OSC 440 sounds support per-profile mute, `system`/`off`/named bells, and verified asset packs fetched over the existing SFTP connection. The complete server contract—including OSC bytes, manifest limits, and Hello fields—is in [Game client protocol v1](docs/game-client-protocol.md). Sprite/font/help assets are cached without installing or executing them. Set `W11TERM_DATA_DIR` to redirect keys, assets, and window placements for portable configuration or isolated tests; appearance settings retain their own override.

Turn on **Settings → Interaction → Show a visual bell** to briefly outline the pane that rings in the foreground window. The outline works with audio muted, leaves focus where it is, and appears at most once per second per pane. Bells from hidden panes still appear in the Attention Inbox. The setting is off by default and is stored as `"visualBell": true` under `interaction`.

Example optional profile settings (all fields may be omitted):

```json
"player": {
  "reconnect": true,
  "game_mode": false,
  "alternate_scroll": null,
  "rustmush": false,
  "mute": false,
  "bell": "system",
  "sync_assets": null,
  "remote_sounds": null,
  "device_key": null
}
```

```powershell
cargo run -- --replay C:\path\to\capture.vt
cargo run -- --replay-timed C:\path\to\cockpit.vt
cargo run -- --benchmark
cargo run -- --demo --smoke-seconds 5
```

You can open a raw VT file containing inline images directly with `--replay`.
The GUI uses the pane's measured cell grid for captures without recording metadata.
Use `--cols` and `--rows` to choose an initial grid for a particular capture.
For a restored background replay pane with unknown dimensions, showing the pane
or sizing it with `w11term-cli resize` starts playback.

To capture an animation or a resize bug, choose **Start timed recording…** in
the command palette or the tab menu’s **Recordings** submenu. Choose **Stop
recording** on that pane to finish. **Open recording…** opens the selected
`.vt` in a new tab and uses its timing index when available. You can keep
working in your original SSH or PowerShell session during playback. To inspect
a fast animation, choose **Open recording paused…**, then **Next recorded
update** to advance one timestamp at a time. **Pause replay** and **Resume
replay** are available on an active timed recording.
[Recording files and CLI commands](CONTROL.md#vt-recording)

The benchmark measures **only VT parsing and scrollback**. It excludes snapshot creation, rendering, SSH, and input-to-screen latency. Debug-build results are not performance claims. Build with `cargo build --release` for performance work. The release GUI executable does not attach a console.

## Controls

Open **Tab menu → Clipboard** for **Paste**, **Paste raw**, **Copy selection**, and **Select all terminal text**. Press **Ctrl+Shift+A** to select the focused pane’s retained text, including lines above the viewport. Press **Ctrl+Shift+C** to copy it as plain text, or **Esc** to cancel. Selecting leaves the clipboard untouched even when copy-on-select is on. In a full-screen application using the alternate screen, the selection covers that application’s text. For output larger than the 1 MiB clipboard limit, use **Panes → Save scrollback as text…**.

Use **Copy pane as image** or **Save pane image…** under **Tab menu → Panes** or in the command palette. Both capture the current terminal grid, including graphics and any scrollback currently in view. Copy places a bitmap on the Windows clipboard and Save writes a PNG. The capture excludes selection highlights and surrounding window controls.

Choose **Copy visible pane text** to copy the same viewport as plain text without selecting it first. It joins soft-wrapped rows, trims trailing space on hard lines, and omits unused rows at the bottom.

**Ctrl+Shift+X** or **More → Panes → Keyboard copy mode** lets you select text in the focused pane while its session continues running. Move with the arrow keys or **h/j/k/l**, page with **Page Up/Down**, use **Home/End** or **0/$** for the edges of a row, and **g/G** or **Ctrl+Home/End** for the ends of retained history. Press **v** for character selection or **Shift+V** for whole rows, then **y** or **Enter** to copy plain text. Soft-wrapped rows join in character and line selections. **Esc** cancels and restores your previous viewport. Inside an alternate screen the current grid is available. Remap the activation shortcut under **Settings → Keyboard**.

To copy columns from a table, move to one corner, press **Ctrl+V** or **r** in copy mode, move to the opposite corner, then press **y** or **Enter**. A rectangular selection copies each displayed row on its own line, including across a soft wrap. **Settings → Remove trailing whitespace in block selections** controls whether short rows are trimmed or padded to the selected width. Press the same selection key again to clear the selection, or switch between character, line and rectangular selection while keeping the original anchor.

In copy mode, **w** or **Ctrl+Right** moves to the next word start, **b** or **Ctrl+Left** moves to the previous word start, and **e** moves to the next word end. To copy a path, move to its first character, press **v**, then **e** and **y**. The **Word selection delimiters** setting controls where punctuation splits a word. For example, removing `/` from the delimiters lets you select `/srv/game/zone` as one word. Motions skip whitespace and continue through soft wraps and retained scrollback.

**Ctrl+Shift+H** or **More → Panes → Keyboard hints** freezes the focused viewport and labels its URLs and file paths. Type the letters shown beside a target. **Enter** opens a URL or copies a path, **C** copies either, **Backspace** edits the label, and **Esc** returns to the live pane. URLs use the same link rules as Ctrl+click, including OSC 8 hyperlinks. Paths can include quoted spaces, soft wraps, and compiler-style line and column suffixes.

In a PowerShell or named WSL pane, press **R** to select a file or folder in File Explorer. For example, when a build prints `src/main.rs:12:8`, select its hint and press **R** to highlight `main.rs`. WSL translates the path in the selected distribution, including files on Windows drives mounted there. In an SSH pane, **R** reveals the selected Unix path in that pane’s SFTP drawer. Relative paths use the directory the shell reported when hints opened. Absolute paths work without a directory report. [WSL path translation](https://learn.microsoft.com/en-us/windows/dev-environment/wsl-interop#path-translation)

Use **Settings → Hint patterns** to add, edit or remove named regular expressions. For example, an **Issue** pattern with `\bOPS-[0-9]+\b` labels issue numbers such as `OPS-104`. Select its label and press **Enter** or **C** to copy exactly the matched text. When the entire match is a supported URL, **O** opens it. When it is a recognized path, **R** reveals it in File Explorer or SFTP according to the pane.

Patterns match across soft wraps within a visible logical line. Explicit OSC 8 links take priority, followed by custom patterns in saved order, then automatic links and paths. Patterns use Rust regex syntax and must consume at least one character. You can save up to 16 patterns. Each invocation considers up to 512 custom matches across 256 KiB of viewport text and skips logical lines longer than 16 KiB.

For focus-independent automation, raw SSH input, screen text, PNG capture, and launch geometry, see [External control](CONTROL.md).

| Input | Behavior |
| --- | --- |
| Ctrl+Tab / Ctrl+Shift+Tab | Open/cycle the tab switcher; release Ctrl to commit; Escape cancels |
| Ctrl+Shift+T | Open another tab using the startup connection or terminal lab |
| Ctrl+Shift+W | Close the selected tab |
| Ctrl+Alt+1 through Ctrl+Alt+9 | Select a tab by its position |
| Drag / double-click / triple-click | Select text / a word / a line; selection is copied automatically |
| Alt+drag | Rectangular selection |
| Right-click | Paste; with auto-copy disabled, copy an existing selection first |
| Ctrl+V / Ctrl+Shift+V / Shift+Insert | Normal paste, using negotiated bracketed-paste mode |
| Ctrl+Alt+V / Clipboard → Paste raw | UTF-8 clipboard bytes, without bracket markers, trimming, newline conversion, or key encoding |
| Ctrl+Shift+A | Select all retained text in the focused pane |
| Ctrl+Shift+C / Ctrl+Insert | Copy the selection as plain Unicode text |
| Ctrl+C with a selection | Copy; without a selection, send Ctrl+C to the remote program |
| Ctrl++ / Ctrl+- / Ctrl+0 | Increase / decrease / reset font size (16 DIPs) |
| Ctrl+mouse wheel | Zoom the font |
| Ctrl+click a URL | Open an HTTP(S) or mail link; links underline on hover |
| Shift+PageUp / Shift+PageDown | Browse scrollback by one visible page |
| Ctrl+Shift+Home / Ctrl+Shift+End | Oldest retained history / return to the live screen |
| Ctrl+Shift+Up / Ctrl+Shift+Down | Previous / next OSC 133 prompt |
| Ctrl+Shift+F | Find text or a regular expression in the current pane’s scrollback |
| Ctrl+Shift+K | Search terminal actions and launch profiles |
| Ctrl+Shift+P | Find a pane by title, profile or SSH destination |
| Enter in find / F3; Shift+Enter / Shift+F3 | Next / previous find result, wrapping at either end |
| Escape in find | Close find and restore terminal keyboard input |
| Mouse wheel | Scrollback using Windows' wheel-line/page preference, including accumulated fractional deltas; remote wheel events when mouse reporting is enabled |
| F11 | Toggle borderless fullscreen |
| F5 in the local lab | Reload the protocol demo |
| Ctrl+L in the local lab | Clear the display |

Hold Shift to select or right-click paste when a remote TUI enables mouse reporting. Selection retains scrolled rows, joins soft-wrapped lines, and copies graphemes without font/color formatting. Native pointer capture lets dragging continue outside the canvas, with automatic scroll at its top/bottom edges.

Use **Settings → Word selection delimiters…** to choose which punctuation separates words for double-click selection and screen-reader word navigation. For example, removing `/` keeps `/usr/bin` together. Leave the field empty to split on whitespace alone. **Save** updates existing panes. **Restore default delimiters** puts the original characters back in the editor so you can review them before saving.

Find searches the current pane’s retained history and visible screen. It starts with a case-insensitive literal search using Unicode lowercase matching. Turn on **Regex** for patterns such as `error\s+[0-9]+`, or **Match case** to distinguish uppercase and lowercase. Matches can span a line wrapped by the terminal and stop at a hard newline. Enter and Shift+Enter move between highlighted matches. Escape closes Find and restores terminal input.

Select a filename or error code and open **Find in scrollback** to search for it. Mouse, keyboard copy mode, and accessibility selections work. Find uses the selected text literally, including punctuation, and keeps the pane’s **Match case** setting. A single line can wrap across several displayed rows. With no eligible selection, Find restores the pane’s previous query and options. **Find next** and **Find previous** continue the previous search.

Under **Settings → Keyboard**, you can change or clear **Find in scrollback**, **Find next**, and **Find previous**. For example, clearing **Find next** gives the default F3 key back to the terminal application. The command palette shows the current assignments. Enter and Shift+Enter also navigate while the Find box is open.

Queries accept up to 4,096 UTF-8 bytes. The match count refreshes as output arrives, and `+` indicates more than 10,000 matches. If a pattern is invalid, the Find bar shows the reason as you edit. Regex search skips logical lines larger than 64 KiB and shows when the results are incomplete. Ordinary scrolling stays anchored to retained rows as new output arrives.

When a pane has scrollback, drag the scrollbar at its right edge to browse history, or click the track above or below the thumb to move one page. While Find is open, marks on the scrollbar show matching rows throughout the retained history. The selected match has its own color and follows Find Next and Previous. Pink marks beside the track locate captured output from commands that exited with a nonzero status. Click one to jump to that output, or use **Previous failed command** and **Next failed command** in the command palette. The marks are on by default and can be hidden under **Settings → Interaction**. Drag to the bottom to follow new output. The scrollbar uses the pane's existing padding and preserves its column count. Assistive technology can use the focused terminal's UI Automation Scroll pattern to read the position, page through history or jump to a percentage.

Use **Toggle scrollback bookmark** in the command palette to mark the current cursor row or, while viewing history, the top visible row. Click a blue mark at the outer edge of the scrollbar, or use **Previous bookmark** and **Next bookmark**. Run the toggle again at the row to remove the bookmark. Each pane holds up to 256 marks and keeps them with retained text as the window resizes. They expire when their rows are discarded or when the terminal connection resets.

Choose **Find scrollback bookmark…** to search the focused pane’s bookmarks by their line previews. The picker shows the newest marks first and omits concealed terminal text from previews. Press Enter to jump to the highlighted result or Esc to return to the pane at its current scroll position.

The alternate screen has no local scrollback. A TUI receives wheel reports when it enables mouse reporting. For other TUIs, **Alternate-screen wheel keys** enables the arrow-key fallback used by pagers. It defaults to on for standard profiles and off in game mode. Shift+wheel scrolls local primary-screen history and has no effect on the alternate screen. Find on the alternate screen searches its current contents. Returning to the primary screen restores its history position.

When you scroll into the middle of a command’s retained output, its command line appears at the top of the pane. Select it to jump to the start of the output. **Settings → Interaction → Show command context in scrollback** controls this behavior and is on by default.

**OSC 133 semantic prompts.** With shell integration enabled, Ctrl+Shift+Up/Down jumps between prompt starts. **More → Panes → Command history** opens the focused pane’s completed commands, newest first. Type in **Search command history** to match any part of the captured command line, including text beyond its shortened list label. The search ignores case and treats punctuation literally. Check **Include command output** to also search the full retained output. When an output match is found, the preview starts near the first match. Use **Command status** to narrow the results to failed commands, successful commands, or commands whose shell omitted the exit status. Select a command to preview its output and exit status. **Copy command** copies the captured command line. **Copy output** copies its full plain text, joining soft wraps and preserving hard newlines. **Refresh history** selects the latest matching command, and **Return to pane** takes you back to the originating shell. **Copy last command output** remains available directly in the Panes menu.

Choose **Save output as text…** to write the selected command’s captured output to a UTF-8 file. It uses the same plain text as **Copy output**, including blank lines, and remains available for a read-only source pane.

To compare two captures in the same window, select the first and choose **Use output as reference**. Select a second capture in any pane’s history and choose **Compare with reference**. The view shows both exit statuses and the changed lines with their original line numbers. Choose **Save full differences…** to export all changed lines while both captures are retained.

Choose **Show output in pane** to return to the source pane and scroll to where the selected command’s output began. The location follows the text when the pane is resized. If its start has left scrollback, history stays open and you can still copy or save the captured output.

When the shell reports a working directory, history shows the path captured at the command’s start. **Open command directory** opens an SSH path in the original pane’s file drawer while its connection is available, or opens a supported local path in File Explorer. A later directory change leaves the saved path intact.

Command history shows the selected capture’s timestamps in UTC and the elapsed time observed between shell markers. Choose **Slowest first** under **Command order** to compare commands with measured durations. Captures without a start marker show their finish time and sort after timed commands.

Choose **Insert command** to return to the source pane and paste the selected command for editing. Press Enter in the shell when you are ready to submit it. The action requires a live, writable shell on the same connection. Commands containing line breaks or tabs require bracketed paste.

Scripts can read the same OSC 133 history with `w11term-cli list-commands` and retrieve one command’s output with `get-command`. [Command history in the control reference](CONTROL.md#completed-shell-commands)

Choose **Panes → Save scrollback as text…** to keep the output of a completed build or SSH session. The Save dialog writes a snapshot of the originating pane, including text that has scrolled out of view. The UTF-8 file joins soft wraps and preserves hard line breaks. On the alternate screen it contains the current application grid. Scripts can save the same text with `w11term-cli save-text --pane ID PATH`. [Control reference](CONTROL.md)

Choose **Panes → Clear scrollback** to discard the rows above the current screen in that pane and return to its live output. The current screen, cursor and shell stay in place. The action is available when the primary screen has history, including in a read-only pane or after a shell exits. Find and text exports immediately use the remaining rows. You can assign the action a shortcut under **Settings → Keyboard**.

When a direct SSH shell or local program has exited, choose **Panes → Restart exited pane** or find the action in the command palette. It opens a fresh login or local process in the same split, with the same pane ID and retained scrollback. The action becomes available after the previous transport finishes, including any automatic reconnect attempts. Durable sessions use their own reattach actions. Saved remote startup commands require an explicit new launch.

While a direct SSH pane is connecting or retrying, choose **Panes → Stop SSH attempt** or find the same action in the command palette. The pane and its scrollback stay available for **Restart exited pane**. For a background pane, scripts can read its attempt token from `w11term-cli list` and pass it to `stop-ssh-attempt`.

When a direct SSH pane is waiting between retries, choose **Panes → Retry SSH now** or find it in the command palette. It starts the next retry early in the same pane using the time left in the existing retry window. Scripts can use `w11term-cli retry-ssh-now` with the token from `list` or `events`.

For a connected direct SSH pane, choose **Panes → Disconnect SSH shell…** or find it in the command palette. Confirm to end that shell and any file transfers in the pane. The pane and scrollback stay open, and **Restart exited pane** opens a fresh shell when you want to reconnect. Scripts can disconnect an exact connection generation with `w11term-cli disconnect-ssh-shell`.

Choose **Panes → Notify when command finishes** before starting a build or while it is running, then switch panes or use another application. Its exit status appears once in the **Attention inbox**. Selecting the entry takes you back to the originating pane. Choose **View command** beside the alert to open that command’s captured output and exit status while it is still in the pane’s history. If another window has focus, the owning window’s taskbar button flashes. Choose **Cancel command notification** to disarm it.

Turn on **Panes → Notify for long commands** to receive an alert whenever a command runs for at least 10 seconds in that pane. It starts off in each new pane and follows that pane through reconnects. An explicitly watched command can still alert sooner. A command that qualifies for both alerts creates one inbox entry.

Both alerts work in direct SSH and local ConPTY sessions with shell integration that emits **OSC 133 C** when execution starts. **OSC 133 D** reports completion and an optional exit code. A fresh prompt can also report completion with an unknown exit code. Local PowerShell profiles can supply these markers with the checkbox above. Disconnecting or resetting the terminal clears the one-shot watch and the current timing. Durable viewers and VT replays can repeat old output and do not offer these actions. [OSC 133 shell integration markers](https://learn.microsoft.com/en-us/windows/terminal/tutorials/shell-integration)

History uses the original command line when the shell reports it as OSC 133 C’s `cmdline_url` field. This preserves multiline input, indentation and quoting even when the prompt reprints the command. Current Fish and the bundled Zsh integration can supply it. If the field is absent, invalid, or exceeds 4,096 UTF-8 bytes, history uses visible text between the B and C markers when available. Output runs from C to D or the next prompt. The pane footer shows an explicit numeric exit status. Each pane retains up to 64 commands and 4 MiB of captured text in memory. A command whose output exceeded 1 MiB or left scrollback before completion shows the reason in its preview. Alternate-screen applications leave shell history intact. A terminal reset or reconnect clears it. [Kitty command-line reporting](https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers)

Choose **Find command history…** from More or the command palette to search retained SSH and local shell commands across the current window. Search ignores case and matches the full command line, captured working directory, pane name, profile, or SSH address. Use **Command status** to show failures, successes, or commands with an unknown exit status. Results default to **Newest first**. **Slowest first** shows elapsed times and puts captures without timing last. Check **Include command output** to match literal text in retained output and open the preview near the match. **F7** cycles status, **F8** changes order, and **F9** toggles output search. Use the arrow keys and **Enter** to open a result. **Back to search results** restores your query, filters, and selected capture if it is still retained. **Esc** closes the picker and returns to the tab or pane where you started if it is still open. You can assign a shortcut to open the picker under **Settings → Keyboard**.

Choose **Save results as JSON…** or **Ctrl+S** to export the query, filters, and metadata for all result pages in order. The report records which matches came from output and counts results that expired before saving. Use **Save output as text…** in an individual capture to save its output.

The Settings tab exposes auto-copy, block-selection trimming, paste trimming, grid snapping, tab-switcher mode (tab order/MRU/disabled), Ctrl+scroll zoom, URL detection, and remote OSC 52 writes. These switches default to enabled, and the tab switcher defaults to tab order. Copies contain plain text. By default, normal paste removes trailing whitespace at the end of the entire payload when the application receives ordinary input. When the application requests bracketed paste, trailing spaces and newlines are preserved. Raw paste always preserves the original UTF-8 bytes. The resize border snaps to cell boundaries during manual dragging. [Windows Terminal interaction settings](https://learn.microsoft.com/en-us/windows/terminal/customize-settings/interaction)

Under **Settings → Multiline paste warning**, **Automatic** asks before sending multiple lines to an ordinary prompt or using raw paste. It lets a bracketed paste through without the multiline prompt. **Always** asks for multiline text in either mode, and **Never** turns off that prompt. **Warn about large pastes** is an independent switch, enabled by default, for clipboard text over 5,120 UTF-16 code units. A paste that meets both conditions opens one dialog. Confirmation sends the clipboard text captured when you invoked paste to the original pane and connection. Cancel sends nothing. These settings apply to keyboard, menu, and right-click clipboard paste. Scripts using `w11term-cli send-text` keep their existing byte and paste behavior.

Remote programs can set the local Windows clipboard with **OSC 52**, without a forwarded control pipe. For example, on the SSH host:

```sh
printf '\033]52;c;%s\a' "$(printf '%s' 'Copied from the remote host 🌍' | base64 | tr -d '\n')"
```

OSC 52 writes are plain text and are capped at 1 MiB and four remote writes per second across the process. Clipboard-read queries receive no response. Background sessions can write by default. Turn on **Limit OSC 52 to the foreground pane** in Settings to accept writes only from the selected pane in the foreground w11term window. The app checks again when it writes to the clipboard. Turn off **Allow remote clipboard copy (OSC 52)** to disable remote writes entirely. Normal UI paste remains distinct from the control CLI's `send-text --no-paste`, which always sends raw bytes. For bytes that cannot be represented as Windows clipboard text, use `--file`, `--stdin`, or `--base64` through the CLI.

The primary device attributes reply (DA1) includes attribute 52 when the pane supports OSC 52 writes. Foreground-only panes advertise support and enforce focus when a write arrives. Disabling remote writes removes attribute 52 on the next query. [Windows Terminal's DA1 extension](https://github.com/microsoft/terminal/pull/19034)

## Architecture

- `terminal.rs`: pinned WezTerm VT engine adapter, keyboard encoding, immutable screen snapshots, and protocol tests. Kitty's full input encoder comes from `wezterm-input-types`; `Terminal::key_down` alone does not implement that protocol.
- `session.rs`: a dedicated terminal thread with its own Tokio runtime. Output has a bounded queue and bounded batches; immutable snapshots replace the previous frame rather than accumulating render work. Replies and keystrokes return to SSH.
- `ssh/transport.rs`: embedded `russh` transport, host verification, private authentication prompts, automatic reconnection, remote PTY, shell, and resize messages.
- `keys.rs` / `prompts.rs` / `enroll.rs`: DPAPI-protected Ed25519 device keys, private native challenges, and verified Rustmush key enrollment/revocation.
- `assets.rs` / `sound.rs`: bounded, hashed SFTP asset synchronization and names-only sound playback outside the parser/UI threads.
- `events.rs` / `recorder.rs`: cursor-based event journals and output-only VT captures with replay geometry.
- `window_state.rs`: per-profile native placement/grid persistence and reversible fullscreen.
- `sftp.rs`: reusable SFTP v3 subsystem on each pane's existing SSH connection; streaming get/put, directory and file handles, metadata, links, and advertised OpenSSH extensions through `w11term-cli sftp`. See [the control API](CONTROL.md) for commands and JSON schemas.
- `options.rs` / `profiles.rs`: CLI, saved profiles, relative key paths, and atomic profile creation.
- `tabs.rs`: stable tab identities, selection, closing, and reorder validation.
- `renderer.rs`: DirectWrite layout cache and Direct2D/Direct3D rendering. Whole images are cached on the GPU and their per-cell source regions are drawn from that cache. Repainting is demand-driven.
- `native_drawing.rs`: a small, documented COM ABI adapter between the Windows binding versions currently used by Canvas and the `windows` crate. It adds color-glyph rendering and source-rectangle image drawing.
- `appearance.rs`: installed-font filtering and per-user font/ligature preferences.
- `interaction.rs` / `clipboard.rs`: interaction preferences, stable-row selection, safe link detection, and a focus-independent plain-text Windows clipboard service with OSC 52 integration.
- `search.rs`: bounded literal matching across soft wraps, Unicode cell-coordinate mapping, match navigation, and OSC 133 prompt navigation. Search runs on the terminal worker and passes only immutable summaries and visible highlights to the UI.
- `app.rs`: native title bar, tab strip, profile flyout/form, appearance Settings, and a shared active-terminal renderer. Frame subscriptions are cancelled and rearmed when the active tab changes.
- `input.rs`: a UI-thread-local input bridge because Reactor 0.100 does not expose the necessary raw keyboard events. It routes terminal input to the active session, releases held keys on the old session before switching, and removes its terminal target for native form/Settings pages. Native popup windows are excluded from interception.

## Current boundaries

Synchronized output (`CSI ? 2026 h` / `CSI ? 2026 l`) commits text and image pixels
together. Parsing, replies and input continue during a hold; a missing end marker
expires after 150 ms. `get-text`, screenshots and `wait-for-text` observe committed
frames. Capture queries and no-op input do not publish another frame. Oversized or
invalid image resources are skipped without disabling a tab, and visible textures
are retained when the GPU budget is full.

The local control pipe uses a protected user/SYSTEM DACL and an explicit mandatory
integrity label matching its process. Lower-integrity clients are rejected; the CLI
also verifies the connected server's PID and user. Run the CLI at the same integrity
level as the GUI. The opt-in elevated regression is
`cargo test --lib native_high_server_refuses_medium_integrity_client -- --ignored`
from an administrator terminal; ordinary tests exercise medium/low isolation.

This is a runnable foundation, not complete terminal conformance or a kitty/Ghostty performance result.

- ANSI/DEC behavior, primary/alternate screens, scrollback/reflow, 256/true color, wide and combining text, basic mouse reporting, bracketed paste, and negotiated kitty key encoding are connected.
- SSH apps using SGR pixel mouse mode (`CSI ? 1016 h`) receive Kitty-compatible pointer-leave reports when the pointer leaves their pane. This lets a TUI clear its hover state when the pointer crosses into a split or moves onto window controls. Captured mouse gestures finish in their original pane. [Kitty mouse-leave protocol](https://sw.kovidgoyal.net/kitty/misc-protocol/#reporting-when-the-mouse-leaves-the-window)
- SSH apps can set, push, pop and query mouse pointer shapes with Kitty `OSC 22`, including the text, hand, crosshair, wait and resize cursors. The mouse uses the hovered pane's shape with independent primary and alternate screen stacks. Windows uses native cursors for the supported names, including several xterm aliases. Capability queries report `0` for `alias`, `copy`, `vertical-text`, `zoom-in` and `zoom-out`. [Kitty pointer protocol](https://sw.kovidgoyal.net/kitty/pointer-shapes/)
- SSH games and TUIs can subscribe to pane size changes with `CSI ? 2048 h`. Each enable immediately returns `CSI 48 ; rows ; columns ; height_px ; width_px t`. Further reports follow changes to the text area, including a pixel-size change with the same cell grid. The dimensions exclude the tab bar, padding and status line. `CSI ? 2048 l` stops reports for that pane. [In-band resize protocol](https://gist.github.com/rockorager/e695fb2924d36b2bcf1fff4a3704bd83)
- iTerm2’s `OSC 1337;ReportCellSize` reports cell dimensions in Windows device-independent pixels and includes the monitor scale, such as `1.25` at 125%. Applications can multiply by the reported scale to recover physical pixels, or query them directly with `CSI 16 t`. [iTerm2 cell-size report](https://iterm2.com/3.6/documentation-escape-codes.html#report-cell-size)
- When an SSH editor saves the shell’s title on entry and restores it on exit, the pane picker and `w11term-cli get-text` show the restored title. Scripts can follow window title changes with `w11term-cli events`. w11term supports xterm’s icon and window title stack, including its ten directly addressable slots and the stack-depth query. [xterm title operations](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) · [ncurses alternate-screen sequences](https://invisible-island.net/ncurses/terminfo.src.html#tic-xterm+alt+title)
- The terminal displays kitty, sixel and iTerm images. Kitty supports timed or client-selected animation frames, Unicode placeholders and relative placements. `kitten icat --clear` clears visible images, and `kitten icat --clear-all` also clears named images from scrollback on the active screen. The [graphics audit](docs/graphics-audit.md) documents reference fixtures, GPU cache eviction, transfer budgets and protocol coverage.
- Kitty's `OSC 66;w=…;text` assigns a run one to seven grid columns at normal scale. Set `s=2` through `s=7` to scale the width and height together. Set `n=1:d=2` to draw half-size ink within the allocated cells, with `v=1` for a subscript or `v=2:h=2` for centering. The cursor-position probe reports width and scale support. In primary scrollback, text reflows around scaled rectangles on resize. Rectangles too large for the new grid are discarded. [Kitty text sizing](https://sw.kovidgoyal.net/kitty/text-sizing-protocol/)
- Shell integrations can use Kitty's [`CSI 22 J` screen archive](https://sw.kovidgoyal.net/kitty/misc-protocol/#an-escape-code-to-move-the-contents-of-the-screen-into-the-scrollback) to keep the current screen's text and images in scrollback while leaving the cursor in place.
- Kitty's [`CSI Ps + T` reverse scroll](https://github.com/kovidgoyal/kitty/blob/master/kitty/screen.c#L2677-L2709) pulls recent history back into a full-width primary scroll region that starts at the top. Omit `Ps` to restore one row. If the request exceeds the available history, the remaining scroll uses blank rows.
- Kitty indexed-color images use the pane's current palette. Charts and other static images sent as `f=8` or `f=16` follow OSC color changes and Windows theme changes without a retransmission. `f=16` also carries per-pixel opacity. [Kitty indexed color](https://sw.kovidgoyal.net/kitty/graphics-protocol/#indexed-color-data)
- DirectWrite shapes text runs with matching attributes and supports italic, color emoji and five underline styles with independent decoration color. Bidirectional ordering uses the terminal's row direction with a left-to-right default. Selection, search highlights, URL hit testing and cursor placement share the displayed cell map. Copy and control-pipe text retain logical order. Each physical row resolves independently, including soft-wrapped rows. Scaled Kitty text keeps its columns across all rows it spans, with bidirectional ordering applied separately on either side. The replay fixture exercises mixed Hebrew and digits, Arabic joining, a Devanagari conjunct, italic and underline styles. [Replay fixture](tests/fixtures/complex-text.vt)
- Applications can save and restore private terminal settings with XTSAVE and XTRESTORE, including bracketed paste, cursor visibility, mouse reporting, and the active screen buffer. XTPUSHSGR and XTPOPSGR save and restore text styling in nested scopes. XTREPORTSGR reports the styling shared by a rectangle on the active screen. [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- Applications can temporarily change colors and then restore them with the xterm palette stack. `CSI # P` saves the current overrides and `CSI # Q` restores them. The stack supports ten numbered slots and the `CSI # R` position report. Inherited colors follow the current pane scheme, including a Windows light or dark theme change. Restoring the palette also refreshes existing indexed kitty images. [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) · [XTerm palette stack implementation](https://github.com/ThomasDickey/xterm-snapshots/blob/master/misc.c#L7938-L8015)
- `OSC 17` and `OSC 19` set the selection background and foreground for a pane. Reset either color with `OSC 117` or `OSC 119`. Selection colors apply to the live mouse or keyboard selection, and copied text stays plain. [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- Kitty's `OSC 21` can set, reset, or query up to the full 256-color palette in one message, along with the pane's foreground, background, cursor, selection and visual bell colors. Empty cursor colors follow pane foreground and background, and empty selection or bell colors use the normal UI effects. Transparency slots report as unsupported in the opaque Windows surface. `OSC 30001` and `OSC 30101` share the xterm palette stack. [Kitty color control](https://sw.kovidgoyal.net/kitty/color-stack/)

- DEC single-width, double-width, and double-height line renditions work in live panes and PNG captures. Doubled rows use half the normal columns for text, cursor addressing, and selection.
- Applications can mark a label or field as protected with DECSCA and use DECSEL, DECSED, or DECSERA to erase surrounding text. Selective erase preserves protected graphemes and the existing cell and line renditions. The protection flag follows a cell through scrolling and reflow. [VT510 Programmer Information](https://vt100.net/mirror/mds-199909/cd3/term/vt510rmb.pdf)
- Applications can fill a screen rectangle with DECFRA or clear it with DECERA. Fill uses the active character set and rendition. Erase clears the cell rendition and protection. Both keep the cursor and line rendition. [VT420 Programmer Reference Manual](https://vt100.net/mirror/mds-199909/cd3/term/vt420rm2.pdf)
- Applications can copy a text rectangle within the active screen with DECCRA. Overlapping copies preserve the original source text and cell styling. The destination keeps its line rendition, and the cursor and Kitty image placements stay in place. [VT420 Programmer Reference Manual](https://vt100.net/mirror/mds-199909/cd3/term/vt420rm2.pdf)
- Applications can change or toggle visual attributes with DECCARA and DECRARA. DECSACE switches between a wrapped range and an exact rectangle. These operations preserve text, color, protection, and image placements. The active pen and cursor position are unchanged. [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- DECIC and DECDC insert or delete columns from the cursor's column through the right scrolling margin. They affect every row in the scrolling region and keep independent Kitty placements in place. [VT420 Programmer Reference Manual](https://vt100.net/mirror/mds-199909/cd3/term/vt420rm2.pdf)
- DEC back and forward index (`ESC 6` and `ESC 9`) move the cursor by one column. At the corresponding horizontal margin inside the scrolling region, they shift text by one column and keep the cursor on the margin. [VT420 Programmer Reference Manual](https://vt100.net/mirror/mds-199909/cd3/term/vt420rm2.pdf)
- ECMA-48 horizontal scrolling (`CSI Ps SP @` and `CSI Ps SP A`) shifts the scrolling region left or right from its left margin. It follows xterm's cursor-in-region rule, keeps the cursor still, and preserves independent Kitty placements. [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) · [XTerm implementation](https://github.com/ThomasDickey/xterm-snapshots/blob/master/util.c#L1075-L1185)
- Applications can overline text and spaces with SGR 53 and clear the overline with SGR 55. The stroke follows the foreground across the full width of each grapheme. Underlines can use their own color at the same time. [ECMA-48, §8.3.117](https://ecma-international.org/publications-and-standards/standards/ecma-48/)
- Applications can swap the pane's default foreground and background with DEC reverse-screen mode (`CSI ? 5 h`), restore them with `CSI ? 5 l`, and query the current mode with `CSI ? 5 $ p`. Explicit RGB and indexed colors keep their values. SGR 7 still swaps the colors of an individual cell. Pane previews and PNG captures use the same colors as the window. [Xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- Applications can select a blinking or steady block, underline, or bar with DECSCUSR, or toggle blinking on the current shape with `CSI ? 12 h` and `CSI ? 12 l`. An editor can query the current style with DECRQSS and restore it on exit. A focused pane fills the application block cursor and draws the character in the cursor's text color. Other panes show the block as an outline, so you can see which split has keyboard focus. A blinking cursor in the focused terminal follows the Windows caret interval. Disabling caret blinking in Windows keeps it steady. Pane previews and control-pipe PNGs use the outlined block throughout its blink cycle. [Cursor-style sequences](https://ghostty.org/docs/vt/csi/decscusr) · [Windows caret timing](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getcaretblinktime)
- CTC (`CSI Ps W`) can set or clear a horizontal tab stop and clear the shared table. DECST8C (`CSI ? 5 W`) restores stops at columns 9, 17, 25 and onward. Forward and backward tabbing use the installed stops and clamp at the line limits. [ECMA-48](https://ecma-international.org/publications-and-standards/standards/ecma-48/) · [DEC VT510 programmer reference](https://vt100.net/mirror/mds-199909/cd3/term/vt510rmb.pdf)
- Applications can save horizontal tab stops with a DEC presentation-state query (`CSI 2 $ w`). w11term replies with the current one-based columns in a DECTABSR report. The application can later restore the saved list with `DCS 2 $ t … ST`. [DEC VT510 programmer reference](https://vt100.net/mirror/mds-199909/cd3/term/vt510rmb.pdf)
- Remote programs can query the current text attributes with the DECRQSS rendition request (`DCS $ q m ST`). The reply describes the style that newly printed text would receive at that point. For example, Vim sets a curly underline, queries the rendition, and resets it in a single write. w11term returns `0;4:3m` for that query. [Vim’s undercurl probe](https://github.com/vim/vim/blob/master/src/term.c#L5184-L5191) · [xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- Windows input methods compose at the focused pane's caret. Committing sends the composed text once. The UI Automation Text and Text2 patterns expose the current viewport, selection and caret to assistive technology. See [IME behavior and manual checks](docs/ime.md).
- When output scrolls a screen reader's text range out of view, its UI Automation client can bring the retained range back to the top or bottom of the focused pane. This preserves the selection and keyboard focus.
- Screen readers can search a word that wraps onto the next row. Line navigation follows the displayed rows, and paragraph navigation follows hard line breaks. A wrapped selection reports a separate bounding rectangle for each visible row it covers.
- Keyboard tests exercise press, repeat, release, game-mode repeat suppression, keypad functions and alternate physical positions for printable keys.
- The [workspace and SSH roadmap](docs/terminal-workspaces-roadmap.md) covers native splits, a graphical SFTP drawer, durable remote sessions, terminal fidelity and accessibility, local PowerShell via ConPTY, and subsequent RustMUSH features.

## Verify

For visible typing-latency comparisons against WezTerm and Windows Terminal, see [the loopback SSH / Typometer setup](bench/README.md).

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

The integration tests start ephemeral **loopback-only** SSH servers with generated test keys. They verify both public-key and keyless authentication, PTY/shell setup, terminal query replies, keyboard roundtrip, and resizing without accessing a user server or credential. Unit tests cover profiles, fragmented UTF-8/VT traffic, alternate-screen restoration, reflow, bracketed paste, and kitty image/key state.

Pull requests run the Rust suite and native Windows tests, including GUI automation, record/replay, ConPTY beside SSH, and a relocated release-package launch. The Rust job reports line coverage for `src/` and uploads LCOV and per-file JSON reports. Native GUI scenarios run in a separate job. Linux jobs check durable PTY recovery and compare graphics against upstream kitty. The Actions run includes the tested Windows x64 ZIP and checksum for download. [CI workflow](.github/workflows/windows-ci.yml)

A merge into `main` reruns these checks at the merged commit. After they pass, GitHub publishes that run's verified ZIP and checksum under a `build-<commit>` release tag. Re-running the workflow preserves an existing published release. [GitHub Releases](https://github.com/conclavio/w11term/releases)

The optional `ui-smoke` feature exercises the real native control lifecycle using only local terminal labs: independent tab buffers, switching, reordering, closing a background tab, Settings, and profile-form validation. It does not save a profile or connect to a server:

```powershell
cargo build --features ui-smoke
$env:W11TERM_UI_SMOKE = '1'
.\target\debug\w11term.exe --demo
Remove-Item Env:W11TERM_UI_SMOKE
```

Reactor 0.100 currently fails native startup when a menu owner is wrapped in a tooltip. The title-bar buttons therefore use accessible names without an outer tooltip wrapper.

Set both `W11TERM_UI_SMOKE=1` and `W11TERM_SEARCH_SMOKE=1` with an isolated `W11TERM_SETTINGS_PATH` to exercise native find-box focus, the wheel callback, history matching/highlights, next-result navigation, independent tab state, focus restoration, and closing find. Like the other UI smoke, it uses only local labs and closes its own window.

Set `W11TERM_UI_SMOKE=1` and `W11TERM_PLAYER_SMOKE=1`, with isolated
`W11TERM_SETTINGS_PATH` and `W11TERM_DATA_DIR`, to exercise the real trust and
masked/echoing authentication dialogs, fullscreen roundtrip, normal-grid retention,
and Rustmush wizard defaults without contacting a server or changing the clipboard.
Loopback integration tests cover actual password/MFA/host-key handshakes,
reconnection without offline-input replay, asset hashing/cache reuse, enrollment
and revocation, event-driven waits, audio events under mute, and VT recording.

An optional live probe opens a remote PTY through the production session and VT engine, waits for terminal output, and disconnects. It sends no shell commands and follows the same host-verification rules:

```powershell
cargo run --example ssh_probe -- --profile root
cargo run --example ssh_probe -- --profile play-6789
cargo run --example ssh_probe -- --profile play-6792
```

The terminal core is vendored from commit `e2a98e1b8af156988ad0ac778658ac27b60a55fb` with embedding fixes and extensions; see `vendor/wezterm-term/PATCHES.md`. The escape parser is also vendored at that revision for animation controls and extended placement parameters. The other shared crates remain pinned to that revision. `Cargo.lock` pins the resolved dependency graph. Treat upgrades as compatibility changes and rerun protocol and visual checks.

## License

w11term's original project code is licensed under [Apache-2.0](LICENSE). Vendored `wezterm-term`, `wezterm-cell`, and `wezterm-escape-parser` remain MIT-licensed, with their upstream copyright and license notices preserved. Other dependencies retain their respective licenses. See [NOTICE](NOTICE) for attribution.

Control requests have separate ordinary-command and long-poll capacity. Overload
returns a JSON `busy` error; that request was not executed. `sftp get` accepts
`--max-bytes N` to cap a scripted download. Cancelling an SFTP caller does not
invalidate unrelated handles; transport errors and actual operation timeouts do.

Remote named sounds and automatic asset fetching follow the profile's game/rustmush
mode unless `player.remote_sounds` / `player.sync_assets` explicitly override it.
The connect wizard and tab menu expose both controls. Ordinary SSH profiles default
them off; the bundled play profiles explicitly enable them. BEL remains independent.

Native profile-editor regression (isolated configuration and loopback SSH only):

```powershell
cargo build --features ui-smoke --bin w11term --example echo_server
.\tests\profile-ui-smoke.ps1
```
