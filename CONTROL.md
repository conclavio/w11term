# External control

Build both executables with `cargo build --release --bins`. `w11term.exe` is the GUI; `w11term-cli.exe` is its console companion. The CLI writes one JSON reply to stdout and exits with code 1 on errors. It never starts a GUI or a fallback server.

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

`open` uses the most recently active window. `resize` routes to the window that owns the pane, including after a live tab transfer. Closing one window leaves the process pipe and the remaining windows’ sessions accessible. Pipe requests preserve desktop focus.

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

& $ctl --pid 12345 screenshot --pane $pane .\frame.png
& $ctl --pid 12345 save-text --pane $pane .\shell-transcript.txt
```

`send-text` requires `--no-paste`. It sends the supplied bytes directly to the SSH channel, with no bracketed-paste delimiters, key encoding, UTF-8 normalization, or escape expansion. `--base64`, `--file`, and `--stdin` accept arbitrary bytes including NUL and invalid UTF-8; the limit is 1 MiB per command. A successful reply acknowledges the write to the SSH channel or local ConPTY input pipe. The receiving program may still be processing it. Use `wait-for-text` to wait for output. A write timeout means delivery is unknown; retrying could duplicate input. Demo/replay panes reject raw input. Reconnecting panes reject raw writes and SFTP work rather than queuing them for a later connection.

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

Open a new pipe connection for each command. Write one UTF-8 JSON object followed by LF; read one JSON reply followed by LF. Maximum request size is 2 MiB; at most eight requests run concurrently. The initial read and reply write time out after ten seconds. Input/snapshot requests have a ten-second execution timeout; the CLI allows thirty seconds for a reply to accommodate rendering.

```json
{"verb":"list"}
{"verb":"send-text","pane":1,"no_paste":true,"data_base64":"Qk9BUkQN"}
{"verb":"get-text","pane":1}
{"verb":"inspect","pane":1}
{"verb":"save-text","pane":1,"path":"C:\\tests\\shell-transcript.txt","overwrite":false}
{"verb":"screenshot","pane":1,"path":"C:\\tests\\frame.png"}
```

Direct protocol screenshot paths must be absolute. The CLI resolves relative paths in the caller's working directory.

```json
{"version":1,"ok":true,"result":{"pane":1,"bytes_written":6}}
{"version":1,"ok":false,"error":"unknown pane 99"}
```

`list.result` contains `pid` and a `sessions` array. Use each entry’s `id` as the argument to `--pane`. Its `tab_id` identifies the containing tab. Both IDs are local to the running process. The `active` flag identifies the selected terminal pane, and `visible` reports whether the layout displays it. Selecting a utility tab leaves every terminal inactive. A script can address any pane independently of visibility or native keyboard focus.

Each entry’s `label` is its launch label and `title` is the application-reported terminal title when a capture is available. Explicit user names appear in separate optional fields. `pane_name` belongs to that individual pane and `tab_title` belongs to its group. Resetting either name removes its field from subsequent replies.

Each entry also has a `label` and `status`. Available sessions report `title`, `cols`, `rows`, `connection_generation`, and `prompt`. An unavailable session retains its IDs and flags and reports an `error`. Prompts expose public challenge metadata only. The connection generation increases after each new SSH login. `get-text.result` adds `generation`, `lines`, `text`, and `alternate_screen`. Screenshot replies contain `pane`, `generation`, absolute `path`, `width`, and `height` in pixels.

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

`mouse` contains the booleans `click`, `drag` and `motion`, and an `encoding` of `x10`, `utf8`, `sgr` or `sgr-pixels`. `keyboard.encoding` is `xterm`, `csi-u`, `win32` or `kitty`. `keyboard.flags` is the numeric kitty keyboard protocol bitmask, or zero for other encodings.

## Completed shell commands

`list-commands --pane ID` reads the pane’s OSC 133 command history. It returns
`pane`, `connection_generation`, and a `commands` array, newest first. Each entry
contains `id`, `command`, `exit_code`, and `output_available`. The array is empty
until the shell marks a completed command. A missing command line or exit status
is JSON `null`. Listing transfers only this metadata and preserves keyboard focus
and the scroll position.

Use `--status failed` for nonzero exit codes, `--status succeeded` for zero, or
`--status unknown` when the shell omitted the exit code. The default is `all`.
Filtered listings keep the original IDs, generation, and newest-first order.

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

Direct pipe clients use these JSON requests.

```json
{"verb":"list-commands","pane":2}
{"verb":"list-commands","pane":2,"status":"failed"}
{"verb":"get-command","pane":2,"id":5,"connection_generation":1}
```

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

## Waiting, geometry, tabs, and events

`find-text` searches each selected pane’s retained scrollback and visible screen once. Use `--pane ID` for one pane or `--all-panes` for every pane in the process, across all its windows. Matching uses a case-insensitive Unicode literal, joins soft wraps, and stops at hard line breaks. Text can be up to 4,096 UTF-8 bytes. Use `--` before a pattern beginning with two hyphens. An alternate-screen pane searches its current screen.

The JSON `panes` array is sorted by pane ID. Each row includes `pane`, `tab_id`, `window_id`, `count`, `truncated`, and `first`. A match includes a plain-text `excerpt`, a stable buffer `row` and a zero-based cell `column`. An empty result has a count of zero and `first: null`. A count of 10,000 with `truncated: true` means there are additional matches. The coordinates describe the buffer at search time and can go stale after output, resize or reconnection.

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
between snapshots.

`resize` accepts the launch grid ranges. In a visible split it adjusts available dividers to reach the requested cell grid. Unzoom the focused pane before resizing it. A dimension fixed by the tab bounds, or a grid that leaves too little space for a sibling, produces a geometry error. The command keeps keyboard focus on its current pane.

For a single pane it resizes an active pane's window or a
background pane's PTY without selecting the pane or activating the window. Changes
to the active grid require leaving fullscreen first. A grid that cannot fit the
current display/window constraints fails clearly; geometry may
already have changed. The next user tab activation uses the shared window's grid.
Success acknowledges the client model/resize request, not a remote TUI redraw.

`open` creates an independent tab in the existing process from a saved profile.
Without `--activate`, the current tab stays selected. `--activate` selects the new
tab inside the app; it does not request OS keyboard focus. Omitted dimensions use
the current window grid. Replies return `pane`, `cols`, `rows`, and `active`. New
tabs may still be connecting or awaiting a native trust/authentication prompt;
scripts can inspect `list`. No command starts a new GUI process.

`events` returns `{"pane":1,"journal":{"events":[...],"next":42,"oldest":1,"lost":false},"restart_token":null}`.
Each event has `sequence`, Unix `timestamp_ms`, `kind`, and JSON `data`. Use the
returned `next` for the next request. `--timeout` long-polls until an event arrives
(default 0, range 0–3600). Cursors are per pane and process; a future cursor fails.
The journal retains 4096 entries and truncates data larger than 4096 serialized
bytes. `lost=true` reports eviction. Events include `bell`, `title`, `notification`,
`sound`, connection/reconnection, assets, and recording. A bell event does not imply
audible playback; inspect the `sound` outcome. See [the game protocol](docs/game-client-protocol.md).
The CLI allows the requested wait timeout plus 15 seconds for these replies.

A command completion watch armed in the GUI adds one `command-finished` event to
the originating pane. Its data is `{"exit_code":17}` for a reported status or
`{"exit_code":null}` when the shell supplied none. Command lines and output stay
out of this event.

When a direct SSH shell or local program finishes, a restartable pane emits
`pane-exited` with `{"exit_token":1}`. The top-level `restart_token` in the
events result identifies its current exit, even if the event has aged out.
Use that token with the same process and pane ID to restart the program in
place. Each exit can be claimed once. Live and reconnecting panes have a
null token. The restart reply acknowledges the request. Use `list` to observe
the new connection generation or `wait-for-text` for an application prompt.

```powershell
$state = (& $ctl --pid 12345 events --pane $pane | ConvertFrom-Json).result
if ($null -ne $state.restart_token) {
    & $ctl --pid 12345 restart --pane $pane --exit-token $state.restart_token
}
```

```json
{"verb":"wait-for-text","pane":1,"pattern":"fps=\\d+","regex":true,"timeout_seconds":15}
{"verb":"resize","pane":1,"cols":170,"rows":48}
{"verb":"open","profile":"play-6792","cols":170,"rows":48,"activate":false}
{"verb":"events","pane":1,"since":42,"timeout_seconds":30}
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

```powershell
& $ctl --pid 12345 sftp --pane $pane info
& $ctl --pid 12345 sftp --pane $pane ls /var/log
& $ctl --pid 12345 sftp --pane $pane get /tmp/output.bin .\output.bin
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
| `get-dir REMOTE NEW_LOCAL_DIRECTORY` | Recursively download a directory into the exact local path. Its parent must exist. Success returns `files`, `directories` (including the root), `skipped`, and `bytes_transferred`. |
| `put-dir LOCAL NEW_REMOTE_DIRECTORY` | Recursively upload to the exact remote path. Success returns `files`, `directories` (including the root), `skipped`, and `bytes_transferred`. |
| `ls [PATH]` | Collect directory entries (default `.`); maximum 8 MiB JSON, then use paged reads |
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
