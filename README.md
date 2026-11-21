# w11term

A Windows-native terminal in Rust. Windows Reactor hosts the window and GPU terminal surface. SSH panes connect directly through russh. Local shell profiles run Windows console programs through ConPTY.

Terminal emulation is provided by **[wezterm-term](https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/term)**, the terminal core from [WezTerm](https://wezterm.org/), by Wez Furlong and contributors. Its VT parser, screen model, scrollback/reflow, graphics protocols, and semantic prompt support are the foundation of w11term's terminal. The vendored crate retains its [original MIT license](vendor/wezterm-term/LICENSE.md); our local changes are documented in [PATCHES.md](vendor/wezterm-term/PATCHES.md).

## Run

GitHub Releases provides a Windows x64 ZIP containing the GUI, `w11term-cli.exe` and the Windows App Runtime. Extract the entire ZIP into one folder and launch `w11term.exe`. Each release includes a SHA-256 checksum. [Download a release](https://github.com/conclavio/w11term/releases)

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

Without arguments, the app opens a **local terminal lab**, not a shell. Its ANSI colors, Unicode text, and kitty image are actual VT traffic processed by the same engine used for SSH. Typing in the lab only exercises local echo.

`profiles.json` contains the three initial targets. `root` uses the local `hostinger` key; both `play` profiles use SSH `none` authentication, without contacting the agent or offering a private key. The key is excluded by `.gitignore`. Profiles load from the current working directory, then beside the executable or up to two parent directories (for development builds), or from an explicit `--profiles C:\path\to\profiles.json`. Relative identity paths resolve beside that configuration file. CLI SSH options can override a profile.

## Tabs and profiles

When creating or editing an SSH profile, **SSH terminal type (TERM)** selects the terminfo name sent with each new PTY. Existing profiles use `xterm-256color`. Choose `xterm-sixel` for a host running Venti with Sixel graphics, or choose **Custom…** to enter a terminfo name supported by the host. This setting is stored as `"terminal_type": "xterm-sixel"` for the Sixel preset. Direct shells and PTY-based zmx and tmux attachments request `COLORTERM=truecolor`. A process already running inside a durable session keeps its existing environment.

Open **caret → Command palette…** or press **Ctrl+Shift+K** to search actions for the current window. Search by words in any order, a profile name, or a current shortcut. Use Up and Down to highlight a result, then press Enter or click it. Disabled commands stay visible when their pane or connection requirements are unmet. Escape returns to the current pane. The palette also works from Settings and other utility tabs.

Use **Ctrl+Shift+P** or **caret → Find pane…** to search every terminal pane in the window, including panes in other tabs and hidden splits. Type part of the terminal title, profile name or SSH `user@host`. Use the arrow keys or hover over a match to preview its live viewport, including terminal graphics. The preview preserves each pane’s grid size and focus. Screen readers can read the last two nonblank preview lines. Press Enter or click a result to focus it. Escape returns to the pane you were using.

The native WinUI tab strip sits in the title bar. Click **+** to open a profile in a new tab. Drag tabs to reorder them. Closing a tab closes its panes, and closing the last tab closes the window. Each visible pane renders independently. Background sessions continue receiving output.

Use **caret → Panes** to split right or below. Splitting the current host opens a new shell channel on its authenticated SSH connection when the endpoint, user, authentication method and host-key settings match. A server that declines the extra channel gets a separate login attempt. Choosing a specific profile always opens an independent login. Each pane owns its PTY and SFTP handles. Either pane can close while the other keeps running.

Click a pane to focus it, drag a divider to resize, or zoom one pane and restore the layout when finished. `Alt+Shift+Arrow` moves the nearest divider on that axis by five percent of its span. Hold the chord to keep moving it. The same resize actions appear under **Panes**, and saving a workspace keeps its divider positions. The control CLI keeps targeting the same pane as focus and layout change.

Choose **caret → Panes → Move tab to new window** to move the whole terminal tab, including every split, to another native window. The tab keeps its title, focused pane, split proportions and zoom. Each SSH channel and local shell keeps running with its scrollback. The last tab leaving a window closes that window after the destination takes ownership. Both windows share a process and control pipe. Use the pane’s `window_id` in `w11term-cli list` to find its current window.

To monitor an SSH or local shell, choose **caret → Panes → Enable read-only pane input** or search for **read-only** in the command palette. The pane’s status line and picker entry show **Read-only**, and `w11term-cli list` reports `read_only: true`. Typing, IME commits, paste, and application mouse input are blocked. A control-pipe write gets a read-only error. Output, terminal query replies, resizing, selection, copying, and scrollback search continue. Use **Disable read-only pane input** to resume typing. The lock follows the live pane when it moves. Newly opened panes start unlocked. Closing panes and SFTP file operations are still available.

To move a running shell beside one in another tab, choose **caret → Panes → Move pane to existing tab…**. Search for the destination by title, profile or SSH address and press Enter or click it. Press an arrow key or choose **Move left**, **Move right**, **Move above** or **Move below**. Escape cancels at either step. The moved pane keeps focus, scrollback and its control ID. SSH channels and local processes keep running. The source tab closes if its last pane moves. You can assign a shortcut in Keyboard settings.

Choose **caret → Rename tab…** to give a group of panes a name such as `Production logs`. The tab strip, window title, tab switcher and pane picker use the name as you move between panes. Saving a workspace keeps the name. Choose **Use default** to return to the focused pane’s label. The control pipe’s `list` reply includes the optional `tab_title` alongside each pane’s own `label` and terminal `title`.

Choose **+ → Windows PowerShell · Local** to start a local shell or **caret → Panes → Split right with profile** to put it beside an SSH session. PowerShell 7 appears separately when installed. Under **Settings → New local shell profile…**, choose an executable, its arguments and an optional starting directory. Each argument is a separate value, so paths with spaces need no extra quoting. The same profiles work with `--profile`, `--tab` and `w11term-cli open --profile`.

Saved workspaces record the local executable, arguments and directory. Opening the workspace launches a fresh process in that directory. Closing the pane terminates its local process tree. Remote SSH panes keep their own shell, input stream and SFTP connection.

On an SSH pane, choose **caret → Panes → SFTP files** to browse the server’s SFTP working directory. Select a directory and choose **Open folder**, or select a file and choose **Download…**. Use **Upload…** or drop local files onto the drawer to upload into the displayed directory. Transfers show confirmed bytes and stay attached to the originating pane when you switch tabs.

Choose **New folder** and enter a name to create a directory under the displayed remote path. After the server confirms creation, the drawer refreshes and selects the new folder. If the name already exists, the drawer shows the server’s error. **Replace existing files** applies to file transfers.

The **Inbox** button beside the tab menu shows how many bells and terminal notifications are unread across this window. The inbox keeps the newest 128 alerts, including alerts from background tabs and splits. Select a message to mark it read and focus its source pane. **Mark all read** acknowledges the remaining messages, and **Clear inbox** empties the list. Both leave the per-pane control event journals available to scripts.

Choose **caret → Panes → Open files here** to open the drawer at the directory the focused shell last reported with OSC 7. For example, `file://server.example/home/me/my%20project` opens `/home/me/my project` on that pane’s existing SSH connection. The URI must specify an absolute path and either the pane’s SSH host or an empty host. An absent, invalid or foreign-host report opens the usual SFTP working directory.

Check **Follow shell directory** in the drawer to track later directory changes from that shell. The drawer waits for active transfers and file pickers to finish, then reads the latest reported directory when visible. It ignores invalid and foreign-host reports. Opening a folder manually or revealing a file turns following off for that pane.

**Replace existing files** starts off. Downloads publish the local destination after the transfer completes. An interrupted upload can leave a partial file on the server, and the drawer reports that outcome. A new SSH connection clears the previous directory and overwrite choice.

Use **caret → Workspaces…** to name and save your current terminal tabs. A workspace remembers tab order, split directions and proportions, the focused pane in each tab and which tab you were using. Under **Switch workspace**, type part of a name and select the workspace to bring its live window forward. If the window has closed, w11term opens its saved layout. Saving a workspace associates the current window with that name.

Use `w11term --switch-workspace "Admin console"` for the same behavior from a launcher. Workspaces match within the same Windows user, session, elevation level, workspace catalog and profile file. **Append in this window** adds a fresh copy of the layout to the current window. `w11term --workspace "Admin console"` always opens a new window. When multiple windows have the same workspace open, switching from one of those windows keeps its sessions selected. Switching from elsewhere lists the matching process IDs so you can choose the window yourself.

Each restored pane receives a new process-local control ID. SSH panes normally start a fresh shell. A saved durable pane can return to its original shell. Named profile references use the current profile settings. Ad-hoc SSH panes retain their connection options and key paths. Replay panes reference their original capture files. Workspaces are stored in `%LOCALAPPDATA%\w11term\workspaces.json`, or in `W11TERM_DATA_DIR` when set.

To give an ordinary SSH pane a startup command, choose **caret → Workspace startup…**, enter a remote command or an absolute starting directory, and choose **Apply to pane**. Then save or update the named workspace. Reopening it sends the command as an SSH exec request on a fresh PTY. For example, set the directory to `/srv/game` and the command to `tail -f logs/server.log`. A directory alone starts an interactive shell there. Directory selection requires a POSIX-compatible remote shell. Clearing both fields restores the profile’s default shell.

The recipe belongs to that pane in the saved workspace and is bound to its SSH host, port and user. Editing the recipe leaves the running session alone. After the client sends a startup command, a lost connection leaves the pane available for inspection. Reopen the workspace explicitly to run the command again. Directory-only shells retain their normal reconnect behavior.

**Settings → Import OpenSSH config** previews aliases from your Windows SSH config or another selected file. Pick an alias to see its destination, identity, jump hosts and any unsupported directives. Saving creates an editable w11term profile. Import reads the selected file and leaves it intact. Routes or trust settings that cannot be represented are flagged before saving.

Imported `ProxyJump` routes use SSH `direct-tcpip` channels. Each jump host and the final destination have their own authentication and host-key checks. The resulting SSH shell and its SFTP drawer use the destination connection. For a manually configured profile, `proxy_jump` is an ordered array of objects with `host`, `user`, optional `port`, `auth` and optional `host_key`. Each hop supports the same authentication choices as the destination.

Choose **Settings → Keyboard** to edit shortcuts for panes, tabs, the command palette, the pane picker and workspaces. Select **Edit…**, enter a chord such as `Ctrl+Alt+Right` or `F8`, and choose **Save shortcut**. **Clear shortcut** releases its key for the terminal. **Restore default** restores that action’s original binding. The editor identifies conflicts, and pane menus show the current bindings. Changes apply immediately and persist in the per-user settings file.

| Pane action | Default shortcut |
| --- | --- |
| Split right or below | Ctrl+Alt+Shift+Right or Down |
| Focus a neighbor | Ctrl+Alt+Arrow |
| Resize the focused split | Alt+Shift+Arrow |
| Zoom pane or restore layout | Ctrl+Alt+Shift+Z |
| Close focused pane | Ctrl+Alt+Shift+W |

The **+ → New Profile…** form supports host, user, port, agent/key-file/none/password/keyboard-interactive authentication, an optional verified host fingerprint, device-key generation, and player preferences. **Save and connect** adds the profile to the selected configuration file and opens it. New Profile refuses duplicate names. **Settings → Profiles → Edit…** or **caret → Edit current profile…** opens an existing profile with its saved values. The editor supports renaming and all connection/player fields. **Save changes** updates it without opening or disconnecting a session; **Save and connect** opens the updated connection. Connection changes apply to new sessions; sound and game options update open tabs. Edits preserve unrelated fields and unchanged relative key paths, refuse name collisions, and report a conflict if the profile changed on disk while the editor was open. Saving preserves unrelated configuration fields and atomically replaces the file. Only the key file's path is stored; private key contents never go into the configuration.

From the profile editor, **Duplicate saved profile…** opens a copy of the last saved settings, including key paths and custom fields. Give the copy a name, then choose **Save copy** or **Save and connect**. **Delete profile…** asks you to confirm the saved name. Open sessions in this window keep running independently, and key files stay on disk.

The caret menu displays **On**/**Off** for fullscreen, mute, game mode, alternate-screen wheel keys, remote named sounds, and asset synchronization, using the active terminal’s effective settings. Terminal-only actions are disabled when a settings/form tab is selected.

Choose **caret → Terminal inspector** to see the focused pane’s live cell grid, primary or alternate screen, mouse tracking and encoding, keyboard encoding and kitty flags, bracketed paste, and synchronized output. The panel follows focus across splits and tabs. It reads the local parser, so it can show a pending synchronized update while the terminal still displays the last committed frame. Close it with **×** or the caret menu.

Move the pointer over the focused pane to read its column and row, counted from 1 at the top-left of the visible grid, including while viewing scrollback. **Hyperlink** shows the allowed OSC 8 target or a detected URL under that cell. Moving into the padding or another pane clears the readout. Hovering leaves keyboard focus and the link destination alone.

The **caret → Settings** command opens profile management, the terminal font selector and programming-ligatures toggle. The selector includes only installed Cascadia Code, Cascadia Mono, Consolas, JetBrains Mono, Monaspace-family, and Fira Code fonts. Changes apply across the window's terminal tabs and are saved automatically to `%LOCALAPPDATA%\w11term\settings.json`. `W11TERM_SETTINGS_PATH` can override that file for portable configuration or isolated tests. Settings and New Profile tabs use native controls and do not send their typing into an SSH session. The plus menu also includes the terminal lab.

Programming ligatures default to enabled for adjacent ASCII cells with matching attributes. Monaspace also enables its programming-ligature feature sets. DirectWrite shapes connected scripts even with programming ligatures disabled. Font changes recalculate cell geometry and resize existing sessions. PNG capture uses the same font and shaping settings as the live pane. [Monaspace features](https://github.com/githubnext/monaspace#code-ligatures)

SSH supports OpenSSH private keys supplied with `--identity`, protected w11term device keys, the Windows OpenSSH agent (the default for `ssh user@host`), `--auth none`, `--auth password`, and `--auth keyboard-interactive`. Password and MFA challenges use native dialogs; hidden answers never enter terminal input, profiles, event logs, or recordings. A successful password remains in memory only for reconnection; one-time codes are requested again. Passphrase-protected OpenSSH keys can be used through the agent.

The first connection to an unknown host shows its algorithm and SHA256 fingerprint. **Accept and save** writes the public key to OpenSSH `known_hosts`. Cancel stops the connection. A changed known key fails without offering to replace it. An explicitly verified `--host-key SHA256:...` pin remains an alternative. Headless probes fail on unknown hosts instead of opening a dialog.

## Browse existing Herdr terminals

Choose **caret → Find remote sessions → Browse Herdr terminals on _profile_** for a saved Linux SSH profile with Herdr Core’s `hc` available to remote SSH commands. Select a terminal by its display name and numeric address (such as `t7`). w11term opens a full interactive viewer over that SSH login. The program receives input and resize events, and continues running after the viewer detaches. This explicit command runs `hc connect`, which can start the account’s Core daemon. Session discovery and attachment use existing terminal records. The adapter targets Core’s generation 1 framed protocol from v0.1.21. [Herdr viewer protocol](https://github.com/herdrdev/herdr-core/blob/v0.1.21/schema/README.md#attach-streams)

Reopen a saved workspace or choose **Remote session → Reattach to remote session** to request the same program again. w11term checks the SSH account and host key, persistent Core ID, terminal address and creation time, and the Linux process’s PID, boot ID and start ticks. Verification requires readable `/proc` entries and SSH exec channels. After a disconnect or a kept terminal replacing its program, the viewer shows its state. Choose an existing terminal again when its identity changes.

Herdr can repaint the screen during attachment or after a slow viewer catches up. For the lifetime of these panes, remote clipboard changes, bells, named sounds and notifications stay disabled. Local copy/paste, terminal input, rendering and SFTP remain available.

## Durable Linux shells

In the SSH profile editor, enable **Keep the remote shell running with zmx (Linux)** to let a remote PTY survive a network drop or a w11term restart. The Linux account needs a compatible zmx 1.x executable on its SSH command path and permission to run SSH exec requests. The tested version is 1.0.1. Save the workspace after the first attachment to reopen the same shell and layout together.

From a fresh workspace, choose **caret → Find remote sessions → Find sessions on _profile_**. w11term authenticates to that SSH account and lists its w11term-managed zmx shells and existing tmux sessions on the account’s default socket. Each row shows the session name, backend and attached client count, plus the window count for tmux or the available command and directory for zmx. Select a row to attach in the browser tab. **Refresh** checks the host again. Discovery works with an ordinary saved SSH shell profile and keeps its usual launch behavior. You can save the selected session in a workspace or save an unselected browser to list that host again on reopen.

tmux attaches in its normal full-screen UI on an SSH PTY. Its prefix keys, windows and panes work as they do in a plain SSH attachment. Detach in tmux or use **Remote session → Detach from remote session**. Reattach and workspace restore use the same session ID even after a rename. If the tmux server restarts or the session ends, choose **Remote session → Find remote sessions again** to return to the list. Creation and termination of tmux sessions stay with the remote account. Stock tmux 3.4 on Linux is exercised in CI.

Use **caret → Panes → Remote session** to detach, reattach, or end the remote session. Ending requires confirmation. w11term checks the SSH destination, account, host fingerprint and remote session generation before accepting input. If recovery fails, the pane shows the outcome and offers an explicit choice to start a new durable shell or a plain SSH shell. Ordinary game profiles that accept only shell requests can keep using direct SSH.

The Linux trial restored Vim's unsaved buffer, editing mode and current grid. An application using a static kitty image needs to redraw it after reattachment. [Linux trial and reproduction steps](docs/durable-linux-trial.md)

## Player profiles

Dropped transports reconnect in the same tab, with 1/2/4/8/10-second backoff and a 300-second retry window. Permanent authentication/trust errors and normal remote process/channel closure stop retrying. Input while disconnected is discarded; raw control writes and SFTP operations fail explicitly instead of being replayed. The latest grid is applied to the new PTY, terminal protocol state resets, and primary scrollback remains. The client returns to the server's prompt; it never sends `BOARD` or replays game commands automatically. Every SSH shell requests `COLORTERM=truecolor`.

Applications that negotiate the [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) can distinguish keypad Enter and Num Lock-off navigation from the main Enter and navigation cluster. With disambiguation enabled, printable keypad digits and operators still type the text produced by the active Windows layout. Report-all mode identifies the physical keypad function, including a localized decimal key, and reports associated text when requested.

Game mode suppresses held-key repeats while preserving the initial press and final release. Normal terminal mode sends negotiated kitty repeat event type 2 and release type 3. The caret menu toggles game mode and mute for the active saved profile. **F11** or the caret menu toggles borderless fullscreen. Normal window size and position are remembered per profile in `%LOCALAPPDATA%\w11term\windows.json`, clamped to the current monitor on restore. Explicit launch `--cols`/`--rows` takes precedence. The status bar displays columns × rows throughout resizing.

Alternate-screen wheel-to-arrow compatibility is disabled by default in game mode.
`player.alternate_scroll` defaults to `null` (follow game mode); set it to `true` or
`false` to override that policy. The connect form and caret menu expose this setting.
Application-requested wheel reports still pass through, Shift keeps the local
override, and alternate-screen history remains the remote application's job.
The bundled `play-6789` and `play-6792` profiles explicitly disable wheel-generated
arrow keys.

The wizard generates Ed25519 device keys in process, stores them under `%LOCALAPPDATA%\w11term\keys`, encrypts private bytes with current-Windows-user DPAPI, and restricts the private file ACL to that user and SYSTEM. The machine name becomes the public comment. Only the public line can be copied by the wizard. These protected files are w11term-specific; OpenSSH cannot read them directly. For a generic host, append the copied public line to `~/.ssh/authorized_keys`.

For a Rustmush profile, select the Rustmush checkbox, generate a device key, and sign in with a password. Once at the game command prompt, choose **caret → Enroll this device**. It sends `@sshkey/add`, reads a fresh `@sshkey/list`, and changes future profile authentication to the key only after verifying the exact SHA256 fingerprint. The confirmation shows that fingerprint. **Forget this device** revokes that fingerprint, verifies the server listing, and returns the profile to password authentication. The encrypted local key remains available for reenrollment. `account+character` usernames pass through unchanged. Account creation stays in the server's welcome flow. Do not concurrently drive the same pane while enrollment is running.

BEL and names-only OSC 440 sounds support per-profile mute, `system`/`off`/named bells, and verified asset packs fetched over the existing SFTP connection. The complete server contract—including OSC bytes, manifest limits, and Hello fields—is in [Game client protocol v1](docs/game-client-protocol.md). Sprite/font/help assets are cached without installing or executing them. Set `W11TERM_DATA_DIR` to redirect keys, assets, and window placements for portable configuration or isolated tests; appearance settings retain their own override.

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
cargo run -- --benchmark
cargo run -- --demo --smoke-seconds 5
```

The benchmark measures **only VT parsing and scrollback**. It excludes snapshot creation, rendering, SSH, and input-to-screen latency. Debug-build results are not performance claims. Build with `cargo build --release` for performance work. The release GUI executable does not attach a console.

## Controls

**Ctrl+Shift+X** or **caret → Panes → Keyboard copy mode** lets you select text in the focused pane while its session continues running. Move with the arrow keys or **h/j/k/l**, page with **Page Up/Down**, use **Home/End** or **0/$** for the edges of a row, and **g/G** or **Ctrl+Home/End** for the ends of retained history. Press **v** for character selection or **Shift+V** for whole rows, then **y** or **Enter** to copy plain text. Soft-wrapped rows join on copy. **Esc** cancels and restores your previous viewport. Inside an alternate screen the current grid is available. Remap the activation shortcut under **Settings → Keyboard**.

**Ctrl+Shift+H** or **caret → Panes → Keyboard hints** freezes the focused viewport and labels its URLs and file paths. Type the letters shown beside a target. **Enter** opens a URL or copies a path, **C** copies either, **Backspace** edits the label, and **Esc** returns to the live pane. URLs use the same link rules as Ctrl+click, including OSC 8 hyperlinks. Paths can include quoted spaces, soft wraps, and compiler-style line and column suffixes.

On an SSH pane, **R** reveals a selected Unix path in the existing SFTP drawer and selects its directory entry. Relative paths use the OSC 7 directory reported by that SSH host when hints opened. Absolute paths work without a directory report. Local Windows paths and paths with no usable remote base can still be copied.

Use **Settings → Hint patterns** to add, edit or remove named regular expressions. For example, an **Issue** pattern with `\bOPS-[0-9]+\b` labels issue numbers such as `OPS-104`. Select its label and press **Enter** or **C** to copy exactly the matched text. When the entire match is a supported URL, **O** opens it. For a recognized remote path, **R** reveals it in SFTP.

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
| Ctrl+Alt+V / caret → Paste raw | UTF-8 clipboard bytes, without bracket markers, trimming, newline conversion, or key encoding |
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

Find searches the current pane’s retained history and visible screen. It starts with a case-insensitive literal search using Unicode lowercase matching. Turn on **Regex** for patterns such as `error\s+[0-9]+`, or **Match case** to distinguish uppercase and lowercase. Matches can span a line wrapped by the terminal and stop at a hard newline. Enter and Shift+Enter move between highlighted matches. Escape closes Find and restores terminal input. Reopening Find restores that pane’s query and both options.

Queries accept up to 4,096 UTF-8 bytes. The match count refreshes as output arrives, and `+` indicates more than 10,000 matches. If a pattern is invalid, the Find bar shows the reason as you edit. Regex search skips logical lines larger than 64 KiB and shows when the results are incomplete. Ordinary scrolling stays anchored to retained rows as new output arrives.

The alternate screen has no local scrollback. A TUI receives wheel reports when it enables mouse reporting. For other TUIs, **Alternate-screen wheel keys** enables the arrow-key fallback used by pagers. It defaults to on for standard profiles and off in game mode. Shift+wheel scrolls local primary-screen history and has no effect on the alternate screen. Find on the alternate screen searches its current contents. Returning to the primary screen restores its history position.

**OSC 133 semantic prompts.** With shell integration enabled, Ctrl+Shift+Up/Down jumps between prompt starts. **Caret → Panes → Command history** opens the focused pane’s completed commands, newest first. Select a command to preview its output and exit status. **Copy output** copies its full plain text, joining soft wraps and preserving hard newlines. **Refresh history** selects the latest command, and **Return to pane** takes you back to the originating shell. **Copy last command output** remains available directly in the Panes menu.

History uses visible command text between the shell’s B and C markers when available. Output runs from C to D or the next prompt. The pane footer shows an explicit numeric exit status. Each pane retains up to 64 commands and 4 MiB of captured text in memory. A command whose output exceeded 1 MiB or left scrollback before completion shows the reason in its preview. Alternate-screen applications leave shell history intact. A terminal reset or reconnect clears it.

The Settings tab exposes auto-copy, block-selection trimming, paste trimming, grid snapping, tab-switcher mode (tab order/MRU/disabled), Ctrl+scroll zoom, URL detection, and remote OSC 52 writes. Auto-copy is enabled as requested; the other listed boolean options default to enabled, and the switcher defaults to tab order. Copies always contain plain text. Normal paste trims whitespace only at the end of the entire payload by default; raw paste preserves it. The resize border snaps to cell boundaries during manual dragging. These behaviors follow the relevant [Windows Terminal interaction settings](https://learn.microsoft.com/en-us/windows/terminal/customize-settings/interaction), with the explicit auto-copy override.

Remote programs can set the local Windows clipboard with **OSC 52**, without a forwarded control pipe. For example, on the SSH host:

```sh
printf '\033]52;c;%s\a' "$(printf '%s' 'Copied from the remote host 🌍' | base64 | tr -d '\n')"
```

OSC 52 writes are plain text, work for background sessions, and are capped at 1 MiB and four remote writes per second across the process. Clipboard-read queries receive no response. Remote clipboard writes can be disabled in Settings. Normal UI paste remains distinct from the control CLI's `send-text --no-paste`, which always sends raw bytes. For bytes that cannot be represented as Windows clipboard text, use `--file`, `--stdin`, or `--base64` through the CLI.

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
- The terminal displays kitty, sixel and iTerm images. Kitty supports timed or client-selected animation frames, Unicode placeholders and relative placements. The [graphics audit](docs/graphics-audit.md) documents reference fixtures, GPU cache eviction, transfer budgets and protocol coverage.
- DirectWrite shapes text runs with matching attributes and supports italic, color emoji and five underline styles with independent decoration color. Bidirectional ordering uses the terminal's row direction with a left-to-right default. Selection, search highlights, URL hit testing and cursor placement share the displayed cell map. Copy and control-pipe text retain logical order. Each physical row resolves independently, including soft-wrapped rows. The replay fixture exercises mixed Hebrew and digits, Arabic joining, a Devanagari conjunct, italic and underline styles. [Replay fixture](tests/fixtures/complex-text.vt)
- Windows input methods compose at the focused pane's caret. Committing sends the composed text once. The UI Automation Text and Text2 patterns expose the current viewport, selection and caret to assistive technology. See [IME behavior and manual checks](docs/ime.md).
- Keyboard press, repeat, release and game-mode repeat suppression are connected. Physical and alternate key handling still have coverage gaps.
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

w11term's original project code is licensed under [Apache-2.0](LICENSE). Vendored `wezterm-term` and `wezterm-escape-parser` remain MIT-licensed, with their upstream copyright and license notices preserved. Other dependencies retain their respective licenses. See [NOTICE](NOTICE) for attribution.

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
