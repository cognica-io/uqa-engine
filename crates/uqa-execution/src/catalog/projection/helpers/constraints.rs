//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint catalog row and state models.

use uqa_sql::ast::{
    ColumnDef as SQLColumnDef, ForeignKey, ForeignKeyAction, ForeignKeyMatch,
    TableKeyConstraintKind,
};
use uqa_sql::SQLError;

use crate::catalog::{CatalogReadView, RelationNameResolution};

use super::dependencies::{check_constraint_columns, named_constraint_columns};
use super::oids::split_schema_name;
use super::rows::catalog_ordinal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintCatalogKind {
    PrimaryKey,
    Unique { nulls_not_distinct: bool },
    ForeignKey,
    Check,
    NotNull,
}

impl ConstraintCatalogKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::PrimaryKey => "PRIMARY KEY",
            Self::Unique { .. } => "UNIQUE",
            Self::ForeignKey => "FOREIGN KEY",
            Self::Check => "CHECK",
            Self::NotNull => "NOT NULL",
        }
    }

    pub const fn pg_type(self) -> &'static str {
        match self {
            Self::PrimaryKey => "p",
            Self::Unique { .. } => "u",
            Self::ForeignKey => "f",
            Self::Check => "c",
            Self::NotNull => "n",
        }
    }

    pub const fn nulls_distinct(self) -> Option<bool> {
        match self {
            Self::Unique { nulls_not_distinct } => Some(!nulls_not_distinct),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConstraintCatalogColumn {
    pub name: String,
    pub table_ordinal: i64,
}

#[derive(Debug, Clone)]
pub struct ForeignKeyCatalogData {
    pub referenced_index: Option<[u8; 16]>,
    pub schema: String,
    pub table: String,
    pub column_ordinals: Vec<i64>,
    pub positions_in_unique_constraint: Vec<Option<i64>>,
    pub on_update: ForeignKeyAction,
    pub on_delete: ForeignKeyAction,
    pub match_type: ForeignKeyMatch,
}

#[derive(Debug, Clone)]
pub struct ConstraintCatalogRow {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub object_id: Option<[u8; 16]>,
    pub catalog_oid: Option<i64>,
    pub kind: ConstraintCatalogKind,
    pub columns: Vec<ConstraintCatalogColumn>,
    pub state: ConstraintCatalogState,
    pub period: bool,
    pub foreign_key: Option<ForeignKeyCatalogData>,
    /// The expression of a CHECK constraint.
    pub expression: Option<uqa_sql::ast::Expr>,
    /// The catalog row of the constraint this one derives from, which `pg_constraint.conparentid` names; such a constraint is not local and is inherited once.
    pub parent_oid: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConstraintCatalogState {
    validation: ConstraintValidationState,
    deferral: ConstraintDeferralState,
    inheritance: ConstraintInheritanceState,
}

#[derive(Debug, Clone, Copy)]
pub struct ConstraintValidationState {
    enforced: bool,
    validated: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ConstraintDeferralState {
    NotDeferrable,
    InitiallyImmediate,
    InitiallyDeferred,
}

#[derive(Debug, Clone, Copy)]
pub enum ConstraintInheritanceState {
    Inheritable,
    NoInherit,
}

impl ConstraintCatalogState {
    pub const fn new(
        validation: ConstraintValidationState,
        deferral: ConstraintDeferralState,
        inheritance: ConstraintInheritanceState,
    ) -> Self {
        Self {
            validation,
            deferral,
            inheritance,
        }
    }

    pub const fn enforced(self) -> bool {
        self.validation.enforced
    }

    pub const fn validated(self) -> bool {
        self.validation.validated
    }

    pub const fn deferrable(self) -> bool {
        !matches!(self.deferral, ConstraintDeferralState::NotDeferrable)
    }

    pub const fn initially_deferred(self) -> bool {
        matches!(self.deferral, ConstraintDeferralState::InitiallyDeferred)
    }

    pub const fn no_inherit(self) -> bool {
        matches!(self.inheritance, ConstraintInheritanceState::NoInherit)
    }
}

impl ConstraintValidationState {
    pub const fn new(enforced: bool, validated: bool) -> Self {
        Self {
            enforced,
            validated,
        }
    }
}

impl ConstraintDeferralState {
    pub const fn new(deferrable: bool, initially_deferred: bool) -> Self {
        if !deferrable {
            ConstraintDeferralState::NotDeferrable
        } else if initially_deferred {
            ConstraintDeferralState::InitiallyDeferred
        } else {
            ConstraintDeferralState::InitiallyImmediate
        }
    }
}

impl ConstraintInheritanceState {
    pub const fn new(no_inherit: bool) -> Self {
        if no_inherit {
            ConstraintInheritanceState::NoInherit
        } else {
            ConstraintInheritanceState::Inheritable
        }
    }
}

#[derive(Debug)]
pub struct PendingConstraintCatalogRow {
    pub schema: String,
    pub table: String,
    pub requested_name: Option<String>,
    pub object_id: Option<[u8; 16]>,
    pub catalog_oid: Option<i64>,
    pub kind: ConstraintCatalogKind,
    pub columns: Vec<ConstraintCatalogColumn>,
    pub state: ConstraintCatalogState,
    pub period: bool,
    pub foreign_key: Option<ForeignKeyCatalogData>,
    pub expression: Option<uqa_sql::ast::Expr>,
    pub parent_oid: Option<i64>,
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub fn constraint_catalog_rows(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ConstraintCatalogRow>, SQLError> {
    let mut out = Vec::new();
    for table_name in catalog.table_names() {
        let (schema, table) = split_schema_name(&table_name)?;
        let table_snapshot = catalog
            .table(resolution, &table_name)?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let columns = table_snapshot.columns.clone();
        let mut pending = Vec::new();

        for (idx, col) in columns.iter().enumerate() {
            let ordinal = catalog_ordinal(idx, "constraint column")?;
            if col.not_null {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: col.not_null_name.clone(),
                    object_id: col.not_null_identity.map(|identity| identity.object_id),
                    catalog_oid: Some(
                        col.not_null_identity
                            .ok_or_else(|| {
                                SQLError::Internal(
                                    "NOT NULL constraint has no catalog identity".into(),
                                )
                            })?
                            .oid,
                    ),
                    kind: ConstraintCatalogKind::NotNull,
                    columns: vec![ConstraintCatalogColumn {
                        name: col.name.clone(),
                        table_ordinal: ordinal,
                    }],
                    state: ConstraintCatalogState::new(
                        ConstraintValidationState::new(true, col.not_null_validated),
                        ConstraintDeferralState::new(false, false),
                        ConstraintInheritanceState::new(col.not_null_no_inherit),
                    ),
                    period: false,
                    foreign_key: None,
                    expression: None,
                    parent_oid: None,
                });
            }
            if let Some(expr) = &col.check {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: col.check_name.clone(),
                    object_id: col.check_object_id,
                    catalog_oid: col.check_catalog_oid,
                    kind: ConstraintCatalogKind::Check,
                    columns: check_constraint_columns(expr, &columns, &table_name)?,
                    state: ConstraintCatalogState::new(
                        ConstraintValidationState::new(col.check_enforced, col.check_validated),
                        ConstraintDeferralState::new(false, false),
                        ConstraintInheritanceState::new(col.check_no_inherit),
                    ),
                    period: false,
                    foreign_key: None,
                    expression: Some(expr.clone()),
                    parent_oid: None,
                });
            }
            if let Some(reference) = &col.references {
                let foreign_key = uqa_sql::schema::foreign_keys::column_foreign_key(col, reference);
                pending.push(foreign_key_catalog_row(
                    catalog,
                    resolution,
                    &schema,
                    &table,
                    &table_name,
                    &columns,
                    &foreign_key,
                )?);
                pending.extend(derived_constraint_catalog_rows(
                    catalog,
                    &schema,
                    &table,
                    &columns,
                    &foreign_key,
                )?);
            }
        }

        let mut key_constraints = table_snapshot.keys.as_ref().clone();
        for column in columns.iter() {
            let kind = if column.primary_key {
                Some(TableKeyConstraintKind::PrimaryKey)
            } else if column.unique {
                Some(TableKeyConstraintKind::Unique)
            } else {
                None
            };
            let Some(kind) = kind else {
                continue;
            };
            if key_constraints.iter().any(|constraint| {
                constraint.kind == kind
                    && constraint.columns.as_slice() == std::slice::from_ref(&column.name)
            }) {
                continue;
            }
            key_constraints.push(uqa_sql::ast::TableKeyConstraint {
                catalog_identity: None,
                index_identity: None,
                name: None,
                kind,
                columns: vec![column.name.clone()],
                included_columns: Vec::new(),
                nulls_not_distinct: false,
                without_overlaps: false,
            });
        }
        for constraint in key_constraints {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: constraint.name,
                object_id: constraint
                    .catalog_identity
                    .map(|identity| identity.object_id),
                catalog_oid: constraint.catalog_identity.map(|identity| identity.oid),
                kind: match constraint.kind {
                    TableKeyConstraintKind::PrimaryKey => ConstraintCatalogKind::PrimaryKey,
                    TableKeyConstraintKind::Unique => ConstraintCatalogKind::Unique {
                        nulls_not_distinct: constraint.nulls_not_distinct,
                    },
                },
                columns: named_constraint_columns(&constraint.columns, &columns, &table_name)?,
                state: ConstraintCatalogState::new(
                    ConstraintValidationState::new(true, true),
                    ConstraintDeferralState::new(false, false),
                    ConstraintInheritanceState::new(true),
                ),
                period: constraint.without_overlaps,
                foreign_key: None,
                expression: None,
                parent_oid: None,
            });
        }

        for constraint in table_snapshot.checks.iter() {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: constraint.name.clone(),
                object_id: constraint.object_id,
                catalog_oid: constraint.catalog_oid,
                kind: ConstraintCatalogKind::Check,
                columns: check_constraint_columns(&constraint.expr, &columns, &table_name)?,
                state: ConstraintCatalogState::new(
                    ConstraintValidationState::new(constraint.enforced, constraint.validated),
                    ConstraintDeferralState::new(false, false),
                    ConstraintInheritanceState::new(constraint.no_inherit),
                ),
                period: false,
                foreign_key: None,
                expression: Some(constraint.expr.clone()),
                parent_oid: None,
            });
        }

        for foreign_key in table_snapshot.foreign_keys.iter() {
            pending.push(foreign_key_catalog_row(
                catalog,
                resolution,
                &schema,
                &table,
                &table_name,
                &columns,
                foreign_key,
            )?);
            pending.extend(derived_constraint_catalog_rows(
                catalog,
                &schema,
                &table,
                &columns,
                foreign_key,
            )?);
        }

        for constraint in pending {
            let name = constraint.requested_name.ok_or_else(|| {
                SQLError::Internal(format!(
                    "durable constraint on `{}.{}` has no name",
                    constraint.schema, constraint.table
                ))
            })?;
            out.push(ConstraintCatalogRow {
                schema: constraint.schema,
                table: constraint.table,
                name,
                object_id: constraint.object_id,
                catalog_oid: constraint.catalog_oid,
                kind: constraint.kind,
                columns: constraint.columns,
                state: constraint.state,
                period: constraint.period,
                foreign_key: constraint.foreign_key,
                expression: constraint.expression,
                parent_oid: constraint.parent_oid,
            });
        }
    }
    for (table_name, foreign_table) in catalog.foreign_tables() {
        let (schema, table) = split_schema_name(&table_name)?;
        let columns = foreign_table.columns;
        let mut pending = Vec::new();
        for (idx, column) in columns.iter().enumerate() {
            let ordinal = catalog_ordinal(idx, "foreign-table constraint column")?;
            if column.not_null {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: column.not_null_name.clone(),
                    object_id: column.not_null_identity.map(|identity| identity.object_id),
                    catalog_oid: Some(
                        column
                            .not_null_identity
                            .ok_or_else(|| {
                                SQLError::Internal(
                                    "foreign-table NOT NULL constraint has no catalog identity"
                                        .into(),
                                )
                            })?
                            .oid,
                    ),
                    kind: ConstraintCatalogKind::NotNull,
                    columns: vec![ConstraintCatalogColumn {
                        name: column.name.clone(),
                        table_ordinal: ordinal,
                    }],
                    state: ConstraintCatalogState::new(
                        ConstraintValidationState::new(true, column.not_null_validated),
                        ConstraintDeferralState::new(false, false),
                        ConstraintInheritanceState::new(column.not_null_no_inherit),
                    ),
                    period: false,
                    foreign_key: None,
                    expression: None,
                    parent_oid: None,
                });
            }
            if let Some(expression) = &column.check {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: column.check_name.clone(),
                    object_id: column.check_object_id,
                    catalog_oid: column.check_catalog_oid,
                    kind: ConstraintCatalogKind::Check,
                    columns: check_constraint_columns(expression, &columns, &table_name)?,
                    state: ConstraintCatalogState::new(
                        ConstraintValidationState::new(
                            column.check_enforced,
                            column.check_validated,
                        ),
                        ConstraintDeferralState::new(false, false),
                        ConstraintInheritanceState::new(column.check_no_inherit),
                    ),
                    period: false,
                    foreign_key: None,
                    expression: Some(expression.clone()),
                    parent_oid: None,
                });
            }
        }
        for check in foreign_table.checks {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: check.name,
                object_id: check.object_id,
                catalog_oid: check.catalog_oid,
                kind: ConstraintCatalogKind::Check,
                columns: check_constraint_columns(&check.expr, &columns, &table_name)?,
                state: ConstraintCatalogState::new(
                    ConstraintValidationState::new(check.enforced, check.validated),
                    ConstraintDeferralState::new(false, false),
                    ConstraintInheritanceState::new(check.no_inherit),
                ),
                period: false,
                foreign_key: None,
                expression: Some(check.expr.clone()),
                parent_oid: None,
            });
        }
        for constraint in pending {
            let name = constraint.requested_name.ok_or_else(|| {
                SQLError::Internal(format!(
                    "durable constraint on `{}.{}` has no name",
                    constraint.schema, constraint.table
                ))
            })?;
            out.push(ConstraintCatalogRow {
                schema: constraint.schema,
                table: constraint.table,
                name,
                object_id: constraint.object_id,
                catalog_oid: constraint.catalog_oid,
                kind: constraint.kind,
                columns: constraint.columns,
                state: constraint.state,
                period: constraint.period,
                foreign_key: constraint.foreign_key,
                expression: constraint.expression,
                parent_oid: constraint.parent_oid,
            });
        }
    }
    Ok(out)
}

