//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate captured constraint references against one immutable native catalog.

use crate::{
    ast::{
        ColumnDef, CreateDomain, Expr, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint,
    },
    catalog::{
        events::StoredTrigger,
        index::stored::input::{metadata_with_work, IndexMetadata},
        resolution::bound_relation_identity_with_work,
        work::{inline, retire_entries, ReferenceWork},
        CatalogWork,
    },
    SQLError,
};
use uqa_core::{
    catalog_index::CatalogIndexRow,
    memory::{Budgeted, BudgetedMap, MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity,
};

pub mod bindings;
mod foreign_keys;
mod identities;
mod partitions;

#[derive(Clone, Copy)]
pub struct NativeConstraintTable<'a> {
    pub name: &'a RelationIdentity,
    pub object_id: [u8; 16],
    pub columns: &'a [ColumnDef],
    pub checks: &'a [TableCheck],
    pub keys: &'a [TableKeyConstraint],
    pub foreign_keys: &'a [ForeignKey],
    /// Foreign tables have no ordinary inheritance or partition owner.
    pub hierarchy: Option<&'a TableHierarchy>,
}

struct Index<'a> {
    row: &'a CatalogIndexRow,
    metadata: IndexMetadata<'a>,
}

struct Catalog<'a> {
    tables: OwnedMap<&'a RelationIdentity, NativeConstraintTable<'a>>,
    identities: BudgetedMap<[u8; 16], ()>,
    columns: OwnedMap<([u8; 16], &'a str), &'a ColumnDef>,
    indexes: BudgetedMap<[u8; 16], Index<'a>>,
    memory: &'a MemoryBudget,
}

/// Validate supplied constraint identities across ordinary/foreign tables, domains and constraint triggers, live key/CHECK columns and original foreign-key targets without inferring identities, evaluating expressions, reading rows or changing validation flags. Index ownership and full expression typing are separate predicates. Both ordinary and foreign CHECK declarations use their captured row layout.
pub fn validate<'a>(
    tables: impl Iterator<Item = NativeConstraintTable<'a>>,
    indexes: impl Iterator<Item = &'a CatalogIndexRow>,
    domains: impl Iterator<Item = &'a CreateDomain>,
    triggers: impl Iterator<Item = &'a StoredTrigger>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<(), SQLError> {
    inline(validate_with_work(
        tables,
        indexes,
        domains,
        triggers,
        memory,
        cancellation,
        &mut (),
    ))
}

/// Validate and retire the same borrowed constraints within the caller's work windows.
pub async fn validate_with_work<'a, W: CatalogWork + Send>(
    tables: impl Iterator<Item = NativeConstraintTable<'a>>,
    indexes: impl Iterator<Item = &'a CatalogIndexRow>,
    domains: impl Iterator<Item = &'a CreateDomain>,
    triggers: impl Iterator<Item = &'a StoredTrigger>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
    work: &mut W,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let mut input = ReferenceWork {
        control: &control,
        work,
    };
    let mut catalog = Catalog {
        tables: OwnedMap::new(),
        identities: BudgetedMap::new(memory),
        columns: OwnedMap::new(),
        indexes: BudgetedMap::new(memory),
        memory,
    };
    let mut constraints = identities::Identities::new(memory);
    for table in tables {
        catalog.insert(table, &mut input).await?;
        constraints.table(table, &mut input).await?;
    }
    for domain in domains {
        constraints.domain(domain, &mut input).await?;
    }
    for trigger in triggers {
        constraints.trigger(trigger, &mut input).await?;
    }
    constraints.retire(&mut input).await?;
    for row in indexes {
        input.checkpoint().await?;
        let metadata =
            metadata_with_work(row.definition_json.as_deref(), &control, input.work).await?;
        if let Some(address) = &metadata.catalog {
            if catalog
                .indexes
                .insert(address.identity.object_id, Index { row, metadata })?
                .is_some()
            {
                return Err(invalid("duplicate index identity"));
            }
        }
    }
    let partitions = partitions::Partitions::new(&catalog, &mut input).await?;
    for (_, table) in &catalog.tables {
        input.checkpoint().await?;
        catalog.table(*table, &partitions, &mut input).await?;
    }
    control.check()?;
    partitions.retire(&mut input).await?;
    retire_entries(catalog.indexes.into_entries(), &control, work).await?;
    retire_entries(catalog.columns.into_iter(), &control, work).await?;
    retire_entries(catalog.identities.into_entries(), &control, work).await?;
    retire_entries(catalog.tables.into_iter(), &control, work).await
}

