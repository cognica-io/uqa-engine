//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Indexed predicate selectivity from MCV and histogram statistics.

use super::{
    column_entropy, compare_values, value_as_f64, CardinalityEstimator, ColumnStats, Predicate,
    Value,
};

impl CardinalityEstimator {
    // -----------------------------------------------------------------
    // Filter selectivity (operator-tree surface)
    // -----------------------------------------------------------------

    pub fn filter_selectivity(&self, field: &str, predicate: &Predicate, _n: f64) -> f64 {
        let Some(cs) = self.column_stats.get(field) else {
            return 0.5;
        };
        if cs.distinct_count == 0 {
            return 0.5;
        }
        let ndv = cs.distinct_count;
        let mut selectivity = match predicate {
            Predicate::Equals(target) => Self::equality_selectivity(cs, target, ndv),
            Predicate::NotEquals(target) => 1.0 - Self::equality_selectivity(cs, target, ndv),
            Predicate::InSet(values) => values
                .iter()
                .map(|v| Self::equality_selectivity(cs, v, ndv))
                .sum::<f64>()
                .min(1.0),
            Predicate::Between { low, high } => self.range_selectivity_for(cs, low, high),
            Predicate::GreaterThan(target) | Predicate::GreaterThanOrEqual(target) => {
                self.gt_selectivity(cs, target)
            }
            Predicate::LessThan(target) | Predicate::LessThanOrEqual(target) => {
                self.lt_selectivity(cs, target)
            }
            Predicate::IsNull => {
                if cs.row_count > 0 {
                    cs.null_count as f64 / cs.row_count as f64
                } else {
                    0.05
                }
            }
            Predicate::IsNotNull => {
                let null_frac = if cs.row_count > 0 {
                    cs.null_count as f64 / cs.row_count as f64
                } else {
                    0.05
                };
                1.0 - null_frac
            }
        };

        // Entropy-based lower bound.
        if cs.distinct_count > 1 {
            let h = column_entropy(cs);
            if h > 0.0 {
                let min_sel = 1.0 / 2.0_f64.powf(h);
                selectivity = selectivity.max(min_sel);
            }
        }
        selectivity.clamp(0.0, 1.0)
    }

    fn equality_selectivity(cs: &ColumnStats, target: &Value, ndv: u64) -> f64 {
        for (mcv, freq) in cs.mcv_values.iter().zip(cs.mcv_frequencies.iter()) {
            if mcv == target {
                return *freq;
            }
        }
        if ndv > 0 {
            1.0 / ndv as f64
        } else {
            1.0
        }
    }

    fn histogram_fraction(boundaries: &[Value], low: &Value, high: &Value) -> f64 {
        if boundaries.len() < 2 {
            return 0.5;
        }
        let n_buckets = (boundaries.len() - 1) as f64;
        let mut overlapping = 0.0;
        for i in 0..(boundaries.len() - 1) {
            let b_low = &boundaries[i];
            let b_high = &boundaries[i + 1];
            if compare_values(high, b_low).is_lt() || compare_values(low, b_high).is_gt() {
                continue;
            }
            if compare_values(low, b_low).is_le() && compare_values(high, b_high).is_ge() {
                overlapping += 1.0;
                continue;
            }
            let (Some(lo_f), Some(hi_f), Some(b_lo_f), Some(b_hi_f)) = (
                value_as_f64(low),
                value_as_f64(high),
                value_as_f64(b_low),
                value_as_f64(b_high),
            ) else {
                overlapping += 1.0;
                continue;
            };
            let b_span = b_hi_f - b_lo_f;
            if b_span <= 0.0 {
                overlapping += 1.0;
                continue;
            }
            let clamp_lo = lo_f.max(b_lo_f);
            let clamp_hi = hi_f.min(b_hi_f);
            overlapping += (clamp_hi - clamp_lo) / b_span;
        }
        (overlapping / n_buckets).clamp(0.0, 1.0)
    }

    fn range_selectivity_for(&self, cs: &ColumnStats, low: &Value, high: &Value) -> f64 {
        if !cs.histogram.is_empty() {
            return Self::histogram_fraction(&cs.histogram, low, high);
        }
        if let (Some(min_v), Some(max_v)) = (cs.min_value.as_ref(), cs.max_value.as_ref()) {
            if let (Some(lo), Some(hi), Some(mn), Some(mx)) = (
                value_as_f64(low),
                value_as_f64(high),
                value_as_f64(min_v),
                value_as_f64(max_v),
            ) {
                let span = mx - mn;
                if span > 0.0 {
                    return ((hi - lo) / span).clamp(0.0, 1.0);
                }
            }
        }
        0.25
    }

    fn gt_selectivity(&self, cs: &ColumnStats, target: &Value) -> f64 {
        if let Some(last) = cs.histogram.last() {
            return Self::histogram_fraction(&cs.histogram, target, last);
        }
        if let (Some(min_v), Some(max_v)) = (cs.min_value.as_ref(), cs.max_value.as_ref()) {
            if let (Some(t), Some(mn), Some(mx)) = (
                value_as_f64(target),
                value_as_f64(min_v),
                value_as_f64(max_v),
            ) {
                let span = mx - mn;
                if span > 0.0 {
                    return ((mx - t) / span).max(0.0);
                }
            }
        }
        1.0 / 3.0
    }

    fn lt_selectivity(&self, cs: &ColumnStats, target: &Value) -> f64 {
        if let Some(first) = cs.histogram.first() {
            return Self::histogram_fraction(&cs.histogram, first, target);
        }
        if let (Some(min_v), Some(max_v)) = (cs.min_value.as_ref(), cs.max_value.as_ref()) {
            if let (Some(t), Some(mn), Some(mx)) = (
                value_as_f64(target),
                value_as_f64(min_v),
                value_as_f64(max_v),
            ) {
                let span = mx - mn;
                if span > 0.0 {
                    return ((t - mn) / span).max(0.0);
                }
            }
        }
        1.0 / 3.0
    }
}