fn foreign_key_catalog_row(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    schema: &str,
    table: &str,
    table_name: &str,
    columns: &[SQLColumnDef],
    foreign_key: &ForeignKey,
) -> Result<PendingConstraintCatalogRow, SQLError> {
    let identity = foreign_key
        .catalog_identity
        .filter(|identity| identity.is_valid())
        .ok_or_else(|| SQLError::Internal("FOREIGN KEY has no valid catalog identity".into()))?;
    let local_columns = named_constraint_columns(&foreign_key.local_columns, columns, table_name)?;
    let referenced_name = catalog
        .table_name(resolution, &foreign_key.ref_table)?
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "constraint on table `{table_name}` references missing table `{}`",
                foreign_key.ref_table
            ))
        })?;
    let (referenced_schema, referenced_table) = split_schema_name(&referenced_name)?;
    let referenced = catalog
        .table(resolution, &referenced_name)?
        .ok_or_else(|| SQLError::UnknownTable(referenced_name.clone()))?;
    let referenced_columns = &referenced.columns;
    let referenced_column_rows = named_constraint_columns(
        &foreign_key.ref_columns,
        referenced_columns,
        &referenced_name,
    )?;
    let referenced_key = catalog
        .catalog_indexes()
        .find(|row| {
            crate::catalog::index::index_definition(row)
                .ok()
                .is_some_and(|definition| {
                    definition.catalog.is_some_and(|identity| {
                        Some(identity.identity.object_id) == foreign_key.referenced_index
                    })
                })
        })
        .map(|row| serde_json::from_str::<Vec<uqa_sql::ast::IndexKey>>(&row.columns_json))
        .transpose()
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let positions_in_unique_constraint =
        referenced_key_positions(&foreign_key.ref_columns, referenced_key.as_deref())?;
    Ok(PendingConstraintCatalogRow {
        schema: schema.to_string(),
        table: table.to_string(),
        requested_name: foreign_key.name.clone(),
        object_id: foreign_key.object_id,
        catalog_oid: Some(identity.oid),
        kind: ConstraintCatalogKind::ForeignKey,
        columns: local_columns,
        state: ConstraintCatalogState::new(
            ConstraintValidationState::new(foreign_key.enforced, foreign_key.validated),
            ConstraintDeferralState::new(foreign_key.deferrable, foreign_key.initially_deferred),
            ConstraintInheritanceState::new(true),
        ),
        period: foreign_key.period,
        foreign_key: Some(ForeignKeyCatalogData {
            referenced_index: foreign_key.referenced_index,
            schema: referenced_schema,
            table: referenced_table,
            column_ordinals: referenced_column_rows
                .iter()
                .map(|column| column.table_ordinal)
                .collect(),
            positions_in_unique_constraint,
            on_update: foreign_key.on_update,
            on_delete: foreign_key.on_delete,
            match_type: foreign_key.match_type,
        }),
        expression: None,
        parent_oid: None,
    })
}

