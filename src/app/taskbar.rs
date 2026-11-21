use wezterm_term::Progress;
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
