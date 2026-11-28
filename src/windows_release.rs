//! Identification and discovery of the published Windows package.
use crate::https::{self, Request};
use anyhow::{Context, Result, bail, ensure};
use chrono::DateTime;
use serde::Deserialize;
use url::Url;

const REPOSITORY: &str = "https://github.com/conclavio/w11term";
pub const LATEST_PAGE: &str = "https://github.com/conclavio/w11term/releases/latest";
const API: &str = "https://api.github.com/repos/conclavio/w11term/releases/latest";
const ARCHIVE: &str = "w11term-windows-x64.zip";
const CHECKSUM: &str = "w11term-windows-x64.zip.sha256";
const MAX_METADATA_BYTES: usize = 128 * 1024;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn build_commit() -> Option<&'static str> {
    option_env!("W11TERM_BUILD_COMMIT").filter(|commit| commit_id(commit))
}

fn commit_id(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug)]
pub struct Latest {
    pub commit: String,
    pub tag: String,
    pub published: String,
    pub archive_bytes: u64,
}

impl Latest {
    pub fn page(&self) -> String {
        format!("{REPOSITORY}/releases/tag/{}", self.tag)
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    target_commitish: String,
    published_at: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    size: u64,
}

fn parse_release(json: &[u8]) -> Result<Latest> {
    let release: Release =
        serde_json::from_slice(json).context("Reading GitHub release metadata")?;
    ensure!(
        !release.draft && !release.prerelease,
        "Wait for a published Windows release on GitHub"
    );
    let commit = release.target_commitish;
    ensure!(
        commit_id(&commit) && release.tag_name == format!("build-{}", &commit[..12]),
        "The latest GitHub release has no verified Windows build tag"
    );
    let asset = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name && asset.state == "uploaded" && asset.size > 0)
            .with_context(|| format!("The latest GitHub release is missing {name}"))
    };
    let archive_bytes = asset(ARCHIVE)?.size;
    asset(CHECKSUM)?;
    let published = DateTime::parse_from_rfc3339(&release.published_at)
        .context("Reading the GitHub release date")?
        .format("%Y-%m-%d")
        .to_string();
    Ok(Latest {
        commit,
        tag: release.tag_name,
        published,
        archive_bytes,
    })
}

pub fn check(cancelled: impl Fn() -> bool) -> Result<Latest> {
    let headers: Vec<_> =
        "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2026-03-10\r\n"
            .encode_utf16()
            .collect();
    let (status, bytes) = https::request(
        &endpoint(),
        Request::Json {
            headers: &headers,
            body: None,
        },
        MAX_METADATA_BYTES,
        cancelled,
    )?;
    match status {
        200 => parse_release(&bytes),
        404 => bail!("GitHub has no published Windows release"),
        403 | 429 => bail!("GitHub refused or rate-limited the release check (HTTP {status})"),
        _ => bail!("GitHub returned HTTP {status} during the release check"),
    }
}

fn endpoint() -> Url {
    let url = https::parse(API).expect("fixed GitHub release endpoint");
    #[cfg(feature = "ui-smoke")]
    if std::env::var_os("W11TERM_DATA_DIR").is_some()
        && let Ok(port) = std::env::var("W11TERM_RELEASE_FIXTURE_PORT")
        && let Ok(port) = port.parse::<u16>()
        && port != 0
    {
        return Url::parse(&format!("http://127.0.0.1:{port}{}", url.path()))
            .expect("fixed GitHub fixture endpoint");
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release() -> serde_json::Value {
        json!({
            "tag_name": "build-09ad0eb3a2c5",
            "target_commitish": "09ad0eb3a2c55c52e84667fdf5add693b8d19fab",
            "published_at": "2026-11-27T23:41:58Z",
            "draft": false,
            "prerelease": false,
            "html_url": "https://example.test/release",
            "assets": [
                {"name": ARCHIVE, "state": "uploaded", "size": 45038313},
                {"name": CHECKSUM, "state": "uploaded", "size": 90},
            ]
        })
    }

    #[test]
    fn discovers_the_uploaded_package_under_the_fixed_repository() {
        let latest = parse_release(&serde_json::to_vec(&release()).unwrap()).unwrap();
        assert_eq!(latest.archive_bytes, 45_038_313);
        assert_eq!(latest.published, "2026-11-27");
        assert_eq!(
            latest.page(),
            "https://github.com/conclavio/w11term/releases/tag/build-09ad0eb3a2c5"
        );
    }

    #[test]
    fn requires_matching_build_metadata_and_both_uploaded_assets() {
        let mut draft = release();
        draft["draft"] = json!(true);
        let mut prerelease = release();
        prerelease["prerelease"] = json!(true);
        let mut mismatched = release();
        mismatched["tag_name"] = json!("build-0123456789ab");
        let mut foreign = release();
        foreign["target_commitish"] = json!("main");
        let mut empty = release();
        empty["assets"][0]["size"] = json!(0);
        let mut incomplete = release();
        incomplete["assets"][1]["state"] = json!("starter");
        for invalid in [draft, prerelease, mismatched, foreign, empty, incomplete] {
            assert!(parse_release(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
}
