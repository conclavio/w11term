use w11term::sftp::UtilityArchitecture;

// Minimal ELF programs for Linux x86-64 and AArch64 that exit with status 0.
// The optional interpreter segment lets a fixture exercise missing-loader diagnostics.
pub fn executable(architecture: UtilityArchitecture, interpreter: Option<&str>) -> Vec<u8> {
    let mut bytes = vec![0u8; 256];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    let (machine, code): (u16, &[u8]) = match architecture {
        UtilityArchitecture::X86_64 => (62, &[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]),
        UtilityArchitecture::Aarch64 => (
            183,
            &[0xa8, 0x0b, 0x80, 0xd2, 0, 0, 0x80, 0xd2, 0x01, 0, 0, 0xd4],
        ),
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0040_00e0u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&(1 + u16::from(interpreter.is_some())).to_le_bytes());
    bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
    bytes[80..88].copy_from_slice(&0x0040_0000u64.to_le_bytes());
    bytes[96..104].copy_from_slice(&256u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&256u64.to_le_bytes());
    bytes[112..120].copy_from_slice(&4096u64.to_le_bytes());
    if let Some(loader) = interpreter {
        bytes[120..124].copy_from_slice(&3u32.to_le_bytes());
        bytes[124..128].copy_from_slice(&4u32.to_le_bytes());
        bytes[128..136].copy_from_slice(&176u64.to_le_bytes());
        bytes[152..160].copy_from_slice(&(loader.len() as u64 + 1).to_le_bytes());
        bytes[160..168].copy_from_slice(&(loader.len() as u64 + 1).to_le_bytes());
        bytes[176..176 + loader.len()].copy_from_slice(loader.as_bytes());
    }
    bytes[224..224 + code.len()].copy_from_slice(code);
    bytes
}
