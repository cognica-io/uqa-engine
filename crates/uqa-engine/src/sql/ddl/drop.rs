//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP preflight, object removal, and index side effects.

use super::{CatalogIndexRow, ColumnType, DropKind, DropStmt, Engine, SQLError, SQLResult};

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
    if stmt.cascade
        && matches!(
            stmt.kind,
            DropKind::View | DropKind::MaterializedView | DropKind::Schema
        )
        && !(stmt.kind == DropKind::Schema
            && only_graph_namespaces(engine, &stmt.names, stmt.if_exists)?)
    {
        return Err(SQLError::Unsupported(format!(
            "DROP {} CASCADE is not supported; no objects were changed",
            match stmt.kind {
                DropKind::View => "VIEW",
                DropKind::MaterializedView => "MATERIALIZED VIEW",
                DropKind::Schema => "SCHEMA",
                DropKind::Table | DropKind::Index | DropKind::Sequence => unreachable!(),
            }
        )));
    }
    let mut lock_targets = std::collections::BTreeSet::new();
    match stmt.kind {
        DropKind::Table | DropKind::View | DropKind::MaterializedView => {
            let mut table_targets = Vec::new();
            for name in &stmt.names {
                if let Some((canonical, kind)) = engine
                    .try_resolve_relation_kind(name)
                    .map_err(|err| ddl_storage_error("DROP relation lock", err))?
                {
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
        DropKind::Index => {
            for name in &stmt.names {
                if let Some(index) = engine
                    .catalog_index(name)
                    .map_err(|err| ddl_storage_error("DROP INDEX relation lock", err))?
                {
                    lock_targets.insert(index.table_name);
                }
            }
        }
        DropKind::Schema => {
            // Dropping a schema removes every relation it owns, including the label relations of a graph namespace, so each of them takes the same AccessExclusive lock a direct DROP would.
            for name in &stmt.names {
                for table in engine
                    .tables_in_schema(name)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA relation lock", err))?
                {
                    lock_targets.insert(format!("{name}.{table}"));
                }
            }
        }
        DropKind::Sequence => {}
    }
    for table in lock_targets {
        engine.lock_relation(&table, crate::row_locks::RelationLockMode::AccessExclusive)?;
    }
    engine.with_implicit_transaction(move |engine| run_drop_inner(engine, stmt))
}

/// `DROP SCHEMA ... CASCADE` is implemented for graph namespaces, whose only
/// dependents are the graph's own label relations, so cascading drops the
/// graph exactly like AGE.
fn only_graph_namespaces(
    engine: &Engine,
    names: &[String],
    if_exists: bool,
) -> Result<bool, SQLError> {
    for name in names {
        let is_graph = engine
            .has_graph(name)
            .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?;
        let is_schema = engine
            .has_schema(name)
            .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?;
        // `IF EXISTS` skips a name that is neither a graph nor a schema, so
        // it must not force the unsupported-CASCADE rejection.
        if if_exists && !is_graph && !is_schema {
            continue;
        }
        if !is_graph || is_schema {
            return Ok(false);
        }
    }
    Ok(!names.is_empty())
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
                match engine
                    .try_resolve_relation_kind(name)
                    .map_err(|err| ddl_storage_error("DROP TABLE", err))?
                {
                    Some((canonical, "table")) => tables.push(canonical),
                    Some((canonical, kind)) => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP TABLE: relation `{canonical}` is a {kind}, not a table"
                        )));
                    }
                    None if stmt.if_exists => {}
                    None => {
                        return Err(SQLError::Unsupported(format!(
                            "DROP TABLE: relation `{name}` does not exist"
                        )));
                    }
                }
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
        DropKind::Index => {
            let mut indexes = Vec::new();
            for name in &stmt.names {
                let Some(row) = engine
                    .catalog_index(name)
                    .map_err(|err| ddl_storage_error("DROP INDEX", err))?
                else {
                    if stmt.if_exists {
                        continue;
                    }
                    return Err(SQLError::Unsupported(format!(
                        "DROP INDEX: index `{name}` does not exist"
                    )));
                };
                indexes.push(row);
            }
            for row in indexes {
                drop_index_side_effects(engine, &row)?;
                engine
                    .try_drop_catalog_index(&row.name)
                    .map_err(|e| ddl_storage_error("DROP INDEX", e))?;
            }
        }
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
                match engine
                    .try_resolve_relation_kind(name)
                    .map_err(|err| ddl_storage_error(command, err))?
                {
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
            let drop_set = views
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            for view in &views {
                let dependents = engine
                    .views_depending_on_relation(view)
                    .map_err(|err| ddl_storage_error(command, err))?
                    .into_iter()
                    .filter(|dependent| !drop_set.contains(dependent))
                    .collect::<Vec<_>>();
                if !dependents.is_empty() {
                    return Err(SQLError::Unsupported(format!(
                        "{command} `{view}` rejected: dependent view(s) `{}` still reference it",
                        dependents.join("`, `")
                    )));
                }
            }
            engine.drop_views(&views)?;
        }
        DropKind::Sequence => {
            let mut sequences = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            let current_user = engine.session_execution_view().current_user();
            for name in &stmt.names {
                match engine.try_resolve_sequence_relation_kind(name, &current_user)? {
                    Some((canonical, "sequence")) => {
                        if seen.insert(canonical.clone()) {
                            sequences.push(canonical);
                        }
                    }
                    Some((_canonical, _kind)) => {
                        return Err(SQLError::Routine {
                            sqlstate: "42809".into(),
                            message: format!("\"{name}\" is not a sequence"),
                        });
                    }
                    None => {
                        if stmt.if_exists {
                            engine.push_sql_notice(
                                "NOTICE",
                                &format!("sequence \"{name}\" does not exist, skipping"),
                            );
                        } else {
                            engine.ensure_sequence_reference_schema_exists(name)?;
                            return Err(SQLError::Routine {
                                sqlstate: "42P01".into(),
                                message: format!("sequence \"{name}\" does not exist"),
                            });
                        }
                    }
                }
            }
            engine.drop_sequences_sql_inner(&sequences, stmt.cascade)?;
        }
        DropKind::Schema => {
            let mut schemas = Vec::new();
            let mut graphs = Vec::new();
            for name in &stmt.names {
                let exists = engine
                    .preflight_drop_schema(name)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?;
                if exists {
                    schemas.push(name.clone());
                    continue;
                }
                // A named graph owns a namespace of the same name whose
                // label relations always depend on it, exactly like AGE's
                // graph schema: RESTRICT fails and CASCADE drops the graph.
                if engine
                    .has_graph(name)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?
                {
                    if !stmt.cascade {
                        return Err(SQLError::Routine {
                            sqlstate: "2BP01".into(),
                            message: format!(
                                "cannot drop schema {name} because other objects depend on it"
                            ),
                        });
                    }
                    graphs.push(name.clone());
                } else if !stmt.if_exists {
                    return Err(SQLError::Unsupported(format!(
                        "DROP SCHEMA: schema `{name}` does not exist"
                    )));
                }
            }
            for schema in schemas {
                engine
                    .drop_schema(&schema)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?;
            }
            for graph in graphs {
                engine
                    .drop_graph(&graph)
                    .map_err(|err| ddl_storage_error("DROP SCHEMA", err))?;
            }
        }
    }
    Ok(SQLResult::empty())
}

