//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Upgrade foreign identities inside the caller-owned initial catalog transaction.
use crate::catalog::foreign::StoredForeignTable;
use uqa_core::RelationIdentity;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub fn migrate_foreign_table_identities(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
    for mut row in catalog.load_foreign_tables()? {
        let relation_name = row.relation.qualified_name();
        let options = serde_json::from_str(&row.options_json)?;
        let (mut table, legacy_schema) = StoredForeignTable::from_catalog(
            relation_name,
            row.server_name.clone(),
            options,
            &row.columns_json,
        )?;
        let mut changed = legacy_schema;
        if table.object_id == [0; 16] {
            table.object_id =
                crate::catalog::identity::new_nonzero_catalog_identity("table", "object identity")?;
            changed = true;
        }
        let mut constraints = uqa_sql::ast::TableConstraintSet {
            checks: std::mem::take(&mut table.checks),
            ..uqa_sql::ast::TableConstraintSet::default()
        };
        changed |=
            materialize_constraint_metadata(&row.relation, &mut table.columns, &mut constraints)?;
        table.checks = constraints.checks;
        changed |=
            crate::schema::sequences::migration::materialize_persisted_foreign_implicit_sequences(
                catalog,
                &row.relation,
                &row.role_owner,
                table.object_id,
                &mut table.columns,
            )?;
        if changed {
            row.columns_json = table.schema_json()?;
            catalog.save_foreign_table(&row)?;
        }
    }
    Ok(())
}

fn materialize_constraint_metadata(
    relation: &RelationIdentity,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
) -> StorageBackendResult<bool> {
    uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
        relation,
        columns,
        constraints,
        &mut crate::catalog::identity::allocate_catalog_object_id,
    )
    .map_err(|error| StorageBackendError::Other(error.to_string()))
}
