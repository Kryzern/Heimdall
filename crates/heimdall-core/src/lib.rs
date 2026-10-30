//! Heimdall core: the file index, query language, ranking and IPC protocol
//! shared by the launcher, the indexer service and the benchmark.

pub mod b64;
pub mod calc;
pub mod index;
#[cfg(windows)]
pub mod ntfs;
#[cfg(windows)]
pub mod proto;
pub mod query;
pub mod search;
pub mod snapshot;
#[cfg(windows)]
pub mod walk;

pub use index::Volume;
pub use query::Query;
pub use search::{Hit, Results, Searcher};
