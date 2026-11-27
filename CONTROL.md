# External control

Build both executables with `cargo build --release --bins`. `w11term.exe` is the GUI and `w11term-cli.exe` is its console companion. The CLI connects to an existing instance, writes JSON to stdout, and exits with code 1 on errors. Commands use one reply, while `follow-ssh-group-output` emits JSON Lines.

Launch directly into a remote session with a known cell box:

```powershell
.\target\release\w11term.exe --profile play-6792 --cols 170 --rows 48
.\target\release\w11term.exe --cols 170 --rows 48 ssh pixeltest@conclav.io --port 6792 --identity .\hostinger
```

Geometry applies to the initial PTY as well as the window. Either dimension can be supplied separately (the other defaults to 120 columns / 32 rows). Allowed ranges are 1–1000 columns and 1–500 rows. Subsequent manual window resizing updates the PTY normally. The window must fit the requested grid on the available desktop.

Find the instance and pane:

```powershell
$ctl = '.\target\release\w11term-cli.exe'
& $ctl instances
$listing = & $ctl --pid 12345 list | ConvertFrom-Json
$pane = $listing.result.sessions[0].id
```

Omit `--pid` when exactly one process is running. Each process serves all its windows on one pipe. Zero processes fail immediately and multiple processes require an explicit PID. Each session in `list` includes its process-local `window_id`, `tab_id` and pane `id`. Moving a live tab between windows changes its window ID and preserves its tab and pane IDs. Visible panes in all windows report `visible: true`. At most one pane reports `active: true`, in the most recently active window. Settings and New Profile pages are excluded.

`list-windows` returns the registered windows ordered by `window_id`, including windows with only utility tabs. Each entry has `name` and `active_tab_title` (nullable), `tab_count` including utility tabs, terminal `pane_count`, and `active` for the most recently active window in the process.

`list-tabs --window ID` returns `window_id` and the left-to-right `tabs` array for that window. Each item gives its `tab_id`, displayed `title`, `selected`, `pinned`, `zoomed`, `pane_count`, and remembered `focused_pane`. The effective `color` and explicit `color_override` use the named tab palette. A null override follows the focused pane's profile. A utility tab such as Settings has zero panes and a null `focused_pane`.

`reorder-tabs --window ID --tab ID [--tab ID ...]` sets the entire strip order, including utility tabs. Provide every current tab ID once and keep pinned tabs before unpinned tabs. Invalid orders leave the strip intact. The reply gives `window_id`, ordered `tab_ids`, and `changed`. Both commands preserve tab selection and desktop focus, including in a background window.

```powershell
& $ctl --pid 12345 list-tabs --window 2
& $ctl --pid 12345 reorder-tabs --window 2 --tab 10 --tab 14 --tab 12
```

Direct pipe clients can send `{"verb":"list-tabs","window_id":2}` or `{"verb":"reorder-tabs","window_id":2,"tab_ids":[10,14,12]}`.

`rename-tab --window ID --tab ID (--name NAME | --reset)` sets or clears the explicit title of a terminal tab. `rename-pane --pane ID (--name NAME | --reset)` sets or clears the name of one pane, including after it moves. The commands target hidden tabs and background panes without selecting them or changing desktop focus. Names can contain 1–128 characters, with no control characters or surrounding spaces.

Replies include `window_id`, `tab_id`, the explicit `name` (null after reset), and `changed`. Pane replies also include `pane`. The names appear in later listings and workspace saves. Direct pipe clients must supply the `name` field. Send `{"verb":"rename-tab","window_id":2,"tab_id":10,"name":"Fleet Ops"}` or `{"verb":"rename-pane","pane":42,"name":null}` to reset a pane.

`pin-tab --window ID --tab ID (--on | --off)` pins or unpins a terminal tab. Pinning puts it at the end of the pinned section and protects it from tab close actions. Unpinning puts it at the start of the unpinned section.

`set-tab-color --window ID --tab ID (--color NAME | --reset)` uses the tab palette `red`, `orange`, `yellow`, `green`, `blue`, or `purple`. Use `--color none` to hide the marker and `--reset` to follow the focused pane's profile. Both commands preserve selection and desktop focus, including in background windows, and both settings are included in workspace saves. Replies give `window_id`, `tab_id`, `pinned`, `color_override`, effective `color`, and `changed`. Direct pipe clients can send `{"verb":"pin-tab","window_id":2,"tab_id":10,"pinned":true}` or `{"verb":"set-tab-color","window_id":2,"tab_id":10,"color":"blue"}`. Send `"color":null` to restore the profile color.

`open --profile NAME --window ID` opens in that window and returns its `window_id` alongside the pane ID and grid. Omitting `--window` uses the most recently active window. An unknown or closed ID fails. `--activate` selects the newly opened tab in the target window. Pipe requests preserve desktop focus.

`list-profiles` reads the running instance's profile catalog in the same order as the profile picker. The reply includes `startup_profile` (nullable) and a `profiles` array with `name`, `backend` (`ssh` or `local`), and `pinned`. SSH entries give `host`, `user`, and `port`. Local entries give `wsl_distribution` for interactive WSL profiles, or null. The reply omits authentication settings, executable paths, arguments, and working directories.

`open --profile NAME --new-window [--cols N --rows N]` opens a saved profile in a new window of the same process, preserving desktop focus. The reply has the new `window_id` and `pane`, the initial terminal `cols` and `rows`, and `active: true` for the selected pane within that window. The new window is registered on the same pipe when the reply arrives. With no grid options, the window can restore its saved geometry. Use `list` for the measured grid after window layout. For direct pipe clients, send `{"verb":"open-profile-window","profile":"Fleet Ops","cols":120,"rows":32}`.

`list-workspaces` reads the saved workspace catalog and returns `workspaces` in name order. Each entry has `name`, `tab_count`, and `pane_count`.

`get-workspace --name NAME [--window ID]` previews a saved workspace without opening it. The reply contains `name`, zero-based `active_tab`, ordered `tabs`, and sorted `required_ssh_profiles`. Each tab has its `title`, `pinned`, `color_override`, remembered `focused_slot`, and `layout`. The layout array follows the same split and child-index format as `get-tab-layout`, with saved `slot` values for pane leaves. Slots identify positions in a recipe and receive new live pane IDs on open.

Pane leaves include `pane_name`, `font_size`, and a `source` with `kind`, the nullable `profile` name, and `startup` and `durable` flags. The preview omits connection settings, credential and recording paths, launch commands, and durable session identifiers. With `--window`, `profile_check` gives that `window_id` and its `missing_ssh_profiles`. This checks saved SSH profile names against the selected window's catalog. Without a window, `profile_check` is null. Direct pipe clients can send `{"verb":"get-workspace","name":"Fleet Ops","window_id":2}`.

`rename-workspace --name OLD --to NEW` renames a recipe in the saved workspace catalog. The new name must be unused. The layout and workspace identity carry over, so the workspace switcher still recognizes an open window under the new name. The reply gives `old_name` and `name`. The command preserves live sessions and desktop focus. Direct pipe clients can send `{"verb":"rename-workspace","name":"Fleet Ops","new_name":"Production Ops"}`.

`repair-workspace --window ID --name NAME --replace OLD NEW [--replace OLD NEW ...]` replaces missing SSH profile references in a saved workspace. Each pair updates every pane that refers to `OLD`. The command checks the selected window's SSH profile catalog and applies the same connection checks as the GUI repair, including for startup commands and durable sessions. All requested replacements are saved together, and the workspace keeps its identity. The reply includes `name`, `window_id`, the `replacements` object keyed by old name, and `updated_panes`. Repair changes the saved recipe while leaving live sessions and desktop focus in place.

Direct pipe clients can send `{"verb":"repair-workspace","window_id":2,"name":"Fleet Ops","replacements":[{"missing":"ops-old","replacement":"ops"}]}`.

`save-workspace --window ID --name NAME` saves the terminal tabs in that window as a new workspace and registers the window under the saved name for the workspace switcher. It captures the same pane launch recipes, split proportions, titles, and remembered selection as **Save workspace** in the GUI. Utility tabs are skipped. Saving keeps the current sessions, selection, and desktop focus in place. Existing names are rejected, and the reply includes `name`, `window_id`, `tab_count`, and `pane_count`. Direct pipe clients can send `{"verb":"save-workspace","window_id":2,"name":"Fleet Ops"}`.

`open-workspace --window ID --name NAME` appends a fresh copy of a saved layout to the specified window. The existing selection stays in place by default. Pass `--activate` to select the saved tab and pane within that window while preserving desktop focus. Missing SSH profiles cause the entire request to fail before any tabs are added.

`open-workspace --new-window --name NAME` opens a fresh copy in a new window of the same process and selects its saved pane while preserving desktop focus. The reply arrives after the new window has registered with the control pipe. The window has the usual saved workspace identity and can be found by the workspace switcher. For direct pipe clients, send `{"verb":"open-workspace-window","name":"Fleet Ops"}`.

The reply contains `name`, `window_id`, and `tabs` in saved order. Each tab contains its new `tab_id` and `panes` (new pane IDs in layout order). `workspace_active_pane` identifies the saved active pane's new ID, and `active` reports whether the new workspace selection took effect in that window.

```powershell
& $ctl --pid 12345 list-profiles
& $ctl --pid 12345 open --profile 'Fleet Ops' --new-window --cols 120 --rows 32
& $ctl --pid 12345 list-workspaces
& $ctl --pid 12345 get-workspace --name 'Fleet Ops' --window 2
& $ctl --pid 12345 repair-workspace --name 'Fleet Ops' --window 2 --replace 'ops-old' 'ops'
& $ctl --pid 12345 save-workspace --window 2 --name 'Fleet Ops'
& $ctl --pid 12345 open-workspace --window 2 --name 'Fleet Ops' --activate
& $ctl --pid 12345 open-workspace --new-window --name 'Fleet Ops'
& $ctl --pid 12345 rename-workspace --name 'Fleet Ops' --to 'Production Ops'
```

`resize` routes to the window that owns the pane, including after a live tab transfer. Closing one window leaves the process pipe and the remaining windows’ sessions accessible.

`split-pane --pane ID --profile NAME --axis columns|rows [--activate]` starts a fresh session to the right of the pane (`columns`) or below it (`rows`). The command targets the pane's current tab and window, including background tabs and panes moved between windows. The reply includes `source_pane`, the new `pane`, `tab_id`, `window_id`, `axis`, and `active`.

By default the current selection and zoom stay in place. With `--activate`, the owning window selects the new pane and shows the split. The reply's `active` field reports this window-local selection. Desktop focus stays where it is.

```powershell
& $ctl --pid 12345 split-pane --pane $pane --profile 'Windows PowerShell' --axis columns
```

Direct pipe clients can send `{"verb":"split-pane","pane":42,"profile":"Windows PowerShell","axis":"columns"}`.

`move-pane --pane ID --target-pane ID --direction left|right|up|down [--activate]` moves an existing session beside a pane in another tab of the same process. The tab can be in either window. The response includes `pane`, `target_pane`, the destination `window_id` and `tab_id`, `direction`, and `active`. Pane IDs, connections, and scrollback survive the move.

The destination keeps its selection and zoom by default. Use `--activate` to select the moved pane in the destination window. The source selects a surviving pane if its selected pane moves. Desktop focus stays where it is. A move that would empty and close the source window is rejected.

```powershell
& $ctl --pid 12345 move-pane --pane $pane --target-pane 57 --direction right
```

Direct pipe clients can send `{"verb":"move-pane","pane":42,"target_pane":57,"direction":"right"}`.

`move-tab --pane ID --window ID` moves the entire tab containing the pane to an existing window of the same process. Use `--new-window` in place of `--window` to move it to a fresh window. The move preserves the tab ID, pane IDs, split geometry, remembered selection, zoom state, and live sessions. The source needs another tab so moving the requested one leaves its window open.

