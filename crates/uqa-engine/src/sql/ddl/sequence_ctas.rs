//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence DDL and CREATE TABLE AS execution.

use std::collections::BTreeSet;

use super::{ddl_storage_error, ColumnType, Document, Engine, SQLError, SQLParam, SQLResult};

pub(in crate::sql) struct CreateTableAsExecution<'a> {
    pub name: &'a str,
    pub if_not_exists: bool,
    pub column_names: &'a [String],
    pub with_no_data: bool,
    pub persistence: uqa_sql::ast::RelationPersistence,
    pub on_commit: uqa_sql::ast::OnCommitAction,
    pub query: &'a uqa_planner::QueryPlan,
    pub params: &'a [SQLParam],
}

pub(in crate::sql) fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    if !engine.create_sequence_sql(&s)? {
        engine.push_sql_notice(
            "NOTICE",
            &format!("relation \"{}\" already exists, skipping", s.name),
        );
    }
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    if !engine.alter_sequence_sql(&s)? {
        engine.push_sql_notice(
            "NOTICE",
            &format!("relation \"{}\" does not exist, skipping", s.name),
        );
    }
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_create_table_as(
    engine: &Engine,
    execution: CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        run_create_table_as_inner(engine, &execution)
    } else {
        engine.transaction(move |engine| run_create_table_as_inner(engine, &execution))
    }
}

fn run_create_table_as_inner(
    engine: &Engine,
    execution: &CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    let ctes = crate::sql::select::CteScope::new_for_current_routine(engine);
    let mut query_schema = execution
        .with_no_data
        .then(|| {
            crate::sql::select::analyze_query_plan_schema(
                engine,
                execution.query,
                execution.params,
                &ctes,
                None,
            )
        })
        .transpose()?;
    let preliminary_name = create_table_as_target_name(engine, execution);
    if let Ok(name) = &preliminary_name {
        if should_skip_existing_create_table_as(engine, name, execution.if_not_exists)? {
            return Ok(SQLResult::empty());
        }
    }
    // A locking source must acquire and recheck every tuple before this session promotes its deferred backend transaction. Promoting first would invert the global writer and tuple-lock order against a concurrent updater. The target is checked again after promotion so a concurrent relation create still wins atomically.
    let locking_result =
        if !execution.with_no_data && crate::sql::select::query_has_row_locks(execution.query) {
            query_schema = Some(crate::sql::select::analyze_query_plan_schema(
                engine,
                execution.query,
                execution.params,
                &ctes,
                None,
            )?);
            Some(crate::sql::select::execute_query_plan(
                engine,
                execution.query,
                execution.params,
            )?)
        } else {
            None
        };
    if execution.persistence != uqa_sql::ast::RelationPersistence::Temporary {
        engine.prepare_explicit_transaction_writer()?;
    }
    let name = create_table_as_target_name(engine, execution)?;
    if should_skip_existing_create_table_as(engine, &name, execution.if_not_exists)? {
        return Ok(SQLResult::empty());
    }
    let query_schema = match query_schema {
        Some(schema) => schema,
        None => crate::sql::select::analyze_query_plan_schema(
            engine,
            execution.query,
            execution.params,
            &ctes,
            None,
        )?,
    };
    let columns = create_table_as_columns(&query_schema, execution.column_names)?;
    let result = if execution.with_no_data {
        None
    } else if let Some(result) = locking_result {
        Some(result)
    } else {
        Some(crate::sql::select::execute_query_plan(
            engine,
            execution.query,
            execution.params,
        )?)
    };
    if let Some(result) = &result {
        if result.columns.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "CREATE TABLE AS query schema width {} changed to {} during execution",
                columns.len(),
                result.columns.len()
            )));
        }
    }
    create_table_as_relation(
        engine,
        &name,
        &columns,
        execution.persistence,
        execution.on_commit,
    )?;
    let affected = result.as_ref().map_or(Ok(0), |result| {
        materialize_create_table_as_rows(engine, &name, &columns, result)
    })?;
    Ok(SQLResult::from_affected(affected))
}

