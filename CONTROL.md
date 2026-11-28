# External control

Build both executables with `cargo build --release --bins`. `w11term.exe` is the GUI and `w11term-cli.exe` is its console companion. Control commands connect to an existing instance, write JSON to stdout, and exit with code 1 on errors. Commands use one reply, while `follow-ssh-group-output` emits JSON Lines.

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

## Offline command reference

`command-reference QUERY` reads the bundled or cached catalog and private pages without connecting to a w11term instance. Exact lookup prefers the selected platform and falls back to Common. The default platform is Windows. Pass `--search` to match partial command names or task descriptions.

`--format text` shows the manual and its examples, or a short list for a search. The default JSON includes `schema_version`, `query`, `preferred_platform`, `catalog_status`, and `matches`. Each match includes the command name, title, platform, source, description, documentation URL when present, related commands, and examples. A missing exact manual exits with code 1. A search without matches returns an empty array.

```powershell
.\target\release\w11term-cli.exe command-reference 'git checkout' --platform linux --format text
.\target\release\w11term-cli.exe command-reference 'comparative benchmark' --platform linux --search
```

`open-command-reference QUERY [--window ID] [--platform PLATFORM]` opens an exact manual in the side panel of a running instance. Use `list-windows` to find the target ID. With no `--window`, the request uses the instance's most recently active window. With no `--platform`, it prefers the target window's selected pane's platform, with a Common fallback. It keeps the active tab and desktop focus in place and sends no shell input. A missing manual or an open Command reference editor produces a failed reply.

```powershell
& $ctl --pid 12345 open-command-reference 'git checkout' --window 2 --platform linux
```

The pipe verb is `open-command-reference`. Its fields are `query` and optionally `window_id` and `platform`. The JSON result includes the resolved `window_id`, canonical `command`, and actual `platform`.

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

From a connected SSH pane, open **More → Command reference → Apply SSH dotfiles**. The panel starts with the profile's bundle and shell if set. You can also choose a local bundle and Bash, Zsh, or Fish as the target. Zsh accepts an optional startup directory. Select **Review dotfiles changes** to see the account, resolved SFTP home, planned file changes, and any conflicts. Check that the resolved directory is the shell account's home, then select **Apply reviewed dotfiles** on the same connection. A changed bundle or remote startup file requires another review. Applying updates the managed startup files for future shells and leaves the profile's login-sync settings in place.

Expand **Environment fragment changes** or **Prompt fragment changes** to review the lines that will be added and removed. Expand a proposed **Startup hook** to see the generated block in context of its target path. The review omits unrelated startup-file contents. If the active managed fragments cannot be verified, the review identifies that condition and shows the proposed lines. A verified active fragment that changes after review makes the confirmation stale.

`sftp --pane ID preview-dotfiles BUNDLE.json --shell bash|zsh|fish` shows how a Windows bundle would fit into the startup files for an open SSH pane. The preview reads the remote filesystem over the pane's SFTP connection. Use it when the SFTP starting directory is the login shell's home, and check the returned `home`. Pass `--zdotdir RELATIVE_DIR` for a custom Zsh directory or `--fish-config-dir RELATIVE_DIR` for a custom Fish directory. Use the same option for the reviewed apply, hook repair, and version switch.

Save a UTF-8 JSON bundle with `version` set to `1`. The optional `environment` fragment is shared by Bash and Zsh. Set `bash`, `zsh`, or `fish` for the prompt, aliases, and interactive setup specific to that shell. Fish reads its own `fish_environment` fragment and uses Fish syntax in both sections. Each fragment supports up to 16 KiB. The preview computes planned file hashes with LF line endings and a final newline on nonempty fragments.

