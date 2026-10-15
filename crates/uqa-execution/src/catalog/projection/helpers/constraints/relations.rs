//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build selected table constraints without enumerating unrelated definitions.

use super::{
    check_constraint_columns, derived_constraint_catalog_rows, foreign_key_catalog_row,
    named_constraint_columns, split_schema_name, CatalogReadView, ConstraintCatalogColumn,
    ConstraintCatalogKind, ConstraintCatalogRow, ConstraintCatalogState, ConstraintDeferralState,
    ConstraintInheritanceState, ConstraintValidationState, PendingConstraintCatalogRow,
    RelationNameResolution, SQLError, TableKeyConstraintKind,
};

pub(crate) fn table_constraint_catalog_rows(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table_name: &str,
    table_snapshot: &crate::catalog::CatalogTableSnapshot,
) -> Result<Vec<ConstraintCatalogRow>, SQLError> {
    let mut out = Vec::new();
    let (schema, table) = split_schema_name(table_name)?;
    let columns = table_snapshot.columns.clone();
    let mut pending = Vec::new();

    pending.extend(column_constraints(
        catalog, resolution, table_name, &columns,
    )?);
    pending.extend(key_constraints(table_name, table_snapshot)?);

    for constraint in table_snapshot.checks.iter() {
        pending.push(PendingConstraintCatalogRow {
            schema: schema.clone(),
            table: table.clone(),
            requested_name: constraint.name.clone(),
            object_id: constraint.object_id,
            catalog_oid: constraint.catalog_oid,
            kind: ConstraintCatalogKind::Check,
            columns: check_constraint_columns(&constraint.expr, &columns, table_name)?,
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
            table_name,
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
    Ok(out)
}

fn column_constraints(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table_name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
) -> Result<Vec<PendingConstraintCatalogRow>, SQLError> {
    let (schema, table) = split_schema_name(table_name)?;
    let mut pending = Vec::new();
    for (idx, col) in columns.iter().enumerate() {
        let ordinal = super::super::rows::column_ordinal(idx, col)?;
        if col.not_null {
            pending.push(PendingConstraintCatalogRow {
                schema: schema.clone(),
                table: table.clone(),
                requested_name: col.not_null_name.clone(),
                object_id: col.not_null_identity.map(|identity| identity.object_id),
                catalog_oid: Some(
                    col.not_null_identity
                        .ok_or_else(|| {
                            SQLError::Internal("NOT NULL constraint has no catalog identity".into())
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
                columns: check_constraint_columns(expr, columns, table_name)?,
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
                table_name,
                columns,
                &foreign_key,
            )?);
            pending.extend(derived_constraint_catalog_rows(
                catalog,
                &schema,
                &table,
                columns,
                &foreign_key,
            )?);
        }
    }

    Ok(pending)
}

fn key_constraints(
    table_name: &str,
    table_snapshot: &crate::catalog::CatalogTableSnapshot,
) -> Result<Vec<PendingConstraintCatalogRow>, SQLError> {
    let (schema, table) = split_schema_name(table_name)?;
    let columns = &table_snapshot.columns;
    let mut pending = Vec::new();
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
            columns: named_constraint_columns(&constraint.columns, columns, table_name)?,
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

    Ok(pending)
}
