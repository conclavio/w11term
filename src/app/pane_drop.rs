use super::*;
use anyhow::ensure;
use std::{
    io::{Read, Seek, SeekFrom},
    os::windows::process::CommandExt,
    path::{Component, Path, PathBuf, Prefix},
    process::{Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    id: TabId,
    origin: clipboard_paste::Origin,
}

impl Target {
    pub(super) fn data(self, data: DroppedData) -> Message {
        match data {
            DroppedData::StorageItems(items) => Message::PaneDrop(Action::Drop(
                self,
                items.into_iter().map(|item| item.path).collect(),
            )),
            _ => Message::ControlIdle,
        }
    }
}

pub enum Action {
    Drop(Target, Vec<String>),
    Ready(Target, u64, Result<String, String>),
    #[cfg(feature = "ui-smoke")]
    Fixture(Target, PathBuf),
}

#[derive(Default)]
pub(super) struct State {
    serial: u64,
    task: Option<ComponentTask>,
}

impl State {
    pub(super) fn cancel(&mut self) {
        self.serial += 1;
        if let Some(task) = self.task.take() {
            task.cancel();
        }
    }
}

impl TerminalApp {
    pub(super) fn pane_drop_target(&self, id: TabId, tab: &LiveTab) -> Option<Target> {
        let Source::Local(shell) = &tab.source else {
            return None;
        };
        if shell.wsl_distribution().is_none()
            && !LocalOptions::supports_powershell_integration(&shell.executable, &shell.args)
        {
            return None;
        }
        let origin = clipboard_paste::Origin {
            pane: tab.session.pane_id,
            group: self.tabs.group_id(id)?,
            connection: tab.session.frames.load().connection_generation,
        };
        self.pane_paste_target(id, origin)?;
        let frame = tab.session.frames.load();
        (!frame.alternate
            && !frame.mouse_grabbed
            && tab.session.events.shell_generation() == Some(origin.connection)
            && tab.error.is_none()
            && !tab.find.open
            && !self.clipboard.is_open()
            && self.copy_mode.borrow().is_none()
            && self.hints.borrow().is_none())
        .then_some(Target { id, origin })
    }

    fn current_path_drop(&self, target: Target) -> Option<&LocalOptions> {
        let tab = self.pane_paste_target(target.id, target.origin)?;
        if self.pane_drop_target(target.id, tab)? != target {
            return None;
        }
        match &tab.source {
            Source::Local(shell) => Some(shell),
            _ => None,
        }
    }

    pub(super) fn pane_drop_action(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Drop(target, paths) => {
                let Some(shell) = self.current_path_drop(target) else {
                    return;
                };
                if paths.is_empty() {
                    return;
                }
                let wsl = shell
                    .wsl_distribution()
                    .map(|distribution| (shell.executable.clone(), distribution.to_owned()));
                self.focus_pane(target.id, context);
                self.pane_drop.cancel();
                let serial = self.pane_drop.serial;
                if let Some((executable, distribution)) = wsl {
                    self.pane_drop.task = Some(context.spawn_background_with_rejection(
                        move |cancel| {
                            let deadline = Instant::now() + Duration::from_secs(8);
                            let result = format_paths(paths, |path| {
                                let mounted = wsl_path(path, &distribution, |drive_path| {
                                    translate_drive_path(
                                        &executable,
                                        &distribution,
                                        drive_path,
                                        deadline,
                                        || cancel.is_cancelled(),
                                    )
                                })?;
                                Ok(quote_wsl(&mounted))
                            });
                            Message::PaneDrop(Action::Ready(
                                target,
                                serial,
                                result.map_err(|e| format!("{e:#}")),
                            ))
                        },
                        Message::PaneDrop(Action::Ready(
                            target,
                            serial,
                            Err("Couldn’t start WSL path translation.".into()),
                        )),
                    ));
                } else {
                    let result = format_paths(paths, |path| Ok(quote_powershell(path)));
                    self.finish_path_drop(target, result.map_err(|e| format!("{e:#}")), context);
                }
            }
            Action::Ready(target, serial, result) => {
                if self.pane_drop.serial == serial {
                    self.pane_drop.task = None;
                    self.finish_path_drop(target, result, context);
                }
            }
            #[cfg(feature = "ui-smoke")]
            Action::Fixture(target, path) => {
                let paths: anyhow::Result<Vec<String>> =
                    std::fs::read(path).map_err(Into::into).and_then(|bytes| {
                        serde_json::from_slice(&bytes)
                            .context("Reading the shell path drop fixture")
                    });
                match paths {
                    Ok(paths) => {
                        let data = DroppedData::StorageItems(
                            paths
                                .into_iter()
                                .map(|path| DroppedStorageItem {
                                    name: String::new(),
                                    path,
                                })
                                .collect(),
                        );
                        context.sender().send(target.data(data));
                    }
                    Err(error) => self.window_error = Some(format!("{error:#}")),
                }
            }
        }
    }

    fn finish_path_drop(
        &mut self,
        target: Target,
        result: Result<String, String>,
        context: &ComponentContext<Self>,
    ) {
        if self.paste_origin() != Some(target.origin) || self.current_path_drop(target).is_none() {
            return;
        }
        match result {
            Ok(text) => self.paste_saved_text(target.origin, text, context),
            Err(error) => self.window_error = Some(format!("Couldn’t insert file paths. {error}")),
        }
    }
}

