//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Priority queues reuse controlled vector growth and retain the buffer when transferring results.

use super::{BudgetedVec, MemoryBudget, MemoryError};

#[derive(Debug)]
pub struct BudgetedBinaryHeap<T> {
    values: BudgetedVec<T>,
}

impl<T> BudgetedBinaryHeap<T> {
    pub fn new(budget: &MemoryBudget) -> Self {
        Self {
            values: BudgetedVec::new(budget),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn peek(&self) -> Option<&T> {
        self.values.first()
    }

    pub fn reserve(&mut self, additional: usize) -> Result<(), MemoryError> {
        self.values.reserve(additional)
    }

    /// Transfer heap-order values with their allocation lease, without copying or sorting them.
    pub fn into_vec(self) -> BudgetedVec<T> {
        self.values
    }
}

impl<T: Ord> BudgetedBinaryHeap<T> {
    pub fn push(&mut self, value: T) -> Result<(), MemoryError> {
        self.values.push(value)?;
        let mut child = self.values.len() - 1;
        while child > 0 {
            let parent = (child - 1) / 2;
            if self.values[parent] >= self.values[child] {
                break;
            }
            self.values.swap(parent, child);
            child = parent;
        }
        Ok(())
    }

    pub fn pop(&mut self) -> Option<T> {
        let last = self.values.pop()?;
        if self.values.is_empty() {
            return Some(last);
        }
        let result = std::mem::replace(&mut self.values[0], last);
        let mut root = 0_usize;
        while let Some(left) = root
            .checked_mul(2)
            .and_then(|position| position.checked_add(1))
            .filter(|&position| position < self.values.len())
        {
            let right = left + 1;
            let child = if right < self.values.len() && self.values[right] > self.values[left] {
                right
            } else {
                left
            };
            if self.values[root] >= self.values[child] {
                break;
            }
            self.values.swap(root, child);
            root = child;
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests;
