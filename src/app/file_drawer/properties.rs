use anyhow::{Context, Result};
use chrono::DateTime;
use serde_json::Value;
use w11term::sftp::Attributes;

const UNREPORTED: &str = "Not reported";

fn entry_type(mode: Option<u32>) -> &'static str {
    match mode.unwrap_or(0) & 0o170000 {
        0o100000 => "Regular file",
        0o040000 => "Directory",
        0o120000 => "Symbolic link",
        0o010000 => "FIFO",
        0o020000 => "Character device",
        0o060000 => "Block device",
        0o140000 => "Socket",
        0 => UNREPORTED,
        _ => "Unknown file type",
    }
}

fn timestamp(seconds: Option<u32>) -> String {
    seconds
        .and_then(|s| DateTime::from_timestamp(i64::from(s), 0))
        .map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| UNREPORTED.into())
}

pub(super) fn display_text(path: &str, mut reply: Value) -> Result<String> {
    let data = reply
        .get_mut("attrs")
        .context("Missing SFTP file attributes")?
        .take();
    let attrs: Attributes =
        serde_json::from_value(data).context("Missing or invalid SFTP file attributes")?;
    let kind = entry_type(attrs.permissions);
    let size = match attrs.size {
        Some(1) => "1 byte".into(),
        Some(bytes) => format!("{bytes} bytes"),
        None => UNREPORTED.into(),
    };
    let permissions = attrs
        .permissions
        .map(|m| format!("{:04o}", m & 0o7777))
        .unwrap_or_else(|| UNREPORTED.into());
    let id = |value: Option<u32>| {
        value
            .map(|n| n.to_string())
            .unwrap_or_else(|| UNREPORTED.into())
    };
    let mut lines = vec![
        format!("Path  {path}"),
        format!("Type  {kind}"),
        format!("Size  {size}"),
        format!("Modified  {}", timestamp(attrs.mtime)),
        format!("Accessed  {}", timestamp(attrs.atime)),
        format!("Permissions  {permissions}"),
        format!("Owner UID  {}", id(attrs.uid)),
        format!("Group GID  {}", id(attrs.gid)),
    ];
    if let Some(target) = reply["target"].as_str() {
        lines.push(format!("Link target  {target}"));
    } else if let Some(reason) = reply["target_error"].as_str() {
        lines.push(format!("Link target unavailable. {reason}"));
    }
    Ok(lines.join("\r\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PATH: &str = "/srv/雪/latest.log";

    #[test]
    fn properties_preserve_zero_ids_and_epoch_special_mode_bits_and_wide_sizes() {
        let text = display_text(
            PATH,
            json!({"attrs":{
                "size":u64::MAX,"permissions":0o104750,"uid":0,"gid":12,"mtime":0,"atime":u32::MAX
            }}),
        )
        .unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "Path  /srv/雪/latest.log",
                "Type  Regular file",
                "Size  18446744073709551615 bytes",
                "Modified  1970-01-01 00:00:00 UTC",
                "Accessed  2106-02-07 06:28:15 UTC",
                "Permissions  4750",
                "Owner UID  0",
                "Group GID  12"
            ]
        );
    }

    #[test]
    fn missing_attributes_and_partial_link_metadata_stay_distinct_from_zero() {
        let text = display_text(PATH, json!({"attrs":{}})).unwrap();
        assert_eq!(text.matches(UNREPORTED).count(), 7);
        assert!(display_text(PATH, json!({})).is_err());
        let text = display_text(
            PATH,
            json!({"attrs":{"permissions":0o120777,"size":1},
            "target_error":"Permission denied"}),
        )
        .unwrap();
        assert!(text.contains("Type  Symbolic link\r\nSize  1 byte"));
        assert!(text.ends_with("Link target unavailable. Permission denied"));
        let text = display_text(
            PATH,
            json!({"attrs":{"permissions":0o120640},
            "target":"../archives/古い.log"}),
        )
        .unwrap();
        assert!(text.ends_with("Link target  ../archives/古い.log"));
    }
}
