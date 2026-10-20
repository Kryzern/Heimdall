//! Heimdall core: the file index, query language, ranking and IPC protocol
//! shared by the launcher, the indexer service and the benchmark.

pub mod calc;
pub mod index;
pub mod ntfs;
pub mod proto;
pub mod query;
pub mod search;
pub mod snapshot;
pub mod walk;

pub use index::Volume;
pub use query::Query;
pub use search::{Hit, Results, Searcher};
