# Windows Reactor

The vendored source is the windows-reactor 0.100.0 crate published from
[a57cc321](https://github.com/microsoft/windows-rs/tree/a57cc321bfdbac6bb98c488ba49f399da8fcb98a/crates/libs/reactor).
Its original MIT and Apache-2.0 license files are included.

Local changes backport the element cursor property from
[windows-rs PR #5414](https://github.com/microsoft/windows-rs/pull/5414).
The 0.100.0 property reconciler sets or clears WinUI's protected cursor
on the Border through the Windows App SDK 2.4.0 ABI. Remove the local Cargo
patch when upgrading to a release that provides Border::pointer_cursor.
