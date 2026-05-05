//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rayon-backed parallel split + recombine.
//!
//! Mirrors `uqa/planner/parallel.py`. Used by parallel-aware operators
//! (large hash joins, blocking sorts, hash aggregates) to fan out
//! per-partition work.

use rayon::prelude::*;

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
            acc.last_mut().unwrap().push(x);
            acc
        });
    chunks
        .into_par_iter()
        .map(worker)
        .reduce(Vec::new, |mut a, b| {
            a.extend(b);
            a
        })
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
}
