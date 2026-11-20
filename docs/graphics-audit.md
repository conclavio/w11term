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
images retained in primary scrollback or other image protocols. APC buffers are capped at 48 MiB; oversized strings are discarded through their terminator. Kitty animations are capped at 256 frames / 32 MiB and accounted in the 64 MiB table. The separate CPU dedup cache retains at most 4 MiB, and clears on deletion, reset and animation edits. Chunked animation frames share the direct transmission budget. Chunked queries are unsupported.
Large graphics or unused table entries may be rejected or evicted.

DA1 includes attribute 4 and XTVERSION identifies `w11term <package-version>`.
Both replies are tested as actual parser response bytes, alongside negotiated
press/repeat/release encoding and transport-reset behavior.

## Animation and placement fixtures

The session worker interprets kitty `a=a` for client-selected frames, loading
and running modes, frame gaps and loop counts. It schedules the next visible
frame with a monotonic timer. A stopped animation, an undisplayed image, and a
loading animation waiting for another frame require no presentation timer.
Each committed snapshot freezes the selected pixels. Frame edits under another
image ID cannot change them. Direct `a=f` accepts bounded chunked input.

`U=1` records a virtual placement. It acquires screen coordinates from actual
U+10EEEE cells. The foreground color, underline color and up to three combining
marks identify the image, placement, row and column. Omitted coordinates follow
kitty's left-cell inheritance rules. The source glyphs remain in selectable
terminal text. Relative `P,Q,H,V` placements follow their parent through
repositioning, reflow and scrollback, including a parent outside the viewport.
Deleting a parent removes its dependents. Cycles and missing parents return
`ECYCLE` and `ENOPARENT`. Each terminal accepts up to 1,024 placements, a
relative ancestry depth of 16, and 65,536 projected visible tiles per snapshot.

`tests/fixtures/kitty/placements.vt` is the exact byte stream consumed by both
renderers. `samples.json` gives zero-based cell centers and expected RGB values.
It includes two differently colored placeholder cells, a blue relative child
and the yellow second animation frame. Run the Windows Direct2D check with
`cargo test --bin w11term offscreen_kitty_reference_cell_centers`. The engine
fixtures are `cargo test --test kitty_graphics --test image_limits --test synchronized`.
They also check loading, finite loops, signed gaps, fragmented transfers, old
snapshots, resizing, scrolling, parent deletion and error replies.

The `Kitty graphics reference` workflow runs Ubuntu 24.04's kitty 0.32.2
under Xvfb using Mesa software GL. It compares an X11 capture against the
same cell coordinates and uploads the PNG, measured grid, and package
versions. On a Linux host with those packages installed, run
`xvfb-run -a /usr/bin/python3 tests/kitty-reference.py artifacts` from the repo.
The five colored sample points test placement and selected pixels. GPU interpolation,
general deletion selectors, placeholders under every rendition attribute, and every
possible compositing edge remain outside this fixture.

[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) and
[kitty 0.32.2 placement implementation](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/graphics.c)