An existing destination keeps its selection unless you pass `--activate`. A new window selects the moved tab. Both forms preserve desktop focus. The reply gives the destination `window_id`, `tab_id`, `panes` in layout order, and `active` for selection within the destination.

```powershell
& $ctl --pid 12345 move-tab --pane $pane --window 2
& $ctl --pid 12345 move-tab --pane $pane --new-window
```

Direct pipe clients can send `{"verb":"move-tab","pane":42,"window_id":2}` or `{"verb":"move-tab","pane":42,"new_window":true}`.

`focus-pane --pane ID` selects that pane and its tab in the owning window. If the tab is zoomed, the selected pane fills it. The command follows the pane across window moves and preserves desktop focus.

`zoom-pane --pane ID --on` selects and enlarges the pane within its split tab. `--off` restores the target tab's split layout while keeping its selection and the window's current tab. Both settings can be applied repeatedly. Each reply includes `pane`, `window_id`, `tab_id`, `active` for selection within that window, and `zoomed` for the target tab's resulting state.

```powershell
& $ctl --pid 12345 focus-pane --pane $pane
& $ctl --pid 12345 zoom-pane --pane $pane --on
& $ctl --pid 12345 zoom-pane --pane $pane --off
```

Direct pipe clients can send `{"verb":"focus-pane","pane":42}` or `{"verb":"zoom-pane","pane":42,"zoomed":true}`.

`get-tab-layout --pane ID` reads the terminal tab containing that pane. It works for a hidden tab, a zoomed split, or another window and preserves desktop focus. The reply gives `window_id`, `tab_id`, the tab's remembered `focused_pane`, `selected` within its window, and `zoomed`. A zoomed tab still reports its entire split tree.

The `layout` array starts at index 0. A node of kind `pane` contains a live pane ID. A node of kind `split` contains an `axis` (`columns` or `rows`), the fraction `ratio` assigned to its first child, and the zero-based indices `first` and `second`. Children are ordered left to right or top to bottom. The ratio is the stored divider weight. Measured cells are available through `list`. Node indices apply only to that reply.

```json
{
  "window_id": 1,
  "tab_id": 9,
  "focused_pane": 42,
  "selected": true,
  "zoomed": false,
  "layout": [
    {"kind":"split","axis":"columns","ratio":0.375,"first":1,"second":2},
    {"kind":"pane","pane":42},
    {"kind":"pane","pane":57}
  ]
}
```

Direct pipe clients can send `{"verb":"get-tab-layout","pane":42}`.

`resize-split --pane ID --target-pane ID --percent N` sets the divider where the two panes' paths first diverge. The branch containing `--pane` gets `N` percent of that split's usable width or height and the opposite branch gets the rest. Decimal percentages between 0 and 100 are accepted when both branches have room for at least one cell per pane. Child divider weights stay in place.

The two panes must share a measured, unzoomed split in their window's selected tab. The command also works in a background window and keeps desktop focus and pane selection in place. The reply includes both pane IDs, `window_id`, `tab_id`, `axis`, the requested `percent`, the first child's resulting `ratio`, and `changed`. An identical weight returns `changed: false`. The reply confirms the layout edit. Use `list` to read subsequent measured cell sizes.

Direct pipe clients can send `{"verb":"resize-split","pane":42,"target_pane":57,"percent":62.5}`.

```powershell
# Literal UTF-8, including a carriage return supplied by PowerShell.
& $ctl --pid 12345 send-text --pane $pane --no-paste "BOARD`r"

# Arbitrary bytes: BEL (Ctrl+G), then CSI C (right arrow).
& $ctl --pid 12345 send-text --pane $pane --no-paste --base64 'Bw=='
& $ctl --pid 12345 send-text --pane $pane --no-paste --base64 'G1tD'

# Binary input without text decoding or newline conversion.
& $ctl --pid 12345 send-text --pane $pane --no-paste --file .\input.bin
# --stdin also reads binary bytes; the producer must preserve bytes itself.

$screen = & $ctl --pid 12345 get-text --pane $pane | ConvertFrom-Json
if ($screen.result.text -notmatch 'fps=') { throw 'HUD missing' }

& $ctl --pid 12345 find-text --pane $pane 'connection reset'
$found = & $ctl --pid 12345 find-text --all-panes 'connection reset' | ConvertFrom-Json
$found.result.panes | Where-Object { !$_.error -and $_.count -gt 0 }
& $ctl --pid 12345 find-text --all-panes --regex --match-case 'ERROR\s+[0-9]+'

