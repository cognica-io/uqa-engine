//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture stored expression keys and their eligibility without evaluating expressions again.

use super::{comparison::StoredComparison, ValueIndexCache};
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedString},
    DocId, Value, ValueRetentionError,
};
use uqa_storage::{
    memory_capture::MemoryCaptureSource, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult, ValueIndexKey,
};

/// The live collection is mutated only through its enclosing source admission owner.
#[derive(Default)]
pub struct ValueIndexes(BTreeMap<ValueIndexKey, ValueIndexCache>);

impl From<BTreeMap<ValueIndexKey, ValueIndexCache>> for ValueIndexes {
    fn from(indexes: BTreeMap<ValueIndexKey, ValueIndexCache>) -> Self {
        Self(indexes)
    }
}

impl Deref for ValueIndexes {
    type Target = BTreeMap<ValueIndexKey, ValueIndexCache>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ValueIndexes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueIndexComparison {
    Ordered,
    TypedField,
    Fallible,
}

/// Eligibility can retain observations of removed values. Recomputing it from current keys would change the selected execution path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturedValueIndexPolicy {
    pub carried: bool,
    pub has_temporal: bool,
    pub has_row_values: bool,
    pub comparison: ValueIndexComparison,
}

/// A detached, read-only evaluated key map with its original capture allowance and cancellation.
pub struct CapturedValueIndex {
    field: BudgetedString,
    policy: CapturedValueIndexPolicy,
    values: BudgetedMap<DocId, Budgeted<Value>>,
    control: StorageReadControl,
}

impl ValueIndexCache {
    pub fn capture_values(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedValueIndex>> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<CapturedValueIndex>())?;
        let field = copy_text(&self.field, control)?;
        let mut values = BudgetedMap::new(control.memory());
        let mut insert = |id, value| -> StorageBackendResult<()> {
            control.check()?;
            if values.insert(id, value)?.is_some() {
                return Err(StorageBackendError::Other(
                    "duplicate captured value-index identity".into(),
                ));
            }
            Ok(())
        };
        if let Some(retained) = &self.retained {
            retained.visit_values_owned(control, &mut |id, value| {
                insert(id, value)?;
                Ok(true)
            })?;
        } else {
            for (&id, value) in &self.policy.values {
                let value = value
                    .clone_budgeted(control.memory(), control.cancellation())
                    .map_err(value_error)?;
                insert(id, value)?;
            }
        }
        control.check()?;
        Ok(Budgeted::new(
            CapturedValueIndex {
                field,
                policy: CapturedValueIndexPolicy {
                    carried: self.is_carried(),
                    has_temporal: self.policy.has_temporal,
                    has_row_values: self.policy.has_row_values,
                    comparison: match self.policy.comparison {
                        StoredComparison::Ordered => ValueIndexComparison::Ordered,
                        StoredComparison::TypedField => ValueIndexComparison::TypedField,
                        StoredComparison::Fallible => ValueIndexComparison::Fallible,
                    },
                },
                values,
                control: control.clone(),
            },
            memory,
        ))
    }
}

impl CapturedValueIndex {
    pub fn field(&self) -> StorageBackendResult<&str> {
        self.control.check()?;
        Ok(&self.field)
    }

    pub fn policy(&self) -> StorageBackendResult<CapturedValueIndexPolicy> {
        self.control.check()?;
        Ok(self.policy)
    }

    pub fn len(&self) -> StorageBackendResult<usize> {
        self.control.check()?;
        Ok(self.values.len())
    }

    pub fn is_empty(&self) -> StorageBackendResult<bool> {
        self.control.check()?;
        Ok(self.values.is_empty())
    }

    /// Absence and an evaluated SQL NULL remain distinct. The returned borrow cannot outlive its admitted source owner.
    pub fn value(&self, id: DocId) -> StorageBackendResult<Option<&Value>> {
        self.control.check()?;
        Ok(self.values.get(&id).map(|value| &**value))
    }

    /// Visit exact stored representations in document identity order. Cancellation is observed between callbacks and after the final callback.
    pub fn visit(
        &self,
        visitor: &mut dyn FnMut(DocId, &Value) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        for (&id, value) in &self.values {
            self.control.check()?;
            visitor(id, value)?;
        }
        self.control.check()
    }
}

/// Named SQL indexes retain evaluated keys. Opportunistic column accelerators are derived caches and do not enter the image.
pub struct CapturedValueIndexes {
    indexes: BudgetedMap<ValueIndexKey, Budgeted<CapturedValueIndex>>,
    control: StorageReadControl,
}

impl MemoryCaptureSource for ValueIndexes {
    type Frozen = CapturedValueIndexes;
    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let mut indexes = BudgetedMap::new(control.memory());
        for (key, index) in &self.0 {
            if !matches!(key, ValueIndexKey::Index(_)) {
                continue;
            }
            control.check()?;
            let name = copy_text(key.name(), control)?;
            let (name, name_memory) = name.into_parts();
            let (index, mut memory) = index.capture_values(control)?.into_parts();
            memory.absorb(name_memory);
            indexes.insert(ValueIndexKey::Index(name), Budgeted::new(index, memory))?;
        }
        control.check()?;
        Ok(CapturedValueIndexes {
            indexes,
            control: control.clone(),
        })
    }
}

impl CapturedValueIndexes {
    pub fn get(&self, key: &ValueIndexKey) -> StorageBackendResult<Option<&CapturedValueIndex>> {
        self.control.check()?;
        Ok(self.indexes.get(key).map(|index| &**index))
    }

    pub fn keys(&self) -> StorageBackendResult<impl ExactSizeIterator<Item = &ValueIndexKey>> {
        self.control.check()?;
        Ok(self.indexes.iter().map(|(key, _)| key))
    }
}

fn copy_text(value: &str, control: &StorageReadControl) -> StorageBackendResult<BudgetedString> {
    let value = uqa_core::memory::ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(value)
    .map_err(value_error)?
    .into_budgeted()
    .expect("captured text admission");
    Ok(BudgetedString::from_budgeted(value))
}

fn value_error(error: ValueRetentionError) -> StorageBackendError {
    match error {
        ValueRetentionError::Memory(error) => error.into(),
        ValueRetentionError::Cancelled(error) => error.into(),
        error @ ValueRetentionError::Malformed { .. } => {
            StorageBackendError::backend("captured value-index key", error)
        }
    }
}

#[cfg(test)]
mod tests;
