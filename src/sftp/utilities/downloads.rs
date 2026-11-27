use super::*;
use std::io::{Cursor, Write};

const MAX_RESOURCE: usize = 64 * 1024 * 1024;
const MAX_ARCHIVE_PLAIN: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const STARTER_MANIFEST: &[u8] = include_bytes!("../../../examples/tool-bundles/essentials.json");

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "format", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UtilityDownload {
    Raw {
        url: String,
    },
    TarGz {
        url: String,
        member: String,
        archive_sha256: String,
    },
    Zip {
        url: String,
        member: String,
        archive_sha256: String,
    },
}

impl UtilityDownload {
    fn url(&self) -> &str {
        match self {
            Self::Raw { url } | Self::TarGz { url, .. } | Self::Zip { url, .. } => url,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        crate::https::parse(self.url())?;
        match self {
            Self::Raw { .. } => (),
            Self::TarGz {
                member,
                archive_sha256,
                ..
            }
            | Self::Zip {
                member,
                archive_sha256,
                ..
            } => {
                ensure!(
                    member.len() <= 1024 && member.split('/').all(safe_component),
                    "Use a relative archive member path with slash separators"
                );
                normalize_sha256(archive_sha256).context("Invalid archive SHA-256")?;
            }
        }
        Ok(())
    }

    fn executable(&self, response: Vec<u8>) -> Result<Vec<u8>> {
        ensure!(
            response.len() <= MAX_RESOURCE,
            "Tool release asset exceeds 64 MiB"
        );
        match self {
            Self::Raw { .. } => Ok(response),
            Self::TarGz {
                member,
                archive_sha256,
                ..
            } => {
                check_pin(&response, archive_sha256, "Release archive")?;
                tar_member(&response, member)
            }
            Self::Zip {
                member,
                archive_sha256,
                ..
            } => {
                check_pin(&response, archive_sha256, "Release archive")?;
                zip_member(&response, member)
            }
        }
    }
}

fn check_pin(bytes: &[u8], expected: &str, source: &str) -> Result<()> {
    ensure!(
        digest_hex(Sha256::new_with_prefix(bytes)) == normalize_sha256(expected)?,
        "{source} SHA-256 does not match its pin"
    );
    Ok(())
}

fn member_matches(path: &[u8], expected: &str) -> bool {
    path.strip_prefix(b"./").unwrap_or(path) == expected.as_bytes()
}

fn read_member(reader: impl Read, size: u64) -> Result<Vec<u8>> {
    ensure!(
        (1..=build::MAX_BINARY).contains(&size),
        "Executable archive member must be between 1 byte and 64 MiB"
    );
    let mut bytes = Vec::with_capacity(size as usize);
    reader.take(size + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == size,
        "Executable archive member size does not match its header"
    );
    Ok(bytes)
}

fn tar_member(archive: &[u8], member: &str) -> Result<Vec<u8>> {
    let decoder = flate2::read::MultiGzDecoder::new(archive).take(MAX_ARCHIVE_PLAIN + 1);
    let mut archive = tar::Archive::new(decoder);
    let mut found = None;
    for (i, entry) in archive.entries()?.enumerate() {
        ensure!(i < MAX_ENTRIES, "Tool archive contains too many entries");
        let entry = entry?;
        if member_matches(&entry.path_bytes(), member) {
            ensure!(
                found.is_none(),
                "Duplicate executable archive member {member}"
            );
            ensure!(
                entry.header().entry_type().is_file(),
                "Executable archive member must be a regular file"
            );
            let size = entry.size();
            found = Some(read_member(entry, size)?);
        }
    }
    let mut decoder = archive.into_inner();
    // Consume the gzip trailer and enforce the expanded limit, including padding.
    std::io::copy(&mut decoder, &mut std::io::sink())?;
    ensure!(decoder.limit() > 0, "Expanded tool archive exceeds 256 MiB");
    found.with_context(|| format!("Release archive is missing {member}"))
}

fn zip_member(archive: &[u8], member: &str) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(archive))?;
    ensure!(
        archive.len() <= MAX_ENTRIES,
        "Tool archive contains too many entries"
    );
    let mut found = None;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        if member_matches(entry.name_raw(), member) {
            ensure!(
                found.is_none(),
                "Duplicate executable archive member {member}"
            );
            ensure!(
                entry.is_file()
                    && !entry.is_symlink()
                    && entry
                        .unix_mode()
                        .is_none_or(|mode| matches!(mode & 0o170000, 0 | 0o100000)),
                "Executable archive member must be a regular file"
            );
            found = Some(index);
        }
    }
    let index = found.with_context(|| format!("Release archive is missing {member}"))?;
    let entry = archive.by_index(index)?;
    let size = entry.size();
    read_member(entry, size)
}

