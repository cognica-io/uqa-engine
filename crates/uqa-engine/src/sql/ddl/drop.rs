//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP preflight, object removal, and index side effects.

use super::{DropKind, DropStmt, Engine, SQLError, SQLResult};
use crate::capabilities::RelationResolution;

pub(in crate::sql) fn run_drop(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    if stmt.kind == DropKind::Table {
        for name in &stmt.names {
            if let Some(canonical) = crate::sql::resolve_age_label_relation_name(engine, name)? {
                let relation =
                    crate::RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                        SQLError::Internal(format!(
                            "resolve AGE label relation `{canonical}` for DROP TABLE: {error}"
                        ))
                    })?;
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "table \"{}\" is for label \"{}\"",
                        relation.name, relation.name
                    ),
                });
            }
        }
    }
    if stmt.kind == DropKind::Index {
        return uqa_execution::schema::indexes::removal::run_drop_index(
            &engine.index_removal_context(),
            stmt,
        );
    }
    if stmt.kind == DropKind::Schema {
        return engine.with_implicit_transaction(|engine| {
            engine.drop_schemas_sql(&stmt)?;
            Ok(SQLResult::empty())
        });
    }
    if stmt.kind == DropKind::Domain {
        return engine.with_implicit_transaction(|engine| {
            engine.drop_domains_sql(&stmt)?;
            Ok(SQLResult::empty())
        });
    }
    let mut lock_targets = std::collections::BTreeSet::new();
    match stmt.kind {
        DropKind::Table | DropKind::ForeignTable | DropKind::View | DropKind::MaterializedView => {
            let mut table_targets = Vec::new();
            for name in &stmt.names {
                if let Some((canonical, kind)) = engine.try_resolve_visible_relation_kind(name)? {
                    if stmt.kind == DropKind::Table && kind == "table" {
                        table_targets.push(canonical.clone());
                    }
                    lock_targets.insert(canonical);
                }
            }
            if stmt.kind == DropKind::Table {
                let (hierarchy_targets, _) =
                    engine.hierarchy_drop_targets(&table_targets, stmt.cascade);
                lock_targets.extend(hierarchy_targets);
            }
        }
        DropKind::Index => unreachable!("DROP INDEX has a bound execution path"),
        DropKind::Schema => unreachable!("DROP SCHEMA has a namespace dependency path"),
        DropKind::Domain => unreachable!("DROP DOMAIN has a type dependency path"),
        DropKind::Sequence => {}
    }
    for table in lock_targets {
        engine.lock_relation(&table, crate::row_locks::RelationLockMode::AccessExclusive)?;
    }
    engine.with_implicit_transaction(move |engine| run_drop_inner(engine, stmt))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
