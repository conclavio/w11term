use super::*;
use goblin::elf::{Elf, header, program_header};
use std::io::Read;
use windows::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_SHARE_READ};

pub(super) const MAX_BINARY: u64 = 64 * 1024 * 1024;

#[derive(Serialize)]
pub(super) struct Verified {
    pub sha256: String,
    pub bytes: u64,
    pub interpreter: Option<String>,
    pub glibc_minimum: Option<LibcVersion>,
    pub libraries: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(super) struct LibcVersion(u16, u16, u16);

impl LibcVersion {
    pub fn parse(version: &str) -> Option<Self> {
        let mut components = version.split('.');
        let major = components.next()?.parse().ok()?;
        let minor = components.next()?.parse().ok()?;
        let patch = components
            .next()
            .map_or(Some(0), |value| value.parse().ok())?;
        components
            .next()
            .is_none()
            .then_some(Self(major, minor, patch))
    }
}

impl std::fmt::Display for LibcVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.0, self.1, self.2)
    }
}

impl Verified {
    pub(super) fn read(build: &UtilityBuild, remaining: &mut u64) -> Result<(Self, PathBuf)> {
        let sha256 = normalize_sha256(&build.sha256)?;
        let path = if build.download.is_some() {
            super::downloads::cache_dir()?.join(&sha256)
        } else {
            build.path.clone()
        };
        let bytes = if build.download.is_some() {
            read_pinned(&path, &sha256, remaining)
                .context("Cached executable unavailable or invalid. Run w11term-cli fetch-tool-bundle for this manifest")?
        } else {
            read_pinned(&path, &sha256, remaining)?
        };
        Ok((Self::from_elf(build.architecture, &bytes, sha256)?, path))
    }

    pub(super) fn from_elf(
        architecture: UtilityArchitecture,
        bytes: &[u8],
        sha256: String,
    ) -> Result<Self> {
        Self::inspect_elf(bytes, sha256, Some(architecture)).map(|(verified, _)| verified)
    }

    pub(super) fn inspect_elf(
        bytes: &[u8],
        sha256: String,
        expected: Option<UtilityArchitecture>,
    ) -> Result<(Self, UtilityArchitecture)> {
        let elf = Elf::parse(bytes).context("Reading Linux ELF executable headers")?;
        let architecture = match elf.header.e_machine {
            header::EM_X86_64 => UtilityArchitecture::X86_64,
            header::EM_AARCH64 => UtilityArchitecture::Aarch64,
            _ => bail!("Choose a Linux x86-64 or ARM64 executable"),
        };
        ensure!(
            elf.is_64
                && elf.little_endian
                && expected.is_none_or(|expected| expected == architecture)
                && matches!(
                    elf.header.e_ident[header::EI_OSABI],
                    header::ELFOSABI_NONE | header::ELFOSABI_LINUX
                ),
            "ELF format or machine does not match the declared Linux architecture"
        );
        ensure!(
            matches!(elf.header.e_type, header::ET_EXEC | header::ET_DYN)
                && elf.entry != 0
                && elf.program_headers.iter().any(|segment| {
                    segment.p_type == program_header::PT_LOAD
                        && segment.p_flags & program_header::PF_X != 0
                        && elf
                            .entry
                            .checked_sub(segment.p_vaddr)
                            .is_some_and(|offset| offset < segment.p_filesz)
                        && segment
                            .p_offset
                            .checked_add(segment.p_filesz)
                            .is_some_and(|end| end <= bytes.len() as u64)
                }),
            "ELF has no file-backed executable entry point"
        );
        let interpreter = elf.interpreter.map(str::to_owned);
        let has_interpreter = elf
            .program_headers
            .iter()
            .any(|header| header.p_type == program_header::PT_INTERP);
        ensure!(
            has_interpreter == interpreter.is_some(),
            "ELF has a malformed interpreter"
        );
        if let Some(path) = &interpreter {
            validate_dotfiles_home(path).context("Invalid ELF interpreter path")?;
            ensure!(path != "/", "Invalid ELF interpreter path");
        } else {
            ensure!(
                elf.libraries.is_empty(),
                "ELF requires shared libraries without an interpreter"
            );
        }
        let mut glibc_minimum = None;
        if let Some(needed) = elf.verneed {
            for dependency in needed.iter() {
                for requirement in dependency.iter() {
                    let name = elf
                        .dynstrtab
                        .get_at(requirement.vna_name)
                        .context("Invalid ELF version requirement")?;
                    if let Some(version) = name.strip_prefix("GLIBC_").and_then(LibcVersion::parse)
                    {
                        glibc_minimum = Some(
                            glibc_minimum
                                .map_or(version, |minimum: LibcVersion| minimum.max(version)),
                        );
                    }
                }
            }
        }
        let verified = Self {
            sha256,
            bytes: bytes.len() as u64,
            interpreter,
            glibc_minimum,
            libraries: elf.libraries.into_iter().map(str::to_owned).collect(),
        };
        Ok((verified, architecture))
    }
}

pub(super) fn read_pinned(path: &Path, sha256: &str, remaining: &mut u64) -> Result<Vec<u8>> {
    let bytes = read_executable(path, remaining)?;
    ensure!(
        digest_hex(Sha256::new_with_prefix(&bytes)) == sha256,
        "SHA-256 does not match the pinned executable"
    );
    Ok(bytes)
}

pub(super) fn read_executable(path: &Path, remaining: &mut u64) -> Result<Vec<u8>> {
    use super::super::upload_directory::{Source, open_source};

    let Source::File(file, size) = open_source(path, FILE_READ_DATA, FILE_SHARE_READ)? else {
        bail!("Choose a raw Linux executable file");
    };
    ensure!(
        size > 0 && size <= MAX_BINARY,
        "Linux executable must be between 1 byte and 64 MiB"
    );
    ensure!(size <= *remaining, "Tool read exceeded its byte budget");
    *remaining -= size;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(size + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == size,
        "The Windows file size changed while reading it"
    );
    Ok(bytes)
}
