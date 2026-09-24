//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Match each inherited key with one child whose index is available to that parent.

use super::{index_definition, BTreeMap, RelationIdentity, StorageBackendResult};
use crate::catalog::CatalogReadView;
use uqa_sql::{
    ast::TableKeyConstraint, schema::inheritance::alter::append_inherited_keys_matching,
};

pub(super) fn append_keys(
    catalog: &CatalogReadView,
    child: &RelationIdentity,
    target: &mut Vec<TableKeyConstraint>,
    inherited: &[TableKeyConstraint],
) -> StorageBackendResult<Vec<TableKeyConstraint>> {
    let definitions = catalog
        .catalog_indexes()
        .map(|row| Ok((row, index_definition(row)?)))
        .collect::<StorageBackendResult<Vec<_>>>()?;
    let parent_owners = definitions
        .iter()
        .filter_map(|(_, definition)| {
            definition.catalog.as_ref().map(|identity| {
                (
                    identity.identity.object_id,
                    definition.relationships.owning_constraint,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let attachments = definitions
        .iter()
        .filter(|(row, _)| row.table_name == child.qualified_name())
        .filter_map(|(_, definition)| {
            Some((
                definition.relationships.owning_constraint?,
                definition.relationships.parent_index?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    Ok(append_inherited_keys_matching(
        target,
        inherited,
        |child, parent| {
            let attached = child
                .catalog_identity
                .and_then(|identity| attachments.get(&identity.object_id));
            attached.is_none_or(|index| {
                parent_owners.get(index).copied().flatten()
                    == parent.catalog_identity.map(|identity| identity.object_id)
            })
        },
    ))
}
