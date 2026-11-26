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
pub use negotiation::InitialSshAlgorithms;
pub use sharing::{ConnectionShare, VerifiedSshPeer, VerifiedSshRoute};
pub(crate) use sharing::{LoginGate, NETWORK, ShareSlot, Sharing};
pub(crate) use transport::{AttemptStopped, Handler, run_with_sharing};
pub use transport::{Event, run, run_configured};

const SESSION_SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionStep {
    OpeningTcp,
    OpeningJumpChannel,
    KeyExchange,
    Authenticating,
    OpeningSessionChannel,
    RequestingTerminal,
    PreparingRemoteProgram,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ConnectionProgress {
    pub step: ConnectionStep,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// One-based ProxyJump index. A missing index identifies the destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jump: Option<usize>,
}