& $ctl --pid 12345 screenshot --pane $pane .\frame.png
& $ctl --pid 12345 save-text --pane $pane .\shell-transcript.txt
```

`send-text` requires `--no-paste`. It sends the supplied bytes directly to the SSH channel or local ConPTY input pipe, with no bracketed-paste delimiters, key encoding, UTF-8 normalization, or escape expansion. `--base64`, `--file`, and `--stdin` accept arbitrary bytes including NUL and invalid UTF-8. The limit is 1 MiB per command. A successful reply acknowledges the write. The receiving program may still be processing it. Use `wait-for-text` to wait for output. A write timeout means delivery is unknown and retrying could duplicate input. Demo and replay panes reject raw input. Writes and SFTP requests during reconnect return errors.

Pass `--connection-generation N` to bind the write to the live session you
inspected. Use the generation from `list`, or from `wait-ssh-connected` for SSH,
with the same process and pane ID. A stale generation returns an error before
any bytes reach the replacement session. A successful scoped reply includes
`connection_generation` alongside `pane` and `bytes_written`.

`get-text` returns the current live screen, including alternate-screen TUI content. It excludes scrollback, trims trailing ASCII spaces on each row, and preserves the row count as `lines`. `text` joins those lines with LF. Graphics are not OCRed. Both text and screenshots include a snapshot generation to help correlate results. Reading does not change the user's scrollback position.

`screenshot` renders an immutable snapshot with the same DirectWrite/Direct2D code used by the window, including the selected font, ligature preference, and kitty/sixel images. It exports only the terminal grid, at the pane's last negotiated DPI; title bar, padding, and status bar are excluded. It works for background tabs and covered/minimized windows without selecting a tab or touching focus. Existing destination files are replaced atomically after PNG encoding succeeds. The parent directory must already exist. Captures above 16 megapixels are rejected. Current renderer limitations (including first-frame-only animated images) also apply to PNGs.

`save-text` exports the addressed pane’s retained screen and history as UTF-8
plain text with CRLF line endings. It joins soft wraps. While the alternate
screen is active it exports that screen’s rows. The reply contains `pane`,
`path`, and the number of UTF-8 `bytes` written. The CLI resolves the path from
its working directory and requires `--overwrite` to replace an existing file.
The parent directory must exist. The file is published atomically after capture.
Exports over 32 MiB and captures during a pending synchronized update return
an error. A retry after the update captures the newly committed text.

## Pipe protocol, version 1

Each GUI owns `\\.\pipe\w11term-<pid>` and publishes `%LOCALAPPDATA%\w11term\instances\<pid>.json`:

```json
{"version":1,"pid":12345,"pipe":"\\\\.\\pipe\\w11term-12345"}
```

The pipe accepts only the process's Windows user and SYSTEM, and rejects remote clients. Treat access as permission to type into that user's SSH and local sessions and save screenshots with that user's filesystem permissions. Discovery records are removed on normal exit; stale records from a crash are ignored when the pipe cannot be opened.

Open a new pipe connection for each command. Write one UTF-8 JSON object followed
by LF. Replies are UTF-8 JSON objects followed by LF. `follow-ssh-group-output`
sends a sequence of replies on the same connection. Maximum request size is
2 MiB. The server allows eight ordinary requests and sixteen waits or followers
concurrently. The initial read and each reply write time out after ten seconds.
Input and snapshot requests have a ten-second execution timeout, and the CLI
allows thirty seconds for a reply to accommodate rendering.

Keep the connection open while waiting for replies. Disconnecting cancels a
pending wait or output follower and releases its slot. A screenshot write that
already started after a text match can still finish.

```json
{"verb":"list"}
{"verb":"send-text","pane":1,"no_paste":true,"data_base64":"Qk9BUkQN"}
{"verb":"send-text","pane":1,"no_paste":true,"connection_generation":2,"data_base64":"Qk9BUkQN"}
{"verb":"get-text","pane":1}
{"verb":"inspect","pane":1}
{"verb":"save-text","pane":1,"path":"C:\\tests\\shell-transcript.txt","overwrite":false}
{"verb":"screenshot","pane":1,"path":"C:\\tests\\frame.png"}
```

Direct protocol screenshot paths, including `wait-for-text.screenshot_path`, must be absolute. The CLI resolves relative paths in the caller's working directory.

```json
{"version":1,"ok":true,"result":{"pane":1,"bytes_written":6}}
{"version":1,"ok":false,"error":"unknown pane 99"}
```

`list.result` contains `pid` and a `sessions` array. Use each entry’s `id` as the argument to `--pane`. Its `tab_id` identifies the containing tab. Both IDs are local to the running process. The `active` flag identifies the selected terminal pane, and `visible` reports whether the layout displays it. Selecting a utility tab leaves every terminal inactive. A script can address any pane independently of visibility or native keyboard focus.

Each entry’s `label` is its launch label and `title` is the application-reported terminal title when a capture is available. Explicit user names appear in separate optional fields. `pane_name` belongs to that individual pane and `tab_title` belongs to its group. Resetting either name removes its field from subsequent replies.

Each entry also has a `label` and `status`. Available sessions report `title`, `cols`, `rows`, `connection_generation`, and `prompt`. An unavailable session retains its IDs and flags and reports an `error`. Prompts expose public challenge metadata only. The connection generation increases after each new SSH login. `get-text.result` adds `generation`, `lines`, `text`, and `alternate_screen`. Screenshot replies contain `pane`, `generation`, absolute `path`, `width`, and `height` in pixels.

Available SSH entries also report the current `ssh_connection` and
`restart_token` using the same fields as `events`. One `list` request can
show the lifecycle state of all SSH panes in a process.

## Live pane inspection

`inspect --pane ID` reads the selected pane’s local terminal parser once. It works on background and read-only panes and preserves keyboard focus. During a synchronized update, `inspect` can already show the new modes while `get-text` still returns the last committed screen.

```powershell
$reply = & $ctl --pid 12345 inspect --pane $pane | ConvertFrom-Json
if (!$reply.ok) { throw $reply.error }
$modes = $reply.result
"$($modes.cols) × $($modes.rows) cells, $($modes.pixel_width) × $($modes.pixel_height) pixels"
if ($modes.in_band_resize) { 'This pane has subscribed to resize reports.' }
```

The reply includes `pane`, `connection_generation`, `cols`, `rows`, `pixel_width`, `pixel_height` and `dpi`. Pixel dimensions describe the drawable text area. An unavailable measurement is zero. The booleans `alternate_screen`, `bracketed_paste`, `synchronized_output` and `in_band_resize` reflect the modes at the time of the read.

For a live shell that emits supported integration markers, `running_command`
appears after the command start marker. It contains `id`, `start_token`, `command`,
`reported_directory`, `started_unix_ms`, and `elapsed_ms`. The command line
and initial directory can be `null`. Elapsed time uses the local monotonic
clock. The command remains visible through alternate-screen programs and the
field clears when completion, a new prompt, reset, or disconnect ends the
observation. When the shell finishes normally, the capture uses the same ID
in `list-commands` for this pane and connection generation.

SSH panes also include `ssh_connection`, with the same phase and optional reason or retry attempt as `list`. While a shell is opening, `ssh_progress` reports the current `step`, `host`, `port` and `user`. A one-based `jump` identifies a ProxyJump hop. Its absence identifies the destination. A connection that ends in failure during an endpoint operation retains that step. Steps are `opening-tcp`, `opening-jump-channel`, `key-exchange`, `authenticating`, `opening-session-channel`, `requesting-terminal` and `preparing-remote-program`. Each observed step also appears in the pane's event journal as `ssh-step`. Local and replay panes omit these SSH fields.

A connected SSH pane also reports `ssh.destination` and `ssh.jump_hosts`, ordered from the first jump toward the destination. Each peer contains `host`, `user`, `port`, the verified SHA256 `key_fingerprint`, and `host_key_alias` when configured. The `ssh` object is available only while that pane has an authenticated connection.

Each peer's `initial_algorithms` reports `key_exchange`, `host_key`, `cipher`, and the boolean `strict_key_exchange` from the first key exchange on that transport. `host_key` includes the SSH certificate algorithm suffix when a certificate was presented. Panes that share a transport report the same initial algorithms. Reopening the transport uses the next connection's negotiation.

Each peer's `authentication` reports the completed user authentication. The `method` is `none`, `password`, `keyboard-interactive`, or `public-key`. With `public-key`, `source` is `file` or `agent`, and `key_fingerprint` identifies the accepted user key. An OpenSSH user certificate adds `certificate_authority_fingerprint`. The optional `additional_methods` array lists successful password or keyboard-interactive steps in order. These fields describe the authenticated transport and are shared by all panes that use it.

While the pane has an active SSH transport, `inspect` reports `ssh_transport` with its numeric `id` and a sorted `pane_ids` list, including the inspected pane. Match it to `ssh_transport_id` in `list` to find other panes on the same authenticated connection across the process's windows. Independent logins have different IDs, even for the same profile. A fresh transport gets a new ID. These fields are omitted when the pane has no active SSH transport.

`mouse` contains the booleans `click`, `drag` and `motion`, and an `encoding` of `x10`, `utf8`, `sgr` or `sgr-pixels`. `keyboard.encoding` is `xterm`, `csi-u`, `win32` or `kitty`. `keyboard.flags` is the numeric kitty keyboard protocol bitmask, or zero for other encodings.

When xterm modified-key reporting or its alternate format is enabled, `keyboard.modify_other_keys` is the active level from 0 through 3 and `keyboard.format_other_keys` is `xterm-27` or `csi-u`.

## Running command output

`get-running-output --pane ID --start-token TOKEN` reads a bounded sample from
one active command. Take the token from `inspect.result.running_command.start_token`
or from the same pane's `command-started` event. The token is valid only for that
capture in the running w11term process. A capture can reuse its history ID after
an abandoned or repeated start marker. A new start has a new token. Use
`inspect` to find the current token after a replacement.

```powershell
$inspection = & $ctl --pid 12345 inspect --pane $pane | ConvertFrom-Json
$token = $inspection.result.running_command.start_token
if ($inspection.ok -and $token) {
    & $ctl --pid 12345 get-running-output --pane $pane --start-token $token
}
```

The reply includes `pane`, `id`, `connection_generation`, `start_token`, and
`state`. With `state: "ready"`, `text` contains up to six primary-screen rows
joined with LF, including the current unfinished line. An empty string means
there is no text in the sample. `earlier_omitted` marks rows outside the sample,
and a trailing ellipsis marks a shortened line. `"updating"` indicates an
uncommitted synchronized update. `"alternate-screen"` indicates that an
application is using the alternate screen. `"unavailable"` indicates that
the active capture's output range could not be located. Those three states
omit `text`. A finished, replaced, wrong-pane, or disconnected token returns
an error. Use `get-command` for a completed capture's retained output.

The read preserves focus and scroll position, including on background and
read-only panes. The equivalent pipe request is
`{"verb":"get-running-output","pane":2,"start_token":27}`.

`wait-running-output --pane ID --start-token TOKEN [--regex] [--match-case]
[--timeout SECONDS] [--] TEXT` waits for a completed logical line in that
same active command. It checks retained output first, then watches lines as
the parser receives them. Soft wraps are joined, and the shell's completion
marker makes the final line eligible even without a newline. Matching is a
case-insensitive literal search by default. It runs on parsed primary-screen
output independently of when a synchronized update is painted.

The timeout defaults to 30 seconds and accepts 1–3600. A match returns
`pane`, `id`, `connection_generation`, `start_token`, and the matched line
as `text` without its newline. If the capture ends without a match, is
replaced, or disconnects, the wait fails immediately. An already stale token
is rejected. Each scan can read at most 128 physical rows and 1,024 UTF-8 bytes
per logical line. Dropped or reflowed rows and overlong lines return an
incomplete-output error. Alternate-screen contents are excluded.

The pipe request uses `verb: "wait-running-output"` with `pane`,
`start_token`, and `pattern`. Optional fields are `regex`, `match_case`,
and `timeout_seconds`. It uses the same wait capacity as `wait-for-text` and
leaves focus, scroll position, and shell input in place.

`interrupt-command --pane ID --start-token TOKEN` sends Ctrl+C to that
observed running command on native SSH or local ConPTY. Take the token from
`inspect` or `command-started`. It works on a background pane and uses the
pane's negotiated keyboard encoding. The worker checks that the pane accepts
input, then the transport rechecks the queued interrupt against the client's
latest observed capture before writing. A newer interrupt request for the same
pane supersedes an older request still waiting to write.

The result contains `pane`, `start_token`, and `sent: true`. This confirms
the transport write. Use `wait-command --start-token` to check whether the
shell reports completion and which exit status it returns. Read-only panes,
authentication prompts, stale tokens, and disconnections produce errors.
The equivalent pipe request is
`{"verb":"interrupt-command","pane":2,"start_token":27}`.

## Completed shell commands

`list-commands --pane ID` reads the pane’s OSC 133 or OSC 633 command history. It
returns `pane`, `connection_generation`, `latest_id`, and a `commands` array,
newest first. Use `latest_id` as the wait cursor even if a filter or terminal
reset leaves the listing empty. It starts at zero. Each entry
contains `id`, `command`, `reported_directory`, `exit_code`, and `output_available`. The array is empty
until the shell marks a completed command. A missing command line or exit status
is JSON `null`. Listing transfers only this metadata and preserves keyboard focus
and the scroll position.

The command comes from the shell’s OSC 133 C `cmdline_url` field when
available, including whitespace and line breaks. Otherwise it comes from
visible text between the B and C markers.

Both `list-commands` and `get-command` include `reported_directory`. This is the
shell’s file URI from the start of that command, or JSON `null` if unavailable.
The value is the original shell report, including its authority and escapes.

Both commands also include `started_unix_ms`, `finished_unix_ms`, and
`duration_ms`. The timestamps use the Windows client's clock. Duration uses
monotonic time between the observed shell markers, through the next prompt
when it closes a capture without an explicit finish marker. Start time and
duration are JSON `null` when there is no command start marker.

Use `--status failed` for nonzero exit codes, `--status succeeded` for zero, or
`--status unknown` when the shell omitted the exit code. The default is `all`.
Filtered listings keep the original IDs, generation, latest ID, and newest-first order.

To get the latest command’s output, pass its ID and the generation from the same
listing. A generation of zero is valid.

```powershell
$historyReply = & $ctl --pid 12345 list-commands --pane $pane | ConvertFrom-Json
if (!$historyReply.ok) { throw $historyReply.error }
$history = $historyReply.result
if ($history.commands.Count -gt 0) {
    $id = $history.commands[0].id
    $reply = & $ctl --pid 12345 get-command --pane $pane --id $id --connection-generation $history.connection_generation | ConvertFrom-Json
    if (!$reply.ok) { throw $reply.error }
    if ($null -ne $reply.result.output_error) { throw $reply.result.output_error }
    $reply.result.output
}
```

To wait for a new completion, read the cursor before triggering the command.
`wait-command` returns the first completed capture after that cursor in the
same generation, with the same fields as `get-command`. A command that has
already finished since the cursor was read is returned immediately. The shell
exit code is in `result.exit_code`, and a successfully retrieved capture
returns CLI exit code zero even when the shell command failed.

```powershell
$cursor = & $ctl --pid 12345 list-commands --pane $pane | ConvertFrom-Json
if (!$cursor.ok) { throw $cursor.error }
# Trigger the shell command here.
$completion = & $ctl --pid 12345 wait-command --pane $pane --after-id $cursor.result.latest_id --connection-generation $cursor.result.connection_generation --timeout 60 | ConvertFrom-Json
if (!$completion.ok) { throw $completion.error }
$completion.result.exit_code
```

Add `--command TEXT` to wait for the first capture with that exact recorded
command line. Matching is case-sensitive and preserves spacing and line
breaks. Captures whose shell integration supplied no command line are skipped.
The cursor still advances past inspected captures, and a gap in uninspected
history remains an error.

To wait for a specific command, use the `start_token` from `inspect` or the
pane's `command-started` event.

```powershell
& $ctl --pid 12345 wait-command --pane $pane --start-token $token --timeout 60
```

This form returns the exact capture when the parser receives its completion
marker. If it already completed, the reply comes from retained history and
is available even after the shell exits. The result includes `start_token`
along with the usual command fields. Replacement, reset, disconnection before
completion, and expired history produce errors. Use `--start-token` on its own
as the command selector. It is exclusive with `--after-id`,
`--connection-generation`, and `--command`.

Use `--require-success` to make the CLI exit 0 for shell exit code zero,
2 for a nonzero or unknown shell exit status, and 1 if the wait or file save
fails. The JSON reply and any requested output file are available for exit 2.
This flag affects the CLI process and is independent of the pipe request.

Both forms default to 30 seconds and accept 1 to 3600 seconds. The cursor-based
wait fails if the pane closes, its SSH connection ends, or the next capture has already fallen
out of the retained history. After a gap or reconnect, take a new cursor with
`list-commands`. Use a pane dedicated to the script if it needs to attribute
the next completion to a particular input.

Add `--output-file PATH` to either `get-command` or `wait-command` to save
the captured text as UTF-8 without a BOM. The file preserves the capture's
line endings and trailing whitespace. The CLI resolves relative paths from
its current directory. The destination directory must already exist.

The reply still includes the command metadata and exit code. It sets
`output` to `null` and returns `output_file` with the absolute `path` and
`bytes` saved. An empty capture produces a zero-byte file. A failed or
unavailable capture leaves the destination untouched. Pass `--overwrite`
to replace an existing file with the complete new capture.

Direct pipe clients use these JSON requests.

```json
{"verb":"list-commands","pane":2}
{"verb":"list-commands","pane":2,"status":"failed"}
{"verb":"get-command","pane":2,"id":5,"connection_generation":1}
{"verb":"wait-command","pane":2,"after_id":5,"connection_generation":1,"timeout_seconds":60}
{"verb":"wait-command","pane":2,"after_id":5,"connection_generation":1,"command":"make test"}
{"verb":"wait-command","pane":2,"after_id":5,"connection_generation":1,"output_file":{"path":"C:\\Logs\\capture.txt"}}
{"verb":"wait-command-capture","pane":2,"start_token":27,"timeout_seconds":60}
```

For either command, direct pipe clients send an absolute `output_file.path`
and may set `output_file.overwrite` to `true`.

`get-command` returns `pane`, `connection_generation`, `id`, `command`,
`exit_code`, `output`, and `output_error`. Output preserves Unicode text,
joins soft wraps, and uses CRLF between hard lines. A successful capture sets
`output_error` to `null`. A silent command can have an empty output string and
`output_available: true` in the listing. If capture failed, the command still
has a record with `output: null` and an explanation in `output_error`.

Each pane retains up to 64 completed commands within a 4 MiB text budget.
Capture limits are 4,096 UTF-8 bytes per command line and 1 MiB per command’s
output. Keep the pane ID, command ID and connection generation together. A lookup
fails if its record has been evicted or its connection generation has changed.
After a reconnect, list again to see the new session’s history.

<a id="retained-ssh-group-runs"></a>

## SSH group rosters and runs

`list-ssh-groups` lists live SSH group tabs across the selected process. The
reply includes `pid` and a `windows` array with `window_id` and `groups` for
each window. A group has `tab_id`, `label`, and `runs` in submission order,
oldest first. A tab retains the eight newest runs. A group can have an empty
run list.

`list-saved-ssh-groups --window ID` reads the selected window's current
profile catalog from disk and returns `window_id` and `groups`, ordered by
saved name. Each group contains its exact `name`, the `members` in saved
order, and the effective saved roster `defaults`. A member has `profile`
and `available`. A group has `launchable: true` when its saved definition
passes launch validation. Otherwise, `launch_error` explains the first
problem. Discovery leaves the window and saved catalog in place.

`open-ssh-group --window ID --name NAME` opens a fresh copy of a saved SSH
group in an existing window from that list. Pass the exact saved name as one
argument. The new SSH tabs follow the saved member order. The group applies
its saved defaults, including the parallel login limit, and starts with all
of its members checked. The current tab and window focus
stay in place. An unknown group or a group with an unavailable profile is
rejected before any tabs or sessions are created.

The reply contains `window_id`, the new group's `tab_id`, its saved `name`,
and a `members` array in saved order. Each member has `profile`, `pane_id`,
and `tab_id`. A successful reply acknowledges the launch while connections
may still be queued or in progress. Pass the group ID to
`wait-ssh-group-ready` to wait for verified prompts. If the launch is
unacknowledged, check `list-ssh-groups` before retrying.

`get-ssh-group-roster --window ID --tab ID` returns the current roster for
those IDs. The reply includes `window_id`, `tab_id`, and a `roster` object
with `schema_version: 1`, `label`, `scope` (`group` or `window`), `sort`,
`filter`, and `hosts` in roster sort order. The filter reports its `query`,
`wildcards`, `state`, and any pattern `error`. Each in-scope host has
`pane_id`, `name`, `profile`, `address`, `state`, `connected`, `checked`,
and `shown`. A checked host hidden by the current filter stays in the reply
with `shown: false`. Ready hosts include `prompt_verification` as
`shell_markers` or `pattern`. Reconnecting hosts can carry `reconnect_attempt`.
Host states are `ready`, `waiting`, `unverified`, `queued`, `connecting`,
`reconnecting`, `disconnected`, `failed`, `stopped`, `host_key`,
`authentication`, `read_only`, and `alternate`. The state filter also
supports `all`.

The `counts` object includes `hosts`, `shown`, `checked`, `checked_shown`,
and `ready_checked_shown`. `selection_ready` reports whether the shown
checked hosts meet the group's `require_all_ready` prompt rule. The Send
button also depends on the current draft and review.
The query leaves the group's selection and window focus in place.

`wait-ssh-group-ready --window ID --tab ID [--pane ID ...] [--timeout SECONDS]`
returns the roster once every selected recipient has a verified prompt.
With repeated `--pane` flags, it waits for those distinct panes in the
group's roster scope even if the UI hides or unchecks them. A pane outside
the scope, including one that leaves during the wait, returns an error.
The explicit reply adds `ready_pane_ids` in request order. The roster's
selection fields still describe its current UI state.

Omit `--pane` to wait for the initially checked, shown hosts. An empty or
changed checked-and-shown set returns an error in this mode. Both modes
require every requested prompt, including when the group allows partial
sends. A closed tab or timeout returns an error. The wait defaults to
30 seconds, accepts 1 through 3600, and works in the background.

Each run summary has `run_id`, `label`, `kind` (`command` or `environment`),
`recipient_count`, `settled_count`, and `outcomes` counts for `waiting`,
`running`, `succeeded`, `attention`, and `skipped`. Both commands can read a
background tab or window without changing selection or keyboard focus.

`submit-ssh-group-command --window ID --tab ID --pane ID [--pane ID ...] [--dry-run] [--canary PANE] [--parallel N] [--interval-ms MS] [--queue-expiry-ms MS] [--on-failure stop|continue] [--expect-output TEXT [--regex] [--match-case]] [--record-output PATH] [--] COMMAND`
sends one shell line of up to 4,096 UTF-8 bytes to explicitly named panes.
Pass the command as one quoted argument to preserve spaces. Use `--` before
a command beginning with two hyphens. The panes must be distinct, belong
to the group's current roster scope, and each have a fresh verified prompt.
The host filter and checkboxes in the group UI have no bearing on this
explicit recipient list. If a recipient fails validation, the entire request
is rejected before dispatch. If a prompt changes after validation, the
affected pane's run result records that delivery was withheld.

The group applies its configured queue policy. Use `--canary PANE` to
select the canary for this run. The pane must be one of the named
recipients, and there must be at least one other ready recipient. If
`--canary` is omitted, the group uses its configured canary, if any, with
the same recipient requirements. Approve the canary after a verified
successful exit to release the remaining hosts. The
response returns `window_id`, `tab_id`, and a new `run_id` as soon as the
run is accepted. Use the run ID below to confirm delivery, check shell exit
status, or follow output. A background submission preserves the window focus,
group draft, and roster selection. If the submission is unacknowledged, check
the retained runs before retrying.

Use `--parallel N` to set the maximum number of in-flight recipients for
this submission. `N` must be a positive integer. In-flight commands hold
their slots until execution settles. Other queue settings follow the
group defaults and any explicit options in the same submission. Later
runs keep their configured parallel limit.

Use `--interval-ms MS` to set the minimum time between dispatches for
this submission. The first eligible dispatch has no initial delay.
`0` disables pacing for the run. Omit the option to use the group's
configured interval.

Use `--queue-expiry-ms MS` to expire unsent recipients after that many
milliseconds from dispatch setup. The deadline continues while the queue
is paused or waiting for canary approval. Already dispatched commands
continue normally. Use `0` for no expiry or omit the option to use the
group's configured deadline.

Use `--on-failure stop` to withhold unsent commands after an unsuccessful
host. If the run has no parallel limit or pacing interval, it dispatches
one host at a time. `--on-failure continue` allows the queue to proceed
after an ordinary recipient fails. A canary still requires a verified
successful exit and approval under either policy. Omitting the option
keeps the group's policy.

Use `--dry-run` to validate the same request and return `window_id`,
`tab_id`, and a `plan` containing the command, effective dispatch policy,
and recipients in planned dispatch order. When a canary is configured
for the run, it is first, followed by the other panes in argument order.
A preview creates no run, recording, or output alert and sends no bytes.
The real submission revalidates the live roster and prompt state and
opens any requested recording file.

Use `--expect-output TEXT` to check captured lines from each named pane.
The check is armed before dispatch and defaults to case-insensitive literal
matching. `--regex` and `--match-case` change how the pattern is matched.
The pattern must be nonempty and at most 4,096 UTF-8 bytes. An invalid
pattern rejects the request before any command is sent. The settled run
reports each pane as matched, missing, or unverified. Missing or unverified
output also appears in the Attention inbox.

Use `--record-output PATH` to create a UTF-8 transcript with host labels,
output, and final outcomes. The CLI resolves relative paths from its working
directory. Raw requests require an absolute path. The destination must be a
new file in an existing directory. If preparation fails, no command is sent.
If the request expires or a recipient changes after the file is prepared,
the file may contain only `[No command was sent]`.

Run replies include an optional `recording` object for CLI-started recordings.
It has `path`, `state` (`opening`, `recording`, `saved`, or `failed`), and
`error`, which is null unless the writer failed. The recorder starts before
dispatch and writes through settlement. Stopping it from the GUI or closing
or retiring the run ends capture early and marks that in the file footer.

`get-ssh-group-run --window ID --tab ID --run ID` takes the three IDs from
the listing. An evicted run or closed tab or window returns a lookup error.
The reply includes `window_id`, `tab_id`, and a `run` object with
`schema_version: 1`, `summary`, `command`, `dispatch_policy`,
`queue_activity`, and `recipients` in the original dispatch order. Recipient
rows carry the same pane identity, delivery, execution, and interrupt
statuses as saved group reports. Environment assignments appear in their
recipient rows.

`get-ssh-group-retry-plan --window ID --tab ID --run ID` returns the
original shell command and recipients whose delivery was rejected before
sending. Each row includes its original rejection reason, current
readiness, and any blocker. `plan.ready_pane_ids` lists the currently
eligible recipients in submission order. The plan excludes sent,
unconfirmed, and deliberately skipped commands. It leaves the group
draft and terminal sessions alone. Pass the reviewed command and panes
to `submit-ssh-group-command`, which rechecks current prompt readiness
and applies the effective queue policy.

`retry-ssh-group-run --window ID --tab ID --run ID --pane ID [--pane ID ...]`
submits the retained run's original command to explicitly chosen unsent
recipients. It rejects the entire request if any pane is ineligible. Each
pane must still be in the group, on the original SSH destination, and at
a fresh prompt. A source run with an unapproved canary is ineligible.
The new run uses the current group queue policy and returns
`source_run_id` and `run_id`.

Each source pane can be retried once with this command. Its plan row
reports `retry_run_id` after acceptance. Check that field if a retry was
unacknowledged. If the new run also withholds a host, inspect the new run's
retry plan.

`interrupt-ssh-group-command --window ID --tab ID --run ID --pane ID`
requests Ctrl+C for one verified running command on a named recipient.
The target must still be running the command from that retained run on
the same direct SSH connection. The terminal rechecks the connection,
command identity, and input mode immediately before the write. The
action works in background tabs and uses the explicit pane ID regardless
of the roster selection. It returns a run snapshot. Check the recipient's
`interrupt.state` for `pending`, `sent`, `rejected`, or `unconfirmed`.
A `sent` receipt confirms delivery to the SSH channel. The command's
final outcome is recorded separately under `execution`.

When a per-recipient expected-output check resolves, `run.output_expectation`
contains its `query`, `regex`, and `match_case` settings. Each checked
recipient has its own `output_expectation` field with the value `matched`,
`missing`, or `unverified`. Missing means capture completed with adequate
coverage and no match. Unverified means delivery or capture could not establish
whether the text appeared. Saved JSON reports use the same fields, with the
check settings at the document root. An unresolved check has no result fields.

The detail reply carries metadata. To read a completed recipient's retained
output, use `get-command` with its `pane_id` and the
`execution.command_id` and `execution.connection_generation` in the reply.
The usual pane history retention rules apply.

`ssh-group-queue --window ID --tab ID --run ID pause|resume|stop`
controls unsent commands in a retained run. Pause holds further dispatch.
Resume allows dispatch under the run's existing limits and prompt checks.
Stop marks the remaining queued commands cancelled before sending.
Commands already dispatched to an SSH pane continue through delivery
and execution. Pause and resume require an
ungated queue. Stop is available while a canary is pending. Actions on
an expired queue or a run with no unsent commands return an error.

`ssh-group-queue --window ID --tab ID --run ID approve-canary --pane ID`
releases the run's canary gate after that pane reports a verified exit
code of zero. Read `queue.canary_pane_id` from the same run before
approving. Approval requires the exact canary pane and a queue in
`canary_approval`. Remaining recipients still pass the existing prompt
checks, dispatch limits, and queue deadline.

`ssh-group-queue --window ID --tab ID --run ID skip --pane ID` withholds
one unsent recipient and reports its delivery state as `skipped`.
The rest of the queue keeps its current dispatch policy. The pane must
belong to the run and still be queued. An active canary cannot be skipped.

The action returns the same detail reply as `get-ssh-group-run`. When a
run uses a queue, the reply has a `queue` object with `unsent` and `state`.
The state is `dispatching`, `paused`, `canary_running`, `canary_approval`, or
`drained`. Both canary states include `canary_pane_id`. Drained means there
are no unsent recipients. Check
`queue_activity` and the recipient states for dispatch and cancellation
outcomes. Queue actions work in a background tab or window.

`save-ssh-group-report --window ID --tab ID --run ID [--format json|text] [--overwrite] [--] PATH`
saves the same full-run report available from the group results view. JSON
is the default. Both formats include all recipients, their current status,
and available captured output. Use `wait-ssh-group-run` before exporting
if you need final outcomes. An unknown or evicted run returns an error
before any file is created.

The CLI resolves relative paths from its working directory. Raw requests
require an absolute path. A completed report is published in one operation
and requires `--overwrite` to replace an existing file. The reply returns
`window_id`, `tab_id`, `run_id`, `path`, and `format`. The export works while
the group is in the background.

`wait-ssh-group-run --window ID --tab ID --run ID [--timeout SECONDS] [--require-success]`
returns the same detail reply once all recipients settle, including the result
of any active per-recipient expected-output check. For CLI-started recordings,
it also waits for the file writer's final status. It handles running, paused,
and canary-gated queues in the background. The timeout defaults to
30 seconds and accepts 1 through 3600. A settled run returns immediately.
Timeout, a closed window or tab, or an evicted run returns an error. A timeout
leaves the original run in progress.

With `--require-success`, the CLI exits **0** when every recipient reports a
verified exit code of zero and every configured output check matches. It
exits **2** when the run settles with any other recipient outcome, including
a skipped host or missing exit status. In either case the JSON reply still
has `ok: true` and the full run. Request errors and timeouts exit **1**.
Without the flag, any successfully retrieved run exits **0**.

`wait-ssh-group-approval --window ID --tab ID --run ID [--timeout SECONDS]`
returns the run snapshot when the canary reports a verified zero exit and
unsent hosts are waiting for approval. Use `run.queue.canary_pane_id` with
`ssh-group-queue ... approve-canary --pane ID` after reviewing the result.
The wait leaves the gate in place and works from the background. A failed
canary, drained or expired queue, previously approved run, or retired
run returns an error. The timeout defaults to 30 seconds and accepts 1
through 3600.

`wait-ssh-group-output --window ID --tab ID --run ID [--pane ID] [--after-line ID] [--regex] [--match-case] [--timeout SECONDS] [--] TEXT`
returns the first matching line from the selected run. It searches the retained
combined feed in arrival order, then listens for new output. With `--pane ID`,
it searches that recipient's retained feed and subsequent lines from that
pane. It uses case-insensitive literal matching by default. Patterns are
limited to 4,096 UTF-8 bytes. The timeout defaults to 30 seconds and accepts
1 through 3600. Use `--` before a pattern beginning with two hyphens.

The result includes `window_id`, `tab_id`, `run_id`, and a `line` with
`line_id`, `pane_id`, `label`, `address`, `text`, `elapsed_ms`, and
`shortened`. The line ID identifies its arrival order within the run.
Elapsed time uses the local run clock. Matching uses the available marked
output, including any truncation recorded in `shortened`. The view can
be paused or in a background tab. A timeout, settled run without a match,
retired run, or overrun of the incoming buffer returns a distinct error.

Pass the reply's `line_id` as `--after-line ID` to continue after that line
in the same run. Use 0 to start before the first line. The cursor uses the
run's line IDs, including when the wait is scoped to one pane. A cursor
beyond the latest line returns an error. If any newer lines from the
selected feed have already left retention, the wait reports the last
missing line ID as a history gap. Omit the cursor to search the available
retained feed from its oldest line.

`get-ssh-group-output --window ID --tab ID --run ID [--pane ID] [--after-line ID] [--limit LINES]`
returns a snapshot of retained lines in arrival order. The pane selector and
cursor use the same run scope as the wait command. The limit defaults to 128
and accepts 1 through 128. A page can be empty.

The result contains the window, tab, and run IDs, `pane_id` (null for the
combined feed), and a `lines` array with the same line fields as the wait
command. It also includes these fields.

- `settled` indicates that the run has finished tracking all recipients.
- `latest_line` is the newest line ID captured in the run, or 0 before any output.
- `evicted_through` is the last line ID lost from the selected feed, or 0.
- `gap` is true when lines after the supplied cursor have been evicted. With no cursor, it reports whether the beginning of the selected feed was lost. The page still contains the available lines.
- `has_more` means the snapshot has more retained lines after this page.
- `next_after_line` is the last returned line ID when more retained lines are available. Once the page drains the feed, it advances to `latest_line`, including when the selected pane was quiet.

Pass `next_after_line` to the same read command until `has_more` is false.
You can then use that cursor in a wait command for the next marker.

`follow-ssh-group-output --window ID --tab ID --run ID [--pane ID] [--after-line ID] [--timeout SECONDS]`
emits retained and live output from one run over a single pipe connection.
Each line of stdout is a standard JSON reply with the window, tab, and run
IDs. A successful stream can include these records.

- `kind: "line"` includes the same `line` object as the wait command.
- `kind: "gap"` precedes available lines if the beginning of the selected feed was evicted and no cursor was supplied. It includes `pane_id` and `evicted_through`.
- `kind: "settled"` is the last success record. It includes `pane_id` and `next_after_line`, the newest captured run line considered by the stream.

Supplying a cursor requires complete retained output after that cursor.
A missing interval, retired run, stream overrun, or timeout ends the stream
with an error reply and exit code 1. The timeout defaults to 30 seconds
and accepts 1 through 3600. Closing the pipe ends the subscription. A
settled record describes the end of output tracking. Read the run result
for recipient exit statuses.

```powershell
w11term-cli --pid 12345 list-ssh-groups
w11term-cli --pid 12345 list-saved-ssh-groups --window 1
w11term-cli --pid 12345 open-ssh-group --window 1 --name "Fleet Ops"
w11term-cli --pid 12345 get-ssh-group-roster --window 1 --tab 9
w11term-cli --pid 12345 wait-ssh-group-ready --window 1 --tab 9 --timeout 60
w11term-cli --pid 12345 wait-ssh-group-ready --window 1 --tab 9 --pane 17 --pane 19 --timeout 60
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 -- "uname -a"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --canary 19 -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --canary 19 --dry-run -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --parallel 1 -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --interval-ms 1200 -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --queue-expiry-ms 1800 -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --on-failure stop -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --expect-output '^READY$' --regex --match-case -- "check-service"
w11term-cli --pid 12345 submit-ssh-group-command --window 1 --tab 9 --pane 17 --pane 19 --record-output "service check.txt" -- "check-service"
w11term-cli --pid 12345 get-ssh-group-run --window 1 --tab 9 --run 3
w11term-cli --pid 12345 ssh-group-queue --window 1 --tab 9 --run 3 pause
w11term-cli --pid 12345 ssh-group-queue --window 1 --tab 9 --run 3 approve-canary --pane 17
w11term-cli --pid 12345 ssh-group-queue --window 1 --tab 9 --run 3 skip --pane 19
w11term-cli --pid 12345 interrupt-ssh-group-command --window 1 --tab 9 --run 3 --pane 17
w11term-cli --pid 12345 get-ssh-group-retry-plan --window 1 --tab 9 --run 3
w11term-cli --pid 12345 retry-ssh-group-run --window 1 --tab 9 --run 3 --pane 19
w11term-cli --pid 12345 save-ssh-group-report --window 1 --tab 9 --run 3 "group report.json"
w11term-cli --pid 12345 wait-ssh-group-run --window 1 --tab 9 --run 3 --timeout 60
w11term-cli --pid 12345 wait-ssh-group-run --window 1 --tab 9 --run 3 --require-success
w11term-cli --pid 12345 wait-ssh-group-approval --window 1 --tab 9 --run 3 --timeout 60
w11term-cli --pid 12345 wait-ssh-group-output --window 1 --tab 9 --run 3 --pane 17 --regex '^ready for traffic$'
w11term-cli --pid 12345 wait-ssh-group-output --window 1 --tab 9 --run 3 --pane 17 --after-line 42 --regex '^ready for traffic$'
w11term-cli --pid 12345 get-ssh-group-output --window 1 --tab 9 --run 3 --pane 17 --after-line 42 --limit 20
w11term-cli --pid 12345 follow-ssh-group-output --window 1 --tab 9 --run 3 --after-line 42 --timeout 120
```

Direct pipe clients use these JSON requests.

```json
{"verb":"list-ssh-groups"}
{"verb":"list-saved-ssh-groups","window_id":1}
{"verb":"open-ssh-group","window_id":1,"name":"Fleet Ops"}
{"verb":"get-ssh-group-roster","window_id":1,"tab_id":9}
{"verb":"wait-ssh-group-ready","window_id":1,"tab_id":9,"timeout_seconds":60}
{"verb":"wait-ssh-group-ready","window_id":1,"tab_id":9,"pane_ids":[17,19],"timeout_seconds":60}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"command":"uname -a"}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"canary_pane_id":19,"command":"check-service"}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"canary_pane_id":19,"command":"check-service","dry_run":true}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"parallel_limit":1,"command":"check-service"}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"interval_ms":1200,"command":"check-service"}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"queue_expiry_ms":1800,"command":"check-service"}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"stop_on_failure":true,"command":"check-service"}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"command":"check-service","expect_output":{"query":"^READY$","regex":true,"match_case":true}}
{"verb":"submit-ssh-group-command","window_id":1,"tab_id":9,"pane_ids":[17,19],"command":"check-service","record_output":"C:\\Reports\\service check.txt"}
{"verb":"get-ssh-group-run","window_id":1,"tab_id":9,"run_id":3}
{"verb":"ssh-group-queue","window_id":1,"tab_id":9,"run_id":3,"action":"pause"}
{"verb":"ssh-group-queue","window_id":1,"tab_id":9,"run_id":3,"action":{"approve_canary":{"pane_id":17}}}
{"verb":"ssh-group-queue","window_id":1,"tab_id":9,"run_id":3,"action":{"skip":{"pane_id":19}}}
{"verb":"interrupt-ssh-group-command","window_id":1,"tab_id":9,"run_id":3,"pane_id":17}
{"verb":"get-ssh-group-retry-plan","window_id":1,"tab_id":9,"run_id":3}
{"verb":"retry-ssh-group-run","window_id":1,"tab_id":9,"run_id":3,"pane_ids":[19]}
{"verb":"save-ssh-group-report","window_id":1,"tab_id":9,"run_id":3,"path":"C:\\Reports\\group report.json","format":"json","overwrite":false}
{"verb":"wait-ssh-group-run","window_id":1,"tab_id":9,"run_id":3,"timeout_seconds":60}
{"verb":"wait-ssh-group-approval","window_id":1,"tab_id":9,"run_id":3,"timeout_seconds":60}
{"verb":"wait-ssh-group-output","window_id":1,"tab_id":9,"run_id":3,"pane":17,"pattern":"^ready for traffic$","regex":true}
{"verb":"wait-ssh-group-output","window_id":1,"tab_id":9,"run_id":3,"pane":17,"after_line":42,"pattern":"^ready for traffic$","regex":true}
{"verb":"get-ssh-group-output","window_id":1,"tab_id":9,"run_id":3,"pane":17,"after_line":42,"limit":20}
{"verb":"follow-ssh-group-output","window_id":1,"tab_id":9,"run_id":3,"after_line":42,"timeout_seconds":120}
```

## Waiting, geometry, tabs, and events

`find-text` searches each selected pane’s retained scrollback and visible screen once. Use `--pane ID` for one pane or `--all-panes` for every pane in the process, across all its windows. The default is a case-insensitive Unicode literal. Use `--regex` for Rust regex syntax and `--match-case` to select case-sensitive matching. Both modes join soft wraps and stop at hard line breaks. Patterns can be up to 4,096 UTF-8 bytes. Use `--` before a pattern beginning with two hyphens. An alternate-screen pane searches its current screen.

The JSON `panes` array is sorted by pane ID. Each row includes `pane`, `tab_id`, `window_id`, `count`, `truncated`, and `first`. A match includes a plain-text `excerpt`, a stable buffer `row` and a zero-based cell `column`. An empty result has a count of zero and `first: null`. A count of 10,000 with `truncated: true` means there are additional matches. The coordinates describe the buffer at search time and can go stale after output, resize or reconnection.

Successful regex rows also include `incomplete`. It is true when the scan skipped a logical line larger than 64 KiB. An invalid pattern fails the entire request before any panes are searched.

All-pane results retain a row with an `error` for a closed, timed-out or mid-synchronized-frame pane. Other panes still return their results. A failure for an explicitly selected pane returns a CLI error. Searching preserves focus, scroll positions and any open Find queries.

```powershell
& $ctl --pid 12345 wait-for-text --pane $pane --regex --timeout 15 'fps=\d+'
& $ctl --pid 12345 resize --pane $pane --cols 170 --rows 48
$opened = & $ctl --pid 12345 open --profile play-6792 --cols 170 --rows 48 | ConvertFrom-Json
& $ctl --pid 12345 events --pane $pane --since 0
& $ctl --pid 12345 events --pane $pane --since 42 --timeout 30
```

`wait-for-text` waits on published live-screen changes; it does not poll or move
the user's scrollback. Patterns are literal UTF-8 unless `--regex` is supplied.
Regex uses Rust's regex syntax, is capped at 4096 pattern bytes and bounded compiled
size, and can match across LF-separated rows. It does not search scrollback or OCR
images. Timeout defaults to 30 seconds, range 1–3600; no match is a JSON error.
Success returns `pane`, `matched`, `generation`, `cols`, `rows`, and matching screen
`text`. It observes screen state, not every transient byte that may have appeared
between snapshots. Closing the target pane ends a pending text wait with
an error.

Pass `--screenshot PATH` to save the exact matched frame as a PNG, even if the
pane has already repainted. The reply includes `screenshot` with the absolute
`path`, `width`, and `height` in pixels. The same file replacement and rendering
rules as `screenshot` apply. A text match timeout leaves the destination
untouched. If the text matches but saving fails, the request returns a JSON error.

Pass `--after-frame N` to consider only committed frames with a generation
greater than `N`. Use the `generation` from `get-text` or a previous match
for the same process and pane. A cursor ahead of the pane's current frame is an
error. A later frame can satisfy the pattern with text that was also visible in
the baseline. Synchronized output becomes eligible when its frame commits.

Pass `--connection-generation N` to match frames from a particular live SSH
session or local process. Use the generation from `list` or
`wait-ssh-connected` with the same process and pane ID. A scoped wait fails as
soon as that connection ends or changes. On success, the reply includes
`connection_generation` as well as the frame's `generation`. Use the regular
unscoped wait to observe a pane across reconnects or to read its final screen.
`get-text` returns both generations from the same screen snapshot.

```powershell
$screen = (& $ctl --pid 12345 get-text --pane $pane | ConvertFrom-Json).result
& $ctl --pid 12345 wait-for-text --pane $pane --connection-generation $screen.connection_generation --after-frame $screen.generation --screenshot .\cockpit.png --regex 'fps=\d+'
```

`resize` accepts the launch grid ranges. In a visible split it adjusts available dividers to reach the requested cell grid. Unzoom the focused pane before resizing it. A dimension fixed by the tab bounds, or a grid that leaves too little space for a sibling, produces a geometry error. The command keeps keyboard focus on its current pane.

For a single pane it resizes an active pane's window or a
background pane's PTY without selecting the pane or activating the window. Changes
to the active grid require leaving fullscreen first. A grid that cannot fit the
current display/window constraints fails clearly; geometry may
already have changed. The next user tab activation uses the shared window's grid.
For a visible pane, success means the measured viewport and terminal grid both
match the request. For a background pane, success means its terminal model
accepted the new grid. A remote TUI can redraw afterward.

`open` creates an independent tab in the existing process from a saved profile.
Without `--activate`, the current tab stays selected. `--activate` selects the new
tab inside the app; it does not request OS keyboard focus. Omitted dimensions use
the current window grid. Replies return `pane`, `cols`, `rows`, and `active`. New
tabs may still be connecting or awaiting a native trust/authentication prompt;
scripts can inspect `list`. No command starts a new GUI process.

`close-pane --pane ID` closes one terminal pane and acknowledges after it has
been removed from `list`. It can close a background tab without selecting it, or
remove a split while keeping a surviving active pane selected. The reply contains
`pane` and `tab_closed`, which is true when the pane was alone in its tab. A
single-pane tab must be unpinned and have another tab in the same window. Closing
discards the pane's scrollback and shuts down its local process or SSH shell
channel. As with closing through the UI, an eligible closed layout can be reopened
with a fresh session. Use the pane ID returned by `open` to clean up a script's tab.

`wait-ssh-connected --pane ID [--after-generation N] [--timeout SECONDS]`
waits until an existing SSH pane reports a successful connection. The reply
contains `pane` and `connection_generation`. An already connected pane returns
immediately unless its generation is at or below `--after-generation`.
For an automatic reconnect or a manual `restart`, use the previous generation
from the same pane. A settled disconnect, a failed or stopped login, or a closed
pane returns an error. A pending login or scheduled retry can continue until the
timeout, which defaults to 30 seconds and accepts 1–3600 seconds. Use
`wait-for-text` separately when a script needs a specific remote shell prompt.

```powershell
& $ctl --pid 12345 wait-ssh-connected --pane 2 --after-generation 1 --timeout 60
```

The same pipe request is
`{"verb":"wait-ssh-connected","pane":2,"after_generation":1,"timeout_seconds":60}`.

`events` returns `{"pane":1,"journal":{"events":[...],"next":42,"oldest":1,"lost":false},"restart_token":null}`.
Each event has `sequence`, Unix `timestamp_ms`, `kind`, and JSON `data`. Use the
returned `next` for the next request. `--timeout` long-polls until an event arrives
(default 0, range 0–3600). Cursors are per pane and process; a future cursor fails.
The journal retains 4096 entries and truncates data larger than 4096 serialized
bytes. `lost=true` reports eviction. Events include `bell`, `title`, `notification`,
`sound`, connection/reconnection, assets, and recording. A bell event does not imply
audible playback; inspect the `sound` outcome. See [the game protocol](docs/game-client-protocol.md).
The CLI allows the requested wait timeout plus 15 seconds for these replies.

Use `--kind KIND` to wait for an exact, case-sensitive kind. For example, after
saving the pane's current `journal.next` and triggering a bell, run
`events --pane 1 --since 42 --kind bell --timeout 15`. Unrelated events do not
complete the wait. Use `--kind sound` and check `data.outcome` for `played`
when playback is what the test needs to verify. With no match at the deadline,
the reply has an empty `journal.events` array. An unread history gap returns
`lost=true` immediately, with any matching retained events.

The cursor always follows the full pane journal and advances past skipped
events. Keep separate cursors if you also need to read those skipped kinds.
Closing the target pane ends a pending event wait with an error.

For SSH panes the reply also includes `ssh_connection`, an object with a
`phase` of `queued`, `connecting`, `connected`, `reconnecting`, `disconnected`,
`stopped`, or `failed`. Reconnecting includes an `attempt` number for an
automatic retry and `null` for an explicit restart. A queued pane is waiting for
a slot in a group launch. Failed includes a short
`reason`. The journal adds an `ssh-connection` event on each phase change
with the same data shape. The connection snapshot and restart token describe
the pane at reply time, including when recorded transitions have aged out.

The `reconnecting` journal event records the scheduled wait's `attempt`,
`delay_seconds`, `retry_remaining_seconds`, and `will_retry`. The two time
values round up to whole seconds. When `will_retry` is false, the wait ends
at the retry deadline and the client starts no further automatic attempt.

While a direct SSH login or retry is pending, `list` and `events` also include
`ssh_attempt_token` for its pane. The `ssh-attempt` journal event carries
`attempt_token` and a `pending` flag whenever the token is offered or retired.
Use the token with the same process ID and pane ID.

```powershell
& $ctl --pid 12345 stop-ssh-attempt --pane 2 --attempt-token 4
```

An accepted stop returns `pane`, `attempt_token`, and `accepted: true`. Wait
for the pane's `stopped` phase or a `restart_token` before restarting it. A
retired token returns an error. The pane, its history, and desktop focus stay in place.

During an automatic retry wait for a direct SSH pane, `list` and `events`
include `ssh_retry_token`. Use it to start that specific retry early.

```powershell
& $ctl --pid 12345 retry-ssh-now --pane 2 --retry-token 3
```

A successful reply includes `pane`, `retry_token`, and `accepted: true`.
The `ssh-retry-wait` journal event records when each token becomes available
or retires. A successful early request retires the token with `requested: true`.
Each wait has its own token. The retry window and input behavior are the same
as for a scheduled retry.

To end a connected direct SSH shell, use the `connection_generation` from
`list` for the same process and pane.

```powershell
& $ctl --pid 12345 disconnect-ssh-shell --pane 2 --connection-generation 4
```

The reply includes `pane`, `connection_generation`, and `accepted: true`.
The pane journals `ssh-disconnect-requested` with that generation. Wait for
its `restart_token` with `wait-pane-exit` before starting a fresh shell. A
stale generation is rejected. Deliberate disconnect leaves the pane, scrollback, and focus in
place and skips automatic reconnect. Other shells on the same authenticated
transport can continue.

A live shell's integration start marker adds a `command-started` event to the pane's
journal. Its data contains `id`, `connection_generation`, `start_token`, and
`started_unix_ms` from the first observed start. Wait for it with
`events --pane ID --since CURSOR --kind command-started --timeout SECONDS`.
The journal records starts even when a command starts and finishes in one
read. Duplicate markers during the same capture produce one event. The ID
identifies the completed capture in that generation if the shell finishes it.
If the capture is abandoned before completion, the next command can reuse
that ID. Event data contains metadata only.

A command completion watch armed in the GUI adds one `command-finished` event to
the originating pane. Its data is `{"exit_code":17}` for a reported status or
`{"exit_code":null}` when the shell supplied none. Command lines and output stay
out of this event.

With **Notify for long commands** on, the same event also includes integer
`duration_ms` for commands that ran for at least 10 seconds. If the one-shot
watch is also armed, that completion produces a single event.

When an SFTP drawer transfer or batch ends, the originating pane records one
`sftp-transfer` event. The data includes `transfer_id`, `outcome`, the
`message` describing the result, and a `background` flag. Results with `background: true` also appear
in the Attention inbox.

When a direct SSH shell or local program finishes, a restartable pane emits
`pane-exited` with `{"exit_token":1}`. The top-level `restart_token` in the
events result identifies its current exit, even if the event has aged out.
Use that token with the same process and pane ID to restart the program in
place. Each exit can be claimed once. Live and reconnecting panes have a
null token. The restart reply acknowledges the request. Use
`wait-ssh-connected` for the new SSH generation or `wait-for-text` for an
application prompt.

`wait-pane-exit --pane ID [--after-exit-token N] [--timeout SECONDS]` waits for
an SSH session or local program to end and publish its final screen.
The reply contains `pane`, `exit_token`, and `restart_token`, plus the final
`ssh_connection` phase for SSH panes. A `null` restart token means the
program cannot restart in that pane. An already ended pane returns immediately
unless its exit token is at or below `--after-exit-token`. That option lets
scripts wait for a later exit after a restart. Automatic SSH retries keep the
wait pending. Replay and demo panes, closed panes, and a request for a newer
exit from a pane that cannot restart return errors. The timeout defaults to
30 seconds and accepts 1–3600 seconds.

```powershell
$exitReply = & $ctl --pid 12345 wait-pane-exit --pane $pane --timeout 60 | ConvertFrom-Json
if ($exitReply.ok -and $null -ne $exitReply.result.restart_token) {
    & $ctl --pid 12345 restart --pane $pane --exit-token $exitReply.result.restart_token
}
```

```json
{"verb":"wait-for-text","pane":1,"pattern":"fps=\\d+","regex":true,"timeout_seconds":15}
{"verb":"wait-for-text","pane":1,"connection_generation":2,"after_frame":35,"screenshot_path":"C:\\tests\\ready.png","pattern":"ready","timeout_seconds":15}
{"verb":"resize","pane":1,"cols":170,"rows":48}
{"verb":"open","profile":"play-6792","cols":170,"rows":48,"activate":false}
{"verb":"close-pane","pane":3}
{"verb":"events","pane":1,"since":42,"kind":"bell","timeout_seconds":30}
{"verb":"wait-pane-exit","pane":1,"after_exit_token":1,"timeout_seconds":30}
{"verb":"restart","pane":1,"exit_token":1}
```

## VT recording

The command palette and the tab menu’s **Recordings** submenu share the
recorder with the control pipe. **Start timed recording…** selects a new
destination for the current SSH or local shell pane. The capture stays with
that pane if another one becomes active while the picker is open. Its recording
indicator shows the active path, including a capture started from the CLI.
**Stop recording** finishes that pane’s capture. If the recorder hits a size
limit or a disk error, the window’s notification inbox shows the reason.

**Open recording…** opens a `.vt` file in a new tab and selects timed
playback when its `.vt.jsonl` index is present. A raw capture uses its saved
initial grid when available, or the measured pane size. Both modes in this
viewer suppress historical desktop clipboard, sound and notification requests.

```powershell
& $ctl --pid 12345 record --pane $pane .\bug.vt
# Reproduce the problem, or request a repaint from the remote application.
& $ctl --pid 12345 record --pane $pane --stop
.\target\release\w11term.exe --replay .\bug.vt
```

Recordings contain the incoming terminal output, including image data and escape
sequences, starting at the moment the recorder opens. A server may echo typed
text or display sensitive information. Review the output before sharing it.
When starting mid-session, request a full repaint from the application. The
capture begins with a fresh parser on replay, and SSH reconnects do not
reconstruct a previous session's state.

Use a timed recording for a bug involving animation, synchronized output, or a
window resize.

```powershell
& $ctl --pid 12345 record --pane $pane --timed .\cockpit.vt
# Reproduce the bug, including any pane resizes.
& $ctl --pid 12345 record --pane $pane --stop
.\target\release\w11term.exe --replay-timed .\cockpit.vt
```

| File | Contents |
|---|---|
| `cockpit.vt` | Exact output bytes for either replay mode. Keyboard input and SSH authentication answers are excluded. |
| `cockpit.vt.json` | Initial columns and rows for ordinary `--replay`. |
| `cockpit.vt.jsonl` | The timed recording's initial geometry, output byte counts and resize events at elapsed microseconds, and a completion record with the raw file's SHA-256. |

Keep the three files together when attaching a timed capture. Ordinary
`--replay` renders the raw output in one pass. Timed replay verifies the raw
file and timing index, then reproduces the recorded grid, pixel dimensions and
DPI at each resize. `--cols` and `--rows` choose the initial viewing window.
Changing that window leaves the recorded grid intact. Titles remain available
to the pane picker and event journal. Recorded clipboard writes, desktop
notifications and sounds are suppressed during timed playback.

The destination directory must exist. Recording creates new files exclusively.
Stopping flushes them to disk and returns the raw path and byte count. Closing
the pane also finishes the recording. A disk error or the 256 MiB raw limit
stops recording and emits `recording-error`. The timing index is limited to
64 MiB and one million events. After an interrupted recording, a partial raw
stream can still be opened with `--replay`.

```json
{"verb":"record","pane":1,"path":"C:\\tests\\bug.vt"}
{"verb":"record","pane":1,"path":"C:\\tests\\cockpit.vt","timed":true}
{"verb":"record","pane":1,"path":null}
```

### Inspect one recorded update at a time

Choose **Open recording paused…** in the command palette or the tab menu’s
**Recordings** submenu to start before the first output. **Next recorded update**
applies all output and resizes at the next distinct capture timestamp in file
order. **Resume replay** runs at the recorded pace. **Pause replay** freezes
the playhead, the synchronized-output timeout and kitty image animation.

```powershell
.\w11term.exe --replay-timed .\cockpit.vt --paused
# Use the replay window's process ID and its pane ID from w11term-cli list.
& $ctl --pid $replayPid playback --pane $pane --step
& $ctl --pid $replayPid get-text --pane $pane
& $ctl --pid $replayPid screenshot --pane $pane .\cockpit-update.png
& $ctl --pid $replayPid playback --pane $pane --resume
```

`playback --pane ID` returns the pane ID, `state`, `position_us` and
`duration_us`. The times are microseconds from the beginning of the capture.
The duration is available after validation. States are `loading`, `playing`,
`paused`, `complete` and `failed`. A failed replay includes an `error`.
Control operations address a background pane without activating its tab.

A step returns after its timestamp group has been processed. During a
synchronized update, `get-text` and `screenshot` keep showing the previous
committed screen until an end marker or the 150 ms presentation timeout.
An interval with several recorded writes can therefore take several steps
to produce a visible change. Stepping over trailing silence reaches the end
of the capture.

```json
{"verb":"playback","pane":1}
{"verb":"playback","pane":1,"action":"step"}
```

## SFTP

SFTP is available through a pane's **existing authenticated SSH connection**, on a separate, lazily opened subsystem channel. It never starts a local shell, opens a second SSH login, or changes window/tab focus. A server must offer the `sftp` subsystem; a rejected subsystem leaves the terminal usable. Remote paths are evaluated by the server, independently of the shell's current directory. Use `realpath .` to discover the SFTP working directory, and prefer absolute remote paths in scripts.

### Control drawer transfers

```powershell
$active = & $ctl --pid 12345 sftp list-transfers | ConvertFrom-Json
$active.result.transfers
& $ctl --pid 12345 sftp list-transfers --pane 7
& $ctl --pid 12345 sftp cancel-transfer $active.result.transfers[0].transfer_id
& $ctl --pid 12345 sftp wait-transfer $active.result.transfers[0].transfer_id --timeout 30
```

`list-transfers` samples active drawer transfers across the selected process.
Each row has `transfer_id`, `pane`, `window_id`, `tab_id`, `label`, `operation`,
`state`, `remote`, `progress`, and `batch`. Operations are `prepare-upload`,
`upload-file`, `upload-folder`, `download-file`, and `download-folder`.
A transfer is `preparing`, `running`, or `cancelling`. When available,
`progress` has `bytes`, the estimated `total`,
and the number of completed `files`. A multi-item batch has one-based `item`
and `total` in `batch`. Progress and batch data can be `null`.

Pass the exact `transfer_id` from the same process to `cancel-transfer`. The ID
stays with the batch across items and pane moves. An accepted cancellation
returns `cancelling`, `already-cancelling`, or `cancelled-before-start` in
`result.state`. An in-flight transfer may still need to close its remote
handle. A finished transfer or one from an earlier connection on a live pane
fails with a stale ID error. A closed pane returns an unknown-pane error.
The commands leave the selected pane and window focus alone.

`wait-transfer` takes the same ID and waits for that transfer's final journal
result, or returns it immediately if retained. The reply's `result` contains
`pane`, `sequence`, `timestamp_ms`, and `transfer` with the same fields as the
`sftp-transfer` event data. Inspect `outcome` for `completed`, `cancelled`,
`failed`, or `interrupted`. Any recorded outcome is a successful wait.
The default timeout is 30 seconds. The transfer keeps running if the wait times
out. A closed pane or an unknown or evicted ID returns an error.

Direct pipe clients send
`{"verb":"list-sftp-transfers","pane":7}` or
`{"verb":"cancel-sftp-transfer","transfer_id":"7:2:11"}`. To wait, send
`{"verb":"wait-sftp-transfer","transfer_id":"7:2:11","timeout_seconds":30}`.

### Preview and apply SSH dotfiles

`sftp --pane ID preview-dotfiles BUNDLE.json --shell bash|zsh` shows how a Windows bundle would fit into the startup files for an open SSH pane. The preview reads the remote filesystem over the pane's SFTP connection. Use it when the SFTP starting directory is the login shell's home, and check the returned `home`. For a Zsh setup that uses a different startup directory under that home, pass `--zdotdir RELATIVE_DIR`.

Save a UTF-8 JSON bundle with `version` set to `1`. The optional `environment` fragment is shared by the chosen shells. Set `bash` or `zsh` for the prompt, aliases, and interactive setup specific to that shell. Each fragment supports up to 16 KiB. The preview computes planned file hashes with LF line endings and a final newline on nonempty fragments.

```json
{
  "version": 1,
  "environment": "export EDITOR=vim\n",
  "bash": "PS1='ops \\u@\\h> '\n",
  "zsh": "PROMPT='ops %n@%m> '\n"
}
```

```powershell
$bundle = '.\Fleet dotfiles.json'
$preview = & $ctl --pid 12345 sftp --pane $pane preview-dotfiles $bundle --shell bash | ConvertFrom-Json
$preview.result.sftp
# Review the home, files, and proposed hooks before running apply.
$review = $preview.result.sftp.review
& $ctl --pid 12345 sftp --pane $pane apply-dotfiles $bundle --shell bash --review $review
```

In `result.sftp`, `directories_to_create` names missing managed directories and `files` gives the absolute paths, proposed actions, and before/after SHA-256 hashes. For Bash, the preview selects the first existing login file in the order `.bash_profile`, `.bash_login`, `.profile`, and plans a new `.profile` if none exists. It also previews `.bashrc`. Zsh previews `.zprofile` and `.zshrc`. Startup entries include the exact `hook` block. A changed or duplicated marker block is a `hook-conflict`, and modified version files or selectors get their own conflict status. The `supports_atomic_replace` field reports whether the server offers the OpenSSH atomic replacement extension.

Pass the returned `review` token to apply on the same pane with the same bundle, shell and optional `--zdotdir`. If either the plan or the SFTP connection changed, preview again. Apply checks for conflicts and atomic replacement support before writing. It stages fragments in a directory named for `bundle_sha256` under `~/.config/w11term/dotfiles/`, installs the startup hooks, then switches the shell’s active bundle with a single file rename. New shells use that version. A failed activation can leave staged files, which a fresh preview can reuse. Older bundle directories are retained.

Apply replies with `shell`, `home`, `bundle_sha256` and the ordered `changed_files`. An unchanged bundle returns an empty list. A missing fragment becomes an empty managed file. Direct pipe clients can send `{"op":"preview-dotfiles","shell":"bash","bundle":{"version":1,"bash":"PS1='ops> '"}}` with the normal SFTP request fields. Use `"op":"apply-dotfiles"` plus the returned `"review"` for the reviewed installation.

### Run SFTP operations

```powershell
& $ctl --pid 12345 sftp --pane $pane info
& $ctl --pid 12345 sftp --pane $pane ls /var/log
& $ctl --pid 12345 sftp --pane $pane get /tmp/output.bin .\output.bin
& $ctl --pid 12345 sftp --pane $pane get /tmp/output.bin .\output.bin --sha256 $expectedDigest
& $ctl --pid 12345 sftp --pane $pane get-dir /tmp/diagnostics .\incident-042
& $ctl --pid 12345 sftp --pane $pane put .\input.bin /tmp/input.bin
& $ctl --pid 12345 sftp --pane $pane put-dir .\cockpit /tmp/cockpit-v2
& $ctl --pid 12345 sftp --pane $pane mkdir /tmp/work --permissions 0750
& $ctl --pid 12345 sftp --pane $pane rename /tmp/input.bin /tmp/work/input.bin
& $ctl --pid 12345 sftp --pane $pane stat /tmp/work/input.bin
& $ctl --pid 12345 sftp --pane $pane remove_file /tmp/work/input.bin
& $ctl --pid 12345 sftp --pane $pane remove_dir /tmp/work

