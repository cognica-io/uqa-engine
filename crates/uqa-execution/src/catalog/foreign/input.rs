//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored foreign declarations retain their original native fields under one input allowance.

use super::{reference, validation, StoredForeignTable};
use serde::de::Error as _;
use uqa_core::{
    json::JsonReadError,
    memory::{Budgeted, BudgetedVec, MemoryReservation, Produced, ProductionControl},
    CancellationToken,
};
use uqa_storage::{
    read_control::StorageReadControl, ForeignTableRow, StorageBackendError, StorageBackendResult,
};

mod fields;
mod schema;

impl StoredForeignTable {
    /// Decode original options and untagged schema before checking version, server/OID identities, option order and the reference fence. No registry is published and no legacy row is rewritten.
    pub fn from_catalog_row_with_control(
        row: &ForeignTableRow,
        reference_format: Option<u32>,
        control: &StorageReadControl,
        invoking: &CancellationToken,
    ) -> StorageBackendResult<Budgeted<(Self, bool)>> {
        let production = ProductionControl::new(control.memory(), control.cancellation(), invoking);
        production.check()?;
        let mut memory = control.memory().empty_reservation();
        let name = take(
            retained(row.relation.qualified_name_with_control(&production)?),
            &mut memory,
        );
        let server_name = take(
            retained(production.copy_text(&row.server_name)?),
            &mut memory,
        );
        let options = take(fields::options(&row.options_json, production)?, &mut memory);
        let schema = take(
            schema::read(&row.columns_json, control, invoking)?,
            &mut memory,
        );
        let legacy = schema.validate(&name)?;
        let option_order = match schema.option_order {
            Some(order) => order,
            None if legacy => {
                let mut order = BudgetedVec::new(control.memory());
                for key in options.keys() {
                    production.check()?;
                    order.push(take(retained(production.copy_text(key)?), &mut memory))?;
                }
                let (order, lease) = order.into_parts();
                memory.absorb(lease);
                order
            }
            None => {
                return Err(StorageBackendError::Other(format!(
                    "foreign table `{name}` has no option order"
                )))
            }
        };
        validation::option_order(
            &name,
            &option_order,
            options.len(),
            |name| options.contains_key(name),
            &production,
        )?;
        reference::validate_schema_format(reference_format, legacy, &name)?;
        let table = Self {
            name,
            persistence: uqa_sql::ast::RelationPersistence::Permanent,
            object_id: schema.object_id,
            catalog_oids: schema.catalog_oids,
            row_type_array_name: schema.row_type_array_name,
            server_name,
            server_reference: schema.server_reference,
            columns: schema.columns,
            dropped_attributes: schema.dropped_attributes,
            checks: schema.checks,
            options,
            option_order,
        };
        production.check()?;
        Ok(Budgeted::new((table, legacy), memory))
    }
}

fn retained<T>(value: Produced<T>) -> Budgeted<T> {
    value
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("controlled foreign declaration"))
}

fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, lease) = value.into_parts();
    memory.absorb(lease);
    value
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Serde(serde_json::Error::custom(
        "invalid stored foreign declaration JSON",
    ))
}

fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => invalid(),
    }
}

fn sql_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Cancelled(error) => error.into(),
        uqa_sql::SQLError::Internal(_) => invalid(),
        error => StorageBackendError::backend("stored foreign declaration", error),
    }
}

#[cfg(test)]
mod tests;