```json
{
  "version": 1,
  "environment": "export EDITOR=vim\n",
  "bash": "PS1='ops \\u@\\h> '\n",
  "zsh": "PROMPT='ops %n@%m> '\n",
  "fish_environment": "set -gx EDITOR vim\n",
  "fish": "function fish_prompt\n    printf 'ops> '\nend\n"
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

Fish defaults to `~/.config/fish/config.fish`. In the Fish configuration directory field, enter the existing directory holding `config.fish`, relative to the SSH home. For example, if `XDG_CONFIG_HOME` is `$HOME/settings`, enter `settings/fish`. A review can create the default startup directories or add `config.fish` to an existing custom directory. The hook loads the Fish environment on startup and loads the prompt fragment in interactive shells. [Fish configuration files](https://fishshell.com/docs/current/language.html#configuration-files)

Pass the returned `review` token to apply on the same pane with the same bundle, shell and optional `--zdotdir`. If either the plan or the SFTP connection changed, preview again. Apply checks for conflicts and atomic replacement support before writing. It stages fragments in a directory named for `bundle_sha256` under `~/.config/w11term/dotfiles/`, installs the startup hooks, then switches the shell’s active bundle with a single file rename. New shells use that version. A failed activation can leave staged files, which a fresh preview can reuse. Older bundle directories are retained.

Apply replies with `shell`, `home`, `bundle_sha256` and the ordered `changed_files`. An unchanged bundle returns an empty list. A missing fragment becomes an empty managed file. Direct pipe clients can send `{"op":"preview-dotfiles","shell":"bash","bundle":{"version":1,"bash":"PS1='ops> '"}}` with the normal SFTP request fields. Use `"op":"apply-dotfiles"` plus the returned `"review"` for the reviewed installation.

Install and switch previews include `fragment_changes`. Its `baseline` is `verified`, `inactive`, or `unavailable`, and each entry in `fragments` includes its `role` and complete managed `before` and `after` text. A null side has no verified text for that state. An empty string is an empty managed fragment. Disable previews have null proposed text, and resuming a held version shows matching before and after text.

To recover an edited or missing startup hook, choose **Review startup hook repair** in the dotfiles installer and confirm **Repair reviewed startup hooks**. The review shows changed lines inside a replaced block and the full block for a missing hook. Repair uses the selected shell and Zsh startup directory, preserves the surrounding file text, and keeps the active version and update policy. Duplicate, incomplete, or nested w11term markers block repair. Changes to existing files require the server's atomic rename support.

Scripts use `preview-dotfile-hooks --shell bash|zsh|fish [--zdotdir RELATIVE_DIR]` and `repair-dotfile-hooks --shell bash|zsh|fish --review TOKEN` with the same optional `--zdotdir` for Zsh. The preview includes `ready` and `files`, with the old managed block in `replaced_hook` and replacement in `hook`. Repair returns `changed_files`. A failed or interrupted repair requires a fresh review.

### Restore or disable SSH dotfiles

In **Command reference → Apply SSH dotfiles**, choose the shell and its startup directory, then open **Dotfile versions**. Select a retained version and choose **Review dotfile restore** followed by **Restore reviewed dotfiles**. To disable managed fragments, use **Review disabling managed dotfiles** followed by **Disable reviewed dotfiles**. Both actions pause automatic dotfile sync for that shell on the remote account.

Choose **Review resuming automatic dotfile sync** and then **Resume automatic dotfile sync** to allow an approved profile to sync on its next SSH connection. After a restore, the currently selected version stays active. After disabling, managed fragments stay inactive until a bundle is installed. To apply a particular local bundle immediately, return to the installer and review it there.

Use `list-dotfile-versions --shell bash|zsh|fish` to get a shell's SFTP home, active hash, selector state and up to 64 retained versions. The active version appears first, followed by the most recently modified directories. After resuming from disabled, the selector state is `awaiting-sync` until a bundle is installed. The listing identifies candidate directories. **Review dotfile restore** verifies the selected bundle's contents and startup hooks.

Use `preview-dotfiles-switch SHA256|disabled|automatic --shell bash|zsh|fish` to review a switch on the pane's SFTP connection. The preview includes the current `active_bundle_sha256` and the proposed `bundle_sha256`. A null value represents disabled or inactive dotfiles. Pass the same `--zdotdir` used to install Zsh hooks in a custom directory.

```powershell
$versions = & $ctl --pid 12345 sftp --pane $pane list-dotfile-versions --shell bash | ConvertFrom-Json
$preview = & $ctl --pid 12345 sftp --pane $pane preview-dotfiles-switch $previousBundle --shell bash | ConvertFrom-Json
$preview.result.sftp
# Review the currently active and proposed bundles.
& $ctl --pid 12345 sftp --pane $pane switch-dotfiles $previousBundle --shell bash --review $preview.result.sftp.review
```

Use the same two commands with `disabled` to stop loading managed dotfiles in new shells, or with `automatic` to lift a hold from a restore or disable. Switching only changes the selector. The startup hooks and retained files stay available for the next review. Restoring a version or lifting its hold checks that the retained fragments match their bundle hash and that both startup hooks are installed. A stale token or edited fragment prevents the change. The reply reports `shell`, `home`, `target`, the selected `bundle_sha256` or null, and `changed`.

Direct pipe clients use `{"op":"list-dotfile-versions","shell":"bash"}` to list the versions and `{"op":"switch-dotfiles","shell":"bash","target":{"kind":"disabled"}}` for preview. To target a bundle, provide `"target":{"kind":"bundle","sha256":"SHA256"}`. To lift a hold, use `"target":{"kind":"automatic"}`. Include the returned `"review"` to publish the switch.

Select an active or older version in **Dotfile versions** and choose **Save selected dotfile bundle** to write a new Windows JSON manifest. The manifest contains the verified environment and prompt fragments for the selected shell. Choose that shell when reviewing the saved manifest for another SSH account. The destination must be a new file.

For the profile that opened the connected pane, choose **Review saved dotfiles in this SSH profile** after saving. The profile editor shows the saved digest, verified host-key fingerprint, SFTP home, shell, and optional Zsh startup directory. **Approve dotfiles sync** checks that the manifest still reproduces the saved version. This handoff selects **Pin approved dotfile version** by default. Choose **Save profile changes** to use it on the next connection.

Scripts use `sftp --pane ID export-dotfile-bundle SHA256 LOCAL.json --shell bash|zsh|fish`. The result includes `shell`, `home`, `bundle_sha256`, and the saved `local` path. Direct pipe clients send `"op":"export-dotfile-bundle"` with `shell`, `bundle_sha256`, and an absolute Windows `local` path.

### Sync dotfiles on SSH connect

In **Settings → Edit profile → SSH dotfiles**, choose **Create dotfiles bundle** and fill in the fragments you want to manage. Use the shared environment for Bash and Zsh and the separate Fish environment and startup fields for Fish. **Save dotfiles bundle** writes a new local file and selects it in the profile. To change an existing bundle, select it and choose **Revise selected dotfiles bundle**. Save the revision to a new file. Profiles that use the original file continue to use it until you approve and save the revision for those profiles.

In either editor, **Import shell fragment** appends a local UTF-8 file to the section you choose, including extensionless files such as `.profile`, `.bashrc`, and `.zshrc`. It accepts a UTF-8 BOM and converts CRLF to LF. Review or edit the combined text before saving. Each combined fragment can be up to 16 KiB.

Use **Add shell setup** to append the bundled Bash, Zsh, or Fish setup after the existing startup fragment. Choose **Directory reporting only** when your prompt already emits semantic markers, or choose **Directory and prompt integration** to enable directory reporting, prompt navigation, command history, and completion alerts. The inserted code stays editable. Selecting another mode for the same shell replaces an intact generated block. Saving writes a new bundle that you can review and approve for SSH sync.

Use `save-dotfiles-bundle` to build the same bundles offline. `--environment-file` imports into the shared Bash and Zsh environment. `--bash-file`, `--zsh-file`, `--fish-environment-file`, and `--fish-file` import into the corresponding fragments. Repeat a file option to append multiple files to that fragment in order. The importer accepts UTF-8 with an optional BOM, normalizes CRLF to LF, and applies the same combined 16 KiB limit as the editor.

Pass `--from-bundle PATH` to start from an existing bundle. Imports append to its selected fragments, and the command preserves untouched fragments. You can also request `--shell bash|zsh|fish --setup directory|full` with or without imports. The CLI inserts or updates that setup block after reading the requested files. With `--output-file PATH` it saves a new file and returns its absolute path. Omit that option to print the bundle JSON. Review the result before approving it for an account.

```powershell
& $ctl save-dotfiles-bundle --environment-file '.\.profile' --bash-file '.\.bashrc' --zsh-file '.\.zshrc' --output-file '.\fleet.json'
& $ctl save-dotfiles-bundle --fish-file '.\config.fish' --shell fish --setup full --from-bundle '.\fleet.json' --output-file '.\fleet-with-fish-setup.json'
```

Use `inspect-dotfiles-bundle BUNDLE` to validate a local bundle and list its available shells and their `bundle_sha256` values in `result.shells`. These are the per-shell digests used for profile pins and retained remote versions. The reply contains no shell fragment text. Add `--shell bash|zsh|fish` to check one shell. With `--sha256 HEX` it exits with code 1 if that shell’s digest differs from the expected value.

```powershell
$before = & $ctl inspect-dotfiles-bundle '.\fleet.json' --shell bash | ConvertFrom-Json
$digest = $before.result.shells[0].bundle_sha256
& $ctl inspect-dotfiles-bundle '.\candidate.json' --shell bash --sha256 $digest
```

Use `compare-dotfiles-bundles BEFORE AFTER` to compare two local bundles. It reports `result.changed` and lists the shells present in either file in `result.shells`. Each entry has a `change` of `added`, `removed`, `changed`, or `unchanged` and the `before_bundle_sha256` and `after_bundle_sha256` values. A null digest means the bundle has no fragments for that shell. `changed_fragments` names the affected roles, `environment` and `prompt`.

The comparison uses the normalized text w11term would install. JSON formatting, CRLF versus LF, and a missing final newline on nonempty text produce the same result. Select one shell with `--shell`. Add `--include-text` to see the exact normalized `before` and `after` text of each changed fragment. A successful comparison exits with code 0 even when it finds changes.

```powershell
$comparison = & $ctl compare-dotfiles-bundles '.\fleet.json' '.\candidate.json' --shell bash | ConvertFrom-Json
$comparison.result.shells
```

From the full text preview of an SSH file, **Use in dotfiles editor** brings a snapshot into the connected profile's draft. It opens a new bundle draft if there is no editor open for that profile. Choose the first and last lines and use **Append SSH lines** to select the destination fragment. Select lines outside existing w11term startup blocks. Saving creates a local bundle. Using the bundle on login still requires profile approval and saving.

Open **Settings → Edit profile → SSH dotfiles**. Choose the bundle and shell, then enter the account's SFTP home and verified SHA256 host-key fingerprint. The dotfiles fingerprint field can stay blank when the profile already has that fingerprint pinned. Review the displayed account and host, select **Approve dotfiles sync**, then save the profile. To stop syncing on future connects, select **Disable dotfiles sync** and save. Use the manual `switch-dotfiles` command to select `disabled` on the server as well.

Use **Preview dotfile changes** to see the selected shell’s proposed digest and line changes. The preview compares with that shell’s currently approved local bundle when available. For a pinned approval, the old file must still match its pin. If the original cannot be verified, the preview shows the proposed fragments. Once you preview a bundle, approval checks that the selected shell still matches the previewed digest, even if you leave the pin checkbox clear.

Use `audit-dotfiles-profiles` to check all saved SSH profiles offline. It uses the normal profile catalog and resolves bundle paths beside that catalog. Use `--profiles PATH` for a different catalog or `--profile NAME` for one saved SSH profile. Entries in `result.profiles` report `matches-pin`, `pin-mismatch`, `follows-file`, `bundle-error`, `invalid-settings`, `disabled`, or `not-run`. The last status covers durable and RustMUSH profiles, which use their own shell startup. The report checks local files and configuration. The authenticated account, remote home, and startup hooks are checked on the SSH connection.

Add `--candidate BUNDLE` to compare a proposed revision with the local file for each configured direct SSH profile. The candidate comparison identifies equivalent content, changed content, unavailable shells, or a missing current baseline. `candidate.matches_pin` is true or false when the profile has a pin and the candidate supplies its shell. The audit returns digests and metadata and leaves profiles untouched.

```powershell
$audit = & $ctl audit-dotfiles-profiles --candidate '.\fleet-revision.json' | ConvertFrom-Json
$audit.result.profiles | Where-Object status -eq 'pin-mismatch'
```

Use `rollout-dotfiles-profiles` to adopt a reviewed local revision in one or more saved SSH profiles with existing dotfile approval. Select each profile explicitly with `--profile`. The preview lists the previous approval, the candidate digest for that shell, and the pin that would be saved. It preserves the configured shell, account, host key, home, and startup directory. Pinned profiles receive the new digest and profiles that follow the file keep that policy.

```powershell
$rollout = @('rollout-dotfiles-profiles', '--candidate', '.\fleet-revision.json', '--profile', 'East', '--profile', 'West')
$preview = & $ctl @rollout | ConvertFrom-Json
$preview.result.profiles
```

After reviewing the result, repeat the same selection with `--review` and the returned token.

```powershell
& $ctl @rollout --review $preview.result.review
```

Both steps use the normal profile catalog unless you supply `--profiles PATH`. The report identifies each baseline as a matching pinned version, an unpinned file on disk, or unavailable. It includes the digest for a readable baseline. The apply step rechecks the selected approvals, baseline digests, and candidate shell digests and writes all updates in a single catalog replacement. Changes to those inputs require a new preview. Profiles without an existing valid approval and revisions missing any selected shell cannot be applied. The rollout edits the local catalog. Sync runs when a new ordinary SSH session opens with the updated profile.

In the Windows editor, select the revision under **SSH dotfiles** and choose **Roll out to saved profiles**. Check the saved profiles you intend to update. **Review selected rollout profiles** shows their accounts, previous bundle paths and pins, proposed shell digests, and expandable environment and prompt line changes. If the baseline is missing or no longer matches its pin, the review shows the proposed lines. **Apply reviewed rollout** saves that selection with the same stale-input checks as the CLI.

While the Windows rollout result is open, **Review rollback** lists the bundle paths and pins the rollback would restore. **Apply reviewed rollback** restores the batch together. Each selected approval must still match the completed rollout. If a bundle changes after the rollback preview, review it again before confirming.

Choose **Save rollout receipt** in the Windows result to keep a JSON file you can use after closing the editor. The receipt includes the local catalog and bundle paths and the profile names and approval metadata. For a CLI rollout, save the JSON reply from the successful apply for the same purpose.

To reopen either receipt in Windows, go to **Settings → Edit profile → SSH dotfiles → Review rollout receipt**. Choose a receipt for the profile catalog the window uses. Review the previous paths, pins, and bundle status, then choose **Apply reviewed rollback** to restore the listed approvals.

`rollback-dotfiles-profiles` accepts either receipt and previews the previous bundle paths and pins for the profiles that rollout changed. It uses the catalog recorded in the reply and rejects the whole batch if any of those approvals have since changed. The preview reports whether each previous bundle is available and whether it matches its saved pin. Rollback restores the prior approvals even if a previous bundle is missing. Login sync checks the restored approval and bundle when a new SSH session opens.

```powershell
# For a Windows rollout, use the saved receipt and skip the first line.
& $ctl @rollout --review $preview.result.review | Set-Content -Encoding utf8 '.\rollout-reply.json'
$rollback = & $ctl rollback-dotfiles-profiles '.\rollout-reply.json' | ConvertFrom-Json
$rollback.result.profiles
```

After reviewing the restored approvals, apply the rollback with its own token.

```powershell
& $ctl rollback-dotfiles-profiles '.\rollout-reply.json' --review $rollback.result.review
```

Select **Pin approved dotfile version** before approving to record the digest of the selected shell's environment and startup fragments. If the local bundle later produces a different digest, login sync skips the update and the SSH shell still opens. Approve and save the revised bundle to update the pin. Clear the checkbox, approve, and save to follow future edits to the local bundle. Existing approvals keep following the file until you pin them.

The same approval can be set in the profile's `dotfiles` object in `profiles.json`. A relative bundle path resolves beside the profile catalog. An optional `bundle_sha256` field stores the pin shown in the editor. Set `zdotdir` for a custom Zsh directory or `fish_config_dir` for a custom Fish directory under the approved home. If you move an existing configuration, review startup hook repair at the new location before resuming automatic updates.

```json
{
  "bundle": "Fleet dotfiles.json",
  "shell": "bash",
  "host": "dev.example.net",
  "user": "operator",
  "port": 22,
  "fingerprint": "SHA256:REPLACE_WITH_VERIFIED_HOST_FINGERPRINT",
  "home": "/home/operator"
}
```

On an ordinary SSH shell launch or reconnect, w11term checks that the verified host, account, port, fingerprint, and SFTP home match this approval. It then reads the local bundle and stages updates before requesting the shell. Sync verifies the currently active files and hooks before replacing their selection. The operation has a ten-second deadline, and a skipped or timed-out sync leaves opening the SSH shell available.

**More → Command reference** shows the dotfile sync outcome for the active SSH connection. Open **Apply SSH dotfiles** for the reason when sync is skipped or unconfirmed. A rollback or disabled selector appears as **Paused by manual selection** on the next login. The `dotfiles-sync` event remains available to scripts. Game sessions, durable attachments, and explicit workspace startup commands use their own startup paths, and the installer explains why login sync was skipped for those sessions.

A reviewed manual switch holds the chosen bundle on later logins, including when you choose `disabled`. Use a fresh manual preview and `apply-dotfiles` with the bundle you want to return to automatic sync.

### Download a pinned SSH tool bundle

In **Settings → Edit profile → SSH tools**, select a preset or pick individual utilities under **Custom tools**. Choose **Use selected tools** for a custom set. The download runs in the background and selects the prepared manifest when it finishes. Review the host and account, approve sync, and save the profile. Each selection and preset revision has its own local manifest, so earlier approvals keep their pins until you select and approve a new version.

Choose **Create bundle from local tools** to build a manifest from raw Linux executables on Windows. Give the bundle and each command a name, then pick the local executable. You can add an x86-64 and an ARM64 build under the same command name. w11term detects the architecture and computes the SHA-256 pin. The launcher directory defaults to `.local/bin` under the remote home. Save to a new manifest file, then review and approve it in the profile. Keep the source executables at their saved locations so installation can verify the pins.

Choose **Revise selected tool bundle** to load the current manifest. Add or remove a build, or use **Replace** to pin a new local executable of the same architecture in the same fallback position. A revision keeps the other entries and their existing pins and download sources. **Save bundle revision** writes a separate manifest. Review and approve it, then save the profile to use the revision on connect.

**Linux essentials** includes `rg`, `jq`, `dust`, and `hyperfine`. **Linux admin tools** adds `bandwhich`, `diskwatch`, `mdd` (mddog), `ncdu`, and `netwatch`. The presets and custom selections share the `linux-essentials` bundle name and `.local/bin` launchers for updates and rollback. Profiles that connect to the same account update the same active set. Omitted launchers become dormant and can be restored by rollback. Linux `x86_64` and `aarch64` are supported. Omit `--architecture` to fetch both. Use manual preview and apply for a first transfer that needs more than the login sync deadline.

```powershell
& $ctl fetch-tool-bundle --example '.\Linux essentials.json'
& $ctl fetch-tool-bundle '.\Linux essentials.json' --architecture x86_64
& $ctl fetch-tool-bundle --example '.\Linux admin tools.json' --preset admin
& $ctl fetch-tool-bundle '.\Linux admin tools.json' --architecture x86_64
& $ctl fetch-tool-bundle --example '.\My Linux tools.json' --tools 'rg,jq,mdd'
& $ctl fetch-tool-bundle '.\My Linux tools.json'
```

Each build in a version-1 manifest can use a `download` object in place of `path`. Set `format` to `raw` for a single executable. For `tar-gz` or `zip`, also set `member` to its path inside the archive and `archive_sha256` to the hash of the complete archive. The build's top-level `sha256` always pins the executable, and the client verifies its Linux ELF architecture before caching it. [Essentials pins](examples/tool-bundles/essentials.json) · [Admin additions](examples/tool-bundles/admin-additions.json)

Fetch the builds on Windows before using the manifest in a preview or SSH profile. w11term caches verified executables by SHA-256 in `%LOCALAPPDATA%\w11term\tools\cache\v1`. Preview and profile sync reverify the cached bytes and use them offline. To update a pin, edit the manifest and run the fetch command again.

### Preview a managed SSH tool bundle

Use a downloaded bundle or keep the Linux executables on Windows and point a version-1 bundle at them. Relative local build paths resolve beside the manifest. Pin the SHA-256 of each executable. Use the optional `release` URL to record its source. Supply an `x86_64` build, an `aarch64` build, or both. The default `bin_directory` is `.local/bin` under the remote home.

```json
{
  "version": 1,
  "name": "my-tools",
  "tools": [
    {
      "name": "rg",
      "builds": [
        {
          "architecture": "x86_64",
          "path": "linux-x86_64/rg",
          "sha256": "e62198eb19b136b88c330af83647b5a962cb99b6b1f066758568f12de1974849",
          "release": "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz"
        },
        {
          "architecture": "aarch64",
          "path": "linux-aarch64/rg",
          "sha256": "c14cdb389f34e504d69e386cfc67d5c5d9a730a990de03ca6910b2a15e30386a",
          "release": "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-aarch64-unknown-linux-musl.tar.gz"
        }
      ]
    }
  ]
}
```

```powershell
& $ctl --pid 12345 sftp --pane $pane preview-tool-bundle '.\Fleet tools.json'
```

The preview uses the pane's verified SSH connection. A read-only SSH exec probe identifies Linux, architecture, home, and the exec environment's `PATH`. The preview checks the same home over SFTP, verifies local file hashes and ELF headers, and selects the first verified self-contained build for each command. For dynamic builds it reports the interpreter, linked libraries, and glibc requirements it could read, including a missing loader or insufficient glibc when detectable. The report shows planned destinations, directory and launcher conflicts, and executable files found earlier in the probed `PATH`. Interactive shell startup files can change that search order.

### Install or roll back a reviewed SSH tool bundle

From a connected SSH pane, open **More → Command reference → Install SSH tools**. Choose a Windows manifest or use the bundle from the pane's profile. Select **Review bundle** to see the resolved home, architecture, launcher paths, PATH status, and any blocking conflicts. **Install reviewed bundle** applies that review on the same connection. The installation runs alongside the SSH shell and uses the manual SFTP deadline.

To repair an installed release, select the Windows manifest with the same bundle and executable pins. **Review bundle** identifies missing, changed, and non-executable files. **Repair from reviewed bundle** restores the verified Windows builds. Repairing the active release keeps its update policy. Repairing a retained release also activates it. A missing or invalid release manifest and unexpected file types block repair. Replacing an existing file requires OpenSSH atomic rename. Run a fresh review to continue after an interrupted repair.

From the manual for a built-in Linux tool such as `jq` or `rg`, choose **Review installing jq** or the corresponding action for that tool. The review shows the SSH account, launcher, retained tools, and the pinned build for the host's architecture. Choose **Download pinned Linux build** if the Windows cache needs the executable, then confirm **Install reviewed jq**. The install adds the command to the shared built-in bundle and preserves its active binaries. A new release pauses that bundle's automatic updates until you resume them from **Bundle versions**.

After a manual install or a review of an already installed built-in tool, choose **Manage PATH for new shells**. Pick Bash, Zsh, or Fish. For a custom `ZDOTDIR` or Fish configuration location, enter the directory relative to the remote home. **Review PATH setup** shows the startup files, managed blocks, and any default Fish startup directories it would create. **Apply reviewed PATH setup** appends the verified bundle's launcher directory after your existing PATH entries for new shells. This is a one-time change to the connected account and requires no profile approval.

To undo it, choose **Review PATH removal** and then **Remove reviewed PATH blocks**. The review shows exactly which managed blocks will be removed. Other startup content, the files themselves, and the installed tools are preserved. An edited, duplicate, or incomplete block stops the reviewed removal. Profiles configured to manage this directory can add it again during their next connection.

Open **Bundle versions** to see the releases still stored under the SSH account, with the active version marked. Choose a retained release and select **Review rollback** to verify its binaries and compare it with the active release. The review shows which commands would return, become inactive, change their pinned build, or keep their pin. It includes the SHA-256 and size of each relevant executable. **Restore reviewed version** switches to that release and pauses automatic updates for the bundle. Launchers for commands absent from the restored release become dormant.

Select the active release and choose **Save pinned bundle on Windows** to use those exact builds in a profile. w11term verifies the executables against the installed release and saves them with a manifest in `tool-bundle-snapshots` beside the profile catalog. The manifest contains the installed architecture. **Review in this SSH profile** opens the original pane's saved profile and reviews the manifest against the authenticated account. Choose **Approve tool sync** and **Save profile changes** to use it on the next login. You can also copy the manifest path and select it under **Edit profile → SSH tools**. If updates are paused, review and resume them from **Bundle versions** after saving the profile. Keep the manifest and its sibling executables together.

Review the destinations, PATH status, and selected builds. When the report has `ready: true`, pass its `review` token back to the same pane. The commands below use the default JSON response from `w11term-cli`.

```powershell
$plan = (& $ctl --pid 12345 sftp --pane $pane preview-tool-bundle '.\Fleet tools.json' | ConvertFrom-Json).result.sftp
& $ctl --pid 12345 sftp --pane $pane apply-tool-bundle '.\Fleet tools.json' --review $plan.review
```

Installation verifies the uploaded files over SFTP and publishes the version under `~/.local/share/w11term/tools/<name>/versions/<sha256>/`. It adds launchers to the bundle's `bin_directory`, preserving entries already owned by the account. Add that directory to the shell's `PATH` if the preview reports it missing. Keep the bin directory fixed across versions of one named bundle. The installer reuses a verified copy when the requested version is already present and copies unchanged tools from the active version when building a new one. The `previous_bundle_sha256` in an update reply identifies the version to restore.

The same CLI commands review and apply repairs. The preview adds `repairs` with each executable's expected `pin`, observed `before` state, and `reason`. The apply reply lists `repaired_tools` and reports `automatic_sync_paused`. A repair can succeed with `changed: false` when the active selector already chose the repaired version.

Use `preview-tool-bundle-add BUNDLE.json` and `add-to-tool-bundle BUNDLE.json --review TOKEN` to add tools to the named bundle. The review shows the retained executables and checks their hashes. The addition keeps the active bundle's launcher directory and each existing tool's exact binary, including when the input names an existing tool. It uses the manifest's launcher directory when creating a bundle. A new version pauses automatic sync for that bundle so an older profile manifest cannot remove the added tools on reconnect. Use **Bundle versions** to review a rollback or resume automatic updates.

From **Installed SSH tools**, select a command and choose **Review removing** for its bundle. **Remove reviewed SSH tool** activates a release that keeps the other commands and their exact executable pins. The removed command's managed launcher becomes dormant, including when you remove the last tool. Previous releases remain available for rollback, and automatic sync pauses until you resume it. User-owned launcher paths are left in place. Scripts can use `preview-tool-bundle-remove NAME TOOL` and `remove-from-tool-bundle NAME TOOL --review TOKEN`.

```powershell
$addition = (& $ctl --pid 12345 sftp --pane $pane preview-tool-bundle-add '.\Extra tools.json' | ConvertFrom-Json).result.sftp
& $ctl --pid 12345 sftp --pane $pane add-to-tool-bundle '.\Extra tools.json' --review $addition.review
```

Direct pipe clients use `"op":"add-to-tool-bundle"` with a `bundle` object and include the returned `review` to apply the addition on the same SFTP connection.

For a retained version, w11term can calculate the hashes on the verified SSH host using `head` and `sha256sum`. It checks the manifest and file metadata through SFTP and falls back to hashing the bytes over SFTP when the remote checksum command is unavailable.

```powershell
$restore = (& $ctl --pid 12345 sftp --pane $pane preview-tool-bundle-rollback my-tools $previousSha256 | ConvertFrom-Json).result.sftp
& $ctl --pid 12345 sftp --pane $pane rollback-tool-bundle my-tools $previousSha256 --review $restore.review
```

Rollback uses the retained remote files. The client checks their hashes and executable permissions before activating them. Launchers for commands omitted from the active version remain dormant so older versions can restore them. Installation and rollback require the host's `flock` command and SFTP symlinks. Switching an active version requires OpenSSH's atomic rename extension. Run a fresh preview after changing the manifest, moving to another pane or connection, or retrying an interrupted operation.

The rollback preview includes `changes` in command-name order. Each row has an `action` of `add`, `remove`, `change`, or `keep`, plus `before` and `after` pins with `sha256` and `bytes`. A missing pin is `null`. The before pins come from the active release manifest so the review is available even if an active executable needs repair. `changes` is `null` if the active selector is invalid.

To remove an inactive release permanently, select it in **Bundle versions**, choose **Review release deletion**, and confirm **Delete reviewed release**. The review lists the managed files and their total size. The active release and unrecognized files are protected. If cleanup is interrupted, refresh the versions and review the remaining files again. An inactive release that has lost its manifest can only be deleted if its directory is empty.

For scripts, use `preview-tool-bundle-prune NAME SHA256` followed by `prune-tool-bundle-release NAME SHA256 --review TOKEN`. The preview reports `files`, `remaining_bytes`, and `incomplete`. The result reports `removed_bytes`.

### Sync a tool bundle on SSH connect

Open **Settings → Edit profile → SSH tools**. Browse for the bundle manifest and enter the remote SFTP home and verified SHA256 host-key fingerprint. A pinned profile fingerprint also works. Select **Manage Bash PATH**, **Manage Zsh PATH**, or **Manage Fish PATH** to append the launcher directory after the shell's existing PATH entries. Then select **Review tool bundle**, confirm the account and launcher directory, choose **Approve tool sync**, and save.

The review lists the commands and their available architectures. Expand a command to see each build's local path or download URL, executable SHA-256, and archive member and SHA-256 when applicable.

New approvals pin the exact manifest file by default. If the file changes, w11term skips tool and PATH sync and still opens the SSH shell. Review, approve, and save again to sync a revision. Choose **Follow changes to the tool manifest** before review to let this profile follow edits to the file. Existing approvals with no pin already follow the file.

Profiles can store the same approval under `tool_bundle`. Paths to local manifests resolve beside the profile catalog. The approved `name` and `bin_directory` come from the manifest and bind later updates to that destination. Use `add_to_path.shell` with `bash`, `zsh`, or `fish`. For a custom startup directory, use `add_to_path.zdotdir` for Zsh or `add_to_path.fish_config_dir` for Fish.

Set the optional `manifest_sha256` to the SHA-256 of the entire manifest file to pin it. In PowerShell, use `(Get-FileHash '.\Fleet tools.json' -Algorithm SHA256).Hash`.

```json
{
  "bundle": "Fleet tools.json",
  "name": "my-tools",
  "bin_directory": ".local/bin",
  "host": "dev.example.net",
  "user": "operator",
  "port": 22,
  "fingerprint": "SHA256:REPLACE_WITH_VERIFIED_HOST_FINGERPRINT",
  "home": "/home/operator",
  "add_to_path": { "shell": "bash" }
}
```

In Windows, open **Settings → Profiles → Audit SSH tool profiles**. Filter by profile name or SSH address, or choose **Show only tool sync issues**. Expand a profile's manifest details to see the path and any current and pinned digests. **Edit SSH tools** opens that profile's SSH tools editor. Use **Refresh audit** after saving changes.

Under **Audit scope**, choose one architecture or **Verify all manifest architectures** to check local executable files and the download cache. Expand the build details for each architecture to see verified and missing files, hash or ELF errors, and commands that have no build for that architecture. The issues filter includes profiles where an architecture has a command with no verified local build. **Refresh audit** repeats the selected check.

Run `audit-tool-profiles` to check saved SSH tool approvals offline. It uses the normal profile catalog unless you pass `--profiles CATALOG`. Add `--profile NAME` to check one saved SSH profile. The report includes the pinned and current manifest digests. Its statuses are `matches-pin`, `pin-mismatch`, `follows-file`, `manifest-error`, `manifest-scope-mismatch`, `invalid-settings`, `disabled`, and `not-run`. A separate `path_error` flags locally detectable errors in the optional Shell PATH approval. The command exits successfully when it can produce a report, even when rows contain problems. The audit reads the catalog and manifests. Login checks the authenticated account, remote home, executable builds, and startup files.

```powershell
$audit = & $ctl audit-tool-profiles | ConvertFrom-Json
$audit.result.profiles | Where-Object status -eq 'pin-mismatch'
```

Add `--verify-builds` to verify the pinned SHA-256 and Linux ELF format and architecture of local executables and cached downloads. It checks the architectures declared in each approved manifest. Add `--architecture x86_64` or `--architecture aarch64` to target one architecture, including one absent from the manifest. Each `build_checks` entry reports whether every command has at least one verified build for that architecture. An empty build list means that command has no matching build. Individual builds report `verified`, `missing`, or `error` in manifest order. The check uses the installation budget of 256 MiB per architecture per manifest. It opens no SSH connections and fetches no assets. Login checks remote interpreter and library availability.

```powershell
$audit = & $ctl audit-tool-profiles --verify-builds --architecture aarch64 | ConvertFrom-Json
$audit.result.profiles | Where-Object {
    $_.build_checks | Where-Object { !$_.locally_available }
}
```

Use `rollout-tool-profiles` to adopt one revised manifest in multiple saved SSH profiles. Select each profile explicitly. Each must already approve the candidate's bundle name and launcher directory. The preview lists the candidate's SHA-256 and any detectable additions, removals, and build changes. A missing or changed baseline is marked separately. Pinned profiles receive the new manifest hash, and profiles that follow the file keep that setting.

```powershell
$rollout = @('rollout-tool-profiles', '--candidate', '.\Fleet revision.json', '--profile', 'East', '--profile', 'West')
$preview = & $ctl @rollout | ConvertFrom-Json
$preview.result.profiles
& $ctl @rollout --review $preview.result.review
```

Use `--profiles CATALOG` to select another catalog. The apply step rechecks the candidate, readable baselines, and selected approvals before writing the entire batch. It changes the manifest bindings in the catalog. The revised tools sync when those profiles open a new ordinary SSH connection.

On an ordinary SSH connect or reconnect, w11term checks the verified endpoint and SFTP home, syncs the approved bundle, and then opens the shell. An unavailable or unconfirmed sync still allows login. The 20-second deadline covers the bundle and PATH work together. The `tool-bundle-sync` event reports the bundle outcome and, when configured, the PATH outcome. Bash and Zsh use the login and interactive startup files. Fish uses one block in the selected `config.fish` and retains the command order of the existing PATH list. An edited block requires a fresh review. Game sessions, durable attachments, and explicit workspace startup commands use their own startup paths. [Fish PATH variables](https://fishshell.com/docs/current/tutorial.html#path)

**More → Command reference** shows the last login sync for the selected SSH connection. Open **Install SSH tools** for the PATH result and any error detail. Manual installs and rollbacks show their own results in the installer and version history.

When the login PATH update is skipped, **Review PATH repair** checks the approved startup files over the current SSH connection. The review shows each file and the current and proposed text for any managed block it would replace. **Apply reviewed PATH repair** validates the review and writes the changes for new shells. Replacement requires one complete marker pair and preserves the text around it. Duplicate or incomplete marker pairs require manual correction.

A manual rollback, addition, or removal pauses automatic bundle updates. To hold the current release, select it in **Bundle versions**, choose **Review update pause**, and then **Pause automatic updates**. The same release stays installed and future SSH logins skip sync for that bundle on the account. To re-enable sync, select the active release marked **Updates paused**, choose **Review update resumption**, and then **Resume automatic updates**. Approved profiles can sync the bundle on their next SSH connection. Use the manual installation flow to apply a specific bundle immediately. **Disable tool sync** stops automatic updates from that profile.

For scripts, use `preview-tool-bundle-pause NAME SHA256` followed by `pause-tool-bundle-updates NAME SHA256 --review TOKEN`. The pause review applies to the reviewed active release and reports `automatic_sync_paused`. The corresponding resume commands are `preview-tool-bundle-resume` and `resume-tool-bundle-updates`.

```powershell
$resume = (& $ctl --pid 12345 sftp --pane $pane preview-tool-bundle-resume my-tools $activeSha256 | ConvertFrom-Json).result.sftp
& $ctl --pid 12345 sftp --pane $pane resume-tool-bundle-updates my-tools $activeSha256 --review $resume.review
```

### Open SSH diagnostics

**Recent files** in **Diagnostics overview** remembers the last 12 Linux triage reports, folded CPU profiles, packet captures, or syscall trace sets opened or saved in the Windows app. The list survives app restarts. Use the file actions menu to **Pin** up to 12 entries in a separate section. **Unpin** moves an entry back to recent files. **Copy path** copies the full Windows path. For split syscall traces, **Copy paths** copies one path per line in the original selection order, and reopening loads the same set. Packet captures reopen in the capture panel, where you can choose **Open in Wireshark**. **Forget** removes the entry from the list and leaves the artifact on disk.

Choose **Use as baseline** on a saved CPU profile, triage report, or syscall trace. Then choose **Compare with baseline** on another entry of the same kind to open the two files directly in their comparison viewer. The baseline is the Before or Working side. The selection stays in the current window until you replace it, choose **Clear baseline**, or remove the entry from the list.

Open **More → Diagnostics → Diagnostics overview** or search for **Diagnostics overview** in the command palette. The overview shows the connected SSH endpoint for the focused pane and updates when you switch panes. Use its entries to open Linux performance triage, CPU flame graph, packet capture, or the syscall trace viewer. The tools retain their direct command-palette entries.

Use **Back** in the triage, flame graph, or packet capture panel to return to the overview. Open saved reports, folded stacks, and syscall traces even when you have no active SSH connection. Each live capture still has its own target review and start action.

In **Diagnostics overview**, choose **Open saved diagnostic…** or drop files into the labelled area. The overview recognizes `.folded`, `.collapsed`, triage `.json`, `.pcap`, `.pcapng`, `.cap`, `.strace`, and split `name.PID` traces. Select one saved file or all the parts of a single split trace. Each viewer validates the file before adding it to recent diagnostics.

Choose **Compare saved diagnostics…** to browse for two CPU profiles, Linux triage reports, packet captures, or syscall traces. Pick the baseline first. It becomes **Before** in a CPU, triage, or packet comparison and **Working** in a syscall comparison. For a split trace, select all the files for that run in its dialog. Cancel either dialog to return to the overview. For files already in **Recent files** or **Pinned files**, choose **Use as baseline** on one file, then **Compare with baseline** on another of the same type.

Use `w11term.exe` to launch directly into the corresponding offline viewer. Paths can be relative to the working directory or absolute. Repeat `--open-syscall-trace` for all parts of a split trace. A diagnostics-only window uses the full width and starts without opening a terminal or applying the startup profile.

```powershell
.\w11term.exe --open-cpu-profile '.\worker profile.folded'
.\w11term.exe --open-triage-report '.\linux-triage.json'
.\w11term.exe --open-packet-capture '.\capture.pcapng'
.\w11term.exe --open-syscall-trace '.\run.321' --open-syscall-trace '.\run.322'
```

To start in the comparison view, supply the Before or Working files with the `--open-*` option and the After or Failing files with `--compare-with`. For split syscall traces, repeat the option for each side independently. The viewer adds both sources to recent diagnostics. Use **Back to flame graph**, **Back to report**, **Back to capture**, or **Back to working trace** to return to the baseline.

```powershell
.\w11term.exe --open-cpu-profile '.\before.folded' --compare-with '.\after.folded'
.\w11term.exe --open-triage-report '.\before.json' --compare-with '.\after.json'
.\w11term.exe --open-packet-capture '.\before.pcap' --compare-with '.\after.pcapng'
.\w11term.exe --open-syscall-trace '.\working.321' --open-syscall-trace '.\working.322' --compare-with '.\failing.411' --compare-with '.\failing.412'
```

Use `open-diagnostic` to open files in a running instance. Get the target ID with `list-windows`. The control command preserves desktop focus and returns the window ID, artifact kind, and whether it loaded a comparison. A load error produces a failed reply. Repeat `--file` or `--compare-with` for the parts of a split syscall trace.

```powershell
& $ctl --pid 12345 list-windows
& $ctl --pid 12345 open-diagnostic --window 1 --kind cpu-profile --file '.\before.folded' --compare-with '.\after.folded'
& $ctl --pid 12345 open-diagnostic --window 1 --kind packet-capture --file '.\before.pcap' --compare-with '.\after.pcapng'
```

**Activity** shows jobs started from the app in this window, with the original SSH endpoint, current state, and the latest result still available in each tool. Packet and syscall captures report their stop reason and saved path. Select an activity to return to the running job or its result even after switching panes. Linux performance triage shows the live count of checks that have finished, including checks that returned an error. **Stop and keep report** ends the collection early. The report retains collected output, labels unfinished checks as incomplete, and records that you stopped the collection.

### Capture Linux packets and open Wireshark

In the Windows app, select a connected Linux SSH pane and open **More → Diagnostics → Packet capture** or search for it in the command palette. Choose the interface, optional BPF filter, snap length, duration, and byte budget. Turn on **Use RAM-backed scratch storage** to require tmpfs. **Review packet capture** shows the verified host, effective packet limit, scratch location, and tcpdump arguments. **Capture and save…** starts the capture after you choose a new Windows file. **Stop and save** asks the running tcpdump to finish writing its capture, then transfers the verified file. The result reports why the capture ended and how many packets the kernel dropped when tcpdump reports that count.

Expand **Capture settings files** to save the current inputs as a new JSON file or load settings from an earlier run. Loading fills the editable fields and clears the previous review. Select the SSH pane and review the settings for that connection before capturing. The file contains `schema_version: 1` and `options`, using the packet capture request's option fields. It carries the interface, filter, duration, snap length, packet and file limits, byte budget, and scratch settings.

Use `--settings PATH` in place of the capture option flags in both CLI commands. Pass the pane, review ID, connection generation, and output path as usual. Both commands read the settings file when called. If its capture options change after preview, run the review again.

Use `save-packet-capture-settings` to create the same JSON from the capture flags without launching w11term or connecting to SSH. With `--output-file`, it saves a new file and returns its path in the JSON reply. Omit the option to print the settings to stdout.

```powershell
$settings = '.\packet-capture-settings.json'
& $ctl save-packet-capture-settings --interface lo --filter 'udp and port 4321' --seconds 15 --output-file $settings
$plan = (& $ctl --pid 12345 preview-packet-capture --pane 1 --settings $settings | ConvertFrom-Json).result.plan
& $ctl --pid 12345 capture-packets --pane 1 --settings $settings --connection-generation $plan.connection_generation --review-id $plan.review_id --output-file '.\after.pcap'
```

Choose **Save capture report…** on a completed capture to keep a JSON report alongside the PCAP. It is available from the completed result after the SSH pane closes. The report includes the filename, byte count, SHA-256, stop reason, available packet counters, and rolling-buffer outcome. Its `reviewed` section records the verified SSH endpoint and host key fingerprint, the settings, the resolved scratch location, and the effective tcpdump arguments.

Pass `--report-file '.\after.pcap.json'` to `capture-packets` to save the report automatically. The reply includes `result.report_path` after export. If the report destination becomes unavailable during capture, the verified PCAP remains saved and the reply includes `result.report_error` alongside `result.capture`. Choose a new path for each file.

Open the capture and choose **Verify capture report…**, or use `verify-packet-capture-report CAPTURE REPORT` from a shell. Either check compares the file's byte count and SHA-256 with the selected report and works offline. You can verify a moved or renamed capture. Windows shows the recorded host and host key, interface, filter, snap length, stop reason, and available drop and rotation counts after a match. The CLI returns the two absolute paths, byte count, digest, and the report's descriptive metadata in `result.details`. A digest-only report returns `details: null`. Invalid digests and mismatches return a nonzero exit code and a JSON error. The digest checks the capture file itself. The host, settings, and counters are the values recorded in the selected report.

```powershell
& $ctl verify-packet-capture-report '.\after.pcap' '.\after.pcap.json'
```

Choose **Open in Wireshark** after saving. w11term looks in the standard Windows Program Files folders. Use **Choose Wireshark executable…** for another installation. The saved capture stays available after you disconnect the SSH pane or close the packet capture panel.

Choose **Open capture file…** in the same panel to reopen a local PCAP or PCAPNG file from an earlier session. This works without SSH. The panel shows the format, file size, packet count, captured bytes, truncated packets, and interface link types. The timestamp span uses the earliest and latest timestamps and shows how many packets were timestamped. The scan stops after one million records or blocks and reads only a bounded prefix of each packet. A partial summary shows the reason it stopped. **Open in Wireshark** opens the original file for packet analysis. [PCAP format](https://datatracker.ietf.org/doc/draft-ietf-opsawg-pcap/) · [PCAPNG format](https://datatracker.ietf.org/doc/draft-ietf-opsawg-pcapng/) · [Link types](https://www.iana.org/assignments/pcap)

Use `inspect-packet-capture` to read the same summary as schema-version-1 JSON without a running w11term instance. The report records the local filename, numeric link types, and the timestamp span in nanoseconds as a decimal string. A missing span is `null`. An incomplete scan returns the verified prefix with `complete: false` and the reason in `issue`. A file with an invalid capture header returns a nonzero exit code. Use `--output-file` to save the report to a new file and get its absolute path in the CLI reply.

The capture and interface summaries show captured bytes and original packet bytes. Original bytes sum each observed record's packet length before snap-length truncation. For legacy records with a shorter original length field, the total uses the captured length. Use the interface and OS drop counters to check for capture loss when a file provides them. JSON includes `original_bytes` in the overall and per-interface totals. [PCAP packet records](https://ietf-opsawg-wg.github.io/draft-ietf-opsawg-pcap/draft-ietf-opsawg-pcap.html#section-5) · [PCAPNG packet blocks](https://ietf-opsawg-wg.github.io/draft-ietf-opsawg-pcap/draft-ietf-opsawg-pcapng.html#section-4.3)

Add `--all-conversations` to include every tracked IP and transport pair, or use `--filter-conversations 'udp 443 192.0.2.0/24'` to search both lists with the same terms as the app. The optional `conversation_results` object contains `ip` and `transport` arrays ranked by captured bytes. A search with no matches returns empty arrays. The existing top-ten arrays and scan coverage remain available in the same report.

Use **Saved capture interfaces** when a capture contains multiple interfaces. Each entry shows the recorded name when available, link type, snap length, packet count, bytes, truncation, and timestamp coverage. The JSON `interface_details` array includes the same breakdown. PCAPNG sections are numbered starting at one, and interface IDs start at zero within each section. A classic PCAP file has one implicit interface with a `null` section. A `null` snap length means the PCAPNG interface has no limit.

If a PCAPNG file records interface statistics, the panel shows the newest timestamped counter snapshot for that interface. These cumulative counters can report packets received, accepted by the filter, delivered, dropped by the interface, or dropped by the OS. The JSON `capture_statistics` object includes the snapshot time as `timestamp_unix_ns`, a decimal nanosecond string. Missing counters are `null`. Each snapshot is reported once, and the packet-record totals still describe the packets stored in the file. [PCAPNG Interface Statistics Block](https://datatracker.ietf.org/doc/html/draft-ietf-opsawg-pcapng-06#section-4.6)

Expand **Packet timeline** to see packet activity across the whole capture, with the peak bucket and its captured bytes. The timeline uses up to 64 equal-width buckets of at least one microsecond, including empty intervals, and counts packets that have usable timestamps. The JSON `timeline` object gives the start and bucket width as decimal nanosecond strings, followed by packet and byte counts for each bucket. Bucket intervals include their start and exclude their end. A capture without usable timestamps has a `null` timeline. The timeline covers the scanned prefix when the report is partial.

Select a populated interval, then choose **Open interval in Wireshark** to open the original capture with a UTC display filter for that interval. The selector starts at the peak bucket. **Open in Wireshark** opens the full capture. [Wireshark display filters](https://www.wireshark.org/docs/man-pages/wireshark-filter.html)

Expand **IP protocols** for packet and captured-byte counts by outer IP protocol. IPv4 uses its protocol field. IPv6 follows supported extension headers within the first 256 captured bytes and counts unresolved protocol chains separately. The panel also reports unclassified records and identified fragments. It counts observed SYN without ACK, SYN-ACK, RST, and FIN flags when the full TCP header is available, including its options. Later fragments contribute protocol counts when the protocol can be identified. The JSON `ip_protocols` object exposes the same data, with numeric protocol keys in `by_number`. The counts describe captured packets and may include retransmissions. [IANA protocol numbers](https://www.iana.org/assignments/protocol-numbers) · [IPv6 extension headers](https://www.rfc-editor.org/rfc/rfc8200.html#section-4) · [TCP header](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.1)

Expand **IP conversations** to browse tracked outer IPv4 or IPv6 address pairs ranked by captured bytes, with totals and bytes in each direction. This works offline with Ethernet (including up to four 802.1Q or 802.1ad tags), Linux cooked v1 and v2, raw IP, and BSD or Npcap loopback captures. Each captured record contributes once, across all interfaces, with the link header included in its recorded byte count. The panel reports packets without classifiable IP headers and packets omitted after the first 4,096 distinct pairs. A tracked pair continues to accumulate traffic after that limit. JSON lists coverage and the top ten pairs under `ip_conversations`, with packet and byte counts in both `a_to_b` and `b_to_a`. [Link types](https://www.iana.org/assignments/pcap)

Expand **TCP and UDP conversations** to browse tracked address-and-port pairs ranked by captured bytes. Each row combines both traffic directions for one protocol and endpoint pair. The scan tracks the first 4,096 distinct transport pairs independently of the IP conversation list. The coverage count includes classified TCP or UDP packets whose port header could not be used, such as later fragments or truncated transport headers. Packet and byte totals include only records with a usable port header. JSON lists coverage and the top ten pairs under `transport_conversations`. [TCP header](https://www.rfc-editor.org/rfc/rfc9293.html#section-3.1) · [UDP header](https://www.rfc-editor.org/rfc/rfc768.html)

Use **Previous** or **Next** to browse ten pairs at a time. **Filter IP conversations** and **Filter port pairs** search every tracked pair in their respective lists. Enter an exact IP address, address prefix, or CIDR. The transport filter also accepts TCP or UDP, ports, and exact address-and-port endpoints. Separate terms with spaces to require all of them. Select any matching row to open it in Wireshark or save its packets as a capture excerpt.

Select a TCP or UDP row and choose **Open port pair in Wireshark** to open the original capture at that protocol and both address-and-port endpoints, in either direction. The filter selects the transport associated with the outer IP header. Wireshark uses its own dissection and reassembly preferences when displaying the packets. [Wireshark layer filters](https://www.wireshark.org/docs/man-pages/wireshark-filter.html#_the_layer_operator)

Select a ranked pair and choose **Open conversation in Wireshark** to open the original capture with both directions of that outer IP pair. The filter uses the layer operator in Wireshark 4.0 or later. [Wireshark layer filters](https://www.wireshark.org/docs/man-pages/wireshark-filter.html#_the_layer_operator) · [Wireshark 4.0 release notes](https://www.wireshark.org/docs/relnotes/wireshark-4.0.0.html)

Turn on **Limit to timeline interval** to apply the selected packet interval as well. The checkbox shows the start and end relative to the timeline start. The interval includes its start and excludes its end. Choose a different interval in **Packet timeline** to change those bounds. The selection remains in effect when the timeline is collapsed. The checkbox starts off for each opened capture.

In **TCP and UDP conversations**, **Limit port pair to timeline interval** controls the same time limit for both conversation views in that capture.

Choose **Compare captures…** to use the displayed capture as **Before** and choose a second PCAP or PCAPNG as **After**. The comparison shows each capture's packet totals, timestamp span, and coverage. It lists changes across all tracked IP pairs, with packet and byte counts in each direction. Deltas subtract **Before** from **After** and describe the two capture windows as recorded. Partial scans withhold deltas, and pairs whose counts could be hidden by a scan or tracking limit show unavailable coverage. Pair counts describe decoded outer IP headers. Non-IP or undecodable packets contribute to capture totals and the unclassified count. **Back to capture** returns to the original summary.

**Average rates over each recorded timestamp span** shows packets, captured bytes, and original packet bytes per second for each file. Each rate divides the observed total by the time between that file's earliest and latest packet timestamps. Original bytes and their rates help compare files recorded with different snap lengths, including comparisons limited to an elapsed interval. A side needs a complete scan, timestamps on every packet, and a positive span. Conversation averages for packets and captured bytes use the same full-file spans, so a pair with identical packet and byte counts can appear if its average rate changed. Transport averages also require usable TCP and UDP port coverage. JSON reports each scope's `average_rates` with `before`, `after`, and the difference when both rates are available. Capture-wide and elapsed-interval rates include `original_bytes_per_second`. It lists pairs whose counts stayed the same under `rate_only_changes`.

Expand **Elapsed packet timeline** to compare bursts on two charts with the same interval widths and packet-count scale. Each file's first timestamped record marks zero. Out-of-order records can appear at negative offsets. Choose a **Start interval**, then use **Range ends at** to include more consecutive intervals. The charts highlight the range and show the observed packet and byte totals for both files. The charts count timestamped records from the scanned portion. Intervals between a side's occupied buckets can show zero, and intervals outside its occupied span show unavailable coverage. The Before and After Wireshark buttons open the selected range on each file's original clock when that side has packets in it. Text and JSON exports include the aligned intervals. JSON `timeline` gives the two Unix timestamp origins, shared start offset and bucket width as decimal nanosecond strings. Each bucket contains both observations and uses `null` for a side outside its timestamped coverage.

Expand **IP protocol changes** to see which decoded protocols account for changed packet or byte totals. TCP flag observations show counts and decoded-header coverage for each file. TCP flag deltas require complete scans, no unresolved IP protocol chains, and usable headers for every classified TCP packet on both sides. Text and JSON exports include these observations. JSON uses `protocol_changes` and `tcp_flag_comparison`.

Set **Conversation type** to **TCP and UDP ports** to compare changed endpoint pairs across all tracked ports, including changes in traffic direction. Each row identifies its protocol and both endpoints. Deltas require complete scans, resolved IP protocol chains, and usable port headers for all observed TCP and UDP packets on both sides. If the tracking limit leaves a pair's counts unknown, that side and its delta show unavailable coverage. Counts for pairs tracked on both sides remain comparable when the limit is reached. JSON includes these rows under `transport_changes`.

**Filter conversations** searches across all pages in the selected view. Enter an IP address, address prefix, or CIDR, such as `192.0.2.0/24` or `2001:db8::/32`. The transport view also accepts TCP or UDP, an exact port, or an address and port such as `[2001:db8::1]:443`. Separate terms with spaces to require all of them, such as `tcp 443 192.0.2.0/24`. The count shows how many conversations matched. **Clear conversation filter** restores the full list. Exports always include the full comparison.

Choose **Export comparison…** to save a new Text or JSON report. The report includes both capture filenames, totals, scan coverage, and every changed IP and transport pair across all pages. Use `compare-packet-captures BEFORE AFTER [--format json|text] [--output-file PATH]` to produce the same report offline. JSON is the CLI default and represents unavailable counts and deltas as `null`. Each file is subject to the same scan limits as `inspect-packet-capture`.

In the comparison, select a changed IP or transport pair, choose **Before capture** or **After capture**, and choose **Open conversation in Wireshark**. The source defaults to **After capture**. Wireshark opens the selected original file with both directions of the pair, including when the summary reports zero packets or unavailable coverage for that side. A transport row also filters by the protocol and both ports. Use **Choose Wireshark executable…** to select an installation from the comparison view.

Turn on **Limit to elapsed interval** to apply the selected interval from **Elapsed packet timeline** to the conversation. The same elapsed bounds are translated to the chosen file's clock. The checkbox remains active when the timeline is collapsed. A capture without timestamped records cannot open with the interval applied. Clear the checkbox to open the whole conversation. The interval affects Wireshark only, and the comparison and its exports still cover both files in full.

Choose **Compare selected interval** in **Elapsed packet timeline** to recompute the report for the highlighted range. Counts, protocol and conversation changes, and exports use only timestamped packets reached by the scan at or after the start and before the end. Rates divide by the full range's width. A side needs a complete scan, timestamps on every source packet, and a recorded span that covers the range for rates and signed changes. A scoped conversation opens in Wireshark at the report's range. **Compare whole captures** restores the full report. The equivalent CLI option is `compare-packet-captures BEFORE AFTER --elapsed-ns START:END`, using signed integer nanosecond offsets from each file's first timestamped record. JSON includes both offsets, both absolute ranges, and timestamp coverage in `window`.

Choose **Save capture excerpt…** in a saved capture's timeline, or **Save Before excerpt…** or **Save After excerpt…** in an elapsed comparison. The new capture preserves selected packet bytes, order, and original timestamps in PCAP or PCAPNG. PCAPNG retains the interface definitions needed by those packets and omits capture-wide statistics, comments, decryption-secret blocks, and unrelated metadata. The writer scans the source within the usual record limits and publishes a new file only when the scan succeeds and packets match. For a scripted export, use `clip-packet-capture FILE --elapsed-ns START:END --output-file PATH`. The signed nanosecond bounds are relative to the first timestamped record, include the start, and exclude the end. [PCAPNG format](https://www.ietf.org/archive/id/draft-ietf-opsawg-pcapng-06.html)

Select a conversation and choose **Save conversation excerpt…** or **Save port pair excerpt…** to save both directions of that pair. A comparison uses the selected Before or After source. The export honors an active time limit or the current elapsed comparison window. An IP pair uses the outer IP addresses. A TCP or UDP pair also requires the recorded transport header to contain both ports. Add `--ip-pair A,B`, `--tcp-pair A:PORT,B:PORT`, or `--udp-pair A:PORT,B:PORT` to `clip-packet-capture`. Use brackets around IPv6 addresses with ports. Omit `--elapsed-ns` to select the pair across the whole source, including matching records without usable timestamps.

```powershell
$summary = & $ctl inspect-packet-capture '.\loopback.pcap' | ConvertFrom-Json
$summary.packets
& $ctl inspect-packet-capture '.\loopback.pcap' --output-file '.\capture-summary.json'
```

Choose **List capture interfaces** to see the devices that tcpdump reports for the selected SSH connection. Selecting a device fills the editable interface field. You can also type a name directly. From the CLI, `w11term-cli [--pid PID] list-packet-interfaces --pane ID` returns device names, details, and the connection generation. [tcpdump device listing](https://github.com/the-tcpdump-group/tcpdump/blob/master/tcpdump.1.in)

Use a connected Linux SSH pane and review the interface, literal tcpdump filter, limits, and scratch location. Pass the same capture options and the returned connection generation and review ID to record. The host needs tcpdump, Bash, GNU coreutils, flock, the SSH SFTP subsystem, and permission to capture on the selected interface. [tcpdump manual](https://github.com/the-tcpdump-group/tcpdump/blob/master/tcpdump.1.in)

```powershell
$packetOptions = @('--pane', $pane, '--interface', 'lo', '--filter', 'udp port 4096', '--seconds', '10', '--max-mib', '8', '--tmpfs')
$plan = (& $ctl --pid 12345 preview-packet-capture @packetOptions | ConvertFrom-Json).result.plan
$plan
$job = [guid]::NewGuid().ToString('N')
& $ctl --pid 12345 capture-packets @packetOptions --connection-generation $plan.connection_generation --review-id $plan.review_id --job-id $job --output-file '.\loopback.pcap' --wireshark 'C:\Program Files\Wireshark\Wireshark.exe'
```

Run `stop-packet-capture --pane ID --connection-generation N --job-id HEX` from a separate prompt to finish the matching capture early. Use the pane, connection generation and job ID from the capture command. Choose a fresh job ID for each run. The stop reply confirms the request, and the original command returns the saved capture and its stop reason. Omit `--job-id` when a script only needs to wait for the automatic packet or time limit.

Defaults are 15 seconds, a 192-byte snap length, a 16 MiB budget, and `/tmp` scratch storage. Use `--tmpfs` to verify a RAM-backed mount, which defaults to `/dev/shm`, or choose a path with `--scratch-directory`. The worker requires 1 MiB of free-space headroom and applies a shared Linux-account capture lease. The effective packet limit is the smaller of `--packet-limit` and the number of maximum-size packet records that fit in the budget. A time limit interrupts tcpdump so it can finish writing and report drops.

Set **Rolling capture files** in the Windows panel or pass `--rotate-files 2..16` to both preview and capture to keep a rolling buffer. The total byte budget is divided into equal per-file limits, with room for a full packet record in every file. On the host, a Python 3.8+ worker reads tcpdump’s classic pcap stream and reuses the oldest slot when the next record would exceed the current file’s limit. The capture can continue until the duration or explicit packet limit is reached. The review shows the slot count and per-file budget.

Windows verifies the hashes, headers, and packet records of the retained files, then saves them in capture order as one `.pcap`. **Stop and save** works with the rolling buffer. The result reports the total packets captured, packets saved and files overwritten. [tcpdump capture output](https://github.com/the-tcpdump-group/tcpdump/blob/master/tcpdump.1.in) · [libpcap savefile format](https://github.com/the-tcpdump-group/libpcap/blob/master/pcap-savefile.manfile.in)

SFTP transfers the completed file over the existing SSH connection. w11term checks its size, SHA-256, and packet records before publishing a new local file and reports the packet count, stop reason, and available drop statistics. The private scratch files expire after the transfer lease or are cleaned up after verified receipt. `--wireshark` is optional and launches Wireshark on Windows after saving. If it cannot launch, the reply includes the saved path and the launch error. [Wireshark command-line options](https://www.wireshark.org/docs/wsug_html_chunked/ChCustCommandLine.html)

### Capture Linux syscalls

In the Windows app, open **More → Diagnostics → Syscall trace viewer → Capture over SSH…**. Select a connected SSH pane, enter a Linux executable and its arguments as a JSON array of strings, and choose a working directory, preset and limits. **Review syscall capture** verifies the endpoint and shows the resolved executable and exact strace arguments. **Trace and save…** asks for a new Windows file before starting. **Stop syscall capture** ends the launched command and saves the records collected so far. The viewer opens the verified trace automatically and shows the stop reason.

Use a connected Linux SSH pane with SFTP and a version of strace that supports `--kill-on-exit`. Review the working directory, executable identity, exact arguments, and capture bounds first. `--arg` can be repeated and each value is passed literally. The command runs as the remote SSH account with stdin, stdout, and stderr disconnected from the terminal.

```powershell
$traceOptions = @('--pane','1','--program','/usr/bin/ls','--arg','-l','--arg','/tmp','--directory','/tmp','--filter','files','--seconds','15','--max-mib','4')
$plan = (& $ctl --pid 12345 preview-syscall-capture @traceOptions | ConvertFrom-Json).result.plan
$job = [guid]::NewGuid().ToString('N')
& $ctl --pid 12345 capture-syscalls @traceOptions --connection-generation $plan.connection_generation --review-id $plan.review_id --job-id $job --output-file '.\example.strace'
```

From a separate prompt, `stop-syscall-capture --pane 1 --connection-generation GENERATION --job-id JOB` requests an early stop on the matching job. Choose a fresh job ID for each run. Stop, the deadline, and the byte limit terminate the launched command and traced descendants. The result gives the stop reason and reports the command's exit status when it completes on its own. A file that reaches the byte limit can end partway through a record.

The original PID-tagged trace includes per-call timing and decoded descriptor details. One byte budget covers the full stream across descendants. w11term checks the SFTP download's size and SHA-256 before acknowledging remote cleanup and saving a new local file. [strace manual](https://github.com/strace/strace/blob/master/doc/strace.1.in)

Open **More → Diagnostics → Syscall trace viewer** to inspect a saved trace offline. Open one combined log or select the files from the same `strace -ff` run together. Names ending in `.PID` identify the process for those split logs. w11term merges files with epoch timestamps. It also merges short runs with ordered time-of-day timestamps that fit in a window under 12 hours. A merge across midnight requires at least one file to show the rollover in its record order. When timestamps are ambiguous or incomplete, the viewer keeps the files in their input order. Capture with `-ttt` to retain dates in longer runs. [strace timestamp formats](https://github.com/strace/strace/blob/master/doc/strace-log-merge.1.in)

For a combined `strace -r` log, the viewer adds up the record deltas to show elapsed time from the first timed record. Resumed calls keep their entry time and contribute their resume delta to subsequent records. A missing delta on a trace event makes subsequent elapsed times unavailable. The viewer uses the absolute clock when both absolute and relative timestamps are present. Split files with relative timestamps alone keep their file order. [strace relative timestamps](https://github.com/strace/strace/blob/master/doc/strace.1.in)

The viewer groups adjacent calls with the same process, syscall, resource, and outcome. Each group shows its count and available call duration. Select a group to review its original records, including the two lines for an unfinished and resumed call. Search by PID, name, resource, or errno, choose a filter, and select **Apply filters**. A missing absolute path is marked when the same process later succeeds on that path with the same syscall. The original error stays visible. w11term reads up to 64 MiB or one million records per selection. Very long lines are clipped in the preview and remain available in the source file. [strace manual](https://github.com/strace/strace/blob/master/doc/strace.1.in)

Choose **Syscall timing summary** to rank syscalls by total recorded duration, slowest single call, call count, or error count. The summary starts with all processes. Enter a PID and choose **Apply summary PID** to inspect one process. Select a row for the slowest call's original records. The counts show how many calls had a duration and how many were incomplete. An unfinished call matched to a resume counts once. Recorded durations come from `strace -T` and can overlap across threads or processes.

Use `inspect-syscall-trace` for the same summary offline. Repeat `--trace` for the files of a split run. In the JSON, durations are decimal strings in nanoseconds or `null` when unavailable. The slowest call includes one-based file positions in argument order and original line numbers. Add `--output-file` to save the report to a new file.

```powershell
& $ctl inspect-syscall-trace --trace '.\trace.42' --trace '.\trace.43' --pid 42 --sort slowest
```

Choose **File descriptors** to follow open files, sockets and pipes. Search by path, `PID 123`, or `FD 4`. Reuse of the same descriptor number produces separate lifetimes. **Show related lifetimes** follows duplicates and inherited copies of the same open file description. Select a lifetime for the original calls that established, used, or closed it, including observed sharing of a descriptor table and close-on-exec. A lifetime first seen after open or still active when the available records end is labelled with the evidence available. Use the all-calls capture preset and retain timestamps when tracing several processes to capture more of these transitions. [Linux clone(2)](https://man7.org/linux/man-pages/man2/clone.2.html) · [Linux close(2)](https://man7.org/linux/man-pages/man2/close.2.html)

Open the working capture and choose **Compare with failing trace…**. The comparison pairs processes by the executable and observed ancestry, then aligns syscall sequences by name and resource. It uses recorded descriptor lifetimes and process relationships to interpret numbers that differ between runs. Select a change to see both original records, the result or count delta, and available timing relative to each run's start. Turn on **Include timing-only changes** to include calls that differ by at least 1 ms and by at least a quarter of the slower duration. Some alignments are marked uncertain when evidence is incomplete or several sequences could match. Use the evidence arrows to see each record inside a repeated group.

Choose **Use selected pair as time zero** to measure from the selected calls. Earlier calls show negative offsets and later calls show positive offsets in their respective runs. A pair needs a reliable match, equal group counts, and usable times on both sides. For example, two epoch-timestamped calls in a run can be compared when its first record has no timestamp. The reference stays fixed while you change the selection, filter, or page. Choose **Use run starts** to reset it.

Enter terms such as `openat ENOENT /tmp/settings` and choose **Apply change filter** to narrow the comparison by syscall, resource, executable, change type, or errno. Searches ignore case. All terms must match the change or its paired evidence. If an errno matches a call inside a repeated group, the viewer opens that pair of original records. Clear the field and apply again to restore all changes allowed by the timing setting.

Choose **Export comparison…**, then choose Text or JSON under **Save as type**. Both formats include all changes that pass the applied filter and timing setting across every results page, with counts, available duration deltas, and source file and line references. If you set a timing reference, both formats include it and the available offsets. Text includes the original lines for the currently selected evidence pair within the viewer's preview limits. JSON uses the CLI schema below and records the evidence pair you are viewing for the selected change.

Use `compare-syscall-traces` to compare saved runs from a script without a GUI or SSH connection. Repeat `--working` or `--failing` to supply each side's `strace -ff` files. The command uses the viewer's normalization and matching, including errno filters and the default of hiding timing-only rows. Add `--include-timing` to include those rows. Use `--anchor WORKING_FILE:LINE,FAILING_FILE:LINE` to choose a reliably aligned pair, including an unchanged call. File positions and line numbers start at 1. File positions follow the order of the arguments for each side. An unfinished and resumed call uses its entry time.

```powershell
$report = & $ctl compare-syscall-traces --working '.\working.strace' --failing '.\failing.strace' --filter 'openat ENOENT' | ConvertFrom-Json
$report.changes
& $ctl compare-syscall-traces --working '.\working.strace' --failing '.\failing.strace' --anchor '1:12,1:15'
& $ctl compare-syscall-traces --working '.\working.21' --working '.\working.22' --failing '.\failing.77' --failing '.\failing.78' --include-timing --output-file '.\syscall-comparison.json'
```

The schema-version-1 JSON includes the source file names, merge order, comparison limit status, and all matching changes. Each change contains counts, timing when available, alignment confidence, and the file and line references for the representative evidence pair or the pair matched by the filter. The `representative_working_result` and `representative_failing_result` fields describe the group. Read `working_evidence` and `failing_evidence` for the exact call that matched an errno filter and its `elapsed_ns` from the start of that run. A timing reference adds a top-level `timing_anchor` with the source of each call and a signed `offset_from_anchor_ns` to evidence with usable times. With `--output-file`, the command writes a new report file and returns its path at `result.path`. Errors return a nonzero exit code and a JSON reply on stderr.

### Collect Linux performance triage

In the Windows app, open **More → Diagnostics → Linux performance triage** or find it in the command palette. Select a connected SSH pane to review the endpoint and checks, then choose **Run triage**. Choose a resource under **Findings by resource**, then select a finding to see its source sample and output line when available. **Save report** writes a new JSON file. **Open report** reopens that format from the app, including reports collected by the CLI.

Findings summarize observed CPU activity and runnable tasks, available memory and swapping, disk activity and queueing, network traffic and TCP errors, and kernel-log evidence. Peaks use the collected interval rows and exclude the first vmstat row and sar averages. Missing or denied samples appear under their resource. The same findings are saved in the JSON report and can be derived when opening an earlier capture. [Linux performance checklist](https://www.brendangregg.com/Articles/Netflix_Linux_Perf_Analysis_60s.pdf) · [iostat metrics](https://github.com/sysstat/sysstat/blob/master/man/iostat.in) · [sar metrics](https://github.com/sysstat/sysstat/blob/master/man/sar.in)

Choose **Compare reports** to use the displayed report as **Before** and open a saved JSON report as **After**. The offline view shows changes in check status, sampling commands, and reported observations. Choose a change to see the two values and their source output lines or excerpts. Numeric deltas require matching sampling commands and complete output on both sides. Disk and interface deltas also require the same recorded endpoint and device name. **Back to report** restores the original sample selection.

Choose **Export comparison…**, then choose Text or JSON under **Save as type**. Both formats include all changes, both source names and coverage summaries, and the stored stdout and stderr text for each changed check. Output keeps its source line numbers so you can trace an observation back to either report. The file records whether the SSH endpoints match and uses only the report filenames from disk. JSON uses the same schema as the CLI.

Run `compare-linux-triage` with the before and after report paths to compare offline from a script. The default schema-version-1 JSON includes coverage, check and observation changes, signed numeric deltas with units when the inputs support them, and both source outputs for each changed check. Use `--format text` for the same report the viewer exports.

```powershell
$comparison = & $ctl compare-linux-triage '.\before.json' '.\after.json' | ConvertFrom-Json
$comparison.changes
& $ctl compare-linux-triage '.\before.json' '.\after.json' --format text --output-file '.\comparison.txt'
```

With `--output-file`, the command saves a new file and returns its path at `result.path`. Errors return a nonzero exit code and a JSON reply on stderr. The command requires no GUI or SSH connection.

Review the verified SSH endpoint, fixed sample commands and limits, then pass the reviewed connection generation when collecting. Choose a new Windows file for the report.

```powershell
$plan = (& $ctl --pid 12345 preview-linux-triage --pane $pane | ConvertFrom-Json).result.plan
$plan
& $ctl --pid 12345 linux-triage --pane $pane --connection-generation $plan.connection_generation --output-file '.\linux-triage.json'
$report = Get-Content -Raw '.\linux-triage.json' | ConvertFrom-Json
$report.collection.samples | Select-Object id, status, exit_status
```

The collector uses a sibling SSH channel with a 60-second client deadline and a 50-second remote collection deadline. It runs the ten checklist probes in parallel with Bash and GNU coreutils, captures up to 16 KiB per command per stream, and uses a private temporary directory. The report keeps the verified endpoint, commands, timing, output, and individual outcomes such as missing utilities or denied kernel logs. It marks streams that reached their byte limit and keeps the original bytes in base64 when they contain invalid UTF-8. [Linux Performance Analysis in 60,000 Milliseconds](https://www.brendangregg.com/Articles/Netflix_Linux_Perf_Analysis_60s.pdf)

### Review terminal text with OpenRouter

Open **Settings → OpenRouter assistance** to save an API key protected by Windows and enter a model ID. **Refresh model catalog** fetches the public model list. **Save OpenRouter settings** remembers the model and routing preference. Zero Data Retention routing is required by default, and each request asks OpenRouter to exclude providers that collect data. [OpenRouter routing and data collection](https://openrouter.ai/docs/guides/routing/provider-selection) · [Zero Data Retention](https://openrouter.ai/docs/guides/features/zdr)

Open **More → Draft a Bash command with OpenRouter** to describe a task, or select terminal text and choose **Explain selected text with OpenRouter**. To capture a pane without selecting text first, focus it and choose **Explain visible pane with OpenRouter** from **More** or the command palette. This snapshots the text rows then on screen, including a scrolled view, with no offscreen history or images. In a Linux performance report, **Review report with OpenRouter…** starts with local findings and excerpts from the selected sample.

In a triage comparison, **Review comparison with OpenRouter…** drafts an editable preview of the changes, coverage limitations, and paired evidence for the selected row. The default preview leaves out report paths, host addresses, usernames, and host fingerprints from the report headers. It keeps the comparison open beside the review so you can check the output against both sources.

In a syscall comparison, select a change and its evidence pair, then choose **Review comparison with OpenRouter…**. The draft shows the selected outcomes, relative timing and numbered source lines, plus a bounded list of changes within the applied filter. The preview includes normalized remote resource and executable names. Raw syscall lines and local filenames stay in the viewer. Use **Back to syscall viewer** to return to the same comparison.

In a CPU profile comparison, select a stack and choose **Review comparison with OpenRouter…**. The comparison stays beside the draft. The preview includes the two sample totals, the selected stack, and up to ten other stacks ranked by change in sample share within the applied filter. It uses symbol names and sample counts while leaving the local source names in the comparison. The review interprets shifts in sample share as relative measurements.

In a packet capture comparison, choose **Review comparison with OpenRouter…** to draft a summary of the whole captures or the elapsed interval currently being compared. The draft includes packet counts, rates, coverage, and the largest recorded protocol changes. Selecting a conversation adds directional counts and, for TCP or UDP, the two port numbers. IP addresses, filenames, and packet contents stay in the local files and viewer. The original comparison remains beside the editable draft.

Edit the preview to remove any details you want to keep local. The preview accepts up to 16 KiB of text. **Review OpenRouter request** shows the destination, model, and privacy policy. **Send reviewed request** submits the displayed text.

The response appears as selectable text. **Copy response** puts it on the Windows clipboard for manual use. Token usage appears when OpenRouter returns it. **Stop waiting** discards the eventual response in w11term. A request already sent can still finish at OpenRouter.

### Capture a Linux CPU profile

In the Windows app, select a connected Linux SSH pane and open **More → Diagnostics → CPU flame graph → Capture from SSH**. Enter a PID, duration, frequency, and unwind mode. You can also choose a process under **Busy processes during triage** in the CPU findings. That list ranks processes by their peak reported interval CPU usage and fills in the PID from the live report. [pidstat manual](https://github.com/sysstat/sysstat/blob/master/man/pidstat.in)

Choose **Review CPU profile** to check the live process and verified SSH endpoint. Then choose **Record CPU profile** and a new local folded stacks file. Changes to the settings require a fresh review. Recording stays tied to the pane, connection, and process start time from review. The resulting file opens in the flame graph with the unresolved frame count and **Perf output**, when available.

Choose **Stop and save profile** during a recording to finish early and save the collected samples. For an attached PID, the target keeps running. For a profiled command, the remote wrapper stops the workload it started. The viewer and Diagnostics Activity show the stop reason. A requested raw `.perf.data` file uses the same verification and save path as a timed capture.

Select **Keep raw perf recording** during review to save a `.perf.data` file beside the `.folded` file, for either capture target. The PID capture CLI accepts `--perf-data PATH` for a separate destination. w11term streams the raw file over the same SSH channel, checks its byte count and SHA-256, and saves it without replacing an existing file. The raw recording is capped at 32 MiB and can add up to 60 seconds for transfer. A verified raw file is saved even when folded stacks are unavailable.

Open the raw file on Linux with `perf report --stdio -i my-profile.perf.data`. Symbol resolution is best on the recorded host or with its matching binaries and debug symbols. Raw recordings can include process metadata and captured stack data. [perf report](https://github.com/torvalds/linux/blob/master/tools/perf/Documentation/perf-report.txt) · [Linux perf data collection](https://docs.kernel.org/admin-guide/perf-security.html)

To profile a fresh run, choose **Launch command** as the target. Enter a Linux executable, an optional absolute working directory, and arguments as a JSON array of strings such as `["--threads", "4", "input file"]`. Relative executable paths resolve from that directory. Bare names use the diagnostic PATH. The directory defaults to the SSH account's home. Review shows the resolved paths and literal arguments before launch.

The executable runs non-interactively in an SSH exec channel. Samples include its child processes, and the time limit stops processes in the workload's process group. The viewer reports the command's exit status or that it reached its time limit. It can open sampled stacks from a timed-out run. Command input and output are disconnected from the terminal, and perf diagnostics appear in **Perf output**. [perf record](https://github.com/torvalds/linux/blob/master/tools/perf/Documentation/perf-record.txt) · [GNU timeout](https://www.gnu.org/software/coreutils/manual/html_node/timeout-invocation.html)

For CLI capture, choose a running PID on the remote Linux host. Preview the verified SSH endpoint, process name and start time, sampling options, and file and time limits. Use the same options and the returned connection generation and process start ticks when starting the capture.

```powershell
$sampling = @('--pane', $pane, '--target-pid', 4102, '--seconds', 10, '--frequency', 99, '--call-graph', 'fp')
$plan = (& $ctl --pid 12345 preview-cpu-profile @sampling | ConvertFrom-Json).result.plan
$plan
& $ctl --pid 12345 capture-cpu-profile @sampling --connection-generation $plan.connection_generation --process-start-ticks $plan.process_start_ticks --output-file '.\cpu.folded'
```

The defaults are 10 seconds, 99 Hz, and frame pointers. Choose 1–30 seconds, 1–199 Hz, or `dwarf` for DWARF unwinding with an 8 KiB stack snapshot. The remote Linux host needs `perf`, Bash, GNU coreutils, and permission to sample the process. w11term samples user-space CPU activity on the selected process and checks that its start time matches before and after recording. It runs under the signed-in SSH account. [perf record](https://github.com/torvalds/linux/blob/master/tools/perf/Documentation/perf-record.txt) · [Linux perf permissions](https://docs.kernel.org/admin-guide/perf-security.html)

The capture uses a sibling SSH exec channel, with up to 32 MiB for the temporary recording and 4 MiB for the `perf script` output. The worker has a deadline and its private files are removed at completion. w11term folds the captured stacks into a new Windows file and reports the sample count, unresolved frames, and any diagnostics printed by perf, including lost-sample warnings when reported. [perf script](https://github.com/torvalds/linux/blob/master/tools/perf/Documentation/perf-script.txt)

#### View and share the flame graph

Open **CPU flame graph** from **More** or the command palette, then choose **Open folded stacks**. The viewer reads local UTF-8 folded stacks from w11term and tools that use the same format. It works offline and accepts files up to 16 MiB.

Width represents samples. Callers are below their callees. Hover for the sample count and share of the full capture, or click a frame to zoom. Use **Up** to return to its caller and **Reset zoom** for the full capture. **Shallower** and **Deeper** move through stacks taller than the panel. Search highlights symbols and counts each matching sample once, even when multiple frames in that stack match. Use **Previous** and **Next** to zoom to matches by sample count. **Export SVG** saves the current branch at every depth to a new file. [CPU flame graphs](https://www.brendangregg.com/FlameGraphs/cpuflamegraphs.html)

Choose **Compare profiles** to use the open recording as **Before** and pick a second folded file as **After**. The offline comparison ranks exact call stacks by their change in share of total samples. Each sample contributes to its full recorded stack once. The detail shows both counts, their shares, and the change in percentage points. Use recordings of comparable workloads and sampling configurations to interpret the result. Filter by any symbol in the call chain, or choose **Back to flame graph** to resume the original zoom and search. [Normalizing CPU profiles](https://www.brendangregg.com/blog/2014-11-09/differential-flame-graphs.html)

Select a change and choose **Inspect before flame graph** or **Inspect after flame graph** to open that recording at the selected call path. A graph shows inclusive samples for the frame and its callees. The comparison counts samples that ended at the exact stack. Browse or export the inspected graph, then choose **Back to comparison** to return to the same filtered list and selection.

Choose **Differential flame graph** to view both complete profiles. **After widths** shows the current recording, and **Before widths** reveals paths removed in the current recording. Width represents inclusive samples in the selected recording. Red, blue, and gray show an increase, decrease, or no change in exact-stack sample share from Before to After. Hover or zoom to read the signed change in percentage points. Each width view keeps its own zoom and search, and **Export SVG** preserves the active widths and differential colors.

Choose **Export comparison…** to save a new JSON file. Schema version 1 records both source file names and sample totals, the applied filter, and every matching change in the same order as the viewer across all pages. Each row includes its full stack as an array of original symbols, both sample counts and shares, and the signed change in percentage points.

Scripts can use `compare-cpu-profiles` with the same Before/After order, file size limit, and case-insensitive literal filter. The command requires no running w11term instance or SSH connection. It writes the same JSON report to stdout by default. With `--output-file`, it saves the report to a new file and prints a CLI reply whose `result.path` is the absolute destination. Errors return a nonzero exit code and a JSON error reply on stderr.

```powershell
$report = & $ctl compare-cpu-profiles '.\before.folded' '.\after.folded' --filter 'worker' | ConvertFrom-Json
$report.changes
& $ctl compare-cpu-profiles '.\before.folded' '.\after.folded' --output-file '.\comparison.json'
```

Use `--format svg` to draw a full differential flame graph offline. The graph defaults to After widths. Pass `--widths before` to see removed call paths. Both modes use the same colors and sample-share deltas as the Windows viewer. SVG goes to stdout unless you provide `--output-file`. The `--filter` option applies to JSON reports.

```powershell
& $ctl compare-cpu-profiles '.\before.folded' '.\after.folded' --format svg --output-file '.\after.svg'
& $ctl compare-cpu-profiles '.\before.folded' '.\after.folded' --format svg --widths before --output-file '.\before.svg'
```

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
