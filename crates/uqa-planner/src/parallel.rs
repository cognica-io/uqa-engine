//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rayon-backed parallel split + recombine, plus a branch-level
//! [`ParallelExecutor`] that mirrors the canonical UQA implementation's
//! UQA `planner/parallel`.
//!
//! Two parallelism shapes live here:
//!
//! * [`run_parallel`] — chunked Vec-in / Vec-out. Used by parallel-
//!   aware operators (large hash joins, blocking sorts, hash
//!   aggregates) to fan out per-partition work.
//! * [`ParallelExecutor`] — runs N independent worker closures
//!   concurrently and returns their results in input order. Mirrors
//!   `ParallelExecutor.execute_branches` so the operator-tree driver
//!   can fork independent branches (`Intersect` / `Union` /
//!   `LogOddsFusion` / `ProbBoolFusion` children, deep-fusion
//!   `SignalLayer` signals) without serialising them.

// The browser (emscripten) target runs single-threaded, so the rayon
// pool is native-only and every parallel site keeps a sequential twin.
#[cfg(not(target_os = "emscripten"))]
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Split `input` into `num_partitions` chunks, run `worker` over each
/// chunk in parallel, and concatenate the results in the same order
/// the chunks were emitted in.
pub fn run_parallel<T, R, F>(input: Vec<T>, num_partitions: usize, worker: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(Vec<T>) -> Vec<R> + Sync + Send,
{
    let parts = if num_partitions == 0 {
        1
    } else {
        num_partitions
    };
    let chunk_size = input.len().div_ceil(parts).max(1);
    let chunks: Vec<Vec<T>> = input
        .into_iter()
        .fold(Vec::with_capacity(parts), |mut acc, x| {
            if acc
                .last()
                .map(|c: &Vec<T>| c.len() >= chunk_size)
                .unwrap_or(true)
            {
                acc.push(Vec::with_capacity(chunk_size));
            }
            if let Some(chunk) = acc.last_mut() {
                chunk.push(x);
            }
            acc
        });
    #[cfg(not(target_os = "emscripten"))]
    {
        chunks
            .into_par_iter()
            .map(worker)
            .reduce(Vec::new, |mut a, b| {
                a.extend(b);
                a
            })
    }
    #[cfg(target_os = "emscripten")]
    {
        chunks.into_iter().map(worker).fold(Vec::new(), |mut a, b| {
            a.extend(b);
            a
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_parallel_preserves_total_elements() {
        let v: Vec<i32> = (0..100).collect();
        let out = run_parallel(v.clone(), 4, |chunk| {
            chunk.into_iter().map(|x| x * 2).collect()
        });
        assert_eq!(out.len(), v.len());
        assert_eq!(
            out.iter().sum::<i32>(),
            v.iter().map(|x| x * 2).sum::<i32>()
        );
    }

    #[test]
    fn run_parallel_with_zero_partitions_is_safe() {
        let out = run_parallel(vec![1, 2, 3], 0, |c| c);
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn parallel_executor_returns_results_in_branch_order() {
        let par = ParallelExecutor::new(4);
        let workers: Vec<Box<dyn Fn() -> i32 + Send + Sync>> =
            vec![Box::new(|| 1), Box::new(|| 2), Box::new(|| 3)];
        let out = par.execute_branches(&workers);
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn parallel_executor_disabled_falls_back_to_sequential() {
        let par = ParallelExecutor::new(0);
        assert!(!par.enabled());
        let workers: Vec<Box<dyn Fn() -> i32 + Send + Sync>> =
            vec![Box::new(|| 10), Box::new(|| 20)];
        let out = par.execute_branches(&workers);
        assert_eq!(out, vec![10, 20]);
    }

    #[test]
    fn parallel_executor_below_threshold_skips_pool() {
        let par = ParallelExecutor::new(4);
        let workers: Vec<Box<dyn Fn() -> i32 + Send + Sync>> = vec![Box::new(|| 99)];
        let out = par.execute_branches(&workers);
        assert_eq!(out, vec![99]);
    }
}

// ---------------------------------------------------------------
// Branch-level parallel executor (mirrors the canonical UQA implementation's `ParallelExecutor`)
// ---------------------------------------------------------------

/// Default thread pool size; matches `_DEFAULT_MAX_WORKERS` in the
/// canonical UQA behavior. Setting `0` disables parallel execution.
pub const DEFAULT_PARALLEL_WORKERS: usize = 4;

/// Minimum number of branches before parallel dispatch kicks in.
/// Below this, sequential execution wins on overhead. Mirrors
/// `_MIN_PARALLEL_BRANCHES`.
pub const MIN_PARALLEL_BRANCHES: usize = 2;

/// Branch-level parallel executor.
///
/// Holds the configured worker count and a "shutdown" flag. Each call
/// to [`Self::execute_branches`] runs the supplied workers in parallel
/// (when enabled and above the branching threshold) and collects
/// their results in input order.
#[derive(Debug, Clone)]
pub struct ParallelExecutor {
    max_workers: usize,
    shutdown: Arc<AtomicBool>,
}

impl ParallelExecutor {
    /// Build an executor with at most `max_workers` concurrent
    /// branches. `0` disables parallel dispatch (every branch runs
    /// sequentially).
    #[must_use]
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether the executor will dispatch concurrently.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.max_workers > 0 && !self.shutdown.load(Ordering::Acquire)
    }

    /// Mark the executor as shut down. Subsequent
    /// [`Self::execute_branches`] calls fall back to sequential
    /// execution.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Run each `worker` and collect results in the same order. Uses
    /// rayon's work-stealing pool when [`Self::enabled`] is `true` and
    /// the branch count is at least [`MIN_PARALLEL_BRANCHES`];
    /// otherwise falls back to sequential execution. Mirrors the canonical UQA implementation's
    /// `ParallelExecutor.execute_branches`.
    pub fn execute_branches<R, F>(&self, workers: &[F]) -> Vec<R>
    where
        R: Send,
        F: Fn() -> R + Sync + Send,
    {
        if !self.enabled() || workers.len() < MIN_PARALLEL_BRANCHES {
            return workers.iter().map(|w| w()).collect();
        }
        #[cfg(not(target_os = "emscripten"))]
        {
            workers.par_iter().map(|w| w()).collect()
        }
        #[cfg(target_os = "emscripten")]
        {
            workers.iter().map(|w| w()).collect()
        }
    }
}

impl Default for ParallelExecutor {
    fn default() -> Self {
        Self::new(DEFAULT_PARALLEL_WORKERS)
    }
}
