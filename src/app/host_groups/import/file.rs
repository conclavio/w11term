use super::list;
use anyhow::{Context, Result, ensure};
use std::{fs::File, io::Read, path::Path};

// UTF-16 ASCII can occupy twice as many bytes as the decoded import limit.
const MAX_FILE_BYTES: usize = 2 * list::MAX_BYTES + 2;

pub(super) fn read(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .context("Opening the host list file")?
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("Reading the host list file")?;
    ensure!(
        bytes.len() <= MAX_FILE_BYTES,
        "The host list is limited to 32 KiB."
    );
    decode(&bytes)
}

const TEXT_FORMAT: &str = "Choose a UTF-8 or BOM-marked UTF-16 text file.";

fn decode(bytes: &[u8]) -> Result<String> {
    let text = if let Some(utf16) = bytes.strip_prefix(&[0xff, 0xfe]) {
        decode_utf16(utf16, u16::from_le_bytes)?
    } else if let Some(utf16) = bytes.strip_prefix(&[0xfe, 0xff]) {
        decode_utf16(utf16, u16::from_be_bytes)?
    } else {
        let utf8 = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        std::str::from_utf8(utf8).context(TEXT_FORMAT)?.to_owned()
    };
    ensure!(!text.contains('\0'), "{TEXT_FORMAT}");
    list::check_size(text.len())?;
    Ok(text)
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Result<String> {
    let (pairs, remainder) = bytes.as_chunks::<2>();
    ensure!(remainder.is_empty(), "{TEXT_FORMAT}");
    char::decode_utf16(pairs.iter().copied().map(unit))
        .collect::<std::result::Result<String, _>>()
        .context(TEXT_FORMAT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_formats_without_leaking_boms_into_addresses() {
        let text = "# café\r\nops@web01.example:2222\r\n";
        let utf8 = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
        let mut little = vec![0xff, 0xfe];
        little.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let mut big = vec![0xfe, 0xff];
        big.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        for bytes in [text.as_bytes(), &utf8, &little, &big] {
            assert_eq!(decode(bytes).unwrap(), text);
        }
        for invalid in [
            b"ops\0@web".as_slice(),
            b"\xff\x80",
            b"\xff\xfe\x41",
            b"\xff\xfe\x00\xd8",
            b"\xff\xfe\x00\x00",
        ] {
            assert!(decode(invalid).is_err());
        }
    }

    #[test]
    fn file_read_caps_raw_bytes_and_the_decoded_import_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("32 KiB"));
        std::fs::write(&path, vec![b'x'; list::MAX_BYTES + 1]).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("32 KiB"));
        let mut allowed = vec![0xff, 0xfe];
        allowed.extend(std::iter::repeat_n([b'x', 0], list::MAX_BYTES).flatten());
        std::fs::write(&path, allowed).unwrap();
        assert_eq!(read(&path).unwrap().len(), list::MAX_BYTES);
    }
}
