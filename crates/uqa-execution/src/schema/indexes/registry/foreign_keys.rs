//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve foreign-key dependencies to immutable unique-index incarnations.

use super::{
    index_definition, invalid, BTreeMap, CatalogIndexRow, ColumnDef, IndexKey, RelationIdentity,
    StorageBackendResult, TableConstraintSet,
};

pub(crate) fn bind(
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    allow_new: bool,
) -> StorageBackendResult<bool> {
    let mut changed = false;
    for column in columns {
        if let Some(reference) = &mut column.references {
            let names = reference.column.iter().cloned().collect::<Vec<_>>();
            changed |= bind_one(
                rows,
                &reference.table,
                &names,
                &mut reference.referenced_index,
                &mut reference.referenced_key,
                allow_new,
            )?;
        }
    }
    for key in &mut constraints.foreign_keys {
        changed |= bind_one(
            rows,
            &key.ref_table,
            &key.ref_columns,
            &mut key.referenced_index,
            &mut key.referenced_key,
            allow_new,
        )?;
    }
    for key in &mut constraints.hierarchy.partition_inherited_foreign_keys {
        changed |= bind_one(
            rows,
            &key.ref_table,
            &key.ref_columns,
            &mut key.referenced_index,
            &mut key.referenced_key,
            allow_new,
        )?;
    }
    Ok(changed)
}

fn bind_one(
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    table: &str,
    columns: &[String],
    identity: &mut Option<[u8; 16]>,
    name: &mut Option<String>,
    allow_new: bool,
) -> StorageBackendResult<bool> {
    if identity.is_none() && !allow_new {
        return Err(invalid("foreign key has no referenced index incarnation"));
    }
    for row in rows.values().filter(|row| row.table_name == table) {
        let definition = index_definition(row)?;
        let index = definition
            .catalog
            .as_ref()
            .ok_or_else(|| invalid("referenced index has no catalog address"))?;
        if let Some(id) = identity {
            if *id != index.identity.object_id {
                continue;
            }
        } else if name.as_ref().is_some_and(|name| name != &row.relation.name) {
            continue;
        }
        let keys = serde_json::from_str::<Vec<IndexKey>>(&row.columns_json)?;
        if !definition.unique
            || definition.predicate.is_some()
            || keys.len() != columns.len()
            || !keys.iter().all(|key| {
                key.column()
                    .is_some_and(|key| columns.iter().any(|column| column == key))
            })
        {
            if identity.is_some() {
                return Err(invalid("foreign key references an ineligible index"));
            }
            continue;
        }
        let changed = identity.is_none();
        *identity = Some(index.identity.object_id);
        // A current incarnation is authoritative even when its diagnostic name predates a rename.
        if changed {
            *name = Some(row.relation.name.clone());
        }
        return Ok(changed);
    }
    Err(invalid(format!(
        "foreign key references a missing unique index on `{table}`"
    )))
}
