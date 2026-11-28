use crate::sftp::DotfileShell;

mod fragments;

#[derive(Clone, Copy)]
pub enum Mode {
    Directory,
    Full,
}

#[derive(Clone, Copy)]
pub enum Setup {
    BashDirectory,
    ZshDirectory,
    FishDirectory,
    BashIntegration,
    ZshIntegration,
    FishIntegration,
}

impl Setup {
    pub const INTEGRATIONS: [Self; 3] = [
        Self::BashIntegration,
        Self::ZshIntegration,
        Self::FishIntegration,
    ];
    pub const ALL: [Self; 6] = [
        Self::BashDirectory,
        Self::ZshDirectory,
        Self::FishDirectory,
        Self::BashIntegration,
        Self::ZshIntegration,
        Self::FishIntegration,
    ];

    pub fn for_shell(shell: DotfileShell, mode: Mode) -> Self {
        match (shell, mode) {
            (DotfileShell::Bash, Mode::Directory) => Self::BashDirectory,
            (DotfileShell::Zsh, Mode::Directory) => Self::ZshDirectory,
            (DotfileShell::Fish, Mode::Directory) => Self::FishDirectory,
            (DotfileShell::Bash, Mode::Full) => Self::BashIntegration,
            (DotfileShell::Zsh, Mode::Full) => Self::ZshIntegration,
            (DotfileShell::Fish, Mode::Full) => Self::FishIntegration,
        }
    }

    pub fn shell(self) -> DotfileShell {
        match self {
            Self::BashDirectory | Self::BashIntegration => DotfileShell::Bash,
            Self::ZshDirectory | Self::ZshIntegration => DotfileShell::Zsh,
            Self::FishDirectory | Self::FishIntegration => DotfileShell::Fish,
        }
    }

    pub fn bundle_label(self) -> &'static str {
        match self {
            Self::BashDirectory => "Bash directory reporting only",
            Self::ZshDirectory => "Zsh directory reporting only",
            Self::FishDirectory => "Fish directory reporting only",
            Self::BashIntegration => "Bash directory and prompt integration",
            Self::ZshIntegration => "Zsh directory and prompt integration",
            Self::FishIntegration => "Fish directory and prompt integration",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::BashDirectory => "Copy Bash directory setup",
            Self::ZshDirectory => "Copy Zsh directory setup",
            Self::FishDirectory => "Copy Fish directory setup",
            Self::BashIntegration => "Copy Bash shell integration",
            Self::ZshIntegration => "Copy Zsh shell integration",
            Self::FishIntegration => "Copy Fish shell integration",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shell| shell.label() == label)
    }

    pub fn script(self) -> &'static str {
        match self {
            Self::BashDirectory => include_str!("app/bash-directory.sh"),
            Self::ZshDirectory => include_str!("app/zsh-directory.zsh"),
            Self::FishDirectory => include_str!("app/fish-directory.fish"),
            Self::BashIntegration => concat!(
                include_str!("app/bash-directory.sh"),
                "\n",
                include_str!("app/bash-integration.sh")
            ),
            Self::ZshIntegration => concat!(
                include_str!("app/zsh-directory.zsh"),
                "\n",
                include_str!("app/zsh-integration.zsh")
            ),
            Self::FishIntegration => concat!(
                include_str!("app/fish-directory.fish"),
                "\n",
                include_str!("app/fish-integration.fish")
            ),
        }
    }

    pub fn insert(self, existing: &str) -> anyhow::Result<String> {
        fragments::insert(existing, self)
    }
}
