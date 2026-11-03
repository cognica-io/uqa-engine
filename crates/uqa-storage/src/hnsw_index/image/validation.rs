//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate evaluated vector storage and derived active membership before ordinary topology validation.

use super::{
    active_key, invalid, HNSWIndex, StorageBackendResult, StorageReadControl, MAX_HNSW_LEVEL,
};
use crate::vector_index::validate_vector_values_controlled;

pub(super) fn validate(
    index: &HNSWIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    index.params.validate()?;
    if index.max_level > MAX_HNSW_LEVEL {
        return Err(invalid());
    }
    let mut live = 0_usize;
    let mut deleted = 0_usize;
    for entry in index.nodes.iter_with_memory(control.memory()) {
        control.check()?;
        let (id, node) = entry?;
        if id != u128::from(node.id)
            || node.level > MAX_HNSW_LEVEL
            || node.neighbors.len() != node.level + 1
        {
            return Err(invalid());
        }
        if node.deleted {
            deleted += 1;
        } else {
            live += 1;
            if index
                .active
                .get_with_memory(
                    active_key(node.doc_id, node.vector_ordinal),
                    control.memory(),
                )?
                .is_none_or(|active| *active != node.id)
            {
                return Err(invalid());
            }
        }
        for (normalized, map) in [
            (false, &index.raw_vectors),
            (true, &index.normalized_vectors),
        ] {
            let vector = map
                .get_with_memory(id, control.memory())?
                .ok_or_else(invalid)?;
            if vector.norm.is_nan()
                || vector.norm < 0.0
                || (normalized && vector.norm.to_bits() != 1.0_f32.to_bits())
            {
                return Err(invalid());
            }
            validate_vector_values_controlled(index.dimensions, &vector.values, Some(control))?;
        }
    }
    if live != index.active.len() || deleted != index.deleted_count {
        return Err(invalid());
    }
    let mut previous = None;
    for entry in index.active.iter_with_memory(control.memory()) {
        control.check()?;
        let (key, _) = entry?;
        if key >> 96 != 0 {
            return Err(invalid());
        }
        let document = (key >> 32) as u64;
        let ordinal = key as u32;
        let expected = match previous {
            Some((prior, ordinal)) if prior == document => u64::from(ordinal) + 1,
            _ => 0,
        };
        if u64::from(ordinal) != expected {
            return Err(invalid());
        }
        previous = Some((document, ordinal));
    }
    for entry in index.dirty_nodes.iter_with_memory(control.memory()) {
        control.check()?;
        let (key, _) = entry?;
        if !index.nodes.contains_key(key)? {
            return Err(invalid());
        }
    }
    index.validate_controlled(Some(control))
}
