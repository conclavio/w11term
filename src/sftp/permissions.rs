use super::*;
use russh_sftp::protocol::Packet;

const LSETSTAT: &str = "lsetstat@openssh.com";
pub const PERMISSIONS_OUTCOME_UNKNOWN: &str =
    "Permission change outcome unknown. Reopen Properties to check the mode";
const PERMISSIONS_REFRESH: &str =
    "Server acknowledged the permission change. Reopen Properties to check the mode";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionReview {
    mode: u32,
    uid: Option<u32>,
    gid: Option<u32>,
}

impl PermissionReview {
    pub fn from_attributes(attrs: &Attributes) -> Option<Self> {
        let mode = attrs.permissions?;
        matches!(
            EntryKind::from_permissions(Some(mode)),
            Some(EntryKind::File | EntryKind::Directory)
        )
        .then_some(Self {
            mode,
            uid: attrs.uid,
            gid: attrs.gid,
        })
    }

    pub fn mode(self) -> u32 {
        self.mode & 0o7777
    }
}

impl Engine {
    pub(super) async fn set_entry_permissions(
        &self,
        path: &str,
        review: PermissionReview,
        mode: u32,
        stage: &mut JobStage,
    ) -> Result<Value> {
        ensure!(mode <= 0o7777, "Enter a valid octal permission mode");
        ensure!(mode != review.mode(), "Enter a different permission mode");
        self.checked_source_folder(path).await?;
        let attrs = Attributes::from(self.raw.lstat(path).await?.attrs);
        ensure!(
            PermissionReview::from_attributes(&attrs) == Some(review),
            "The entry type, ownership or permissions changed. Reopen Properties to review"
        );
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..FileAttributes::empty()
        };
        *stage = JobStage::AwaitingStatus(PERMISSIONS_OUTCOME_UNKNOWN);
        if self.extension(LSETSTAT, "1").is_ok() {
            let payload = russh_sftp::ser::to_bytes(&(path, attrs))?.to_vec();
            match self.raw.extended(LSETSTAT, payload).await? {
                Packet::Status(status) if status.status_code == StatusCode::Ok => {}
                Packet::Status(status) => return Err(Error::Status(status).into()),
                _ => return Err(Error::UnexpectedPacket.into()),
            }
        } else {
            self.raw.setstat(path, attrs).await?;
        }
        *stage = JobStage::AwaitingStatus(PERMISSIONS_REFRESH);
        let attrs = self
            .raw
            .lstat(path)
            .await
            .context(PERMISSIONS_REFRESH)?
            .attrs;
        Ok(json!({ "attrs": Attributes::from(attrs) }))
    }
}
