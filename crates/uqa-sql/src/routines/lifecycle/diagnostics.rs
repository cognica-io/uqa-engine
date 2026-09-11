//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable dependency diagnostics and cascade notices for routine removal.

use super::{RoutineDropTarget, RoutineObjectDependents};
use crate::SQLError;

pub fn append_routine_cascade_notice(
    notices: &mut Vec<(&'static str, String)>,
    cascaded_routines: &[RoutineDropTarget],
    dependents: &RoutineObjectDependents,
) {
    let mut cascaded = cascaded_routines
        .iter()
        .map(|target| format!("{} {}", target.kind(), target.label()))
        .collect::<Vec<_>>();
    cascaded.extend(dependents.columns.iter().map(|(table, column, foreign)| {
        format!(
            "column {column} of {} {table}",
            routine_schema_relation_kind(*foreign)
        )
    }));
    cascaded.extend(dependents.defaults.iter().map(|(table, column, foreign)| {
        format!(
            "default value for column {column} of {} {table}",
            routine_schema_relation_kind(*foreign)
        )
    }));
    cascaded.extend(
        dependents
            .checks
            .iter()
            .map(|(table, constraint, foreign)| {
                format!(
                    "constraint {constraint} on {} {table}",
                    routine_schema_relation_kind(*foreign)
                )
            }),
    );
    cascaded.extend(dependents.views.iter().map(|view| format!("view {view}")));
    cascaded.extend(
        dependents
            .triggers
            .iter()
            .map(|(table, trigger)| format!("trigger {trigger} on table {table}")),
    );
    cascaded.extend(
        dependents
            .rules
            .iter()
            .map(|(table, rule)| format!("rule {rule} on table {table}")),
    );
    cascaded.sort();
    cascaded.dedup();
    match cascaded.as_slice() {
        [] => {}
        [object] => notices.push(("NOTICE", format!("drop cascades to {object}"))),
        objects => notices.push((
            "NOTICE",
            format!("drop cascades to {} other objects", objects.len()),
        )),
    }
}

pub fn routine_schema_relation_kind(foreign: bool) -> &'static str {
    if foreign {
        "foreign table"
    } else {
        "table"
    }
}

pub fn ensure_no_function_dependencies(
    target: &RoutineDropTarget,
    dependents: &RoutineObjectDependents,
) -> Result<(), SQLError> {
    if dependents.columns.is_empty()
        && dependents.defaults.is_empty()
        && dependents.checks.is_empty()
        && dependents.views.is_empty()
        && dependents.triggers.is_empty()
        && dependents.rules.is_empty()
        && dependents.indexes.is_empty()
    {
        return Ok(());
    }
    let mut dependency_kinds = Vec::new();
    if !dependents.columns.is_empty() {
        dependency_kinds.push(format!(
            "generated column(s) `{}`",
            dependents
                .columns
                .iter()
                .map(|(table, column, _)| format!("{table}.{column}"))
                .collect::<Vec<_>>()
                .join("`, `")
        ));
    }
    if !dependents.defaults.is_empty() {
        dependency_kinds.push(format!(
            "default value(s) `{}`",
            dependents
                .defaults
                .iter()
                .map(|(table, column, _)| format!("{table}.{column}"))
                .collect::<Vec<_>>()
                .join("`, `")
        ));
    }
    if !dependents.checks.is_empty() {
        dependency_kinds.push(format!(
            "CHECK constraint(s) `{}`",
            dependents
                .checks
                .iter()
                .map(|(table, constraint, _)| format!("{constraint} on {table}"))
                .collect::<Vec<_>>()
                .join("`, `")
        ));
    }
    if !dependents.views.is_empty() {
        dependency_kinds.push(format!("view(s) `{}`", dependents.views.join("`, `")));
    }
    if !dependents.triggers.is_empty() {
        dependency_kinds.push(format!(
            "trigger(s) `{}`",
            dependents
                .triggers
                .iter()
                .map(|(table, trigger)| format!("{trigger} on {table}"))
                .collect::<Vec<_>>()
                .join("`, `")
        ));
    }
    if !dependents.indexes.is_empty() {
        dependency_kinds.push(format!(
            "indexes {}",
            dependents
                .indexes
                .iter()
                .map(uqa_core::RelationIdentity::qualified_name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !dependents.rules.is_empty() {
        dependency_kinds.push(format!(
            "rule(s) `{}`",
            dependents
                .rules
                .iter()
                .map(|(table, rule)| format!("{rule} on {table}"))
                .collect::<Vec<_>>()
                .join("`, `")
        ));
    }
    Err(SQLError::Routine {
        sqlstate: "2BP01".into(),
        message: format!(
            "cannot drop function {} because {} depend on it",
            target.label(),
            dependency_kinds.join(" and ")
        ),
    })
}
