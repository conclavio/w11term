use wezterm_term::{ProgramBlockKind, ProgramState, ProgramStatus, Progress};
use windows::Win32::{
    Foundation::HWND,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    UI::Shell::{
        ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED,
        TBPFLAG, TaskbarList,
    },
};

// Use Explorer's own grouping order for splits within a tab and tabs within a window.
pub(super) fn most_important(left: Progress, right: Progress) -> Progress {
    fn priority(progress: Progress) -> (u8, u8) {
        match progress {
            Progress::Error(value) => (0, value),
            Progress::Paused(value) => (1, value),
            Progress::Percentage(value) => (2, value),
            Progress::Indeterminate => (3, 0),
            Progress::None => (4, 0),
        }
    }
    if priority(right) < priority(left) {
        right
    } else {
        left
    }
}

pub(super) fn tab_header(label: &str, progress: Progress) -> String {
    match progress {
        Progress::None => label.into(),
        Progress::Percentage(value) => format!("{value}% · {label}"),
        Progress::Error(value) => format!("Error {value}% · {label}"),
        Progress::Paused(value) => format!("Paused {value}% · {label}"),
        Progress::Indeterminate => format!("In progress · {label}"),
    }
}

pub(super) fn program_progress(status: &ProgramStatus) -> Progress {
    match status.state {
        ProgramState::Idle | ProgramState::Done => Progress::None,
        ProgramState::Working => status
            .progress
            .map_or(Progress::Indeterminate, Progress::Percentage),
        ProgramState::Blocked => Progress::Paused(status.progress.unwrap_or(100)),
        ProgramState::Error => Progress::Error(100),
    }
}

pub(super) fn prefer_program(status: &ProgramStatus, legacy: Progress) -> bool {
    let progress = program_progress(status);
    legacy == Progress::None || most_important(progress, legacy) == progress
}

pub(super) fn program_tab_header(label: &str, status: &ProgramStatus) -> String {
    let state = match status.state {
        ProgramState::Idle => return label.into(),
        ProgramState::Working => return tab_header(label, program_progress(status)),
        ProgramState::Done => "Done",
        ProgramState::Error => "Error",
        ProgramState::Blocked => match status.kind {
            Some(ProgramBlockKind::Permission) => "Needs approval",
            Some(ProgramBlockKind::Question) => "Needs an answer",
            Some(ProgramBlockKind::Auth) => "Needs sign-in",
            None => "Waiting for input",
        },
    };
    if let Some(value) = status.progress {
        format!("{state} {value}% · {label}")
    } else {
        format!("{state} · {label}")
    }
}

pub(super) fn program_tooltip(label: &str, pane: u64, status: &ProgramStatus) -> String {
    let mut lines = vec![format!("{label} (pane {pane})")];
    for field in [
        status.title.as_deref().or(status.app.as_deref()),
        status.message.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let mut printable = field
            .chars()
            .map(|c| {
                if matches!(c, '\u{2028}' | '\u{2029}') {
                    ' '
                } else {
                    c
                }
            })
            .filter(|&c| {
                !matches!(c, '\u{00ad}' | '\u{061c}' | '\u{200b}' | '\u{200e}' | '\u{200f}' |
                '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
            });
        let mut text: String = printable.by_ref().take(300).collect();
        if printable.next().is_some() {
            text.push('…');
        }
        if !text.trim().is_empty() {
            lines.push(text);
        }
    }
    lines.join("\n")
}

fn button(progress: Progress) -> (TBPFLAG, Option<u8>) {
    match progress {
        Progress::None => (TBPF_NOPROGRESS, None),
        Progress::Indeterminate => (TBPF_INDETERMINATE, None),
        Progress::Percentage(value) => (TBPF_NORMAL, Some(value)),
        Progress::Error(value) => (TBPF_ERROR, Some(value)),
        Progress::Paused(value) => (TBPF_PAUSED, Some(value)),
    }
}

#[derive(Default)]
pub(super) struct Taskbar {
    list: Option<ITaskbarList3>,
    last: Progress,
}

impl Taskbar {
    pub(super) fn show(&mut self, hwnd: HWND, progress: Progress) {
        if progress == self.last {
            return;
        }
        self.last = progress;
        if let Err(error) = self.update_button(hwnd, progress) {
            log::debug!("Windows taskbar progress: {error}");
            self.list = None;
        }
    }

    pub(super) fn shell_restarted(&mut self) {
        *self = Self::default();
    }

    fn update_button(&mut self, hwnd: HWND, progress: Progress) -> windows::core::Result<()> {
        // Reactor owns the UI thread's COM apartment and this window's HWND.
        unsafe {
            if self.list.is_none() {
                let list: ITaskbarList3 =
                    CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
                list.HrInit()?;
                self.list = Some(list);
            }
            let list = self.list.as_ref().unwrap();
            let (state, value) = button(progress);
            list.SetProgressState(hwnd, state)?;
            if let Some(value) = value {
                list.SetProgressValue(hwnd, value.into(), 100)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Progress::*;

    #[test]
    fn program_header_and_tooltip_attribute_a_plain_text_report_to_its_pane() {
        let status = ProgramStatus {
            state: ProgramState::Blocked,
            kind: Some(ProgramBlockKind::Permission),
            progress: Some(31),
            app: Some("cargo".into()),
            title: Some("Build\u{202e} tests".into()),
            message: Some("Proceed?\u{2028}Confirm in the terminal.".into()),
        };
        assert_eq!(
            program_tab_header("SSH", &status),
            "Needs approval 31% · SSH"
        );
        assert_eq!(program_progress(&status), Paused(31));
        assert!(prefer_program(&status, Percentage(20)));
        assert!(!prefer_program(&status, Error(23)));
        assert_eq!(
            program_tooltip("Needs approval 31% · SSH", 7, &status),
            "Needs approval 31% · SSH (pane 7)\nBuild tests\nProceed? Confirm in the terminal.",
        );
    }

    #[test]
    fn explorer_priority_is_independent_of_pane_order() {
        let states = [
            None,
            Indeterminate,
            Percentage(90),
            Percentage(12),
            Paused(80),
            Error(70),
            Error(42),
        ];
        for (index, &higher) in states.iter().enumerate() {
            for &lower in &states[..=index] {
                assert_eq!(most_important(higher, lower), higher);
                assert_eq!(most_important(lower, higher), higher);
            }
        }
    }

    #[test]
    fn tab_and_windows_taskbar_distinguish_every_state() {
        for (state, title, flag, value) in [
            (None, "Build", TBPF_NOPROGRESS, Option::None),
            (
                Indeterminate,
                "In progress · Build",
                TBPF_INDETERMINATE,
                Option::None,
            ),
            (Percentage(0), "0% · Build", TBPF_NORMAL, Some(0)),
            (Paused(12), "Paused 12% · Build", TBPF_PAUSED, Some(12)),
            (Error(71), "Error 71% · Build", TBPF_ERROR, Some(71)),
        ] {
            assert_eq!(tab_header("Build", state), title);
            assert_eq!(button(state), (flag, value));
        }
    }
}
