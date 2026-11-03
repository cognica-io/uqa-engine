//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete HNSW images retain topology, exact normalization and pending persistence without graph replay.

use super::{
    types::{active_key, HNSWIndex, HNSWNode, HNSWVector},
    MAX_HNSW_LEVEL,
};
use crate::{
    image_io::{ImageInput, ImageOutput},
    read_control::StorageReadControl,
    vector_index::image::{floats, header, invalid, read_floats},
    StorageBackendResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec};

mod input;
#[cfg(test)]
mod tests;
mod validation;
pub(crate) use input::read;

pub(super) fn write(
    index: &HNSWIndex,
    writer: &mut dyn std::io::Write,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    validation::validate(index, control)?;
    let mut out = ImageOutput {
        writer,
        control,
        written: 0,
    };
    header(&mut out, 2, index.dimensions)?;
    for count in [
        index.params.m,
        index.params.ef_construction,
        index.params.ef_search,
        index.params.rebuild_threshold,
    ] {
        out.count(count)?;
    }
    out.bytes(&index.params.seed.to_le_bytes())?;
    if let Some(entry) = index.entry_point {
        out.bytes(&[1])?;
        out.bytes(&entry.to_le_bytes())?;
    } else {
        out.bytes(&[0])?;
    }
    out.count(index.max_level)?;
    out.bytes(&index.next_node_id.to_le_bytes())?;
    out.count(index.deleted_count)?;
    out.bytes(&[u8::from(index.full_rewrite)])?;
    out.count(index.nodes.len())?;
    for entry in index.nodes.iter_with_memory(control.memory()) {
        control.check()?;
        let (id, node) = entry?;
        out.bytes(&node.id.to_le_bytes())?;
        out.bytes(&node.doc_id.to_le_bytes())?;
        out.bytes(&node.vector_ordinal.to_le_bytes())?;
        out.count(node.level)?;
        out.bytes(&[u8::from(node.deleted)])?;
        for map in [&index.raw_vectors, &index.normalized_vectors] {
            let vector = map
                .get_with_memory(id, control.memory())?
                .ok_or_else(invalid)?;
            out.bytes(&vector.norm.to_bits().to_le_bytes())?;
            floats(&mut out, &vector.values)?;
        }
        for layer in &node.neighbors {
            out.count(layer.len())?;
            for neighbor in layer {
                out.bytes(&neighbor.to_le_bytes())?;
            }
        }
    }
    out.count(index.dirty_nodes.len())?;
    for entry in index.dirty_nodes.iter_with_memory(control.memory()) {
        let (id, value) = entry?;
        out.bytes(&u64::try_from(id).map_err(|_| invalid())?.to_le_bytes())?;
        out.bytes(&value.to_le_bytes())?;
    }
    control.check()?;
    Ok(out.written)
}

fn flag(input: &mut ImageInput<'_>) -> StorageBackendResult<bool> {
    match input.array::<1>()?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid()),
    }
}
