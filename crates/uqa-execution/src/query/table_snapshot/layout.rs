//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column-incarnation mapping distinguishes retained base rows from current private rows.

use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedString, BudgetedVec, MemoryReservation},
    Value,
};
use uqa_sql::{ast::ColumnDef, schema::retention::RetainedColumns, SQLError};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult, StoredDocument};

mod generated;
mod projection;
pub(super) use projection::RowProjection;

pub(super) struct RowLayout {
    columns: RetainedColumns,
    source: BudgetedVec<(String, Option<String>)>,
    _names: MemoryReservation,
    control: StorageReadControl,
}

impl RowLayout {
    pub(super) fn new(
        source: &[ColumnDef],
        columns: RetainedColumns,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut by_id = BudgetedMap::new(control.memory());
        for column in columns.iter() {
            control.check()?;
            if let Some(id) = column.object_id {
                by_id.insert(id, column)?;
            }
        }
        let mut names = control.memory().empty_reservation();
        let mut mapped = BudgetedVec::new(control.memory());
        for column in source {
            control.check()?;
            let target = column
                .object_id
                .and_then(|id| by_id.get(&id).copied())
                .or_else(|| {
                    columns.iter().find(|target| {
                        target.name == column.name
                            && (column.object_id.is_none() || target.object_id.is_none())
                    })
                });
            mapped.reserve(1)?;
            let source = copy_name(&column.name, control, &mut names)?;
            let target = target
                .map(|target| copy_name(&target.name, control, &mut names))
                .transpose()?;
            mapped.push((source, target))?;
        }
        drop(by_id);
        control.check()?;
        Ok(Self {
            columns,
            source: mapped,
            _names: names,
            control: control.clone(),
        })
    }

    pub(super) fn adapt_base(
        &self,
        document: StoredDocument,
        memory: &mut MemoryReservation,
    ) -> Result<StoredDocument, SQLError> {
        let document = self.remap_base(document, memory)?;
        self.complete_private(document, memory)
    }

    fn remap_base(
        &self,
        mut document: StoredDocument,
        memory: &mut MemoryReservation,
    ) -> Result<StoredDocument, SQLError> {
        self.control.cancellation().check()?;
        // Remove every changed source slot before installing targets so rename chains and name reuse cannot overwrite another column incarnation.
        let fields = document.fields_mut();
        let mut moved = BudgetedVec::new(self.control.memory());
        for (source, target) in self.source.iter() {
            self.control.cancellation().check()?;
            if target.as_deref() == Some(source.as_str()) {
                continue;
            }
            let value = fields.remove(source);
            if let (Some(target), Some(value)) = (target, value) {
                moved
                    .push((target, value))
                    .map_err(|error| super::snapshot_error("row adaptation", &error.into()))?;
            }
        }
        let (moved, _memory) = moved.into_parts();
        for (target, value) in moved {
            self.control.cancellation().check()?;
            memory
                .grow(size_of::<(String, Value)>())
                .map_err(|error| super::snapshot_error("row adaptation", &error.into()))?;
            let target = copy_name(target, &self.control, memory)
                .map_err(|error| super::snapshot_error("row adaptation", &error))?;
            fields.insert(target, value);
        }
        Ok(document)
    }

    pub(super) fn complete_private(
        &self,
        document: StoredDocument,
        memory: &mut MemoryReservation,
    ) -> Result<StoredDocument, SQLError> {
        let mut document = self.complete_defaults(document, memory)?;
        crate::query::generated::materialize_missing_generated_columns(
            &self.columns,
            document.fields_mut(),
        )?;
        self.control.cancellation().check()?;
        Ok(document)
    }

    fn complete_defaults(
        &self,
        mut document: StoredDocument,
        memory: &mut MemoryReservation,
    ) -> Result<StoredDocument, SQLError> {
        self.control.cancellation().check()?;
        let fields = document.fields_mut();
        for column in self.columns.iter() {
            self.control.cancellation().check()?;
            if column.generated.is_none() && !fields.contains_key(&column.name) {
                memory
                    .grow(size_of::<(String, Value)>())
                    .map_err(|error| super::snapshot_error("row defaults", &error.into()))?;
                let name = copy_name(&column.name, &self.control, memory)
                    .map_err(|error| super::snapshot_error("row defaults", &error))?;
                let copied = self
                    .copy_default(column.missing_value.as_ref())
                    .map_err(|error| super::snapshot_error("row defaults", &error))?;
                let (value, value_memory) = copied.into_parts();
                fields.insert(name, value);
                memory.absorb(value_memory);
            }
        }
        self.control.cancellation().check()?;
        Ok(document)
    }

    fn copy_default(&self, value: Option<&Value>) -> StorageBackendResult<Budgeted<Value>> {
        value
            .unwrap_or(&Value::Null)
            .clone_budgeted(self.control.memory(), self.control.cancellation())
            .map_err(|error| match error {
                uqa_core::ValueRetentionError::Memory(error) => error.into(),
                uqa_core::ValueRetentionError::Cancelled(error) => error.into(),
            })
    }
}

fn copy_name(
    name: &str,
    control: &StorageReadControl,
    memory: &mut MemoryReservation,
) -> StorageBackendResult<String> {
    control.check()?;
    let mut copied = BudgetedString::new(control.memory());
    copied.push_str(name)?;
    let (copied, reservation) = copied.into_parts();
    memory.absorb(reservation);
    Ok(copied)
}
