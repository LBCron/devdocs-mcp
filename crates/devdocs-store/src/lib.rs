//! SQLite storage, schema, and migrations for devdocs.
//!
//! The store is the only crate that touches SQLite
//! (docs/implementation/05 §Concurrency model). It wraps a
//! `Mutex<Connection>`; async callers reach it through
//! `tokio::task::spawn_blocking` and release the guard before every await.

mod error;
mod fschecks;
mod hash;
mod list;
mod migrate;
mod publish;
mod query;
mod search;
mod store;

pub use error::StoreError;
pub use hash::{canonical_body, content_hash, fast_hash};
pub use migrate::{
    AppliedMigration, MIGRATION_COUNT, MigrateOptions, migrate, migrate_with_options,
};
pub use publish::{NewChunk, Symbol};
pub use query::build_match_query;
pub use search::{Hit, SymbolMatch, SymbolScope};
pub use store::{Store, StoreOptions};
