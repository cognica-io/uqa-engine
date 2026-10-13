//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CHECK and domain constraint views, including the distinct owner and type-usage visibility rules of `information_schema`.

use super::super::{
    helpers::{
        constraints::{constraint_catalog_rows, ConstraintCatalogKind, ConstraintCatalogRow},
        rows::{catalog_name, row, str_value},
    },
    view_definition::{stored_domain_expression_definition, stored_expression_definition},
    CatalogOutput, CatalogRequest,
};
use crate::catalog::{context::CatalogContext, CatalogReadView, RelationNameResolution};
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{catalog::security::object_acl, ResultRow, SQLError};

#[cfg(test)]
mod tests;

pub fn build_info_check_constraints(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &CatalogRequest,
) -> Result<Vec<ResultRow>, SQLError> {
    let output = CatalogOutput(CatalogContext {
        catalog,
        ..*context
    });
    let mut rows = Vec::new();
    for constraint in constraint_catalog_rows(catalog, resolution)? {
        if !matches!(
            constraint.kind,
            ConstraintCatalogKind::Check | ConstraintCatalogKind::NotNull
        ) || !selected(request, &constraint.schema, &constraint.name)
        {
            continue;
        }
        let relation = RelationIdentity::new(&constraint.schema, &constraint.table);
        let owner = catalog
            .snapshot()
            .tables
            .get(&relation)
            .map(|table| table.security.role_owner)
            .or_else(|| {
                catalog
                    .snapshot()
                    .definitions
                    .foreign_table_security
                    .get(&relation)
                    .map(|security| security.role_owner)
            })
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "constraint relation {} has no owner",
                    relation.qualified_name()
                ))
            })?;
        if !catalog.role_is_enabled_for(resolution.current_user(), &owner) {
            continue;
        }
        let clause = if request.includes("check_clause") {
            relation_check_clause(&output, catalog, resolution, constraint)?
        } else {
            Value::Null
        };
        rows.push(check_row(&constraint.schema, &constraint.name, clause));
    }
    for domain in catalog.domains() {
        if !catalog.role_is_enabled_for(resolution.current_user(), &domain.owner) {
            continue;
        }
        let schema = &domain.identity.schema;
        if let Some(constraint) = &domain.definition.not_null {
            let name = constraint_name(constraint.name.as_deref())?;
            if selected(request, schema, name) {
                rows.push(check_row(schema, name, str_value("VALUE IS NOT NULL")));
            }
        }
        for constraint in &domain.definition.checks {
            let name = constraint_name(constraint.name.as_deref())?;
            if !selected(request, schema, name) {
                continue;
            }
            let clause = if request.includes("check_clause") {
                str_value(stored_domain_expression_definition(
                    Some(&output),
                    catalog,
                    resolution,
                    &constraint.expression,
                    false,
                )?)
            } else {
                Value::Null
            };
            rows.push(check_row(schema, name, clause));
        }
    }
    Ok(rows)
}

pub fn build_info_domain_constraints(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &CatalogRequest,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for domain in catalog.domains() {
        let identity = &domain.identity;
        if !request.matches_name("domain_schema", &identity.schema)
            || !request.matches_name("domain_name", &identity.name)
        {
            continue;
        }
        let has_role = |role: &uqa_core::catalog_role::RoleIdentity| {
            catalog.role_is_enabled_for(resolution.current_user(), role)
        };
        if !has_role(&domain.owner)
            && !object_acl::privilege_allowed(
                &domain.owner,
                domain.usage_acl.as_deref(),
                false,
                false,
                has_role,
            )
        {
            continue;
        }
        let names = domain
            .definition
            .not_null
            .iter()
            .map(|constraint| constraint.name.as_deref())
            .chain(
                domain
                    .definition
                    .checks
                    .iter()
                    .map(|constraint| constraint.name.as_deref()),
            );
        for name in names {
            let name = constraint_name(name)?;
            if !selected(request, &identity.schema, name) {
                continue;
            }
            rows.push(row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(&identity.schema)),
                ("constraint_name", str_value(name)),
                ("domain_catalog", catalog_name()),
                ("domain_schema", str_value(&identity.schema)),
                ("domain_name", str_value(&identity.name)),
                ("is_deferrable", str_value("NO")),
                ("initially_deferred", str_value("NO")),
            ]));
        }
    }
    Ok(rows)
}

fn selected(request: &CatalogRequest, schema: &str, name: &str) -> bool {
    request.matches_name("constraint_schema", schema)
        && request.matches_name("constraint_name", name)
}

fn relation_check_clause(
    output: &CatalogOutput<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    constraint: &ConstraintCatalogRow,
) -> Result<Value, SQLError> {
    if constraint.kind == ConstraintCatalogKind::NotNull {
        let column = constraint
            .columns
            .first()
            .ok_or_else(|| SQLError::Internal("NOT NULL constraint has no column".into()))?;
        // PostgreSQL's view uses format('%s IS NOT NULL', attname), preserving the decoded name without identifier quoting.
        return Ok(str_value(format!("{} IS NOT NULL", column.name)));
    }
    let expression = constraint
        .expression
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CHECK constraint has no expression".into()))?;
    stored_expression_definition(Some(output), catalog, resolution, expression, false)
        .map(str_value)
}

fn constraint_name(name: Option<&str>) -> Result<&str, SQLError> {
    name.ok_or_else(|| SQLError::Internal("domain constraint has no catalog name".into()))
}

fn check_row(schema: &str, name: &str, clause: Value) -> ResultRow {
    row([
        ("constraint_catalog", catalog_name()),
        ("constraint_schema", str_value(schema)),
        ("constraint_name", str_value(name)),
        ("check_clause", clause),
    ])
}