pub(super) fn cache_dir() -> Result<PathBuf> {
    Ok(crate::keys::data_dir()?
        .join("tools")
        .join("cache")
        .join("v1"))
}

/// Export the built-in Linux essentials manifest without overwriting a file.
pub fn export_starter_tool_bundle(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    write_starter_manifest(&path, false)?;
    Ok(path)
}

/// Select a version of the starter manifest and cache its builds on Windows.
pub fn prepare_starter_tool_bundle(cancelled: impl Fn() -> bool) -> Result<PathBuf> {
    ensure!(!cancelled(), "Tool download cancelled");
    let manifest = managed_starter_manifest(&crate::keys::data_dir()?)?;
    let bundle =
        serde_json::from_slice(STARTER_MANIFEST).context("Reading the built-in tool bundle")?;
    fetch_bundle_at(&bundle, None, &cache_dir()?, |url, limit| {
        crate::https::get(url, limit, &cancelled)
    })?;
    ensure!(!cancelled(), "Tool download cancelled");
    Ok(manifest)
}

fn managed_starter_manifest(data: &Path) -> Result<PathBuf> {
    // Keep each app version's pins available to profiles that already reference them.
    let digest = digest_hex(Sha256::new_with_prefix(STARTER_MANIFEST));
    let directory = data.join("tools").join("bundles").join(digest);
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("linux-essentials.json");
    write_starter_manifest(&path, true)?;
    Ok(path)
}

fn write_starter_manifest(path: &Path, reuse: bool) -> Result<()> {
    let parent = path
        .parent()
        .context("Choose a file for the starter tool bundle")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(STARTER_MANIFEST)?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if reuse && error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = Vec::new();
            std::fs::File::open(path)?
                .take(STARTER_MANIFEST.len() as u64 + 1)
                .read_to_end(&mut existing)?;
            ensure!(
                existing == STARTER_MANIFEST,
                "The saved Linux essentials manifest was modified. Browse for that manifest to use your copy, or move it aside and retry."
            );
            Ok(())
        }
        Err(error) => Err(error.error).context("Writing a new starter tool bundle"),
    }
}

/// Cache a bundle's pinned downloads locally. No SSH pane is required.
pub fn fetch_tool_bundle(
    bundle: &UtilityBundle,
    architecture: Option<UtilityArchitecture>,
) -> Result<Value> {
    fetch_bundle_at(bundle, architecture, &cache_dir()?, |url, limit| {
        crate::https::get(url, limit, || false)
    })
}

