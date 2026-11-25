use super::*;
use russh_sftp::extensions::Statvfs;
use w11term::sftp::Operation;

pub(super) fn fixture_stats(available: u64, flags: u64) -> Value {
    json!({
        "block_size":8192, "fragment_size":4096,
        "blocks":1048576, "blocks_free":524288, "blocks_avail":available,
        "inodes":100, "inodes_free":30, "inodes_avail":20, "fs_id":5,
        "flags":flags, "name_max":255
    })
}

impl Files {
    pub(super) fn fixture_statvfs(&self, id: u32, payload: Vec<u8>) -> Result<Packet, StatusCode> {
        let mut payload = payload.into();
        let path: String =
            russh_sftp::de::from_bytes(&mut payload).map_err(|_| StatusCode::BadMessage)?;
        if !payload.is_empty() {
            return Err(StatusCode::BadMessage);
        }
        self.trace_properties("STATVFS", &path)?;
        fs::metadata(self.path(&path)).map_err(io)?;
        let data = fs::read(self.root.join("fixture-space.json")).map_err(io)?;
        let stats: Statvfs = serde_json::from_slice(&data).map_err(|_| StatusCode::BadMessage)?;
        let data = russh_sftp::ser::to_bytes(&stats)
            .map_err(|_| StatusCode::Failure)?
            .to_vec();
        Ok(Packet::ExtendedReply(ExtendedReply { id, data }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn statvfs_reads_the_selected_remote_path_and_reports_missing_extension() {
    for supported in [true, false] {
        let (pane, _, remote, opens, server) = fixture(false).await;
        let root = remote.path();
        fs::create_dir(root.join("remote folder")).unwrap();
        fs::write(root.join("properties-trace"), []).unwrap();
        if supported {
            fs::write(
                root.join("fixture-space.json"),
                fixture_stats(131072, 1).to_string(),
            )
            .unwrap();
        }
        let generation = Some(pane.frames.load().connection_generation);
        let reply = guarded_job(
            &pane,
            generation,
            Operation::Statvfs {
                path: "/remote folder".into(),
            },
        )
        .await;
        let trace = fs::read_to_string(root.join("properties-trace")).unwrap();
        if supported {
            assert_eq!(reply.unwrap(), fixture_stats(131072, 1));
            assert_eq!(
                trace.lines().collect::<Vec<_>>(),
                ["STATVFS /remote folder"]
            );
        } else {
            assert!(
                reply
                    .unwrap_err()
                    .contains("does not advertise statvfs@openssh.com version 2")
            );
            assert!(trace.is_empty());
        }
        assert_eq!(opens.load(Ordering::Relaxed), 2);
        server.abort();
    }
}
