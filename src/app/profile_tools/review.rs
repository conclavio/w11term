use super::*;
use crate::app::command_reference::dotfiles::comparison::code_details;
use std::collections::BTreeSet;
use w11term::sftp::{UtilityBuild, UtilityDownload};

pub(super) fn build_label(builds: &[UtilityBuild], index: usize) -> String {
    let architecture = builds[index].architecture;
    let platform = architecture_label(architecture);
    if builds
        .iter()
        .filter(|build| build.architecture == architecture)
        .nth(1)
        .is_some()
    {
        format!("{platform} build {}", index + 1)
    } else {
        platform.into()
    }
}

pub(super) fn build_source(build: &UtilityBuild) -> String {
    build.download.as_ref().map_or_else(
        || build.path.display().to_string(),
        |download| download.url().to_owned(),
    )
}

pub(super) fn tool_view(tool: &UtilityTool) -> KeyedView {
    let platforms = tool
        .builds
        .iter()
        .map(|build| architecture_label(build.architecture))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let builds = tool.builds.iter().enumerate().map(|(index, build)| {
        let mut detail = format!(
            "{}\n{}\nExecutable SHA-256 {}",
            build_label(&tool.builds, index),
            build_source(build),
            build.sha256.trim()
        );
        if let Some(
            UtilityDownload::TarGz {
                member,
                archive_sha256,
                ..
            }
            | UtilityDownload::Zip {
                member,
                archive_sha256,
                ..
            },
        ) = &build.download
        {
            detail.push_str(&format!(
                "\nArchive member {member}\nArchive SHA-256 {}",
                archive_sha256.trim()
            ));
        }
        detail
    });
    code_details(
        format!("review-tool-{}", tool.name),
        &format!("{} · {platforms}", tool.name),
        &builds.collect::<Vec<_>>().join("\n\n"),
    )
}
