//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build newly materialized partition fields before publishing their catalog rows.

use super::{
    index_definition, BTreeMap, BTreeSet, CatalogIndexRow, IndexRegistryContext, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};
use uqa_sql::ast::CreateIndex;

pub(in crate::schema::indexes) fn new_descendants(
    previous: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<Vec<CatalogIndexRow>> {
    let identities = previous
        .values()
        .map(|row| {
            Ok(index_definition(row)?
                .catalog
                .map(|identity| identity.identity.object_id))
        })
        .collect::<StorageBackendResult<BTreeSet<_>>>()?;
    let mut builds = Vec::new();
    for row in rows.values() {
        let definition = index_definition(row)?;
        if definition.relationships.parent_index.is_some()
            && !identities.contains(
                &definition
                    .catalog
                    .map(|identity| identity.identity.object_id),
            )
        {
            builds.push(row.clone());
        }
    }
    Ok(builds)
}

pub(super) fn build(
    context: &IndexRegistryContext<'_>,
    row: &CatalogIndexRow,
) -> StorageBackendResult<()> {
    let definition = index_definition(row)?;
    let options: BTreeMap<String, String> = serde_json::from_str(&row.parameters_json)?;
    let statement = CreateIndex {
        name: Some(row.relation.name.clone()),
        table: row.table_name.clone(),
        access_method: row.index_type.clone(),
        columns: serde_json::from_str(&row.columns_json)?,
        included_columns: definition.included_columns,
        column_order: definition.column_order,
        predicate: definition.predicate,
        unique: definition.unique,
        nulls_not_distinct: definition.nulls_not_distinct,
        if_not_exists: false,
        options: options.into_iter().collect(),
    };
    super::super::creation::build_physical_index(
        context.vectors,
        context.builds,
        &statement,
        &row.index_type,
    )
    .map_err(|error| StorageBackendError::backend("partition index build", error))
}
