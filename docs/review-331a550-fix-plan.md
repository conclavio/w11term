# Review verification and fix plan

Verified against `331a5504c59a9c9d4c96bcd0a986ee9f104cf6ab` on Windows,
2026-10-22. Scope: the five priorities in `w11term-review-331a550.md`.
All five fixes are implemented in the order below, with regression coverage.
The verification table records the original behavior at the reviewed commit.

## Evidence

The isolated harness is `.local/review-probe`, with output in
`.local/review-probe/results.txt`. It imports the existing library and renderer,
uses offscreen Direct2D, and never connects to a remote host or plays audio.
These files are ignored by Git. The original verification command at that baseline
(before the implementation API changes) was:

```powershell
cargo run --offline --manifest-path .local/review-probe/Cargo.toml --target-dir target --quiet
```

| Finding | Verification | Qualification |
| --- | --- | --- |
| Synchronized output and redundant publication | The vendor's set/reset handlers explicitly delegate to WezTerm's absent mux. A query after `CSI ? 2026 h` returns `CSI ? 2026 ; 2 $ y` (reset). An isolated session publishes `NEW` before receiving sync-end. `Scroll(0)` changes the generation from 2 to 3 with identical text. | The loop waits for an event; this is not an idle busy loop. The number of redundant repaints per cockpit frame has not been measured. |
| Oversized bitmap disables tab rendering | The device reports a 16,384-pixel maximum dimension. A raw 20,000 by 1 RGBA image reaches the model, then both direct upload and the real renderer return `E_INVALIDARG`. The UI permanently selects its error view after a render error. | The same offscreen renderer succeeds after deleting the image. The permanent failure is the UI's error latch, not a permanently broken GPU device or SSH connection. The trigger is about 107 KB of base64 plus protocol framing; device limits vary. |
| WAV validation disagrees with playback's input contract | The real validator accepts data before format, two conflicting format chunks, and a RIFF size claiming almost 4 GiB for a small buffer. A file containing 1,920,000 audio bytes passes using its last format's five-second limit, while its first format describes 240 seconds. | No malformed data was passed to WinMM. Actual Windows overread, first-format playback behavior, and exploitability have not been demonstrated. The validation defect is independently confirmed. |
| Pipe lacks an explicit integrity boundary | The code creates only a protected user/SYSTEM DACL. An otherwise equivalent native pipe created at medium integrity has no mandatory label in its actual descriptor. No client-integrity authorization is present. | A high-integrity server / medium-integrity client pair has not been exercised. Treat the elevated-process confused-deputy scenario as a credible conditional risk, not a demonstrated UAC bypass. |
| Delete and alternate-screen wheel | With kitty flags 1 or 3, Delete press produces `0x08`; with flag 3, repeat/release identify Delete using `CSI 3 ; 1 : event ~`. With flags 8 or 11, press correctly identifies Delete. Alternate-screen wheel emits `CSI A` with both legacy and kitty keyboard modes when mouse reporting is off. | Delete is negotiation-dependent. Alternate scrolling is intentional inherited compatibility behavior, but unsuitable as an unconditional game-client policy. The existing wheel unit test asserts this behavior and passes. |

Relevant implementation locations:

- Synchronized mode: `vendor/wezterm-term/src/terminalstate/mod.rs:1709`;
  publication and direct capture: `src/session.rs:251` and `:322`;
  invalidation: `src/app.rs:2143`.
- GPU upload and budget: `src/renderer.rs:540`; upload API:
  `src/native_drawing.rs:159`; persistent error view: `src/app.rs:1285` and `:2439`.
- WAV parsing: `src/assets.rs:279`; playback: `src/sound.rs:125`.
- Pipe security: `src/control.rs:627`, particularly `:667`.
- Delete mapping: `src/input.rs:864`, `src/terminal.rs:138`; wheel policy:
  `src/terminal.rs:15`, `vendor/wezterm-term/src/terminalstate/mouse.rs:121`.

## Implementation sequence

Keep five separate commits so failures and protocol changes remain easy to review.
Start with the small failures reachable from remote content, then complete frame
publication and input behavior before another feature batch.

### 1. Contain image failures — implemented

- Query the render target's maximum bitmap dimension before CPU conversion or
  upload. Retain checked dimensions, buffer-length checks, and byte budgets.
- Treat unsupported or invalid image resources as a rejected image; render text
  and other images. Remember rejected content per device generation so every
  cell and repaint does not retry an impossible upload.
- Distinguish resource rejection from device loss. Device loss should invalidate
  GPU resources and follow the Canvas recovery path. Make any remaining UI error
  state recoverable, rather than permanently replacing the terminal surface.
- Fix the adjacent mid-frame cache-clearing problem in the same resource path:
  admit images once per frame, evict unused entries first, and skip excess images
  deterministically. Never clear textures needed by the current frame to admit
  another tile. Keep accounting and rejection state bounded.
- Acceptance: the 20,000 by 1 image cannot disable a tab; valid text and images
  remain usable before and after deletion, replacement, resize, and tab switch.
  Repeated rejected placements cause at most one admission/upload attempt per
  resource/device generation. Over-budget visible images do not cause repeated
  uploads within one frame. Preserve device-loss recovery and the existing
  replacement/deletion regression.

### 2. Give playback a single, bounded PCM interpretation — implemented

- Parse the complete RIFF container with checked sizes, exact bounds and padding.
  Require one supported format before one nonempty data chunk; reject duplicate
  formats/data, truncation, and contradictory sizes or format fields.
- Validate block alignment, byte rate, and whole sample frames. Compute the
  five-second limit from that one format using checked arithmetic.
- Return a validated sound object containing a canonical PCM WAV buffer. Build
  the header from validated fields and copy only the validated sample bytes;
  discard accepted ancillary metadata. The playback worker should accept this
  object rather than arbitrary file bytes. Apply the same parser to downloaded
  assets and local sound files.
