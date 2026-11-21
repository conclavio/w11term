//! Read-only selected-folder export exercised against the Linux FUSE guest.
mod filesystem;
mod protocol;

pub(super) use filesystem::Root;
pub(super) use protocol::{Export, valid_read_path_encoding};