fn fetch_bundle_at(
    bundle: &UtilityBundle,
    architecture: Option<UtilityArchitecture>,
    cache: &Path,
    mut fetch: impl FnMut(&str, usize) -> Result<Vec<u8>>,
) -> Result<Value> {
    bundle.validate()?;
    let selected =
        |build: &UtilityBuild| architecture.is_none_or(|arch| build.architecture == arch);
    for tool in &bundle.tools {
        ensure!(
            tool.builds.iter().any(selected),
            "No requested architecture build for {}",
            tool.name
        );
    }
    let mut binary_remaining = MAX_LOCAL_BUILD_BYTES;
    let mut network_remaining = MAX_LOCAL_BUILD_BYTES as usize;
    let mut reports = Vec::new();
    for tool in &bundle.tools {
        for build in tool.builds.iter().filter(|build| selected(build)) {
            let report = if let Some(download) = &build.download {
                fetch_build(
                    build,
                    download,
                    cache,
                    &mut binary_remaining,
                    &mut network_remaining,
                    &mut fetch,
                )
            } else {
                Verified::read(build, &mut binary_remaining)
                    .map(|(checked, path)| (checked, path, "local"))
            }
            .with_context(|| format!("Preparing {} for {:?}", tool.name, build.architecture))?;
            let (checked, path, status) = report;
            reports.push(json!({
                "name": tool.name, "architecture": build.architecture, "path": path, "status": status,
                "sha256": checked.sha256, "bytes": checked.bytes, "interpreter": checked.interpreter,
            }));
        }
    }
    Ok(json!({"bundle": bundle.name, "builds": reports}))
}

fn fetch_build(
    build: &UtilityBuild,
    download: &UtilityDownload,
    cache: &Path,
    binary_remaining: &mut u64,
    network_remaining: &mut usize,
    fetch: &mut impl FnMut(&str, usize) -> Result<Vec<u8>>,
) -> Result<(Verified, PathBuf, &'static str)> {
    let sha256 = normalize_sha256(&build.sha256)?;
    let path = cache.join(&sha256);
    let mut cache_budget = build::MAX_BINARY;
    if let Ok(bytes) = build::read_pinned(&path, &sha256, &mut cache_budget) {
        charge(&bytes, binary_remaining)?;
        let checked = Verified::from_elf(build.architecture, &bytes, sha256)?;
        return Ok((checked, path, "cached"));
    }
    let limit = MAX_RESOURCE.min(*network_remaining);
    ensure!(limit > 0, "Tool bundle exceeds its download budget");
    let response = fetch(download.url(), limit)?;
    ensure!(
        response.len() <= limit,
        "Tool bundle exceeds its download budget"
    );
    *network_remaining -= response.len();
    let bytes = download.executable(response)?;
    charge(&bytes, binary_remaining)?;
    check_pin(&bytes, &sha256, "Executable")?;
    let checked = Verified::from_elf(build.architecture, &bytes, sha256.clone())?;
    std::fs::create_dir_all(cache)?;
    let mut staged = tempfile::NamedTempFile::new_in(cache)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    if let Err(error) = staged.persist(&path) {
        // Another instance may already be using a validated copy of this digest.
        let mut cache_budget = build::MAX_BINARY;
        if build::read_pinned(&path, &sha256, &mut cache_budget).is_ok() {
            return Ok((checked, path, "cached"));
        }
        return Err(error.error).context("Publishing the verified tool cache entry");
    }
    Ok((checked, path, "downloaded"))
}

