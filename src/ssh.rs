//! Embedded SSH transport, interactive authentication, and reconnect lifecycle.
mod durability;
mod game_resume;
mod herdr;
mod known_hosts;
mod sharing;
mod transport;
pub(crate) use durability::DurableControl;
pub(crate) use durability::Start as DurableStart;
pub use durability::{DurableAction, DurablePhase, DurableState};
pub use sharing::ConnectionShare;
pub(crate) use sharing::{NETWORK, ShareSlot, Sharing};
pub use transport::{Event, run, run_configured};
pub(crate) use transport::{Handler, run_with_sharing};
