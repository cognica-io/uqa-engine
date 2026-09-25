//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cmp::Ordering;
use std::path::Path;
use uqa_core::memory::{BudgetedVec, MemoryError};

use super::super::runs::{RunReader, RunWriter};
use super::super::temporary::{File, TemporaryRun};
use super::{invalid, navigation, DiskANNBuildInput};
use crate::{read_control::StorageReadControl, StorageBackendResult};

const WIDTH: u64 = 24;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Candidate {
    pub(super) source: u64,
    pub(super) neighbor: u64,
    pub(super) distance: f64,
}

impl Candidate {
    fn compare(&self, other: &Self) -> Ordering {
        self.source
            .cmp(&other.source)
            .then_with(|| self.distance.total_cmp(&other.distance))
            .then_with(|| self.neighbor.cmp(&other.neighbor))
    }

    fn encode(self) -> [u8; WIDTH as usize] {
        let mut bytes = [0; WIDTH as usize];
        bytes[..8].copy_from_slice(&self.source.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.neighbor.to_le_bytes());
        bytes[16..].copy_from_slice(&self.distance.to_bits().to_le_bytes());
        bytes
    }
}

/// A fixed record range, with an independently positioned and block-aligned cipher reader.
pub(super) struct Range<'a> {
    reader: RunReader<'a>,
    remaining: u64,
    nodes: u64,
    previous: Option<Candidate>,
}

impl<'a> Range<'a> {
    pub(super) fn new(
        file: &'a mut File,
        first: u64,
        count: u64,
        nodes: u64,
        control: &'a StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let offset = first.checked_mul(WIDTH).ok_or(MemoryError::SizeOverflow)?;
        Ok(Self {
            reader: RunReader::at(file, offset, control)?,
            remaining: count,
            nodes,
            previous: None,
        })
    }

    pub(super) fn next(&mut self) -> StorageBackendResult<Option<Candidate>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let bytes = self.reader.record::<{ WIDTH as usize }>()?;
        let candidate = Candidate {
            source: u64::from_le_bytes(bytes[..8].try_into().expect("source")),
            neighbor: u64::from_le_bytes(bytes[8..16].try_into().expect("neighbor")),
            distance: f64::from_bits(u64::from_le_bytes(
                bytes[16..].try_into().expect("distance"),
            )),
        };
        if candidate.source >= self.nodes
            || candidate.neighbor >= self.nodes
            || candidate.source == candidate.neighbor
            || !candidate.distance.is_finite()
            || candidate.distance.is_sign_negative()
            || self
                .previous
                .is_some_and(|previous| previous.compare(&candidate).is_gt())
        {
            return Err(invalid("invalid or unordered global edge candidate"));
        }
        self.remaining -= 1;
        self.previous = Some(candidate);
        Ok(Some(candidate))
    }

    fn reset(&mut self, first: u64, count: u64) -> StorageBackendResult<()> {
        self.reader
            .seek_to(first.checked_mul(WIDTH).ok_or(MemoryError::SizeOverflow)?)?;
        self.remaining = count;
        self.previous = None;
        Ok(())
    }
}

pub(super) struct Sorted {
    pub(super) run: TemporaryRun,
    pub(super) count: u64,
    pub(super) passes: u8,
}

/// Sort fixed-width weighted candidates without a resident run directory or complete source neighborhood.
pub(super) fn sort(
    input: &DiskANNBuildInput,
    edges: TemporaryRun,
    count: u64,
    directory: &Path,
    capacity: usize,
) -> StorageBackendResult<Sorted> {
    let control = &input.control;
    let mut buffer = BudgetedVec::new(control.memory());
    buffer.reserve(count.min(capacity as u64) as usize)?;
    let mut writer = RunWriter::new(directory, &input.temporary, control)?;
    edges.read(control, |file| {
        if count == 0 {
            return Ok(());
        }
        let mut reader = RunReader::new(file, control)?;
        let mut source_vector = None;
        for _ in 0..count {
            let bytes = reader.record::<16>()?;
            let source = u64::from_le_bytes(bytes[..8].try_into().expect("source"));
            let neighbor = u64::from_le_bytes(bytes[8..].try_into().expect("neighbor"));
            if source >= input.node_count() || neighbor >= input.node_count() || source == neighbor
            {
                return Err(invalid("invalid global edge input"));
            }
            if source_vector.as_ref().is_none_or(|(id, _)| *id != source) {
                // Release the previous coordinates before admitting their replacement.
                drop(source_vector.take());
                source_vector = Some((source, navigation(input, source)?));
            }
            let vector = navigation(input, neighbor)?;
            let distance = source_vector
                .as_ref()
                .expect("selected source")
                .1
                .squared_distance(&vector, control)?
                .get();
            buffer.push(Candidate {
                source,
                neighbor,
                distance,
            })?;
            if buffer.len() == capacity {
                flush(&mut buffer, &mut writer, control)?;
            }
        }
        Ok(())
    })?;
    flush(&mut buffer, &mut writer, control)?;
    drop(buffer);
    drop(edges);
    let mut run = writer.finish()?;
    let mut width = capacity as u64;
    let mut passes = 0;
    while width < count {
        let merged = merge_pass(input, &run, count, width, directory)?;
        run = merged;
        width = width.saturating_mul(2).min(count);
        passes += 1;
    }
    Ok(Sorted { run, count, passes })
}

fn flush(
    buffer: &mut BudgetedVec<Candidate>,
    writer: &mut RunWriter,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    buffer.sort_unstable_by(Candidate::compare);
    for candidate in &**buffer {
        control.check()?;
        writer.append(&candidate.encode())?;
    }
    buffer.clear();
    Ok(())
}

pub(super) fn merge_pass(
    input: &DiskANNBuildInput,
    run: &TemporaryRun,
    count: u64,
    width: u64,
    directory: &Path,
) -> StorageBackendResult<TemporaryRun> {
    let control = &input.control;
    let mut writer = RunWriter::new(directory, &input.temporary, control)?;
    run.read(control, |left_file| {
        run.read(control, |right_file| {
            let mut left = Range::new(left_file, 0, 0, input.node_count(), control)?;
            let mut right = Range::new(right_file, 0, 0, input.node_count(), control)?;
            let mut first = 0;
            while first < count {
                control.check()?;
                let left_count = width.min(count - first);
                let right_first = first + left_count;
                let right_count = width.min(count - right_first);
                left.reset(first, left_count)?;
                right.reset(right_first, right_count)?;
                let mut a = left.next()?;
                let mut b = right.next()?;
                while a.is_some() || b.is_some() {
                    control.check()?;
                    let take_left = match (a, b) {
                        (Some(a), Some(b)) => a.compare(&b).is_le(),
                        (Some(_), None) => true,
                        _ => false,
                    };
                    let candidate = if take_left {
                        let current = a.take().expect("left head");
                        a = left.next()?;
                        current
                    } else {
                        let current = b.take().expect("right head");
                        b = right.next()?;
                        current
                    };
                    writer.append(&candidate.encode())?;
                }
                first = right_first + right_count;
            }
            Ok(())
        })
    })?;
    writer.finish()
}
