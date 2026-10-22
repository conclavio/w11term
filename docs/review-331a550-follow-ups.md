# Review follow-ups

The five original fixes were pushed through c268991. This document tracks the
remaining worthwhile findings from the source review at 331a550.

## Batch 1: resource and persistence correctness

- M3 / L18: 4 MiB CPU dedup-cache budget, deletion/reset/mutation cleanup;
  bounded APC collection, chunk count, strict PNG decode limits, animation
  frame count/bytes and table growth accounting. Live scrollback references
  remain separate; this is not a global terminal RAM ceiling.
- Asset drift 1 / L10: publish all asset kinds under shared OS-lock leases;
  serialize sync/prune with an endpoint filesystem lock across processes.
  Failed replacements retain the previous pack, and other live panes keep
  their files. Hashing, validation and filesystem work run off Tokio workers.
- M6 / L12: at most two grid correction attempts plus a 300 ms fallback;
  native user sizing cancels a pending request. Placement fits small work areas.
- Regressions: bounded cache/delete/reset, APC overflow and recovery, animation
  frame cap, real SSH/SFTP pack rollback including help and two live stores,
  grid convergence/cancellation, tiny and negative-coordinate work areas.

## Batch 2: automation and SFTP availability

- L6: bounded ingress, eight ordinary requests and sixteen long-polls in separate
  pools; saturation returns an explicit JSON busy error without executing work.
- L16: frame/control watcher rejection is handled with a native UI retry timer
  independent of background task slots.
- L5: oversized SFTP data replies fail before disk writes; get supports optional
  --max-bytes without imposing a small universal file limit.
- L8 / drift 3: asset reads use negotiated limits and cancellation enqueues Close
  through an independent cleanup queue. Abandoned requests finish within their
  deadlines, abandoned opens close their handles, and cancellation does not reset
  unrelated handles. Actual transport failure/timeouts still invalidate handles.
- Regressions cover native 17-client long-poll saturation with responsive ordinary
  commands, timer execution/cancellation, asset guard cleanup, oversized wire
  replies, failed download publication and a cancelled read preserving handles.

## Batch 3: trust confirmation and failure reporting

- L1: host verification handles case-folded names, comma lists, wildcard/negated
  patterns and OpenSSH hashed names. Revoked keys fail before prompts or auth,
  including explicit pins. Matching certificate-authority entries fail explicitly
  until host-certificate verification is implemented; they never trigger TOFU.
- L9: enrollment confirmations and listing boundaries start at line boundaries;
  chat substrings cannot confirm enrollment. The fresh double-list check remains.
- L13: a worker panic publishes a visible failure and session-error event while
  retaining the last committed screen; the engine is discarded, never resumed.
  Reopening the tab is the recovery action.
- list now returns failed/closed panes with an explicit error instead of omitting them.
- Regressions cover pattern/hash/negation behavior, revocation on real handshakes
  with and without a configured pin, chat spoof rejection, normal enrollment,
  injected engine failure and closed-pane listing.

## Batch 4: input and reconnect correctness

- L11: reconnect scrolls the primary screen into history before new session output;
  stale graphics remain only as historical cells rather than orphaned live content.
- L14: native modifier keys reach REPORT_ALL; Enter/Tab/Backspace releases require
  that flag. Shifted keys carry the unshifted layout identity, and composed text
  retains press/repeat/release identity and associated Unicode text. Mouse pixel
  offsets are converted from DIPs using the current scale.
- L19: a transport failure during password auth preserves the zeroizing candidate
  for reconnection; unsolicited Success messages cannot announce connection twice.
- Regressions cover byte-exact modifier/safety-key/shift/composition matrices and
  reconnect isolation of primary text and graphics, in addition to existing held-key
  bridge and SSH reconnect tests.

## Batch 5: explicit policy and contract completion

- L3/L4: optional profile controls for OSC 440 playback and asset sync follow game
  profiles by default, with wizard/menu overrides. Bundled play profiles opt in.
  OSC 52 retains the requested default and has a process-wide four-writes/second
  remote limit; local copy is independent. BEL queue overflow emits sound-error.
- Drift 2/3: document exact name/version grammar, reserved names, nonzero sizes,
  unknown-field rejection, SFTP v3, negotiated read sizes and timing.
- L7: transfer deadline scales with declared pack size rather than rejecting large
  otherwise valid packs at a fixed 180 seconds. This is not a latency guarantee.
- Latest pack announcement is retained while a sync runs; completion wakes the
  session to process it. Local sound fallback matches case exactly.
- Test discovery records use isolated directories; control listener termination
  removes its discovery record. Endpoint lock acquisition is cancellation-safe.
- Regressions cover explicit/automatic profile policies, queue overflow, clipboard
  rate limiting without writing the clipboard, exact-case sounds, pending pack
  versions, size-based budgets and a real server advertising 512-byte SFTP reads.

## Deliberate remaining limits

L17/L20 performance changes remain deferred pending profiling. The memory bounds
are not a universal cap on all terminal/scrollback image allocations. Full kitty
animation rendering and host-certificate authority verification remain unsupported.
The latter fails explicitly rather than offering TOFU.

The application retains challenge answers in zeroizing strings, but russh's public
keyboard-interactive API takes owned Vec<String> and does not guarantee zeroizing
those internal copies. Fixing that part of L19 requires an upstream API change or
vendoring the SSH library; it is recorded here rather than claiming full erasure.
The elevated high-server/medium-client test and clipboard-writing smoke remain
opt-in; the normal native low/medium integrity and renderer tests run routinely.

## Final validation

98 tests passed across all targets/features; two opt-in tests were skipped as
identified above. Formatting and strict Clippy passed. Release GUI/CLI builds,
native tab/font/interaction/player/search smokes, and the loopback control smoke
passed. The latter also passed against the final normal build without ui-smoke.

The user's existing release executable was running and was not interrupted.
The fresh normal GUI is target/release/w11term-review.exe; the temporary alternate
Cargo target used to build it is not part of the committed project configuration.
