//! Embedded SSH transport, interactive authentication, and reconnect lifecycle.
pub mod agent;
mod authentication;
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
pub(crate) mod triage;
pub use authentication::{AdditionalSshAuthentication, SshKeySource, VerifiedSshAuthentication};
pub(crate) use durability::DurableControl;
pub(crate) use durability::Start as DurableStart;
pub use durability::{DurableAction, DurablePhase, DurableState};
pub use folder_share::{
    FolderMount, FolderMountStatus, FolderRoot, MountedFolder, valid_mount_name,
};
pub use local_forward::{LocalForward, LocalForwardStatus};
pub use negotiation::InitialSshAlgorithms;
pub use sharing::{ConnectionShare, SshTransport, VerifiedSshPeer, VerifiedSshRoute};
pub(crate) use sharing::{LoginGate, NETWORK, ShareSlot, Sharing, TransportWatch};
pub(crate) use transport::{AttemptStopped, Handler, run_with_sharing};
pub use transport::{Event, run, run_configured};
pub use triage::capture_job::CaptureStop;
pub use triage::capture_job::CaptureStop as SyscallCaptureStop;
pub use triage::cpu_profile::launch::{CpuLaunchOptions, CpuLaunchPlan, CpuWorkloadOutcome};
pub use triage::cpu_profile::{
    CpuProfilePlan, CpuProfileSummary, PERF_DATA_TRANSFER_SECONDS, PerfDataArtifact,
};
pub use triage::packet_capture::{
    Capture as PacketCapture, CaptureComparison as PacketCaptureComparison,
    CaptureExcerpt as PacketCaptureExcerpt, CaptureExcerptPlan as PacketCaptureExcerptPlan,
    CaptureInterfaces as PacketCaptureInterfaces, ConversationChange as PacketConversationChange,
    ConversationEndpoints as PacketConversationEndpoints,
    ConversationQuery as PacketConversationQuery, LocalCapture as LocalPacketCapture,
    Plan as PacketCapturePlan, ip_protocol_label, open_capture_conversation_in_wireshark,
    open_capture_conversation_interval_in_wireshark, open_capture_in_wireshark,
    open_capture_interval_in_wireshark, open_capture_pair_in_wireshark,
    open_capture_transport_in_wireshark,
};
pub use triage::syscall_capture::{Capture as SyscallCapture, Plan as SyscallCapturePlan};
pub use triage::{
    Delta as TriageDelta, Finding as TriageFinding, Plan as TriagePlan, Report as TriageReport,
    Resource as TriageResource, SampleStatus as TriageSampleStatus, Target as TriageTarget,
};

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
