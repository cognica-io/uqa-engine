//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use sha2::{Digest, Sha256};
use std::path::Path;
use uqa_core::memory::BudgetedVec;

use super::super::runs::{RunReader, RunWriter};
use super::super::temporary::TemporaryRun;
use super::{invalid, DiskANNBuildInput};
use crate::diskann_index::{metric, NavigationInput, PQTrainer, PQTrainingOptions};
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Clone, Copy)]
pub(super) enum Ids<'a> {
    All(u64),
    Child(&'a Split, u8),
}

impl Ids<'_> {
    pub(super) fn len(self) -> u64 {
        match self {
            Self::All(count) => count,
            Self::Child(split, label) => split.counts[usize::from(label)],
        }
    }

    pub(super) fn visit(
        self,
        control: &StorageReadControl,
        visitor: &mut dyn FnMut(u64) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        match self {
            Self::All(count) => {
                for id in 0..count {
                    control.check()?;
                    visitor(id)?;
                }
                Ok(())
            }
            Self::Child(split, label) => split.run.read(control, |file| {
                let mut reader = RunReader::new(file, control)?;
                let mut previous = None;
                let mut seen = 0;
                for _ in 0..split.rows {
                    let record = reader.record::<10>()?;
                    let id = u64::from_le_bytes(record[..8].try_into().expect("membership node"));
                    let labels = [record[8], record[9]];
                    if id >= split.nodes
                        || previous.is_some_and(|last| last >= id)
                        || labels[0] == labels[1]
                        || labels
                            .iter()
                            .any(|&label| usize::from(label) >= split.counts.len())
                    {
                        return Err(invalid("invalid partition membership record"));
                    }
                    previous = Some(id);
                    if labels.contains(&label) {
                        visitor(id)?;
                        seen += 1;
                    }
                }
                if seen != self.len() {
                    return Err(invalid("partition membership count differs"));
                }
                Ok(())
            }),
        }
    }
}

pub(super) struct Split {
    run: TemporaryRun,
    rows: u64,
    nodes: u64,
    pub(super) counts: BudgetedVec<u64>,
    pub(super) digest: [u8; 32],
}

pub(super) fn split(
    input: &DiskANNBuildInput,
    source: Ids<'_>,
    directory: &Path,
    mut options: PQTrainingOptions,
    seed: u64,
) -> StorageBackendResult<Split> {
    options.seed = seed;
    let control = &input.control;
    let mut trainer = PQTrainer::new(input.dimensions, 1, options, control)?;
    source.visit(control, &mut |id| {
        let record = input.read_node(id)?;
        let NavigationInput::Navigable(vector) =
            NavigationInput::from_raw(input.dimensions, record.raw(), control)?
        else {
            return Err(invalid("navigable partition input changed classification"));
        };
        trainer.observe(&vector)
    })?;
    let model = trainer.finish()?;
    let mut counts = BudgetedVec::new(control.memory());
    counts.reserve(usize::from(model.centroid_count()))?;
    for _ in 0..model.centroid_count() {
        counts.push(0_u64)?;
    }
    let mut writer = RunWriter::new(directory, &input.temporary, control)?;
    let mut hash = Sha256::new();
    hash.update(b"UQA DiskANN membership\0\x01");
    source.visit(control, &mut |id| {
        let record = input.read_node(id)?;
        let NavigationInput::Navigable(vector) =
            NavigationInput::from_raw(input.dimensions, record.raw(), control)?
        else {
            return Err(invalid("navigable partition input changed classification"));
        };
        let labels = nearest_two(
            vector.coordinates(),
            model.chunk_centroids(0).expect("one-chunk coarse model"),
            control,
        )?;
        let mut bytes = [0; 10];
        bytes[..8].copy_from_slice(&id.to_le_bytes());
        bytes[8..].copy_from_slice(&labels);
        writer.append(&bytes)?;
        hash.update(bytes);
        for label in labels {
            counts[usize::from(label)] += 1;
        }
        Ok(())
    })?;
    Ok(Split {
        run: writer.finish()?,
        rows: source.len(),
        nodes: input.node_count(),
        counts,
        digest: hash.finalize().into(),
    })
}

pub(super) fn nearest_two(
    coordinates: &[f64],
    centroids: &[f64],
    control: &StorageReadControl,
) -> StorageBackendResult<[u8; 2]> {
    control.check()?;
    if coordinates.is_empty()
        || !centroids.len().is_multiple_of(coordinates.len())
        || !(2..=256).contains(&(centroids.len() / coordinates.len()))
    {
        return Err(invalid("invalid coarse centroid dimensions or count"));
    }
    let mut best = [(f64::INFINITY, 0_u8); 2];
    for (label, centroid) in centroids.chunks_exact(coordinates.len()).enumerate() {
        let distance = metric::squared_distance(coordinates, centroid, control)?;
        if !distance.is_finite() {
            return Err(invalid("nonfinite coarse assignment distance"));
        }
        if distance < best[0].0 {
            best[1] = best[0];
            best[0] = (distance, label as u8);
        } else if distance < best[1].0 {
            best[1] = (distance, label as u8);
        }
    }
    Ok([best[0].1, best[1].1])
}

pub(super) fn child_seed(parent: u64, label: u8) -> u64 {
    let mut hash = Sha256::new();
    hash.update(b"UQA DiskANN partition child\0\x01");
    hash.update(parent.to_le_bytes());
    hash.update([label]);
    u64::from_le_bytes(hash.finalize()[..8].try_into().expect("eight seed bytes"))
}
