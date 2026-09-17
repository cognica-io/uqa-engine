//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind each explicit DROP target in statement order and recheck it after relation-lock waits.

use super::RelationRemovalContext;
use crate::row_locks::{
    binding::{bind_relation, lock_descendants, RelationBinding},
    RelationLockMode,
};
use std::collections::BTreeSet;
use uqa_sql::{
    ast::{DropKind, DropStmt},
    schema::removal::bind_relation_drop_target,
    SQLError,
};

pub(super) fn bind_drop_targets(
    context: &RelationRemovalContext<'_>,
    statement: &DropStmt,
) -> Result<Vec<String>, SQLError> {
    let mut notice = |message: &str| {
        context
            .notices
            .lock()
            .push(("NOTICE".into(), message.into()));
    };
    if statement.kind == DropKind::Sequence {
        return uqa_sql::schema::removal::bind_relation_drop_targets(
            context.catalog,
            statement,
            &mut notice,
        );
    }
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    for name in &statement.names {
        let bound = bind_relation(
            context.locks,
            RelationLockMode::AccessExclusive,
            false,
            || {
                let Some(canonical) = bind_relation_drop_target(
                    context.catalog,
                    name,
                    statement.kind,
                    statement.if_exists,
                    &mut notice,
                )?
                else {
                    return Ok(None);
                };
                let object_id = context.identities.relation_object_id(&canonical)?;
                Ok(Some(RelationBinding {
                    name: canonical,
                    object_id,
                    value: (),
                }))
            },
            |binding| match statement.kind {
                DropKind::Table => context
                    .privileges
                    .ensure_table_drop_authority(&binding.name),
                DropKind::ForeignTable => context
                    .privileges
                    .ensure_foreign_table_drop_authority(&binding.name),
                DropKind::View | DropKind::MaterializedView => {
                    context.views.ensure_view_drop_authority(&binding.name)
                }
                _ => unreachable!("relation DROP binding requires a table-shaped target"),
            },
        )?;
        if let Some(bound) = bound {
            if seen.insert(bound.name.clone()) {
                targets.push(bound.name);
            }
        }
    }
    if statement.kind == DropKind::Table {
        let (descendants, _) = context
            .tables
            .hierarchy_drop_targets(&targets, statement.cascade);
        lock_descendants(
            context.identities,
            context.locks,
            descendants.into_iter().filter(|name| !seen.contains(name)),
            RelationLockMode::AccessExclusive,
            false,
        )?;
        let (tables, _) = context
            .tables
            .hierarchy_drop_targets(&targets, statement.cascade);
        context.views.lock_dependent_views(&tables)?;
    } else if statement.kind == DropKind::ForeignTable {
        context.views.lock_dependent_views(&targets)?;
    }
    Ok(targets)
}