#[cfg(feature = "ui-smoke")]
pub(super) fn with_fixture(
    content: View,
    target: Option<Target>,
    context: &ViewContext<TerminalApp>,
) -> View {
    let (Some(target), Some(path)) = (target, std::env::var_os("W11TERM_TEST_PANE_DROP")) else {
        return content;
    };
    let path = PathBuf::from(path);
    let label = format!("Drop paths on pane {}", target.origin.pane);
    Grid::new().children((
        content,
        Button::new()
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Top)
            .automation_name(label.clone())
            .on_click(
                context.callback(move |_| Message::PaneDrop(Action::Fixture(target, path.clone()))),
            )
            .content(TextBlock::new().text(label)),
    ))
}

fn format_paths(
    paths: Vec<String>,
    format: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let mut result = Vec::with_capacity(paths.len());
    for path in paths {
        validate_windows_path(&path)?;
        result.push(format(&path)?);
    }
    Ok(result.join(" "))
}

fn validate_windows_path(path: &str) -> anyhow::Result<()> {
    let parsed = Path::new(path);
    ensure!(
        parsed.is_absolute()
            && matches!(
                parsed.components().next(),
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(),
                        Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))
            )
            && safe_terminal_text(path),
        "Drop absolute file or folder paths without control characters"
    );
    Ok(())
}

fn safe_terminal_text(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn quote_powershell(path: &str) -> String {
    fn literal(text: &str) -> String {
        format!("'{}'", text.replace('\'', "''"))
    }

    if path.is_ascii() {
        return literal(path);
    }

    // Legacy Windows PowerShell can drop Unicode characters during terminal
    // input. Use ASCII source for the original UTF-16 filename in that case.
    use std::fmt::Write;
    let mut expression = String::from("(");
    let mut start = 0;
    for (index, character) in path.char_indices().filter(|(_, ch)| !ch.is_ascii()) {
        expression.push_str(&literal(&path[start..index]));
        for unit in character.encode_utf16(&mut [0; 2]) {
            write!(expression, "+[char]0x{unit:04x}").unwrap();
        }
        expression.push('+');
        start = index + character.len_utf8();
    }
    expression.push_str(&literal(&path[start..]));
    expression.push(')');
    expression
}

fn quote_wsl(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn wsl_path(
    path: &str,
    distribution: &str,
    translate: impl FnOnce(&str) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let mut components = Path::new(path).components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        anyhow::bail!("The Windows path is missing a drive or share")
    };
    match prefix.kind() {
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {
            translate(path.strip_prefix(r"\\?\").unwrap_or(path))
        }
        Prefix::UNC(host, share) | Prefix::VerbatimUNC(host, share) => {
            ensure!(
                (host.eq_ignore_ascii_case("wsl$") || host.eq_ignore_ascii_case("wsl.localhost"))
                    && share.eq_ignore_ascii_case(distribution),
                "Drop files from a Windows drive or the current WSL distribution"
            );
            let mut linux = String::new();
            for component in components {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        linux.push('/');
                        linux.push_str(
                            name.to_str()
                                .context("The WSL filename isn’t valid UTF-8")?,
                        );
                    }
                    _ => anyhow::bail!("The WSL path contains a relative component"),
                }
            }
            if linux.is_empty() {
                linux.push('/');
            }
            Ok(linux)
        }
        _ => anyhow::bail!("The Windows path is missing a drive or share"),
    }
}

const MAX_WSL_OUTPUT: u64 = 128 * 1024;

fn translate_drive_path(
    executable: &Path,
    distribution: &str,
    path: &str,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<String> {
    ensure!(
        !cancelled() && Instant::now() < deadline,
        "WSL path translation expired"
    );
    let mut output = tempfile::tempfile()?;
    // Redirect stdout to a file so waiting on an unusually long path cannot
    // block the child on a pipe while the worker is waiting for it to exit.
    let mut child = ProcessCommand::new(executable)
        .args([
            "--distribution",
            distribution,
            "--exec",
            "/usr/bin/wslpath",
            "-u",
            path,
        ])
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(output.try_clone()?))
        .spawn()
        .context("Starting wslpath")?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if !cancelled() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => break Err(anyhow::anyhow!("WSL path translation expired")),
            Err(error) => break Err(error.into()),
        }
    };
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    ensure!(
        status?.success(),
        "wslpath couldn’t translate this Windows drive path"
    );
    output.seek(SeekFrom::Start(0))?;
    let mut linux = String::new();
    output.take(MAX_WSL_OUTPUT + 1).read_to_string(&mut linux)?;
    parse_wsl_output(&linux)
}

