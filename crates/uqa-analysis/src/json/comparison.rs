//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed JSON string observations retain progress through long common prefixes.

use std::cmp::Ordering;

use crate::work::Work;

pub(crate) async fn bytes_compare<E>(
    left: &[u8],
    right: &[u8],
    work: &mut impl Work<E>,
) -> Result<Ordering, E> {
    let common = left.len().min(right.len());
    for (left, right) in left[..common]
        .chunks(4096)
        .zip(right[..common].chunks(4096))
    {
        work.checkpoint().await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

pub(crate) async fn bytes_equal<E>(
    left: &[u8],
    right: &[u8],
    work: &mut impl Work<E>,
) -> Result<bool, E> {
    if left.len() != right.len() {
        return Ok(false);
    }
    Ok(bytes_compare(left, right, work).await? == Ordering::Equal)
}
