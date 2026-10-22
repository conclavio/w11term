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

## Remaining batches

- Automation and SFTP: L5/L6/L8/L16; negotiated read sizes; explicit overload
  responses, independent wait capacity, reliable watchers, cancellation cleanup.
- Trust and failure reporting: L1/L9/L13; revoked/patterned known-host handling,
  anchored enrollment confirmations, session failure containment.
- Compatibility and small correctness items: L11/L14/L19; reconnect screen
  separation, kitty event/modifier/DPI behavior and authentication cleanup.
- Contract clarity: document manifest grammar/nonzero sizes/SFTP v3 and timing;
  explicit profile policy for OSC 440/assets, bounded OSC 52 writes while
  retaining the requested clipboard feature.

L2 server authentication, M4 GPU cache thrashing and L15 alternate scroll policy
are already resolved. L17/L20 speculative performance changes remain deferred
pending profiling; no performance claim depends on implementing them.
