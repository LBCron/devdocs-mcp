//! Evaluation harness for devdocs retrieval quality (docs/implementation/14).
//!
//! The harness loads a golden suite (`eval/golden/*.jsonl`), runs the
//! retrieval pipeline (a stub until M4, docs/implementation/13), scores the
//! metrics, writes the run artifacts, and prints the gate table.

pub mod error;
pub mod gates;
pub mod metrics;
pub mod pipeline;
pub mod refs;
pub mod report;
pub mod run;
pub mod subprocess;
pub mod suite;

/// Serializes the process-global state — working directory, environment —
/// that tests exercising the resolver fallbacks touch; parallel in-process
/// test threads would otherwise race on it.
#[cfg(test)]
pub(crate) fn process_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
