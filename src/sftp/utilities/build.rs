use super::*;
use goblin::elf::{Elf, header, program_header};
use std::io::Read;
use windows::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_SHARE_READ};

const MAX_BINARY: u64 = 64 * 1024 * 1024;

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
    pub(super) fn read(build: &UtilityBuild, remaining: &mut u64) -> Result<Self> {
        use super::super::upload_directory::{Source, open_source};

        let Source::File(file, size) = open_source(&build.path, FILE_READ_DATA, FILE_SHARE_READ)?
        else {
            bail!("Choose a raw Linux executable file");
        };
        ensure!(
            size > 0 && size <= MAX_BINARY,
            "Linux executable must be between 1 byte and 64 MiB"
        );
        ensure!(
            size <= *remaining,
            "Tool preview exceeds its 256 MiB local read budget"
        );
        *remaining -= size;
        let mut bytes = Vec::with_capacity(size as usize);
        file.take(size + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == size,
            "The Windows file size changed while reading it"
        );
        let sha256 = digest_hex(Sha256::new_with_prefix(&bytes));
        ensure!(
            sha256 == normalize_sha256(&build.sha256)?,
            "SHA-256 does not match the pinned executable"
        );
        Self::from_elf(build.architecture, &bytes, sha256)
    }

    fn from_elf(architecture: UtilityArchitecture, bytes: &[u8], sha256: String) -> Result<Self> {
        let elf = Elf::parse(bytes).context("Reading Linux ELF executable headers")?;
        let machine = match architecture {
            UtilityArchitecture::X86_64 => header::EM_X86_64,
            UtilityArchitecture::Aarch64 => header::EM_AARCH64,
        };
        ensure!(
            elf.is_64
                && elf.little_endian
                && elf.header.e_machine == machine
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
        Ok(Self {
            sha256,
            bytes: bytes.len() as u64,
            interpreter,
            glibc_minimum,
            libraries: elf.libraries.into_iter().map(str::to_owned).collect(),
        })
    }
}
