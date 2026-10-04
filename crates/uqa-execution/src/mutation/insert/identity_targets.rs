//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The identity columns an `INSERT` statement supplies values for, checked before it reads or writes a row.

use std::collections::BTreeSet;

use uqa_sql::{
    ast::OverridingKind,
    plan::{ConflictActionPlan, ConflictPlan, InsertPlan},
    semantics::generated_values::GeneratedValueColumns,
    SQLError, ScalarExpr,
};

use crate::mutation::{errors::dml_storage_error, identity::InsertIdentityContext};

/// Reject a value the statement supplies for a `GENERATED ALWAYS` identity column without an `OVERRIDING` clause, and an `ON CONFLICT DO UPDATE` assignment other than `DEFAULT` to such a column. Every `VALUES` row and every column a query source supplies counts, whether or not a row reaches the target.
pub(super) fn validate_insert_identity_targets(
    identities: InsertIdentityContext<'_>,
    stmt: &InsertPlan,
) -> Result<(), SQLError> {
    let targets = if stmt.columns.is_empty() {
        identities
            .columns
            .try_describe_table(&stmt.table)
            .map_err(|error| dml_storage_error("INSERT", error))?
            .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?
            .into_iter()
            .map(|column| column.name)
            .collect::<Vec<_>>()
    } else {
        stmt.columns
            .iter()
            .map(|target| target.column.clone())
            .collect()
    };
    let supplied = match &stmt.source {
        Some(source) => {
            let width = if stmt.columns.is_empty() {
                uqa_sql::semantics::query_plan_output_columns(source)
                    .map_or(targets.len(), |columns| columns.len())
            } else {
                targets.len()
            };
            (0..targets.len())
                .map(|position| position < width)
                .collect::<Vec<_>>()
        }
        None => (0..targets.len())
            .map(|position| {
                stmt.rows.iter().any(|row| {
                    row.get(position)
                        .is_some_and(|expression| !matches!(expression, ScalarExpr::Default))
                })
            })
            .collect(),
    };
    let identity = GeneratedValueColumns::of(identities.columns, &stmt.table)?;
    identity.validate_insert(
        targets.iter().map(String::as_str).zip(supplied),
        stmt.overriding,
    )?;
    if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = &stmt.on_conflict
    {
        identity.validate_update(assignments.iter().map(|assignment| {
            (
                assignment.target.column.as_str(),
                matches!(assignment.value, ScalarExpr::Default),
            )
        }))?;
    }
    Ok(())
}

/// The identity columns of the statement's target table whose supplied values `OVERRIDING USER VALUE` discards without evaluating them, so that each draws its sequence value instead; empty without that clause and for a view whose rules receive the row.
pub fn user_value_identity_columns(
    identities: InsertIdentityContext<'_>,
    stmt: &InsertPlan,
    columns: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<BTreeSet<String>, SQLError> {
    if stmt.overriding != Some(OverridingKind::UserValue) || !stmt.view_rule_relations.is_empty() {
        return Ok(BTreeSet::new());
    }
    let identity = GeneratedValueColumns::of(identities.columns, &stmt.table)?;
    Ok(columns
        .into_iter()
        .filter(|column| identity.contains(column.as_ref()))
        .map(|column| column.as_ref().to_owned())
        .collect())
}
