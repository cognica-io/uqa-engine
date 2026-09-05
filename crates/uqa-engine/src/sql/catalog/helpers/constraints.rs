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

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

use super::dependencies::{check_constraint_columns, named_constraint_columns};
use super::oids::split_schema_name;
use super::rows::catalog_ordinal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::sql::catalog) enum ConstraintCatalogKind {
    PrimaryKey,
    Unique { nulls_not_distinct: bool },
    ForeignKey,
    Check,
    NotNull,
}

impl ConstraintCatalogKind {
    pub(in crate::sql::catalog) const fn label(self) -> &'static str {
        match self {
            Self::PrimaryKey => "PRIMARY KEY",
            Self::Unique { .. } => "UNIQUE",
            Self::ForeignKey => "FOREIGN KEY",
            Self::Check => "CHECK",
            Self::NotNull => "NOT NULL",
        }
    }

    pub(in crate::sql::catalog) const fn pg_type(self) -> &'static str {
        match self {
            Self::PrimaryKey => "p",
            Self::Unique { .. } => "u",
            Self::ForeignKey => "f",
            Self::Check => "c",
            Self::NotNull => "n",
        }
    }

    pub(in crate::sql::catalog) const fn nulls_distinct(self) -> Option<bool> {
        match self {
            Self::Unique { nulls_not_distinct } => Some(!nulls_not_distinct),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::sql::catalog) struct ConstraintCatalogColumn {
    pub(in crate::sql::catalog) name: String,
    pub(in crate::sql::catalog) table_ordinal: i64,
}

#[derive(Debug, Clone)]
pub(in crate::sql::catalog) struct ForeignKeyCatalogData {
    pub(in crate::sql::catalog) referenced_key: Option<String>,
    pub(in crate::sql::catalog) schema: String,
    pub(in crate::sql::catalog) table: String,
    pub(in crate::sql::catalog) column_ordinals: Vec<i64>,
    pub(in crate::sql::catalog) positions_in_unique_constraint: Vec<Option<i64>>,
    pub(in crate::sql::catalog) on_update: ForeignKeyAction,
    pub(in crate::sql::catalog) on_delete: ForeignKeyAction,
    pub(in crate::sql::catalog) match_type: ForeignKeyMatch,
}

#[derive(Debug, Clone)]
pub(in crate::sql::catalog) struct ConstraintCatalogRow {
    pub(in crate::sql::catalog) schema: String,
    pub(in crate::sql::catalog) table: String,
    pub(in crate::sql::catalog) name: String,
    pub(in crate::sql::catalog) object_id: Option<[u8; 16]>,
    pub(in crate::sql::catalog) kind: ConstraintCatalogKind,
    pub(in crate::sql::catalog) columns: Vec<ConstraintCatalogColumn>,
    pub(in crate::sql::catalog) state: ConstraintCatalogState,
    pub(in crate::sql::catalog) period: bool,
    pub(in crate::sql::catalog) foreign_key: Option<ForeignKeyCatalogData>,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::sql::catalog) struct ConstraintCatalogState {
    validation: ConstraintValidationState,
    deferral: ConstraintDeferralState,
    inheritance: ConstraintInheritanceState,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ConstraintValidationState {
    enforced: bool,
    validated: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ConstraintDeferralState {
    NotDeferrable,
    InitiallyImmediate,
    InitiallyDeferred,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ConstraintInheritanceState {
    Inheritable,
    NoInherit,
}

impl ConstraintCatalogState {
    pub(super) const fn new(
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

    pub(in crate::sql::catalog) const fn enforced(self) -> bool {
        self.validation.enforced
    }

    pub(in crate::sql::catalog) const fn validated(self) -> bool {
        self.validation.validated
    }

    pub(in crate::sql::catalog) const fn deferrable(self) -> bool {
        !matches!(self.deferral, ConstraintDeferralState::NotDeferrable)
    }

    pub(in crate::sql::catalog) const fn initially_deferred(self) -> bool {
        matches!(self.deferral, ConstraintDeferralState::InitiallyDeferred)
    }

    pub(in crate::sql::catalog) const fn no_inherit(self) -> bool {
        matches!(self.inheritance, ConstraintInheritanceState::NoInherit)
    }
}

impl ConstraintValidationState {
    pub(super) const fn new(enforced: bool, validated: bool) -> Self {
        Self {
            enforced,
            validated,
        }
    }
}

impl ConstraintDeferralState {
    pub(super) const fn new(deferrable: bool, initially_deferred: bool) -> Self {
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
    pub(super) const fn new(no_inherit: bool) -> Self {
        if no_inherit {
            ConstraintInheritanceState::NoInherit
        } else {
            ConstraintInheritanceState::Inheritable
        }
    }
}

#[derive(Debug)]
pub(super) struct PendingConstraintCatalogRow {
    pub(super) schema: String,
    pub(super) table: String,
    pub(super) requested_name: Option<String>,
    pub(super) object_id: Option<[u8; 16]>,
    pub(super) kind: ConstraintCatalogKind,
    pub(super) columns: Vec<ConstraintCatalogColumn>,
    pub(super) state: ConstraintCatalogState,
    pub(super) period: bool,
    pub(super) foreign_key: Option<ForeignKeyCatalogData>,
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(in crate::sql::catalog) fn constraint_catalog_rows(
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
                    object_id: None,
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
                });
            }
            if let Some(expr) = &col.check {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: col.check_name.clone(),
                    object_id: None,
                    kind: ConstraintCatalogKind::Check,
                    columns: check_constraint_columns(expr, &columns, &table_name)?,
                    state: ConstraintCatalogState::new(
                        ConstraintValidationState::new(col.check_enforced, col.check_validated),
                        ConstraintDeferralState::new(false, false),
                        ConstraintInheritanceState::new(col.check_no_inherit),
                    ),
                    period: false,
                    foreign_key: None,
                });
            }
            if let Some(reference) = &col.references {
                let foreign_key = ForeignKey {
                    referenced_key: reference.referenced_key.clone(),
                    name: reference.name.clone(),
                    object_id: reference.object_id,
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table.clone(),
                    ref_columns: reference.column.iter().cloned().collect(),
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
                    enforced: reference.enforced,
                    validated: reference.validated,
                    deferrable: reference.deferrable,
                    initially_deferred: reference.initially_deferred,
                    period: reference.period,
                };
                pending.push(foreign_key_catalog_row(
                    catalog,
                    resolution,
                    &schema,
                    &table,
                    &table_name,
                    &columns,
                    &foreign_key,
                )?);
            }
        }

        let mut key_constraints = table_snapshot.keys.clone();
        for column in &columns {
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
                name: None,
                kind,
                columns: vec![column.name.clone()],
                nulls_not_distinct: false,
                without_overlaps: false,
            });
        }
        for constraint in key_constraints {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: constraint.name,
                object_id: None,
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
            });
        }

        for constraint in &table_snapshot.checks {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: constraint.name.clone(),
                object_id: None,
                kind: ConstraintCatalogKind::Check,
                columns: check_constraint_columns(&constraint.expr, &columns, &table_name)?,
                state: ConstraintCatalogState::new(
                    ConstraintValidationState::new(constraint.enforced, constraint.validated),
                    ConstraintDeferralState::new(false, false),
                    ConstraintInheritanceState::new(constraint.no_inherit),
                ),
                period: false,
                foreign_key: None,
            });
        }

        for foreign_key in &table_snapshot.foreign_keys {
            pending.push(foreign_key_catalog_row(
                catalog,
                resolution,
                &schema,
                &table,
                &table_name,
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
                kind: constraint.kind,
                columns: constraint.columns,
                state: constraint.state,
                period: constraint.period,
                foreign_key: constraint.foreign_key,
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
                    object_id: None,
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
                });
            }
            if let Some(expression) = &column.check {
                pending.push(PendingConstraintCatalogRow {
                    schema: schema.clone(),
                    table: table.clone(),
                    requested_name: column.check_name.clone(),
                    object_id: None,
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
                });
            }
        }
        for check in foreign_table.checks {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: check.name,
                object_id: None,
                kind: ConstraintCatalogKind::Check,
                columns: check_constraint_columns(&check.expr, &columns, &table_name)?,
                state: ConstraintCatalogState::new(
                    ConstraintValidationState::new(check.enforced, check.validated),
                    ConstraintDeferralState::new(false, false),
                    ConstraintInheritanceState::new(check.no_inherit),
                ),
                period: false,
                foreign_key: None,
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
                kind: constraint.kind,
                columns: constraint.columns,
                state: constraint.state,
                period: constraint.period,
                foreign_key: constraint.foreign_key,
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
    let mut referenced_keys = referenced.keys.clone();
    for column in referenced_columns {
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
        if referenced_keys.iter().any(|constraint| {
            constraint.kind == kind
                && constraint.columns.as_slice() == std::slice::from_ref(&column.name)
        }) {
            continue;
        }
        referenced_keys.push(uqa_sql::ast::TableKeyConstraint {
            name: None,
            kind,
            columns: vec![column.name.clone()],
            nulls_not_distinct: false,
            without_overlaps: false,
        });
    }
    let referenced_key = referenced_keys.iter().find(|constraint| {
        constraint.columns.len() == foreign_key.ref_columns.len()
            && foreign_key
                .ref_columns
                .iter()
                .all(|column| constraint.columns.contains(column))
    });
    let positions_in_unique_constraint = foreign_key
        .ref_columns
        .iter()
        .map(|column| {
            referenced_key
                .and_then(|constraint| constraint.columns.iter().position(|item| item == column))
                .map(|index| catalog_ordinal(index, "referenced key column"))
                .transpose()
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(PendingConstraintCatalogRow {
        schema: schema.to_string(),
        table: table.to_string(),
        requested_name: foreign_key.name.clone(),
        object_id: foreign_key.object_id,
        kind: ConstraintCatalogKind::ForeignKey,
        columns: local_columns,
        state: ConstraintCatalogState::new(
            ConstraintValidationState::new(foreign_key.enforced, foreign_key.validated),
            ConstraintDeferralState::new(foreign_key.deferrable, foreign_key.initially_deferred),
            ConstraintInheritanceState::new(true),
        ),
        period: foreign_key.period,
        foreign_key: Some(ForeignKeyCatalogData {
            referenced_key: foreign_key
                .referenced_key
                .clone()
                .or_else(|| referenced_key.and_then(|key| key.name.clone())),
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
    })
}
