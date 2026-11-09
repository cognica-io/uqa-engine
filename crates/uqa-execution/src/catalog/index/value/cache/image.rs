//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated image input preserves historical policy and original admitted value owners.

use super::super::{identities::NullIdentities, values::StoredValues};
use super::{
    capture::value_error, comparison::StoredComparison, storage, value_bytes,
    CapturedValueIndexPolicy, ColumnValueIndex, ValueIndexCache, ValueIndexComparison,
    ValueIndexImageMetadata,
};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError, ProductionControl, RetainedMap},
    DocId, Value,
};
use uqa_sql::{
    catalog::table_metadata::{SharedTableMetadata, TableMetadata},
    SQLError,
};
use uqa_storage::{
    btree_index::BTreeIndexImageBuilder, read_control::StorageReadControl, StorageBackendError,
};

/// Unpublished native keys in increasing document-identity order. A rejected addition consumes the complete partial owner. Completion retains original input reservations without retaining input cancellation; the ordinary cache threshold governs later mutations.
pub struct ValueIndexImageBuilder {
    field: SharedTableMetadata<String>,
    policy: CapturedValueIndexPolicy,
    expected: usize,
    previous: Option<DocId>,
    values: RetainedMap<DocId, Budgeted<Value>>,
    index: Option<BTreeIndexImageBuilder>,
    nulls: BudgetedVec<DocId>,
    bytes: usize,
    limit: usize,
    control: StorageReadControl,
}

impl ValueIndexImageBuilder {
    pub fn new(
        metadata: ValueIndexImageMetadata,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        control.check().map_err(storage)?;
        let (field, memory) = metadata.field.into_parts();
        let field = Budgeted::new(field, memory);
        if !field.budget().shares_allowance(control.memory())
            || field.reserved_bytes() < field.capacity()
        {
            return Err(invalid(
                "evaluated index field belongs to another input allowance",
            ));
        }
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let index = if metadata.policy.carried {
            None
        } else {
            let field = production
                .copy_text(&field)
                .map_err(|error| storage(value_error(error)))?
                .into_budgeted()
                .expect("admitted index field");
            Some(BTreeIndexImageBuilder::new(field, control).map_err(storage)?)
        };
        let field = TableMetadata::from_admitted(field, control.cancellation())?.snapshot();
        Ok(Self {
            field,
            policy: metadata.policy,
            expected: metadata.len,
            previous: None,
            values: RetainedMap::new(control.memory()),
            index,
            nulls: BudgetedVec::new(control.memory()),
            bytes: 0,
            limit,
            control: control.clone(),
        })
    }

    pub fn add_value(mut self, id: DocId, value: Budgeted<Value>) -> Result<Self, SQLError> {
        self.control.check().map_err(storage)?;
        if self.values.len() == self.expected
            || self.previous.is_some_and(|previous| previous >= id)
        {
            return Err(invalid(
                "evaluated image has excess, duplicate or unordered identities",
            ));
        }
        if !value.budget().shares_allowance(self.control.memory()) {
            return Err(invalid(
                "evaluated image value belongs to another input allowance",
            ));
        }
        let payload = value
            .retained_payload_bytes(self.control.memory(), self.control.cancellation())
            .map_err(|error| storage(value_error(error)))?;
        if value.reserved_bytes() < payload {
            return Err(invalid(
                "evaluated image value has no complete input admission",
            ));
        }
        validate_policy(self.policy, &value)?;
        self.bytes = self
            .bytes
            .checked_add(value_bytes(&value, &self.control)?)
            .ok_or_else(|| storage(MemoryError::SizeOverflow.into()))?;
        if let Some(index) = self.index.take() {
            let index = if matches!(*value, Value::Null) {
                self.nulls.push(id).map_err(|error| storage(error.into()))?;
                index
            } else {
                let production = ProductionControl::new(
                    self.control.memory(),
                    self.control.cancellation(),
                    self.control.cancellation(),
                );
                let key = production
                    .copy_value(&value)
                    .map_err(|error| storage(value_error(error)))?
                    .into_budgeted()
                    .expect("admitted physical key");
                index.add_value(id, key).map_err(storage)?
            };
            self.index = Some(index);
        }
        self.values
            .insert(id, value, payload)
            .map_err(|error| storage(error.into()))?;
        self.previous = Some(id);
        self.control.check().map_err(storage)?;
        Ok(self)
    }

    pub fn finish(self) -> Result<ValueIndexCache, SQLError> {
        self.control.check().map_err(storage)?;
        if self.values.len() != self.expected {
            return Err(invalid("evaluated image is missing declared values"));
        }
        let (nulls, memory) = self.nulls.into_parts();
        let nulls = Budgeted::new(nulls, memory)
            .into_shared()
            .map_err(|error| storage(error.into()))?;
        Ok(ValueIndexCache {
            policy: ColumnValueIndex {
                index: self
                    .index
                    .map(BTreeIndexImageBuilder::finish)
                    .transpose()
                    .map_err(storage)?,
                values: StoredValues::from_admitted(self.values),
                nulls: NullIdentities::Admitted(nulls),
                has_temporal: self.policy.has_temporal,
                has_row_values: self.policy.has_row_values,
                comparison: match self.policy.comparison {
                    ValueIndexComparison::Ordered => StoredComparison::Ordered,
                    ValueIndexComparison::TypedField => StoredComparison::TypedField,
                    ValueIndexComparison::Fallible => StoredComparison::Fallible,
                },
            },
            retained: None,
            field: self.field,
            limit: self.limit,
            resident_bytes: self.bytes,
        })
    }
}

fn validate_policy(policy: CapturedValueIndexPolicy, value: &Value) -> Result<(), SQLError> {
    // Carried values have never populated physical key eligibility. Preserve their stored policy without inferring comparison requirements from those values.
    if policy.carried {
        return Ok(());
    }
    let temporal = super::super::value_is_temporal(value);
    let row = super::super::value_is_row(value);
    let fallible = uqa_sql::expr::value_comparison_can_fail(value);
    let typed = super::comparison::field_needs_typed_comparison(value);
    if (temporal && !policy.has_temporal)
        || (row && !policy.has_row_values)
        || (fallible && policy.comparison != ValueIndexComparison::Fallible)
        || (typed && policy.comparison == ValueIndexComparison::Ordered)
    {
        return Err(invalid(
            "evaluated image policy contradicts its stored values",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> SQLError {
    storage(StorageBackendError::Other(message.into()))
}

#[cfg(test)]
mod tests;
