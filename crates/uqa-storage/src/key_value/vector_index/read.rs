//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical tensor reads for compound index preparation.

use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

use super::super::{
    codec::{blob_to_vector, other_error, read_str, read_u64, vector_field_prefix},
    KeyValueRead,
};
use super::KeyValueVectorIndex;
use crate::StorageBackendResult;

type Entries = Vec<(DocId, u32, Vec<f32>)>;

impl KeyValueVectorIndex {
    pub(in crate::key_value) fn load_all_from(
        &self,
        read: &dyn KeyValueRead,
    ) -> StorageBackendResult<Budgeted<Entries>> {
        // Destroy row allocations before releasing either their slot or payload allowance.
        let mut output = (
            BudgetedVec::new(read.control().memory()),
            read.control().memory().reserve(0)?,
        );
        let (vectors, payload) = (&mut output.0, &mut output.1);
        let mut decoder = CanonicalDecoder::new(self);
        read.visit_prefix(
            &vector_field_prefix(&self.table, &self.field)?,
            &mut |key, value| {
                payload.grow(value.len())?;
                vectors.reserve(1)?;
                vectors.push(decoder.decode(key, value)?)?;
                Ok(())
            },
        )?;
        let (vectors, mut memory) = output.0.into_parts();
        memory.absorb(output.1);
        Ok(Budgeted::new(vectors, memory))
    }
}

pub(super) struct CanonicalDecoder<'a> {
    index: &'a KeyValueVectorIndex,
    current_doc: Option<DocId>,
    expected_ordinal: u32,
}

impl<'a> CanonicalDecoder<'a> {
    pub(super) fn new(index: &'a KeyValueVectorIndex) -> Self {
        Self {
            index,
            current_doc: None,
            expected_ordinal: 0,
        }
    }

    pub(super) fn decode(
        &mut self,
        key: &[u8],
        value: &[u8],
    ) -> StorageBackendResult<(DocId, u32, Vec<f32>)> {
        let mut offset = 1;
        let _table = read_str(key, &mut offset)?;
        let _field = read_str(key, &mut offset)?;
        let doc_id = read_u64(key, &mut offset)?;
        let ordinal = u32::try_from(read_u64(key, &mut offset)?)
            .map_err(|_| other_error("persisted vector ordinal exceeds u32 index format"))?;
        if offset != key.len() {
            return Err(other_error("persisted vector key has trailing bytes"));
        }
        if self.current_doc != Some(doc_id) {
            self.current_doc = Some(doc_id);
            self.expected_ordinal = 0;
        }
        if ordinal != self.expected_ordinal {
            return Err(other_error(format!("invalid persisted vector ordinal sequence for document {doc_id}: expected {}, found {ordinal}", self.expected_ordinal)));
        }
        self.expected_ordinal = self
            .expected_ordinal
            .checked_add(1)
            .ok_or_else(|| other_error("persisted vector ordinal sequence overflow"))?;
        let vector = blob_to_vector(value)?;
        self.index.validate_dimensions(&vector)?;
        Ok((doc_id, ordinal, vector))
    }
}
