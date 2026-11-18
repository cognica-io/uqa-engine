//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Freezing and retirement reuse child slots for ancestor links without allocation or recursion.

use super::{absorb_leaf, MemoryReservation, ScalarMemory};

pub(in crate::plan) struct Folding {
    current: Option<ScalarMemory>,
    pending: Option<ScalarMemory>,
    retained: MemoryReservation,
    ascending: bool,
}

impl Folding {
    pub(in crate::plan) fn new(memory: ScalarMemory) -> Self {
        let retained = memory.direct.budget().empty_reservation();
        Self {
            current: Some(memory),
            pending: None,
            retained,
            ascending: false,
        }
    }

    /// Detach at most one descendant or retire at most one completed metadata node.
    pub(in crate::plan) fn advance(&mut self) -> bool {
        if !self.ascending {
            if let Some(node) = self
                .current
                .as_mut()
                .filter(|node| !node.children.is_empty())
            {
                self.pending = node.children.replace(0, self.pending.take());
                std::mem::swap(&mut self.current, &mut self.pending);
                return false;
            }
            absorb_leaf(self.current.take(), &mut self.retained);
            self.ascending = true;
        } else if let Some(parent) = self.pending.as_mut() {
            if parent.children.len() > 1 {
                self.current = parent.children.pop().expect("remaining scalar owner");
                self.ascending = false;
            } else {
                let previous = if parent.children.is_empty() {
                    None
                } else {
                    parent.children.take(0)
                };
                absorb_leaf(
                    std::mem::replace(&mut self.pending, previous),
                    &mut self.retained,
                );
            }
        }
        self.is_complete()
    }

    pub(in crate::plan) fn is_complete(&self) -> bool {
        self.current.is_none() && self.pending.is_none()
    }

    pub(in crate::plan) fn finish(self) -> MemoryReservation {
        assert!(self.is_complete());
        self.retained
    }
}
