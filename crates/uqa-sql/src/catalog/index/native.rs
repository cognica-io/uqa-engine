//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native index ownership closes over the original table, key and partition identities.

use super::stored::input::{
    columns_match_with_work, metadata_with_work, ColumnKeys, IndexMetadata, StoredColumnKey,
};
use crate::{
    ast::{ColumnDef, TableHierarchy, TableKeyConstraint},
    catalog::{
        resolution::bound_relation_identity_with_work,
        work::{compare_text, inline, retire_entries, ReferenceWork, Work},
    },
    SQLError,
};
use uqa_core::{
    catalog_index::CatalogIndexRow,
    memory::{Budgeted, BudgetedMap, MapKeyInput, MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity,
};

mod expressions;
pub use expressions::{validate_bindings, validate_bindings_with_work, IndexBindingContext};

#[derive(Clone, Copy)]
pub struct NativeIndexTable<'a> {
    pub name: &'a RelationIdentity,
    pub object_id: [u8; 16],
    /// None denotes a document table without a declared row layout; an empty declared layout is Some(&[]).
    pub columns: Option<&'a [ColumnDef]>,
    pub keys: &'a [TableKeyConstraint],
    pub hierarchy: &'a TableHierarchy,
}

struct Index<'a> {
    table: NativeIndexTable<'a>,
    row: &'a CatalogIndexRow,
    metadata: IndexMetadata<'a>,
}

/// Validate stored ownership and plain key/INCLUDE columns without inferring identities, rebinding expressions or rebuilding indexes. Legacy definitions without recorded addresses remain addressless and still validate their declared columns. Native addresses, declared key ownership and partition-parent edges must agree in both directions. All scratch and decoded text retain the supplied allowance until validation finishes or fails.
pub fn validate<'a>(
    tables: impl Iterator<Item = NativeIndexTable<'a>>,
    rows: impl Iterator<Item = &'a CatalogIndexRow>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    inline(validate_with_work(
        tables,
        rows,
        memory,
        cancellation,
        &mut (),
    ))
}

/// Validate and retire the original catalog indexes through the caller's work allowance.
pub async fn validate_with_work<'a, W: Work + Send>(
    tables: impl Iterator<Item = NativeIndexTable<'a>>,
    rows: impl Iterator<Item = &'a CatalogIndexRow>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut W,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut input = ReferenceWork {
        control: &control,
        work,
    };
    let mut tables_by_identity = Tables::new(memory);
    for table in tables {
        tables_by_identity.insert(table, memory, &mut input).await?;
    }
    let mut indexes = BudgetedMap::new(memory);
    let mut oids = BudgetedMap::new(memory);
    for row in rows {
        input.checkpoint().await?;
        let name = bound_relation_identity_with_work(&row.table_name, &control, input.work).await?;
        let table = *tables_by_identity
            .names
            .get_from(&*name, &mut input)
            .await?
            .ok_or_else(|| invalid("missing table"))?;
        if compare_text(
            input.work,
            &row.relation.schema,
            &table.name.schema,
            &control,
        )
        .await?
        .is_ne()
        {
            return Err(invalid("index and table namespaces differ"));
        }
        let metadata =
            metadata_with_work(row.definition_json.as_deref(), &control, input.work).await?;
        column_references(
            table,
            &row.columns_json,
            &tables_by_identity.columns,
            &mut input,
        )
        .await?;
        if let Some(included) = metadata.included_columns {
            if !column_references(table, included, &tables_by_identity.columns, &mut input).await? {
                return Err(invalid("included attribute is not a column"));
            }
        }
        let Some(catalog) = metadata.catalog.as_ref() else {
            if !metadata.relationships.is_empty() {
                return Err(invalid("relationship has no index identity"));
            }
            continue;
        };
        if !catalog
            .matches_table_with_work(table.object_id, &control, input.work)
            .await?
        {
            return Err(invalid("table incarnation"));
        }
        metadata
            .relationships
            .validate(catalog.identity.object_id)
            .map_err(|_| invalid("ownership or parent identity"))?;
        if oids.insert(catalog.identity.oid, 0u8)?.is_some() {
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
                },
            )?
            .is_some()
        {
            return Err(invalid("duplicate index identity"));
        }
    }
    ownership_edges(
        &indexes,
        &tables_by_identity.constraints,
        memory,
        &mut input,
    )
    .await?;
    acyclic(&indexes, &mut oids, &mut input).await?;
    retire_entries(oids.into_entries(), &control, work).await?;
    retire_entries(indexes.into_entries(), &control, work).await?;
    tables_by_identity.retire(&control, work).await?;
    control.check()?;
    Ok(())
}

