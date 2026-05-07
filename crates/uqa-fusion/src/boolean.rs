//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Probabilistic Boolean operators in log space (Section 5, Paper 3).

use uqa_scoring::{prob_and, prob_not, prob_or};

pub struct ProbabilisticBoolean;

impl ProbabilisticBoolean {
    pub fn and(probs: &[f64]) -> f64 {
        prob_and(probs)
    }

    pub fn prob_and(probs: &[f64]) -> f64 {
        Self::and(probs)
    }

    pub fn or(probs: &[f64]) -> f64 {
        prob_or(probs)
    }

    pub fn prob_or(probs: &[f64]) -> f64 {
        Self::or(probs)
    }

    pub fn not(p: f64) -> f64 {
        prob_not(p)
    }

    pub fn prob_not(p: f64) -> f64 {
        Self::not(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {a} ~ {b}");
    }

    #[test]
    fn and_or_de_morgan_pair() {
        let a = 0.7;
        let b = 0.4;
        // ~(a AND b) == ~a OR ~b  =>  not(and([a,b])) == or([not a, not b])
        let lhs = ProbabilisticBoolean::not(ProbabilisticBoolean::and(&[a, b]));
        let rhs =
            ProbabilisticBoolean::or(&[ProbabilisticBoolean::not(a), ProbabilisticBoolean::not(b)]);
        approx_eq(lhs, rhs);
    }
}
