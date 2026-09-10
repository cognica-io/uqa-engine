//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a hierarchy candidate after binding all inherited schema dependencies.
use super::{materialize_metadata, resolve_table_name, table_not_found, SchemaPublicationContext};
use crate::schema::hierarchy::HierarchyCatalog;
use std::collections::BTreeSet;
use uqa_sql::ast::{ColumnDef, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint};
use uqa_sql::schema::{dependencies::registration, inheritance::origins::InheritanceOriginChange};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub struct HierarchySchemaChange {
    pub columns: Vec<ColumnDef>,
    pub checks: Vec<TableCheck>,
    pub foreign_keys: Vec<ForeignKey>,
    pub key_constraints: Vec<TableKeyConstraint>,
    pub hierarchy: TableHierarchy,
}
pub fn replace_hierarchy_components(
    context: &SchemaPublicationContext<'_>,
    catalog: &dyn HierarchyCatalog,
    table: &str,
    change: HierarchySchemaChange,
) -> StorageBackendResult<()> {
    let HierarchySchemaChange {
        mut columns,
        mut checks,
        mut foreign_keys,
        key_constraints,
        hierarchy,
    } = change;
    let table_name = resolve_table_name(context.catalog, table)?;
    let state = context
        .catalog
        .table_state(&table_name)?
        .ok_or_else(|| table_not_found(&table_name))?;
    let previous_hierarchy = state.hierarchy();
    if let Some(origins) = InheritanceOriginChange::between(&previous_hierarchy, &hierarchy) {
        let mut inherited = BTreeSet::new();
        for parent in &hierarchy.parents {
            for column in catalog
                .try_describe_table(parent)?
                .ok_or_else(|| table_not_found(parent))?
            {
                if column.not_null && !column.not_null_no_inherit {
                    inherited.insert(column.name);
                }
            }
        }
        origins.update_not_null(&mut columns, &inherited);
        let mut inherited = BTreeSet::new();
        for parent in &hierarchy.parents {
            for check in catalog.try_check_constraint_definitions(parent)? {
                if !check.no_inherit {
                    inherited.extend(check.name);
                }
            }
        }
        origins.update_checks(&mut columns, &mut checks, &inherited);
    }
    for foreign_key in &mut foreign_keys {
        foreign_key.ref_table = resolve_table_name(context.catalog, &foreign_key.ref_table)?;
    }
    let mut constraints = state.constraints();
    constraints.checks = checks;
    constraints.foreign_keys = foreign_keys;
    constraints.key_constraints = key_constraints;
    constraints.hierarchy = hierarchy;
    registration::bind_table_schema_routine_identities(
        &context.bindings,
        &table_name,
        &mut columns,
        &mut constraints.checks,
    )
    .map_err(StorageBackendError::Other)?;
    materialize_metadata(context, &table_name, &mut columns, &mut constraints)?;
    state.persist_candidate(&columns, &constraints)?;
    let hierarchy = constraints.hierarchy.clone();
    state.publish_columns(
        constraints.columns_declared.unwrap_or(false) || !columns.is_empty(),
        columns,
        constraints,
    );
    state.publish_hierarchy(hierarchy);
    state.refresh_value_indexes()?;
    Ok(())
}
