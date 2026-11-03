//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sorted centroid lists are exactly the projection of canonical assignments.

use super::{invalid, IVFState, StorageBackendResult, StorageReadControl, View};

pub(super) fn validate(view: &View<'_>, control: &StorageReadControl) -> StorageBackendResult<()> {
    control.check()?;
    if view.index.nlist == 0
        || *view.nprobe == 0
        || view.index.train_threshold == 0
        || view.centroids.len() > view.index.nlist
        || view.lists.len() != view.centroids.len()
    {
        return Err(invalid());
    }
    match *view.state {
        IVFState::Untrained => {
            if !view.centroids.is_empty() || *view.trained_size != 0 || *view.deletes != 0 {
                return Err(invalid());
            }
        }
        IVFState::Trained | IVFState::Stale => {
            if view.centroids.is_empty() {
                return Err(invalid());
            }
        }
    }
    for centroid in view.centroids.iter() {
        crate::vector_index::validate_vector_values_controlled(
            view.index.dimensions,
            centroid,
            Some(control),
        )?;
    }
    let mut previous = None;
    for (&key, vector) in view.vectors.iter() {
        control.check()?;
        let expected = match previous {
            Some((document, ordinal)) if document == key.0 => u64::from(ordinal) + 1,
            _ => 0,
        };
        if u64::from(key.1) != expected
            || vector.key != key
            || vector.doc_id != key.0
            || vector.vector_ordinal != key.1
            || vector.norm.is_nan()
            || vector.norm < 0.0
        {
            return Err(invalid());
        }
        crate::vector_index::validate_vector_values_controlled(
            view.index.dimensions,
            &vector.raw_vector,
            Some(control),
        )?;
        crate::vector_index::validate_vector_values_controlled(
            view.index.dimensions,
            &vector.vector,
            Some(control),
        )?;
        match (*view.state, vector.centroid) {
            (IVFState::Untrained, None) => {}
            (IVFState::Trained | IVFState::Stale, Some(centroid))
                if centroid < view.centroids.len() => {}
            _ => return Err(invalid()),
        }
        previous = Some(key);
    }
    let mut total = 0_usize;
    for (centroid, list) in view.lists.iter().enumerate() {
        let mut previous = None;
        for key in list {
            control.check()?;
            if previous.is_some_and(|previous| previous >= *key)
                || view
                    .vectors
                    .get(key)
                    .is_none_or(|vector| vector.centroid != Some(centroid))
            {
                return Err(invalid());
            }
            total = total
                .checked_add(1)
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            previous = Some(*key);
        }
    }
    if *view.state != IVFState::Untrained && total != view.vectors.len() {
        return Err(invalid());
    }
    control.check()
}
