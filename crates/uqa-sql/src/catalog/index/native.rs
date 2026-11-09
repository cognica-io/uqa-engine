//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native index ownership closes over the original table, key and partition identities.

use super::stored::input::{columns_match, metadata, IndexMetadata};
use crate::{
    ast::{TableHierarchy, TableKeyConstraint},
    catalog::resolution::bound_relation_identity_with_control,
    SQLError,
};
use std::cell::Cell;
use uqa_core::{
    catalog_index::CatalogIndexRow,
    memory::{BudgetedMap, MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};

#[derive(Clone, Copy)]
pub struct NativeIndexTable<'a> {
    pub name: &'a RelationIdentity,
    pub object_id: [u8; 16],
    pub keys: &'a [TableKeyConstraint],
    pub hierarchy: &'a TableHierarchy,
}

struct Index<'a> {
    table: NativeIndexTable<'a>,
    row: &'a CatalogIndexRow,
    metadata: IndexMetadata<'a>,
    visited: Cell<u8>,
}

/// Validate stored ownership without inferring identities, rebinding expressions or rebuilding indexes. Legacy definitions without recorded addresses remain addressless. Native addresses, declared key ownership and partition-parent edges must agree in both directions. All scratch and decoded text retain the supplied allowance until validation finishes or fails.
pub fn validate<'a>(
    tables: impl Iterator<Item = NativeIndexTable<'a>>,
    rows: impl Iterator<Item = &'a CatalogIndexRow>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut relations = BudgetedMap::new(memory);
    let mut table_ids = BudgetedMap::new(memory);
    let mut constraints = BudgetedMap::new(memory);
    for table in tables {
        control.check()?;
        if table.object_id == [0; 16]
            || table_ids.insert(table.object_id, ())?.is_some()
            || relations.insert(table.name, table)?.is_some()
        {
            return Err(invalid("duplicate or invalid table identity"));
        }
        for key in table.keys {
            control.check()?;
            if let Some(identity) = key.catalog_identity {
                if !identity.is_valid()
                    || constraints
                        .insert(identity.object_id, (table, key))?
                        .is_some()
                {
                    return Err(invalid("duplicate or invalid constraint identity"));
                }
            } else if key.index_identity.is_some() {
                return Err(invalid("indexed constraint has no identity"));
            }
        }
    }
    let mut indexes = BudgetedMap::new(memory);
    let mut oids = BudgetedMap::new(memory);
    for row in rows {
        control.check()?;
        let name = bound_relation_identity_with_control(&row.table_name, &control)?;
        let table = *relations
            .get(&*name)
            .ok_or_else(|| invalid("missing table"))?;
        if row.relation.schema != table.name.schema {
            return Err(invalid("index and table namespaces differ"));
        }
        let metadata = metadata(row.definition_json.as_deref(), &control)?;
        let Some(catalog) = metadata.catalog.as_ref() else {
            if !metadata.relationships.is_empty() {
                return Err(invalid("relationship has no index identity"));
            }
            continue;
        };
        catalog
            .validate(table.object_id)
            .map_err(|_| invalid("table incarnation"))?;
        metadata
            .relationships
            .validate(catalog.identity.object_id)
            .map_err(|_| invalid("ownership or parent identity"))?;
        if oids.insert(catalog.identity.oid, ())?.is_some() {
            return Err(invalid("duplicate index OID"));
        }
        let id = catalog.identity.object_id;
        if indexes
            .insert(
                id,
                Index {
                    table,
                    row,
                    metadata,
                    visited: Cell::new(0),
                },
            )?
            .is_some()
        {
            return Err(invalid("duplicate index identity"));
        }
    }
    ownership_edges(&indexes, &constraints, memory, &control)?;
    acyclic(&indexes, &control)?;
    control.check()?;
    Ok(())
}

fn ownership_edges(
    indexes: &BudgetedMap<[u8; 16], Index<'_>>,
    constraints: &BudgetedMap<[u8; 16], (NativeIndexTable<'_>, &TableKeyConstraint)>,
    memory: &MemoryBudget,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut physical_keys = BudgetedMap::new(memory);
    let mut owned_constraints = BudgetedMap::new(memory);
    for (_, index) in indexes {
        control.check()?;
        let catalog = index.metadata.catalog.as_ref().expect("addressed index");
        if physical_keys
            .insert((index.table.object_id, catalog.physical_key.as_str()), ())?
            .is_some()
        {
            return Err(invalid("duplicate physical key"));
        }
        if let Some(owner) = index.metadata.relationships.owning_constraint {
            if owned_constraints.insert(owner, ())?.is_some() {
                return Err(invalid("constraint owns multiple indexes"));
            }
            let (table, key) = constraints
                .get(&owner)
                .ok_or_else(|| invalid("missing owning constraint"))?;
            if table.name != index.table.name
                || key
                    .index_identity
                    .is_some_and(|identity| identity != catalog.identity)
                || !index.metadata.unique
                || index.metadata.predicate.is_some()
                || index.metadata.nulls_not_distinct != key.nulls_not_distinct
                || !columns_match(Some(&index.row.columns_json), &key.columns, control)?
                || !columns_match(
                    index.metadata.included_columns,
                    &key.included_columns,
                    control,
                )?
            {
                return Err(invalid("constraint and index disagree"));
            }
        }
        if let Some(parent) = index.metadata.relationships.parent_index {
            parent_edge(
                index,
                indexes
                    .get(&parent)
                    .ok_or_else(|| invalid("missing parent index"))?,
                control,
            )?;
        }
    }
    for (id, (table, key)) in constraints {
        control.check()?;
        let Some(identity) = key.index_identity else {
            continue;
        };
        let index = indexes
            .get(&identity.object_id)
            .ok_or_else(|| invalid("missing constraint index"))?;
        if !identity.is_valid()
            || index.table.name != table.name
            || index
                .metadata
                .catalog
                .as_ref()
                .expect("addressed index")
                .identity
                != identity
            || index.metadata.relationships.owning_constraint != Some(*id)
        {
            return Err(invalid("constraint index ownership"));
        }
    }
    Ok(())
}

fn parent_edge(
    child: &Index<'_>,
    parent: &Index<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let hierarchy = child.table.hierarchy;
    if !hierarchy.is_partition()
        || hierarchy.parents.len() != 1
        || parent.table.hierarchy.partition_spec.is_none()
    {
        return Err(invalid("parent index outside a partition edge"));
    }
    let name = bound_relation_identity_with_control(&hierarchy.parents[0], control)?;
    if *name != *parent.table.name {
        return Err(invalid("parent index belongs to another table"));
    }
    Ok(())
}

// Each index has at most one parent. Two iterative walks visit each new chain once, using only its admitted node-local color; catalog input is immutable.
fn acyclic(
    indexes: &BudgetedMap<[u8; 16], Index<'_>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for (start, _) in indexes {
        control.check()?;
        let mut cursor = Some(*start);
        while let Some(id) = cursor {
            control.check()?;
            let index = indexes.get(&id).expect("validated parent edge");
            match index.visited.get() {
                1 => return Err(invalid("cyclic index ancestry")),
                2 => break,
                _ => index.visited.set(1),
            }
            cursor = index.metadata.relationships.parent_index;
        }
        cursor = Some(*start);
        while let Some(id) = cursor {
            control.check()?;
            let index = indexes.get(&id).expect("validated parent edge");
            if index.visited.replace(2) == 2 {
                break;
            }
            cursor = index.metadata.relationships.parent_index;
        }
    }
    Ok(())
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native index reference: {reason}"))
}

#[cfg(test)]
mod tests;
