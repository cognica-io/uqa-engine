//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata vector growth moves each existing field record with its payload.

use super::{Copying, RewriteMemory};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

pub(in crate::plan) struct OwnedFields<T> {
    pub value: T,
    pub memory: Option<RewriteMemory>,
}

pub(in crate::plan) struct FieldVector<T> {
    pub values: Vec<T>,
    pub memory: Option<RewriteMemory>,
}

struct Fields<T> {
    values: std::vec::IntoIter<T>,
    memory: Option<RewriteMemory>,
    index: usize,
}
impl<T> Fields<T> {
    fn take(&mut self) -> OwnedFields<T> {
        let value = self.values.next().expect("field vector item");
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| memory.item_mut(self.index).take());
        self.index += 1;
        OwnedFields { value, memory }
    }
}

impl<T> FieldVector<T> {
    pub fn insert(
        self,
        index: usize,
        incoming: OwnedFields<T>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        assert!(index <= self.values.len(), "metadata insertion index");
        let count = self
            .values
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("metadata vector count overflow".into()))?;
        let mut previous = Fields {
            values: self.values.into_iter(),
            memory: self.memory,
            index: 0,
        };
        let mut incoming = Some(incoming);
        let items = (0..count).map(|position| {
            if position == index {
                incoming.take().expect("new field")
            } else {
                previous.take()
            }
        });
        let (values, memory) = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let values = copying.map(items, Copying::take_fields)?;
            (values, Some(copying.into_rewrite_memory()))
        } else {
            (
                items
                    .map(|item| {
                        assert!(item.memory.is_none(), "ordinary field owner");
                        item.value
                    })
                    .collect(),
                None,
            )
        };
        Ok(Self { values, memory })
    }
}