struct Tables<'a> {
    names: OwnedMap<&'a RelationIdentity, NativeIndexTable<'a>>,
    ids: BudgetedMap<[u8; 16], ()>,
    constraints: BudgetedMap<[u8; 16], (NativeIndexTable<'a>, &'a TableKeyConstraint)>,
    columns: OwnedMap<ColumnName<'a>, ()>,
}

impl<'a> Tables<'a> {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            names: OwnedMap::new(),
            ids: BudgetedMap::new(memory),
            constraints: BudgetedMap::new(memory),
            columns: OwnedMap::new(),
        }
    }

    async fn insert<W: Work + Send>(
        &mut self,
        table: NativeIndexTable<'a>,
        memory: &MemoryBudget,
        input: &mut ReferenceWork<'_, '_, W>,
    ) -> Result<(), SQLError> {
        input.checkpoint().await?;
        if table.object_id == [0; 16]
            || self.ids.insert(table.object_id, ())?.is_some()
            || self
                .names
                .insert_admitted_key_from(
                    Budgeted::new(table.name, memory.reserve(0)?),
                    table,
                    input,
                )
                .await?
                .is_some()
        {
            return Err(invalid("duplicate or invalid table identity"));
        }
        for column in table.columns.into_iter().flatten() {
            input.checkpoint().await?;
            if self
                .columns
                .insert_admitted_key_from(
                    Budgeted::new((table.object_id, column.name.as_str()), memory.reserve(0)?),
                    (),
                    input,
                )
                .await?
                .is_some()
            {
                return Err(invalid("duplicate live column"));
            }
        }
        for key in table.keys {
            input.checkpoint().await?;
            if let Some(identity) = key.catalog_identity {
                if !identity.is_valid()
                    || self
                        .constraints
                        .insert(identity.object_id, (table, key))?
                        .is_some()
                {
                    return Err(invalid("duplicate or invalid constraint identity"));
                }
            } else if key.index_identity.is_some() {
                return Err(invalid("indexed constraint has no identity"));
            }
        }
        Ok(())
    }

    async fn retire(
        self,
        control: &ProductionControl<'_>,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        retire_entries(self.columns.into_iter(), control, work).await?;
        retire_entries(self.constraints.into_entries(), control, work).await?;
        retire_entries(self.ids.into_entries(), control, work).await?;
        retire_entries(self.names.into_iter(), control, work).await
    }
}

type ColumnName<'a> = ([u8; 16], &'a str);

async fn column_references<W: Work + Send>(
    table: NativeIndexTable<'_>,
    text: &str,
    columns: &OwnedMap<ColumnName<'_>, ()>,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<bool, SQLError> {
    let mut keys = ColumnKeys::new(text, input.control, input.work).await?;
    let mut ordinary = true;
    while let Some(key) = keys.next(input.work).await? {
        match key {
            StoredColumnKey::Column(name) => {
                if table.columns.is_some()
                    && columns
                        .get_from(&(table.object_id, name.as_str()), input)
                        .await?
                        .is_none()
                {
                    return Err(invalid("missing live column"));
                }
            }
            StoredColumnKey::Expression => ordinary = false,
        }
    }
    Ok(ordinary)
}

