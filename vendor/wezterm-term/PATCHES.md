# Local modifications

Source: https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/term

- The manifest expands workspace dependencies so this crate can be built outside the WezTerm workspace. Shared crates remain pinned to the same upstream revision.
- The embedded `termwiz/data/wezterm` terminfo blob is copied to `data/wezterm`, and its include path is adjusted for the standalone crate.
- `src/screen.rs`: in `rewrap_lines`, only apply the wrap-boundary cursor adjustment when the logical cursor offset is greater than zero. A cursor at column zero on a hard-newline row must stay on that row when width changes. Previously widening an 80-column terminal containing `remote ready\r\n` to 100 columns moved its cursor from `(0, 1)` to `(99, 0)`. The next character appeared at the end of the previous row.

Regression coverage is in the parent project's `terminal.rs` unit tests and the loopback SSH session test. Keep this change isolated when updating upstream.

The original MIT license is preserved in `LICENSE.md`.
