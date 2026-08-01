//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence DDL and CREATE TABLE AS execution.

use super::{ddl_storage_error, Document, Engine, SQLError, SQLParam, SQLResult};

pub(in crate::sql) fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .create_sequence(&s.name, s.start, s.increment, s.if_not_exists)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .alter_sequence_if_exists(&s.name, s.restart, s.increment, s.start, s.if_exists)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

pub(in crate::sql) fn run_create_table_as(
    engine: &Engine,
    name: String,
    if_not_exists: bool,
    query: &uqa_planner::QueryPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine
        .try_has_table(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?
    {
        if if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "Table `{name}` already exists"
        )));
    }
    let result = crate::sql::select::execute_query_plan(engine, query, params)?;
    let cols: Vec<uqa_sql::ast::ColumnDef> = result
        .columns
        .iter()
        .map(|c| uqa_sql::ast::ColumnDef {
            name: c.clone(),
            ty: uqa_sql::ast::ColumnType::Text,
            primary_key: false,
            not_null: false,
            auto_increment: false,
            unique: false,
            default: None,
            check: None,
            references: None,
        })
        .collect();
    let analyzer = uqa_analysis::analyzer::standard_analyzer("english");
    engine
        .create_table(name.clone(), analyzer, Vec::new())
        .map_err(|err| ddl_storage_error("CREATE TABLE AS", err))?;
    if let Some(t) = engine
        .try_table(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?
    {
        (*t.columns.write()).clone_from(&cols);
    }
    engine
        .try_persist_table_schema(&name)
        .map_err(|err| ddl_storage_error("CREATE TABLE AS schema", err))?;
    let mut affected: u64 = 0;
    for (idx, row) in result.rows.iter().enumerate() {
        let doc_id = (idx as u64) + 1;
        let mut document = Document::new();
        for (k, v) in row {
            document.insert(k.clone(), v.clone());
        }
        engine.add_document(&name, doc_id, document)?;
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
}
