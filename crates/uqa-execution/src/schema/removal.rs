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
            let names = binding::bind_drop_targets(context, &stmt)?;
            if names.is_empty() {
                return Ok(SQLResult::empty());
            }
            if !matches!(stmt.kind, DropKind::View | DropKind::MaterializedView) {
                context.locks.prepare_definition_write()?;
            }
            run_drop_inner(context, DropStmt { names, ..stmt })
        }))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
fn run_drop_inner(
    context: &RelationRemovalContext<'_>,
    stmt: DropStmt,
) -> Result<SQLResult, SQLError> {
    match stmt.kind {
        DropKind::Table => {
            let tables = stmt.names;
            let (tables, dependents) = context.tables.hierarchy_drop_targets(&tables, stmt.cascade);
            if !dependents.is_empty() {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop table {} because other objects depend on it",
                        tables.join(", ")
                    ),
                });
            }
            if !stmt.cascade {
                context
                    .routines
                    .drop_relation_routine_dependents(&tables, false, "table")?;
                let restrict_dependents = context
                    .tables
                    .try_drop_table_restrict_dependents(&tables)
                    .map_err(|err| ddl_storage_error("DROP TABLE dependency preflight", err))?;
                if !restrict_dependents.is_empty() {
                    return Err(SQLError::Routine {
                        sqlstate: "2BP01".into(),
                        message: format!(
                            "cannot drop table {} because other objects depend on it: {}",
                            tables.join(", "),
                            restrict_dependents.join(", ")
                        ),
                    });
                }
            }
            for table in &tables {
                context
                    .events
                    .ensure_no_pending_trigger_events(table, "DROP TABLE")?;
            }
            context
                .tables
                .try_drop_tables(&tables, stmt.cascade)
                .map_err(|err| ddl_storage_error("DROP TABLE", err))?;
        }
        DropKind::ForeignTable => {
            let foreign_tables = stmt.names;
            context.routines.drop_relation_routine_dependents(
                &foreign_tables,
                stmt.cascade,
                "foreign table",
            )?;
            let target_names = foreign_tables.iter().cloned().collect();
            let owned_sequences = context
                .foreign_tables
                .foreign_table_owned_sequence_names(&foreign_tables)
                .map_err(|error| {
                    ddl_storage_error("DROP FOREIGN TABLE sequence ownership", error)
                })?;
            let dependents = uqa_sql::schema::removal::foreign_table_drop_dependents(
                context.dependencies,
                &foreign_tables,
                &owned_sequences,
                &target_names,
            )?;
            if !stmt.cascade && !dependents.is_empty() {
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop foreign table {} because other objects depend on it: {}",
                        foreign_tables.join(", "),
                        dependents.into_iter().collect::<Vec<_>>().join(", ")
                    ),
                });
            }
            if stmt.cascade {
                context
                    .events
                    .drop_rules_depending_on_relations_inner(&foreign_tables)
                    .map_err(|error| ddl_storage_error("DROP FOREIGN TABLE CASCADE", error))?;
                context
                    .views
                    .drop_views_depending_on_relations(&foreign_tables)
                    .map_err(|error| ddl_storage_error("DROP FOREIGN TABLE CASCADE", error))?;
            }
            for table in foreign_tables {
                let removed = context
                    .foreign_tables
                    .drop_foreign_table_inner(&table)
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "DROP FOREIGN TABLE failed in storage backend: {error}"
                        ))
                    })?;
                if !removed {
                    return Err(SQLError::Internal(format!(
                        "foreign table `{table}` disappeared after DROP preflight"
                    )));
                }
            }
            for sequence in owned_sequences {
                context
                    .sequences
                    .sequence_removal_context()
                    .drop_owned_sequence(&sequence, stmt.cascade)
                    .map_err(|error| {
                        ddl_storage_error("DROP FOREIGN TABLE owned sequence", error)
                    })?;
            }
        }
        DropKind::Index => unreachable!("DROP INDEX has a bound execution path"),
        DropKind::View | DropKind::MaterializedView => {
            context.views.drop_views(
                &stmt.names,
                stmt.cascade,
                uqa_sql::schema::removal::drop_relation_kind(stmt.kind),
            )?;
        }
        DropKind::Sequence => {
            context
                .sequences
                .sequence_removal_context()
                .drop_sequences(&stmt.names, stmt.cascade)?;
        }
        DropKind::Schema => unreachable!("DROP SCHEMA has a namespace dependency path"),
        DropKind::Domain => unreachable!("DROP DOMAIN has a type dependency path"),
    }
    Ok(SQLResult::empty())
}

fn ddl_storage_error(action: &str, err: impl std::error::Error + 'static) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &err)
}
