use super::Setup;
use crate::sftp::DotfileBundle;
use anyhow::{Context, Result, ensure};
use std::ops::Range;

const BEGIN: &str = "# >>> w11term shell setup >>>";
const END: &str = "# <<< w11term shell setup <<<";

fn block(setup: Setup) -> String {
    format!("{BEGIN}\n{}\n{END}", setup.script().trim_end())
}

fn lines(text: &str, block: &str) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    text.match_indices(block).find_map(|(start, _)| {
        let end = start + block.len();
        let starts_line = start == 0 || bytes[start - 1] == b'\n';
        let ends_line = end == bytes.len() || bytes[end] == b'\n';
        (starts_line && ends_line).then_some(start..end)
    })
}

pub(super) fn insert(existing: &str, setup: Setup) -> Result<String> {
    let shell_modes = || {
        Setup::ALL
            .into_iter()
            .filter(|mode| mode.shell() == setup.shell())
    };
    let marker_count = existing
        .lines()
        .map(str::trim_start)
        .filter(|line| {
            line.starts_with("# >>> w11term shell setup")
                || line.starts_with("# <<< w11term shell setup")
        })
        .count();
    let decline = "The existing w11term shell setup block was edited. Remove that block before adding a new one.";
    let added = block(setup);
    let mut revised = existing.to_owned();
    if marker_count == 0 {
        ensure!(
            shell_modes().all(|mode| lines(existing, mode.script().trim_end()).is_none()),
            "The fragment already contains a pasted w11term setup. Remove that copy before adding a new one."
        );
        if !revised.is_empty() && !revised.ends_with('\n') {
            revised.push('\n');
        }
        revised.push_str(&added);
    } else {
        ensure!(marker_count == 2, "{decline}");
        let range = shell_modes()
            .find_map(|mode| lines(existing, &block(mode)))
            .context(decline)?;
        revised.replace_range(range, &added);
    }
    DotfileBundle::validate_fragment(&revised)?;
    Ok(revised)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_and_switches_the_marked_setup_without_overwriting_adjacent_text() {
        let original = "function fish_prompt; printf 'ops> '; end";
        let directory = insert(original, Setup::FishDirectory).unwrap();
        assert_eq!(
            directory,
            format!("{original}\n{}", block(Setup::FishDirectory))
        );
        assert_eq!(insert(&directory, Setup::FishDirectory).unwrap(), directory);

        let tail = "\n# user notes\n";
        let existing = format!("{directory}{tail}");
        let full = insert(&existing, Setup::FishIntegration).unwrap();
        assert_eq!(
            full,
            format!("{original}\n{}{tail}", block(Setup::FishIntegration))
        );
        assert_eq!(insert(&full, Setup::FishDirectory).unwrap(), existing);
    }

    #[test]
    fn refuses_incomplete_edited_duplicate_and_pasted_setup() {
        let marked = block(Setup::BashIntegration);
        let edited = marked.replace("PROMPT_COMMAND", "my_prompt_command");
        for invalid in [
            format!("{BEGIN}\n# incomplete"),
            edited,
            format!("{marked}\n{marked}"),
            Setup::BashDirectory.script().trim_end().to_owned(),
            block(Setup::ZshIntegration),
        ] {
            assert!(insert(&invalid, Setup::BashIntegration).is_err());
        }
    }

    #[test]
    fn applies_the_fragment_byte_limit_to_every_shell_setup() {
        let existing = "#".repeat(16 * 1024 - 1);
        for setup in Setup::ALL {
            assert!(insert(&existing, setup).is_err());
            let standalone = insert("", setup).unwrap();
            assert_eq!(standalone, block(setup));
        }
    }
}
