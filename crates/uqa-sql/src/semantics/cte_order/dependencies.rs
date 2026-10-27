//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable dependency ordering without repeatedly scanning every emitted declaration.

use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn order(
    dependencies: &[Vec<usize>],
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let mut remaining = ProductionVec::new(control);
    let mut readers = ProductionVec::new(control);
    let mut links = ProductionVec::new(control);
    let mut ready = Ready::new(control);
    let mut ordered = ProductionVec::new(control);
    for _ in dependencies {
        remaining.push_copy(0usize)?;
        readers.push_copy(usize::MAX)?;
    }
    for (index, dependencies) in dependencies.iter().enumerate() {
        for target in dependencies
            .iter()
            .copied()
            .filter(|target| *target != index)
        {
            control.check()?;
            links.push_copy((index, readers[target]))?;
            readers.as_mut_slice()[target] = links.len() - 1;
            remaining.as_mut_slice()[index] += 1;
        }
        if remaining[index] == 0 {
            ready.push(index)?;
        }
    }
    while let Some(index) = ready.pop()? {
        ordered.push_copy(index)?;
        let mut next = readers[index];
        while next != usize::MAX {
            control.check()?;
            let (reader, following) = links[next];
            next = following;
            remaining.as_mut_slice()[reader] -= 1;
            if remaining[reader] == 0 {
                ready.push(reader)?;
            }
        }
    }
    if ordered.len() != dependencies.len() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "mutual recursion between WITH items is not implemented".into(),
        });
    }
    Ok(ordered.finish()?)
}

/// A declaration-index min-heap retains its admitted capacity for the complete traversal.
struct Ready<'a> {
    slots: ProductionVec<'a, usize>,
    len: usize,
    control: ProductionControl<'a>,
}

impl<'a> Ready<'a> {
    fn new(control: ProductionControl<'a>) -> Self {
        Self {
            slots: ProductionVec::new(control),
            len: 0,
            control,
        }
    }

    fn push(&mut self, index: usize) -> Result<(), SQLError> {
        self.control.check()?;
        let mut position = self.len;
        if position == self.slots.len() {
            self.slots.push_copy(index)?;
        } else {
            self.slots.as_mut_slice()[position] = index;
        }
        self.len += 1;
        while position > 0 {
            self.control.check()?;
            let parent = (position - 1) / 2;
            if self.slots[parent] <= self.slots[position] {
                break;
            }
            self.slots.as_mut_slice().swap(parent, position);
            position = parent;
        }
        Ok(())
    }

    fn pop(&mut self) -> Result<Option<usize>, SQLError> {
        self.control.check()?;
        if self.len == 0 {
            return Ok(None);
        }
        let index = self.slots[0];
        self.len -= 1;
        self.slots.as_mut_slice()[0] = self.slots[self.len];
        let mut position = 0;
        while position < self.len / 2 {
            self.control.check()?;
            let left = position * 2 + 1;
            let right = left + 1;
            let child = if right < self.len && self.slots[right] < self.slots[left] {
                right
            } else {
                left
            };
            if self.slots[position] <= self.slots[child] {
                break;
            }
            self.slots.as_mut_slice().swap(position, child);
            position = child;
        }
        Ok(Some(index))
    }
}
