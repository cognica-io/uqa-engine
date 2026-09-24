//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Container staging keeps per-value leases so overwritten fields release their payload promptly.

use std::collections::{btree_map::Entry, BTreeMap};

use crate::{
    json::JsonReadError,
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken, Value,
};

pub(super) struct ArrayBuffer {
    values: BudgetedVec<Value>,
    memory: MemoryReservation,
}

impl ArrayBuffer {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: BudgetedVec::new(memory),
            memory: memory.empty_reservation(),
        }
    }

    pub(super) fn push(&mut self, value: Budgeted<Value>) -> Result<(), JsonReadError> {
        self.values.reserve(1)?;
        let (value, memory) = value.into_parts();
        self.values.push(value)?;
        self.memory.absorb(memory);
        Ok(())
    }

    pub(super) fn finish(mut self) -> Budgeted<Value> {
        let (values, memory) = self.values.into_parts();
        self.memory.absorb(memory);
        Budgeted::new(Value::List(values), self.memory)
    }
}

pub(super) struct FieldBuffer {
    fields: BTreeMap<String, Budgeted<Value>>,
    key: Option<Budgeted<String>>,
    memory: MemoryReservation,
}

struct Fields {
    values: BTreeMap<String, Value>,
    memory: MemoryReservation,
}

impl FieldBuffer {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            fields: BTreeMap::new(),
            key: None,
            memory: memory.empty_reservation(),
        }
    }

    pub(super) fn key(&mut self, key: Budgeted<String>) {
        debug_assert!(self.key.is_none());
        self.key = Some(key);
    }

    pub(super) fn push(&mut self, value: Budgeted<Value>) -> Result<(), JsonReadError> {
        let (key, mut memory) = self
            .key
            .take()
            .expect("object value follows its key")
            .into_parts();
        match self.fields.entry(key) {
            Entry::Occupied(mut entry) => {
                entry.insert(value);
            }
            Entry::Vacant(entry) => {
                memory.grow(size_of::<(String, Budgeted<Value>)>())?;
                entry.insert(value);
                self.memory.absorb(memory);
            }
        }
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        cancellation: &CancellationToken,
    ) -> Result<Budgeted<BTreeMap<String, Value>>, JsonReadError> {
        let mut output = Fields {
            values: BTreeMap::new(),
            memory: self.memory.budget().empty_reservation(),
        };
        while let Some((name, value)) = self.fields.pop_first() {
            cancellation.check()?;
            output.memory.grow(size_of::<(String, Value)>())?;
            output.memory.absorb(self.memory.split(name.capacity()));
            let (value, memory) = value.into_parts();
            output.memory.absorb(memory);
            output.values.insert(name, value);
            drop(self.memory.split(size_of::<(String, Budgeted<Value>)>()));
        }
        Ok(Budgeted::new(output.values, output.memory))
    }
}