fn run_drop_inner(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    match stmt.kind {
        DropKind::Table => {
            let mut tables = Vec::new();
            for name in &stmt.names {
                let (_, local) =
                    crate::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
                match engine.resolve_visible_relation_kind(name)? {
                    RelationResolution::Found(canonical, "table") => tables.push(canonical),
                    RelationResolution::Found(_, _) => {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!("\"{local}\" is not a table"),
                        });
                    }
                    RelationResolution::MissingSchema(schema) if stmt.if_exists => {
                        engine.push_sql_notice(
                            "NOTICE",
                            &format!("schema \"{schema}\" does not exist, skipping"),
                        );
                    }
                    RelationResolution::MissingRelation if stmt.if_exists => {
                        engine.push_sql_notice(
                            "NOTICE",
                            &format!("table \"{local}\" does not exist, skipping"),
                        );
                    }
                    RelationResolution::MissingSchema(schema) => {
                        return Err(SQLError::Routine {
                            sqlstate: "3F000".into(),
                            message: format!("schema \"{schema}\" does not exist"),
                        });
                    }
                    RelationResolution::MissingRelation => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("table \"{local}\" does not exist"),
                        });
                    }
                }
            }
            for table in &tables {
                engine.ensure_table_drop_authority(table)?;
            }
            let (tables, dependents) = engine.hierarchy_drop_targets(&tables, stmt.cascade);
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
                engine.drop_relation_routine_dependents(&tables, false, "table")?;
                let restrict_dependents = engine
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
                engine.ensure_no_pending_trigger_events(table, "DROP TABLE")?;
            }
            engine
                .try_drop_tables(&tables, stmt.cascade)
                .map_err(|err| ddl_storage_error("DROP TABLE", err))?;
        }
        DropKind::ForeignTable => {
            let mut foreign_tables = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for name in &stmt.names {
                match engine.resolve_visible_relation_kind(name)? {
                    RelationResolution::Found(canonical, "foreign table") => {
                        if seen.insert(canonical.clone()) {
                            foreign_tables.push(canonical);
                        }
                    }
                    RelationResolution::Found(_, _) => {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!("\"{name}\" is not a foreign table"),
                        });
                    }
                    RelationResolution::MissingSchema(schema) if stmt.if_exists => {
                        engine.push_sql_notice(
                            "NOTICE",
                            &format!("schema \"{schema}\" does not exist, skipping"),
                        );
                    }
                    RelationResolution::MissingRelation if stmt.if_exists => {
                        engine.push_sql_notice(
                            "NOTICE",
                            &format!("foreign table \"{name}\" does not exist, skipping"),
                        );
                    }
                    RelationResolution::MissingSchema(schema) => {
                        return Err(SQLError::Routine {
                            sqlstate: "3F000".into(),
                            message: format!("schema \"{schema}\" does not exist"),
                        });
                    }
                    RelationResolution::MissingRelation => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("foreign table \"{name}\" does not exist"),
                        });
                    }
                }
            }
            for table in &foreign_tables {
                engine.ensure_foreign_table_drop_authority(table)?;
            }
            engine.drop_relation_routine_dependents(
                &foreign_tables,
                stmt.cascade,
                "foreign table",
            )?;
            let target_names = foreign_tables.iter().cloned().collect();
            let owned_sequences = engine
                .foreign_table_owned_sequence_names(&foreign_tables)
                .map_err(|error| {
                    ddl_storage_error("DROP FOREIGN TABLE sequence ownership", error)
                })?;
            let mut dependents = std::collections::BTreeSet::new();
            for table in &foreign_tables {
                dependents.extend(
                    engine
                        .views_depending_on_relation(table)
                        .map_err(|error| {
                            ddl_storage_error("DROP FOREIGN TABLE dependency preflight", error)
                        })?
                        .into_iter()
                        .map(|view| format!("view {view}")),
                );
            }
            dependents.extend(
                engine
                    .rules_depending_on_relations(&foreign_tables)
                    .map_err(|error| {
                        ddl_storage_error("DROP FOREIGN TABLE dependency preflight", error)
                    })?
                    .into_iter()
                    .map(|(table, rule)| {
                        format!("rule {rule} on table {}", table.qualified_name())
                    }),
            );
            for sequence in &owned_sequences {
                dependents.extend(
                    engine
                        .sequence_external_dependents_for_owner_drop(sequence, &target_names)
                        .map_err(|error| {
                            ddl_storage_error(
                                "DROP FOREIGN TABLE owned-sequence dependency preflight",
                                error,
                            )
                        })?,
                );
            }
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
                engine
                    .drop_rules_depending_on_relations_inner(&foreign_tables)
                    .map_err(|error| ddl_storage_error("DROP FOREIGN TABLE CASCADE", error))?;
                engine
                    .drop_views_depending_on_relations(&foreign_tables)
                    .map_err(|error| ddl_storage_error("DROP FOREIGN TABLE CASCADE", error))?;
            }
            for table in foreign_tables {
                let removed = engine.drop_foreign_table_inner(&table).map_err(|error| {
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
                engine
                    .drop_owned_sequence(&sequence, stmt.cascade)
                    .map_err(|error| {
                        ddl_storage_error("DROP FOREIGN TABLE owned sequence", error)
                    })?;
            }
        }
        DropKind::Index => unreachable!("DROP INDEX has a bound execution path"),
        DropKind::View | DropKind::MaterializedView => {
            let expected_kind = if stmt.kind == DropKind::View {
                "view"
            } else {
                "materialized view"
            };
            let command = if stmt.kind == DropKind::View {
                "DROP VIEW"
            } else {
                "DROP MATERIALIZED VIEW"
            };
            let mut views = Vec::new();
            for name in &stmt.names {
                match engine.try_resolve_visible_relation_kind(name)? {
                    Some((canonical, kind)) if kind == expected_kind => views.push(canonical),
                    Some((canonical, kind)) => {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!(
                                "{command}: relation `{canonical}` is a {kind}, not a {expected_kind}"
                            ),
                        });
                    }
                    None if stmt.if_exists => {}
                    None => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("{command}: relation `{name}` does not exist"),
                        });
                    }
                }
            }
            engine.drop_views(&views, stmt.cascade, expected_kind)?;
        }
        DropKind::Sequence => {
            let mut sequences = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for name in &stmt.names {
                match engine.resolve_visible_relation_kind(name)? {
                    RelationResolution::Found(canonical, "sequence") => {
                        if seen.insert(canonical.clone()) {
                            sequences.push(canonical);
                        }
                    }
                    RelationResolution::Found(_canonical, _kind) => {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!("\"{name}\" is not a sequence"),
                        });
                    }
                    RelationResolution::MissingRelation | RelationResolution::MissingSchema(_)
                        if stmt.if_exists =>
                    {
                        engine.push_sql_notice(
                            "NOTICE",
                            &format!("sequence \"{name}\" does not exist, skipping"),
                        );
                    }
                    RelationResolution::MissingSchema(schema) => {
                        return Err(SQLError::Routine {
                            sqlstate: "3F000".into(),
                            message: format!("schema \"{schema}\" does not exist"),
                        });
                    }
                    RelationResolution::MissingRelation => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!("sequence \"{name}\" does not exist"),
                        });
                    }
                }
            }
            engine.drop_sequences_sql_inner(&sequences, stmt.cascade)?;
        }
        DropKind::Schema => unreachable!("DROP SCHEMA has a namespace dependency path"),
        DropKind::Domain => unreachable!("DROP DOMAIN has a type dependency path"),
    }
    Ok(SQLResult::empty())
}

pub(super) fn ddl_storage_error(action: &str, err: impl std::error::Error + 'static) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &err)
}
