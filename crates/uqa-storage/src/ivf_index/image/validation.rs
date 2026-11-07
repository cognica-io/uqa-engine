//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resumable source validation preserves exact assignments and counts empty list boundaries.

use super::{
    invalid, IVFData, IVFState, StorageBackendResult, StorageReadControl, StoredVector, VectorKey,
};
use crate::{
    image_io::ImagePreparation,
    vector_index::{validate_vector_components_controlled, validate_vector_dimensions},
};
use uqa_core::memory::MemoryError;

pub(super) fn validate(index: &IVFData, control: &StorageReadControl) -> StorageBackendResult<()> {
    let mut validation = Validation::new(index);
    while !validation.advance(128, control)?.ready {}
    Ok(())
}

struct Coordinates<'a> {
    values: &'a [f32],
    position: usize,
}

impl<'a> Coordinates<'a> {
    fn new(dimensions: u32, values: &'a [f32]) -> StorageBackendResult<Self> {
        validate_vector_dimensions(dimensions, values.len())?;
        Ok(Self {
            values,
            position: 0,
        })
    }

    fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<bool> {
        let length = (self.values.len() - self.position).min(1024);
        validate_vector_components_controlled(
            &self.values[self.position..self.position + length],
            self.position,
            Some(control),
        )?;
        self.position += length;
        Ok(self.position == self.values.len())
    }
}

enum Stage<'a> {
    Configuration,
    Centroid,
    CentroidValues(Coordinates<'a>),
    Vector,
    Raw(&'a StoredVector, Coordinates<'a>),
    Normalized(&'a StoredVector, Coordinates<'a>),
    Assignment(&'a StoredVector),
    List,
    Members {
        centroid: usize,
        keys: &'a [VectorKey],
        previous: Option<VectorKey>,
    },
    Complete,
}

pub(super) struct Validation<'a> {
    index: &'a IVFData,
    stage: Stage<'a>,
    centroids: std::slice::Iter<'a, Vec<f32>>,
    vectors: std::collections::btree_map::Iter<'a, VectorKey, StoredVector>,
    lists: std::iter::Enumerate<std::slice::Iter<'a, Vec<VectorKey>>>,
    previous: Option<VectorKey>,
    members: usize,
}

impl<'a> Validation<'a> {
    pub(super) fn new(index: &'a IVFData) -> Self {
        Self {
            index,
            stage: Stage::Configuration,
            centroids: index.centroids.iter(),
            vectors: index.vectors.iter(),
            lists: index.inverted_lists.iter().enumerate(),
            previous: None,
            members: 0,
        }
    }

    pub(super) fn advance(
        &mut self,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImagePreparation> {
        control.check()?;
        let mut positions = 0;
        for _ in 0..maximum {
            if matches!(self.stage, Stage::Complete) {
                break;
            }
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
        self.stage = match std::mem::replace(&mut self.stage, Stage::Complete) {
            Stage::Configuration => {
                configuration(self.index)?;
                Stage::Centroid
            }
            Stage::Centroid => match self.centroids.next() {
                Some(values) => {
                    Stage::CentroidValues(Coordinates::new(self.index.dimensions, values)?)
                }
                None => Stage::Vector,
            },
            Stage::CentroidValues(mut values) => {
                if values.advance(control)? {
                    Stage::Centroid
                } else {
                    Stage::CentroidValues(values)
                }
            }
            Stage::Vector => match self.vectors.next() {
                Some((&key, vector)) => {
                    vector_identity(key, vector, self.previous)?;
                    Stage::Raw(
                        vector,
                        Coordinates::new(self.index.dimensions, &vector.raw_vector)?,
                    )
                }
                None => Stage::List,
            },
            Stage::Raw(vector, mut values) => {
                if values.advance(control)? {
                    Stage::Normalized(
                        vector,
                        Coordinates::new(self.index.dimensions, &vector.vector)?,
                    )
                } else {
                    Stage::Raw(vector, values)
                }
            }
            Stage::Normalized(vector, mut values) => {
                if values.advance(control)? {
                    Stage::Assignment(vector)
                } else {
                    Stage::Normalized(vector, values)
                }
            }
            Stage::Assignment(vector) => {
                match (self.index.state, vector.centroid) {
                    (IVFState::Untrained, None) => {}
                    (IVFState::Trained | IVFState::Stale, Some(centroid))
                        if centroid < self.index.centroids.len() => {}
                    _ => return Err(invalid()),
                }
                self.previous = Some(vector.key);
                Stage::Vector
            }
            Stage::List => self.next_list()?,
            Stage::Members {
                centroid,
                keys,
                previous,
            } => match keys.split_first() {
                Some((&key, remaining)) => {
                    if previous.is_some_and(|previous| previous >= key)
                        || self
                            .index
                            .vectors
                            .get(&key)
                            .is_none_or(|vector| vector.centroid != Some(centroid))
                    {
                        return Err(invalid());
                    }
                    self.members = self
                        .members
                        .checked_add(1)
                        .ok_or(MemoryError::SizeOverflow)?;
                    Stage::Members {
                        centroid,
                        keys: remaining,
                        previous: Some(key),
                    }
                }
                None => Stage::List,
            },
            Stage::Complete => Stage::Complete,
        };
        Ok(())
    }

    fn next_list(&mut self) -> StorageBackendResult<Stage<'a>> {
        if let Some((centroid, keys)) = self.lists.next() {
            Ok(Stage::Members {
                centroid,
                keys,
                previous: None,
            })
        } else {
            if self.index.state != IVFState::Untrained && self.members != self.index.vectors.len() {
                return Err(invalid());
            }
            Ok(Stage::Complete)
        }
    }
}

fn configuration(index: &IVFData) -> StorageBackendResult<()> {
    if index.nlist == 0
        || index.nprobe == 0
        || index.train_threshold == 0
        || index.centroids.len() > index.nlist
        || index.inverted_lists.len() != index.centroids.len()
    {
        return Err(invalid());
    }
    match index.state {
        IVFState::Untrained => {
            if !index.centroids.is_empty()
                || index.trained_size != 0
                || index.deletes_since_train != 0
            {
                return Err(invalid());
            }
        }
        IVFState::Trained | IVFState::Stale => {
            if index.centroids.is_empty() {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn vector_identity(
    key: VectorKey,
    vector: &StoredVector,
    previous: Option<VectorKey>,
) -> StorageBackendResult<()> {
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
    Ok(())
}
