//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate captured constraint references against one immutable native catalog.

use crate::{
    ast::{ColumnDef, Expr, ForeignKey, TableCheck, TableHierarchy, TableKeyConstraint},
    catalog::{
        index::stored::input::{metadata, IndexMetadata},
        resolution::bound_relation_identity_with_control,
    },
    SQLError,
};
use uqa_core::{
    catalog_index::CatalogIndexRow,
    memory::{BudgetedMap, MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};

mod foreign_keys;

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
    tables: BudgetedMap<&'a RelationIdentity, NativeConstraintTable<'a>>,
    identities: BudgetedMap<[u8; 16], NativeConstraintTable<'a>>,
    columns: BudgetedMap<([u8; 16], &'a str), &'a ColumnDef>,
    indexes: BudgetedMap<[u8; 16], Index<'a>>,
    memory: &'a MemoryBudget,
    control: ProductionControl<'a>,
}

/// Validate live key/CHECK columns and original foreign-key targets without inferring identities, evaluating expressions, reading rows or changing validation flags. Index ownership and full expression typing are separate predicates. Both ordinary and foreign CHECK declarations use their captured row layout.
pub fn validate<'a>(
    tables: impl Iterator<Item = NativeConstraintTable<'a>>,
    indexes: impl Iterator<Item = &'a CatalogIndexRow>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
) -> Result<(), SQLError> {
    let mut catalog = Catalog {
        tables: BudgetedMap::new(memory),
        identities: BudgetedMap::new(memory),
        columns: BudgetedMap::new(memory),
        indexes: BudgetedMap::new(memory),
        memory,
        control: ProductionControl::new(memory, cancellation, cancellation),
    };
    catalog.control.check()?;
    for table in tables {
        catalog.control.check()?;
        if table.object_id == [0; 16]
            || catalog.tables.insert(table.name, table)?.is_some()
            || catalog.identities.insert(table.object_id, table)?.is_some()
        {
            return Err(invalid("duplicate or invalid relation"));
        }
        for column in table.columns {
            catalog.control.check()?;
            if catalog
                .columns
                .insert((table.object_id, column.name.as_str()), column)?
                .is_some()
            {
                return Err(invalid("duplicate live column"));
            }
        }
    }
    for row in indexes {
        catalog.control.check()?;
        let metadata = metadata(row.definition_json.as_deref(), &catalog.control)?;
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
    for (_, table) in &catalog.tables {
        catalog.table(*table)?;
    }
    catalog.control.check()?;
    Ok(())
}

impl<'a> Catalog<'a> {
    fn column(
        &self,
        table: NativeConstraintTable<'a>,
        name: &str,
    ) -> Result<&'a ColumnDef, SQLError> {
        self.control.check()?;
        self.columns
            .get(&(table.object_id, name))
            .copied()
            .ok_or_else(|| invalid("missing live column"))
    }

    fn table(&self, table: NativeConstraintTable<'a>) -> Result<(), SQLError> {
        for key in table.keys {
            if key.columns.is_empty() {
                return Err(invalid("empty key"));
            }
            self.column_list(table, &key.columns, true)?;
            self.column_list(table, &key.included_columns, false)?;
        }
        for check in table.checks {
            self.expression(table, &check.expr)?;
            if let Some(partition) = &check.partition_constraint {
                for key in &partition.spec.keys {
                    self.expression(table, key)?;
                }
            }
        }
        for column in table.columns {
            if let Some(check) = &column.check {
                self.expression(table, check)?;
            }
            if let Some(reference) = &column.references {
                self.foreign_key(table, foreign_keys::Reference::column(column, reference))?;
            }
        }
        for key in table.foreign_keys {
            self.foreign_key(table, foreign_keys::Reference::table(key))?;
        }
        if let Some(hierarchy) = table.hierarchy {
            if let Some(partition) = &hierarchy.partition_spec {
                for key in &partition.keys {
                    self.expression(table, key)?;
                }
            }
            for inherited in &hierarchy.partition_inherited_foreign_keys {
                self.control.check()?;
                if !table.foreign_keys.iter().any(|key| {
                    key.catalog_identity == inherited.catalog_identity
                        && key.object_id == inherited.object_id
                }) {
                    return Err(invalid("partition foreign-key provenance"));
                }
            }
        }
        Ok(())
    }

    fn column_list(
        &self,
        table: NativeConstraintTable<'a>,
        names: &[String],
        distinct: bool,
    ) -> Result<(), SQLError> {
        let mut seen = BudgetedMap::new(self.memory);
        for name in names {
            self.column(table, name)?;
            if distinct && seen.insert(name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate key column"));
            }
        }
        Ok(())
    }

    fn expression(
        &self,
        table: NativeConstraintTable<'a>,
        expression: &Expr,
    ) -> Result<(), SQLError> {
        crate::schema::dependencies::walk_schema_expr_with_control(
            expression,
            &self.control,
            &mut |node| {
                if let Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } = node {
                    if name != "tableoid" {
                        self.column(table, name)?;
                    }
                }
                Ok(())
            },
        )
    }
}

fn invalid(reason: &str) -> SQLError {
    SQLError::Internal(format!("invalid native constraint reference: {reason}"))
}

#[cfg(test)]
mod tests;
