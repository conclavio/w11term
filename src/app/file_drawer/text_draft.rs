/// WinUI can return CR for every line break. Keep the file's UTF-8 BOM and
/// its original line endings, including mixed endings, outside the TextBox.
pub(super) struct TextDraft {
    visible: String,
    endings: Vec<Ending>,
    default_ending: Ending,
    bom: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Ending {
    Lf,
    CrLf,
    Cr,
}

impl TextDraft {
    pub(super) fn new(original: &str) -> Self {
        let content = original.strip_prefix('\u{feff}').unwrap_or(original);
        let mut visible = String::with_capacity(content.len());
        let mut endings = Vec::new();
        let mut chars = content.chars().peekable();
        while let Some(ch) = chars.next() {
            let ending = match ch {
                '\n' => Ending::Lf,
                '\r' if chars.next_if_eq(&'\n').is_some() => Ending::CrLf,
                '\r' => Ending::Cr,
                _ => {
                    visible.push(ch);
                    continue;
                }
            };
            endings.push(ending);
            visible.push('\n');
        }
        Self {
            visible,
            default_ending: endings.first().copied().unwrap_or(Ending::Lf),
            endings,
            bom: content.len() != original.len(),
        }
    }

    pub(super) fn visible(&self) -> &str {
        &self.visible
    }

    pub(super) fn edit(&mut self, value: &str) {
        let updated = value.replace("\r\n", "\n").replace('\r', "\n");
        if updated == self.visible {
            return;
        }
        let old = self.visible.as_bytes();
        let new = updated.as_bytes();
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let breaks = |bytes: &[u8]| bytes.iter().filter(|&&b| b == b'\n').count();
        let first = breaks(&old[..prefix]);
        let old_count = breaks(&old[prefix..old.len() - suffix]);
        let new_count = breaks(&new[prefix..new.len() - suffix]);
        // Equal breaks in the changed region retain their spelling. New ones
        // take the preceding line's ending, or the file's first ending.
        if new_count > old_count {
            let at = first + old_count;
            let ending = at
                .checked_sub(1)
                .map_or(self.default_ending, |i| self.endings[i]);
            self.endings
                .splice(at..at, std::iter::repeat_n(ending, new_count - old_count));
        } else if new_count < old_count {
            self.endings.drain(first + new_count..first + old_count);
        }
        self.visible = updated;
    }

    pub(super) fn contents(&self) -> String {
        let mut text = String::with_capacity(self.visible.len());
        if self.bom {
            text.push('\u{feff}');
        }
        for (i, line) in self.visible.split('\n').enumerate() {
            if i > 0 {
                text.push_str(match self.endings[i - 1] {
                    Ending::Lf => "\n",
                    Ending::CrLf => "\r\n",
                    Ending::Cr => "\r",
                });
            }
            text.push_str(line);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_edits_preserve_encoding_and_unchanged_line_endings() {
        for original in [
            "",
            "a",
            "\u{feff}one\r\ntwo\n三\rfour\r\n",
            "\n\n",
            "\r\r",
            "one\n",
        ] {
            let mut draft = TextDraft::new(original);
            let native = draft.visible().replace('\n', "\r");
            draft.edit(&native);
            assert_eq!(draft.contents(), original);
        }
        let mut draft = TextDraft::new("\u{feff}one\r\ntwo\n三\rfour");
        draft.edit("one\rTWO\r三\rfour");
        assert_eq!(draft.contents(), "\u{feff}one\r\nTWO\n三\rfour");
        draft.edit("one\rnew\rTWO\r三\rfour");
        assert_eq!(draft.contents(), "\u{feff}one\r\nnew\r\nTWO\n三\rfour");
        draft.edit("one\rnew\rTWO\rfour");
        assert_eq!(draft.contents(), "\u{feff}one\r\nnew\r\nTWO\nfour");
    }
}