- Acceptance: malformed RIFF sizes, duplicate/reordered chunks, missing padding,
  partial frames, invalid byte-rate/alignment fields, and durations just above
  five seconds fail without invoking playback. Valid mono/stereo 8/16-bit PCM at
  supported rates still works. Test the canonical output's sizes independently.

### 3. Enforce the control process's integrity boundary — implemented

- Add an explicit mandatory label derived from the server token, with no-write-up
  and no-read-up policy. Preserve the protected user/SYSTEM DACL, local-only pipe,
  first-instance behavior, and request bounds. Fail closed if protection fails.
- Authorize the connecting client's user and integrity before executing commands;
  ensure any temporary impersonation is always reverted before privileged work.
- Verify the pipe server's process identity against discovery metadata in the CLI;
  check the connected process's user as well. A writable manifest or pipe name
  alone must not establish trust. Return clear access/identity errors.
- Acceptance: same-user/same-integrity automation remains functional; lower
  integrity clients cannot command higher integrity servers; unrelated users,
  stale manifests, and substituted pipe servers fail. Include descriptor checks
  in normal tests and an opt-in native high-server/medium-client test using a
  harmless command. The latter is still required to validate the elevated case.

### 4. Implement synchronized presentation and selective publication — implemented

- Track mode 2026 through the engine parser, including correct DECRQM replies,
  reset/reconnect behavior, fragmented escapes, and transitions within one read.
  Do not detect it by scanning raw SSH bytes.
- Continue parsing, replies, input, recording, and control handling during a
  synchronized update. Hold the committed presentation until sync-end. Preserve
  completed boundaries when one read contains end/begin transitions.
- Use a bounded watchdog, initially 150 ms, so missing sync-end cannot freeze the
  display. Repeated begin markers must not extend the deadline indefinitely.
  This timeout is an implementation policy, not a protocol-mandated value.
- Make GUI display, screenshot, get-text and wait-for-text observe committed
  presentation consistently, including a committed live-screen view while the
  user scrolls. A direct `Capture` must not bypass the hold. Keep prompts and
  connection controls responsive independently of held terminal pixels.
- Ensure committed image content stays stable too: image animation/composition
  can mutate shared image data, so freezing only the row list is insufficient.
- Separate presentation dirtiness from metadata changes. Publish and invalidate
  on actual terminal/viewport/overlay changes; capture queries, no-op scrolling,
  and input that has no visible effect must not trigger another terminal paint.
  Preserve status, title, palette, cursor and prompt updates. Reuse the existing
  Canvas invalidator and avoid rebuilding chrome solely for pixel changes.
- Acceptance: fragment a HUD/image update across reads and assert no partial
  presentation, then one committed update at sync-end. Test get-text/screenshot
  during the hold, in-place image changes, multiple boundaries in one read,
  idle/no-op commands, a missing terminator, repeated begins, resize and reconnect.
  Test publication counts directly; no Typometer work is needed.

### 5. Correct Delete and make alternate scrolling a profile policy — implemented

- Keep physical Delete distinct from Backspace through the input path. Since the
  pinned upstream encoder has a Delete fast-path defect, merely replacing the
  Win32 mapping with `KeyCode::Delete` is insufficient: fix the negotiated encoder
  path so Delete consistently uses its functional-key code for press/repeat/release.
- Add a per-profile alternate-scroll setting, defaulting off for game mode.
  Preserve compatibility scrolling for ordinary SSH profiles. When disabled,
  synthesize no cursor keys. Preserve application-requested mouse reports and
  the Shift override; do not invent local alternate-screen history.
- Acceptance: byte-exact Delete versus Backspace tests for legacy encoding and
  kitty flags 1, 3, 8 and 11, including modifiers and held-key game behavior.
  Test wheel policy in primary/alternate screens, tracking on/off, Shift on/off,
  game/non-game profiles, and after reconnect.

## Boundaries and follow-up

The rest of the attached review has not been comprehensively verified. CPU image
cache retention, unattainable geometry, and asset-pack rollback are worthwhile
follow-ups, not evidence established by the five probes above. In particular,
retain all kinds of assets from the published pack before pruning staged packs;
changing only WAV parsing does not resolve that separate rollback concern.

After implementation, run focused regressions, the existing all-target/all-feature
tests, strict Clippy and formatting checks, and the native control/UI smoke tests.
The high/medium-integrity integration check must be reported separately if its
required Windows token configuration is unavailable.

## Primary references

Implementation validation: 73 tests passed across all targets/features; strict
Clippy passed. The native medium-server/low-client test passed. The high-server/
medium-client variant is present but requires an elevated test process and was
not run. The clipboard-writing test remains intentionally opt-in.
Release GUI/CLI builds and native tab/font/interaction, player-dialog/wheel-policy,
search, and loopback control smoke checks passed. The control smoke covers raw
inactive-pane input, PNG capture, event-driven waits, tab open, resize, recording,
JSON failures, and saved-window restoration.

- [Synchronized output semantics and unspecified timeout](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md)
- [Direct2D bitmap dimension capability](https://learn.microsoft.com/en-us/windows/win32/api/d2d1/nf-d2d1-id2d1rendertarget-getmaximumbitmapsize)
- [PlaySound memory playback API](https://learn.microsoft.com/en-us/previous-versions/dd743680(v=vs.85))
- [PCM byte rate and block alignment](https://learn.microsoft.com/en-us/windows/win32/api/mmeapi/ns-mmeapi-waveformatex)
- [Windows mandatory integrity control](https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control)
- [Kitty functional-key encodings](https://sw.kovidgoyal.net/kitty/keyboard-protocol/#functional-key-codes)
