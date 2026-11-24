//! Embedded SSH transport, interactive authentication, and reconnect lifecycle.
mod durability;
#[cfg(test)]
mod folder_mount_fixture;
mod folder_share;
mod game_resume;
mod herdr;
mod known_hosts;
mod local_forward;
mod negotiation;
mod sharing;
mod transport;
pub(crate) use durability::DurableControl;
pub(crate) use durability::Start as DurableStart;
pub use durability::{DurableAction, DurablePhase, DurableState};
pub use folder_share::{
    FolderMount, FolderMountStatus, FolderRoot, MountedFolder, valid_mount_name,
};
pub use local_forward::{LocalForward, LocalForwardStatus};
pub use sharing::ConnectionShare;
pub(crate) use sharing::{LoginGate, NETWORK, ShareSlot, Sharing};
pub(crate) use transport::{AttemptStopped, Handler, run_with_sharing};
pub use transport::{Event, run, run_configured};
