//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Migrate legacy sequence owners at the existing initial-open catalog transaction.
use uqa_sql::schema::sequences::owner_migration::collect_migrated_sequence_owner;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};
pub fn migrate_implicit_sequence_owners(catalog: &dyn CatalogFacade) -> StorageBackendResult<()> {
    let tables = catalog.load_tables()?;
    let rows = catalog.load_sequence_rows()?;
    let sequence_relations = rows
        .iter()
        .map(|row| row.relation.clone())
        .collect::<Vec<_>>();
    let mut valid_owners = std::collections::BTreeSet::new();
    let mut inferred = std::collections::BTreeMap::new();
    for table in tables {
        let columns: Vec<uqa_sql::ast::ColumnDef> = if table.columns_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&table.columns_json)?
        };
        for column in &columns {
            collect_migrated_sequence_owner(
                &table.relation,
                table.object_id,
                column,
                &sequence_relations,
                &mut valid_owners,
                &mut inferred,
            )
            .map_err(StorageBackendError::Other)?;
        }
    }
    for row in catalog.load_foreign_tables()? {
        let relation_name = row.relation.qualified_name();
        let options = serde_json::from_str(&row.options_json)?;
        let (table, _) = crate::catalog::foreign::StoredForeignTable::from_catalog(
            relation_name.clone(),
            row.server_name,
            options,
            &row.columns_json,
        )?;
        if table.object_id == [0; 16] {
            return Err(StorageBackendError::Other(format!(
                "foreign table `{relation_name}` has no object identity during sequence-owner migration"
            )));
        }
        for column in &table.columns {
            collect_migrated_sequence_owner(
                &row.relation,
                table.object_id,
                column,
                &sequence_relations,
                &mut valid_owners,
                &mut inferred,
            )
            .map_err(StorageBackendError::Other)?;
        }
    }
    for mut row in rows {
        if let Some(owner) = row.owner {
            if !valid_owners.contains(&(owner.table_object_id, owner.column_object_id)) {
                return Err(StorageBackendError::Other(format!(
                    "sequence `{}` has a dangling owner dependency",
                    row.relation.qualified_name()
                )));
            }
            continue;
        }
        let Some(owner) = inferred.get(&row.relation).copied() else {
            continue;
        };
        row.owner = Some(owner);
        if !catalog.replace_sequence_row(&row)? {
            return Err(StorageBackendError::Other(format!(
                "sequence `{}` disappeared during owner migration",
                row.relation.qualified_name()
            )));
        }
    }
    Ok(())
}
