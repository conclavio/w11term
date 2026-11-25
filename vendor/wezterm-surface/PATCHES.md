# WezTerm surface and line storage

The source is from WezTerm commit `e2a98e1b8af156988ad0ac778658ac27b60a55fb`.
The original MIT license is preserved in `LICENSE.md`. Cargo dependencies use
the same versions and revision as the original workspace.

Rows containing explicit text widths preserve their cells outside compressed
grapheme storage. Edits clear intersecting spans, and reflow, scrollback and
implicit hyperlink scanning honor the stored widths and text boundaries.
