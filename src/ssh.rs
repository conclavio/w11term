//! Embedded SSH transport, interactive authentication, and reconnect lifecycle.
mod known_hosts;
mod transport;
pub(crate) use transport::Handler;
pub use transport::{Event, run, run_configured};
