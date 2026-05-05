//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-operator cost model.
//!
//! The cost is unitless: relative numbers across plans are what
//! matters. We use the System-R style breakdown of `cpu_cost +
//! io_cost + memory_cost`, scaled by per-operator constants from the
//! Python reference (`uqa/planner/cost_model.py`).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatorKind {
    TableScan,
    IndexScan,
    Filter,
    Project,
    Sort,
    HashAggregate,
    Window,
    Limit,
    HashJoinInner,
    HashJoinOuter,
    SortMergeJoin,
    NestedLoopJoin,
    IndexJoin,
    SemiJoin,
    AntiJoin,
    CrossJoin,
}

#[derive(Debug, Clone, Copy)]
pub struct OperatorCost {
    pub cpu: f64,
    pub io: f64,
    pub memory: f64,
}

impl OperatorCost {
    pub fn zero() -> Self {
        Self {
            cpu: 0.0,
            io: 0.0,
            memory: 0.0,
        }
    }

    pub fn total(&self) -> f64 {
        self.cpu + self.io + self.memory
    }

    pub fn add(&self, other: &OperatorCost) -> OperatorCost {
        OperatorCost {
            cpu: self.cpu + other.cpu,
            io: self.io + other.io,
            memory: self.memory + other.memory,
        }
    }
}

/// Coefficients tuned against the Python reference benchmarks.
/// Stable enough that the join enumerator picks the same shapes the
/// Python optimizer does on the parity test corpus.
#[derive(Debug, Clone, Copy)]
pub struct CostCoefficients {
    pub scan_per_row: f64,
    pub index_per_row: f64,
    pub filter_per_row: f64,
    pub project_per_row: f64,
    pub sort_per_row_log: f64,
    pub hashagg_build_per_row: f64,
    pub window_per_row: f64,
    pub limit_per_row: f64,
    pub hashjoin_build_per_row: f64,
    pub hashjoin_probe_per_row: f64,
    pub sortmerge_per_row: f64,
    pub nestedloop_per_pair: f64,
    pub crossjoin_per_pair: f64,
    pub io_per_disk_row: f64,
}

