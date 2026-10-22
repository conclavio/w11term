# Game client protocol, version 1

This document defines w11term's proposed contract with the rustmush SSH gateway.
The gateway implementation is separate. OSC 440 is a private extension, not a
standard or an inference that every terminal supporting sixel supports sound.
Servers must enable it explicitly for a known supporting client.

## Discovery and session setup

w11term requests a PTY with `TERM=xterm-256color` and sends an SSH environment
request `COLORTERM=truecolor` before requesting the shell. Accept that request
in the gateway and apply it to the player's terminal capability model.

DA1 (`ESC [ c`) replies with `ESC [ ? 65;4;6;18;22;52 c`; attribute 4 advertises
sixel. XTVERSION (`ESC [ > 0 q`) replies with `ESC P >|w11term <version> ESC \`.
The name and version can identify this private OSC extension. Sixel alone does
not advertise OSC 440, and unsupported terminals must continue working silently.

## OSC 440

All examples below use ST (`ESC \`) to terminate the OSC. BEL termination is also
accepted by the terminal parser. There is no reply and no private probe.

| Purpose | Bytes, with escapes shown |
| --- | --- |
| Play a sound by name | `\x1b]440;1;play;shot\x1b\\` |
| Announce the current asset pack | `\x1b]440;1;pack;2026_10_21_a\x1b\\` |

There must be exactly four fields: `440`, wire version `1`, verb, argument.
An argument is 1–64 ASCII letters, digits, `_`, or `-`, case sensitive.
Windows DOS device names (`CON`, `NUL`, `COM1`, etc., case insensitive) are rejected.
No filenames, paths, URLs, shell commands, base64 audio, volume, or executable content
are accepted. Unknown versions/verbs and malformed commands are ignored and
reported as `sound-error` events. An overflowing command queue drops commands.
Do not repeatedly send pack announcements with every rendered frame.

`play` resolves a name from the fully verified pack for this connection first,
then `%LOCALAPPDATA%\w11term\sounds\<name>.wav`. `system` and `off` are reserved
for the Windows default bell and silence. The per-profile `player.mute` applies
to all sounds, including BEL. The profile's `player.bell` defaults to `system`;
it can be `off` or another valid sound name. BEL always produces a `bell` event,
even when muted. The audio worker emits a `sound` event with `name` and `outcome`
(`played`, `muted`, `missing`, `invalid`, `failed`, or `rate-limited`).
`played` means the Windows audio call succeeded; it does not prove audible output.

Playback runs outside the parser and UI threads, is serialized, and has a bounded
16-command audio queue and a 40 ms rate limit. A later sound does not interrupt
the current sound. The gateway should send short game effects rather than music.
Files must be RIFF/WAVE, PCM format 1, mono or stereo, 8/16-bit, 8–96 kHz,
and at most five seconds long.

## Asset manifest and SFTP

The subsystem is `sftp` on a sibling channel of the existing authenticated SSH
connection. It requires no exec request, second login, HTTPS endpoint, or new port.
`pack` schedules synchronization when `player.sync_assets` is true (the default).
The manifest is always `/assets/manifest.json`:

```json
{
  "version": 1,
  "pack_version": "2026_10_21_a",
  "files": [
    {
      "name": "shot",
      "path": "sounds/shot.wav",
      "kind": "sound",
      "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      "size": 4096
    }
  ]
}
```

The example hash/size are placeholders. `version` is the manifest schema version;
`pack_version` is an opaque pack identifier and must exactly match the announcement.
Names must be unique across the pack. Kinds are `sound`, `sprite`, `font`, and
`help`. Non-sound files are cached only: no automatic font installation, document
opening, code execution, or sprite protocol is implied.

Paths are relative to `/assets/`, contain at least two nonempty slash-separated
segments, and cannot contain `.`/`..` segments, backslashes, drive letters, or URLs.
Segments contain only ASCII letters, digits, `_`, `-`, spaces, and `.`. Paths are
unique and at most 200 bytes. Sound paths end in `.wav`. Hashes are exactly 64
lowercase hex digits and cover the complete file bytes. Sizes are exact byte
counts, not hints. Unknown manifest fields are rejected in version 1.

Client limits: 256 KiB manifest, 256 files, 8 MiB per file, 64 MiB declared pack,
15 seconds per SFTP request, 180 seconds per synchronization. Reads are sequential
16 KiB chunks with 16 ms spacing (at most roughly 1 MiB/s). The server should also
cap bytes per connection so asset traffic cannot starve interactive rendering.
The client opens files read-only and uses open/read/close for synchronization.

The gateway exposes only a curated, read-only virtual `/assets` tree. It should
implement open/read/close, opendir/readdir, stat/lstat/fstat, and realpath. Reject
write/create/truncate/append, rename, unlink, mkdir/rmdir, symlinks, and arbitrary
paths. Resolve paths inside the published pack; filesystem symlinks must not escape
that root. This server-side enforcement remains necessary even though the client
validates manifest paths. Ship the manifest and assets atomically with deployment.

The cache lives at `%LOCALAPPDATA%\w11term\assets\<endpoint-sha256>\<content-sha256>`.
The endpoint includes SSH user, host, and port. Cached bytes are rehashed before
reuse, so only changed/missing content is fetched. Each download is verified and
atomically replaced. Sound lookup switches to the new pack only after every file
has passed verification. Failure retains the previous usable pack. Obsolete cached
hashes are removed after successful publication. There is no cross-host lookup.

Announcements received while a synchronization is in progress are currently
ignored. Announce once after login and again after a pack deployment; a server may
repeat an announcement after observing that its previous sync failed. Sounds sent
before the pack is ready can report `missing`; announce early in the welcome flow.

## Hello JSON fields

For consumers of rustmush's application-level Hello, add an optional field:

```json
{
  "asset_pack": {
    "version": 1,
    "pack_version": "2026_10_21_a",
    "manifest": "/assets/manifest.json"
  }
}
```

This is metadata for the game's own Hello schema; it does not replace or rename
existing fields. Omit `asset_pack` when unavailable. The terminal does not decode
private game JSON embedded in terminal output. The gateway translates that metadata
into the OSC `pack` announcement for w11term. The fixed manifest path is deliberate;
Hello does not grant permission to fetch arbitrary locations.

## Automation events

`w11term-cli events --pane ID --since CURSOR [--timeout SECONDS]` returns a bounded
per-pane journal. Asset operations produce `assets-syncing`, `assets-ready`
(`pack_version`, `downloaded`), or `assets-error` (`pack_version`, `error`).
Sound, bell, title, notification, connection, and recording events share the journal.
Sequence cursors are local to one pane in one process. Retain the returned `next`;
`lost=true` means older events were evicted. The journal keeps 4096 entries and
truncates oversized data; it is an observation channel, not a durable audit log.
No event changes window focus. Tests should assert `sound` outcomes rather than
relying on the machine's speakers.
