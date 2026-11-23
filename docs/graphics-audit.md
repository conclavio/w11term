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

The renderer collects visible tiles once per paint. It draws images with
`z < -1073741824` over the window background, then paints cell backgrounds.
It draws the remaining negative-z images before glyphs and nonnegative-z images
after glyphs. Each layer sorts placements by z-index and image ID. An overlap
at equal z shows the higher numbered kitty image on top. [Kitty placement rules](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-displayed-image-layout)

The engine's image table limits direct
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

An application can transmit several thumbnails without choosing image IDs.
Each thumbnail has its own placement. For an image with a client ID, each
`a=p` with omitted `p` or explicit `p=0` adds another copy on the grid.
Reusing an explicit nonzero image and placement ID moves that placement.
Transmitting new pixels under the same client image ID removes its previous
placements. [Kitty placement rules](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-displayed-image-layout)

The `independent-placements.vt` fixture samples thumbnails at different
positions and checks that the old position of a moved placement is empty.
The resource test repeatedly fills and clears a row of anonymous images,
then places a named image after 560 uploads.
An upload that only stores data or defines a virtual placement needs an
image ID or image number. w11term discards such uploads when both are absent.

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

`tests/fixtures/kitty/placements.vt` and `layers.vt` are the byte streams
consumed in order by all three renderers. `samples.json` gives zero-based
cell centers and expected RGB values.
It includes two differently colored placeholder cells, a blue relative child
and the yellow second animation frame. Run the Windows Direct2D check with
`cargo test --bin w11term offscreen_kitty_reference_cell_centers`. The engine
fixtures are `cargo test --test kitty_graphics --test image_limits --test synchronized`.
They also check loading, finite loops, signed gaps, fragmented transfers, old
snapshots, resizing, scrolling, parent deletion and error replies.

## Clearing images

