use super::catalog::MAX_ARCHIVE;

pub(super) fn latest(cancelled: impl Fn() -> bool) -> anyhow::Result<Vec<u8>> {
    w11term::https::get(
        "https://github.com/tldr-pages/tldr/releases/latest/download/tldr-pages.en.zip",
        MAX_ARCHIVE,
        cancelled,
    )
}
