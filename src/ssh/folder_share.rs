//! Read-only Windows folder mounts carried by a pane's authenticated SSH connection.
mod filesystem;
mod mount;
mod protocol;

pub use filesystem::Root as FolderRoot;
pub use mount::{FolderMount, FolderMountStatus, valid_mount_name};
