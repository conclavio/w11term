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
& $ctl --pid 12345 find-text --all-panes --regex --match-case 'ERROR\s+[0-9]+'

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

Open a new pipe connection for each command. Write one UTF-8 JSON object followed by LF. Replies are UTF-8 JSON objects followed by LF. `follow-ssh-group-output` sends a sequence of replies on the same connection. Maximum request size is 2 MiB. The server allows eight ordinary requests and sixteen waits or followers concurrently. The initial read and each reply write time out after ten seconds. Input and snapshot requests have a ten-second execution timeout, and the CLI allows thirty seconds for a reply to accommodate rendering.

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

`mouse` contains the booleans `click`, `drag` and `motion`, and an `encoding` of `x10`, `utf8`, `sgr` or `sgr-pixels`. `keyboard.encoding` is `xterm`, `csi-u`, `win32` or `kitty`. `keyboard.flags` is the numeric kitty keyboard protocol bitmask, or zero for other encodings.

When xterm modified-key reporting or its alternate format is enabled, `keyboard.modify_other_keys` is the active level from 0 through 3 and `keyboard.format_other_keys` is `xterm-27` or `csi-u`.

## Completed shell commands

`list-commands --pane ID` reads the pane’s OSC 133 command history. It returns
`pane`, `connection_generation`, and a `commands` array, newest first. Each entry
contains `id`, `command`, `exit_code`, and `output_available`. The array is empty
until the shell marks a completed command. A missing command line or exit status
is JSON `null`. Listing transfers only this metadata and preserves keyboard focus
and the scroll position.

The command comes from the shell’s OSC 133 C `cmdline_url` field when
available, including whitespace and line breaks. Otherwise it comes from
visible text between the B and C markers.

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

For SSH panes the reply also includes `ssh_connection`, an object with a
`phase` of `queued`, `connecting`, `connected`, `reconnecting`, `disconnected`,
`stopped`, or `failed`. Reconnecting includes an `attempt` number for an
automatic retry and `null` for an explicit restart. A queued pane is waiting for
a slot in a group launch. Failed includes a short
`reason`. The journal adds an `ssh-connection` event on each phase change
with the same data shape. The connection snapshot and restart token describe
the pane at reply time, including when recorded transitions have aged out.

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
| `put LOCAL REMOTE --atomic [--overwrite]` | Stage a single file next to its destination before publishing it. Replacing a regular file requires both `--overwrite` and `posix-rename@openssh.com`. Success returns `atomic: true`. Direct pipe clients set `atomic: true` on the `put` operation. |
| `get-dir REMOTE NEW_LOCAL_DIRECTORY` | Recursively download a directory into the exact local path. Its parent must exist. Success returns `files`, `directories` (including the root), `skipped`, and `bytes_transferred`. |
| `put-dir LOCAL NEW_REMOTE_DIRECTORY` | Recursively upload to the exact remote path. Success returns `files`, `directories` (including the root), `skipped`, and `bytes_transferred`. |
| `ls [PATH]` | Collect directory entries (default `.`); maximum 8 MiB JSON, then use paged reads |
| `find ROOT QUERY` | Search filenames recursively using a literal, case-insensitive fragment up to 256 bytes. Returns the canonical `root`, `matches` with `parent` and `name`, scan counts, and `truncated` and `cancelled` flags. A search examines up to 128 folders, 16,384 entries, 200 matches and 24 nested levels, with a 4096-byte path limit. Direct pipe clients send `{"op":"find","path":"ROOT","query":"QUERY"}`. |
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
