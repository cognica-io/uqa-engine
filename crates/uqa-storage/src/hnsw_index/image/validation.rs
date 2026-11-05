//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resumable evaluated-vector and membership checks precede the shared structural validator.

use super::{
    active_key, invalid, HNSWIndex, StorageBackendResult, StorageReadControl, MAX_HNSW_LEVEL,
};
use crate::{
    hnsw_index::{
        store::{NodeRead, VectorRead},
        validation::Validation as Topology,
    },
    image_io::ImagePreparation,
    vector_index::{validate_vector_components_controlled, validate_vector_dimensions},
};

enum Stage {
    Configuration,
    Node,
    Raw,
    RawValues,
    Normalized,
    NormalizedValues,
    Counts,
    Active,
    Dirty,
    Topology,
    Complete,
}

pub(super) struct Validation<'a> {
    index: &'a HNSWIndex,
    stage: Stage,
    after: Option<u128>,
    previous: Option<(u64, u32)>,
    vector: Option<VectorRead<'a>>,
    offset: usize,
    live: usize,
    deleted: usize,
    topology: Topology<'a>,
}

pub(super) fn validate(
    index: &HNSWIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut validation = Validation::new(index);
    while !validation.advance(128, control)?.ready {}
    Ok(())
}

impl<'a> Validation<'a> {
    pub(super) fn new(index: &'a HNSWIndex) -> Self {
        Self {
            index,
            stage: Stage::Configuration,
            after: None,
            previous: None,
            vector: None,
            offset: 0,
            live: 0,
            deleted: 0,
            topology: Topology::new(index),
        }
    }

    pub(super) fn advance(
        &mut self,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImagePreparation> {
        control.check()?;
        let mut positions = 0;
        while positions < maximum && !matches!(self.stage, Stage::Complete) {
            control.check()?;
            self.step(control)?;
            positions += 1;
        }
        control.check()?;
        Ok(ImagePreparation {
            positions,
            ready: matches!(self.stage, Stage::Complete),
        })
    }

    fn step(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        match self.stage {
            Stage::Configuration => {
                self.index.params.validate()?;
                if self.index.max_level > MAX_HNSW_LEVEL {
                    return Err(invalid());
                }
                self.stage = Stage::Node;
            }
            Stage::Node => self.node(control)?,
            Stage::Raw | Stage::Normalized => {
                let normalized = matches!(self.stage, Stage::Normalized);
                let map = if normalized {
                    &self.index.normalized_vectors
                } else {
                    &self.index.raw_vectors
                };
                let vector =
                    VectorRead::open(map, self.after.expect("selected image node"), control)?
                        .ok_or_else(invalid)?;
                if vector.norm().is_nan()
                    || vector.norm() < 0.0
                    || (normalized && vector.norm().to_bits() != 1.0_f32.to_bits())
                {
                    return Err(invalid());
                }
                validate_vector_dimensions(self.index.dimensions, vector.len())?;
                self.vector = Some(vector);
                self.offset = 0;
                self.stage = if normalized {
                    Stage::NormalizedValues
                } else {
                    Stage::RawValues
                };
            }
            Stage::RawValues | Stage::NormalizedValues => {
                let vector = self.vector.as_ref().expect("selected image coordinates");
                let mut values = [0_f32; 1024];
                let count = vector.read_values(self.offset, &mut values, control)?;
                validate_vector_components_controlled(
                    &values[..count],
                    self.offset,
                    Some(control),
                )?;
                self.offset += count;
                if self.offset == vector.len() {
                    self.vector = None;
                    self.stage = if matches!(self.stage, Stage::RawValues) {
                        Stage::Normalized
                    } else {
                        Stage::Node
                    };
                }
            }
            Stage::Counts => {
                if self.live != self.index.active.len() || self.deleted != self.index.deleted_count
                {
                    return Err(invalid());
                }
                self.after = None;
                self.stage = Stage::Active;
            }
            Stage::Active => self.active(control)?,
            Stage::Dirty => {
                if let Some(key) = self
                    .index
                    .dirty_nodes
                    .next_key(self.after, control.memory())?
                {
                    if !self.index.nodes.contains_key(key)? {
                        return Err(invalid());
                    }
                    self.after = Some(key);
                } else {
                    self.stage = Stage::Topology;
                }
            }
            Stage::Topology => {
                if self.topology.advance(1, control)?.ready {
                    self.stage = Stage::Complete;
                }
            }
            Stage::Complete => (),
        }
        Ok(())
    }
    fn node(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        if let Some(id) = self.index.nodes.next_key(self.after, control.memory())? {
            let node = NodeRead::open(&self.index.nodes, id, control)?.ok_or_else(invalid)?;
            if id != u128::from(node.id)
                || node.level > MAX_HNSW_LEVEL
                || node.layer_count() != node.level + 1
            {
                return Err(invalid());
            }
            if node.deleted {
                self.deleted += 1;
            } else {
                self.live += 1;
                if self
                    .index
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
            self.after = Some(id);
            self.stage = Stage::Raw;
        } else {
            self.stage = Stage::Counts;
        }

        Ok(())
    }
    fn active(&mut self, control: &StorageReadControl) -> StorageBackendResult<()> {
        if let Some(key) = self.index.active.next_key(self.after, control.memory())? {
            if key >> 96 != 0 {
                return Err(invalid());
            }
            let document = (key >> 32) as u64;
            let ordinal = key as u32;
            let expected = match self.previous {
                Some((prior, ordinal)) if prior == document => u64::from(ordinal) + 1,
                _ => 0,
            };
            if u64::from(ordinal) != expected {
                return Err(invalid());
            }
            self.previous = Some((document, ordinal));
            self.after = Some(key);
        } else {
            self.after = None;
            self.stage = Stage::Dirty;
        }

        Ok(())
    }
}
