# Graphics audit: cockpit replacement path

The targeted path is a fresh direct kitty `a=T` transmission using one image id
and placement id, at the cockpit's 20 Hz update rate. It is static-frame replacement,
not kitty's animation-frame (`a=f`) scheduling protocol.

An automated Direct2D regression submits 100 distinct frames under `i=7,p=1`,
repaints through one persistent renderer, changes terminal width halfway through,
then deletes `d=I,i=7`. Every frame retains its placement and owns exactly one GPU
texture (256 bytes for the test's 8×8 RGBA image). After deletion and repaint, the
GPU cache owns zero textures and zero pixel bytes. Frames are submitted without
sleeping; this checks resource behavior and correctness, not a measured 20 Hz
performance guarantee. The offscreen PNG test also verifies kitty, sixel, and HUD
pixels through the production drawing code.

Previously GPU textures survived model deletion/replacement until the entire cache
hit its budget. Now each repaint retains only textures referenced by visible cells.
Changing tabs or scrolling releases textures that leave the visible model. The
renderer has a 32 MiB / 512-texture cache and rejects larger individual textures.
Encoded image decoding uses dimension and allocation limits before conversion.
GPU release occurs at the next paint, not synchronously when the parser sees delete.

The renderer also checks the actual Direct2D maximum bitmap dimension. Invalid or
unsupported images are isolated and remembered while visible on that device.
Admission is attempted once per distinct image per paint; over-budget images are
deferred instead of clearing textures already needed by the frame. Rejection state
and cached textures are discarded when no longer visible or when the device changes.
Regressions cover a 20,000×1 image, malformed pixels, budget deferral, and recovery.

Published snapshots freeze the displayed image data, including in-place kitty frame
edits. Synchronized-output holds use those committed pixels for both Canvas and
screenshots while the parser updates its separate working state. Unchanged frozen
images are shared across snapshots rather than copied again for every cell or paint.

The renderer sorts all visible tiles by z-index inside the below-text and above-text
layers, including placements anchored in different cells. Stable ties preserve model
order. This fixes traversal order overriding explicit z-indices. Cell backgrounds
are painted first, then negative-z images, text, and nonnegative-z images. The full
kitty negative-z/background interaction and every blend/placement extension have
not been exhaustively compared against reference terminals.

The engine's image table now counts replacement of id zero correctly, limits direct
transmissions to 48 MiB encoded / 32 MiB inflated, limits decoded kitty frames to
32 MiB, and rejects image-table growth past 64 MiB or 512 ids. Unreferenced images
are pruned when the table exceeds 32 MiB. Remote SSH peers cannot request local
file, temporary-file, or shared-memory kitty transports. These limits cover direct
replacement; they do not establish a global RAM limit for sixel, iTerm images,
images retained in primary scrollback or other image protocols. APC buffers are capped at 48 MiB; oversized strings are discarded through their terminator. Kitty animations are capped at 256 frames / 32 MiB and accounted in the 64 MiB table. The separate CPU dedup cache retains at most 4 MiB, and clears on deletion, reset and animation edits. Chunked query/animation transmissions are explicitly unsupported.
Large graphics or unused table entries may be rejected/evicted. The renderer still
shows the first frame of animated images.

DA1 includes attribute 4 and XTVERSION identifies `w11term <package-version>`.
Both replies are tested as actual parser response bytes, alongside negotiated
press/repeat/release encoding and transport-reset behavior.