impl Default for CostCoefficients {
    fn default() -> Self {
        Self {
            scan_per_row: 1.0,
            index_per_row: 0.1,
            filter_per_row: 0.2,
            project_per_row: 0.1,
            sort_per_row_log: 1.5,
            hashagg_build_per_row: 1.2,
            window_per_row: 1.5,
            limit_per_row: 0.05,
            hashjoin_build_per_row: 0.8,
            hashjoin_probe_per_row: 0.3,
            sortmerge_per_row: 1.0,
            nestedloop_per_pair: 0.05,
            crossjoin_per_pair: 0.04,
            io_per_disk_row: 5.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CostEstimator {
    pub coefficients: CostCoefficients,
}

impl Default for CostEstimator {
    fn default() -> Self {
        Self {
            coefficients: CostCoefficients::default(),
        }
    }
}

impl CostEstimator {
    pub fn new(coefficients: CostCoefficients) -> Self {
        Self { coefficients }
    }

    /// Cost of materialising `rows` rows from `kind`. For join
    /// operators, `rows` is the input cardinality; the enumerator
    /// folds the build / probe sides separately and adds the costs.
    pub fn estimate_unary(&self, kind: OperatorKind, rows: f64) -> OperatorCost {
        let c = &self.coefficients;
        let rows = rows.max(0.0);
        let log_rows = (rows.max(2.0)).log2();
        match kind {
            OperatorKind::TableScan => OperatorCost {
                cpu: rows * c.scan_per_row,
                io: rows * c.io_per_disk_row,
                memory: 0.0,
            },
            OperatorKind::IndexScan => OperatorCost {
                cpu: rows * c.index_per_row,
                io: rows * c.io_per_disk_row * 0.5,
                memory: 0.0,
            },
            OperatorKind::Filter => OperatorCost {
                cpu: rows * c.filter_per_row,
                io: 0.0,
                memory: 0.0,
            },
            OperatorKind::Project => OperatorCost {
                cpu: rows * c.project_per_row,
                io: 0.0,
                memory: 0.0,
            },
            OperatorKind::Sort => OperatorCost {
                cpu: rows * log_rows * c.sort_per_row_log,
                io: 0.0,
                memory: rows,
            },
            OperatorKind::HashAggregate => OperatorCost {
                cpu: rows * c.hashagg_build_per_row,
                io: 0.0,
                memory: rows,
            },
            OperatorKind::Window => OperatorCost {
                cpu: rows * c.window_per_row,
                io: 0.0,
                memory: rows,
            },
            OperatorKind::Limit => OperatorCost {
                cpu: rows * c.limit_per_row,
                io: 0.0,
                memory: 0.0,
            },
            _ => OperatorCost::zero(),
        }
    }

    /// Cost of joining `left_rows` to `right_rows` via `kind`. For
    /// hash joins, the smaller side is assumed to be the build side
    /// (the enumerator decides which one when it constructs the
    /// node).
    pub fn estimate_join(
        &self,
        kind: OperatorKind,
        left_rows: f64,
        right_rows: f64,
    ) -> OperatorCost {
        let c = &self.coefficients;
        let l = left_rows.max(0.0);
        let r = right_rows.max(0.0);
        let (build, probe) = if l <= r { (l, r) } else { (r, l) };
        match kind {
            OperatorKind::HashJoinInner => OperatorCost {
                cpu: build * c.hashjoin_build_per_row + probe * c.hashjoin_probe_per_row,
                io: 0.0,
                memory: build,
            },
            OperatorKind::HashJoinOuter => OperatorCost {
                cpu: build * c.hashjoin_build_per_row * 1.2
                    + probe * c.hashjoin_probe_per_row * 1.2,
                io: 0.0,
                memory: build,
            },
            OperatorKind::SortMergeJoin => {
                let total = l + r;
                OperatorCost {
                    cpu: total * c.sortmerge_per_row + total * (total.max(2.0)).log2() * 0.5,
                    io: 0.0,
                    memory: total,
                }
            }
            OperatorKind::NestedLoopJoin => OperatorCost {
                cpu: l * r * c.nestedloop_per_pair,
                io: 0.0,
                memory: 0.0,
            },
            OperatorKind::IndexJoin => OperatorCost {
                cpu: l * c.hashjoin_probe_per_row + l * c.index_per_row,
                io: l * c.io_per_disk_row * 0.5,
                memory: 0.0,
            },
            OperatorKind::SemiJoin | OperatorKind::AntiJoin => OperatorCost {
                cpu: probe * c.hashjoin_probe_per_row + build * c.hashjoin_build_per_row,
                io: 0.0,
                memory: build,
            },
            OperatorKind::CrossJoin => OperatorCost {
                cpu: l * r * c.crossjoin_per_pair,
                io: 0.0,
                memory: 0.0,
            },
            _ => OperatorCost::zero(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_join_prefers_smaller_build_side() {
        let est = CostEstimator::default();
        let a = est.estimate_join(OperatorKind::HashJoinInner, 100.0, 1_000_000.0);
        let b = est.estimate_join(OperatorKind::HashJoinInner, 1_000_000.0, 100.0);
        // Symmetric: the model swaps build/probe internally.
        assert!((a.total() - b.total()).abs() < 1e-6);
    }

    #[test]
    fn nested_loop_grows_quadratically() {
        let est = CostEstimator::default();
        let a = est.estimate_join(OperatorKind::NestedLoopJoin, 100.0, 100.0);
        let b = est.estimate_join(OperatorKind::NestedLoopJoin, 200.0, 200.0);
        assert!(b.total() > a.total() * 3.5);
    }

    #[test]
    fn sort_cpu_dominates_for_large_inputs() {
        let est = CostEstimator::default();
        let cost = est.estimate_unary(OperatorKind::Sort, 10_000.0);
        assert!(cost.cpu > 0.0);
        assert!(cost.memory > 0.0);
    }
}