fn parse_wsl_output(linux: &str) -> anyhow::Result<String> {
    ensure!(
        linux.len() as u64 <= MAX_WSL_OUTPUT,
        "wslpath returned too much data"
    );
    let linux = linux
        .strip_suffix("\r\n")
        .or_else(|| linux.strip_suffix('\n'))
        .unwrap_or(linux);
    ensure!(
        linux.starts_with('/') && safe_terminal_text(linux),
        "wslpath returned an unusable Linux path"
    );
    Ok(linux.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_literals_preserve_paths_as_separate_arguments() {
        let text = format_paths(
            vec![
                r"C:\Build & Test\O'Brien’s.txt".into(),
                r"\\server\share\$(name)".into(),
            ],
            |path| Ok(quote_powershell(path)),
        )
        .unwrap();
        assert_eq!(
            text,
            r"('C:\Build & Test\O''Brien'+[char]0x2019+'s.txt') '\\server\share\$(name)'"
        );
        assert_eq!(
            quote_powershell(r"C:\📁"),
            r"('C:\'+[char]0xd83d+[char]0xdcc1+'')"
        );
        for path in ["C:relative", r"\\.\PhysicalDrive0", "C:\\file\rBRANCH"] {
            assert!(format_paths(vec![path.into()], |path| Ok(quote_powershell(path))).is_err());
        }
    }

    #[test]
    fn wsl_paths_use_the_selected_mount_mapping_and_distribution() {
        let linux = wsl_path(r"\\?\C:\My Project\O'Brien", "Debian", |drive| {
            assert_eq!(drive, r"C:\My Project\O'Brien");
            Ok("/custom/mount/c/My Project/O'Brien".into())
        })
        .unwrap();
        assert_eq!(
            quote_wsl(&linux),
            r"'/custom/mount/c/My Project/O'\''Brien'"
        );
        let unc = wsl_path(
            r"\\wsl.localhost\debian\home\dev",
            "Debian",
            |_| unreachable!(),
        );
        assert_eq!(unc.unwrap(), "/home/dev");
        assert_eq!(
            wsl_path(r"\\wsl$\Debian\", "Debian", |_| unreachable!()).unwrap(),
            "/"
        );
        assert!(wsl_path(r"\\wsl$\Ubuntu\home", "Debian", |_| unreachable!()).is_err());
        assert!(wsl_path(r"\\server\share\home", "Debian", |_| unreachable!()).is_err());
        assert!(wsl_path(r"\\wsl$\Debian\..\home", "Debian", |_| unreachable!()).is_err());
    }

    #[test]
    fn wsl_helper_output_preserves_spaces_and_rejects_terminal_controls() {
        assert_eq!(
            parse_wsl_output("/mount/with trailing space \r\n").unwrap(),
            "/mount/with trailing space "
        );
        for output in [
            "relative\n",
            "/line\nline\n",
            "/x\u{1b}[200~\n",
            "/x\u{2028}\n",
        ] {
            assert!(parse_wsl_output(output).is_err());
        }
        assert!(parse_wsl_output(&format!("/{}", "a".repeat(MAX_WSL_OUTPUT as usize))).is_err());
    }
}
