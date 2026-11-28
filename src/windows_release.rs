//! Identification and discovery of the published Windows package.
use crate::https::{self, Request};
use crate::sftp::{apply_mark_of_the_web, digest_hex, normalize_sha256};
use anyhow::{Context, Result, bail, ensure};
use chrono::DateTime;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path, time::Duration};
use url::Url;

const REPOSITORY: &str = "https://github.com/conclavio/w11term";
pub const LATEST_PAGE: &str = "https://github.com/conclavio/w11term/releases/latest";
const API: &str = "https://api.github.com/repos/conclavio/w11term/releases/latest";
const ARCHIVE: &str = "w11term-windows-x64.zip";
const CHECKSUM: &str = "w11term-windows-x64.zip.sha256";
const MAX_METADATA_BYTES: usize = 128 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: usize = 1024;
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

#[derive(Clone, Debug)]
pub struct Latest {
    pub commit: String,
    tag: String,
    pub published: String,
    pub archive_bytes: u64,
    archive_digest: Option<String>,
}

impl Latest {
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn page(&self) -> String {
        format!("{REPOSITORY}/releases/tag/{}", self.tag)
    }

    pub fn suggested_zip_name(&self) -> String {
        format!("w11term-windows-x64-{}.zip", self.tag)
    }

    fn asset(&self, name: &str) -> Url {
        endpoint(&format!(
            "{REPOSITORY}/releases/download/{}/{name}",
            self.tag
        ))
    }

    pub fn save_zip(
        &self,
        path: &Path,
        cancelled: impl Fn() -> bool,
        mut progress: impl FnMut(u64),
    ) -> Result<()> {
        ensure!(
            path.is_absolute()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("zip")),
            "Choose a Windows ZIP file path"
        );
        ensure!(
            !path.try_exists().context("Checking the ZIP destination")?,
            "Choose a new ZIP filename. The selected destination already exists."
        );
        ensure!(
            self.archive_bytes != 0 && self.archive_bytes <= MAX_ARCHIVE_BYTES,
            "The published Windows ZIP exceeds the 256 MiB download limit"
        );
        let (status, bytes) = https::request(
            &self.asset(CHECKSUM),
            Request::Download,
            MAX_CHECKSUM_BYTES,
            &cancelled,
        )
        .context("Reading the published Windows ZIP checksum")?;
        ensure!(
            status == 200,
            "The Windows ZIP checksum returned HTTP {status}"
        );
        let expected = parse_checksum(&bytes)?;
        ensure!(
            self.archive_digest
                .as_ref()
                .is_none_or(|digest| digest == &expected),
            "The published Windows ZIP checksum differs from the GitHub asset digest"
        );

        let parent = path
            .parent()
            .context("Choose a folder for the Windows ZIP")?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)
            .context("Creating the temporary download in the selected folder")?;
        let mut hash = Sha256::new();
        let mut received = 0;
        let length = https::download_into(
            &self.asset(ARCHIVE),
            usize::try_from(self.archive_bytes)?,
            Duration::from_secs(600),
            &cancelled,
            |chunk| {
                staged
                    .write_all(chunk)
                    .context("Writing the staged Windows ZIP")?;
                hash.update(chunk);
                received += chunk.len() as u64;
                progress(received);
                Ok(())
            },
        )
        .context("Downloading the Windows ZIP")?;
        ensure!(
            length as u64 == self.archive_bytes,
            "The Windows ZIP length differs from the published size"
        );
        ensure!(
            digest_hex(hash) == expected,
            "The Windows ZIP SHA-256 does not match the published checksum"
        );
        staged
            .as_file()
            .sync_all()
            .context("Flushing the verified Windows ZIP")?;
        apply_mark_of_the_web(staged.path())?;
        ensure!(!cancelled(), "Windows ZIP download cancelled");
        staged
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .with_context(|| format!("Saving the verified ZIP to {}", path.display()))?;
        Ok(())
    }
}

fn parse_checksum(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("Reading the published Windows ZIP checksum")?;
    let mut lines = text.lines();
    let (digest, filename) = lines
        .next()
        .and_then(|line| line.split_once(' '))
        .context("The published Windows ZIP checksum has no filename")?;
    ensure!(
        filename.strip_prefix([' ', '*']) == Some(ARCHIVE) && lines.next().is_none(),
        "The published checksum must name only {ARCHIVE}"
    );
    normalize_sha256(digest).context("Checking the published Windows ZIP digest")
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
    digest: Option<String>,
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
    let archive = asset(ARCHIVE)?;
    let archive_digest = archive
        .digest
        .as_deref()
        .map(|digest| {
            let digest = digest
                .strip_prefix("sha256:")
                .context("Expected a SHA-256 GitHub asset digest")?;
            normalize_sha256(digest)
        })
        .transpose()?;
    asset(CHECKSUM)?;
    let published = DateTime::parse_from_rfc3339(&release.published_at)
        .context("Reading the GitHub release date")?
        .format("%Y-%m-%d")
        .to_string();
    Ok(Latest {
        commit,
        tag: release.tag_name,
        published,
        archive_bytes: archive.size,
        archive_digest,
    })
}

pub fn check(cancelled: impl Fn() -> bool) -> Result<Latest> {
    let headers: Vec<_> =
        "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2026-03-10\r\n"
            .encode_utf16()
            .collect();
    let (status, bytes) = https::request(
        &endpoint(API),
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

fn endpoint(address: &str) -> Url {
    let url = https::parse(address).expect("fixed GitHub release endpoint");
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
        let mut wrong_digest = release();
        wrong_digest["assets"][0]["digest"] = json!("sha1:deadbeef");
        for invalid in [
            draft,
            prerelease,
            mismatched,
            foreign,
            empty,
            incomplete,
            wrong_digest,
        ] {
            assert!(parse_release(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }

    #[test]
    fn checksum_names_the_archive_and_contains_one_digest() {
        let expected = "a".repeat(64);
        assert_eq!(
            parse_checksum(format!("{expected}  {ARCHIVE}\n").as_bytes()).unwrap(),
            expected
        );
        assert_eq!(
            parse_checksum(format!("{} *{ARCHIVE}\r\n", expected.to_uppercase()).as_bytes())
                .unwrap(),
            expected
        );
        for invalid in [
            format!("{expected}\n"),
            format!("{expected}  other.zip\n"),
            format!("{expected}  {ARCHIVE}\n{expected}  {ARCHIVE}\n"),
            format!("abc  {ARCHIVE}\n"),
        ] {
            assert!(parse_checksum(invalid.as_bytes()).is_err());
        }
    }
}
