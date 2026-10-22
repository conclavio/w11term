# Local modifications

Source: https://github.com/wezterm/wezterm/tree/e2a98e1b8af156988ad0ac778658ac27b60a55fb/vtparse

Workspace dependency versions are expanded. APC strings are limited to 48 MiB;
overflow discards the complete string until its terminator, then parsing resumes.
The upstream MIT license is preserved in LICENSE.md.
