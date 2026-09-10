//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attach implicit sequence owners after their table columns have stable catalog identities.
use uqa_sql::{
    ast::ColumnDef, schema::sequences::implicit_ownership::StoredSequenceNames, SQLError,
};
use uqa_storage::{
    SequenceOwner, SequenceOwnerDependency, StorageBackendError, StorageBackendResult,
};

pub trait ImplicitOwnerTables {
    fn table_owner_columns(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<([u8; 16], Vec<ColumnDef>)>>;
}
pub trait ImplicitOwnerPublication {
    fn attach_owner(&self, sequence: &str, owner: SequenceOwner) -> Result<(), SQLError>;
}
pub struct ImplicitOwnershipContext<'a> {
    pub names: &'a dyn StoredSequenceNames,
    pub tables: &'a dyn ImplicitOwnerTables,
    pub publication: &'a dyn ImplicitOwnerPublication,
}
pub fn implicit_owner_bindings(
    names: &dyn StoredSequenceNames,
    table: &str,
    table_object_id: [u8; 16],
    columns: &[ColumnDef],
) -> StorageBackendResult<Vec<(String, SequenceOwner)>> {
    uqa_sql::schema::sequences::implicit_ownership::bind_implicit_sequence_owners(
        names,
        table,
        table_object_id,
        columns,
    )
    .map(|bindings| {
        bindings
            .into_iter()
            .map(|binding| {
                (
                    binding.sequence,
                    SequenceOwner {
                        table_object_id: binding.table_object_id,
                        column_object_id: binding.column_object_id,
                        dependency: if binding.identity {
                            SequenceOwnerDependency::Internal
                        } else {
                            SequenceOwnerDependency::Automatic
                        },
                    },
                )
            })
            .collect()
    })
    .map_err(StorageBackendError::Other)
}
pub fn attach_table_owners(
    context: &ImplicitOwnershipContext<'_>,
    table: &str,
) -> StorageBackendResult<()> {
    let (object_id, columns) = context
        .tables
        .table_owner_columns(table)?
        .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` disappeared")))?;
    attach_column_owners(context, table, object_id, &columns)
}
pub fn attach_column_owners(
    context: &ImplicitOwnershipContext<'_>,
    table: &str,
    object_id: [u8; 16],
    columns: &[ColumnDef],
) -> StorageBackendResult<()> {
    for (sequence, owner) in implicit_owner_bindings(context.names, table, object_id, columns)? {
        context
            .publication
            .attach_owner(&sequence, owner)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    }
    Ok(())
}

/// Persist legacy owner-marker removal before publishing each retained table generation.
pub fn clear_auto_increment_owner_markers(
    context: &crate::schema::publication::dependencies::SchemaDependencyPublicationContext<'_>,
    sequence: &str,
) -> uqa_storage::StorageBackendResult<()> {
    let target = uqa_core::RelationIdentity::from_legacy_name(sequence)
        .map_err(uqa_storage::StorageBackendError::Other)?;
    let mut catalog_changed = false;
    for (_, state) in context.tables.table_schemas() {
        let mut columns = state.columns();
        if !uqa_sql::schema::sequences::implicit_ownership::clear_auto_increment_owner_markers(
            &mut columns,
            &target,
        ) {
            continue;
        }
        state.persist_columns(&columns)?;
        state.write_columns().publish(columns);
        catalog_changed = true;
    }
    if catalog_changed {
        context.changes.table_catalog_changed();
    }
    let mut foreign_updates = Vec::new();
    for (relation, mut table) in context.foreign.foreign_tables() {
        if uqa_sql::schema::sequences::implicit_ownership::clear_auto_increment_owner_markers(
            &mut table.columns,
            &target,
        ) {
            foreign_updates.push((relation, table));
        }
    }
    for (relation, table) in &foreign_updates {
        context.foreign.persist_foreign_table(relation, table)?;
    }
    if !foreign_updates.is_empty() {
        context.foreign.publish_foreign_tables(foreign_updates);
        context.changes.catalog_registry_changed();
    }
    Ok(())
}
