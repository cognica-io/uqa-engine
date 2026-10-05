//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable evaluated inputs spill in original order and seal the same transaction fingerprint.

mod record;

use super::{IndexKind, Mutation};
use crate::mvcc::{CommitSequence, PreparedRecordCommit, VersionError, VersionResult};
use crate::read_control::StorageReadControl;
use crate::spill_map::{self, Map, Record};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryError},
    DocId,
};

#[derive(Clone)]
pub(in crate::mvcc) struct OwnedVectorMutation {
    pub(in crate::mvcc) kind: IndexKind,
    pub(in crate::mvcc) metadata: Vec<u8>,
    pub(in crate::mvcc) document: DocId,
    vectors: Option<Vec<Vec<f32>>>,
}

impl OwnedVectorMutation {
    pub(in crate::mvcc) fn retain(
        kind: IndexKind,
        metadata: &[u8],
        mutation: Mutation<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<Budgeted<Self>> {
        control.check()?;
        let (document, vectors) = match mutation {
            Mutation::Replace { document, vectors } => (document, Some(vectors)),
            Mutation::Delete(document) => (document, None),
        };
        let memory = control
            .memory()
            .reserve(record::retained_bytes(metadata, vectors)?)?;
        let vectors = vectors.map(<[Vec<f32>]>::to_vec);
        control.check()?;
        Ok(Budgeted::new(
            Self {
                kind,
                metadata: metadata.to_vec(),
                document,
                vectors,
            },
            memory,
        ))
    }

    pub(in crate::mvcc) fn borrowed(&self) -> Mutation<'_> {
        self.vectors
            .as_ref()
            .map_or(Mutation::Delete(self.document), |vectors| {
                Mutation::Replace {
                    document: self.document,
                    vectors,
                }
            })
    }

    pub(in crate::mvcc) fn vectors(&self) -> &[Vec<f32>] {
        self.vectors.as_deref().unwrap_or(&[])
    }
}

/// Clones retain an immutable journal prefix, including savepoints and sealed retries. Resident inputs share one small component allowance; encrypted ordered pages own the remainder.
#[derive(Clone)]
pub(in crate::mvcc) struct VectorInputs {
    entries: Option<Arc<Budgeted<Map<OwnedVectorMutation>>>>,
    memory: MemoryBudget,
}

impl VectorInputs {
    pub(in crate::mvcc) fn new(memory: &MemoryBudget) -> Self {
        Self {
            entries: None,
            memory: memory.clone(),
        }
    }
    pub(in crate::mvcc) fn len(&self) -> usize {
        self.entries.as_ref().map_or(0, |entries| entries.len())
    }
    pub(in crate::mvcc) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(in crate::mvcc) fn push(
        &mut self,
        input: &OwnedVectorMutation,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        let memory = self.memory.reserve(size_of::<Map<OwnedVectorMutation>>())?;
        let mut entries = self.entries.as_ref().map_or_else(
            || Map::new(&self.memory, self.memory.limit() / 32),
            |entries| (***entries).clone(),
        );
        let _copy = self.memory.reserve(input.memory_bytes()?)?;
        entries.insert(self.len() as u128, input.clone(), Some(control))?;
        self.entries = Some(Budgeted::new(entries, memory).into_shared()?);
        Ok(())
    }
    pub(in crate::mvcc) fn iter(
        &self,
    ) -> impl Iterator<
        Item = crate::StorageBackendResult<(u128, spill_map::Read<'_, OwnedVectorMutation>)>,
    > {
        self.entries.iter().flat_map(|entries| entries.iter())
    }
    pub(in crate::mvcc) fn get(
        &self,
        position: u64,
    ) -> VersionResult<spill_map::Read<'_, OwnedVectorMutation>> {
        self.entries
            .as_ref()
            .ok_or(VersionError::InvalidEncoding("missing vector journal"))?
            .get(u128::from(position))?
            .ok_or(VersionError::InvalidEncoding(
                "missing evaluated vector input",
            ))
    }
}

/// Ordered positions for one physical index; neither this selection nor its replay materializes all tensor inputs.
pub(in crate::mvcc) struct VectorOperations<'a> {
    inputs: &'a VectorInputs,
    positions: Map<u64>,
}

impl<'a> VectorOperations<'a> {
    pub(in crate::mvcc) fn new(inputs: &'a VectorInputs, control: &StorageReadControl) -> Self {
        Self {
            inputs,
            positions: Map::new(control.memory(), control.memory().limit() / 64),
        }
    }
    pub(in crate::mvcc) fn push(
        &mut self,
        position: u128,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.positions.insert(position, 0, Some(control))?;
        Ok(())
    }
    pub(in crate::mvcc) fn len(&self) -> usize {
        self.positions.len()
    }
    pub(in crate::mvcc) fn get(
        &self,
        position: u64,
    ) -> VersionResult<spill_map::Read<'_, OwnedVectorMutation>> {
        self.inputs.get(position)
    }
    pub(in crate::mvcc) fn visit(
        &self,
        control: &StorageReadControl,
        mut visit: impl FnMut(u64, &OwnedVectorMutation) -> VersionResult<()>,
    ) -> VersionResult<()> {
        for entry in self.positions.iter() {
            control.check()?;
            let (position, _) = entry?;
            let position = u64::try_from(position).map_err(|_| MemoryError::SizeOverflow)?;
            let input = self.inputs.get(position)?;
            visit(position, &input)?;
        }
        control.check()?;
        Ok(())
    }
}

pub(in crate::mvcc) struct VectorEffects {
    pub(in crate::mvcc) operations: VectorInputs,
}

impl PreparedRecordCommit {
    pub(in crate::mvcc) fn with_vector_effects(
        mut self,
        base: CommitSequence,
        operations: &VectorInputs,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if operations.is_empty() {
            return Ok(self);
        }
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared vector effects 1");
        digest.update(self.fingerprint());
        digest.update(base.as_u64().to_be_bytes());
        digest.update((operations.len() as u64).to_be_bytes());
        for entry in operations.iter() {
            control.check()?;
            let (_, operation) = entry?;
            digest.update([operation.kind.fingerprint_tag()]);
            let key = &operation.metadata;
            digest.update((key.len() as u64).to_be_bytes());
            for part in key.chunks(4096) {
                control.check()?;
                digest.update(part);
            }
            digest.update(operation.document.to_be_bytes());
            digest.update([u8::from(operation.vectors.is_some())]);
            if let Some(vectors) = &operation.vectors {
                digest.update((vectors.len() as u64).to_be_bytes());
                for vector in vectors {
                    digest.update((vector.len() as u64).to_be_bytes());
                    for part in vector.chunks(1024) {
                        control.check()?;
                        for value in part {
                            digest.update(value.to_bits().to_be_bytes());
                        }
                    }
                }
            }
        }
        self.seal_vector_effects(
            digest.finalize().into(),
            VectorEffects {
                operations: operations.clone(),
            },
        );
        Ok(self)
    }
}
