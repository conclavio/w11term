//! Embedded SSH transport, interactive authentication, and reconnect lifecycle.
mod transport;
pub(crate) use transport::Handler;
pub use transport::{Event, run, run_configured};
