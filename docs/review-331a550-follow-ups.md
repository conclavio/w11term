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
