# w11term

A Windows-native SSH terminal prototype in Rust. Windows Reactor supplies the window and hosts a GPU terminal surface. The shell runs on the SSH server; there is no ConPTY, local PowerShell, or cmd dependency.

Terminal emulation is provided by **[wezterm-term](https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/term)**, the terminal core from [WezTerm](https://wezterm.org/), by Wez Furlong and contributors. Its VT parser, screen model, scrollback/reflow, graphics protocols, and semantic prompt support are the foundation of w11term's terminal. The vendored crate retains its [original MIT license](vendor/wezterm-term/LICENSE.md); our local changes are documented in [PATCHES.md](vendor/wezterm-term/PATCHES.md).

## Run

Requires Windows and the Rust MSVC toolchain with Visual Studio C++ Build Tools. The build stages the WinUI runtime beside the executable; there is no separate runtime installation step. The first build downloads Rust dependencies, a pinned WezTerm source revision, and Microsoft's runtime packages.

```powershell
cargo run
cargo run -- --profile root
cargo run -- --profile play-6789
cargo run -- --profile play-6792
cargo run -- --profile root --tab play-6789 --tab play-6792
cargo run -- --list-profiles
cargo run -- ssh user@example.com --identity C:\path\to\id_ed25519
cargo run -- ssh user@example.com --port 2222 --host-key SHA256:verified-fingerprint
cargo run -- ssh play@conclav.io --port 6789 --auth none
```

Without arguments, the app opens a **local terminal lab**, not a shell. Its ANSI colors, Unicode text, and kitty image are actual VT traffic processed by the same engine used for SSH. Typing in the lab only exercises local echo.

`profiles.json` contains the three initial targets. `root` uses the local `hostinger` key; both `play` profiles use SSH `none` authentication, without contacting the agent or offering a private key. The key is excluded by `.gitignore`. Profiles load from the current working directory, then beside the executable or up to two parent directories (for development builds), or from an explicit `--profiles C:\path\to\profiles.json`. Relative identity paths resolve beside that configuration file. CLI SSH options can override a profile.

## Tabs and profiles

The native WinUI tab strip sits in the window title bar. Click **+** to choose a saved profile and open an independent SSH session; opening a profile more than once creates separate sessions. Drag tabs to reorder them and use their close buttons to disconnect. Closing the last tab closes the window. Background sessions retain their terminal state and continue receiving output; only the active terminal is rendered.

The **+ → New Profile…** form supports host, user, port, agent/key-file/none/password/keyboard-interactive authentication, an optional verified host fingerprint, device-key generation, and player preferences. **Save and connect** adds the profile to the selected configuration file and opens it. Existing profiles cannot be overwritten through this form. Saving preserves unrelated configuration fields and atomically replaces the file. Only the key file's path is stored; private key contents never go into the configuration.

The **caret → Settings** command opens the terminal font selector and programming-ligatures toggle. The selector includes only installed Cascadia Code, Cascadia Mono, Consolas, JetBrains Mono, Monaspace-family, and Fira Code fonts. Changes apply across the window's terminal tabs and are saved automatically to `%LOCALAPPDATA%\w11term\settings.json`. `W11TERM_SETTINGS_PATH` can override that file for portable configuration or isolated tests. Settings and New Profile tabs use native controls and do not send their typing into an SSH session. The plus menu also includes the terminal lab.

Ligatures default to enabled and use DirectWrite shaping for adjacent ASCII cells with matching attributes, preserving the terminal grid. Monaspace additionally enables its [documented programming-ligature feature sets](https://github.com/githubnext/monaspace#code-ligatures). Font changes recalculate cell geometry and resize existing sessions; PNG capture uses the same selected font and ligature setting.

SSH supports OpenSSH private keys supplied with `--identity`, protected w11term device keys, the Windows OpenSSH agent (the default for `ssh user@host`), `--auth none`, `--auth password`, and `--auth keyboard-interactive`. Password and MFA challenges use native dialogs; hidden answers never enter terminal input, profiles, event logs, or recordings. A successful password remains in memory only for reconnection; one-time codes are requested again. Passphrase-protected OpenSSH keys can be used through the agent.

The first connection to an unknown host shows its algorithm and SHA256 fingerprint. **Accept and save** writes the public key to OpenSSH `known_hosts`; Cancel stops the connection. A changed known key fails without offering to replace it. An explicitly verified `--host-key SHA256:...` pin remains an alternative. Headless probes fail on unknown hosts instead of opening a dialog. SSH config aliases and jump hosts remain deferred.

## Player profiles

Dropped transports reconnect in the same tab, with 1/2/4/8/10-second backoff and a 300-second retry window. Permanent authentication/trust errors and normal remote process/channel closure stop retrying. Input while disconnected is discarded; raw control writes and SFTP operations fail explicitly instead of being replayed. The latest grid is applied to the new PTY, terminal protocol state resets, and primary scrollback remains. The client returns to the server's prompt; it never sends `BOARD` or replays game commands automatically. Every SSH shell requests `COLORTERM=truecolor`.

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
  "sync_assets": true,
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
| Ctrl+Shift+F | Find in scrollback; case toggle, highlighted matches, and a match counter |
| Enter in find / F3; Shift+Enter / Shift+F3 | Next / previous find result, wrapping at either end |
| Escape in find | Close find and restore terminal keyboard input |
| Mouse wheel | Scrollback using Windows' wheel-line/page preference, including accumulated fractional deltas; remote wheel events when mouse reporting is enabled |
| F11 | Toggle borderless fullscreen |
| F5 in the local lab | Reload the protocol demo |
| Ctrl+L in the local lab | Clear the display |

Hold Shift to select or right-click paste when a remote TUI enables mouse reporting. Selection retains scrolled rows, joins soft-wrapped lines, and copies graphemes without font/color formatting. Native pointer capture lets dragging continue outside the canvas, with automatic scroll at its top/bottom edges.

Find is local to each tab and searches literal text across retained history and the current screen, joining soft-wrapped lines and preserving Unicode cell coordinates. It defaults to case-insensitive Unicode lowercase matching (no regex or canonical-normalization conversion). The query is capped at 4096 UTF-8 bytes, with at most 10,000 results (`+` indicates additional matches). Search text stays in the native find box until Escape closes it. Counts refresh after a short debounce while output is arriving; ordinary scrolling stays anchored to retained rows as new output arrives.

The alternate screen has no local scrollback. A TUI receives wheel reports when it enables mouse reporting; otherwise w11term uses the engine's arrow-key fallback so programs such as pagers can navigate their own content. Shift+wheel overrides mouse reporting for local primary-screen history; on the alternate screen that local action is a no-op. Find on the alternate screen searches only its current contents, without mixing in shell history. Returning to the primary screen restores its history position.

**OSC 133 semantic prompts** are inherited from the vendored WezTerm engine: A/B/C mark prompt/input/output regions, including regions retained in scrollback. Ctrl+Shift+Up/Down now navigates prompt starts relative to the top of the viewport. The remote shell must emit these markers through its own shell integration; w11term does not modify remote shell startup files. The engine currently consumes OSC 133 D without retaining its command exit status, so exit-code badges/command-status tracking are not implemented.

The Settings tab exposes auto-copy, block-selection trimming, paste trimming, grid snapping, tab-switcher mode (tab order/MRU/disabled), Ctrl+scroll zoom, URL detection, and remote OSC 52 writes. Auto-copy is enabled as requested; the other listed boolean options default to enabled, and the switcher defaults to tab order. Copies always contain plain text. Normal paste trims whitespace only at the end of the entire payload by default; raw paste preserves it. The resize border snaps to cell boundaries during manual dragging. These behaviors follow the relevant [Windows Terminal interaction settings](https://learn.microsoft.com/en-us/windows/terminal/customize-settings/interaction), with the explicit auto-copy override.

Remote programs can set the local Windows clipboard with **OSC 52**, without a forwarded control pipe. For example, on the SSH host:

```sh
printf '\033]52;c;%s\a' "$(printf '%s' 'Copied from the remote host 🌍' | base64 | tr -d '\n')"
```

OSC 52 writes are plain text, work for background sessions, and are capped at 1 MiB. Clipboard-read queries receive no response. Remote clipboard writes can be disabled in Settings. Normal UI paste remains distinct from the control CLI's `send-text --no-paste`, which always sends raw bytes. For bytes that cannot be represented as Windows clipboard text, use `--file`, `--stdin`, or `--base64` through the CLI.

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
- The engine has kitty/Sixel/iTerm image handling. The renderer draws static content and the first animation frame; animation scheduling and advanced protocol extensions remain work. The replacement/delete/resize audit is documented in [Graphics audit](docs/graphics-audit.md). It covers the cockpit's single-id replacement path and GPU eviction, with bounded direct transfers and image tables; it is not a claim of complete image-protocol conformance or a global memory bound across all retained scrollback images.
- Text uses cached DirectWrite layouts, including color emoji and programming ligatures across matching ASCII cells. Full Unicode run shaping, full BiDi layout, italic rendering, underline variants, and exact cursor styling remain work.
- Keyboard press/repeat/release and game-mode repeat suppression are connected. Complete physical-key/alternate-key handling and IME/TSF composition remain work.
- Terminal accessibility, split panes, tab tear-out into separate windows, profile editing/deletion, further Settings, and persistent remote sessions are deferred.

## Verify

For visible typing-latency comparisons against WezTerm and Windows Terminal, see [the loopback SSH / Typometer setup](bench/README.md).

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The integration tests start ephemeral **loopback-only** SSH servers with generated test keys. They verify both public-key and keyless authentication, PTY/shell setup, terminal query replies, keyboard roundtrip, and resizing without accessing a user server or credential. Unit tests cover profiles, fragmented UTF-8/VT traffic, alternate-screen restoration, reflow, bracketed paste, and kitty image/key state.

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

The terminal core is vendored from commit `e2a98e1b8af156988ad0ac778658ac27b60a55fb` with embedding fixes and extensions; see `vendor/wezterm-term/PATCHES.md`. Its shared crates remain pinned to that revision. `Cargo.lock` pins the resolved dependency graph. Treat upgrades as compatibility changes and rerun protocol and visual checks.

## License

w11term's original project code is licensed under [Apache-2.0](LICENSE). Vendored `wezterm-term` remains MIT-licensed, with its upstream copyright and license notices preserved. Other dependencies retain their respective licenses. See [NOTICE](NOTICE) for attribution.
