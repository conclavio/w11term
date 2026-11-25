use super::{text_preview::check_preview_cancel, *};

const PREVIEW_NAME: &str = "File tail";
const TAIL_BYTES: u64 = 64 * 1024;
const TAIL_LINES: usize = 200;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct FileTail {
    pub text: String,
    pub file_size: u64,
    pub omitted_prefix: bool,
}

impl Engine {
    pub(super) async fn read_file_tail(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<FileTail> {
        let (handle, size) = self
            .open_preview_file(path, PREVIEW_NAME, None, cancel)
            .await?;
        let read = async {
            let size = size.context("The SFTP server did not report the file size")?;
            let clipped = size > TAIL_BYTES;
            // One extra byte tells us if the tail starts on a line boundary.
            let first = size.saturating_sub(TAIL_BYTES + 1);
            let mut bytes = Vec::with_capacity((size - first) as usize);
            let mut offset = first;
            while offset < size {
                check_preview_cancel(cancel, PREVIEW_NAME)?;
                let len = (size - offset).min(self.read_size as u64) as u32;
                let reply = match self.raw.read(&handle, offset, len).await {
                    Ok(reply) => reply,
                    Err(error) if eof(&error) => {
                        bail!(
                            "The file became shorter while reading. Refresh the tail to try again."
                        )
                    }
                    Err(error) => return Err(error.into()),
                };
                ensure!(
                    !reply.data.is_empty() && reply.data.len() <= len as usize,
                    "The SFTP server returned an invalid file tail response"
                );
                offset += reply.data.len() as u64;
                bytes.extend_from_slice(&reply.data);
            }
            let (text, omitted_prefix) = tail_text(&bytes, clipped)?;
            Ok(FileTail {
                text: text.to_owned(),
                file_size: size,
                omitted_prefix,
            })
        }
        .await;
        self.raw.close(handle).await?;
        check_preview_cancel(cancel, PREVIEW_NAME)?;
        read
    }
}

fn tail_text(mut bytes: &[u8], clipped: bool) -> Result<(&str, bool)> {
    if clipped {
        let (&previous, rest) = bytes.split_first().context("Missing file tail boundary")?;
        bytes = rest;
        if previous != b'\n' {
            if let Some(line_break) = rest.iter().position(|byte| *byte == b'\n')
                && line_break + 1 < rest.len()
            {
                bytes = &rest[line_break + 1..];
            } else {
                // A single long line can occupy the whole read. Keep its readable suffix.
                let continuation = rest
                    .iter()
                    .take(3)
                    .take_while(|byte| **byte & 0xc0 == 0x80)
                    .count();
                bytes = &rest[continuation..];
            }
        }
    }
    ensure!(!bytes.contains(&0), "The file tail contains NUL bytes");
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        // A writer may be appending the final character as we read the file.
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])?
        }
        Err(error) => return Err(error).context("The file tail is not UTF-8 text"),
    };
    let lines = text.strip_suffix('\n').unwrap_or(text);
    let leading = lines
        .rmatch_indices('\n')
        .nth(TAIL_LINES - 1)
        .map_or(0, |(index, _)| index + 1);
    Ok((&text[leading..], clipped || leading != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_complete_lines_and_utf8_at_the_file_boundaries() -> Result<()> {
        assert_eq!(tail_text(b"", false)?, ("", false));
        assert_eq!(
            tail_text("partial\r\nlatest 雪\r\nlast".as_bytes(), true)?,
            ("latest 雪\r\nlast", true)
        );
        assert_eq!(
            tail_text("\nlatest 雪\r\n".as_bytes(), true)?,
            ("latest 雪\r\n", true)
        );
        assert_eq!(
            tail_text(&[0xe9, 0x9b, 0xaa, b'x', b'y', b'\n'], true)?,
            ("xy\n", true)
        );
        assert_eq!(tail_text(b"last\n\xe9\x9b", false)?, ("last\n", false));
        assert!(tail_text(b"corrupt\xff\nlast", false).is_err());
        assert!(tail_text(b"binary\0data", false).is_err());
        Ok(())
    }

    #[test]
    fn tail_preserves_the_last_two_hundred_lines_with_or_without_final_newline() -> Result<()> {
        let text = (0..201)
            .map(|line| format!("{line}\r\n"))
            .collect::<String>();
        for input in [text.as_str(), text.trim_end_matches("\r\n")] {
            let (tail, omitted) = tail_text(input.as_bytes(), false)?;
            assert!(omitted);
            assert_eq!(tail.lines().count(), 200);
            assert!(tail.starts_with("1\r\n"));
            assert_eq!(tail.lines().last(), Some("200"));
            assert_eq!(tail.ends_with('\n'), input.ends_with('\n'));
        }
        Ok(())
    }
}
