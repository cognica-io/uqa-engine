//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain predecessor partition names and physical namespaces during initial conversion.

use super::{invalid, CatalogIndexRow, CatalogReadView, IndexCatalogIdentity, RelationIdentity};
use crate::catalog::{index::index_definition, projection::CatalogIndexRelation};
use crate::schema::indexes::registry::partitions::constraint_kind;
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    ast::IndexKey,
    catalog::index::{can_attach_index, IndexAttachmentShape, IndexRelationships},
    schema::constraint_metadata::CatalogObjectAllocator,
};
use uqa_storage::StorageBackendResult;

pub(super) fn preserve_derived_names(
    catalog: &CatalogReadView,
    rows: &mut BTreeMap<RelationIdentity, CatalogIndexRow>,
    stored: &[CatalogIndexRow],
    predecessor: &[CatalogIndexRelation],
    allocator: &mut dyn CatalogObjectAllocator,
) -> StorageBackendResult<()> {
    // The predecessor projection visits each parent before its children. Preserve those edges when they satisfy current constraint ownership rules.
    for old in predecessor.iter().filter(|index| index.is_partition) {
        let parent = predecessor
            .iter()
            .find(|row| Some(row.oid()) == old.parent_index_oid)
            .ok_or_else(|| invalid("missing predecessor index parent"))?;
        let Some(parent) = rows.get(&parent.relation) else {
            continue;
        };
        let parent_definition = index_definition(parent)?;
        let parent_kind = constraint_kind(catalog, parent, &parent_definition)?;
        let parent_id = parent_definition
            .catalog
            .as_ref()
            .expect("converted address")
            .identity
            .object_id;
        let parent_keys: Vec<IndexKey> = serde_json::from_str(&parent.columns_json)?;
        let mut row = if let Some(row) = rows.get(&old.relation) {
            row.clone()
        } else {
            // Owned parents require real local constraints; the shared materializer attaches their prepared indexes instead of inventing another declaration.
            if parent_kind.is_some() {
                continue;
            }
            let Some(source) = physical_source(old, stored, predecessor)? else {
                continue;
            };
            let table = RelationIdentity::from_legacy_name(&old.table_name).map_err(invalid)?;
            let mut definition = old.definition.clone();
            let mut identity = IndexCatalogIdentity::allocate(
                catalog.snapshot().tables[&table].object_id,
                allocator,
            )
            .map_err(invalid)?;
            identity.physical_key = index_definition(source)?.catalog.map_or_else(
                || source.relation.qualified_name(),
                |identity| identity.physical_key,
            );
            definition.catalog = Some(identity);
            definition.relationships = IndexRelationships::default();
            CatalogIndexRow {
                relation: old.relation.clone(),
                table_name: old.table_name.clone(),
                columns_json: serde_json::to_string(&old.columns)?,
                definition_json: Some(serde_json::to_string(&definition)?),
                ..source.clone()
            }
        };
        let mut definition = index_definition(&row)?;
        let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
        if can_attach_index(
            &IndexAttachmentShape {
                method: &parent.index_type,
                keys: &parent_keys,
                definition: &parent_definition,
                constraint_kind: parent_kind,
            },
            &IndexAttachmentShape {
                method: &row.index_type,
                keys: &keys,
                definition: &definition,
                constraint_kind: constraint_kind(catalog, &row, &definition)?,
            },
        ) {
            definition.relationships.parent_index = Some(parent_id);
            row.definition_json = Some(serde_json::to_string(&definition)?);
            rows.insert(row.relation.clone(), row);
        }
    }
    Ok(())
}

fn physical_source<'a, 'b>(
    mut old: &'b CatalogIndexRelation,
    stored: &'a [CatalogIndexRow],
    predecessor: &'b [CatalogIndexRelation],
) -> StorageBackendResult<Option<&'a CatalogIndexRow>> {
    let mut visited = BTreeSet::new();
    while let Some(parent) = old.parent_index_oid {
        if !visited.insert(parent) {
            return Err(invalid("cyclic predecessor index ancestry"));
        }
        old = predecessor
            .iter()
            .find(|row| row.oid() == parent)
            .ok_or_else(|| invalid("missing predecessor index ancestor"))?;
        if let Some(source) = stored.iter().find(|row| row.relation == old.relation) {
            return Ok(Some(source));
        }
    }
    Ok(None)
}
