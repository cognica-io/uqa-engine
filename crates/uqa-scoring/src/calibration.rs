//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calibration diagnostics (Paper 3 Section 11.3, Paper 5 Section 8.3).

use crate::prob::PROB_EPSILON;

pub struct CalibrationMetrics;

#[derive(Debug, Clone, PartialEq)]
pub struct ReliabilityBin {
    pub avg_predicted: f64,
    pub avg_actual: f64,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationReport {
    pub ece: f64,
    pub brier: f64,
    pub log_loss: f64,
    pub bins: Vec<ReliabilityBin>,
}

impl CalibrationMetrics {
    pub fn log_loss(probabilities: &[f64], labels: &[u8]) -> f64 {
        assert_eq!(
            probabilities.len(),
            labels.len(),
            "probabilities and labels must have the same length"
        );
        if probabilities.is_empty() {
            return 0.0;
        }
        let n = probabilities.len() as f64;
        let mut s = 0.0;
        for (&p, &y) in probabilities.iter().zip(labels) {
            let pp = p.clamp(PROB_EPSILON, 1.0 - PROB_EPSILON);
            let y = f64::from(y);
            s += y * pp.ln() + (1.0 - y) * (1.0 - pp).ln();
        }
        -s / n
    }

    pub fn brier(probabilities: &[f64], labels: &[u8]) -> f64 {
        assert_eq!(probabilities.len(), labels.len());
        if probabilities.is_empty() {
            return 0.0;
        }
        let n = probabilities.len() as f64;
        let s: f64 = probabilities
            .iter()
            .zip(labels)
            .map(|(&p, &y)| (p - f64::from(y)).powi(2))
            .sum();
        s / n
    }

    pub fn ece(probabilities: &[f64], labels: &[u8], n_bins: usize) -> f64 {
        assert_eq!(probabilities.len(), labels.len());
        let total = probabilities.len();
        if total == 0 || n_bins == 0 {
            return 0.0;
        }
        let mut acc = 0.0;
        for bin in reliability_bins(probabilities, labels, n_bins) {
            acc += (bin.count as f64 / total as f64) * (bin.avg_predicted - bin.avg_actual).abs();
        }
        acc
    }

    pub fn report(probabilities: &[f64], labels: &[u8], n_bins: usize) -> CalibrationReport {
        let bins = reliability_bins(probabilities, labels, n_bins);
        let total = probabilities.len() as f64;
        let ece = if total > 0.0 {
            bins.iter()
                .map(|b| (b.count as f64 / total) * (b.avg_predicted - b.avg_actual).abs())
                .sum()
        } else {
            0.0
        };
        CalibrationReport {
            ece,
            brier: Self::brier(probabilities, labels),
            log_loss: Self::log_loss(probabilities, labels),
            bins,
        }
    }
}

fn reliability_bins(probabilities: &[f64], labels: &[u8], n_bins: usize) -> Vec<ReliabilityBin> {
    if n_bins == 0 || probabilities.is_empty() {
        return Vec::new();
    }
    let mut bins: Vec<(f64, f64, usize)> = vec![(0.0, 0.0, 0); n_bins];
    let n_bins_f = n_bins as f64;
    // Bin boundaries match the upstream Python ECE: lowest bin is
    // `[lo, hi]` (inclusive both ends), the rest are `(lo, hi]`. Floor
    // division places exact upper edges into the higher bin, except for
    // `p == 0.0` which belongs to bin 0.
    for (&p, &y) in probabilities.iter().zip(labels) {
        let mut idx = (p * n_bins_f) as usize;
        if idx >= n_bins {
            idx = n_bins - 1;
        }
        if p == 0.0 {
            idx = 0;
        }
        bins[idx].0 += p;
        bins[idx].1 += f64::from(y);
        bins[idx].2 += 1;
    }
    bins.into_iter()
        .map(|(sum_p, sum_y, count)| {
            if count == 0 {
                ReliabilityBin {
                    avg_predicted: 0.0,
                    avg_actual: 0.0,
                    count: 0,
                }
            } else {
                ReliabilityBin {
                    avg_predicted: sum_p / count as f64,
                    avg_actual: sum_y / count as f64,
                    count,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_loss_zero_for_perfect_predictions() {
        let probs = vec![1.0 - PROB_EPSILON, PROB_EPSILON, 1.0 - PROB_EPSILON];
        let labels = vec![1u8, 0, 1];
        let loss = CalibrationMetrics::log_loss(&probs, &labels);
        assert!(loss < 1e-8, "expected ~0, got {loss}");
    }

    #[test]
    fn brier_zero_for_perfect_predictions() {
        let probs = vec![1.0, 0.0, 1.0];
        let labels = vec![1u8, 0, 1];
        let brier = CalibrationMetrics::brier(&probs, &labels);
        assert!(brier < 1e-12);
    }

    #[test]
    fn ece_zero_when_perfectly_calibrated() {
        // Each bin's avg predicted == avg actual.
        let probs = vec![0.05; 100]; // 5% predicted
        let mut labels = vec![0u8; 100];
        for label in &mut labels[..5] {
            *label = 1;
        }
        let ece = CalibrationMetrics::ece(&probs, &labels, 10);
        assert!(ece < 1e-9, "got {ece}");
    }
}
