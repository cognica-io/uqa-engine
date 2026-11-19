//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-width stage/resource identities use retained in-place ordering and deduplication.

use super::Work;
use std::cmp::Ordering;
use uqa_core::ordering::{sort_by_with_progress, SortProgress};

struct Progress<'a, T, K, W> {
    key: fn(&T) -> K,
    work: &'a mut W,
}

impl<T, K: Ord, E, W: Work<E>> SortProgress<T, E> for Progress<'_, T, K, W> {
    async fn checkpoint(&mut self) -> Result<(), E> {
        self.work.checkpoint().await
    }

    async fn compare(&mut self, left: &T, right: &T) -> Result<Ordering, E> {
        Ok((self.key)(left).cmp(&(self.key)(right)))
    }
}

pub(crate) async fn unique<T, K: Ord, E>(
    values: &mut Vec<T>,
    key: fn(&T) -> K,
    work: &mut impl Work<E>,
) -> Result<(), E> {
    sort_by_with_progress(values, &mut Progress { key, work }).await?;
    let mut retained = 0;
    for index in 0..values.len() {
        work.records(index).await?;
        if retained == 0 || key(&values[retained - 1]) != key(&values[index]) {
            values.swap(retained, index);
            retained += 1;
        }
    }
    values.truncate(retained);
    Ok(())
}
