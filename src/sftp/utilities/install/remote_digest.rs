use super::*;
use std::fmt::Write as _;

const DIGEST_REPORT: &str = "w11term-tools-sha256-v1";
const CHECKSUM_SCRIPT: &str = r#"cd -P "$1" && [ "$PWD" = "$1" ] || exit 2
shift
printf "%s\n" w11term-tools-sha256-v1
while [ "$#" -gt 0 ]; do
    file=$1
    count=$2
    shift 2
    [ -f "$file" ] && [ ! -L "$file" ] || exit 3
    sum=$(command -p head -c "$count" < "$file" | command -p sha256sum) || exit 4
    printf "%s\n" "${sum%% *}"
done"#;

impl Engine {
    pub(super) async fn verify_release_over_ssh(
        &self,
        session: &Ssh,
        directory: &str,
        release: &Release,
    ) -> Result<bool> {
        let manifest = serde_json::to_vec(release)?;
        let expected_manifest = digest_hex(Sha256::new_with_prefix(&manifest));
        let mut before = Vec::with_capacity(release.tools.len());
        for tool in &release.tools {
            let path = format!("{directory}/{}", tool.name);
            let attrs = self.tool_file_attrs(&path).await?;
            if attrs.size.is_none() || attrs.mtime.is_none() {
                return Ok(false);
            }
            ensure!(
                attrs.size == Some(tool.bytes),
                "Managed executable contents changed at {path}"
            );
            before.push(attrs);
        }

        let command = checksum_command(directory, manifest.len(), &release.tools);
        let Ok((output, 0)) = tool_exec_report(session, &command, Duration::from_secs(3)).await
        else {
            // An unsupported command, a restricted shell, or an expired deadline
            // falls back to hashing the bytes read from the SFTP subsystem.
            return Ok(false);
        };
        let Some(hashes) = parse_report(&output, &expected_manifest, release.tools.len()) else {
            return Ok(false);
        };
        self.check_entry_parent(directory).await?;
        for ((tool, original), hash) in release.tools.iter().zip(before).zip(hashes) {
            let path = format!("{directory}/{}", tool.name);
            let now = self.tool_file_attrs(&path).await?;
            ensure!(
                now.size == original.size
                    && now.mtime == original.mtime
                    && now.permissions == original.permissions
                    && hash == tool.sha256,
                "Managed executable contents changed at {path}"
            );
        }
        Ok(true)
    }
}

fn checksum_command(directory: &str, manifest_len: usize, tools: &[StoredTool]) -> String {
    // The extra byte makes an overgrown file fail its pin while bounding server I/O.
    let mut command = format!(
        "/bin/sh -c {} w11term-tools-sha256 {} {} {}",
        quote_sh(CHECKSUM_SCRIPT),
        quote_sh(directory),
        quote_sh(MANIFEST),
        manifest_len + 1,
    );
    for tool in tools {
        write!(&mut command, " {} {}", quote_sh(&tool.name), tool.bytes + 1)
            .expect("writing to a String");
    }
    command
}

fn parse_report<'a>(bytes: &'a [u8], manifest: &str, tool_count: usize) -> Option<Vec<&'a str>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines().skip_while(|line| *line != DIGEST_REPORT);
    lines.next()?;
    if lines.next()? != manifest {
        return None;
    }
    let hashes: Vec<_> = lines.collect();
    (hashes.len() == tool_count
        && hashes.iter().all(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }))
    .then_some(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_output_must_identify_the_manifest_and_each_tool() {
        let manifest = "a".repeat(64);
        let tool = "b".repeat(64);
        let report = |body: &str| format!("shell greeting\n{DIGEST_REPORT}\n{body}");
        let good = report(&format!("{manifest}\n{tool}\n"));
        assert_eq!(
            parse_report(good.as_bytes(), &manifest, 1),
            Some(vec![tool.as_str()])
        );
        assert!(parse_report(good.as_bytes(), &tool, 1).is_none());
        assert!(parse_report(good.as_bytes(), &manifest, 2).is_none());
        let surplus = report(&format!("{manifest}\n{tool}\n{tool}\n"));
        let invalid = report(&format!("{manifest}\n{}\n", "X".repeat(64)));
        for report in [surplus, invalid] {
            assert!(parse_report(report.as_bytes(), &manifest, 1).is_none());
        }
    }
}
