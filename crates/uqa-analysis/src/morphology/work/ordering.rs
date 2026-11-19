//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable lexical ranks borrow original rule fields while Core owns heap transitions.

use super::Work;
use crate::morphology::{DictionaryError, DictionaryResult};
use std::cmp::Ordering;
use uqa_core::ordering::{sort_by_with_progress, SortProgress};

pub(crate) trait Surface: Sync {
    fn surface(&self) -> &str;
}

struct Progress<'a, T, W> {
    values: &'a [T],
    work: &'a mut W,
}
impl<T: Surface, W: Work> SortProgress<usize, DictionaryError> for Progress<'_, T, W> {
    async fn checkpoint(&mut self) -> DictionaryResult<()> {
        self.work.checkpoint().await
    }
    async fn compare(&mut self, left: &usize, right: &usize) -> DictionaryResult<Ordering> {
        let (_, order) = crate::morphology::lexicon::common_prefix_order(
            self.values[*left].surface().encode_utf16(),
            self.values[*right].surface().encode_utf16(),
            self.work,
        )
        .await?;
        Ok(order.then_with(|| left.cmp(right)))
    }
}

pub(crate) async fn utf16<T: Surface>(
    values: &[T],
    work: &mut impl Work,
) -> DictionaryResult<Vec<usize>> {
    let mut order = crate::morphology::io::vector(values.len())?;
    for index in 0..values.len() {
        work.records(index).await?;
        order.push(index);
    }
    sort_by_with_progress(&mut order, &mut Progress { values, work }).await?;
    Ok(order)
}
