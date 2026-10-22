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

Omit `--pid` when exactly one instance is running. Zero instances fail immediately; multiple instances require an explicit PID. Pane IDs remain stable through tab reordering and are never reused during that process. Settings and New Profile pages are excluded.

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

& $ctl --pid 12345 screenshot --pane $pane .\frame.png
```

`send-text` requires `--no-paste`. It sends the supplied bytes directly to the SSH channel, with no bracketed-paste delimiters, key encoding, UTF-8 normalization, or escape expansion. `--base64`, `--file`, and `--stdin` accept arbitrary bytes including NUL and invalid UTF-8; the limit is 1 MiB per command. A successful reply acknowledges the SSH channel write, **not remote application processing**. Use `wait-for-text` to wait for output. A write timeout means delivery is unknown; retrying could duplicate input. Demo/replay panes reject raw input. Reconnecting panes reject raw writes and SFTP work rather than queuing them for a later connection.

`get-text` returns the current live screen, including alternate-screen TUI content. It excludes scrollback, trims trailing ASCII spaces on each row, and preserves the row count as `lines`. `text` joins those lines with LF. Graphics are not OCRed. Both text and screenshots include a snapshot generation to help correlate results. Reading does not change the user's scrollback position.

`screenshot` renders an immutable snapshot with the same DirectWrite/Direct2D code used by the window, including the selected font, ligature preference, and kitty/sixel images. It exports only the terminal grid, at the pane's last negotiated DPI; title bar, padding, and status bar are excluded. It works for background tabs and covered/minimized windows without selecting a tab or touching focus. Existing destination files are replaced atomically after PNG encoding succeeds. The parent directory must already exist. Captures above 16 megapixels are rejected. Current renderer limitations (including first-frame-only animated images) also apply to PNGs.

## Pipe protocol, version 1

Each GUI owns `\\.\pipe\w11term-<pid>` and publishes `%LOCALAPPDATA%\w11term\instances\<pid>.json`:

```json
{"version":1,"pid":12345,"pipe":"\\\\.\\pipe\\w11term-12345"}
```

The pipe accepts only the process's Windows user and SYSTEM, and rejects remote clients. Treat access as permission to type into that user's SSH sessions and save screenshots with that user's filesystem permissions. Discovery records are removed on normal exit; stale records from a crash are ignored when the pipe cannot be opened.

Open a new pipe connection for each command. Write one UTF-8 JSON object followed by LF; read one JSON reply followed by LF. Maximum request size is 2 MiB; at most eight requests run concurrently. The initial read and reply write time out after ten seconds. Input/snapshot requests have a ten-second execution timeout; the CLI allows thirty seconds for a reply to accommodate rendering.

```json
{"verb":"list"}
{"verb":"send-text","pane":1,"no_paste":true,"data_base64":"Qk9BUkQN"}
{"verb":"get-text","pane":1}
{"verb":"screenshot","pane":1,"path":"C:\\tests\\frame.png"}
```

Direct protocol screenshot paths must be absolute. The CLI resolves relative paths in the caller's working directory.

```json
{"version":1,"ok":true,"result":{"pane":1,"bytes_written":6}}
{"version":1,"ok":false,"error":"unknown pane 99"}
```

`list.result` contains `pid` and `sessions`; each session has `id`, `label`, `active`, `title`, `status`, `cols`, `rows`, `connection_generation`, and `prompt`. A prompt contains public challenge metadata only; passwords and challenge answers never appear. The connection generation increases after each new SSH login. `get-text.result` adds `generation`, `lines`, `text`, and `alternate_screen`. Screenshot replies contain `pane`, `generation`, absolute `path`, `width`, and `height` in pixels. Commands operate on the addressed pane independently of native keyboard focus.

## Waiting, geometry, tabs, and events

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

`resize` accepts the launch grid ranges. It resizes an active pane's window or a
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

`events` returns `{"pane":1,"journal":{"events":[...],"next":42,"oldest":1,"lost":false}}`.
Each event has `sequence`, Unix `timestamp_ms`, `kind`, and JSON `data`. Use the
returned `next` for the next request. `--timeout` long-polls until an event arrives
(default 0, range 0–3600). Cursors are per pane and process; a future cursor fails.
The journal retains 4096 entries and truncates data larger than 4096 serialized
bytes. `lost=true` reports eviction. Events include `bell`, `title`, `notification`,
`sound`, connection/reconnection, assets, and recording. A bell event does not imply
audible playback; inspect the `sound` outcome. See [the game protocol](docs/game-client-protocol.md).
The CLI allows the requested wait timeout plus 15 seconds for these replies.

```json
{"verb":"wait-for-text","pane":1,"pattern":"fps=\\d+","regex":true,"timeout_seconds":15}
{"verb":"resize","pane":1,"cols":170,"rows":48}
{"verb":"open","profile":"play-6792","cols":170,"rows":48,"activate":false}
{"verb":"events","pane":1,"since":42,"timeout_seconds":30}
```

## VT recording

```powershell
& $ctl --pid 12345 record --pane $pane .\bug.vt
# Reproduce the problem, or request a repaint from the remote application.
& $ctl --pid 12345 record --pane $pane --stop
.\target\release\w11term.exe --replay .\bug.vt
```

Recordings contain exact incoming terminal-output bytes, including graphics and
escape sequences, from the moment recording begins. Input and SSH authentication
answers are excluded, but a server may echo typed text or display sensitive output;
review captures before sharing. Starting mid-session does not capture previous
terminal state or an escape sequence's already-received prefix. Trigger a remote
full repaint when capturing a TUI bug. Timing, reconnection resets, and out-of-band
SSH resize events are not stored; this is an output stream, not a timed video.

Start creates a new file and a `<stem>.vt.json` sidecar with initial columns/rows;
existing files are never overwritten. Replay reads that geometry unless explicit
`--cols`/`--rows` override it. The parent must exist. Stop flushes and syncs the file,
and returns its path and byte count. Closing the pane flushes it too. A disk error
or 256 MiB limit stops recording and emits `recording-error`. Abrupt process death
can lose buffered tail bytes. Keep the sidecar with the `.vt` file.

```json
{"verb":"record","pane":1,"path":"C:\\tests\\bug.vt"}
{"verb":"record","pane":1,"path":null}
```

## SFTP

SFTP is available through a pane's **existing authenticated SSH connection**, on a separate, lazily opened subsystem channel. It never starts a local shell, opens a second SSH login, or changes window/tab focus. A server must offer the `sftp` subsystem; a rejected subsystem leaves the terminal usable. Remote paths are evaluated by the server, independently of the shell's current directory. Use `realpath .` to discover the SFTP working directory, and prefer absolute remote paths in scripts.

```powershell
& $ctl --pid 12345 sftp --pane $pane info
& $ctl --pid 12345 sftp --pane $pane ls /var/log
& $ctl --pid 12345 sftp --pane $pane get /tmp/output.bin .\output.bin
& $ctl --pid 12345 sftp --pane $pane put .\input.bin /tmp/input.bin
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

The command surface covers SFTP v3:

| Commands | Arguments / result |
| --- | --- |
| `info` | Negotiated version, advertised extensions, open-handle count, read/write chunk limits |
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
