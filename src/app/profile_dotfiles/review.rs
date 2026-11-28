use super::*;
use crate::app::command_reference::dotfiles::comparison::{code_details, diff_text};
use anyhow::Context;

#[derive(Clone, Default)]
pub(super) enum Preview {
    #[default]
    Closed,
    Loading(Arc<()>),
    Ready(Arc<Report>),
    Failed,
}

impl Preview {
    pub(super) fn loading(&self) -> bool {
        matches!(self, Self::Loading(_))
    }

    pub(super) fn can_approve(&self) -> bool {
        matches!(self, Self::Closed | Self::Ready(_))
    }

    pub(super) fn digest(&self) -> Option<String> {
        match self {
            Self::Ready(report) => Some(report.digest.clone()),
            _ => None,
        }
    }

    pub(super) fn describe(&self, content: &mut Vec<KeyedView>) {
        match self {
            Self::Loading(_) => content.push(KeyedView::new(
                "preview-status",
                TextBlock::new().text("Reading dotfile fragments…"),
            )),
            Self::Ready(report) => report.describe(content),
            _ => {}
        }
    }
}

pub struct Report {
    digest: String,
    shell: DotfileShell,
    baseline: &'static str,
    changes: [Option<String>; 2],
}

impl Report {
    pub(super) fn read(
        path: &Path,
        shell: DotfileShell,
        snapshot: Option<&str>,
        approved: Option<&DotfileSync>,
    ) -> anyhow::Result<Self> {
        let proposed = DotfileBundle::load(path).context("Reading the proposed dotfiles bundle")?;
        if let Some(version) = snapshot {
            proposed.verify_version(shell, version)?;
        }
        let digest = proposed.content_version(shell)?;
        let fragments = proposed.normalized_fragments(shell)?;
        let (before, baseline) = match approved.filter(|settings| settings.shell == shell) {
            Some(settings) => match approved_fragments(settings) {
                Ok(before) if settings.bundle_sha256.is_some() => {
                    (Some(before), "Compared with the approved pinned version.")
                }
                Ok(before) => (
                    Some(before),
                    "Compared with the approved bundle file on disk.",
                ),
                Err(_) => (
                    None,
                    "The approved bundle could not be verified. Showing proposed fragments.",
                ),
            },
            None => (
                None,
                "No bundle is approved for this shell. Showing proposed fragments.",
            ),
        };
        let proposed_only = before.is_none();
        let [environment, prompt] = before.unwrap_or_default();
        let changes = [
            diff_text(&environment, &fragments[0], proposed_only),
            diff_text(&prompt, &fragments[1], proposed_only),
        ];
        Ok(Self {
            digest,
            shell,
            baseline,
            changes,
        })
    }

    fn describe(&self, content: &mut Vec<KeyedView>) {
        content.push(KeyedView::new(
            "preview-digest",
            TextBlock::new()
                .text(format!(
                    "Proposed {} version {}",
                    self.shell.label(),
                    self.digest
                ))
                .text_wrapping(TextWrapping::Wrap),
        ));
        content.push(KeyedView::new(
            "preview-baseline",
            TextBlock::new()
                .text(self.baseline)
                .text_wrapping(TextWrapping::Wrap),
        ));
        if self.changes.iter().all(Option::is_none) {
            content.push(KeyedView::new(
                "preview-unchanged",
                TextBlock::new().text("No dotfile fragment lines changed."),
            ));
        }
        for ((key, label), change) in [
            ("preview-environment", "Environment fragment preview"),
            ("preview-prompt", "Prompt fragment preview"),
        ]
        .into_iter()
        .zip(&self.changes)
        {
            if let Some(lines) = change {
                content.push(code_details(key.into(), label, lines));
            }
        }
    }
}

fn approved_fragments(settings: &DotfileSync) -> anyhow::Result<[String; 2]> {
    let approved = DotfileBundle::load(&settings.bundle)?;
    if let Some(pin) = settings.bundle_sha256.as_deref() {
        approved.verify_version(settings.shell, pin)?;
    }
    approved.normalized_fragments(settings.shell)
}
