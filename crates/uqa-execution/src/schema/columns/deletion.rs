//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ATExecDropColumn`: a column is removed with the copies its inheritors have from no other source, in one deletion.

use crate::catalog::projection::CatalogDependencies;
use crate::schema::deletion::{perform_deletion, required_address, CatalogRemovalContext};
use uqa_core::RelationIdentity;
use uqa_sql::ast::{ColumnDef, TableHierarchy};
use uqa_sql::catalog::dependencies::{DependencyKind, ObjectAddress, RELATION_CLASS};
use uqa_sql::SQLError;

/// A table of the catalog the statement sees.
pub struct TableEntry {
    pub identity: RelationIdentity,
    pub columns: Vec<ColumnDef>,
    pub hierarchy: TableHierarchy,
}

/// The tables of the catalog, with the parents each names.
struct Tables(Vec<TableEntry>);

impl Tables {
    fn get(&self, identity: &RelationIdentity) -> Option<&TableEntry> {
        self.0.iter().find(|table| table.identity == *identity)
    }

    /// `find_inheritance_children`: the tables that name `parent` as a direct parent.
    fn children(&self, parent: &RelationIdentity) -> Vec<&TableEntry> {
        self.0
            .iter()
            .filter(|table| {
                table.hierarchy.parents.iter().any(|name| {
                    RelationIdentity::from_legacy_name(name).is_ok_and(|name| name == *parent)
                })
            })
            .collect()
    }

    /// `attinhcount`: how many direct parents have the column.
    fn inheritance_count(&self, table: &TableEntry, column: &str) -> usize {
        table
            .hierarchy
            .parents
            .iter()
            .filter_map(|name| RelationIdentity::from_legacy_name(name).ok())
            .filter_map(|parent| self.get(&parent))
            .filter(|parent| {
                parent
                    .columns
                    .iter()
                    .any(|candidate| candidate.name == column)
            })
            .count()
    }
}

/// `attislocal`: whether the table declares the column itself.
fn is_local(tables: &Tables, table: &TableEntry, column: &str) -> bool {
    if table.hierarchy.local_columns.is_empty() {
        tables.inheritance_count(table, column) == 0
    } else {
        table
            .hierarchy
            .local_columns
            .iter()
            .any(|local| local == column)
    }
}

/// An inheritor that keeps its copy of a column dropped with `ONLY`, and the hierarchy that declares the copy as its own.
pub struct LocalizedColumn {
    pub table: RelationIdentity,
    pub columns: Vec<ColumnDef>,
    pub hierarchy: TableHierarchy,
}

/// The outcome of dropping a column.
pub enum ColumnDrop {
    Dropped,
    /// An `IF EXISTS` column is missing from the relation of this name.
    Missing(String),
}

/// Drop `column` of `table` and, unless `recurse` is false, the copies its inheritors do not declare or inherit elsewhere. With `ONLY`, every inheritor keeps its copy as its own column, which `localize` publishes.
pub fn drop_column(
    context: &CatalogRemovalContext<'_>,
    table: &str,
    column: &str,
    (if_exists, cascade, recurse): (bool, bool, bool),
    localize: &mut dyn FnMut(LocalizedColumn) -> Result<(), SQLError>,
) -> Result<ColumnDrop, SQLError> {
    let catalog = context.catalog.catalog_read_view();
    let tables = Tables(
        catalog
            .snapshot()
            .tables
            .iter()
            .map(|(identity, table)| TableEntry {
                identity: identity.clone(),
                columns: table.columns.as_ref().clone(),
                hierarchy: table.hierarchy.as_ref().clone(),
            })
            .collect(),
    );
    drop(catalog);
    let dependencies = crate::schema::deletion::catalog_dependencies(&context.catalog)?;
    let identity = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let mut originals = Vec::new();
    let mut localized = Vec::new();
    let search = ColumnSearch {
        tables: &tables,
        dependencies: &dependencies,
        recurse,
    };
    if !search.collect(
        &identity,
        column,
        if_exists,
        false,
        &mut originals,
        &mut localized,
    )? {
        return Ok(ColumnDrop::Missing(identity.name));
    }
    for child in localized {
        if let Some(localized) = localized_column(&tables, &child, column)? {
            localize(localized)?;
        }
    }
    perform_deletion(
        context,
        |dependencies| {
            originals
                .iter()
                .map(|relation: &RelationIdentity| {
                    required_address(
                        dependencies.relation_address(relation, Some(column)),
                        || format!("column {column} of {}", relation.qualified_name()),
                    )
                })
                .collect()
        },
        cascade,
    )?;
    Ok(ColumnDrop::Dropped)
}

