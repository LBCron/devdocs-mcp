//! Version resolution, hybrid search, RRF fusion, and budget packing.
//!
//! Retrieval code lands at M4 (docs/implementation/12). This crate never
//! reads through ingest.

pub mod budget;
pub mod error;
pub mod fusion;
pub mod matchexpr;
pub mod resolver;
pub mod rung;

pub use budget::{Packed, pack};
pub use error::RetrieveError;
pub use fusion::{RRF_K_DEFAULT, fuse};
pub use matchexpr::rung_match_expr;
pub use resolver::{
    Dep, ProjectContext, Query, ResolutionSource, Resolved, parse_cargo_lock, parse_go_mod,
    parse_package_lock, parse_pnpm_lock, parse_pyproject, parse_uv_lock, resolve,
};
pub use rung::{Scope, fts_rung, symbol_rung};
