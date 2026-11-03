//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign table capture owns definitions and bindings, never remote rows or host callbacks.

use super::{ForeignServerReference, StoredForeignTable};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, RelationPersistence, TableCheck},
    catalog::{
        definition_capture, relation_attributes::DroppedAttribute,
        relation_oids::RelationCatalogOids,
    },
    SQLError,
};

#[derive(Debug)]
pub struct CapturedForeignTable {
    pub name: String,
    pub persistence: RelationPersistence,
    pub object_id: [u8; 16],
    pub catalog_oids: Option<RelationCatalogOids>,
    pub row_type_array_name: Option<String>,
    pub server_name: String,
    pub server_reference: Option<ForeignServerReference>,
    pub columns: Vec<ColumnDef>,
    pub dropped_attributes: Vec<DroppedAttribute>,
    pub checks: Vec<TableCheck>,
    pub options: Vec<(String, String)>,
    pub option_order: Vec<String>,
}

fn table(
    value: &StoredForeignTable,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedForeignTable>, SQLError> {
    let StoredForeignTable {
        name,
        persistence,
        object_id,
        catalog_oids,
        row_type_array_name,
        server_name,
        server_reference,
        columns,
        dropped_attributes,
        checks,
        options,
        option_order,
    } = value;
    let mut memory = control.empty_reservation();
    macro_rules! take {
        ($produced:expr) => {{
            let (value, lease) = $produced?.into_parts();
            memory = control.combine(memory.take(), lease);
            value
        }};
    }
    let value = CapturedForeignTable {
        name: take!(control.copy_text(name)),
        persistence: *persistence,
        object_id: *object_id,
        catalog_oids: *catalog_oids,
        row_type_array_name: take!(definition_capture::optional_name(
            row_type_array_name,
            control
        )),
        server_name: take!(control.copy_text(server_name)),
        server_reference: *server_reference,
        columns: take!(definition_capture::columns(columns, control)),
        dropped_attributes: take!(definition_capture::dropped_attributes(
            dropped_attributes,
            control
        )),
        checks: take!(definition_capture::checks(checks, control)),
        options: take!(definition_capture::foreign::options(options, control)),
        option_order: take!(definition_capture::names(option_order, control)),
    };
    Ok(control.finish(value, memory)?)
}

pub fn tables(
    values: &BTreeMap<RelationIdentity, StoredForeignTable>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, CapturedForeignTable)>>, SQLError> {
    let mut entries = ProductionVec::new(*control);
    entries.reserve(values.len())?;
    for (name, value) in values {
        let (schema, schema_memory) = control.copy_text(&name.schema)?.into_parts();
        let (name, name_memory) = control.copy_text(&name.name)?.into_parts();
        let (value, value_memory) = table(value, control)?.into_parts();
        let memory = control.combine(control.combine(schema_memory, name_memory), value_memory);
        entries
            .push_produced(control.finish((RelationIdentity { schema, name }, value), memory)?)?;
    }
    Ok(entries.finish()?)
}

#[cfg(test)]
mod tests;
