use super::{CaptureFile, Rotation, RotationResult, count_packet_records};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    sequence: u64,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    packets: u64,
    segments: Vec<Segment>,
}

fn manifest(json: &str, limits: Rotation) -> Result<Manifest> {
    let mut manifest: Manifest = serde_json::from_str(json).context("Reading the packet ring")?;
    let segments = &mut manifest.segments;
    let retained = segments.len();
    ensure!(
        (1..=usize::from(limits.files)).contains(&retained),
        "The packet ring exceeds the reviewed file count or is empty"
    );
    let first = segments[0].sequence;
    ensure!(
        first == 0 || retained == usize::from(limits.files),
        "The packet ring is missing retained files"
    );
    for (index, segment) in segments.iter_mut().enumerate() {
        ensure!(
            first.checked_add(index as u64) == Some(segment.sequence),
            "The packet ring sequence is incomplete or out of order"
        );
        ensure!(
            (24..=limits.bytes_per_file).contains(&segment.bytes)
                && (retained == 1 || segment.bytes > 24),
            "A packet ring segment is empty or exceeds its reviewed byte budget"
        );
        segment.sha256 = crate::sftp::normalize_sha256(&segment.sha256)?;
    }
    Ok(manifest)
}

pub(super) async fn download(
    handle: &russh::client::Handle<crate::ssh::Handler>,
    remote: &str,
    parent: &Path,
    limits: Rotation,
    json: &str,
) -> Result<(CaptureFile, RotationResult)> {
    let Manifest { packets, segments } = manifest(json, limits)?;
    let summary = RotationResult {
        retained_files: segments.len(),
        overwritten_files: segments[0].sequence,
        captured_packets: packets,
    };
    let mut verified = Vec::with_capacity(segments.len());
    for segment in segments {
        let slot = segment.sequence % u64::from(limits.files);
        let path = format!("{remote}/segment-{slot}.pcap");
        let file = crate::sftp::stage_diagnostic_download(
            handle,
            &path,
            parent,
            segment.bytes,
            &segment.sha256,
        )
        .await?;
        verified.push((file, segment.bytes));
    }
    let parent = parent.to_owned();
    let capture = tokio::task::spawn_blocking(move || assemble(&parent, verified)).await??;
    ensure!(
        packets
            .checked_sub(u64::from(capture.packets))
            .is_some_and(|discarded| {
                if summary.overwritten_files == 0 {
                    discarded == 0
                } else {
                    discarded >= summary.overwritten_files
                }
            }),
        "Packet ring counts do not match the saved segments"
    );
    Ok((capture, summary))
}

fn assemble(parent: &Path, segments: Vec<(tempfile::NamedTempFile, u64)>) -> Result<CaptureFile> {
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    let mut bytes = 0;
    let mut packets: u32 = 0;
    let mut global_header = None;
    for (segment, size) in segments {
        let mut input = BufReader::new(segment.reopen()?);
        let mut header = [0; 24];
        input.read_exact(&mut header)?;
        packets += count_packet_records(&mut input, &header, size)?;
        if let Some(expected) = global_header {
            ensure!(
                expected == header,
                "Packet ring segments have different pcap headers"
            );
        } else {
            file.write_all(&header)?;
            bytes += header.len() as u64;
            global_header = Some(header);
        }
        input.seek(SeekFrom::Start(header.len() as u64))?;
        let copied = io::copy(&mut input, &mut file)?;
        ensure!(
            copied == size - header.len() as u64,
            "Packet ring segment changed while saving"
        );
        bytes += copied;
    }
    file.as_file().sync_all()?;
    let mut input = BufReader::new(file.reopen()?);
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let length = input.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(CaptureFile {
        file,
        bytes,
        packets,
        sha256: crate::sftp::digest_hex(digest),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reviewed_caps_and_capture_order_apply_before_any_download() {
        let limits = Rotation {
            files: 2,
            bytes_per_file: 128,
        };
        let segment = |sequence, bytes| json!({"sequence": sequence, "bytes": bytes, "sha256": "a".repeat(64)});
        let read = |segments| {
            manifest(
                &json!({"packets": 9, "segments": segments}).to_string(),
                limits,
            )
        };
        assert!(read(vec![segment(5, 128), segment(6, 44)]).is_ok());
        assert!(read(vec![segment(5, 129), segment(6, 44)]).is_err());
        assert!(read(vec![segment(5, 128), segment(7, 44)]).is_err());
        assert!(read(vec![segment(5, 128)]).is_err());
        assert!(read(vec![segment(0, 128), segment(1, 44), segment(2, 44)]).is_err());
    }
}
