//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Child slots maintain their payload total at transfer boundaries; buffer admission stays with the existing scalar owner.

use super::ScalarMemory;

#[derive(Debug, Default)]
pub(super) struct ChildMemory {
    values: Vec<Option<ScalarMemory>>,
    bytes: usize,
}

impl ChildMemory {
    pub(super) fn reserved_bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn add_bytes(&mut self, bytes: usize) {
        self.bytes += bytes;
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.values.capacity()
    }

    pub(super) fn take_buffer(&mut self) -> Vec<Option<ScalarMemory>> {
        std::mem::take(&mut self.values)
    }

    pub(super) fn restore_buffer(&mut self, values: Vec<Option<ScalarMemory>>) {
        self.values = values;
    }

    pub(super) fn replace(
        &mut self,
        index: usize,
        value: Option<ScalarMemory>,
    ) -> Option<ScalarMemory> {
        let bytes = value.as_ref().map_or(0, ScalarMemory::reserved_bytes);
        let previous = std::mem::replace(&mut self.values[index], value);
        self.bytes -= previous.as_ref().map_or(0, ScalarMemory::reserved_bytes);
        self.bytes += bytes;
        previous
    }

    pub(super) fn take(&mut self, index: usize) -> Option<ScalarMemory> {
        self.replace(index, None)
    }

    pub(super) fn pop(&mut self) -> Option<ScalarMemory> {
        let value = self.values.pop().expect("remaining scalar child slot");
        self.bytes -= value.as_ref().map_or(0, ScalarMemory::reserved_bytes);
        value
    }

    pub(super) fn truncate(&mut self, len: usize) {
        while self.values.len() > len {
            self.pop();
        }
    }

    pub(super) fn swap(&mut self, left: usize, right: usize) {
        self.values.swap(left, right);
    }

    pub(super) fn rotate_left(&mut self, count: usize) {
        self.values.rotate_left(count);
    }

    pub(super) fn rotate_right(&mut self, count: usize) {
        self.values.rotate_right(count);
    }

    pub(super) fn rotate_suffix_left(&mut self, start: usize, count: usize) {
        self.values[start..].rotate_left(count);
    }
}

impl std::ops::Deref for ChildMemory {
    type Target = [Option<ScalarMemory>];

    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