fn create_table_as_target_name(
    engine: &Engine,
    execution: &CreateTableAsExecution<'_>,
) -> Result<String, SQLError> {
    if execution.persistence == uqa_sql::ast::RelationPersistence::Temporary {
        engine
            .try_temporary_relation_name_for_create(execution.name)
            .map_err(SQLError::Unsupported)
    } else {
        engine
            .try_relation_name_for_create(execution.name)
            .map_err(SQLError::Unsupported)
    }
}

fn should_skip_existing_create_table_as(
    engine: &Engine,
    name: &str,
    if_not_exists: bool,
) -> Result<bool, SQLError> {
    if engine
        .relation_kind_at(name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?
        .is_none()
    {
        return Ok(false);
    }
    if if_not_exists {
        return Ok(true);
    }
    Err(SQLError::Routine {
        sqlstate: "42P07".into(),
        message: format!("relation \"{name}\" already exists"),
    })
}

fn create_table_as_relation(
    engine: &Engine,
    name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    persistence: uqa_sql::ast::RelationPersistence,
    on_commit: uqa_sql::ast::OnCommitAction,
) -> Result<(), SQLError> {
    let analyzer = uqa_analysis::analyzer::standard_analyzer("english");
    engine
        .create_table_with_lifecycle(name, analyzer, Vec::new(), persistence, on_commit)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?;
    for column in columns {
        if let ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions) = column.ty {
            engine
                .create_vector_field(name, column.name.clone(), dimensions)
                .map_err(|err| ddl_storage_error("CREATE TABLE AS vector field", err))?;
        }
    }
    let table = engine
        .try_table(name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?
        .ok_or_else(|| {
            SQLError::Internal(format!("new CREATE TABLE AS relation `{name}` disappeared"))
        })?;
    *table.columns.write() = columns.to_vec();
    engine
        .try_persist_table_schema(name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?;
    Ok(())
}

fn materialize_create_table_as_rows(
    engine: &Engine,
    name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    result: &SQLResult,
) -> Result<u64, SQLError> {
    for (row_index, _) in result.rows.iter().enumerate() {
        let doc_id = u64::try_from(row_index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| SQLError::Internal("CREATE TABLE AS row count overflow".into()))?;
        let mut document = Document::new();
        for (column_index, column) in columns.iter().enumerate() {
            let value = result
                .value_at(row_index, column_index)
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "CREATE TABLE AS row {row_index} is missing column {column_index}"
                    ))
                })?;
            document.insert(column.name.clone(), value);
        }
        crate::sql::dml::stamp_tuple_xmin(engine, name, &mut document)?;
        let vectors = crate::sql::dml::document_vectors(engine, name, &document)?;
        engine.add_document_with_vector_values(name, doc_id, document, vectors)?;
    }
    u64::try_from(result.rows.len())
        .map_err(|_| SQLError::Internal("CREATE TABLE AS row count overflow".into()))
}

fn create_table_as_columns(
    query_schema: &uqa_execution::RowSchema,
    column_names: &[String],
) -> Result<Vec<uqa_sql::ast::ColumnDef>, SQLError> {
    if column_names.len() > query_schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "too many column names were specified".into(),
        });
    }
    let names = query_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, name)| {
            column_names
                .get(position)
                .cloned()
                .unwrap_or_else(|| name.clone())
        })
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    for name in &names {
        super::validate_postgres_column_name(name)?;
        if !seen.insert(name) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{name}\" specified more than once"),
            });
        }
    }
    Ok(names
        .into_iter()
        .enumerate()
        .map(|(position, name)| uqa_sql::ast::ColumnDef {
            name,
            ty: query_schema
                .column_type(position)
                .cloned()
                .unwrap_or(ColumnType::Text),
            object_id: None,
            missing_value: None,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            references: None,
        })
        .collect())
}
