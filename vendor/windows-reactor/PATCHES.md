# Windows Reactor

The vendored source is the windows-reactor 0.100.0 crate published from
[a57cc321](https://github.com/microsoft/windows-rs/tree/a57cc321bfdbac6bb98c488ba49f399da8fcb98a/crates/libs/reactor).
Its original MIT and Apache-2.0 license files are included.

Local changes backport the element cursor property from
[windows-rs PR #5414](https://github.com/microsoft/windows-rs/pull/5414).
The 0.100.0 property reconciler sets or clears WinUI's protected cursor
on the Border through the Windows App SDK 2.4.0 ABI. Remove the local Cargo
patch when upgrading to a release that provides Border::pointer_cursor.

The local element reference can request scrolling after layout for the SFTP
log tail. [WinUI StartBringIntoView](https://learn.microsoft.com/windows/windows-app-sdk/api/winrt/microsoft.ui.xaml.uielement.startbringintoview)

Compact toolbar buttons and toggle buttons can set native
[Control.Padding](https://learn.microsoft.com/windows/windows-app-sdk/api/winrt/microsoft.ui.xaml.controls.control.padding)
to reserve the content area for their icons.

An explicit tab strip width or selection change queues a scroll to the
selected header after layout. This keeps the selected header visible when
the native title bar narrows.

A menu attached to a TabViewItem uses WinUI's ContextFlyout property for
the mouse and keyboard context-menu gestures. The native TitleBar left
header accepts the tab strip in a pointer-interactive region. The runtime
refreshes its drag regions when the strip width changes independently of
the window.
