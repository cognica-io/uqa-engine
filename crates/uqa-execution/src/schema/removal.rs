//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute DROP relation locks, dependency preflight and ordered publication.
use uqa_sql::{
    ast::{DropKind, DropStmt},
    SQLError, SQLResult,
};
mod binding;
mod context;
pub use context::*;
pub mod direct;
pub mod entry;

pub fn run_drop(
    context: &RelationRemovalContext<'_>,
    stmt: DropStmt,
) -> Result<SQLResult, SQLError> {
    if stmt.kind == DropKind::Index {
        return crate::schema::indexes::removal::run_drop_index(&context.indexes, stmt);
    }
    context
        .transactions
        .with_relation_write(Box::new(move |context| {
            let names = binding::bind_drop_targets(context, &stmt, &mut |message| {
                context.notices.push(uqa_sql::SQLNotice::notice(message));
            })?;
            if names.is_empty() {
                return Ok(SQLResult::empty());
            }
            run_drop_inner(context, DropStmt { names, ..stmt })
        }))
}

/// `RemoveRelations`: the relations bound in statement order, and what depends on them.
fn run_drop_inner(
    context: &RelationRemovalContext<'_>,
    stmt: DropStmt,
) -> Result<SQLResult, SQLError> {
    context.locks.prepare_definition_write()?;
    let relations = stmt
        .names
        .iter()
        .map(|name| uqa_core::RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal))
        .collect::<Result<Vec<_>, _>>()?;
    crate::schema::deletion::perform_deletion(
        &context.deletion.catalog_removal_context(),
        |dependencies| {
            relations
                .iter()
                .map(|relation| {
                    crate::schema::deletion::required_address(
                        dependencies.relation_address(relation, None),
                        || format!("relation {}", relation.qualified_name()),
                    )
                })
                .collect()
        },
        stmt.cascade,
    )?;
    Ok(SQLResult::empty())
}