pub(super) fn ddl_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

fn drop_index_side_effects(engine: &Engine, row: &CatalogIndexRow) -> Result<(), SQLError> {
    if row.index_type.eq_ignore_ascii_case("gin") {
        drop_gin_index_side_effects(engine, row)?;
    } else if row.index_type.eq_ignore_ascii_case("ivf")
        || row.index_type.eq_ignore_ascii_case("hnsw")
    {
        drop_vector_index_side_effects(engine, row)?;
    }
    Ok(())
}

fn catalog_index_columns(row: &CatalogIndexRow, action: &str) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(&row.columns_json).map_err(|e| {
        SQLError::Internal(format!(
            "{action} `{}`: invalid index column metadata: {e}",
            row.name
        ))
    })
}

fn drop_gin_index_side_effects(engine: &Engine, row: &CatalogIndexRow) -> Result<(), SQLError> {
    let fields: std::collections::BTreeSet<String> = catalog_index_columns(row, "DROP INDEX")?
        .into_iter()
        .collect();
    let indexes = engine
        .list_catalog_indexes()
        .map_err(|err| ddl_storage_error("DROP INDEX", err))?;

    for field in fields {
        let mut still_referenced = false;
        for candidate in &indexes {
            if candidate.name == row.name
                || candidate.table_name != row.table_name
                || !candidate.index_type.eq_ignore_ascii_case("gin")
            {
                continue;
            }
            if catalog_index_columns(candidate, "DROP INDEX")?
                .iter()
                .any(|candidate_field| candidate_field == &field)
            {
                still_referenced = true;
                break;
            }
        }
        if !still_referenced {
            engine
                .drop_fts_field(&row.table_name, &field)
                .map_err(|err| {
                    SQLError::Internal(format!(
                        "DROP INDEX `{}`: failed to remove FTS field `{}`.`{field}`: {err}",
                        row.name, row.table_name
                    ))
                })?;
        }
    }
    Ok(())
}

fn drop_vector_index_side_effects(engine: &Engine, row: &CatalogIndexRow) -> Result<(), SQLError> {
    let columns = catalog_index_columns(row, "DROP INDEX")?;
    for col in columns {
        match engine
            .column_type(&row.table_name, &col)
            .map_err(|err| ddl_storage_error("DROP INDEX", err))?
        {
            Some(ColumnType::Vector(dim) | ColumnType::Tensor(dim)) => {
                if !engine
                    .drop_vector_field_index(&row.table_name, col.clone(), dim)
                    .map_err(|err| ddl_storage_error("DROP INDEX vector field", err))?
                {
                    return Err(SQLError::Unsupported(format!(
                        "DROP INDEX `{}`: relation `{}` does not exist",
                        row.name, row.table_name
                    )));
                }
                engine
                    .drop_vector_index_metadata(&row.table_name, &col)
                    .map_err(|e| {
                        SQLError::Internal(format!(
                            "DROP INDEX `{}`: failed to drop vector-index metadata for `{}`.`{col}`: {e}",
                            row.name, row.table_name
                        ))
                    })?;
            }
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "DROP INDEX `{}`: vector-index column `{}`.`{col}` is no longer VECTOR or TENSOR, got {other:?}",
                    row.name, row.table_name
                )));
            }
            None => {
                return Err(SQLError::Unsupported(format!(
                    "DROP INDEX `{}`: column `{}`.`{col}` does not exist",
                    row.name, row.table_name
                )));
            }
        }
    }
    Ok(())
}
