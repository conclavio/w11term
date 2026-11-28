pub use w11term::shell_setup::Setup;
use windows_reactor::MenuItem;

pub(super) fn copy(setup: Setup) -> anyhow::Result<()> {
    w11term::clipboard::write(setup.script().trim_end())
}

pub(super) fn menu_items() -> impl Iterator<Item = MenuItem> {
    Setup::ALL.into_iter().map(|setup| {
        let key = match setup {
            Setup::BashDirectory => "copy-bash-directory",
            Setup::ZshDirectory => "copy-zsh-directory",
            Setup::FishDirectory => "copy-fish-directory",
            Setup::BashIntegration => "copy-bash-integration",
            Setup::ZshIntegration => "copy-zsh-integration",
            Setup::FishIntegration => "copy-fish-integration",
        };
        MenuItem::item(key, setup.label())
    })
}
