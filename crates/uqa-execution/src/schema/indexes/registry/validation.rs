//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the complete durable index graph independently of its construction path.

use super::{
    index_definition, invalid, partitions, BTreeMap, BTreeSet, CatalogIndexRow, IndexDefinition,
    IndexKey, RelationIdentity, StorageBackendResult,
};
use crate::catalog::CatalogReadView;
use uqa_sql::catalog::index::{can_attach_index, IndexAttachmentShape};

fn validated_addresses<'a>(
    catalog: &CatalogReadView,
    rows: &'a BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<ValidatedIndexes<'a>> {
    let mut objects = BTreeMap::new();
    let mut oids = BTreeSet::new();
    let mut physical = BTreeSet::new();
    let mut owners = BTreeSet::new();
    let namespace = catalog.snapshot();
    let graph_relations = crate::catalog::graph::graph_catalog_entries(catalog).map_err(invalid)?;
    for (name, row) in rows {
        if name != &row.relation {
            return Err(invalid("index name disagrees with its registry key"));
        }
        let table_name = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
        let table = catalog
            .snapshot()
            .tables
            .get(&table_name)
            .ok_or_else(|| invalid("index has no indexed table"))?;
        if name.schema != table_name.schema {
            return Err(invalid("index and table schemas disagree"));
        }
        let definition = index_definition(row)?;
        if namespace.tables.contains_key(name)
            || namespace.definitions.views.contains_key(name)
            || namespace.definitions.foreign_tables.contains_key(name)
            || namespace.definitions.sequences.contains_key(name)
            || uqa_sql::catalog::SystemRelation::all()
                .any(|relation| relation.namespace() == name.schema && relation.name() == name.name)
            || graph_relations.iter().any(|graph| {
                graph.name == name.schema
                    && (name.name == "_label_id_seq"
                        || graph.labels.iter().any(|label| {
                            label.name == name.name || name.name == format!("{}_id_seq", label.name)
                        }))
            })
        {
            return Err(invalid("index name conflicts with another relation"));
        }
        let identity = definition
            .catalog
            .as_ref()
            .ok_or_else(|| invalid("index has no catalog identity"))?;
        identity.validate(table.object_id).map_err(invalid)?;
        definition
            .relationships
            .validate(identity.identity.object_id)
            .map_err(invalid)?;
        if objects
            .insert(identity.identity.object_id, (row, definition.clone()))
            .is_some()
            || !oids.insert(identity.identity.oid)
            || !physical.insert((table.object_id, identity.physical_key.clone()))
        {
            return Err(invalid(
                "duplicate index identity or table physical namespace",
            ));
        }
        if let Some(owner) = definition.relationships.owning_constraint {
            if !owners.insert(owner) {
                return Err(invalid("constraint owns more than one index"));
            }
            let key = table
                .keys
                .iter()
                .find(|key| key.catalog_identity.is_some_and(|id| id.object_id == owner))
                .ok_or_else(|| invalid("index has no owning constraint"))?;
            let expected = IndexDefinition::for_constraint(key, &table.columns).map_err(invalid)?;
            let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
            if key.name.as_ref() != Some(&name.name)
                || keys
                    != key
                        .columns
                        .iter()
                        .cloned()
                        .map(IndexKey::Column)
                        .collect::<Vec<_>>()
                || !definition.unique
                || definition.nulls_not_distinct != expected.nulls_not_distinct
                || definition.predicate.is_some()
                || !definition.included_columns.is_empty()
                || row.index_type
                    != if key.without_overlaps {
                        "gist"
                    } else {
                        "btree"
                    }
            {
                return Err(invalid("owned index disagrees with its constraint"));
            }
        }
    }
    for table in catalog.snapshot().tables.values() {
        for key in table.keys.iter() {
            if !key
                .catalog_identity
                .is_some_and(|id| owners.contains(&id.object_id))
            {
                return Err(invalid("constraint has no owned index"));
            }
        }
    }
    Ok(objects)
}

fn validate_parents(
    catalog: &CatalogReadView,
    objects: &ValidatedIndexes<'_>,
) -> StorageBackendResult<()> {
    let mut attached = BTreeSet::new();
    for (row, definition) in objects.values() {
        let Some(parent_id) = definition.relationships.parent_index else {
            continue;
        };
        let (parent, parent_definition) = objects
            .get(&parent_id)
            .ok_or_else(|| invalid("index has no parent index"))?;
        let relation = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
        let parent_table =
            RelationIdentity::from_legacy_name(&parent.table_name).map_err(invalid)?;
        let hierarchy = &catalog.snapshot().tables[&relation].hierarchy;
        if !hierarchy.is_partition()
            || hierarchy.parents.first() != Some(&parent.table_name)
            || catalog.snapshot().tables[&parent_table]
                .hierarchy
                .partition_spec
                .is_none()
            || !attached.insert((parent_id, relation.clone()))
        {
            return Err(invalid("invalid or duplicate partition index edge"));
        }
        let keys = serde_json::from_str::<Vec<IndexKey>>(&row.columns_json)?;
        let parent_keys = serde_json::from_str::<Vec<IndexKey>>(&parent.columns_json)?;
        if !can_attach_index(
            &IndexAttachmentShape {
                method: &parent.index_type,
                keys: &parent_keys,
                definition: parent_definition,
                constraint_kind: partitions::constraint_kind(catalog, parent, parent_definition)?,
            },
            &IndexAttachmentShape {
                method: &row.index_type,
                keys: &keys,
                definition,
                constraint_kind: partitions::constraint_kind(catalog, row, definition)?,
            },
        ) {
            return Err(invalid("partition index disagrees with its parent"));
        }
        let mut path = BTreeSet::new();
        let mut next = Some(parent_id);
        while let Some(id) = next {
            if !path.insert(id) {
                return Err(invalid("cyclic partition index ancestry"));
            }
            next = objects
                .get(&id)
                .ok_or_else(|| invalid("index has no parent index"))?
                .1
                .relationships
                .parent_index;
        }
    }
    for (id, (row, _)) in objects {
        for (name, child) in &catalog.snapshot().tables {
            if child.hierarchy.is_partition()
                && child.hierarchy.parents.first() == Some(&row.table_name)
                && !attached.contains(&(*id, name.clone()))
            {
                return Err(invalid("partition has no attached child index"));
            }
        }
    }
    Ok(())
}

type ValidatedIndexes<'a> = BTreeMap<[u8; 16], (&'a CatalogIndexRow, IndexDefinition)>;

pub(crate) fn validate(
    catalog: &CatalogReadView,
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<()> {
    validate_parents(catalog, &validated_addresses(catalog, rows)?)
}
