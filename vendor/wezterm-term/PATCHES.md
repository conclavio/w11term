# Local modifications

- The Contour color-scheme query reports the embedding's configured light or
  dark preference. Private mode 2031 subscribes the connection to user palette changes.

- Framed terminal viewers can install separate nonblocking writers for user input and parser replies. The default writer remains threaded. The embedding can also detach its clipboard handler when a stream may repaint earlier output.

Source: https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/term

- The manifest expands workspace dependencies so this crate can be built outside the WezTerm workspace. Shared crates remain pinned to the same upstream revision.
- The embedded `termwiz/data/wezterm` terminfo blob is copied to `data/wezterm`, and its include path is adjusted for the standalone crate.
- `src/screen.rs`: in `rewrap_lines`, only apply the wrap-boundary cursor adjustment when the logical cursor offset is greater than zero. A cursor at column zero on a hard-newline row must stay on that row when width changes. Previously widening an 80-column terminal containing `remote ready\r\n` to 100 columns moved its cursor from `(0, 1)` to `(99, 0)`. The next character appeared at the end of the previous row.

Regression coverage is in the parent project's `terminal.rs` unit tests and the loopback SSH session test. Keep this change isolated when updating upstream.

- `src/terminal.rs` / `terminalstate/performer.rs`: forward only private OSC 440 through an application alert. The host validates the version, verb, names, and queue policy. No general unknown-OSC execution hook is added.
- `src/terminal.rs` / `terminalstate/mod.rs`: reset parser and negotiated modes on a new SSH transport while retaining the primary screen/scrollback and discarding old kitty transmission state.
- `src/terminalstate/kitty.rs` bounds direct transmission, inflation and image-table resources and rejects local file and shared-memory transports. Each anonymous image and each placement without a client placement ID has its own lifetime. Named placements retain their client IDs for moves and targeted deletion. [Graphics audit](../../docs/graphics-audit.md)

The original MIT license is preserved in `LICENSE.md`.

- DECRQM for DEC private mode 5 reports the current screen polarity so applications
  can restore the previous setting after using reverse-screen mode.

- OSC 104 restores the requested indexed colors, or the full indexed table
  when no indices are given, while preserving dynamic foreground, background
  and cursor colors.
  A change of configured palette updates inherited colors and preserves each
  explicit OSC color assignment until the application resets it. Color queries
  read the effective palette without marking rows dirty.

- `src/terminal.rs`, `terminalstate/mod.rs`, `terminalstate/performer.rs`: implement
  synchronized-output mode 2026 and DECRQM state in the engine. The embedding can
  observe parsed begin/end boundaries, including multiple transitions within one
  read. Row sequence numbers advance across those boundaries so committed snapshots
  cannot reuse a row from an earlier update in the same read. Reset clears the mode;
  the embedding owns the bounded watchdog and committed presentation.

- CPU dedup cache has a 4 MiB byte budget and is cleared on image deletion,
  connection reset and animation mutation. PNG decoding is explicitly PNG with
  allocation limits. Animations have 256-frame/32 MiB limits, grow the shared table
  accounting, and direct chunk metadata is bounded. Direct animation frames use
  the same chunk accounting as image transmissions. Chunked queries are rejected.

- Reconnection moves the previous primary viewport into scrollback before resetting the transport modes and exposing a clean live screen.

- Automatic image-id exhaustion rejects without overflow; number aliases are only
  published after successful storage and are retired with deleted/pruned images.
  Mutable kitty image IDs have independent frame data and playback state.

- The kitty model owns each image ID independently, schedules animation frames
  and projects Unicode placeholder and relative placements onto snapshot rows.
  The sister vendored escape parser at the same pinned revision preserves their
  control parameters. The projection uses the Unicode 6.0 diacritic indices
  published with kitty's graphics protocol. See `docs/graphics-audit.md`.
  Timed replay supplies a per-terminal animation clock. Large jumps use the
  current frame gaps and loop limit to find the displayed frame in time
  proportional to the frame count. Live panes retain their wall-clock pacing.

- Kitty visible deletion uses the live screen boundary for physical placements.
  The escape parser and image model support inclusive client ID ranges, including
  the full range used by `kitten icat --clear-all`. The primary and alternate
  screens have independent graphics state within the terminal's resource limits.

- `screen.rs` and `terminalstate/` retain a primary-screen OSC 133 command's
  output boundaries across scrolling and reflow. On completion they expose
  bounded plain text and the numeric exit status. The sibling escape parser
  preserves the distinction between a missing status and exit code zero.
  OSC 7 retains the original URI alongside the parsed URL so the embedding can
  validate the authority before using it for the pane's SFTP drawer.

- The embedding receives primary-screen OSC 133 command markers and terminal
  reset notifications as the parser processes them. This lets the command
  completion watch observe a short command even when several complete in one
  input batch or its output has already left scrollback.

- OSC 9;4 retains the percentage of a paused operation. Error and paused
  states with an omitted or zero value reuse the previous percentage, including
  across indeterminate updates. A fresh warning starts at 10%, matching Windows
  Terminal. RIS and transport reset clear both the state and the stored value.

- `Screen::physical_lines` provides a borrowed, double-ended iterator over
  retained rows for plain-text exports without cloning scrollback.

- `Screen::history_clear_count` distinguishes explicit erasure of scrollback
  from eviction as new output arrives. This lets the UI discard an old
  scrollbar gesture after either a local clear or a remote CSI 3 J.

- XTPUSHSGR and XTPOPSGR use a bounded text-style stack with selective
  restoration. A new transport starts with an empty stack.

- XTSAVE and XTRESTORE preserve supported DEC private modes, including the
  alternate-buffer selector and the mouse reporting and encoding modes. Saved
  values are scoped to the connection and cleared by full reset.

- DEC line renditions limit addressable columns, discard truncated text, and
  retain their width and height flags when rows scroll or the grid resizes.
  Enabling left and right margins resets live rows to single width and height.

- Screen storage grows as output arrives. Resizing and rewrapping enforce
  the physical row limit supplied by the profile, including a limit of zero.
  Dropping primary rows advances their stable row indices so Find and export
  continue to address retained text. The alternate buffer keeps one viewport.

- A terminal may replace its input and reply writer after the previous
  transport has exited. Pending bytes on the old writer keep their original
  destination while the next shell starts in the same terminal model.
  The embedding can supply a nonblocking writer to queue PTY resize requests
  and automatic replies in order, and can track window focus locally while
  the peer is offline.

- The embedding can enable ConEmu / Windows Terminal OSC 9;9 reports for
  local Windows panes. Reported drive paths feed the same current-directory
  state as OSC 7.

- A direct SSH shell can enable iTerm2 OSC 1337 `RemoteHost` and `CurrentDir`.
  A matching user and destination authorizes literal POSIX paths until the
  peer reports another identity, sends OSC 7, or the terminal resets. A
  foreign or malformed identity clears the previous directory. Other pane
  types ignore both reports.

- DEC private mode 2048 reports the current text area in cells and pixels on
  each enable and after a size change. DECRQM reports the subscription state.
  RIS and transport reset clear it. Both immediate and asynchronous reports
  follow the parser reply route on framed viewers.

- Xterm title operations retain up to ten sets of window and icon labels per
  terminal. An application can push or pop selected labels, access a slot
  by index, and query stack depth. Restoring a changed window title emits
  the same window title alert as OSC 2. A full reset clears saved labels.
  The model tracks whether an application supplied the window title, including
  across title saves and restores. A full reset or reconnect clears that flag.
  Consumers can read the application window title independently of the icon.