`kitten icat --clear` sends `a=d,d=A`. A physical placement that intersects the live
grid is removed in full, including any rows in scrollback. A placement wholly
in history survives. An image with a surviving placement can be displayed again.
Lowercase `d=a` (also the default selector) keeps the image data for redisplay.
Clearing acts on the active primary or alternate screen, even while the user is
reading older output. [Kitty deletion protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/#deleting-images)

The cursor (`d=c/C`) and an explicit one-based cell (`d=p/P`) can select
individual placements. Use `d=q/Q` to restrict an explicit cell to a z-index.
A hit removes the entire placement, including relative children. Images still
used elsewhere on screen or in scrollback retain their data after uppercase
deletion. Unicode placeholders remain editable text. The shared
`clear-cell.vt` fixture compares the resulting pixels in stock kitty and
Direct2D after cursor deletion, redisplay, cell deletion and z-index filtering.

Use `d=y/Y` for all placements touching a live row. Column deletion
(`d=x/X`) and deletion by z-index (`d=z/Z`) also reach placements in
scrollback on the active screen. The stock kitty and Direct2D
`clear-groups.vt` tests check row and column selection, layer removal, and
redisplay after a different placement keeps an image's data alive.

Use `d=n/N,I=<number>` to target the newest image uploaded under that number.
An optional `p` selects one placement on that image. When `N` frees the
newest image, the same number addresses the next newest surviving image.
Kitty 0.45 and Direct2D share the `clear-number.vt` comparison. Kitty 0.32.2
applies number deletion to every image with that number.
[Kitty 0.45](https://github.com/kovidgoyal/kitty/blob/v0.45.0/kitty/graphics.c#L2011-L2020) · [Kitty 0.32.2](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/graphics.c#L1885-L1888)

Animation frame deletion uses `d=f/F` with an image ID or number and a
one-based frame number in `r`. A missing or zero `r` selects the first frame.
A number beyond the end selects the last frame. Lowercase `f` keeps the
only remaining frame, while uppercase `F` removes its image and placements.
The shared `clear-frame.vt` fixture checks these results in stock kitty and
Direct2D. [Kitty 0.45 frame deletion](https://github.com/kovidgoyal/kitty/blob/v0.45.0/kitty/graphics.c#L1591-L1630)

Applications can query an action with `a=q,i=<id>,e=<action>` and include
the option values they need. For example, `a=q,i=31,e=T,U=1,C=1` reports
`e=T,C=1,U=1` to confirm Unicode placeholders and cursor preservation.
The response omits unavailable options such as file and shared-memory
transports. Queries do not load their payload or disturb an upload in progress.
[Kitty feature queries](https://sw.kovidgoyal.net/kitty/graphics-protocol/#querying-support-for-protocol-features)

Indexed uploads use either one palette index per pixel or an index followed
by alpha. Each indexed image counts both its source indices and expanded
RGBA pixels toward the 32 MiB per-image and 64 MiB image-table budgets.
Palette changes recolor committed images and produce new GPU texture keys.
Older snapshots keep their captured colors. Palette index 254 uses the
default foreground, which is the fallback for the renderer's URL-hover
color. The indexed formats support static images, and frame uploads,
animation controls and frame composition return an error for indexed
images. The Windows tests check the protocol replies, RGBA values and
rendered Direct2D pixels. [Kitty indexed color](https://sw.kovidgoyal.net/kitty/graphics-protocol/#indexed-color-data)

An addressed upload or placement returns one status reply after it is
processed. Placement replies include the requested placement ID, when
present. A chunked upload replies on the final chunk. Clients can set
`q=1` to receive only errors or `q=2` to silence all replies.
An identified request for an unsupported local transport gets an error.
[Kitty placement replies](https://sw.kovidgoyal.net/kitty/graphics-protocol/#display-images-on-screen)

Animation frame uploads acknowledge the one-based frame number that was
edited or appended. Multipart frame uploads retain their first chunk's
address and reply after the final chunk. Frame composition acknowledges
the result and checks the source and destination rectangles before changing
the destination. Successful playback controls run silently. Image and
frame errors follow the requested quiet mode.
[Kitty animation reference](https://github.com/kovidgoyal/kitty/blob/v0.50.0/kitty_tests/graphics.py#L1694-L1873)

Erase-in-line and partial erase-in-display clear text without removing
direct kitty graphics. Unicode placeholder images disappear when their
placeholder text is erased. A full display erase clears visible graphics
and keeps named image data available for later placement.
[Kitty interaction with terminal actions](https://sw.kovidgoyal.net/kitty/graphics-protocol/#interaction-with-other-terminal-actions)

In a bounded scroll region, a direct kitty placement that crosses a margin
stays in place while the text scrolls. Placements entirely inside the region
move with the text and keep their original scale when clipped at the edge.
Rows may enter primary-screen scrollback during a bounded scroll. The image
tiles clipped by the margin stay out of those rows.
[Kitty scrolling reference](https://github.com/kovidgoyal/kitty/blob/v0.50.0/kitty_tests/graphics.py#L1423-L1500)

Inserting or deleting characters or lines leaves direct kitty placements
at their original positions. Images displayed through Unicode placeholders
follow the edited text.
[Kitty text editing implementation](https://github.com/kovidgoyal/kitty/blob/v0.50.0/kitty/screen.c#L2889-L3009)

Current kitty and w11term preserve an image transmitted without a placement
when clearing the visible screen. Kitty 0.32.2 also released stored-only
images on `d=A`. The shared screenshot fixture uses three placed images.
[Current kitty](https://github.com/kovidgoyal/kitty/blob/master/kitty/graphics.c#L2570-L2605) · [Kitty 0.32.2](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/graphics.c#L1683-L1699)

`kitten icat --clear-all` selects the entire client image ID range with
`d=R,y=4294967295`. Narrower ranges use inclusive `x` and `y` bounds and
reach placements in scrollback. Lowercase `r` keeps the selected image data
and uppercase `R` releases it. A delete command also cancels an unfinished
transmission on that screen. [icat implementation](https://github.com/kovidgoyal/kitty/blob/master/kittens/icat/main.go)

The Direct2D and stock kitty checks share `clear-setup.vt` and `clear-visible.vt`.
The scene scrolls two lines, clears visible placements and tries to redisplay
three images. Only the fully scrolled-off red image reappears. A Windows test
also checks GPU texture counts on successive draws (three, one, then zero
after the range clear). Rust wire tests cover range deletion, which kitty
introduced in 0.33 after the pinned 0.32.2 reference.

Range selection addresses client image IDs. On a width reduction, a row containing only
spaces and image tiles can clip horizontally under the existing text reflow
rules. Deleting a placement checks its retained tiles after a resize.

## Linux reference captures

The `Kitty graphics reference` workflow runs two independent Linux jobs under
Xvfb with Mesa software GL. One captures Ubuntu 24.04's kitty 0.32.2. The
other captures Ghostty 1.4.0, the release that added animation and relative
placements. Both jobs run on pull requests and feed the merge release check.
[Ghostty release notes](https://ghostty.org/docs/install/release-notes/1-4-0)

The Ghostty job runs on GitHub’s Ubuntu 26.04 x64 image. It downloads the
community-built `ghostty_1.4.0-0.ppa2_amd64_26.04.deb` from
`mkasberg/ghostty-ubuntu` release `1.4.0-0-ppa2`. Before installing, it
checks SHA-256 `2d70bf419494362dffb6463d8ff3ab76c3fd1b0e247ea9b0b0aeda5c2fa4c681`
and the Debian package name, version and architecture. Ubuntu supplies the
capture dependencies, including Mesa, GTK, Xvfb and DejaVu Sans Mono.
The job checks the running Ghostty version and records the runner image and
installed package versions alongside the captured PNG and cell samples.
[Package release](https://github.com/mkasberg/ghostty-ubuntu/releases/tag/1.4.0-0-ppa2) · [Ubuntu 26.04 runner availability](https://github.com/actions/runner-images/issues/14747)

To repeat either capture on Linux, install the corresponding terminal,
Xvfb, xauth, xdotool, ImageMagick and Python with Pillow. Ghostty also uses
a D-Bus session and the DejaVu Sans Mono font. From the repository root run

```sh
xvfb-run -a /usr/bin/python3 tests/kitty-reference.py artifacts/kitty
dbus-run-session -- xvfb-run -a /usr/bin/python3 tests/kitty-reference.py artifacts/ghostty --terminal ghostty
```

Each job uploads its full window PNG, terminal log, package versions, measured
PTY geometry and a per-cell report with pixel coordinates and expected and
observed RGB. A mismatch of 32 or more in any channel fails the check.
The Linux captures use 9-pixel-wide cells so each placeholder cell has a
single center pixel. Ghostty's default 10-pixel-wide cell puts the center
between two pixels. On the red/green boundary its right-hand pixel already
contains interpolated green. The Ghostty harness sets `adjust-cell-width=-1`
for the pinned font and Xvfb display.
Eight samples in the seventh row exercise a default background, explicit cell
backgrounds on both sides of the z threshold, negative and zero z across a
solid glyph, and equal- and different-z image overlaps. The solid image tiles
contain 8×8 texels so the cell-center sample falls inside an opaque texture
region on all three renderers. The original five sample
points exercise placement and selected animation frames. GPU interpolation,
other deletion selectors, placeholders under every rendition attribute, and every
possible compositing edge remain outside this fixture.

The equal-z sample transmits image 70 before 71, then places 71 before 70.
Both reference terminals show 71 on top. Ghostty 1.4.0 and w11term use the
client's image ID to break ties. Kitty 0.32.2 uses an internal ID assigned at
transmission, so reversing transmission order can change its result. The
protocol leaves overlapping placements with identical image ID and z-index
unspecified. [Protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-displayed-image-layout) · [Ghostty ID ordering](https://github.com/ghostty-org/ghostty/blob/v1.4.0/src/renderer/image.zig#L535-L553) · [Kitty ID allocation](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/graphics.c#L381-L391) · [Kitty sort](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/graphics.c#L1090-L1117)

An explicit RGB cell background equal to the terminal default covers a
below-background image in w11term and Ghostty 1.4.0. Kitty 0.32.2 compares
resolved RGB values and lets the image show through that cell. The shared
fixture uses blue against a black default. The w11term-only native test
`offscreen_explicit_default_rgb_covers_a_below_background_image` checks
explicit black beside a default cell over the same red image.
[Ghostty background opacity](https://github.com/ghostty-org/ghostty/blob/v1.4.0/src/renderer/generic.zig#L3203-L3225) · [Kitty color resolution](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/cell_vertex.glsl#L153-L165) · [Kitty background comparison](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/cell_vertex.glsl#L196-L204)

[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) and
[kitty 0.32.2 placement implementation](https://github.com/kovidgoyal/kitty/blob/v0.32.2/kitty/graphics.c)
