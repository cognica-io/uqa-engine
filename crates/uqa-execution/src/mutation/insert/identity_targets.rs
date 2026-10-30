//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The identity columns an `INSERT` statement supplies values for, checked before it reads or writes a row.

use std::collections::BTreeSet;
use uqa_core::memory::{OwnedSet, Produced, ProductionControl, ProductionVec};

use uqa_sql::{
    ast::OverridingKind,
    plan::{ConflictActionPlan, ConflictPlan, InsertPlan},
    semantics::generated_values::GeneratedValueColumns,
    SQLError, ScalarExpr,
};

use crate::mutation::identity::InsertIdentityContext;

/// Reject a value the statement supplies for a `GENERATED ALWAYS` identity column without an `OVERRIDING` clause, and an `ON CONFLICT DO UPDATE` assignment other than `DEFAULT` to such a column. Every `VALUES` row and every column a query source supplies counts, whether or not a row reaches the target.
pub(super) fn validate_insert_identity_targets(
    identities: InsertIdentityContext<'_>,
    stmt: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let implicit = if stmt.columns.is_empty() {
        Some(
            uqa_sql::assignment::columns::declared_column_names_with_control(
                identities.columns,
                &stmt.table,
                control,
            )
            .map_err(|error| storage_context(error, "INSERT", control))?
            .ok_or_else(|| match control.copy_text(&stmt.table) {
                Ok(name) => SQLError::UnknownTable(name.into_parts().0),
                Err(error) => error.into(),
            })?,
        )
    } else {
        None
    };
    let count = implicit
        .as_ref()
        .map_or(stmt.columns.len(), |names| names.len());
    let width = if let Some(source) = &stmt.source {
        if implicit.is_some() {
            uqa_sql::semantics::query_plan_output_width_with_control(source, control)?
                .unwrap_or(count)
        } else {
            count
        }
    } else {
        count
    };
    let targets = implicit
        .iter()
        .flat_map(|names| names.iter().map(String::as_str))
        .chain(stmt.columns.iter().map(|target| target.column.as_str()));
    let mut supplied = ProductionVec::new(*control);
    for (position, name) in targets.enumerate() {
        let written = if stmt.source.is_some() {
            position < width
                && uqa_sql::semantics::generated_values::insert_has_written_value(stmt, position)
        } else {
            stmt.rows.iter().any(|row| {
                row.get(position)
                    .is_some_and(|expression| !matches!(expression, ScalarExpr::Default))
            })
        };
        supplied.push_copy((name, written))?;
    }
    let supplied = supplied.finish()?;
    let identity =
        GeneratedValueColumns::of_with_control(identities.columns, &stmt.table, control)?;
    identity.validate_insert_with_control(supplied.iter().copied(), stmt.overriding, control)?;
    if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = &stmt.on_conflict
    {
        identity.validate_update_with_control(
            assignments.iter().flat_map(|assignment| {
                assignment.target.targets().iter().map(move |target| {
                    (
                        target.column.as_str(),
                        matches!(assignment.value, ScalarExpr::Default),
                    )
                })
            }),
            control,
        )?;
    }
    Ok(())
}

/// The identity columns of the statement's target table whose supplied values `OVERRIDING USER VALUE` discards without evaluating them, so that each draws its sequence value instead; empty without that clause.
pub fn user_value_identity_columns(
    identities: InsertIdentityContext<'_>,
    stmt: &InsertPlan,
    columns: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<BTreeSet<String>, SQLError> {
    Ok(user_value_identity_columns_with_control(
        identities,
        stmt,
        columns,
        &ProductionControl::uncontrolled(),
    )?
    .into_iter()
    .map(|name| name.into_uncontrolled().expect("ordinary identity name"))
    .collect())
}

pub fn user_value_identity_columns_with_control(
    identities: InsertIdentityContext<'_>,
    stmt: &InsertPlan,
    columns: impl IntoIterator<Item = impl AsRef<str>>,
    control: &ProductionControl<'_>,
) -> Result<OwnedSet<Produced<String>>, SQLError> {
    if stmt.overriding != Some(OverridingKind::UserValue) {
        return Ok(OwnedSet::new());
    }
    let identity =
        GeneratedValueColumns::of_with_control(identities.columns, &stmt.table, control)?;
    let mut selected = OwnedSet::new();
    for column in columns {
        control.check()?;
        let name = column.as_ref();
        if identity.contains_with_control(name, control)? && !selected.contains(name) {
            let name = control.copy_text(name)?;
            if let Some(budget) = control.budget() {
                selected.insert_budgeted(name, budget)?;
            } else {
                selected.insert(name);
            }
        }
    }
    Ok(selected)
}

fn storage_context(error: SQLError, action: &str, control: &ProductionControl<'_>) -> SQLError {
    match error {
        SQLError::Internal(message) => match control.format(format_args!(
            "{action} failed in storage backend: {message}"
        )) {
            Ok(message) => SQLError::Internal(message.into_parts().0),
            Err(error) => error.into(),
        },
        error => error,
    }
}
