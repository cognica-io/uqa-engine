//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent catalog captures fund shared role payloads as well as the native query owner.

use super::{empty_query, Input, Kind, Node, Owner, QueryPlan, SQLError};
use std::ops::{Deref, DerefMut};
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation};

#[derive(Debug)]
pub(in super::super) struct PlanMemory {
    payload: MemoryReservation,
    retained_children: usize,
}

impl PlanMemory {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            payload: memory.empty_reservation(),
            retained_children: 0,
        }
    }

    pub(super) fn retain_child(&mut self, bytes: usize) -> Result<(), MemoryError> {
        self.retained_children = self
            .retained_children
            .checked_add(bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        Ok(())
    }
}

impl Deref for PlanMemory {
    type Target = MemoryReservation;
    fn deref(&self) -> &Self::Target {
        &self.payload
    }
}
impl DerefMut for PlanMemory {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.payload
    }
}

impl Owner {
    pub(in super::super) fn retained_bytes(&self) -> Result<usize, MemoryError> {
        self.frames
            .capacity()
            .checked_mul(size_of::<super::Frame>())
            .and_then(|bytes| bytes.checked_add(self.memory.bytes()))
            .and_then(|bytes| bytes.checked_add(self.memory.retained_children))
            .ok_or(MemoryError::SizeOverflow)
    }

    /// Transfer only between an immutable catalog definition and its retained native retirement owner.
    pub(in super::super) fn exchange_query(&mut self, query: &mut QueryPlan) {
        match &mut self.root {
            Node::Query(value) => std::mem::swap(value, query),
            _ => unreachable!("query retirement owner"),
        }
    }
}

impl Input<'_> {
    pub(in super::super) fn stored_query(&mut self) -> Result<(QueryPlan, Owner), SQLError> {
        let mut owner = self.native_plan(Kind::Query)?;
        let mut query = empty_query();
        owner.exchange_query(&mut query);
        Ok((query, owner))
    }
}