struct ColumnSearch<'a> {
    tables: &'a Tables,
    dependencies: &'a CatalogDependencies,
    recurse: bool,
}

impl ColumnSearch<'_> {
    /// Add the column of `table`, after the copies of its inheritors that go with it, as `ATExecDropColumn` recurses one level at a time. Returns false when an `IF EXISTS` column is missing.
    fn collect(
        &self,
        table: &RelationIdentity,
        column: &str,
        if_exists: bool,
        recursing: bool,
        originals: &mut Vec<RelationIdentity>,
        localized: &mut Vec<RelationIdentity>,
    ) -> Result<bool, SQLError> {
        let entry = self
            .tables
            .get(table)
            .ok_or_else(|| SQLError::UnknownTable(table.qualified_name()))?;
        if uqa_sql::schema::columns::POSTGRES_SYSTEM_COLUMNS.contains(&column) {
            return Err(error(
                "0A000",
                format!("cannot drop system column \"{column}\""),
            ));
        }
        if !entry
            .columns
            .iter()
            .any(|candidate| candidate.name == column)
        {
            if if_exists && !recursing {
                return Ok(false);
            }
            return Err(error(
                "42703",
                format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    entry.identity.name
                ),
            ));
        }
        if !recursing && self.tables.inheritance_count(entry, column) > 0 {
            return Err(error(
                "42P16",
                format!("cannot drop inherited column \"{column}\""),
            ));
        }
        if self.is_partition_key_column(table, column) {
            return Err(error(
                "42P16",
                format!(
                    "cannot drop column \"{column}\" because it is part of the partition key of relation \"{}\"",
                    entry.identity.name
                ),
            ));
        }
        let children = self.tables.children(table);
        if !children.is_empty() && entry.hierarchy.partition_spec.is_some() && !self.recurse {
            return Err(SQLError::Diagnostic {
                sqlstate: "42P16".into(),
                message: "cannot drop column from only the partitioned table when partitions exist"
                    .into(),
                detail: None,
                hint: Some("Do not specify the ONLY keyword.".into()),
            });
        }
        for child in children {
            if !self.recurse {
                localized.push(child.identity.clone());
            } else if self.tables.inheritance_count(child, column) == 1
                && !is_local(self.tables, child, column)
            {
                self.collect(&child.identity, column, false, true, originals, localized)?;
            }
        }
        if !originals.contains(table) {
            originals.push(table.clone());
        }
        Ok(true)
    }

    /// `has_partition_attrs`: the partition key uses the column, which is then part of the table.
    fn is_partition_key_column(&self, table: &RelationIdentity, column: &str) -> bool {
        let Some(address) = self.dependencies.relation_address(table, Some(column)) else {
            return false;
        };
        let relation = ObjectAddress::whole(RELATION_CLASS, address.object_id);
        self.dependencies
            .graph()
            .references_of(address)
            .any(|edge| edge.kind == DependencyKind::Internal && edge.referenced == relation)
    }
}

/// The hierarchy with which an inheritor declares its copy of the column as its own; `None` when it already does.
fn localized_column(
    tables: &Tables,
    child: &RelationIdentity,
    column: &str,
) -> Result<Option<LocalizedColumn>, SQLError> {
    let entry = tables
        .get(child)
        .ok_or_else(|| SQLError::UnknownTable(child.qualified_name()))?;
    if is_local(tables, entry, column) {
        return Ok(None);
    }
    let mut hierarchy = entry.hierarchy.clone();
    if hierarchy.local_columns.is_empty() {
        hierarchy.local_columns = entry
            .columns
            .iter()
            .filter(|candidate| is_local(tables, entry, &candidate.name))
            .map(|candidate| candidate.name.clone())
            .collect();
    }
    hierarchy.local_columns.push(column.to_string());
    Ok(Some(LocalizedColumn {
        table: child.clone(),
        columns: entry.columns.clone(),
        hierarchy,
    }))
}

fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