$directory = & $ctl --pid 12345 sftp --pane $pane opendir /var/log | ConvertFrom-Json
$handle = $directory.result.sftp.handle
do {
    $page = & $ctl --pid 12345 sftp --pane $pane readdir --handle $handle | ConvertFrom-Json
    if (-not $page.ok) { throw $page.error }
    $page.result.sftp.entries
} until ($page.result.sftp.eof)
& $ctl --pid 12345 sftp --pane $pane close --handle $handle
```

`get` and `put` stream binary files in bounded chunks inside the GUI; file contents do not pass through JSON or the clipboard. Local paths resolve in the CLI caller's working directory, including when the GUI started elsewhere. The destination parent must exist. Both commands refuse to replace existing destinations unless `--overwrite` is given. A download publishes a temporary file atomically only after the transfer and remote close succeed. An upload writes directly to the remote destination; failures, timeouts, or disconnects can leave partial remote files, including a truncated previous file with `--overwrite`. Success acknowledges SFTP writes and close; use `fsync` on an open handle when server-side durability is required.

A folder download builds a temporary tree in the destination’s parent directory.
After all reads and closes succeed, it publishes the tree under the requested
name. The command refuses an existing destination, including an empty directory.
It copies regular files and directories and counts skipped symbolic links and
special entries. A Windows filename collision ends the operation. Traversal
allows 32 levels below the root, 65,536 returned directory entries and 8 MiB of
entry-name bytes across the transfer. The server can change files during a
download. The result reflects the files read during the transfer.

A folder upload creates the requested remote directory and requires its parent
to exist. The destination can have a different name from the source folder.
`remote` preserves the requested path. Once the server resolves the parent,
`resolved_remote` gives the absolute destination used by the transfer.
An existing destination, including an empty directory, stops the transfer.
Empty directories and empty files are copied. Windows reparse points such as
junctions inside the tree are counted as skipped. A reparse point supplied as
the source root is rejected. Interruption after remote creation can leave a
partial tree. The reply identifies the destination so you can inspect it before
retrying. A file counts as complete once the server acknowledges all writes
and the final close. An upload allows 32 directory levels below the root,
65,536 entries, and 8 MiB of entry names. w11term reads each file in chunks
and checks its length against the size reported when it opened the file.

The command surface covers SFTP v3:

| Commands | Arguments / result |
| --- | --- |
| `info` | Negotiated version, advertised extensions, open-handle count, read/write chunk limits |
| `get REMOTE LOCAL --sha256 HEX` | Verify a single-file download against 64 hexadecimal digits before publishing the local file. A mismatch preserves any existing destination. Success adds `sha256` to `result.sftp`. Direct pipe clients set `expected_sha256` on the `get` operation. |
| `get REMOTE LOCAL --sha256 HEX --resume` | Keep an interrupted download in a `.w11term-download-*.part` file beside `LOCAL`. Repeat with the same local path and digest on any live connection to continue. Success includes `resumed_from`, the byte offset used for the first remote read. The whole file is verified before publication. A digest mismatch discards the partial file. Direct pipe clients set `resume: true` alongside `expected_sha256`. |
| `sha256 PATH [--max-bytes N]` | Hash a regular remote file over SFTP without creating a local copy. Returns lowercase hexadecimal `sha256` and the number of `bytes` hashed in `result.sftp`. A limit violation, interruption, or observed modification aborts the operation without returning a digest. Direct pipe clients send `{"op":"sha256","path":"PATH","max_bytes":N}` and may omit `max_bytes`. |
| `put LOCAL REMOTE --atomic [--overwrite]` | Stage a single file next to its destination before publishing it. Replacing a regular file requires both `--overwrite` and `posix-rename@openssh.com`. Success returns `atomic: true`. Direct pipe clients set `atomic: true` on the `put` operation. |
| `get-dir REMOTE NEW_LOCAL_DIRECTORY` | Recursively download a directory into the exact local path. Its parent must exist. Success returns `files`, `directories` (including the root), `skipped`, and `bytes_transferred`. |
| `get … --preserve-mtime`, `get-dir … --preserve-mtime` | Keep the SFTP server’s reported modification time on the staged file or tree before publishing it. File downloads use handle metadata, and folder downloads include files and directories with a reported time. Entries without a time keep their local write time. Direct pipe clients set `preserve_mtime: true`. |
| `put-dir LOCAL NEW_REMOTE_DIRECTORY` | Recursively upload to the exact remote path. Success returns `files`, `directories` (including the root), `skipped`, and `bytes_transferred`. |
| `put … --preserve-times`, `put-dir … --preserve-times` | Send the source regular file’s access and modification times after its data. Atomic uploads apply the timestamps before publishing. A timestamp error fails the transfer. A direct file or folder upload may already have written remote data. Direct pipe clients set `preserve_times: true`. |
| `ls [PATH]` | Collect directory entries (default `.`); maximum 8 MiB JSON, then use paged reads |
| `find ROOT [QUERY] [--regex] [--match-case] [--kind TYPE]` | Search filenames recursively with a pattern up to 256 bytes. The default is a case-insensitive literal. `--regex` uses Rust regex syntax, and `--match-case` enables case-sensitive matching. `--kind` keeps entries of type `file`, `directory`, or `symlink`. You can omit the query with `--kind`. Returns the canonical `root`, `matches` with `parent`, `name` and a known `kind`, scan counts, and `truncated` and `cancelled` flags. A search examines up to 128 folders, 16,384 entries, 200 matches and 24 nested levels, with a 4096-byte path limit. Direct pipe clients send `{"op":"find","path":"ROOT","query":"QUERY"}` with optional fields `regex`, `match_case`, and `kind`. Use an empty query for a type-only request. |
| `opendir PATH`, `readdir --handle ID`, `close --handle ID` | Persistent opaque handle, one server page at a time, explicit `eof` |
| `open PATH [--flags FLAGS]` | Comma-separated `read,write,append,create,truncate,exclusive`; default `read`; returns handle |
| `read --handle ID [--offset N] [--length N]` | Default offset 0, length 32768; returns `data_base64`, `bytes_read`, `eof`. Short reads are possible; EOF requires the server's EOF response |
| `write --handle ID [--offset N] --base64 DATA` | Binary bytes at an explicit offset (default 0), returns `bytes_written` |
| `stat PATH`, `lstat PATH`, `fstat --handle ID` | `attrs` containing size, ownership, Unix permissions/type, and timestamps as reported by the server |
| `setstat PATH`, `fsetstat --handle ID` | Attributes: `--size N`, `--uid N --gid N`, `--permissions OCTAL`, `--atime UNIX_SECONDS --mtime UNIX_SECONDS` |
| `mkdir PATH [ATTRIBUTES]` | Same attributes as setstat; no automatic parent creation |
| `rename FROM TO` | Standard v3 rename semantics; no implicit delete/overwrite fallback |
| `remove-file PATH`, `remove-dir PATH` | File/symlink removal or empty-directory removal; no recursive deletion |
| `realpath PATH`, `readlink PATH` | Server-provided names in `entries` |
| `symlink TARGET LINK [--spec-order]` | OpenSSH wire order by default; use `--spec-order` for strict original-draft servers |
| `hardlink FROM TO`, `fsync --handle ID`, `statvfs PATH`, `limits`, `expand-path PATH` | Corresponding OpenSSH extensions, only when the required version is advertised |

Underscore command aliases such as `remove_file` and `remove_dir` are accepted by the CLI. `--` ends option parsing for paths beginning with `--`. Reads and writes are limited to the negotiated chunk size (at most 32768 bytes); `info` reports the actual limit. `get`/`put` have no whole-file size cap. Metadata pairs (`uid`/`gid`, `atime`/`mtime`) must be supplied together. Symlink ordering follows [OpenSSH's documented interoperability convention](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL).

Handles belong to one pane, survive separate CLI invocations, and must be closed explicitly. At most 128 explicit handles may be held per pane (the server can impose a lower limit). After five minutes without SFTP requests, the subsystem closes and all handles expire. Pane closure, channel failure, and operation timeout also invalidate handles. A later command can open a fresh subsystem on the same authenticated SSH connection; failed mutations are never retried automatically. SFTP operations serialize per pane through a bounded queue while terminal input/output continues independently.

SFTP operations default to a **600-second total deadline**, including queue time; `--timeout SECONDS` accepts 1–3600. Individual server packets also have a ten-second response deadline. The CLI allows five extra seconds for the JSON reply. A timeout means the operation may have completed or partially modified its destination; inspect before retrying. Closing the CLI alone does not guarantee cancellation; closing the pane cancels its SFTP worker.

Direct pipe requests use a nested operation. All local transfer paths must be absolute. Operation names/fields match the table above, except `get`/`put` use `remote`, `local`, `overwrite`; `read` uses `len`; `write` uses `data_base64`; attributes use an `attrs` object with decimal numeric values; and `symlink` uses `target`, `path`, optional `spec_order`. `open.flags` is an array of strings.

```json
{"verb":"sftp","pane":1,"timeout_seconds":600,"operation":{"op":"get","remote":"/tmp/output.bin","local":"C:\\tests\\output.bin","overwrite":false}}
{"verb":"sftp","pane":1,"operation":{"op":"get-dir","remote":"/tmp/diagnostics","local":"C:\\tests\\incident-042"}}
{"verb":"sftp","pane":1,"operation":{"op":"put-dir","local":"C:\\Projects\\cockpit","remote":"/tmp/cockpit-v2"}}
{"verb":"sftp","pane":1,"operation":{"op":"mkdir","path":"/tmp/work","attrs":{"permissions":488}}}
{"verb":"sftp","pane":1,"operation":{"op":"open","path":"/tmp/output.bin","flags":["read"]}}
{"verb":"sftp","pane":1,"operation":{"op":"read","handle":7,"offset":0,"len":32768}}
{"version":1,"ok":true,"result":{"pane":1,"sftp":{"handle":7,"directory":false}}}
```

SFTP replies use the same `ok` / `error` envelope and exit status as other commands, with results under `result.sftp`. Non-SSH panes and unavailable/closed connections fail clearly.

## Verification

`cargo test --all-targets` includes an actual named-pipe/SSH roundtrip with raw control sequences, NUL, and invalid UTF-8, plus PNG checks for HUD text, kitty graphics, and sixel content.

`tests/sftp.rs` adds actual SSH/SFTP packets, a temporary server filesystem, binary transfers, Unicode filenames, overwrite protection, metadata, paged directory handles, raw file I/O, timeout recovery, and terminal input during a delayed download. It also invokes the console CLI against the running test pipe and checks subsystem rejection does not break the shell. No test credentials or external server are required.

The opt-in `tests\sftp-live-smoke.ps1 [-Configuration debug] [-Profile root]` checks a real OpenSSH server through a GUI instance it launches, including SHA-256 transfer equality, symlink/hardlink behavior, metadata, and advertised extensions. It creates one uniquely named `/tmp/w11term-sftp-...` directory, cleans up its known files, and closes only its own GUI. Artifacts remain under `.local\sftp-smoke`.

With the release GUI, CLI, and `echo_server` example built, run `tests\control-smoke.ps1` in PowerShell 7 (`-Configuration debug` for debug builds). It launches its own loopback SSH peer and GUI, checks launch geometry, inactive raw input/PNG capture, regex waits, background tab creation, active/background resizing, recording/events, JSON failures, and exact saved-grid restoration after reopening a profile. Window settings are isolated. It closes only the processes it started. Artifacts remain under `.local\control-smoke`.
