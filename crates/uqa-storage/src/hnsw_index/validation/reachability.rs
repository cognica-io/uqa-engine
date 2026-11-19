//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{corrupt, HNSWIndex, NodeRead, StorageBackendResult, StorageReadControl};
use crate::spill_map::Scratch;
use uqa_core::memory::MemoryError;

// Visited node identities occupy the low 64 bits; the ordered queue occupies a disjoint namespace in the same bounded point workspace.
const QUEUE: u128 = 1 << 64;

pub(super) struct Reachability<'a> {
    index: &'a HNSWIndex,
    scratch: Option<Scratch>,
    current: Option<NodeRead<'a>>,
    offset: usize,
    head: u64,
    tail: u64,
}

impl<'a> Reachability<'a> {
    pub(super) fn new(index: &'a HNSWIndex) -> Self {
        Self {
            index,
            scratch: None,
            current: None,
            offset: 0,
            head: 0,
            tail: 0,
        }
    }

    /// One transition initializes the queue, opens one selected node, or visits one layer-zero edge. Deleted nodes participate exactly like live nodes.
    pub(super) fn advance(&mut self, control: &StorageReadControl) -> StorageBackendResult<bool> {
        let Some(entry_point) = self.index.entry_point else {
            return Ok(true);
        };
        let Some(scratch) = &mut self.scratch else {
            let mut scratch = Scratch::new(control)?;
            scratch.insert(u128::from(entry_point), 0)?;
            scratch.insert(QUEUE, entry_point)?;
            self.scratch = Some(scratch);
            self.tail = 1;
            return Ok(false);
        };
        if let Some(node) = &self.current {
            if self.offset == node.neighbor_count(0)? {
                self.current = None;
                self.head += 1;
            } else {
                let mut neighbor = [0];
                node.read_neighbors(0, self.offset, &mut neighbor, control)?;
                if scratch.get(u128::from(neighbor[0]))?.is_none() {
                    let next = self.tail.checked_add(1).ok_or(MemoryError::SizeOverflow)?;
                    scratch.insert(u128::from(neighbor[0]), 0)?;
                    scratch.insert(QUEUE | u128::from(self.tail), neighbor[0])?;
                    self.tail = next;
                }
                self.offset += 1;
            }
        } else if self.head == self.tail {
            if u128::from(self.tail) != self.index.nodes.len() as u128 {
                return Err(corrupt(&format!(
                    "layer-zero graph reaches {} of {} nodes from entry point {entry_point}",
                    self.tail,
                    self.index.nodes.len()
                )));
            }
            return Ok(true);
        } else {
            let id = scratch
                .get(QUEUE | u128::from(self.head))?
                .ok_or_else(|| corrupt("reachability queue entry disappeared"))?;
            self.current = Some(
                NodeRead::open(&self.index.nodes, u128::from(id), control)?
                    .ok_or_else(|| corrupt("reachability references missing node"))?,
            );
            self.offset = 0;
        }
        Ok(false)
    }
}
