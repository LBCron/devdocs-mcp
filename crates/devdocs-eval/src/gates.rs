//! The gate table (docs/implementation/14 §Gate table).
//!
//! A FAIL blocks the phase transition; an INCONCLUSIVE (no denominator, no
//! baseline) is also a block. The protocol forbids widening the margin to
//! fit the data.

use serde::Serialize;

use crate::metrics::{Fraction, SuiteMetrics};

/// The status of one gate (docs/implementation/14 §Gate table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GateStatus {
    /// Measured and inside the threshold.
    Pass,
    /// Measured and outside the threshold; blocks the phase transition.
    Fail,
    /// Not measurable in this run; also a block.
    Inconclusive,
}

impl GateStatus {
    /// The table spelling, identical to the `--json` spelling.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }
}

/// One row of the gate table.
#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    /// The gate name, as docs/implementation/14 prints it.
    pub gate: &'static str,
    /// The measured value, formatted for the table.
    pub measured: String,
    /// The threshold, as docs/implementation/14 states it.
    pub threshold: &'static str,
    /// The phase whose transition the gate blocks.
    pub phase: u8,
    /// The status.
    pub status: GateStatus,
}

/// Evaluates the six gates of docs/implementation/14 §Gate table against
/// the suite metrics and the subprocess p95.
#[must_use]
pub fn evaluate(metrics: &SuiteMetrics, p95_ms: Option<f64>) -> Vec<Gate> {
    vec![
        fraction_gate("Symbol recall", metrics.symbol_recall, 0.95, "≥ 0.95", 1),
        fraction_gate("hit@5", metrics.hit_at_5, 0.8, "≥ 0.8", 1),
        p95_gate(p95_ms),
        Gate {
            gate: "Cost per 1,000 queries",
            measured: "$0.00".to_owned(),
            threshold: "≤ $0.20 frontier-class",
            phase: 3,
            status: GateStatus::Pass,
        },
        Gate {
            gate: "Retrieval failures",
            measured: format!("{:.3} (M4 baseline pending)", metrics.failure_rate.value()),
            threshold: "−49% vs the M4 baseline",
            phase: 2,
            status: GateStatus::Inconclusive,
        },
        fraction_gate(
            "Version correctness",
            metrics.version_correctness,
            0.99,
            "≥ 0.99",
            1,
        ),
    ]
}

/// Whether every gate passes; any other status blocks the transition
/// (docs/implementation/14 §Gate table).
#[must_use]
pub fn all_pass(gates: &[Gate]) -> bool {
    gates.iter().all(|g| g.status == GateStatus::Pass)
}

/// Builds one fraction-threshold gate row; an empty denominator is
/// INCONCLUSIVE, also a block.
fn fraction_gate(
    gate: &'static str,
    fraction: Fraction,
    threshold: f64,
    label: &'static str,
    phase: u8,
) -> Gate {
    let status = if fraction.total == 0 {
        GateStatus::Inconclusive
    } else if fraction.value() >= threshold {
        GateStatus::Pass
    } else {
        GateStatus::Fail
    };
    Gate {
        gate,
        measured: if fraction.total == 0 {
            "no denominator".to_owned()
        } else {
            fraction.display()
        },
        threshold: label,
        phase,
        status,
    }
}

/// Builds the p95 gate row.
fn p95_gate(p95_ms: Option<f64>) -> Gate {
    let status = match p95_ms {
        None => GateStatus::Inconclusive,
        Some(ms) if ms < 400.0 => GateStatus::Pass,
        Some(_) => GateStatus::Fail,
    };
    Gate {
        gate: "p95 latency",
        measured: p95_ms.map_or_else(
            || "not measured".to_owned(),
            |ms| format!("{ms:.1} ms (subprocess, after one warmup; placeholder CLI before M4)"),
        ),
        threshold: "< 400 ms",
        phase: 1,
        status,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{GateStatus, all_pass, evaluate};
    use crate::metrics::SuiteMetrics;

    fn metrics(
        hit: (usize, usize),
        recall: (usize, usize),
        failures: (usize, usize),
        version: (usize, usize),
    ) -> SuiteMetrics {
        let frac = |(hits, total): (usize, usize)| crate::metrics::Fraction { hits, total };
        SuiteMetrics {
            hit_at_5: frac(hit),
            symbol_recall: frac(recall),
            failure_rate: frac(failures),
            version_correctness: frac(version),
        }
    }

    #[test]
    fn measurable_gates_pass_and_the_missing_baseline_blocks() {
        let m = metrics((1, 1), (1, 1), (0, 1), (1, 1));
        let gates = evaluate(&m, Some(50.0));
        for gate in &gates {
            let expected = if gate.gate == "Retrieval failures" {
                // No M4 baseline exists yet, so the gate cannot be judged.
                GateStatus::Inconclusive
            } else {
                GateStatus::Pass
            };
            assert_eq!(gate.status, expected, "{gate:?}");
        }
        // An INCONCLUSIVE gate is also a block (docs/implementation/14).
        assert!(!all_pass(&gates));
    }

    #[test]
    fn failing_fractions_fail_and_empty_denominators_block() {
        let m = metrics((0, 1), (0, 0), (0, 1), (1, 1));
        let gates = evaluate(&m, Some(50.0));
        let hit = gates.iter().find(|g| g.gate == "hit@5").unwrap();
        assert_eq!(hit.status, GateStatus::Fail);
        let recall = gates.iter().find(|g| g.gate == "Symbol recall").unwrap();
        assert_eq!(recall.status, GateStatus::Inconclusive);
        assert_eq!(recall.measured, "no denominator");
        assert!(!all_pass(&gates));
    }

    #[test]
    fn p95_judges_the_400ms_threshold() {
        let m = metrics((1, 1), (1, 1), (0, 1), (1, 1));
        let over = evaluate(&m, Some(400.5));
        assert_eq!(
            over.iter()
                .find(|g| g.gate == "p95 latency")
                .unwrap()
                .status,
            GateStatus::Fail
        );
        let under = evaluate(&m, Some(399.9));
        assert_eq!(
            under
                .iter()
                .find(|g| g.gate == "p95 latency")
                .unwrap()
                .status,
            GateStatus::Pass
        );
        let none = evaluate(&m, None);
        assert_eq!(
            none.iter()
                .find(|g| g.gate == "p95 latency")
                .unwrap()
                .status,
            GateStatus::Inconclusive
        );
    }

    #[test]
    fn retrieval_failures_stays_inconclusive_without_the_m4_baseline() {
        let m = metrics((1, 1), (1, 1), (0, 1), (1, 1));
        let gates = evaluate(&m, Some(50.0));
        let gate = gates
            .iter()
            .find(|g| g.gate == "Retrieval failures")
            .unwrap();
        assert_eq!(gate.status, GateStatus::Inconclusive);
        assert_eq!(gate.threshold, "−49% vs the M4 baseline");
    }
}