impl<'a> Catalog<'a> {
    async fn insert(
        &mut self,
        table: NativeConstraintTable<'a>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        input.checkpoint().await?;
        if table.object_id == [0; 16]
            || self
                .tables
                .insert_admitted_key_from(
                    Budgeted::new(table.name, self.memory.reserve(0)?),
                    table,
                    input,
                )
                .await?
                .is_some()
            || self.identities.insert(table.object_id, ())?.is_some()
        {
            return Err(invalid("duplicate or invalid relation"));
        }
        for column in table.columns {
            let key = Budgeted::new(
                (table.object_id, column.name.as_str()),
                self.memory.reserve(0)?,
            );
            if self
                .columns
                .insert_admitted_key_from(key, column, input)
                .await?
                .is_some()
            {
                return Err(invalid("duplicate live column"));
            }
        }
        Ok(())
    }

    async fn column(
        &self,
        table: NativeConstraintTable<'a>,
        name: &str,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<&'a ColumnDef, SQLError> {
        self.columns
            .get_from(&(table.object_id, name), input)
            .await?
            .copied()
            .ok_or_else(|| invalid("missing live column"))
    }

    async fn table(
        &self,
        table: NativeConstraintTable<'a>,
        partitions: &partitions::Partitions<'_>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        for key in table.keys {
            input.checkpoint().await?;
            if key.columns.is_empty() {
                return Err(invalid("empty key"));
            }
            self.column_list(table, &key.columns, true, input).await?;
            self.column_list(table, &key.included_columns, false, input)
                .await?;
        }
        for check in table.checks {
            self.expression(table, &check.expr, input).await?;
            if let Some(partition) = &check.partition_constraint {
                for key in &partition.spec.keys {
                    self.expression(table, key, input).await?;
                }
            }
        }
        for column in table.columns {
            input.checkpoint().await?;
            if let Some(check) = &column.check {
                self.expression(table, check, input).await?;
            }
            if let Some(reference) = &column.references {
                self.foreign_key(
                    table,
                    foreign_keys::Reference::column(column, reference),
                    partitions,
                    input,
                )
                .await?;
            }
        }
        for key in table.foreign_keys {
            self.foreign_key(
                table,
                foreign_keys::Reference::table(key),
                partitions,
                input,
            )
            .await?;
        }
        if let Some(hierarchy) = table.hierarchy {
            if let Some(partition) = &hierarchy.partition_spec {
                for key in &partition.keys {
                    self.expression(table, key, input).await?;
                }
            }
            for inherited in &hierarchy.partition_inherited_foreign_keys {
                input.checkpoint().await?;
                let mut found = false;
                for key in table.foreign_keys {
                    input.checkpoint().await?;
                    if key.catalog_identity == inherited.catalog_identity
                        && key.object_id == inherited.object_id
                    {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(invalid("partition foreign-key provenance"));
                }
            }
        }
        Ok(())
    }

    async fn column_list(
        &self,
        table: NativeConstraintTable<'a>,
        names: &[String],
        distinct: bool,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        let mut seen = OwnedMap::new();
        for name in names {
            self.column(table, name, input).await?;
            if distinct
                && seen
                    .insert_admitted_key_from(
                        Budgeted::new(name.as_str(), self.memory.reserve(0)?),
                        (),
                        input,
                    )
                    .await?
                    .is_some()
            {
                return Err(invalid("duplicate key column"));
            }
        }
        retire_entries(seen.into_iter(), input.control, input.work).await
    }

    async fn expression(
        &self,
        table: NativeConstraintTable<'a>,
        expression: &Expr,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        let mut nodes = expression.stored_nodes(self.memory)?;
        while let Some(node) = nodes.next(input.control, input.work).await? {
            match node {
                Expr::Column(name) | Expr::QualifiedColumn { column: name, .. }
                    if name != "tableoid" =>
                {
                    self.column(table, name, input).await?;
                }
                Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
                    return Err(crate::schema::dependencies::subquery_error(input.control));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native constraint reference: {reason}"))
}

#[cfg(test)]
mod tests;
