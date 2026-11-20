//! Embedded SSH transport, interactive authentication, and reconnect lifecycle.
mod durability;
mod known_hosts;
mod sharing;
mod transport;
pub(crate) use durability::DurableControl;
pub use durability::{DurableAction, DurablePhase, DurableState};
pub use sharing::ConnectionShare;
pub(crate) use sharing::{NETWORK, ShareSlot, Sharing};
pub use transport::{Event, run, run_configured};
pub(crate) use transport::{Handler, run_with_sharing};
