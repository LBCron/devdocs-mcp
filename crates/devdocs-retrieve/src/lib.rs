//! Version resolution, hybrid search, RRF fusion, and budget packing.
//!
//! Retrieval code lands at M4 (docs/implementation/12). This crate never
//! reads through ingest.

pub mod resolver;

pub use resolver::{
    Dep, ProjectContext, Query, ResolutionSource, Resolved, parse_cargo_lock, parse_go_mod,
    parse_package_lock, parse_pnpm_lock, parse_pyproject, parse_uv_lock, resolve,
};
