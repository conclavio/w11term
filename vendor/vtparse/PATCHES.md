# Local modifications

Source: https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/vtparse

Workspace dependency versions are expanded. APC strings are limited to 48 MiB;
overflow discards the complete string until its terminator, then parsing resumes.
The upstream MIT license is preserved in LICENSE.md.

After the two selectors in an OSC 9;9 directory report, the parser preserves
the complete filename as one field, including literal semicolons. Payloads
over 128 KiB clear the reported directory.

OSC 1337 `RemoteHost=` and `CurrentDir=` also carry one literal field.
The parser retains at most 1 KiB of user and host or 128 KiB of path.
An oversized, malformed UTF-8, or control-containing field produces an
empty report. Other iTerm2 commands keep their normal parameter handling.

Actors can opt into waiting for a complete string terminator on a DCS.
The unhook callback distinguishes ST from cancellation for those strings.
