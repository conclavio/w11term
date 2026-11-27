//! Shared parsing, comparison and JSON export for saved folded CPU stacks.

mod compare;
pub mod flame_graph;
mod profile;

pub use compare::{Change, Comparison, FilteredComparison};
pub use profile::{Frame, Profile, Search};

pub fn sample_share(samples: u64, total: u64) -> f64 {
    100.0 * samples as f64 / total as f64
}
