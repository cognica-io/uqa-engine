//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline scratch uses the existing admitted buffer owner when it spills.

use super::ProductionControl;
use crate::{
    memory::{BudgetedSmallVec, MemoryError},
    ValueRetentionError,
};
use smallvec::{Array, SmallVec};

enum Buffer<A: Array> {
    Ordinary(SmallVec<A>),
    Controlled(BudgetedSmallVec<A>),
}

/// Copy-only inline scratch; every heap allocation remains with its existing reservation owner. Cancellation during movement can leave partial scratch contents; discard the scratch after an error.
pub struct ProductionSmallVec<'a, A: Array>
where
    A::Item: Copy,
{
    buffer: Buffer<A>,
    control: ProductionControl<'a>,
    start: usize,
}

impl<'a, A: Array> ProductionSmallVec<'a, A>
where
    A::Item: Copy,
{
    pub fn new(control: ProductionControl<'a>) -> Self {
        let buffer = control.budget().map_or_else(
            || Buffer::Ordinary(SmallVec::new()),
            |budget| Buffer::Controlled(BudgetedSmallVec::new(budget)),
        );
        Self {
            buffer,
            control,
            start: 0,
        }
    }

    pub fn push(&mut self, value: A::Item) -> Result<(), ValueRetentionError> {
        self.control.check()?;
        let full = match &self.buffer {
            Buffer::Ordinary(values) => values.len() == values.capacity(),
            Buffer::Controlled(values) => values.len() == values.capacity(),
        };
        if full && self.start != 0 {
            self.compact()?;
        }
        match &mut self.buffer {
            Buffer::Ordinary(values) => {
                if values.len() == values.capacity() {
                    let capacity = values
                        .len()
                        .checked_add(1)
                        .and_then(|n| n.checked_mul(2))
                        .ok_or(MemoryError::SizeOverflow)?;
                    let mut replacement = Vec::new();
                    replacement
                        .try_reserve_exact(capacity)
                        .map_err(MemoryError::from)?;
                    for chunk in values.chunks((4096 / size_of::<A::Item>().max(1)).max(1)) {
                        self.control.check()?;
                        replacement.extend_from_slice(chunk);
                    }
                    *values = SmallVec::from_vec(replacement);
                }
                values.push(value);
            }
            Buffer::Controlled(values) => values.push(value)?,
        }
        self.control.check()
    }

    pub fn pop(&mut self) -> Option<A::Item> {
        if self.is_empty() {
            return None;
        }
        let value = match &mut self.buffer {
            Buffer::Ordinary(values) => values.pop(),
            Buffer::Controlled(values) => values.pop(),
        };
        if self.is_empty() {
            self.clear();
        }
        value
    }

    pub fn clear(&mut self) {
        self.start = 0;
        match &mut self.buffer {
            Buffer::Ordinary(values) => values.clear(),
            Buffer::Controlled(values) => values.clear(),
        }
    }

    pub fn insert(&mut self, index: usize, value: A::Item) -> Result<(), ValueRetentionError> {
        assert!(index <= self.len());
        self.control.check()?;
        if index == 0 && self.start != 0 {
            self.start -= 1;
            self[0] = value;
            return self.control.check();
        }
        self.push(value)?;
        for position in (index + 1..self.len()).rev() {
            self.control.check()?;
            self.swap(position - 1, position);
        }
        self.control.check()
    }

    pub fn remove(&mut self, index: usize) -> Result<A::Item, ValueRetentionError> {
        self.control.check()?;
        let value = self[index];
        if index == 0 {
            self.remove_prefix(1)?;
            return Ok(value);
        }
        for position in index + 1..self.len() {
            self.control.check()?;
            self.swap(position - 1, position);
        }
        match &mut self.buffer {
            Buffer::Ordinary(values) => {
                values.pop();
            }
            Buffer::Controlled(values) => {
                values.pop();
            }
        }
        self.control.check()?;
        Ok(value)
    }

    /// Consume an observed prefix without shifting the retained suffix. Later growth compacts it once before allocating more capacity.
    pub fn remove_prefix(&mut self, count: usize) -> Result<(), ValueRetentionError> {
        assert!(count <= self.len());
        self.control.check()?;
        self.start += count;
        if self.is_empty() {
            self.clear();
        }
        self.control.check()
    }

    fn compact(&mut self) -> Result<(), ValueRetentionError> {
        let start = self.start;
        let remaining = self.len();
        let values: &mut [A::Item] = match &mut self.buffer {
            Buffer::Ordinary(values) => values,
            Buffer::Controlled(values) => values,
        };
        for position in 0..remaining {
            self.control.check()?;
            values[position] = values[position + start];
        }
        match &mut self.buffer {
            Buffer::Ordinary(values) => values.truncate(remaining),
            Buffer::Controlled(values) => values.truncate(remaining),
        }
        self.start = 0;
        self.control.check()
    }
}

impl<A: Array> std::ops::Deref for ProductionSmallVec<'_, A>
where
    A::Item: Copy,
{
    type Target = [A::Item];
    fn deref(&self) -> &Self::Target {
        match &self.buffer {
            Buffer::Ordinary(values) => &values[self.start..],
            Buffer::Controlled(values) => &values[self.start..],
        }
    }
}

impl<A: Array> std::ops::DerefMut for ProductionSmallVec<'_, A>
where
    A::Item: Copy,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.buffer {
            Buffer::Ordinary(values) => &mut values[self.start..],
            Buffer::Controlled(values) => &mut values[self.start..],
        }
    }
}

#[cfg(test)]
mod tests;
