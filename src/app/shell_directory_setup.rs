use windows_reactor::MenuItem;

#[derive(Clone, Copy)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    const ALL: [Self; 3] = [Self::Bash, Self::Zsh, Self::Fish];

    fn label(self) -> &'static str {
        match self {
            Self::Bash => "Copy Bash directory setup",
            Self::Zsh => "Copy Zsh directory setup",
            Self::Fish => "Copy Fish directory setup",
        }
    }

    pub(super) fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shell| shell.label() == label)
    }

    pub(super) fn menu_items() -> impl Iterator<Item = MenuItem> {
        Self::ALL.into_iter().map(|shell| {
            let key = match shell {
                Self::Bash => "copy-bash-directory",
                Self::Zsh => "copy-zsh-directory",
                Self::Fish => "copy-fish-directory",
            };
            MenuItem::item(key, shell.label())
        })
    }

    pub(super) fn copy(self) -> anyhow::Result<()> {
        let setup = match self {
            Self::Bash => include_str!("bash-directory.sh"),
            Self::Zsh => include_str!("zsh-directory.zsh"),
            Self::Fish => include_str!("fish-directory.fish"),
        };
        w11term::clipboard::write(setup.trim_end())
    }
}
