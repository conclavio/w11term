use windows_reactor::MenuItem;

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
    const ALL: [Self; 6] = [
        Self::BashDirectory,
        Self::ZshDirectory,
        Self::FishDirectory,
        Self::BashIntegration,
        Self::ZshIntegration,
        Self::FishIntegration,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::BashDirectory => "Copy Bash directory setup",
            Self::ZshDirectory => "Copy Zsh directory setup",
            Self::FishDirectory => "Copy Fish directory setup",
            Self::BashIntegration => "Copy Bash shell integration",
            Self::ZshIntegration => "Copy Zsh shell integration",
            Self::FishIntegration => "Copy Fish shell integration",
        }
    }

    pub(super) fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shell| shell.label() == label)
    }

    pub(super) fn menu_items() -> impl Iterator<Item = MenuItem> {
        Self::ALL.into_iter().map(|shell| {
            let key = match shell {
                Self::BashDirectory => "copy-bash-directory",
                Self::ZshDirectory => "copy-zsh-directory",
                Self::FishDirectory => "copy-fish-directory",
                Self::BashIntegration => "copy-bash-integration",
                Self::ZshIntegration => "copy-zsh-integration",
                Self::FishIntegration => "copy-fish-integration",
            };
            MenuItem::item(key, shell.label())
        })
    }

    pub(super) fn copy(self) -> anyhow::Result<()> {
        let setup = match self {
            Self::BashDirectory => include_str!("bash-directory.sh"),
            Self::ZshDirectory => include_str!("zsh-directory.zsh"),
            Self::FishDirectory => include_str!("fish-directory.fish"),
            Self::BashIntegration => concat!(
                include_str!("bash-directory.sh"),
                "\n",
                include_str!("bash-integration.sh")
            ),
            Self::ZshIntegration => concat!(
                include_str!("zsh-directory.zsh"),
                "\n",
                include_str!("zsh-integration.zsh")
            ),
            Self::FishIntegration => concat!(
                include_str!("fish-directory.fish"),
                "\n",
                include_str!("fish-integration.fish")
            ),
        };
        w11term::clipboard::write(setup.trim_end())
    }
}
