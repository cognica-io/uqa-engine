//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture stored expression keys and their eligibility without evaluating expressions again.

use super::{comparison::StoredComparison, ValueIndexCache};
use std::ops::Deref;
use uqa_core::{
    memory::{Budgeted, BudgetedString, RetainedMapSnapshot},
    DocId, Value, ValueRetentionError,
};
use uqa_storage::{
    btree_index::retained::RetainedValueIndexSnapshot, memory_capture::MemoryCaptureSource,
    read_control::StorageReadControl, StorageBackendError, StorageBackendResult,
};

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
    values: CapturedValues,
    control: StorageReadControl,
}

enum CapturedValues {
    Resident(Budgeted<RetainedMapSnapshot<DocId, Value>>),
    Spilled(Budgeted<RetainedValueIndexSnapshot>),
}

/// Resident values borrow the admitted immutable root; spilled values retain the allowance of their individual decoding.
pub enum CapturedValue<'a> {
    Borrowed(&'a Value),
    Owned(Budgeted<Value>),
}

impl Deref for CapturedValue<'_> {
    type Target = Value;
    fn deref(&self) -> &Value {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

impl ValueIndexCache {
    pub fn capture_values(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedValueIndex>> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<CapturedValueIndex>())?;
        Ok(Budgeted::new(self.freeze(control)?, memory))
    }
}

impl MemoryCaptureSource for ValueIndexCache {
    type Frozen = CapturedValueIndex;

    fn freeze(&self, control: &StorageReadControl) -> StorageBackendResult<Self::Frozen> {
        control.check()?;
        let field = copy_text(&self.field, control)?;
        let values = if let Some(retained) = &self.retained {
            CapturedValues::Spilled(retained.capture(control)?)
        } else {
            CapturedValues::Resident(self.policy.values.capture(control.memory())?)
        };
        control.check()?;
        Ok(CapturedValueIndex {
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
        })
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
        Ok(match &self.values {
            CapturedValues::Resident(values) => values.len(),
            CapturedValues::Spilled(values) => values.len()?,
        })
    }

    pub fn is_empty(&self) -> StorageBackendResult<bool> {
        Ok(self.len()? == 0)
    }

    /// Absence and an evaluated SQL NULL remain distinct. Decoding a spilled value retains its capture allowance until the returned value is dropped.
    pub fn value(&self, id: DocId) -> StorageBackendResult<Option<CapturedValue<'_>>> {
        self.control.check()?;
        Ok(match &self.values {
            CapturedValues::Resident(values) => values.get(&id).map(CapturedValue::Borrowed),
            CapturedValues::Spilled(values) => {
                values.get(id, &self.control)?.map(CapturedValue::Owned)
            }
        })
    }

    /// Visit exact stored representations in document identity order. Cancellation is observed between callbacks and after the final callback.
    pub fn visit(
        &self,
        visitor: &mut dyn FnMut(DocId, &Value) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let mut visit = |id, value: &Value| {
            self.control.check()?;
            visitor(id, value)?;
            self.control.check()
        };
        match &self.values {
            CapturedValues::Resident(values) => {
                for (&id, value) in values.iter() {
                    visit(id, value)?;
                }
            }
            CapturedValues::Spilled(values) => {
                values.visit_values_owned(&self.control, &mut |id, value| {
                    visit(id, &value)?;
                    Ok(true)
                })?;
            }
        }
        self.control.check()
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

pub(super) fn value_error(error: ValueRetentionError) -> StorageBackendError {
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