async fn ownership_edges<W: Work + Send>(
    indexes: &BudgetedMap<[u8; 16], Index<'_>>,
    constraints: &BudgetedMap<[u8; 16], (NativeIndexTable<'_>, &TableKeyConstraint)>,
    memory: &MemoryBudget,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<(), SQLError> {
    let mut physical_keys = OwnedMap::new();
    let mut owned_constraints = BudgetedMap::new(memory);
    for (_, index) in indexes {
        input.checkpoint().await?;
        let catalog = index.metadata.catalog.as_ref().expect("addressed index");
        if physical_keys
            .insert_admitted_key_from(
                Budgeted::new(
                    (index.table.object_id, catalog.physical_key.as_str()),
                    memory.reserve(0)?,
                ),
                (),
                input,
            )
            .await?
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
            if table.object_id != index.table.object_id
                || key
                    .index_identity
                    .is_some_and(|identity| identity != catalog.identity)
                || !index.metadata.unique
                || index.metadata.predicate.is_some()
                || index.metadata.nulls_not_distinct != key.nulls_not_distinct
                || !columns_match_with_work(
                    Some(&index.row.columns_json),
                    &key.columns,
                    input.control,
                    input.work,
                )
                .await?
                || !columns_match_with_work(
                    index.metadata.included_columns,
                    &key.included_columns,
                    input.control,
                    input.work,
                )
                .await?
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
                input,
            )
            .await?;
        }
    }
    for (id, (table, key)) in constraints {
        input.checkpoint().await?;
        let Some(identity) = key.index_identity else {
            continue;
        };
        let index = indexes
            .get(&identity.object_id)
            .ok_or_else(|| invalid("missing constraint index"))?;
        if !identity.is_valid()
            || index.table.object_id != table.object_id
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
    retire_entries(owned_constraints.into_entries(), input.control, input.work).await?;
    retire_entries(physical_keys.into_iter(), input.control, input.work).await
}

async fn parent_edge<W: Work + Send>(
    child: &Index<'_>,
    parent: &Index<'_>,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<(), SQLError> {
    let hierarchy = child.table.hierarchy;
    if !hierarchy.is_partition()
        || hierarchy.parents.len() != 1
        || parent.table.hierarchy.partition_spec.is_none()
    {
        return Err(invalid("parent index outside a partition edge"));
    }
    let name =
        bound_relation_identity_with_work(&hierarchy.parents[0], input.control, input.work).await?;
    if input.compare(&*name, &parent.table.name).await?.is_ne() {
        return Err(invalid("parent index belongs to another table"));
    }
    Ok(())
}

// Each index has one pre-admitted OID slot. Its traversal color follows that index across both parent walks.
async fn acyclic<W: Work + Send>(
    indexes: &BudgetedMap<[u8; 16], Index<'_>>,
    colors: &mut BudgetedMap<i64, u8>,
    input: &mut ReferenceWork<'_, '_, W>,
) -> Result<(), SQLError> {
    for (start, _) in indexes {
        input.checkpoint().await?;
        let mut cursor = Some(*start);
        while let Some(id) = cursor {
            input.checkpoint().await?;
            let index = indexes.get(&id).expect("validated parent edge");
            let color = colors
                .get_mut(
                    &index
                        .metadata
                        .catalog
                        .as_ref()
                        .expect("addressed index")
                        .identity
                        .oid,
                )
                .expect("validated OID");
            match *color {
                1 => return Err(invalid("cyclic index ancestry")),
                2 => break,
                _ => *color = 1,
            }
            cursor = index.metadata.relationships.parent_index;
        }
        cursor = Some(*start);
        while let Some(id) = cursor {
            input.checkpoint().await?;
            let index = indexes.get(&id).expect("validated parent edge");
            let color = colors
                .get_mut(
                    &index
                        .metadata
                        .catalog
                        .as_ref()
                        .expect("addressed index")
                        .identity
                        .oid,
                )
                .expect("validated OID");
            if std::mem::replace(color, 2) == 2 {
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