/// The positions of the referenced columns in the referenced key's index, as `information_schema.key_column_usage` reports them.
fn referenced_key_positions(
    ref_columns: &[String],
    keys: Option<&[uqa_sql::ast::IndexKey]>,
) -> Result<Vec<Option<i64>>, SQLError> {
    ref_columns
        .iter()
        .map(|column| {
            keys.and_then(|keys| {
                keys.iter()
                    .position(|key| key.column() == Some(column.as_str()))
            })
            .map(|index| catalog_ordinal(index, "referenced key column"))
            .transpose()
        })
        .collect()
}

/// A partition's index that a referenced key's index derives, and its keys.
struct ReferencedKeyIndex {
    index: [u8; 16],
    keys: Vec<uqa_sql::ast::IndexKey>,
}

/// The index of `partition` derived from the referenced key's index `referenced` through the chain of parent indexes, and its keys.
fn partition_referenced_key(
    catalog: &CatalogReadView,
    partition: &str,
    referenced: Option<[u8; 16]>,
) -> Result<Option<ReferencedKeyIndex>, SQLError> {
    let parent_index = catalog
        .catalog_indexes()
        .filter_map(|row| crate::catalog::index::index_definition(row).ok())
        .filter_map(|definition| {
            Some((
                definition.catalog?.identity.object_id,
                definition.relationships.parent_index,
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let derives_from_referenced_key = |mut index: [u8; 16]| {
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(index) {
            if Some(index) == referenced {
                return true;
            }
            match parent_index.get(&index).copied().flatten() {
                Some(parent) => index = parent,
                None => return false,
            }
        }
        false
    };
    for row in catalog
        .catalog_indexes()
        .filter(|row| row.table_name == partition)
    {
        let Some(identity) = crate::catalog::index::index_definition(row)
            .ok()
            .and_then(|definition| definition.catalog)
        else {
            continue;
        };
        if derives_from_referenced_key(identity.identity.object_id) {
            let keys = serde_json::from_str::<Vec<uqa_sql::ast::IndexKey>>(&row.columns_json)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            return Ok(Some(ReferencedKeyIndex {
                index: identity.identity.object_id,
                keys,
            }));
        }
    }
    Ok(None)
}

/// The catalog rows of the constraints `foreign_key` derives on the partitions of the partitioned table it references: rows of the referencing table that reference the partition through its own columns and its index derived from the referenced key, and derive from the foreign key or the parent partition's constraint.
fn derived_constraint_catalog_rows(
    catalog: &CatalogReadView,
    schema: &str,
    table: &str,
    columns: &[SQLColumnDef],
    foreign_key: &ForeignKey,
) -> Result<Vec<PendingConstraintCatalogRow>, SQLError> {
    if foreign_key.referenced_partitions.is_empty() {
        return Ok(Vec::new());
    }
    let table_name = format!("{schema}.{table}");
    let local_columns = named_constraint_columns(&foreign_key.local_columns, columns, &table_name)?;
    let foreign_key_oid = foreign_key
        .catalog_identity
        .ok_or_else(|| SQLError::Internal("FOREIGN KEY has no valid catalog identity".into()))?
        .oid;
    let mut rows = Vec::with_capacity(foreign_key.referenced_partitions.len());
    for derived in &foreign_key.referenced_partitions {
        let (partition, partition_table) = catalog
            .snapshot()
            .tables
            .iter()
            .find(|(_, candidate)| candidate.object_id == derived.partition)
            .ok_or_else(|| SQLError::Internal("referenced partition disappeared".into()))?;
        let partition_name = partition.qualified_name();
        let referenced_column_rows = named_constraint_columns(
            &foreign_key.ref_columns,
            &partition_table.columns,
            &partition_name,
        )?;
        let referenced_key =
            partition_referenced_key(catalog, &partition_name, foreign_key.referenced_index)?;
        let positions_in_unique_constraint = referenced_key_positions(
            &foreign_key.ref_columns,
            referenced_key.as_ref().map(|key| key.keys.as_slice()),
        )?;
        let referenced_index = referenced_key.map(|key| key.index);
        let parent_oid = match derived.parent {
            None => foreign_key_oid,
            Some(parent) => {
                foreign_key
                    .referenced_partitions
                    .iter()
                    .find(|candidate| candidate.partition == parent)
                    .ok_or_else(|| {
                        SQLError::Internal("derived constraint parent disappeared".into())
                    })?
                    .catalog_identity
                    .oid
            }
        };
        rows.push(PendingConstraintCatalogRow {
            schema: schema.to_string(),
            table: table.to_string(),
            requested_name: Some(derived.name.clone()),
            object_id: Some(derived.catalog_identity.object_id),
            catalog_oid: Some(derived.catalog_identity.oid),
            kind: ConstraintCatalogKind::ForeignKey,
            columns: local_columns.clone(),
            state: ConstraintCatalogState::new(
                ConstraintValidationState::new(foreign_key.enforced, derived.validated),
                ConstraintDeferralState::new(
                    foreign_key.deferrable,
                    foreign_key.initially_deferred,
                ),
                ConstraintInheritanceState::new(false),
            ),
            period: foreign_key.period,
            foreign_key: Some(ForeignKeyCatalogData {
                referenced_index,
                schema: partition.schema.clone(),
                table: partition.name.clone(),
                column_ordinals: referenced_column_rows
                    .iter()
                    .map(|column| column.table_ordinal)
                    .collect(),
                positions_in_unique_constraint,
                on_update: foreign_key.on_update,
                on_delete: foreign_key.on_delete,
                match_type: foreign_key.match_type,
            }),
            parent_oid: Some(parent_oid),
            expression: None,
        });
    }
    Ok(rows)
}