fn charge(bytes: &[u8], remaining: &mut u64) -> Result<()> {
    *remaining = remaining
        .checked_sub(bytes.len() as u64)
        .context("Tool bundle exceeds its 256 MiB executable budget")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};

    #[test]
    fn managed_starter_reuses_only_the_exact_builtin_manifest() {
        let data = tempfile::tempdir().unwrap();
        let path = managed_starter_manifest(data.path()).unwrap();
        assert_eq!(
            path.parent().unwrap().file_name().unwrap(),
            digest(STARTER_MANIFEST).as_str()
        );
        assert_eq!(std::fs::read(&path).unwrap(), STARTER_MANIFEST);
        assert_eq!(UtilityBundle::load(&path).unwrap().name, "linux-essentials");
        assert_eq!(managed_starter_manifest(data.path()).unwrap(), path);

        std::fs::write(&path, "user edit").unwrap();
        let error = managed_starter_manifest(data.path()).unwrap_err();
        assert!(error.to_string().contains("manifest was modified"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "user edit");
    }

    fn digest(bytes: &[u8]) -> String {
        digest_hex(Sha256::new_with_prefix(bytes))
    }

    fn x86_executable() -> Vec<u8> {
        // A minimal static Linux ELF with one readable and executable load segment.
        let mut bytes = vec![0u8; 129];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x0040_0078u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x0040_0000u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&129u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&129u64.to_le_bytes());
        bytes[112..120].copy_from_slice(&4096u64.to_le_bytes());
        bytes[120..].copy_from_slice(&[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
        bytes
    }

    fn raw_bundle(sha256: &str) -> UtilityBundle {
        serde_json::from_value(json!({
            "version": 1,
            "name": "downloads",
            "tools": [{
                "name": "rg",
                "builds": [{
                    "architecture": "x86_64",
                    "sha256": sha256,
                    "download": {"format": "raw", "url": "https://example.test/linux/rg"}
                }]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn verified_cache_reuses_bytes_offline_and_replaces_only_after_revalidation() {
        let cache = tempfile::tempdir().unwrap();
        let bytes = x86_executable();
        let sha256 = digest(&bytes);
        let bundle = raw_bundle(&sha256);
        let path = cache.path().join(&sha256);
        let result = fetch_bundle_at(&bundle, None, cache.path(), |url, limit| {
            assert_eq!(url, "https://example.test/linux/rg");
            assert!(limit >= bytes.len());
            Ok(bytes.clone())
        })
        .unwrap();
        assert_eq!(result["builds"][0]["status"], "downloaded");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let cached = fetch_bundle_at(&bundle, None, cache.path(), |_, _| {
            panic!("cache must work offline")
        })
        .unwrap();
        assert_eq!(cached["builds"][0]["status"], "cached");

        std::fs::write(&path, b"bad cache").unwrap();
        let failed = fetch_bundle_at(
            &bundle,
            None,
            cache.path(),
            |_, _| Ok(b"wrong pin".to_vec()),
        )
        .unwrap_err();
        assert!(format!("{failed:#}").contains("Executable SHA-256"));
        assert_eq!(std::fs::read(&path).unwrap(), b"bad cache");
        fetch_bundle_at(&bundle, None, cache.path(), |_, _| Ok(bytes.clone())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 1);
    }

    fn tar_fixture(entries: &[&str], binary: &[u8]) -> Vec<u8> {
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        for name in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(binary.len() as u64);
            tar.append_data(&mut header, name, binary).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn archive_member_is_exact_unique_and_pinned() {
        let bytes = x86_executable();
        let tar = tar_fixture(&["other/rg", "download/rg"], &bytes);
        let mut source = UtilityDownload::TarGz {
            url: "https://example.test/rg.tar.gz".into(),
            member: "download/rg".into(),
            archive_sha256: digest(&tar),
        };
        source.validate().unwrap();
        assert_eq!(source.executable(tar.clone()).unwrap(), bytes);
        assert!(
            source
                .executable(b"bad archive".to_vec())
                .unwrap_err()
                .to_string()
                .contains("Release archive SHA-256")
        );
        let duplicate = tar_fixture(&["download/rg", "download/rg"], &bytes);
        assert!(
            tar_member(&duplicate, "download/rg")
                .unwrap_err()
                .to_string()
                .contains("Duplicate")
        );
        assert!(tar_member(&tar, "missing/rg").is_err());
        let UtilityDownload::TarGz { member, .. } = &mut source else {
            unreachable!()
        };
        *member = "../rg".into();
        assert!(source.validate().is_err());

        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("download/rg", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&bytes).unwrap();
        let archive = zip.finish().unwrap().into_inner();
        let zip = UtilityDownload::Zip {
            url: "https://example.test/rg.zip".into(),
            member: "download/rg".into(),
            archive_sha256: digest(&archive),
        };
        assert_eq!(zip.executable(archive).unwrap(), bytes);
    }

    #[test]
    fn download_urls_reject_credentials_plain_http_and_fragments() {
        for url in [
            "http://example.test/file",
            "https://name:password@example.test/file",
            "https://example.test/file#fragment",
            "file:///tmp/rg",
        ] {
            assert!(UtilityDownload::Raw { url: url.into() }.validate().is_err());
        }
    }
}
