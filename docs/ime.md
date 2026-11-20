# Windows input methods

The terminal draws composition text at the focused pane’s insertion point. Windows controls the candidate list. Committing a composition sends its UTF-8 text to the terminal transport once. Moving focus to another pane or opening Find, the tab switcher, a profile editor, or an authentication prompt releases the terminal composition. Text boxes continue using their native input method.

The bridge listens for composition on the window receiving input. It reads UTF-16 text and the composition cursor from IMM32. DirectWrite shapes the preedit and measures its caret. Candidate placement uses the measured pane bounds and display scale. [Windows composition messages](https://learn.microsoft.com/en-us/windows/win32/intl/wm-ime-composition) and [candidate window positioning](https://learn.microsoft.com/en-us/windows/win32/api/imm/ns-imm-candidateform).

## Verification

On Windows, build the GUI with `cargo build --features ui-smoke` and run `.\tests\ime-ui-smoke.ps1`. This opens a temporary terminal lab, splits it, sends native composition lifecycle messages, checks the candidate position and verifies a single commit containing CJK and supplementary-plane characters. The fixture supplies composition strings in place of an installed language IME. The regression tests also check cancellation and pixel conversion at 150% scaling.

The development machine has US English with the US keyboard enabled. Interactive candidate selection with a language IME needs a Windows session that already has one configured. To check it manually:

1. Open two terminal labs side by side. Switch to Microsoft Japanese IME in the right pane, type `nihongo`, and open the conversion list. Check that the preedit stays on the right and the candidate list follows its caret.
2. Choose 日本語 and press Enter. Check that it appears once. Start another conversion and press Escape. Check that the second conversion leaves no terminal input.
3. With a conversion open, switch panes and type. Check that the first pane clears the preedit and the new pane gets only the new input.
4. Repeat in Find and in a profile text field. Move the window to a display with a different scale and repeat the candidate positioning check. Chinese Pinyin and Korean Hangul should also be checked with their installed Microsoft IMEs.
