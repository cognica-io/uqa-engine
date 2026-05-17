//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `Engine::sql` driver: parse SQL via `uqa_sql::compile`, lower each
//! statement onto the engine's mutation / search APIs, and roll the
//! result rows into a [`SQLResult`].
//!
//! Phase 5 covers the quickstart slice: `CREATE TABLE` (with `VECTOR(N)`
//! columns), `CREATE INDEX ... USING gin (...)` (recorded as an FTS
//! field), `CREATE INDEX ... USING ivf (...)` (`hnsw` is accepted as an
//! IVF alias), `INSERT ... VALUES`, and `SELECT` with `text_match`,
//! `knn_match`, and `fuse_log_odds` calls in `WHERE`. Statements outside this surface return
//! [`uqa_sql::SQLError::Unsupported`] cleanly.

#![allow(
    clippy::useless_format,
    clippy::manual_let_else,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::items_after_statements,
    clippy::unnecessary_map_or,
    clippy::match_same_arms,
    clippy::unnested_or_patterns,
    clippy::too_many_lines
)]

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, TemporalValue, Value};
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnDef as SQLColumnDef, ColumnType, CreateIndex,
    CreateTable, DeleteStmt, DropKind, DropStmt, Expr, ForeignKey, ForeignKeyAction,
    ForeignKeyMatch, FromClause, InsertStmt, JoinKind, OrderBy, Projection, SelectStmt, SetOpKind,
    Statement, UpdateStmt, WindowSpec, CTE,
};
use uqa_sql::expr::{eval, value_to_vector, EvalContext};
use uqa_sql::registry::{lookup, registered_names, FunctionKind};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, IVFIndexParams, ScoredEntry};

mod age_cypher;

const SCORE_COLUMN: &str = "_score";
const DOC_ID_COLUMN: &str = "_doc_id";

pub fn execute(engine: &Engine, sql: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
    // Reject cancelled tokens up-front so a stale cancel signal does
    // not leak into a fresh batch. Callers that want the
    // cancellation flag preserved across statements should use
    // [`crate::Engine::reset_cancellation`] explicitly between calls.
    engine.cancellation_token().check()?;
    let stmts = compile(sql)?;
    if stmts.is_empty() {
        return Ok(SQLResult::empty());
    }
    let mut last = SQLResult::empty();
    for stmt in stmts {
        engine.cancellation_token().check()?;
        last = run_stmt(engine, stmt, params)?;
    }
    Ok(last)
}

fn run_stmt(engine: &Engine, stmt: Statement, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
    let stmt = optimize_statement(stmt);
    match stmt {
        Statement::CreateTable(c) => run_create_table(engine, c),
        Statement::CreateIndex(c) => run_create_index(engine, c),
        Statement::Insert(i) => run_insert(engine, i, params),
        Statement::Select(s) => run_select(engine, *s, params),
        Statement::Update(u) => run_update(engine, u, params),
        Statement::Delete(d) => run_delete(engine, d, params),
        Statement::Drop(d) => run_drop(engine, d),
        Statement::AlterTable(a) => run_alter_table(engine, a),
        Statement::CreateView {
            name,
            body,
            or_replace,
        } => {
            if engine.has_table(&name) {
                return Err(SQLError::Unsupported(format!(
                    "CREATE VIEW: relation `{name}` already exists as a table"
                )));
            }
            if !or_replace && engine.view(&name).is_some() {
                return Err(SQLError::Unsupported(format!(
                    "CREATE VIEW: relation `{name}` already exists"
                )));
            }
            engine.register_view(&name, *body);
            Ok(SQLResult::empty())
        }
        Statement::CreateSchema {
            name,
            if_not_exists,
        } => {
            engine.register_schema(&name, if_not_exists);
            Ok(SQLResult::empty())
        }
        Statement::Explain { body, .. } => run_explain(engine, *body, params),
        Statement::SetVariable { name, value } => {
            engine.set_variable(&name, &value);
            Ok(SQLResult::empty())
        }
        Statement::ShowVariable { name } => {
            let value = engine.show_variable(&name);
            let mut row = ResultRow::new();
            row.insert(name.clone(), Value::Str(value));
            Ok(SQLResult {
                columns: vec![name],
                rows: vec![row],
                affected_rows: 0,
            })
        }
        Statement::Discard { target } => {
            engine.discard(target);
            Ok(SQLResult::empty())
        }
        Statement::Analyze { table } => {
            engine.run_analyze(table.as_deref());
            Ok(SQLResult::empty())
        }
        Statement::Truncate { tables, .. } => {
            for t in &tables {
                if !engine.has_table(t) {
                    return Err(SQLError::Unsupported(format!(
                        "TRUNCATE TABLE: relation `{t}` does not exist"
                    )));
                }
                engine.truncate_table(t);
            }
            Ok(SQLResult::empty())
        }
        Statement::Transaction(tx) => {
            engine.run_transaction_statement(tx)?;
            Ok(SQLResult::empty())
        }
        Statement::CreateSequence(s) => run_create_sequence(engine, s),
        Statement::AlterSequence(s) => run_alter_sequence(engine, s),
        Statement::CreateTableAs {
            name,
            if_not_exists,
            body,
        } => run_create_table_as(engine, name, if_not_exists, *body, params),
        Statement::Prepare { name, body } => {
            if engine.lookup_prepared(&name).is_some() {
                return Err(SQLError::Unsupported(format!(
                    "Prepared statement `{name}` already exists"
                )));
            }
            engine.register_prepared(name, *body);
            Ok(SQLResult::empty())
        }
        Statement::Execute { name, params: ps } => run_execute_prepared(engine, &name, &ps, params),
        Statement::Deallocate { name } => {
            if let Some(ref n) = name {
                if engine.lookup_prepared(n).is_none() {
                    return Err(SQLError::Unsupported(format!(
                        "Prepared statement `{n}` does not exist"
                    )));
                }
            }
            engine.deallocate_prepared(name.as_deref());
            Ok(SQLResult::empty())
        }
        Statement::Values { rows } => run_values(engine, rows, params),
        Statement::CreateForeignServer(s) => {
            engine
                .register_foreign_server(s.name, s.fdw_type, s.options, s.if_not_exists)
                .map_err(SQLError::Unsupported)?;
            Ok(SQLResult::empty())
        }
        Statement::CreateForeignTable(s) => {
            engine
                .register_foreign_table(
                    s.name,
                    s.server_name,
                    s.columns,
                    s.options,
                    s.if_not_exists,
                )
                .map_err(SQLError::Unsupported)?;
            Ok(SQLResult::empty())
        }
        Statement::Merge(m) => run_merge(engine, m, params),
    }
}

#[allow(clippy::too_many_lines)]
fn run_merge(
    engine: &Engine,
    stmt: uqa_sql::ast::MergeStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_merge_inner(engine, stmt, params))
}

#[allow(clippy::too_many_lines)]
fn run_merge_inner(
    engine: &Engine,
    stmt: uqa_sql::ast::MergeStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::ast::MergeWhen;
    use uqa_sql::expr::{eval, truthy, EvalContext};
    let target_table = stmt.target.clone();
    let target_qual = stmt
        .target_alias
        .clone()
        .unwrap_or_else(|| target_table.clone());
    let ctes: BTreeMap<String, Vec<ResultRow>> = BTreeMap::new();
    let source_rows = build_join_rows_with_ctes(engine, &stmt.source, params, &ctes)?;
    let mut affected = 0_u64;

    struct Pairing {
        doc_id: Option<uqa_core::DocId>,
        target_row: ResultRow,
        source_row: Option<ResultRow>,
    }
    let mut pairings: Vec<Pairing> = Vec::new();
    let mut matched_source: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    for doc_id in &engine.table_doc_ids(&target_table) {
        let Some(doc) = engine.get_document(&target_table, *doc_id) else {
            continue;
        };
        let target_row = prefix_row(&target_qual, &doc);
        let mut paired_idx: Option<usize> = None;
        for (idx, src) in source_rows.iter().enumerate() {
            if matched_source.contains(&idx) {
                continue;
            }
            let mut joined = ResultRow::new();
            for (k, v) in &target_row {
                joined.insert(k.clone(), v.clone());
            }
            for (k, v) in src {
                joined.insert(k.clone(), v.clone());
            }
            let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
            if truthy(&eval(&stmt.join_condition, &ctx)?) {
                paired_idx = Some(idx);
                matched_source.insert(idx);
                break;
            }
        }
        // Skip target rows that don't pair with any source row --
        // MERGE only emits an action when the join condition holds.
        if let Some(idx) = paired_idx {
            pairings.push(Pairing {
                doc_id: Some(*doc_id),
                target_row,
                source_row: Some(source_rows[idx].clone()),
            });
        }
    }
    for (idx, src) in source_rows.iter().enumerate() {
        if matched_source.contains(&idx) {
            continue;
        }
        pairings.push(Pairing {
            doc_id: None,
            target_row: ResultRow::new(),
            source_row: Some(src.clone()),
        });
    }

    for pair in pairings {
        // MERGE matched semantics: a target row is "matched" only when
        // the join produced a source pairing. A target row that has
        // no corresponding source counts as unmatched and falls
        // through to the WHEN NOT MATCHED branches.
        let matched = pair.doc_id.is_some() && pair.source_row.is_some();
        let mut joined = pair.target_row.clone();
        if let Some(src) = &pair.source_row {
            for (k, v) in src {
                joined.insert(k.clone(), v.clone());
            }
        }
        for clause in &stmt.when_clauses {
            let (condition, action_idx, applies) = match clause {
                MergeWhen::UpdateMatched { condition, .. } if matched => {
                    (condition.as_ref(), 0_u8, true)
                }
                MergeWhen::DeleteMatched { condition } if matched => (condition.as_ref(), 1, true),
                MergeWhen::NothingMatched { condition } if matched => (condition.as_ref(), 2, true),
                MergeWhen::InsertNotMatched { condition, .. } if !matched => {
                    (condition.as_ref(), 3, true)
                }
                MergeWhen::NothingNotMatched { condition } if !matched => {
                    (condition.as_ref(), 2, true)
                }
                _ => (None, 0, false),
            };
            if !applies {
                continue;
            }
            if let Some(c) = condition {
                let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
                if !truthy(&eval(c, &ctx)?) {
                    continue;
                }
            }
            match (action_idx, clause) {
                (0, MergeWhen::UpdateMatched { assignments, .. }) => {
                    if let Some(doc_id) = pair.doc_id {
                        let Some(mut doc) = engine.get_document(&target_table, doc_id) else {
                            break;
                        };
                        let original_doc = doc.clone();
                        let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
                        for (col, expr) in assignments {
                            let value = coerce_to_column_type(
                                engine,
                                &target_table,
                                col,
                                eval(expr, &ctx)?,
                            )?;
                            doc.insert(col.clone(), value);
                        }
                        rewrite_document_with_referential_actions(
                            engine,
                            &target_table,
                            doc_id,
                            &original_doc,
                            doc,
                            params,
                        )?;
                        affected += 1;
                    }
                }
                (1, MergeWhen::DeleteMatched { .. }) => {
                    if let Some(doc_id) = pair.doc_id {
                        let root_deletes = BTreeSet::from([(target_table.clone(), doc_id)]);
                        let mut delete_stack = Vec::new();
                        delete_document_with_referential_actions(
                            engine,
                            &target_table,
                            doc_id,
                            params,
                            &root_deletes,
                            &mut delete_stack,
                        )?;
                        affected += 1;
                    }
                }
                (
                    3,
                    MergeWhen::InsertNotMatched {
                        columns, values, ..
                    },
                ) => {
                    let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
                    let mut document = Document::new();
                    if values.len() != columns.len() {
                        return Err(SQLError::Internal(format!(
                            "MERGE INSERT row width {} != column count {}",
                            values.len(),
                            columns.len()
                        )));
                    }
                    for (i, col) in columns.iter().enumerate() {
                        let v = coerce_to_column_type(
                            engine,
                            &target_table,
                            col,
                            eval(&values[i], &ctx)?,
                        )?;
                        document.insert(col.clone(), v);
                    }
                    let id_col = engine.auto_increment_column(&target_table);
                    let doc_id = match id_col.as_deref().and_then(|c| document.get(c)) {
                        Some(Value::Int(n)) if *n >= 0 => *n as u64,
                        _ => engine.allocate_next_id(&target_table)?,
                    };
                    if let Some(c) = id_col.as_deref() {
                        document.insert(c.into(), Value::Int(doc_id as i64));
                    }
                    engine.advance_next_id(&target_table, doc_id);
                    insert_document_with_constraints(
                        engine,
                        &target_table,
                        doc_id,
                        document,
                        params,
                    )?;
                    affected += 1;
                }
                (2, _) => {}
                _ => {}
            }
            break;
        }
    }
    Ok(SQLResult::from_affected(affected))
}

fn run_create_sequence(
    engine: &Engine,
    s: uqa_sql::ast::CreateSequence,
) -> Result<SQLResult, SQLError> {
    if !engine.create_sequence(&s.name, s.start, s.increment, s.if_not_exists) {
        return Err(SQLError::Unsupported(format!(
            "Sequence `{}` already exists",
            s.name
        )));
    }
    Ok(SQLResult::empty())
}

fn run_alter_sequence(
    engine: &Engine,
    s: uqa_sql::ast::AlterSequence,
) -> Result<SQLResult, SQLError> {
    engine
        .alter_sequence(&s.name, s.restart, s.increment, s.start)
        .map_err(SQLError::Unsupported)?;
    Ok(SQLResult::empty())
}

fn run_create_table_as(
    engine: &Engine,
    name: String,
    if_not_exists: bool,
    body: uqa_sql::ast::SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine.table(&name).is_some() {
        if if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "Table `{name}` already exists"
        )));
    }
    let result = run_select(engine, body, params)?;
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
    engine.create_table(name.clone(), analyzer, Vec::new());
    if let Some(t) = engine.table(&name) {
        (*t.columns.write()).clone_from(&cols);
    }
    let mut affected: u64 = 0;
    for (idx, row) in result.rows.iter().enumerate() {
        let doc_id = (idx as u64) + 1;
        let mut document = Document::new();
        for (k, v) in row {
            document.insert(k.clone(), v.clone());
        }
        engine.add_document(&name, doc_id, document);
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
}

fn run_execute_prepared(
    engine: &Engine,
    name: &str,
    args: &[uqa_sql::ast::Expr],
    outer_params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let stmt = engine.lookup_prepared(name).ok_or_else(|| {
        SQLError::Unsupported(format!("Prepared statement `{name}` does not exist"))
    })?;
    let ctx = uqa_sql::expr::EvalContext::new(None, outer_params).with_engine(engine);
    let mut bound: Vec<SQLParam> = Vec::with_capacity(args.len());
    for a in args {
        let v = uqa_sql::expr::eval(a, &ctx)?;
        bound.push(SQLParam::Scalar(v));
    }
    run_stmt(engine, stmt, &bound)
}

fn run_values(
    engine: &Engine,
    rows: Vec<Vec<uqa_sql::ast::Expr>>,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    let ctx = EvalContext::new(None, params).with_engine(engine);
    if rows.is_empty() {
        return Ok(SQLResult::empty());
    }
    let columns: Vec<String> = (0..rows[0].len())
        .map(|i| format!("column{}", i + 1))
        .collect();
    let mut out_rows: Vec<uqa_sql::result::ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = uqa_sql::result::ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        out_rows.push(r);
    }
    Ok(SQLResult {
        columns,
        rows: out_rows,
        affected_rows: 0,
    })
}

fn optimize_statement(stmt: Statement) -> Statement {
    use uqa_planner::optimizer::{optimize, OptimizerConfig};
    let cfg = OptimizerConfig::default();
    match stmt {
        Statement::Select(s) => Statement::Select(Box::new(optimize(*s, &cfg))),
        other => other,
    }
}

fn run_drop(engine: &Engine, stmt: DropStmt) -> Result<SQLResult, SQLError> {
    match stmt.kind {
        DropKind::Table => {
            for name in &stmt.names {
                if !engine.has_table(name) {
                    if stmt.if_exists {
                        continue;
                    }
                    return Err(SQLError::Unsupported(format!(
                        "DROP TABLE: relation `{name}` does not exist"
                    )));
                }
                let referrers = engine.referrers_to(name);
                if !referrers.is_empty() {
                    if stmt.cascade {
                        // CASCADE: drop every referrer first. The
                        // recursive walk catches transitive
                        // dependencies (A -> B -> C).
                        let referrer_names: Vec<String> =
                            referrers.iter().map(|(n, _)| n.clone()).collect();
                        let mut queue: Vec<String> = referrer_names;
                        while let Some(other) = queue.pop() {
                            for (next, _) in engine.referrers_to(&other) {
                                queue.push(next);
                            }
                            engine.drop_table(&other);
                        }
                    } else {
                        let names: Vec<String> = referrers.iter().map(|(n, _)| n.clone()).collect();
                        return Err(SQLError::TypeMismatch(format!(
                            "DROP TABLE `{name}` rejected: still referenced by `{}`. Use CASCADE.",
                            names.join(", ")
                        )));
                    }
                }
                engine.drop_table(name);
            }
        }
        DropKind::Index => {
            // Persisted as `_catalog_indexes` rows. The in-memory
            // physical structures (FTS / vector indexes attached to
            // table fields) are not torn down here -- the catalog
            // entry merely tracks the CREATE INDEX statement so it
            // survives Engine::open.
            for name in &stmt.names {
                engine.drop_catalog_index(name);
            }
        }
        DropKind::View => {
            for name in &stmt.names {
                if !engine.drop_view(name) && !stmt.if_exists {
                    return Err(SQLError::Unsupported(format!(
                        "DROP VIEW: relation `{name}` does not exist"
                    )));
                }
            }
        }
        DropKind::Schema => {
            for name in &stmt.names {
                if !engine.drop_schema(name) && !stmt.if_exists {
                    return Err(SQLError::Unsupported(format!(
                        "DROP SCHEMA: schema `{name}` does not exist"
                    )));
                }
            }
        }
    }
    Ok(SQLResult::empty())
}

fn run_alter_table(engine: &Engine, stmt: AlterTableStmt) -> Result<SQLResult, SQLError> {
    if !engine.has_table(&stmt.table) {
        if stmt.if_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "ALTER TABLE: relation `{}` does not exist",
            stmt.table
        )));
    }
    match stmt.action {
        AlterTableAction::AddColumn {
            column,
            if_not_exists,
        } => {
            let col_name = column.name.clone();
            if engine.table_has_column(&stmt.table, &col_name) {
                if if_not_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ADD COLUMN: column `{col_name}` already exists"
                )));
            }
            match column.ty {
                ColumnType::Vector(dim) => {
                    engine.create_vector_field(&stmt.table, col_name.clone(), dim);
                }
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, col_name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                _ => {}
            }
            // Capture the default expression and NOT NULL flag before
            // moving the column into the engine so we can backfill any
            // existing rows. PostgreSQL evaluates the default once per
            // existing row at ALTER TABLE time, which keeps NOT NULL
            // constraints satisfiable for non-empty tables.
            let default_expr = column.default.clone();
            let column_not_null = column.not_null;
            engine.register_column(&stmt.table, column);
            backfill_added_column(
                engine,
                &stmt.table,
                &col_name,
                default_expr.as_ref(),
                column_not_null,
            )?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: _,
        } => {
            if !engine.table_has_column(&stmt.table, &name) {
                if if_exists {
                    return Ok(SQLResult::empty());
                }
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE DROP COLUMN: column `{name}` does not exist"
                )));
            }
            engine.drop_column(&stmt.table, &name);
        }
        AlterTableAction::RenameColumn { from, to } => {
            if !engine.table_has_column(&stmt.table, &from) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{from}` does not exist"
                )));
            }
            if engine.table_has_column(&stmt.table, &to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{to}` already exists"
                )));
            }
            engine.rename_column(&stmt.table, &from, &to);
        }
        AlterTableAction::RenameTable { to } => {
            if engine.has_table(&to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !engine.rename_table(&stmt.table, &to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::SetDefault { name, default } => {
            if !engine.set_column_default(&stmt.table, &name, Some(default)) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
        }
        AlterTableAction::DropDefault { name } => {
            if !engine.set_column_default(&stmt.table, &name, None) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
        }
        AlterTableAction::SetNotNull { name } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            ensure_existing_values_not_null(engine, &stmt.table, &name)?;
            engine.set_column_not_null(&stmt.table, &name, true);
        }
        AlterTableAction::DropNotNull { name } => {
            if !engine.set_column_not_null(&stmt.table, &name, false) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
        }
        AlterTableAction::AlterColumnType { name, ty } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            rewrite_column_values_to_type(engine, &stmt.table, &name, &ty)?;
            engine.set_column_type(&stmt.table, &name, &ty);
            match ty {
                ColumnType::Text => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                ColumnType::Vector(dim) => {
                    engine.create_vector_field(&stmt.table, name, dim);
                }
                _ => {}
            }
        }
    }
    Ok(SQLResult::empty())
}

fn ensure_column_exists(engine: &Engine, table: &str, column: &str) -> Result<(), SQLError> {
    if engine.table_has_column(table, column) {
        Ok(())
    } else {
        Err(SQLError::Unsupported(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` does not exist"
        )))
    }
}

fn ensure_existing_values_not_null(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<(), SQLError> {
    let mut null_rows = 0usize;
    for doc_id in engine.table_doc_ids(table) {
        let Some(doc) = engine.get_document(table, doc_id) else {
            continue;
        };
        if matches!(doc.get(column), None | Some(Value::Null)) {
            null_rows += 1;
        }
    }
    if null_rows > 0 {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ALTER COLUMN: column `{column}` contains NULL values"
        )));
    }
    Ok(())
}

/// Coerce a write value to fit the column's declared type.
fn coerce_to_column_type(
    engine: &Engine,
    table: &str,
    column: &str,
    value: Value,
) -> Result<Value, SQLError> {
    let cols = match engine.describe_table(table) {
        Some(c) => c,
        None => return Ok(value),
    };
    let Some(def) = cols.iter().find(|c| c.name == column) else {
        return Ok(value);
    };
    if let ColumnType::Numeric { scale: Some(s), .. } = &def.ty {
        return Ok(round_numeric(value, *s));
    }
    if matches!(&def.ty, ColumnType::Real) {
        return match value {
            Value::Float(_) => Ok(value),
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Str(s) => Ok(s.parse::<f64>().map(Value::Float).unwrap_or(Value::Str(s))),
            other => Ok(other),
        };
    }
    if matches!(&def.ty, ColumnType::Json) {
        return Ok(coerce_json_value(value));
    }
    if matches!(&def.ty, ColumnType::Bytea) {
        return match value {
            Value::Bytes(_) => Ok(value),
            Value::Str(s) => Ok(Value::Bytes(s.into_bytes())),
            other => Ok(other),
        };
    }
    if is_temporal_column_type(&def.ty) {
        return convert_value_to_column_type(value, &def.ty);
    }
    Ok(value)
}

fn coerce_json_value(value: Value) -> Value {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_core_value)
            .unwrap_or(Value::Str(s)),
        other => other,
    }
}

fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    ty: &ColumnType,
) -> Result<(), SQLError> {
    for doc_id in engine.table_doc_ids(table) {
        let Some(doc) = engine.get_document(table, doc_id) else {
            continue;
        };
        let Some(value) = doc.get(column).cloned() else {
            continue;
        };
        let converted = convert_value_to_column_type(value, ty)?;
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), converted.clone());
        let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        if let Ok(vec) = value_to_vector(&converted) {
            vectors.insert(column.to_string(), vec);
        }
        engine.update_document_fields(table, doc_id, updates, vectors);
    }
    Ok(())
}

fn convert_value_to_column_type(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    match ty {
        ColumnType::Integer => match value {
            Value::Int(_) => Ok(value),
            Value::Float(f) => Ok(Value::Int(f as i64)),
            Value::Bool(b) => Ok(Value::Int(i64::from(b))),
            Value::Str(s) => s
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to integer: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to integer"
            ))),
        },
        ColumnType::Text => Ok(Value::Str(value_to_text(&value))),
        ColumnType::Real | ColumnType::Numeric { .. } => match value {
            Value::Float(_) => Ok(value),
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Bool(b) => Ok(Value::Float(if b { 1.0 } else { 0.0 })),
            Value::Str(s) => s
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|e| SQLError::TypeMismatch(format!("cannot cast `{s}` to real: {e}"))),
            other => Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to real"
            ))),
        },
        ColumnType::Json => Ok(coerce_json_value(value)),
        ColumnType::Bytea => Ok(match value {
            Value::Bytes(_) => value,
            Value::Str(s) => Value::Bytes(s.into_bytes()),
            other => Value::Bytes(value_to_text(&other).into_bytes()),
        }),
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimeTz
        | ColumnType::Timestamp
        | ColumnType::TimestampTz => convert_temporal_value(value, ty),
        ColumnType::Vector(_) => {
            value_to_vector(&value)?;
            Ok(value)
        }
    }
}

fn is_temporal_column_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Date
            | ColumnType::Time
            | ColumnType::TimeTz
            | ColumnType::Timestamp
            | ColumnType::TimestampTz
    )
}

fn column_type_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "integer",
        ColumnType::Text => "text",
        ColumnType::Real => "real",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::Bytea => "bytea",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "time with time zone",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamp with time zone",
        ColumnType::Vector(_) => "vector",
    }
}

fn convert_temporal_value(value: Value, ty: &ColumnType) -> Result<Value, SQLError> {
    match value {
        Value::Temporal(temporal) => Ok(Value::Temporal(temporal)),
        other => {
            let text = value_to_text(&other);
            let parsed = match ty {
                ColumnType::Date => TemporalValue::parse_date(&text),
                ColumnType::Time => TemporalValue::parse_time(&text),
                ColumnType::TimeTz => TemporalValue::parse_time_tz(&text),
                ColumnType::Timestamp => TemporalValue::parse_timestamp(&text),
                ColumnType::TimestampTz => TemporalValue::parse_timestamp_tz(&text),
                _ => None,
            };
            parsed.map(Value::Temporal).ok_or_else(|| {
                SQLError::TypeMismatch(format!("cannot cast `{text}` to {}", column_type_name(ty)))
            })
        }
    }
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

fn json_to_core_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::Str(s),
        serde_json::Value::Array(items) => {
            Value::List(items.into_iter().map(json_to_core_value).collect())
        }
        serde_json::Value::Object(obj) => {
            if let Ok(temporal) =
                serde_json::from_value::<TemporalValue>(serde_json::Value::Object(obj.clone()))
            {
                return Value::Temporal(temporal);
            }
            Value::Map(
                obj.into_iter()
                    .map(|(k, v)| (k, json_to_core_value(v)))
                    .collect(),
            )
        }
    }
}

fn core_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
        Value::Bytes(bytes) => serde_json::Value::String(String::from_utf8_lossy(bytes).into()),
        Value::Temporal(t) => serde_json::Value::String(t.to_sql_string()),
        Value::List(items) => {
            serde_json::Value::Array(items.iter().map(core_value_to_json).collect())
        }
        Value::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), core_value_to_json(v)))
                .collect(),
        ),
    }
}

fn json_table_value_to_text(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Bool(b) => Value::Str(b.to_string()),
        serde_json::Value::Number(n) => Value::Str(n.to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Value::Str(serde_json::to_string(value).unwrap_or_default())
        }
    }
}

fn json_table_arg(value: &Value, name: &str) -> Result<serde_json::Value, SQLError> {
    match value {
        Value::Str(s) => serde_json::from_str::<serde_json::Value>(s)
            .map_err(|e| SQLError::TypeMismatch(format!("{name}: invalid JSON: {e}"))),
        other => Ok(core_value_to_json(other)),
    }
}

fn round_numeric(value: Value, scale: u32) -> Value {
    let factor = 10f64.powi(scale as i32);
    match value {
        Value::Float(f) => {
            let rounded = (f * factor).round() / factor;
            Value::Float(rounded)
        }
        Value::Int(i) => Value::Float(i as f64),
        other => other,
    }
}

/// Apply the new column's DEFAULT (or NULL) value to every row that
/// existed before the ADD COLUMN. `PostgreSQL` evaluates the default
/// once per existing row at ALTER TABLE time so NOT NULL columns stay
/// consistent on non-empty tables; the UQA-RS implementation mirrors that
/// semantics by sweeping the document store.
fn backfill_added_column(
    engine: &Engine,
    table: &str,
    column: &str,
    default_expr: Option<&uqa_sql::ast::Expr>,
    not_null: bool,
) -> Result<(), SQLError> {
    let doc_ids = engine.table_doc_ids(table);
    if doc_ids.is_empty() {
        return Ok(());
    }
    let default_value = if let Some(expr) = default_expr {
        let ctx = EvalContext::new(None, &[]).with_engine(engine);
        eval(expr, &ctx)?
    } else if not_null {
        return Err(SQLError::TypeMismatch(format!(
            "ALTER TABLE ADD COLUMN `{column}` is NOT NULL but no DEFAULT supplied; \
             {} existing row(s) would violate the constraint",
            doc_ids.len()
        )));
    } else {
        Value::Null
    };
    let default_value = coerce_to_column_type(engine, table, column, default_value)?;
    let vector_value: Option<Vec<f32>> = value_to_vector(&default_value).ok();
    for doc_id in doc_ids {
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), default_value.clone());
        let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        if let Some(v) = vector_value.as_ref() {
            vectors.insert(column.to_string(), v.clone());
        }
        engine.update_document_fields(table, doc_id, updates, vectors);
    }
    Ok(())
}

fn run_update(
    engine: &Engine,
    stmt: UpdateStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_update_inner(engine, stmt, params))
}

fn run_update_inner(
    engine: &Engine,
    stmt: UpdateStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // UPDATE ... FROM other [WHERE ...]: build the joined relation,
    // evaluate the WHERE against each joined row, and apply
    // assignments to the matching target rows. Mirrors the canonical UQA implementation's
    // _compile_update_from.
    if let Some(from_clause) = stmt.from.as_ref() {
        return run_update_from(engine, &stmt, from_clause, params);
    }
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    for doc_id in engine.table_doc_ids(&stmt.table) {
        cancel.check()?;
        let mut doc = engine
            .get_document(&stmt.table, doc_id)
            .ok_or_else(|| SQLError::Internal("missing document during UPDATE".into()))?;
        let original_doc = doc.clone();
        if let Some(filter) = stmt.r#where.as_ref() {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(engine);
            if !uqa_sql::expr::truthy(&uqa_sql::expr::eval(filter, &ctx)?) {
                continue;
            }
        }
        for (col, expr) in &stmt.assignments {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(engine);
            let value =
                coerce_to_column_type(engine, &stmt.table, col, uqa_sql::expr::eval(expr, &ctx)?)?;
            doc.insert(col.clone(), value);
        }
        rewrite_document_with_referential_actions(
            engine,
            &stmt.table,
            doc_id,
            &original_doc,
            doc.clone(),
            params,
        )?;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_row(
                engine,
                &stmt.table,
                doc_id,
                &doc,
                &stmt.returning,
                params,
            )?);
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return Ok(dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
    }
    Ok(SQLResult::from_affected(affected))
}

fn validate_document_constraints(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for col_def in engine.describe_table(table).unwrap_or_default() {
        if !col_def.not_null || col_def.auto_increment {
            continue;
        }
        match document.get(&col_def.name) {
            Some(Value::Null) | None => {
                return Err(SQLError::TypeMismatch(format!(
                    "NOT NULL constraint violated: column `{}` in table `{table}`",
                    col_def.name
                )));
            }
            _ => {}
        }
    }

    for (cname, expr) in engine.check_constraints(table) {
        let row_ctx = EvalContext::new(Some(document), params).with_engine(engine);
        let result = eval(&expr, &row_ctx)?;
        if !uqa_sql::expr::truthy(&result) {
            let label = cname.unwrap_or_else(|| "<unnamed>".into());
            return Err(SQLError::TypeMismatch(format!(
                "CHECK constraint `{label}` violated in table `{table}`"
            )));
        }
    }

    for fk in engine.foreign_keys(table) {
        let Some(local_values) = foreign_key_lookup_values(&fk, document)? else {
            continue;
        };
        if engine
            .find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)
            .is_none()
        {
            let cols = fk.local_columns.join(", ");
            return Err(SQLError::TypeMismatch(format!(
                "FOREIGN KEY constraint violated: ({cols}) -> {}({}) has no matching row",
                fk.ref_table,
                fk.ref_columns.join(", ")
            )));
        }
    }

    for col in engine.unique_columns(table) {
        let Some(value) = document.get(&col).cloned() else {
            continue;
        };
        if matches!(value, Value::Null) {
            continue;
        }
        if let Some(conflict_id) = engine.find_conflict(
            table,
            std::slice::from_ref(&col),
            std::slice::from_ref(&value),
        ) {
            if conflict_id != doc_id {
                return Err(SQLError::TypeMismatch(format!(
                    "UNIQUE constraint violated: duplicate value for column `{col}` in table `{table}`"
                )));
            }
        }
    }
    Ok(())
}

fn foreign_key_lookup_values(
    fk: &ForeignKey,
    document: &Document,
) -> Result<Option<Vec<Value>>, SQLError> {
    let local_values: Vec<Value> = fk
        .local_columns
        .iter()
        .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
        .collect();
    let null_count = local_values
        .iter()
        .filter(|value| matches!(value, Value::Null))
        .count();
    if null_count == 0 {
        return Ok(Some(local_values));
    }
    match fk.match_type {
        ForeignKeyMatch::Simple => Ok(None),
        ForeignKeyMatch::Full if null_count == local_values.len() => Ok(None),
        ForeignKeyMatch::Full => {
            let cols = fk.local_columns.join(", ");
            Err(SQLError::TypeMismatch(format!(
                "FOREIGN KEY MATCH FULL constraint violated: ({cols}) must be all NULL or all non-NULL"
            )))
        }
    }
}

fn rewrite_document_with_referential_actions(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    old_doc: &Document,
    new_doc: Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_document_constraints(engine, table, doc_id, &new_doc, params)?;
    engine.rewrite_document(table, doc_id, new_doc.clone());
    apply_referenced_key_update_actions(engine, table, old_doc, &new_doc, params)
}

fn apply_referenced_key_update_actions(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(engine, table) {
        let old_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| old_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        let new_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| new_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if old_values == new_values || old_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &old_values);
        for (child_id, child_doc) in referencing {
            match fk.on_update {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::TypeMismatch(format!(
                        "FOREIGN KEY constraint violated: UPDATE on `{table}` is referenced by `{ref_table}` ({} -> {})",
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let mut updated = child_doc.clone();
                    for (col, value) in fk.local_columns.iter().zip(new_values.iter()) {
                        updated.insert(col.clone(), value.clone());
                    }
                    rewrite_document_with_referential_actions(
                        engine, &ref_table, child_id, &child_doc, updated, params,
                    )?;
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let mut updated = child_doc.clone();
                    apply_set_action_to_child(
                        engine,
                        &ref_table,
                        &child_doc,
                        &mut updated,
                        &fk.local_columns,
                        fk.on_update,
                        params,
                    )?;
                    rewrite_document_with_referential_actions(
                        engine, &ref_table, child_id, &child_doc, updated, params,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn referrers_to_for_actions(engine: &Engine, table: &str) -> Vec<(String, ForeignKey)> {
    let mut out = Vec::new();
    for other in engine.table_names() {
        for fk in engine.foreign_keys(&other) {
            if fk.ref_table == table {
                out.push((other.clone(), fk));
            }
        }
    }
    out
}

fn referencing_rows(
    engine: &Engine,
    table: &str,
    local_columns: &[String],
    key_values: &[Value],
) -> Vec<(DocId, Document)> {
    if local_columns.is_empty() || local_columns.len() != key_values.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for doc_id in engine.table_doc_ids(table) {
        let Some(doc) = engine.get_document(table, doc_id) else {
            continue;
        };
        let matches = local_columns
            .iter()
            .zip(key_values.iter())
            .all(|(col, want)| doc.get(col).cloned().unwrap_or(Value::Null) == *want);
        if matches {
            out.push((doc_id, doc));
        }
    }
    out
}

fn apply_set_action_to_child(
    engine: &Engine,
    table: &str,
    old_doc: &Document,
    new_doc: &mut Document,
    columns: &[String],
    action: ForeignKeyAction,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for column in columns {
        let value = match action {
            ForeignKeyAction::SetNull => Value::Null,
            ForeignKeyAction::SetDefault => {
                if let Some(expr) = engine.column_default_expr(table, column) {
                    let ctx = EvalContext::new(Some(old_doc), params).with_engine(engine);
                    eval(&expr, &ctx)?
                } else {
                    Value::Null
                }
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict | ForeignKeyAction::Cascade => {
                return Err(SQLError::Internal(format!(
                    "invalid SET action helper for `{action:?}`"
                )));
            }
        };
        let value = coerce_to_column_type(engine, table, column, value)?;
        new_doc.insert(column.clone(), value);
    }
    Ok(())
}

fn run_update_from(
    engine: &Engine,
    stmt: &UpdateStmt,
    from_clause: &uqa_sql::ast::FromClause,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let ctes: BTreeMap<String, Vec<ResultRow>> = BTreeMap::new();
    let from_rows = build_join_rows_with_ctes(engine, from_clause, params, &ctes)?;
    let cancel = engine.cancellation_token();
    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let target = stmt.table.clone();
    let target_doc_ids = engine.table_doc_ids(&target);
    for doc_id in target_doc_ids {
        cancel.check()?;
        let mut doc = engine
            .get_document(&target, doc_id)
            .ok_or_else(|| SQLError::Internal("missing document during UPDATE FROM".into()))?;
        let original_doc = doc.clone();
        let mut applied = false;
        for from_row in &from_rows {
            // Build a joined row: target columns are exposed both
            // unqualified and prefixed (`<table>.<col>`) so the
            // WHERE / RHS expressions can use either spelling.
            // FROM-side rows already carry their alias prefix when
            // one was supplied.
            let mut joined = ResultRow::new();
            for (k, v) in &doc {
                joined.insert(k.clone(), v.clone());
                joined.insert(format!("{target}.{k}"), v.clone());
            }
            for (k, v) in from_row {
                joined.insert(k.clone(), v.clone());
            }
            if let Some(filter) = stmt.r#where.as_ref() {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&joined), params).with_engine(engine);
                if !uqa_sql::expr::truthy(&uqa_sql::expr::eval(filter, &ctx)?) {
                    continue;
                }
            }
            // Apply assignments evaluated against the joined row so
            // RHS expressions can read FROM-side columns.
            let ctx = uqa_sql::expr::EvalContext::new(Some(&joined), params).with_engine(engine);
            for (col, expr) in &stmt.assignments {
                let value =
                    coerce_to_column_type(engine, &target, col, uqa_sql::expr::eval(expr, &ctx)?)?;
                doc.insert(col.clone(), value);
            }
            rewrite_document_with_referential_actions(
                engine,
                &target,
                doc_id,
                &original_doc,
                doc.clone(),
                params,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &target,
                    doc_id,
                    &doc,
                    &stmt.returning,
                    params,
                )?);
            }
            applied = true;
            break;
        }
        if applied {
            affected += 1;
        }
    }
    if !stmt.returning.is_empty() {
        return Ok(dml_returning_result(
            engine,
            &target,
            &stmt.returning,
            returning_rows,
            affected,
        ));
    }
    Ok(SQLResult::from_affected(affected))
}

fn run_delete(
    engine: &Engine,
    stmt: DeleteStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_delete_inner(engine, stmt, params))
}

fn run_delete_inner(
    engine: &Engine,
    stmt: DeleteStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut to_delete: Vec<uqa_core::DocId> = Vec::new();
    let mut returning_docs: Vec<(uqa_core::DocId, Document)> = Vec::new();
    // DELETE FROM t USING other WHERE ... -- materialise the join
    // first, then collect target doc ids whose joined image
    // satisfies WHERE. Mirrors the canonical UQA implementation's _compile_delete_using.
    let using_rows: Option<Vec<ResultRow>> = match stmt.using.as_ref() {
        Some(clause) => {
            let ctes: BTreeMap<String, Vec<ResultRow>> = BTreeMap::new();
            Some(build_join_rows_with_ctes(engine, clause, params, &ctes)?)
        }
        None => None,
    };
    for doc_id in engine.table_doc_ids(&stmt.table) {
        cancel.check()?;
        let Some(doc) = engine.get_document(&stmt.table, doc_id) else {
            continue;
        };
        let keep = match (stmt.r#where.as_ref(), using_rows.as_ref()) {
            (None, _) => true,
            (Some(filter), None) => {
                let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(engine);
                matches!(
                    uqa_sql::expr::eval(filter, &ctx).map(|v| uqa_sql::expr::truthy(&v)),
                    Ok(true)
                )
            }
            (Some(filter), Some(rows)) => {
                let mut matched = false;
                for using_row in rows {
                    let mut joined = ResultRow::new();
                    for (k, v) in &doc {
                        joined.insert(k.clone(), v.clone());
                        joined.insert(format!("{}.{k}", stmt.table), v.clone());
                    }
                    for (k, v) in using_row {
                        joined.insert(k.clone(), v.clone());
                    }
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&joined), params).with_engine(engine);
                    if matches!(
                        uqa_sql::expr::eval(filter, &ctx).map(|v| uqa_sql::expr::truthy(&v)),
                        Ok(true)
                    ) {
                        matched = true;
                        break;
                    }
                }
                matched
            }
        };
        if keep {
            if !stmt.returning.is_empty() {
                returning_docs.push((doc_id, doc.clone()));
            }
            to_delete.push(doc_id);
        }
    }
    let root_deletes: BTreeSet<(String, DocId)> = to_delete
        .iter()
        .map(|doc_id| (stmt.table.clone(), *doc_id))
        .collect();
    let mut delete_stack = Vec::new();
    for doc_id in to_delete {
        delete_document_with_referential_actions(
            engine,
            &stmt.table,
            doc_id,
            params,
            &root_deletes,
            &mut delete_stack,
        )?;
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        let returning_rows = returning_docs
            .into_iter()
            .map(|(doc_id, doc)| {
                build_returning_row(engine, &stmt.table, doc_id, &doc, &stmt.returning, params)
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
    }
    Ok(SQLResult::from_affected(affected))
}

fn delete_document_with_referential_actions(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    delete_stack: &mut Vec<(String, DocId)>,
) -> Result<(), SQLError> {
    let key = (table.to_string(), doc_id);
    if delete_stack.contains(&key) {
        return Ok(());
    }
    let Some(target) = engine.get_document(table, doc_id) else {
        return Ok(());
    };
    delete_stack.push(key);
    apply_referenced_key_delete_actions(
        engine,
        table,
        &target,
        params,
        root_deletes,
        delete_stack,
    )?;
    delete_stack.pop();
    engine.delete_document(table, doc_id);
    Ok(())
}

fn apply_referenced_key_delete_actions(
    engine: &Engine,
    table: &str,
    target: &Document,
    params: &[SQLParam],
    root_deletes: &BTreeSet<(String, DocId)>,
    delete_stack: &mut Vec<(String, DocId)>,
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(engine, table) {
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| target.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        let referencing = referencing_rows(engine, &ref_table, &fk.local_columns, &key_values);
        for (child_id, child_doc) in referencing {
            if root_deletes.contains(&(ref_table.clone(), child_id)) {
                continue;
            }
            match fk.on_delete {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(SQLError::TypeMismatch(format!(
                        "FOREIGN KEY constraint violated: DELETE on `{table}` is referenced by `{ref_table}` ({} -> {})",
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
                ForeignKeyAction::Cascade => {
                    delete_document_with_referential_actions(
                        engine,
                        &ref_table,
                        child_id,
                        params,
                        root_deletes,
                        delete_stack,
                    )?;
                }
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault => {
                    let mut updated = child_doc.clone();
                    let columns = delete_set_columns(&fk);
                    apply_set_action_to_child(
                        engine,
                        &ref_table,
                        &child_doc,
                        &mut updated,
                        &columns,
                        fk.on_delete,
                        params,
                    )?;
                    rewrite_document_with_referential_actions(
                        engine, &ref_table, child_id, &child_doc, updated, params,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn delete_set_columns(fk: &ForeignKey) -> Vec<String> {
    if fk.on_delete_set_columns.is_empty() {
        fk.local_columns.clone()
    } else {
        fk.on_delete_set_columns.clone()
    }
}

fn find_insert_conflict(
    engine: &Engine,
    table: &str,
    on_conflict: &uqa_sql::ast::OnConflict,
    document: &Document,
) -> Option<DocId> {
    if !on_conflict.conflict_columns.is_empty() {
        let conflict_values: Vec<Value> = on_conflict
            .conflict_columns
            .iter()
            .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        return engine.find_conflict(table, &on_conflict.conflict_columns, &conflict_values);
    }

    for col in engine.unique_columns(table) {
        let value = document.get(&col).cloned().unwrap_or(Value::Null);
        if matches!(value, Value::Null) {
            continue;
        }
        if let Some(doc_id) = engine.find_conflict(
            table,
            std::slice::from_ref(&col),
            std::slice::from_ref(&value),
        ) {
            return Some(doc_id);
        }
    }
    None
}

fn build_returning_row(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    document: &Document,
    returning: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut row_doc = document.clone();
    row_doc.insert(DOC_ID_COLUMN.into(), Value::Int(doc_id as i64));
    build_projection_row(Some(engine), &row_doc, returning, params).map_err(|err| {
        SQLError::Internal(format!(
            "RETURNING projection failed for table `{table}` doc {doc_id}: {err}"
        ))
    })
}

fn dml_returning_result(
    engine: &Engine,
    table: &str,
    returning: &[Projection],
    rows: Vec<ResultRow>,
    affected_rows: u64,
) -> SQLResult {
    SQLResult {
        columns: expand_star_columns(
            projection_columns(returning),
            returning,
            engine,
            Some(table),
        ),
        rows,
        affected_rows,
    }
}

// -------------------------------------------------------------------------
// DDL
// -------------------------------------------------------------------------

fn run_create_table(engine: &Engine, c: CreateTable) -> Result<SQLResult, SQLError> {
    if engine.has_table(&c.name) {
        if c.if_not_exists {
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Unsupported(format!(
            "CREATE TABLE: relation `{}` already exists",
            c.name
        )));
    }
    let mut vector_fields: Vec<(String, u32)> = Vec::new();
    for col in &c.columns {
        if let ColumnType::Vector(dim) = &col.ty {
            vector_fields.push((col.name.clone(), *dim));
        }
    }
    engine.create_default_table(c.name.clone(), Vec::new());
    for (field, dim) in vector_fields {
        engine.create_vector_field(&c.name, field, dim);
    }
    for col in &c.columns {
        engine.register_column(&c.name, col.clone());
    }
    engine.register_table_constraints(&c.name, c.checks.clone(), c.foreign_keys.clone());
    let _ = column_names(&c.columns);
    Ok(SQLResult::empty())
}

fn column_names(cols: &[SQLColumnDef]) -> Vec<String> {
    cols.iter().map(|c| c.name.clone()).collect()
}

fn run_create_index(engine: &Engine, c: CreateIndex) -> Result<SQLResult, SQLError> {
    // CREATE INDEX is metadata-bearing now: `gin` registers the column
    // as an FTS field with the analyzer specified in `WITH (analyzer = ...)`,
    // `ivf` rebuilds the vector field with an IVF backend, `hnsw` is a
    // compatibility alias for the same backend, and others are informational.
    let am = c.access_method.to_ascii_lowercase();
    match am.as_str() {
        "gin" => {
            for col in &c.columns {
                let analyzer = c
                    .options
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                if let Err(e) = engine.add_fts_field_with_analyzer(&c.table, col.clone(), analyzer)
                {
                    return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                }
            }
        }
        "" => {}
        "ivf" | "hnsw" => {
            let params = parse_ivf_index_params(&c.options)?;
            for col in &c.columns {
                match engine.column_type(&c.table, col) {
                    Some(ColumnType::Vector(dim)) => {
                        if !engine.rebuild_ivf_vector_field(&c.table, col.clone(), dim, params) {
                            return Err(SQLError::Unsupported(format!(
                                "CREATE INDEX USING ivf: relation `{}` does not exist",
                                c.table
                            )));
                        }
                    }
                    Some(other) => {
                        return Err(SQLError::Unsupported(format!(
                            "CREATE INDEX USING ivf requires VECTOR column `{col}`, got {other:?}"
                        )));
                    }
                    None => {
                        return Err(SQLError::Unsupported(format!(
                            "CREATE INDEX USING ivf: column `{}`.`{col}` does not exist",
                            c.table
                        )));
                    }
                }
            }
        }
        _ => {}
    }
    // Persist the CREATE INDEX statement itself so reopen sees the
    // same set of registered indexes. The engine layer parses
    // `parameters_json` back into `(key, value)` pairs and re-runs
    // any access-method-specific side effects (e.g. add_fts_field
    // for `gin`) on restore.
    if let Some(name) = c.name.as_ref() {
        let catalog_index_type = match am.as_str() {
            "" => "btree",
            "hnsw" => "ivf",
            other => other,
        };
        engine.register_catalog_index(name, catalog_index_type, &c.table, &c.columns, &c.options);
    }
    Ok(SQLResult::empty())
}

fn parse_ivf_index_params(options: &[(String, String)]) -> Result<IVFIndexParams, SQLError> {
    let mut params = IVFIndexParams::default();
    for (key, value) in options {
        if key.eq_ignore_ascii_case("lists") || key.eq_ignore_ascii_case("nlist") {
            params.nlist = parse_positive_usize_option(key, value)?;
        } else if key.eq_ignore_ascii_case("probes") || key.eq_ignore_ascii_case("nprobe") {
            params.nprobe = parse_positive_usize_option(key, value)?;
        } else if key.eq_ignore_ascii_case("train_threshold")
            || key.eq_ignore_ascii_case("train-threshold")
            || key.eq_ignore_ascii_case("min_train")
        {
            params.train_threshold = parse_positive_usize_option(key, value)?;
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING ivf option `{key}` is not supported"
            )));
        }
    }
    Ok(params)
}

fn parse_positive_usize_option(key: &str, value: &str) -> Result<usize, SQLError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        SQLError::TypeMismatch(format!(
            "CREATE INDEX USING ivf option `{key}` must be a positive integer"
        ))
    })?;
    if parsed == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "CREATE INDEX USING ivf option `{key}` must be a positive integer"
        )));
    }
    Ok(parsed)
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

fn run_insert(
    engine: &Engine,
    stmt: InsertStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_insert_inner(engine, stmt, params))
}

#[allow(clippy::too_many_lines)]
fn run_insert_inner(
    engine: &Engine,
    stmt: InsertStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // INSERT ... SELECT: materialise the inner SELECT first, then
    // route each row through the standard add_document path under
    // the named columns.
    if let Some(source) = stmt.select_source.clone() {
        let result = run_select(engine, *source, params)?;
        let columns: Vec<String> = if stmt.columns.is_empty() {
            result.columns.clone()
        } else {
            stmt.columns.clone()
        };
        let auto_id_col = engine.auto_increment_column(&stmt.table);
        let mut affected = 0u64;
        let mut returning_rows = Vec::new();
        let cancel = engine.cancellation_token();
        for source_row in result.rows {
            cancel.check()?;
            let mut document = Document::new();
            for col in &columns {
                if let Some(v) = source_row.get(col) {
                    document.insert(
                        col.clone(),
                        coerce_to_column_type(engine, &stmt.table, col, v.clone())?,
                    );
                }
            }
            let doc_id = match auto_id_col.as_deref().and_then(|c| document.get(c)) {
                Some(Value::Int(n)) if *n >= 0 => *n as u64,
                _ => engine.allocate_next_id(&stmt.table)?,
            };
            if let Some(c) = auto_id_col.as_deref() {
                document.insert(c.into(), Value::Int(doc_id as i64));
            }
            engine.advance_next_id(&stmt.table, doc_id);
            let document =
                insert_document_with_constraints(engine, &stmt.table, doc_id, document, params)?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &stmt.table,
                    doc_id,
                    &document,
                    &stmt.returning,
                    params,
                )?);
            }
            affected += 1;
        }
        if !stmt.returning.is_empty() {
            return Ok(dml_returning_result(
                engine,
                &stmt.table,
                &stmt.returning,
                returning_rows,
                affected,
            ));
        }
        return Ok(SQLResult::from_affected(affected));
    }

    let auto_id_col = engine.auto_increment_column(&stmt.table);
    // Resolve the table's primary-key column name. Auto-increment
    // (SERIAL / BIGSERIAL) wins; otherwise the first PRIMARY KEY
    // column wins; otherwise we fall back to the conventional "id"
    // slot so legacy tests keep passing.
    let id_column = auto_id_col.clone().or_else(|| {
        engine
            .describe_table(&stmt.table)
            .and_then(|cols| cols.into_iter().find(|c| c.primary_key))
            .map(|c| c.name)
    });
    let id_column = id_column.unwrap_or_else(|| "id".into());

    let columns: Vec<String> = if stmt.columns.is_empty() {
        // INSERT without explicit column list: project the table schema.
        let cols = engine.table_columns(&stmt.table);
        if cols.is_empty() {
            return Err(SQLError::Unsupported(
                "INSERT without column list against a table with no schema".into(),
            ));
        }
        cols
    } else {
        stmt.columns.clone()
    };

    let id_index = columns.iter().position(|c| c == &id_column);
    // No explicit id and no auto-increment column: allocate a synthetic
    // u64 doc_id at insert time. Mirrors the canonical UQA behavior, which
    // treats every table as having an implicit doc_id even when the
    // schema declares no primary key.

    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let cancel = engine.cancellation_token();
    for row in &stmt.rows {
        cancel.check()?;
        if row.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "row width {} != column count {}",
                row.len(),
                columns.len()
            )));
        }
        let mut document = Document::new();
        let mut doc_id: Option<u64> = None;
        for (i, col) in columns.iter().enumerate() {
            let mut v = eval(&row[i], &ctx)?;
            v = coerce_to_column_type(engine, &stmt.table, col, v)?;
            if Some(i) == id_index {
                // Auto-increment primary keys must be integers. A
                // non-auto-increment primary key (TEXT, UUID, ...) keeps
                // the user value in the document and the engine still
                // allocates a synthetic u64 doc_id for posting-list
                // bookkeeping. UNIQUE / PRIMARY KEY enforcement runs
                // through `engine.unique_columns` regardless.
                let is_auto = auto_id_col.as_deref() == Some(id_column.as_str());
                doc_id = match &v {
                    Value::Int(n) if *n >= 0 => Some(*n as u64),
                    Value::Null => None,
                    other if is_auto => {
                        return Err(SQLError::TypeMismatch(format!(
                            "auto-increment id must be an integer, got {other:?}"
                        )));
                    }
                    _ => None,
                };
            }
            document.insert(col.clone(), v);
        }

        // DEFAULT expression -- evaluate when the column was absent
        // from the INSERT column list. Mirrors the canonical UQA behavior's
        // _evaluate_default. The engine hook is in scope so DEFAULT
        // nextval('seq') resolves through the sequence store.
        for col in engine.table_columns(&stmt.table) {
            if document.contains_key(&col) {
                continue;
            }
            if let Some(default_expr) = engine.column_default_expr(&stmt.table, &col) {
                let v =
                    coerce_to_column_type(engine, &stmt.table, &col, eval(&default_expr, &ctx)?)?;
                document.insert(col.clone(), v);
            }
        }

        // NOT NULL validation -- after defaults are applied, every
        // declared NOT NULL column must have a non-null value.
        // Auto-increment columns are exempt because the engine fills
        // them in below.
        for col_def in engine.describe_table(&stmt.table).unwrap_or_default() {
            if !col_def.not_null || col_def.auto_increment {
                continue;
            }
            match document.get(&col_def.name) {
                Some(Value::Null) | None => {
                    return Err(SQLError::TypeMismatch(format!(
                        "NOT NULL constraint violated: column `{}` in table `{}`",
                        col_def.name, stmt.table
                    )));
                }
                _ => {}
            }
        }

        // CHECK constraints -- evaluate every column-level + table-
        // level CHECK against the row and reject when any returns a
        // non-truthy value.
        for (cname, expr) in engine.check_constraints(&stmt.table) {
            let row_ctx = EvalContext::new(Some(&document), params).with_engine(engine);
            let result = eval(&expr, &row_ctx)?;
            if !uqa_sql::expr::truthy(&result) {
                let label = cname.unwrap_or_else(|| "<unnamed>".into());
                return Err(SQLError::TypeMismatch(format!(
                    "CHECK constraint `{label}` violated in table `{}`",
                    stmt.table
                )));
            }
        }

        // FOREIGN KEY constraints -- MATCH SIMPLE skips any tuple
        // containing NULL, while MATCH FULL requires either every
        // local key column to be NULL or none of them to be NULL.
        for fk in engine.foreign_keys(&stmt.table) {
            let Some(local_values) = foreign_key_lookup_values(&fk, &document)? else {
                continue;
            };
            if engine
                .find_conflict(&fk.ref_table, &fk.ref_columns, &local_values)
                .is_none()
            {
                let cols = fk.local_columns.join(", ");
                return Err(SQLError::TypeMismatch(format!(
                    "FOREIGN KEY constraint violated: ({cols}) -> {}({}) has no matching row",
                    fk.ref_table,
                    fk.ref_columns.join(", ")
                )));
            }
        }

        // UNIQUE constraint validation -- before any conflict
        // resolution, every UNIQUE / PRIMARY KEY column whose value
        // is non-null must not already exist in another row. The
        // ON CONFLICT branch below intentionally skips this check
        // because that path explicitly chooses a merge action.
        if stmt.on_conflict.is_none() {
            for col in engine.unique_columns(&stmt.table) {
                let Some(value) = document.get(&col).cloned() else {
                    continue;
                };
                if matches!(value, Value::Null) {
                    continue;
                }
                if engine
                    .find_conflict(
                        &stmt.table,
                        std::slice::from_ref(&col),
                        std::slice::from_ref(&value),
                    )
                    .is_some()
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "UNIQUE constraint violated: duplicate value for column `{col}` in table `{}`",
                        stmt.table
                    )));
                }
            }
        }

        // ON CONFLICT lookup -- check whether a row with matching
        // conflict-target columns already exists. The conflict
        // columns may include the primary key, so we collect their
        // current values from the row being inserted.
        if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            if let Some(existing_id) =
                find_insert_conflict(engine, &stmt.table, on_conflict, &document)
            {
                match &on_conflict.action {
                    uqa_sql::ast::OnConflictAction::Nothing => {
                        continue;
                    }
                    uqa_sql::ast::OnConflictAction::Update {
                        assignments,
                        r#where,
                    } => {
                        let existing_doc = engine
                            .get_document(&stmt.table, existing_id)
                            .unwrap_or_default();
                        let mut conflict_ctx_doc = existing_doc.clone();
                        for (col, value) in &document {
                            conflict_ctx_doc.insert(format!("excluded.{col}"), value.clone());
                        }
                        let row_ctx =
                            EvalContext::new(Some(&conflict_ctx_doc), params).with_engine(engine);
                        if let Some(pred) = r#where {
                            let keep = eval(pred, &row_ctx)?;
                            if !uqa_sql::expr::truthy(&keep) {
                                continue;
                            }
                        }
                        let mut updated_doc = existing_doc.clone();
                        for (col, expr) in assignments {
                            let v = coerce_to_column_type(
                                engine,
                                &stmt.table,
                                col,
                                eval(expr, &row_ctx)?,
                            )?;
                            updated_doc.insert(col.clone(), v.clone());
                        }
                        rewrite_document_with_referential_actions(
                            engine,
                            &stmt.table,
                            existing_id,
                            &existing_doc,
                            updated_doc.clone(),
                            params,
                        )?;
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_returning_row(
                                engine,
                                &stmt.table,
                                existing_id,
                                &updated_doc,
                                &stmt.returning,
                                params,
                            )?);
                        }
                        affected += 1;
                        continue;
                    }
                }
            }
        }

        let doc_id = if let Some(id) = doc_id {
            id
        } else {
            let id = engine.allocate_next_id(&stmt.table)?;
            // Only stamp the allocated id back onto the document when
            // the primary-key column is auto-increment. For non-auto
            // primary keys (TEXT, UUID, ...) the user-supplied value
            // already lives in `document[id_column]` and must be
            // preserved -- the synthetic u64 stays internal.
            if auto_id_col.as_deref() == Some(id_column.as_str()) {
                document.insert(id_column.clone(), Value::Int(id as i64));
            }
            id
        };
        engine.advance_next_id(&stmt.table, doc_id);
        let document =
            insert_document_with_constraints(engine, &stmt.table, doc_id, document, params)?;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_row(
                engine,
                &stmt.table,
                doc_id,
                &document,
                &stmt.returning,
                params,
            )?);
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return Ok(dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        ));
    }
    Ok(SQLResult::from_affected(affected))
}

fn insert_document_with_constraints(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    mut document: Document,
    params: &[SQLParam],
) -> Result<Document, SQLError> {
    apply_missing_column_defaults(engine, table, &mut document, params)?;
    validate_document_constraints(engine, table, doc_id, &document, params)?;
    engine.add_document_with_vectors(table, doc_id, document.clone(), document_vectors(&document));
    Ok(document)
}

fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let ctx = EvalContext::new(None, params).with_engine(engine);
    for col in engine.table_columns(table) {
        if document.contains_key(&col) {
            continue;
        }
        if let Some(default_expr) = engine.column_default_expr(table, &col) {
            let value = coerce_to_column_type(engine, table, &col, eval(&default_expr, &ctx)?)?;
            document.insert(col, value);
        }
    }
    Ok(())
}

fn document_vectors(document: &Document) -> BTreeMap<uqa_core::FieldName, Vec<f32>> {
    let mut vectors = BTreeMap::new();
    for (field, value) in document {
        if let Ok(vector) = value_to_vector(value) {
            vectors.insert(field.clone(), vector);
        }
    }
    vectors
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

/// Render the inner statement as an EXPLAIN-style plan result. Mirrors
/// the canonical UQA implementation's `_explain_plan`: returns a single-column `plan` table with
/// one row per line.
fn run_explain(
    engine: &Engine,
    body: Statement,
    _params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let plan_text = match &body {
        Statement::Select(stmt) => format_select_plan(stmt),
        other => format!("{other:?}"),
    };
    let mut rows: Vec<ResultRow> = Vec::new();
    for line in plan_text.split('\n') {
        let mut r = ResultRow::new();
        r.insert("plan".to_string(), Value::Str(line.to_string()));
        rows.push(r);
    }
    let _ = engine; // keep the parameter live for future cost extension
    Ok(SQLResult {
        columns: vec!["plan".to_string()],
        rows,
        affected_rows: 0,
    })
}

fn format_select_plan(stmt: &SelectStmt) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "Select");
    if !stmt.projections.is_empty() {
        let _ = writeln!(s, "  projections={}", stmt.projections.len());
    }
    if let Some(from) = &stmt.from {
        let _ = writeln!(s, "  from={from:?}");
    }
    if stmt.r#where.is_some() {
        let _ = writeln!(s, "  where=<expr>");
    }
    if !stmt.group_by.is_empty() {
        let _ = writeln!(s, "  group_by={}", stmt.group_by.len());
    }
    if !stmt.grouping_sets.is_empty() {
        let _ = writeln!(s, "  grouping_sets={}", stmt.grouping_sets.len());
    }
    if !stmt.order_by.is_empty() {
        let _ = writeln!(s, "  order_by={}", stmt.order_by.len());
    }
    if let Some(expr) = stmt.limit.as_ref() {
        let _ = writeln!(s, "  limit={}", explain_int_expr(expr));
    }
    if let Some(expr) = stmt.offset.as_ref() {
        let _ = writeln!(s, "  offset={}", explain_int_expr(expr));
    }
    if stmt.distinct {
        let _ = writeln!(s, "  distinct=true");
    }
    s.trim_end().to_string()
}

pub(crate) fn run_select(
    engine: &Engine,
    stmt: SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if !stmt.with.is_empty() || stmt.set_op.is_some() {
        let mut ctes: BTreeMap<String, Vec<ResultRow>> = BTreeMap::new();
        return execute_select(engine, &stmt, params, &mut ctes);
    }

    let Some(from) = stmt.from.as_ref() else {
        // SELECT without FROM -- evaluate the projection list against
        // an empty single-row context. Mirrors the canonical UQA implementation's standalone
        // SELECT 1 / SELECT (SELECT ...).
        return run_select_without_from(engine, &stmt, params);
    };

    // Single-table FROM with no alias and no window function keeps the
    // search-aware fast path. JOIN shapes and window queries drop into
    // the multi-table executor that builds row tuples up-front and
    // filters them via the expression evaluator.
    if let FromClause::Table { name, alias } = from {
        if alias.is_none() && engine.foreign_table(name).is_some() {
            return run_single_foreign_select(engine, name, &stmt, params);
        }
        // Schema-qualified names (information_schema.tables /
        // pg_catalog.pg_*) and CTE references skip the search-aware
        // fast path because they don't correspond to a registered
        // engine table.
        let is_virtual = name.contains('.')
            || (engine.table(name).is_none() && engine.foreign_table(name).is_none());
        if alias.is_none() && !has_window(&stmt.projections) && !is_virtual {
            return run_single_table_select(engine, name, &stmt, params);
        }
    }

    run_joined_select(engine, from, &stmt, params)
}

fn run_select_without_from(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let row = ResultRow::new();
    let projected = build_projection_row(Some(engine), &row, &stmt.projections, params)?;
    let columns: Vec<String> = stmt
        .projections
        .iter()
        .enumerate()
        .map(|(i, p)| {
            p.alias
                .clone()
                .unwrap_or_else(|| format!("column{}", i + 1))
        })
        .collect();
    Ok(SQLResult {
        columns,
        rows: vec![projected],
        affected_rows: 0,
    })
}

/// Execute a SELECT that may carry CTEs and/or set ops, returning the
/// final result. CTEs are materialized into the `ctes` map first so the
/// FROM clause can resolve references to them.
fn execute_select(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    materialize_ctes(engine, &stmt.with, params, ctes)?;

    // The parent `SelectStmt` carries the LHS branch's own clauses
    // (projections / from / where / group-by / ORDER BY / LIMIT /
    // OFFSET). The set-op-level combined clauses live on
    // `set_op.combined_*`. The LHS branch executes with its own
    // clauses applied; the merged result then takes the combined
    // clauses below.
    let mut lhs = run_query_block(engine, stmt, params, ctes)?;
    if stmt.distinct {
        // SELECT DISTINCT: collapse duplicate output rows.
        // Stable so the relative order of survivors matches PG.
        let mut seen: Vec<ResultRow> = Vec::with_capacity(lhs.rows.len());
        for row in lhs.rows.drain(..) {
            if !seen.iter().any(|r| r == &row) {
                seen.push(row);
            }
        }
        lhs.rows = seen;
    }

    let Some(set_op) = stmt.set_op.as_ref() else {
        return Ok(lhs);
    };
    let rhs = execute_select(engine, &set_op.right, params, ctes)?;
    let mut combined = match (set_op.kind, set_op.all) {
        (SetOpKind::Union, true) => {
            let mut rows = lhs.rows;
            rows.extend(rhs.rows);
            SQLResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Union, false) => {
            let mut rows = lhs.rows;
            rows.extend(rhs.rows);
            rows.dedup();
            SQLResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Intersect, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|r| rhs.rows.iter().any(|s| s == r))
                .collect();
            if !set_op.all {
                rows.dedup();
            }
            SQLResult::from_rows(lhs.columns, rows)
        }
        (SetOpKind::Except, _) => {
            let mut rows: Vec<ResultRow> = lhs
                .rows
                .into_iter()
                .filter(|r| !rhs.rows.iter().any(|s| s == r))
                .collect();
            if !set_op.all {
                rows.dedup();
            }
            SQLResult::from_rows(lhs.columns, rows)
        }
    };

    // Apply the union-level ORDER BY / LIMIT / OFFSET to the merged
    // set-op result. The parent `stmt`'s own clauses already fired
    // on the LHS branch above; here we use the combined clauses the
    // compiler stashed on the SetOp.
    if !set_op.combined_order_by.is_empty()
        || set_op.combined_limit.is_some()
        || set_op.combined_offset.is_some()
    {
        let synthetic = SelectStmt {
            projections: Vec::new(),
            from: None,
            r#where: None,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            having: None,
            order_by: set_op.combined_order_by.clone(),
            limit: set_op.combined_limit.clone(),
            offset: set_op.combined_offset.clone(),
            with: Vec::new(),
            set_op: None,
            distinct: false,
        };
        let columns = combined.columns.clone();
        combined.rows = apply_row_order_limit(combined.rows, &synthetic, engine, params)?;
        combined.columns = columns;
    }
    Ok(combined)
}

struct ScopedEngineHook<'a> {
    engine: &'a Engine,
    ctes: &'a BTreeMap<String, Vec<ResultRow>>,
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.nextval(name)
    }

    fn currval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.currval(name)
    }

    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String> {
        self.engine.setval(name, value)
    }

    fn run_subquery(
        &self,
        stmt: &uqa_sql::ast::SelectStmt,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> std::result::Result<(Vec<String>, Vec<ResultRow>), String> {
        run_correlated_subquery(self.engine, stmt, outer_row, params, self.ctes)
            .map(|result| (result.columns, result.rows))
            .map_err(|e| format!("subquery failed: {e}"))
    }
}

pub(crate) fn run_correlated_subquery(
    engine: &Engine,
    stmt: &SelectStmt,
    outer_row: Option<&ResultRow>,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    if let Some(row) = outer_row {
        execute_lateral_subquery(engine, stmt, row, params, ctes)
    } else {
        let mut scoped_ctes = ctes.clone();
        execute_select(engine, stmt, params, &mut scoped_ctes)
    }
}

fn run_query_block(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from(engine, stmt, params);
    };

    let joined = build_join_rows_with_ctes(engine, from, params, ctes)?;
    let scoped_hook = ScopedEngineHook { engine, ctes };

    // Aggregates and window functions still go through their dedicated
    // routines because they need access to the SQL function registry
    // (e.g. text_match calls in projection lists). Pure projection
    // SELECTs flow through a Volcano sub-pipeline:
    //   TableScan -> [Filter] -> Project -> [Sort] -> [Limit]
    // built on the operators in `uqa-execution` so the planner-driven
    // execution layer is exercised on every projection-only SELECT.
    let filtered = if let Some(filter) = stmt.r#where.as_ref() {
        let mut out: Vec<ResultRow> = Vec::with_capacity(joined.len());
        for row in joined {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(&scoped_hook);
            if uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v)) {
                out.push(row);
            }
        }
        out
    } else {
        joined
    };

    if has_aggregate(&stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(engine, &stmt.projections, filtered, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    // Pure projection: use the Volcano Project + Sort + Limit chain.
    let projected = volcano_project_sort_limit(engine, &filtered, stmt, params)?;
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
    Ok(SQLResult::from_rows(columns, projected))
}

fn expand_from_star_columns(
    columns: Vec<String>,
    projections: &[Projection],
    from: &FromClause,
) -> Vec<String> {
    let has_star = projections.iter().any(|p| matches!(p.expr, Expr::Star));
    if !has_star {
        return columns;
    }
    let source_cols = from_clause_output_columns(from);
    if source_cols.is_empty() {
        return columns;
    }
    let mut out = Vec::with_capacity(columns.len() + source_cols.len());
    for column in columns {
        if column == "*" {
            out.extend(source_cols.iter().cloned());
        } else {
            out.push(column);
        }
    }
    out
}

fn from_clause_output_columns(from: &FromClause) -> Vec<String> {
    match from {
        FromClause::Function {
            name,
            alias,
            column_aliases,
            ..
        } => {
            let cols = if column_aliases.is_empty() {
                vec![name.clone()]
            } else {
                column_aliases.clone()
            };
            qualify_output_columns(alias.as_deref(), cols)
        }
        FromClause::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let cols = if column_aliases.is_empty() {
                let width = rows.first().map_or(0, Vec::len);
                (0..width).map(|idx| format!("column{}", idx + 1)).collect()
            } else {
                column_aliases.clone()
            };
            qualify_output_columns(alias.as_deref(), cols)
        }
        FromClause::Subquery {
            alias,
            column_aliases,
            ..
        } => qualify_output_columns(alias.as_deref(), column_aliases.clone()),
        FromClause::Join { left, right, .. } => {
            let mut cols = from_clause_output_columns(left);
            cols.extend(from_clause_output_columns(right));
            cols
        }
        FromClause::Table { .. } => Vec::new(),
    }
}

fn qualify_output_columns(alias: Option<&str>, columns: Vec<String>) -> Vec<String> {
    match alias {
        Some(a) => columns
            .into_iter()
            .map(|column| format!("{a}.{column}"))
            .collect(),
        None => columns,
    }
}

fn volcano_project_sort_limit(
    engine: &Engine,
    src_rows: &[ResultRow],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // Some projection callsites (e.g. `text_match` in the SELECT
    // list) need the engine-side function registry, which the
    // execution-layer Project operator does not understand. Detect
    // those and fall back to the row-by-row engine projector so the
    // contract stays the same for SQL-function-bearing projections.
    let has_engine_funcs = stmt.projections.iter().any(|p| {
        let mut found = false;
        walk_expr(&p.expr, &mut |e| {
            if let Expr::Func { name, .. } = e {
                if uqa_sql::registry::is_registered(&name.to_ascii_lowercase()) {
                    found = true;
                }
            }
        });
        found
    });
    let has_star = stmt
        .projections
        .iter()
        .any(|p| matches!(p.expr, Expr::Star));
    // Pre-projection ordering / limiting. PG semantics: ORDER BY can
    // reference columns that the SELECT list drops, so the sort and
    // the limit must happen against the source rows -- the Project
    // step is the *last* node in the pipeline. Output column aliases
    // from the projection list are not addressable here, but the
    // common cases (`ORDER BY <source-column>`, `ORDER BY <const>`)
    // both work because the source row carries every column the
    // FROM relation produced.
    let resolved_offset = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?;
    let resolved_limit = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")?;

    if has_engine_funcs || has_star {
        let staged = stage_source_rows(
            src_rows,
            stmt,
            engine,
            params,
            resolved_offset,
            resolved_limit,
        )?;
        let rows: Vec<ResultRow> = staged
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        return Ok(rows);
    }

    use uqa_execution::physical::{run_to_rows, ExecError, PhysicalOperator};
    use uqa_execution::relational::{Limit, Project, Sort, SortKey};
    use uqa_execution::scan::TableScan;

    let columns: Vec<String> = src_rows
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();
    let mut op: Box<dyn PhysicalOperator> =
        Box::new(TableScan::from_rows(columns, src_rows.to_vec()));

    if !stmt.order_by.is_empty() {
        let keys: Vec<SortKey> = stmt
            .order_by
            .iter()
            .map(|o| SortKey {
                expr: o.expr.clone(),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        op = Box::new(Sort::new(op, keys, params.to_vec()));
    }
    if resolved_offset.is_some() || resolved_limit.is_some() {
        op = Box::new(Limit::new(op, resolved_offset.unwrap_or(0), resolved_limit));
    }

    let labels = projection_columns(&stmt.projections);
    let projections: Vec<(String, Expr)> = stmt
        .projections
        .iter()
        .enumerate()
        .map(|(i, p)| (labels[i].clone(), p.expr.clone()))
        .collect();
    op = Box::new(Project::new(op, projections, params.to_vec()));

    let (_cols, rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        ExecError::SQL(err) => err,
        ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    Ok(rows)
}

/// Sort + offset + limit a row set against the source-column namespace
/// (pre-projection). Used by the engine-funcs / star projection path
/// where projection happens row-by-row through the engine after the
/// pipeline has already trimmed the input set.
fn stage_source_rows(
    src_rows: &[ResultRow],
    stmt: &SelectStmt,
    _engine: &Engine,
    params: &[SQLParam],
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_execution::physical::{run_to_rows, PhysicalOperator};
    use uqa_execution::relational::{Limit, Sort, SortKey};
    use uqa_execution::scan::TableScan;

    if stmt.order_by.is_empty() && offset.is_none() && limit.is_none() {
        return Ok(src_rows.to_vec());
    }
    let columns: Vec<String> = src_rows
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();
    let mut op: Box<dyn PhysicalOperator> =
        Box::new(TableScan::from_rows(columns, src_rows.to_vec()));
    if !stmt.order_by.is_empty() {
        let keys: Vec<SortKey> = stmt
            .order_by
            .iter()
            .map(|o| SortKey {
                expr: o.expr.clone(),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        op = Box::new(Sort::new(op, keys, params.to_vec()));
    }
    if offset.is_some() || limit.is_some() {
        op = Box::new(Limit::new(op, offset.unwrap_or(0), limit));
    }
    let (_cols, rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        uqa_execution::physical::ExecError::SQL(err) => err,
        uqa_execution::physical::ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    Ok(rows)
}

fn walk_expr<F: FnMut(&Expr)>(expr: &Expr, f: &mut F) {
    f(expr);
    match expr {
        Expr::And(parts) | Expr::Or(parts) => {
            for p in parts {
                walk_expr(p, f);
            }
        }
        Expr::Not(inner) => walk_expr(inner, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::IsNull { expr, .. } => walk_expr(expr, f),
        Expr::Between { expr, low, high } => {
            walk_expr(expr, f);
            walk_expr(low, f);
            walk_expr(high, f);
        }
        Expr::InList { expr, list, .. } => {
            walk_expr(expr, f);
            for p in list {
                walk_expr(p, f);
            }
        }
        Expr::Func { args, .. } | Expr::WindowCall { args, .. } => {
            for p in args {
                walk_expr(p, f);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(b) = base {
                walk_expr(b, f);
            }
            for (c, r) in when {
                walk_expr(c, f);
                walk_expr(r, f);
            }
            if let Some(e) = else_branch {
                walk_expr(e, f);
            }
        }
        Expr::Cast { expr, .. } => walk_expr(expr, f),
        Expr::Array(items) => {
            for p in items {
                walk_expr(p, f);
            }
        }
        _ => {}
    }
}

fn materialize_ctes(
    engine: &Engine,
    list: &[CTE],
    params: &[SQLParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<(), SQLError> {
    for cte in list {
        let rows = if cte.recursive {
            materialize_recursive_cte(engine, cte, params, ctes)?
        } else {
            let result = execute_select(engine, &cte.query, params, ctes)?;
            apply_cte_column_aliases(result.rows, &result.columns, &cte.columns)
        };
        ctes.insert(cte.name.clone(), rows);
    }
    Ok(())
}

/// Iterate the recursive CTE: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the CTE bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Caps at 1024 iterations to keep buggy queries from running away.
fn materialize_recursive_cte(
    engine: &Engine,
    cte: &CTE,
    params: &[SQLParam],
    ctes: &mut BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SQLError> {
    let set_op = cte
        .query
        .set_op
        .as_ref()
        .ok_or_else(|| SQLError::Unsupported("recursive CTE requires UNION ALL".into()))?;
    if set_op.kind != SetOpKind::Union {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION".into(),
        ));
    }

    // Anchor: the LHS - the same SelectStmt with set_op stripped.
    let mut anchor_stmt = cte.query.as_ref().clone();
    anchor_stmt.set_op = None;
    anchor_stmt.with.clear();
    let source_anchor_columns = projection_columns(&anchor_stmt.projections);
    let anchor_columns = if cte.columns.is_empty() {
        source_anchor_columns.clone()
    } else {
        cte.columns.clone()
    };
    let anchor_rows = run_query_block(engine, &anchor_stmt, params, ctes)?.rows;
    let anchor_rows =
        apply_cte_column_aliases(anchor_rows, &source_anchor_columns, &anchor_columns);

    let mut all_rows = anchor_rows.clone();
    let mut working = anchor_rows;

    let mut step_stmt = set_op.right.clone();
    step_stmt.with.clear();
    let step_columns = projection_columns(&step_stmt.projections);

    const MAX_ITER: usize = 1024;
    for _ in 0..MAX_ITER {
        if working.is_empty() {
            break;
        }
        // Bind the CTE name to the working set under the anchor's
        // column shape so the recursive step's FROM ... <cte> ... sees
        // the same keys it saw on the prior pass.
        let working_normalized: Vec<ResultRow> = working
            .iter()
            .map(|row| rename_columns(row, &anchor_columns, &anchor_columns))
            .collect();
        ctes.insert(cte.name.clone(), working_normalized);
        let new_rows = run_query_block(engine, &step_stmt, params, ctes)?.rows;
        ctes.remove(&cte.name);

        if new_rows.is_empty() {
            break;
        }
        // Rename the step's positional projection labels to the
        // anchor's so subsequent iterations and the outer SELECT see a
        // consistent shape (anchor names win, mirroring `PostgreSQL`).
        let renamed: Vec<ResultRow> = new_rows
            .into_iter()
            .map(|row| rename_columns(&row, &step_columns, &anchor_columns))
            .collect();
        let next = if set_op.all {
            renamed
        } else {
            renamed
                .into_iter()
                .filter(|row| !all_rows.iter().any(|seen| seen == row))
                .collect()
        };
        if next.is_empty() {
            break;
        }
        all_rows.extend(next.clone());
        working = next;
    }
    Ok(all_rows)
}

fn apply_cte_column_aliases(
    rows: Vec<ResultRow>,
    source_columns: &[String],
    aliases: &[String],
) -> Vec<ResultRow> {
    if aliases.is_empty() {
        return rows;
    }
    rows.into_iter()
        .map(|row| rename_columns(&row, source_columns, aliases))
        .collect()
}

fn rename_columns(row: &ResultRow, src: &[String], dst: &[String]) -> ResultRow {
    let mut out = ResultRow::new();
    for (i, key) in src.iter().enumerate() {
        if let Some(value) = row.get(key) {
            let target = dst.get(i).cloned().unwrap_or_else(|| key.clone());
            out.insert(target, value.clone());
        }
    }
    for (k, v) in row {
        out.entry(k.clone()).or_insert_with(|| v.clone());
    }
    out
}

fn run_single_table_select(
    engine: &Engine,
    table: &str,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    // Try the operator-tree pipeline first: lower the WHERE clause to
    // an `OperatorTree`, run `QueryOptimizer` (10 algebraic / graph-
    // aware / fusion-reordering passes - compatibility), then execute
    // through `PlanExecutor` against an `EngineDriver`. The bridge
    // returns `None` for shapes the operator IR can't represent
    // (arithmetic across columns, sub-queries, window calls, ...) and
    // we fall back to the legacy direct dispatch in that case.
    let scored = if let Some(rows) =
        crate::operator_tree_bridge::run_optimised(engine, table, stmt.r#where.as_ref(), params)?
    {
        rows
    } else {
        match stmt.r#where.as_ref() {
            None => engine
                .table_doc_ids(table)
                .into_iter()
                .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
                .collect::<Vec<_>>(),
            Some(Expr::Func { name, args, .. }) if uqa_sql::registry::is_registered(name) => {
                execute_function(engine, table, name, args, params)?
            }
            Some(filter_expr) => filter_table_rows(engine, table, filter_expr, params)?,
        }
    };

    if has_aggregate(&stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = build_aggregate_rows(engine, table, &scored, stmt, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if let Some(facet_fields) = facet_projection_fields(&stmt.projections)? {
        return Ok(build_facet_rows(engine, table, &scored, &facet_fields));
    }

    if order_by_references_field(stmt) {
        // ORDER BY references something other than `_score`; defer
        // ordering / skip / limit to the row-level evaluator that can
        // resolve arbitrary expressions against the projected
        // document.
        let columns = projection_columns(&stmt.projections);
        let mut all_rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
        // Bring the underlying document fields into each row so the
        // row evaluator can read columns like `qty` even when the
        // SELECT projection drops them.
        for (entry, row) in scored.iter().zip(all_rows.iter_mut()) {
            if let Some(doc) = engine.get_document(table, entry.doc_id) {
                for (k, v) in doc {
                    row.entry(k).or_insert(v);
                }
            }
        }
        let rows = apply_row_order_limit(all_rows, stmt, engine, params)?;
        // Strip the helper fields to keep the projection honest.
        let projected: Vec<_> = rows
            .into_iter()
            .map(|mut row| {
                row.retain(|k, _| columns.iter().any(|c| c == k));
                row
            })
            .collect();
        return Ok(SQLResult::from_rows(columns, projected));
    }

    let scored = apply_order_limit(scored, stmt, engine, params)?;
    let columns = expand_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        engine,
        Some(table),
    );
    let rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
    Ok(SQLResult::from_rows(columns, rows))
}

fn run_single_foreign_select(
    engine: &Engine,
    table: &str,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let predicates = fdw_predicates_from_where(stmt.r#where.as_ref(), params);
    let scanned = engine
        .scan_foreign_table(table, None, &predicates, None)
        .map_err(SQLError::Unsupported)?;

    let filtered = if let Some(filter) = stmt.r#where.as_ref() {
        let mut out = Vec::with_capacity(scanned.len());
        for row in scanned {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
            if uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v)) {
                out.push(row);
            }
        }
        out
    } else {
        scanned
    };

    if has_aggregate(&stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(engine, &stmt.projections, filtered, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let rows = volcano_project_sort_limit(engine, &filtered, stmt, params)?;
    let columns = expand_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        engine,
        Some(table),
    );
    Ok(SQLResult::from_rows(columns, rows))
}

fn fdw_predicates_from_where(
    expr: Option<&Expr>,
    params: &[SQLParam],
) -> Vec<uqa_fdw::FDWPredicate> {
    let Some(expr) = expr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_fdw_predicates(expr, params, &mut out);
    out
}

fn collect_fdw_predicates(expr: &Expr, params: &[SQLParam], out: &mut Vec<uqa_fdw::FDWPredicate>) {
    match expr {
        Expr::And(parts) => {
            for part in parts {
                collect_fdw_predicates(part, params, out);
            }
        }
        _ => {
            if let Some(predicate) = fdw_predicate(expr, params) {
                out.push(predicate);
            }
        }
    }
}

fn fdw_predicate(expr: &Expr, params: &[SQLParam]) -> Option<uqa_fdw::FDWPredicate> {
    match expr {
        Expr::Binary { op, lhs, rhs } => {
            if let Some(column) = fdw_column_name(lhs) {
                let value = fdw_const_value(rhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_binary_op(*op)?,
                    value,
                });
            }
            if let Some(column) = fdw_column_name(rhs) {
                let value = fdw_const_value(lhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_reversed_binary_op(*op)?,
                    value,
                });
            }
            None
        }
        Expr::InList {
            expr,
            list,
            negated,
        } if !negated => {
            let column = fdw_column_name(expr)?;
            let values = list
                .iter()
                .map(|item| fdw_const_value(item, params))
                .collect::<Option<Vec<_>>>()?;
            Some(uqa_fdw::FDWPredicate {
                column,
                operator: uqa_fdw::PredicateOp::In,
                value: Value::List(values),
            })
        }
        Expr::IsNull { expr, negated } => Some(uqa_fdw::FDWPredicate {
            column: fdw_column_name(expr)?,
            operator: if *negated {
                uqa_fdw::PredicateOp::NotEq
            } else {
                uqa_fdw::PredicateOp::Eq
            },
            value: Value::Null,
        }),
        Expr::Func { name, args, .. } => fdw_like_predicate(name, args, false, params),
        Expr::Not(inner) => match inner.as_ref() {
            Expr::Func { name, args, .. } => fdw_like_predicate(name, args, true, params),
            _ => None,
        },
        _ => None,
    }
}

fn fdw_like_predicate(
    name: &str,
    args: &[Expr],
    negated: bool,
    params: &[SQLParam],
) -> Option<uqa_fdw::FDWPredicate> {
    if args.len() != 2 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let operator = match (lower.as_str(), negated) {
        ("like", false) => uqa_fdw::PredicateOp::Like,
        ("like", true) => uqa_fdw::PredicateOp::NotLike,
        ("ilike", false) => uqa_fdw::PredicateOp::ILike,
        ("ilike", true) => uqa_fdw::PredicateOp::NotILike,
        _ => return None,
    };
    Some(uqa_fdw::FDWPredicate {
        column: fdw_column_name(&args[0])?,
        operator,
        value: fdw_const_value(&args[1], params)?,
    })
}

fn fdw_column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column(name) => Some(name.clone()),
        Expr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn fdw_const_value(expr: &Expr, params: &[SQLParam]) -> Option<Value> {
    let ctx = uqa_sql::expr::EvalContext::new(None, params);
    uqa_sql::expr::eval(expr, &ctx).ok()
}

fn fdw_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Lt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Gt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

fn fdw_reversed_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Gt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Lt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

fn facet_projection_fields(projections: &[Projection]) -> Result<Option<Vec<String>>, SQLError> {
    if projections.len() != 1 {
        return Ok(None);
    }
    let Expr::Func { name, args, .. } = &projections[0].expr else {
        return Ok(None);
    };
    if !name.eq_ignore_ascii_case("uqa_facets") {
        return Ok(None);
    }
    let mut fields = Vec::with_capacity(args.len());
    for arg in args {
        fields.push(expect_column_name(arg, "uqa_facets.field")?);
    }
    Ok(Some(fields))
}

fn build_facet_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    fields: &[String],
) -> SQLResult {
    let include_field = fields.len() > 1;
    let mut rows = Vec::new();
    for field in fields {
        let mut counts: BTreeMap<Value, i64> = BTreeMap::new();
        for entry in scored {
            let Some(doc) = engine.get_document(table, entry.doc_id) else {
                continue;
            };
            let Some(value) = doc.get(field) else {
                continue;
            };
            if matches!(value, Value::Null) {
                continue;
            }
            *counts.entry(value.clone()).or_insert(0) += 1;
        }
        for (value, count) in counts {
            let mut row = ResultRow::new();
            if include_field {
                row.insert("facet_field".into(), Value::Str(field.clone()));
            }
            row.insert("facet_value".into(), value);
            row.insert("facet_count".into(), Value::Int(count));
            rows.push(row);
        }
    }
    let columns = if include_field {
        vec![
            "facet_field".into(),
            "facet_value".into(),
            "facet_count".into(),
        ]
    } else {
        vec!["facet_value".into(), "facet_count".into()]
    };
    SQLResult {
        columns,
        rows,
        affected_rows: 0,
    }
}

/// When a projection list contains `Expr::Star`, replace the synthetic
/// `*` placeholder in the result column list with the source schema.
/// Empty result sets still report the correct column shape, matching
/// `PostgreSQL`'s behaviour of `SELECT * FROM empty_table`.
fn expand_star_columns(
    columns: Vec<String>,
    projections: &[Projection],
    engine: &Engine,
    table: Option<&str>,
) -> Vec<String> {
    let has_star = projections.iter().any(|p| matches!(p.expr, Expr::Star));
    if !has_star {
        return columns;
    }
    let schema_cols: Vec<String> = match table {
        Some(t) => {
            let cols = engine.table_columns(t);
            if cols.is_empty() {
                engine.foreign_table_columns(t)
            } else {
                cols
            }
        }
        None => Vec::new(),
    };
    if schema_cols.is_empty() {
        return columns;
    }
    let mut out: Vec<String> = Vec::with_capacity(columns.len() + schema_cols.len());
    for c in columns {
        if c == "*" {
            for sc in &schema_cols {
                if !out.iter().any(|x| x == sc) {
                    out.push(sc.clone());
                }
            }
        } else if !out.iter().any(|x| x == &c) {
            out.push(c);
        }
    }
    out
}

fn order_by_references_field(stmt: &SelectStmt) -> bool {
    stmt.order_by.iter().any(|o| match &o.expr {
        Expr::Column(name) => name != "_score",
        _ => true,
    })
}

/// Multi-table SELECT path. Each input table contributes a row set
/// keyed by `<alias>.<column>` (the alias falls back to the bare table
/// name when no `AS` is given). The same key shape feeds the WHERE
/// expression evaluator, the GROUP BY accumulators, and the projection
/// resolver.
fn run_joined_select(
    engine: &Engine,
    from: &FromClause,
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut joined = build_join_rows(engine, from, params)?;

    if let Some(filter) = stmt.r#where.as_ref() {
        joined.retain(|row| {
            let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
            uqa_sql::expr::eval(filter, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v))
        });
    }

    if has_aggregate(&stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &joined, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    if has_window(&stmt.projections) {
        let columns = projection_columns(&stmt.projections);
        let with_windows = compute_window_columns(engine, &stmt.projections, joined, params)?;
        let mut rows: Vec<ResultRow> = with_windows
            .iter()
            .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
            .collect::<Result<_, _>>()?;
        rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        from,
    );
    let mut rows: Vec<ResultRow> = joined
        .iter()
        .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
        .collect::<Result<_, _>>()?;
    rows = apply_row_order_limit(rows, stmt, engine, params)?;
    Ok(SQLResult::from_rows(columns, rows))
}

fn has_window(projections: &[Projection]) -> bool {
    projections
        .iter()
        .any(|p| matches!(p.expr, Expr::WindowCall { .. }))
}

fn compute_window_columns(
    engine: &Engine,
    projections: &[Projection],
    rows: Vec<ResultRow>,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = rows;
    let labels = projection_columns(projections);
    for (idx, proj) in projections.iter().enumerate() {
        let Expr::WindowCall { name, args, spec } = &proj.expr else {
            continue;
        };
        let label = labels[idx].clone();
        let values = evaluate_window(engine, name, args, spec, &rows, params)?;
        for (row, value) in rows.iter_mut().zip(values) {
            row.insert(label.clone(), value);
        }
    }
    Ok(rows)
}

fn evaluate_window(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    spec: &WindowSpec,
    rows: &[ResultRow],
    params: &[SQLParam],
) -> Result<Vec<Value>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut partitions: BTreeMap<Vec<Value>, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        let key: Vec<Value> = spec
            .partition_by
            .iter()
            .map(|e| uqa_sql::expr::eval(e, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        partitions.entry(key).or_default().push(i);
    }
    let mut output = vec![Value::Null; rows.len()];
    let lower = name.to_ascii_lowercase();
    for (_, indices) in partitions {
        let mut indexed: Vec<(usize, Vec<Value>)> = indices
            .into_iter()
            .map(|i| -> Result<_, SQLError> {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&rows[i]), params).with_engine(engine);
                let key: Vec<Value> = spec
                    .order_by
                    .iter()
                    .map(|o| uqa_sql::expr::eval(&o.expr, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((i, key))
            })
            .collect::<Result<Vec<_>, _>>()?;
        indexed.sort_by(|a, b| sort_keys(&a.1, &b.1, &spec.order_by));

        match lower.as_str() {
            "row_number" => {
                for (rank, (orig, _)) in indexed.iter().enumerate() {
                    output[*orig] = Value::Int((rank + 1) as i64);
                }
            }
            "rank" => {
                let mut last_key: Option<Vec<Value>> = None;
                let mut last_rank = 0i64;
                for (i, (orig, key)) in indexed.iter().enumerate() {
                    let rank = if last_key.as_ref() == Some(key) {
                        last_rank
                    } else {
                        last_key = Some(key.clone());
                        last_rank = (i + 1) as i64;
                        last_rank
                    };
                    output[*orig] = Value::Int(rank);
                }
            }
            "dense_rank" => {
                let mut last_key: Option<Vec<Value>> = None;
                let mut last_rank = 0i64;
                for (orig, key) in &indexed {
                    if last_key.as_ref() != Some(key) {
                        last_rank += 1;
                        last_key = Some(key.clone());
                    }
                    output[*orig] = Value::Int(last_rank);
                }
            }
            "lag" | "lead" => {
                let direction: i64 = if lower == "lag" { -1 } else { 1 };
                let target_expr = args.first().ok_or_else(|| SQLError::BadArity {
                    name: lower.clone(),
                    expected: ">=1".into(),
                    actual: 0,
                })?;
                let offset_value = match args.get(1) {
                    None => 1i64,
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params);
                        match uqa_sql::expr::eval(expr, &ctx)? {
                            Value::Int(n) => n,
                            other => {
                                return Err(SQLError::TypeMismatch(format!(
                                    "lag/lead offset must be integer, got {other:?}"
                                )));
                            }
                        }
                    }
                };
                let default_value = match args.get(2) {
                    None => Value::Null,
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params);
                        uqa_sql::expr::eval(expr, &ctx)?
                    }
                };
                for (i, (orig, _)) in indexed.iter().enumerate() {
                    let target_idx = i as i64 + direction * offset_value;
                    let value = if target_idx < 0 || target_idx as usize >= indexed.len() {
                        default_value.clone()
                    } else {
                        let target_orig = indexed[target_idx as usize].0;
                        let ctx = uqa_sql::expr::EvalContext::new(Some(&rows[target_orig]), params)
                            .with_engine(engine);
                        uqa_sql::expr::eval(target_expr, &ctx)?
                    };
                    output[*orig] = value;
                }
            }
            "ntile" => {
                let n = match args.first() {
                    Some(expr) => {
                        let ctx =
                            uqa_sql::expr::EvalContext::new(Some(&rows[indexed[0].0]), params);
                        match uqa_sql::expr::eval(expr, &ctx)? {
                            Value::Int(n) if n > 0 => n,
                            other => {
                                return Err(SQLError::TypeMismatch(format!(
                                    "ntile bucket count must be positive integer, got {other:?}"
                                )));
                            }
                        }
                    }
                    None => {
                        return Err(SQLError::BadArity {
                            name: "ntile".into(),
                            expected: "1".into(),
                            actual: 0,
                        });
                    }
                };
                let len = indexed.len() as i64;
                let base = len / n;
                let extra = len % n;
                let mut bucket = 1i64;
                let mut consumed_in_bucket = 0i64;
                let mut bucket_size = if 1 <= extra { base + 1 } else { base };
                for (orig, _) in &indexed {
                    if bucket_size == 0 {
                        output[*orig] = Value::Int(bucket);
                        bucket += 1;
                        continue;
                    }
                    output[*orig] = Value::Int(bucket);
                    consumed_in_bucket += 1;
                    if consumed_in_bucket == bucket_size {
                        bucket += 1;
                        consumed_in_bucket = 0;
                        bucket_size = if bucket <= extra { base + 1 } else { base };
                    }
                }
            }
            "sum" | "count" | "avg" | "min" | "max" => {
                evaluate_window_aggregate(
                    engine,
                    &lower,
                    args,
                    spec,
                    rows,
                    params,
                    &indexed,
                    &mut output,
                )?;
            }
            other => {
                return Err(SQLError::UnknownFunction(format!(
                    "window function `{other}` is not supported"
                )));
            }
        }
    }
    Ok(output)
}

/// Evaluate an aggregate window function (SUM/COUNT/AVG/MIN/MAX) over
/// each row's frame. Matches UQA behavior for `_compute_framed_aggregate` in
/// uqa/execution/relational.py.
#[allow(clippy::too_many_arguments)]
fn evaluate_window_aggregate(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    spec: &WindowSpec,
    rows: &[ResultRow],
    params: &[SQLParam],
    indexed: &[(usize, Vec<Value>)],
    output: &mut [Value],
) -> Result<(), SQLError> {
    use uqa_sql::ast::{FrameBound, FrameMode};
    let arg_expr = args.first();
    let n = indexed.len();
    let materialized: Vec<Value> = match arg_expr {
        Some(expr) => indexed
            .iter()
            .map(|(orig, _)| {
                let ctx =
                    uqa_sql::expr::EvalContext::new(Some(&rows[*orig]), params).with_engine(engine);
                uqa_sql::expr::eval(expr, &ctx)
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![Value::Int(1); n],
    };
    let order_keys: Vec<Vec<Value>> = indexed.iter().map(|(_, k)| k.clone()).collect();
    let (mode, start_bound, end_bound) = match &spec.frame {
        Some(f) => (f.mode, f.start.clone(), f.end.clone()),
        None if spec.order_by.is_empty() => {
            // No ORDER BY and no explicit frame: aggregate over the
            // whole partition.
            let mut acc = AggregateAccumulator::default();
            for v in &materialized {
                acc.observe(v);
            }
            let result = aggregate_value(name, &acc);
            for (orig, _) in indexed {
                output[*orig] = result.clone();
            }
            return Ok(());
        }
        None => (
            FrameMode::Rows,
            FrameBound::UnboundedPreceding,
            FrameBound::CurrentRow,
        ),
    };
    for (i, (orig, _)) in indexed.iter().enumerate() {
        let (start, end) = match mode {
            FrameMode::Range => (
                resolve_range_frame_index(
                    i,
                    n,
                    &order_keys,
                    &start_bound,
                    /* is_start = */ true,
                    rows,
                    params,
                    engine,
                )?,
                resolve_range_frame_index(
                    i,
                    n,
                    &order_keys,
                    &end_bound,
                    false,
                    rows,
                    params,
                    engine,
                )?,
            ),
            // GROUPS mode is rare; treat as ROWS (offset interpreted as
            // peer groups would require extra plumbing; matches the
            // fallback which also goes through `_resolve_frame_index`).
            FrameMode::Rows | FrameMode::Groups => (
                resolve_rows_frame_index(i, n, &start_bound, rows, params, engine, indexed)?,
                resolve_rows_frame_index(i, n, &end_bound, rows, params, engine, indexed)?,
            ),
        };
        let mut acc = AggregateAccumulator::default();
        if start <= end && start < n as i64 && end >= 0 {
            let lo = start.max(0) as usize;
            let hi = (end as usize).min(n.saturating_sub(1));
            for v in &materialized[lo..=hi] {
                acc.observe(v);
            }
        }
        output[*orig] = aggregate_value(name, &acc);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_rows_frame_index(
    current: usize,
    n: usize,
    bound: &uqa_sql::ast::FrameBound,
    rows: &[ResultRow],
    params: &[SQLParam],
    engine: &Engine,
    indexed: &[(usize, Vec<Value>)],
) -> Result<i64, SQLError> {
    use uqa_sql::ast::FrameBound;
    let n = n as i64;
    let cur = current as i64;
    Ok(match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::UnboundedFollowing => n - 1,
        FrameBound::CurrentRow => cur,
        FrameBound::Preceding(e) => {
            let off = eval_frame_offset(e, &rows[indexed[current].0], params, engine)?;
            (cur - off).max(0)
        }
        FrameBound::Following(e) => {
            let off = eval_frame_offset(e, &rows[indexed[current].0], params, engine)?;
            (cur + off).min(n - 1)
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve_range_frame_index(
    current: usize,
    n: usize,
    order_keys: &[Vec<Value>],
    bound: &uqa_sql::ast::FrameBound,
    is_start: bool,
    rows: &[ResultRow],
    params: &[SQLParam],
    engine: &Engine,
) -> Result<i64, SQLError> {
    use uqa_sql::ast::FrameBound;
    let key_at = |idx: usize| -> Option<&Value> { order_keys.get(idx).and_then(|k| k.first()) };
    Ok(match bound {
        FrameBound::UnboundedPreceding => 0,
        FrameBound::UnboundedFollowing => (n as i64) - 1,
        FrameBound::CurrentRow => {
            let cur_val = key_at(current).cloned().unwrap_or(Value::Null);
            if is_start {
                let mut idx = current;
                while idx > 0 && matches!(key_at(idx - 1), Some(v) if v == &cur_val) {
                    idx -= 1;
                }
                idx as i64
            } else {
                let mut idx = current;
                while idx + 1 < n && matches!(key_at(idx + 1), Some(v) if v == &cur_val) {
                    idx += 1;
                }
                idx as i64
            }
        }
        FrameBound::Preceding(e) | FrameBound::Following(e) => {
            let off = eval_frame_offset(e, &rows[current], params, engine)?;
            let cur_val = match key_at(current) {
                Some(Value::Int(n)) => *n as f64,
                Some(Value::Float(f)) => *f,
                _ => {
                    return Ok(if matches!(bound, FrameBound::Preceding(_)) {
                        if is_start {
                            0
                        } else {
                            current as i64
                        }
                    } else if is_start {
                        current as i64
                    } else {
                        (n as i64) - 1
                    });
                }
            };
            let target = if matches!(bound, FrameBound::Preceding(_)) {
                cur_val - off as f64
            } else {
                cur_val + off as f64
            };
            if is_start {
                let mut idx: i64 = -1;
                for i in 0..n {
                    let val = match key_at(i) {
                        Some(Value::Int(n)) => *n as f64,
                        Some(Value::Float(f)) => *f,
                        _ => continue,
                    };
                    if val >= target {
                        idx = i as i64;
                        break;
                    }
                }
                if idx < 0 {
                    n as i64
                } else {
                    idx
                }
            } else {
                let mut idx: i64 = -1;
                for i in 0..n {
                    let val = match key_at(i) {
                        Some(Value::Int(n)) => *n as f64,
                        Some(Value::Float(f)) => *f,
                        _ => continue,
                    };
                    if val <= target {
                        idx = i as i64;
                    } else {
                        break;
                    }
                }
                idx
            }
        }
    })
}

fn eval_frame_offset(
    expr: &Expr,
    row: &ResultRow,
    params: &[SQLParam],
    engine: &Engine,
) -> Result<i64, SQLError> {
    let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
    match uqa_sql::expr::eval(expr, &ctx)? {
        Value::Int(n) => Ok(n),
        Value::Float(f) => Ok(f as i64),
        other => Err(SQLError::TypeMismatch(format!(
            "frame offset must be numeric, got {other:?}"
        ))),
    }
}

fn sort_keys(a: &[Value], b: &[Value], order: &[OrderBy]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    use uqa_sql::ast::NullsOrder;
    for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
        let descending = order.get(i).is_some_and(|o| o.descending);
        // Resolve NULLS FIRST/LAST. Default mirrors PostgreSQL: ASC maps
        // to NULLS LAST, DESC maps to NULLS FIRST.
        let nulls_first = match order.get(i).and_then(|o| o.nulls) {
            Some(NullsOrder::First) => true,
            Some(NullsOrder::Last) => false,
            None => descending,
        };
        let a_null = matches!(av, Value::Null);
        let b_null = matches!(bv, Value::Null);
        if a_null || b_null {
            let null_cmp = match (a_null, b_null) {
                (true, true) => Ordering::Equal,
                (true, false) => {
                    if nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (false, true) => {
                    if nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (false, false) => unreachable!(),
            };
            if null_cmp != Ordering::Equal {
                return null_cmp;
            }
            continue;
        }
        let mut cmp = compare_values(av, bv);
        if descending {
            cmp = cmp.reverse();
        }
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

fn qualifier_for(name: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(name).to_string()
}

fn load_table_rows(engine: &Engine, table: &str) -> Vec<Document> {
    engine
        .table_doc_ids(table)
        .into_iter()
        .filter_map(|id| engine.get_document(table, id))
        .collect()
}

/// Synthesize rows for `information_schema` / `pg_catalog` virtual
/// views. Returns `None` for any unknown name so the caller falls back
/// to the regular table lookup.
fn build_info_schema_rows(engine: &Engine, name: &str) -> Option<Vec<ResultRow>> {
    let lower = name.to_ascii_lowercase();
    let is_information_schema = lower.starts_with("information_schema.");
    let is_pg_catalog = lower.starts_with("pg_catalog.");
    let stripped: &str = lower
        .strip_prefix("information_schema.")
        .or_else(|| lower.strip_prefix("pg_catalog."))
        .unwrap_or(&lower);
    match (is_information_schema, is_pg_catalog, stripped) {
        (true, _, "schemata") => Some(build_info_schemata(engine)),
        (true, _, "tables") => Some(build_info_tables(engine)),
        (true, _, "columns") => Some(build_info_columns(engine)),
        (true, _, "views") => Some(build_info_views(engine)),
        (true, _, "routines") => Some(build_info_routines()),
        (true, _, "sequences") => Some(build_info_sequences(engine)),
        (true, _, "table_constraints") => Some(build_info_table_constraints(engine)),
        (true, _, "key_column_usage") => Some(build_info_key_column_usage(engine)),
        (_, true, "pg_namespace") | (false, false, "pg_namespace") => {
            Some(build_pg_namespace(engine))
        }
        (_, true, "pg_class") | (false, false, "pg_class") => Some(build_pg_class(engine)),
        (_, true, "pg_attribute") | (false, false, "pg_attribute") => {
            Some(build_pg_attribute(engine))
        }
        (_, true, "pg_attrdef") | (false, false, "pg_attrdef") => Some(build_pg_attrdef(engine)),
        (_, true, "pg_constraint") | (false, false, "pg_constraint") => {
            Some(build_pg_constraint(engine))
        }
        (_, true, "pg_index") | (false, false, "pg_index") => Some(build_pg_index(engine)),
        (_, true, "pg_tables") | (false, false, "pg_tables") => Some(build_pg_tables(engine)),
        (_, true, "pg_views") | (false, false, "pg_views") => Some(build_pg_views(engine)),
        (_, true, "pg_indexes") | (false, false, "pg_indexes") => Some(build_pg_indexes(engine)),
        (_, true, "pg_type") | (false, false, "pg_type") => Some(build_pg_type()),
        (_, true, "pg_proc") | (false, false, "pg_proc") => Some(build_pg_proc()),
        (_, true, "pg_database") | (false, false, "pg_database") => Some(build_pg_database()),
        (_, true, "pg_roles") | (false, false, "pg_roles") => Some(build_pg_roles()),
        (_, true, "pg_user") | (false, false, "pg_user") => Some(build_pg_user()),
        (_, true, "pg_settings") | (false, false, "pg_settings") => Some(build_pg_settings(engine)),
        (_, true, "pg_description") | (false, false, "pg_description") => Some(Vec::new()),
        (_, true, "pg_matviews") | (false, false, "pg_matviews") => Some(Vec::new()),
        (_, true, "pg_sequences") | (false, false, "pg_sequences") => {
            Some(build_pg_sequences(engine))
        }
        _ => None,
    }
}

fn catalog_name() -> Value {
    Value::Str("uqa".into())
}

fn str_value(value: impl Into<String>) -> Value {
    Value::Str(value.into())
}

fn int_value(value: i64) -> Value {
    Value::Int(value)
}

fn bool_value(value: bool) -> Value {
    Value::Bool(value)
}

fn list_int(values: &[i64]) -> Value {
    Value::List(values.iter().copied().map(Value::Int).collect())
}

fn row(entries: impl IntoIterator<Item = (&'static str, Value)>) -> ResultRow {
    let mut out = ResultRow::new();
    for (key, value) in entries {
        out.insert(key.to_string(), value);
    }
    out
}

fn split_schema_name(name: &str) -> (String, String) {
    name.split_once('.').map_or_else(
        || ("public".to_string(), name.to_string()),
        |(schema, rel)| (schema.to_string(), rel.to_string()),
    )
}

fn split_index_name(index_name: &str, table_schema: &str) -> (String, String) {
    index_name.split_once('.').map_or_else(
        || (table_schema.to_string(), index_name.to_string()),
        |(schema, rel)| (schema.to_string(), rel.to_string()),
    )
}

fn stable_oid(kind: &str, name: &str) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain([b':']).chain(name.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

fn schema_oid(schema: &str) -> i64 {
    match schema {
        "pg_catalog" => 11,
        "public" => 2200,
        "information_schema" => 13_377,
        other => stable_oid("namespace", other),
    }
}

fn relation_oid(kind: &str, schema: &str, name: &str) -> i64 {
    stable_oid(kind, &format!("{schema}.{name}"))
}

fn current_user_oid() -> i64 {
    10
}

fn current_user_name() -> &'static str {
    "uqa"
}

fn all_schema_names(engine: &Engine) -> Vec<String> {
    let mut schemas = vec!["pg_catalog".to_string(), "information_schema".to_string()];
    schemas.extend(engine.list_schemas());
    schemas.sort();
    schemas.dedup();
    schemas
}

fn table_columns_for(engine: &Engine, table: &str) -> Vec<SQLColumnDef> {
    engine.describe_table(table).unwrap_or_default()
}

fn pg_type_oid(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 23,
        ColumnType::Text => 25,
        ColumnType::Real => 701,
        ColumnType::Numeric { .. } => 1700,
        ColumnType::Json => 114,
        ColumnType::Bytea => 17,
        ColumnType::Date => 1082,
        ColumnType::Time => 1083,
        ColumnType::TimeTz => 1266,
        ColumnType::Timestamp => 1114,
        ColumnType::TimestampTz => 1184,
        ColumnType::Vector(_) => 380_000,
    }
}

fn pg_type_len(ty: &ColumnType) -> i64 {
    match ty {
        ColumnType::Integer => 4,
        ColumnType::Real | ColumnType::Timestamp | ColumnType::TimestampTz => 8,
        ColumnType::Date => 4,
        ColumnType::Time | ColumnType::TimeTz => 8,
        _ => -1,
    }
}

fn info_datetime_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Time | ColumnType::TimeTz | ColumnType::Timestamp | ColumnType::TimestampTz => {
            Value::Int(6)
        }
        _ => Value::Null,
    }
}

fn info_numeric_precision(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Integer => Value::Int(32),
        ColumnType::Real => Value::Int(53),
        ColumnType::Numeric {
            precision: Some(precision),
            ..
        } => Value::Int(i64::from(*precision)),
        _ => Value::Null,
    }
}

fn info_numeric_scale(ty: &ColumnType) -> Value {
    match ty {
        ColumnType::Numeric {
            scale: Some(scale), ..
        } => Value::Int(i64::from(*scale)),
        _ => Value::Null,
    }
}

fn info_udt_name(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Integer => "int4",
        ColumnType::Text => "text",
        ColumnType::Real => "float8",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Json => "json",
        ColumnType::Bytea => "bytea",
        ColumnType::Date => "date",
        ColumnType::Time => "time",
        ColumnType::TimeTz => "timetz",
        ColumnType::Timestamp => "timestamp",
        ColumnType::TimestampTz => "timestamptz",
        ColumnType::Vector(_) => "vector",
    }
}

fn default_expr_text(expr: Option<&Expr>) -> Value {
    expr.map_or(Value::Null, |expr| Value::Str(format!("{expr:?}")))
}

fn index_columns(columns_json: &str) -> Vec<String> {
    serde_json::from_str(columns_json).unwrap_or_default()
}

fn indexdef(name: &str, index_type: &str, table: &str, columns: &[String]) -> String {
    let method = if index_type.is_empty() {
        "btree"
    } else {
        index_type
    };
    format!(
        "CREATE INDEX {name} ON {table} USING {method} ({})",
        columns.join(", ")
    )
}

fn build_info_schemata(engine: &Engine) -> Vec<ResultRow> {
    all_schema_names(engine)
        .into_iter()
        .map(|schema| {
            row([
                ("catalog_name", catalog_name()),
                ("schema_name", str_value(schema)),
                ("schema_owner", str_value(current_user_name())),
                ("default_character_set_catalog", catalog_name()),
                ("default_character_set_schema", str_value("pg_catalog")),
                ("default_character_set_name", str_value("UTF8")),
                ("sql_path", Value::Null),
            ])
        })
        .collect()
}

fn build_info_tables(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for name in engine.table_names() {
        let (schema, table) = split_schema_name(&name);
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(table)),
            ("table_type", str_value("BASE TABLE")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("YES")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    for name in engine.list_views() {
        let (schema, view) = split_schema_name(&name);
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(view)),
            ("table_type", str_value("VIEW")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("NO")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    for name in engine.list_foreign_tables() {
        let (schema, table) = split_schema_name(&name);
        out.push(row([
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_name", str_value(table)),
            ("table_type", str_value("FOREIGN")),
            ("self_referencing_column_name", Value::Null),
            ("reference_generation", Value::Null),
            ("user_defined_type_catalog", Value::Null),
            ("user_defined_type_schema", Value::Null),
            ("user_defined_type_name", Value::Null),
            ("is_insertable_into", str_value("YES")),
            ("is_typed", str_value("NO")),
            ("commit_action", Value::Null),
        ]));
    }
    out.sort_by(|a, b| {
        value_to_text(a.get("table_schema").unwrap_or(&Value::Null))
            .cmp(&value_to_text(
                b.get("table_schema").unwrap_or(&Value::Null),
            ))
            .then_with(|| {
                value_to_text(a.get("table_name").unwrap_or(&Value::Null))
                    .cmp(&value_to_text(b.get("table_name").unwrap_or(&Value::Null)))
            })
    });
    out
}

fn build_pg_tables(engine: &Engine) -> Vec<ResultRow> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut names = engine.table_names();
    names.sort();
    for name in names {
        let (schema, table) = split_schema_name(&name);
        out.push(row([
            ("schemaname", str_value(schema.clone())),
            ("tablename", str_value(table)),
            ("tableowner", str_value(current_user_name())),
            ("tablespace", Value::Null),
            (
                "hasindexes",
                bool_value(
                    engine
                        .list_catalog_indexes()
                        .iter()
                        .any(|idx| idx.table_name == name),
                ),
            ),
            ("hasrules", bool_value(false)),
            ("hastriggers", bool_value(false)),
            ("rowsecurity", bool_value(false)),
            ("table_catalog", catalog_name()),
            ("table_schema", str_value(schema)),
            ("table_type", str_value("BASE TABLE")),
        ]));
    }
    out
}

fn build_info_columns(engine: &Engine) -> Vec<ResultRow> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut tables = engine.table_names();
    tables.sort();
    for tname in tables {
        let Some(cols) = engine.describe_table(&tname) else {
            continue;
        };
        for (idx, col) in cols.iter().enumerate() {
            let (schema, table) = split_schema_name(&tname);
            out.push(row([
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(table)),
                ("column_name", str_value(col.name.clone())),
                ("ordinal_position", int_value((idx + 1) as i64)),
                ("column_default", default_expr_text(col.default.as_ref())),
                (
                    "is_nullable",
                    str_value(if col.not_null || col.primary_key {
                        "NO"
                    } else {
                        "YES"
                    }),
                ),
                ("data_type", str_value(column_type_name(&col.ty))),
                ("character_maximum_length", Value::Null),
                ("character_octet_length", Value::Null),
                ("numeric_precision", info_numeric_precision(&col.ty)),
                ("numeric_precision_radix", Value::Int(10)),
                ("numeric_scale", info_numeric_scale(&col.ty)),
                ("datetime_precision", info_datetime_precision(&col.ty)),
                ("interval_type", Value::Null),
                ("interval_precision", Value::Null),
                ("character_set_catalog", Value::Null),
                ("character_set_schema", Value::Null),
                ("character_set_name", Value::Null),
                ("collation_catalog", Value::Null),
                ("collation_schema", Value::Null),
                ("collation_name", Value::Null),
                ("domain_catalog", Value::Null),
                ("domain_schema", Value::Null),
                ("domain_name", Value::Null),
                ("udt_catalog", catalog_name()),
                ("udt_schema", str_value("pg_catalog")),
                ("udt_name", str_value(info_udt_name(&col.ty))),
                ("scope_catalog", Value::Null),
                ("scope_schema", Value::Null),
                ("scope_name", Value::Null),
                ("maximum_cardinality", Value::Null),
                ("dtd_identifier", str_value((idx + 1).to_string())),
                (
                    "is_self_referencing",
                    str_value(if col.references.is_some() {
                        "YES"
                    } else {
                        "NO"
                    }),
                ),
                (
                    "is_identity",
                    str_value(if col.auto_increment { "YES" } else { "NO" }),
                ),
                ("identity_generation", Value::Null),
                ("identity_start", Value::Null),
                ("identity_increment", Value::Null),
                ("identity_maximum", Value::Null),
                ("identity_minimum", Value::Null),
                ("identity_cycle", str_value("NO")),
                ("is_generated", str_value("NEVER")),
                ("generation_expression", Value::Null),
                ("is_updatable", str_value("YES")),
            ]));
        }
    }
    out
}

fn build_info_views(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_views()
        .into_iter()
        .map(|name| {
            let (schema, view) = split_schema_name(&name);
            row([
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(view)),
                (
                    "view_definition",
                    str_value(
                        engine
                            .view(&name)
                            .map_or_else(String::new, |stmt| format!("{stmt:?}")),
                    ),
                ),
                ("check_option", str_value("NONE")),
                ("is_updatable", str_value("NO")),
                ("is_insertable_into", str_value("NO")),
                ("is_trigger_updatable", str_value("NO")),
                ("is_trigger_deletable", str_value("NO")),
                ("is_trigger_insertable_into", str_value("NO")),
            ])
        })
        .collect()
}

fn build_info_routines() -> Vec<ResultRow> {
    registered_names()
        .into_iter()
        .map(|name| {
            row([
                ("specific_catalog", catalog_name()),
                ("specific_schema", str_value("pg_catalog")),
                ("specific_name", str_value(format!("{name}_0"))),
                ("routine_catalog", catalog_name()),
                ("routine_schema", str_value("pg_catalog")),
                ("routine_name", str_value(name)),
                ("routine_type", str_value("FUNCTION")),
                ("module_catalog", Value::Null),
                ("module_schema", Value::Null),
                ("module_name", Value::Null),
                ("udt_catalog", catalog_name()),
                ("udt_schema", str_value("pg_catalog")),
                ("udt_name", str_value("text")),
                ("data_type", str_value("text")),
                ("routine_body", str_value("EXTERNAL")),
                ("routine_definition", Value::Null),
                ("external_name", Value::Null),
                ("external_language", str_value("rust")),
                ("is_deterministic", str_value("NO")),
                ("sql_data_access", str_value("READS SQL DATA")),
                ("is_null_call", str_value("YES")),
                ("schema_level_routine", str_value("YES")),
                ("max_dynamic_result_sets", Value::Int(0)),
                ("is_udt_dependent", str_value("NO")),
                ("result_cast_from_null", str_value("NO")),
            ])
        })
        .collect()
}

fn build_info_sequences(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_sequences()
        .into_iter()
        .map(|name| {
            let (schema, sequence) = split_schema_name(&name);
            row([
                ("sequence_catalog", catalog_name()),
                ("sequence_schema", str_value(schema)),
                ("sequence_name", str_value(sequence)),
                ("data_type", str_value("bigint")),
                ("numeric_precision", Value::Int(64)),
                ("numeric_precision_radix", Value::Int(2)),
                ("numeric_scale", Value::Int(0)),
                ("start_value", Value::Null),
                ("minimum_value", Value::Null),
                ("maximum_value", Value::Null),
                ("increment", Value::Null),
                ("cycle_option", str_value("NO")),
            ])
        })
        .collect()
}

fn column_constraint_rows(engine: &Engine) -> Vec<(String, String, String, String, String, i64)> {
    let mut out = Vec::new();
    for table_name in engine.table_names() {
        let (schema, table) = split_schema_name(&table_name);
        for (idx, col) in table_columns_for(engine, &table_name).iter().enumerate() {
            let ordinal = (idx + 1) as i64;
            if col.primary_key {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_pkey", col.name),
                    "PRIMARY KEY".to_string(),
                    ordinal,
                ));
            }
            if col.unique {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_key", col.name),
                    "UNIQUE".to_string(),
                    ordinal,
                ));
            }
            if col.references.is_some() {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_fkey", col.name),
                    "FOREIGN KEY".to_string(),
                    ordinal,
                ));
            }
            if col.check.is_some() {
                out.push((
                    schema.clone(),
                    table.clone(),
                    col.name.clone(),
                    format!("{table}_{}_check", col.name),
                    "CHECK".to_string(),
                    ordinal,
                ));
            }
        }
    }
    out
}

fn build_info_table_constraints(engine: &Engine) -> Vec<ResultRow> {
    column_constraint_rows(engine)
        .into_iter()
        .map(|(schema, table, _column, constraint, kind, _ordinal)| {
            row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(schema.clone())),
                ("constraint_name", str_value(constraint)),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(table)),
                ("constraint_type", str_value(kind)),
                ("is_deferrable", str_value("NO")),
                ("initially_deferred", str_value("NO")),
                ("enforced", str_value("YES")),
                ("nulls_distinct", str_value("YES")),
            ])
        })
        .collect()
}

fn build_info_key_column_usage(engine: &Engine) -> Vec<ResultRow> {
    column_constraint_rows(engine)
        .into_iter()
        .filter(|(_, _, _, _, kind, _)| kind != "CHECK")
        .map(|(schema, table, column, constraint, _kind, ordinal)| {
            row([
                ("constraint_catalog", catalog_name()),
                ("constraint_schema", str_value(schema.clone())),
                ("constraint_name", str_value(constraint)),
                ("table_catalog", catalog_name()),
                ("table_schema", str_value(schema)),
                ("table_name", str_value(table)),
                ("column_name", str_value(column)),
                ("ordinal_position", int_value(ordinal)),
                ("position_in_unique_constraint", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_namespace(engine: &Engine) -> Vec<ResultRow> {
    all_schema_names(engine)
        .into_iter()
        .map(|schema| {
            row([
                ("oid", int_value(schema_oid(&schema))),
                ("nspname", str_value(schema)),
                ("nspowner", int_value(current_user_oid())),
                ("nspacl", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_class(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for name in engine.table_names() {
        let (schema, table) = split_schema_name(&name);
        let columns = table_columns_for(engine, &name);
        out.push(pg_class_row(
            &schema,
            &table,
            "r",
            columns.len() as i64,
            engine.document_count(&name) as f64,
            engine
                .list_catalog_indexes()
                .iter()
                .any(|idx| idx.table_name == name),
        ));
    }
    for name in engine.list_views() {
        let (schema, view) = split_schema_name(&name);
        out.push(pg_class_row(&schema, &view, "v", 0, 0.0, false));
    }
    for name in engine.list_foreign_tables() {
        let (schema, table) = split_schema_name(&name);
        out.push(pg_class_row(
            &schema,
            &table,
            "f",
            engine.foreign_table_columns(&name).len() as i64,
            0.0,
            false,
        ));
    }
    for sequence in engine.list_sequences() {
        let (schema, name) = split_schema_name(&sequence);
        out.push(pg_class_row(&schema, &name, "S", 0, 0.0, false));
    }
    for idx in engine.list_catalog_indexes() {
        let (table_schema, _) = split_schema_name(&idx.table_name);
        let (schema, index_name) = split_index_name(&idx.name, &table_schema);
        out.push(pg_class_row(&schema, &index_name, "i", 0, 0.0, false));
    }
    out
}

fn pg_class_row(
    schema: &str,
    name: &str,
    relkind: &str,
    natts: i64,
    tuples: f64,
    has_index: bool,
) -> ResultRow {
    row([
        ("oid", int_value(relation_oid(relkind, schema, name))),
        ("relname", str_value(name)),
        ("relnamespace", int_value(schema_oid(schema))),
        (
            "reltype",
            int_value(stable_oid("rowtype", &format!("{schema}.{name}"))),
        ),
        ("reloftype", int_value(0)),
        ("relowner", int_value(current_user_oid())),
        ("relam", int_value(0)),
        ("relfilenode", int_value(0)),
        ("reltablespace", int_value(0)),
        ("relpages", int_value(0)),
        ("reltuples", Value::Float(tuples)),
        ("relallvisible", int_value(0)),
        ("reltoastrelid", int_value(0)),
        ("relhasindex", bool_value(has_index)),
        ("relisshared", bool_value(false)),
        ("relpersistence", str_value("p")),
        ("relkind", str_value(relkind)),
        ("relnatts", int_value(natts)),
        ("relchecks", int_value(0)),
        ("relhasrules", bool_value(relkind == "v")),
        ("relhastriggers", bool_value(false)),
        ("relhassubclass", bool_value(false)),
        ("relrowsecurity", bool_value(false)),
        ("relforcerowsecurity", bool_value(false)),
        ("relispopulated", bool_value(true)),
        ("relreplident", str_value("d")),
        ("relispartition", bool_value(false)),
        ("relrewrite", int_value(0)),
        ("relfrozenxid", int_value(0)),
        ("relminmxid", int_value(0)),
        ("relacl", Value::Null),
        ("reloptions", Value::Null),
        ("relpartbound", Value::Null),
    ])
}

fn build_pg_attribute(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for table_name in engine.table_names() {
        let (schema, table) = split_schema_name(&table_name);
        let relid = relation_oid("r", &schema, &table);
        for (idx, col) in table_columns_for(engine, &table_name).iter().enumerate() {
            out.push(pg_attribute_row(relid, (idx + 1) as i64, col));
        }
    }
    for table_name in engine.list_foreign_tables() {
        let (schema, table) = split_schema_name(&table_name);
        let relid = relation_oid("f", &schema, &table);
        for (idx, col) in engine.foreign_table_columns(&table_name).iter().enumerate() {
            let col = SQLColumnDef {
                name: col.clone(),
                ty: ColumnType::Text,
                primary_key: false,
                not_null: false,
                auto_increment: false,
                unique: false,
                default: None,
                check: None,
                references: None,
            };
            out.push(pg_attribute_row(relid, (idx + 1) as i64, &col));
        }
    }
    out
}

fn pg_attribute_row(relid: i64, attnum: i64, col: &SQLColumnDef) -> ResultRow {
    row([
        ("attrelid", int_value(relid)),
        ("attname", str_value(col.name.clone())),
        ("atttypid", int_value(pg_type_oid(&col.ty))),
        ("attstattarget", int_value(-1)),
        ("attlen", int_value(pg_type_len(&col.ty))),
        ("attnum", int_value(attnum)),
        ("attndims", int_value(0)),
        ("attcacheoff", int_value(-1)),
        ("atttypmod", int_value(-1)),
        (
            "attbyval",
            bool_value(matches!(col.ty, ColumnType::Integer | ColumnType::Real)),
        ),
        ("attalign", str_value("i")),
        ("attstorage", str_value("x")),
        ("attcompression", str_value("")),
        ("attnotnull", bool_value(col.not_null || col.primary_key)),
        (
            "atthasdef",
            bool_value(col.default.is_some() || col.auto_increment),
        ),
        ("atthasmissing", bool_value(false)),
        (
            "attidentity",
            str_value(if col.auto_increment { "d" } else { "" }),
        ),
        ("attgenerated", str_value("")),
        ("attisdropped", bool_value(false)),
        ("attislocal", bool_value(true)),
        ("attinhcount", int_value(0)),
        ("attcollation", int_value(0)),
        ("attacl", Value::Null),
        ("attoptions", Value::Null),
        ("attfdwoptions", Value::Null),
    ])
}

fn build_pg_attrdef(engine: &Engine) -> Vec<ResultRow> {
    let mut out = Vec::new();
    for table_name in engine.table_names() {
        let (schema, table) = split_schema_name(&table_name);
        let relid = relation_oid("r", &schema, &table);
        for (idx, col) in table_columns_for(engine, &table_name).iter().enumerate() {
            if col.default.is_none() && !col.auto_increment {
                continue;
            }
            let default = if col.auto_increment {
                format!("nextval('{}_{}_seq')", table, col.name)
            } else {
                value_to_text(&default_expr_text(col.default.as_ref()))
            };
            out.push(row([
                (
                    "oid",
                    int_value(stable_oid("attrdef", &format!("{table_name}.{}", col.name))),
                ),
                ("adrelid", int_value(relid)),
                ("adnum", int_value((idx + 1) as i64)),
                ("adbin", str_value(default.clone())),
                ("adsrc", str_value(default)),
            ]));
        }
    }
    out
}

fn build_pg_constraint(engine: &Engine) -> Vec<ResultRow> {
    column_constraint_rows(engine)
        .into_iter()
        .map(|(schema, table, _column, constraint, kind, ordinal)| {
            let contype = match kind.as_str() {
                "PRIMARY KEY" => "p",
                "UNIQUE" => "u",
                "FOREIGN KEY" => "f",
                "CHECK" => "c",
                _ => "c",
            };
            row([
                (
                    "oid",
                    int_value(stable_oid("constraint", &format!("{schema}.{constraint}"))),
                ),
                ("conname", str_value(constraint)),
                ("connamespace", int_value(schema_oid(&schema))),
                ("contype", str_value(contype)),
                ("condeferrable", bool_value(false)),
                ("condeferred", bool_value(false)),
                ("convalidated", bool_value(true)),
                ("conrelid", int_value(relation_oid("r", &schema, &table))),
                ("contypid", int_value(0)),
                ("conindid", int_value(0)),
                ("conparentid", int_value(0)),
                ("confrelid", int_value(0)),
                ("confupdtype", str_value("a")),
                ("confdeltype", str_value("a")),
                ("confmatchtype", str_value("s")),
                ("conislocal", bool_value(true)),
                ("coninhcount", int_value(0)),
                ("connoinherit", bool_value(true)),
                ("conkey", list_int(&[ordinal])),
                ("confkey", Value::Null),
                ("conpfeqop", Value::Null),
                ("conppeqop", Value::Null),
                ("conffeqop", Value::Null),
                ("conexclop", Value::Null),
                ("conbin", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_index(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_catalog_indexes()
        .into_iter()
        .map(|idx| {
            let columns = index_columns(&idx.columns_json);
            let (schema, table) = split_schema_name(&idx.table_name);
            let (index_schema, index_name) = split_index_name(&idx.name, &schema);
            let table_cols = engine.table_columns(&idx.table_name);
            let keys: Vec<i64> = columns
                .iter()
                .filter_map(|col| table_cols.iter().position(|name| name == col))
                .map(|idx| (idx + 1) as i64)
                .collect();
            row([
                (
                    "indexrelid",
                    int_value(relation_oid("i", &index_schema, &index_name)),
                ),
                ("indrelid", int_value(relation_oid("r", &schema, &table))),
                ("indnatts", int_value(columns.len() as i64)),
                ("indnkeyatts", int_value(columns.len() as i64)),
                ("indisunique", bool_value(false)),
                ("indnullsnotdistinct", bool_value(false)),
                ("indisprimary", bool_value(false)),
                ("indisexclusion", bool_value(false)),
                ("indimmediate", bool_value(true)),
                ("indisclustered", bool_value(false)),
                ("indisvalid", bool_value(true)),
                ("indcheckxmin", bool_value(false)),
                ("indisready", bool_value(true)),
                ("indislive", bool_value(true)),
                ("indisreplident", bool_value(false)),
                (
                    "indkey",
                    Value::List(keys.into_iter().map(Value::Int).collect()),
                ),
                ("indcollation", Value::Null),
                ("indclass", Value::Null),
                ("indoption", Value::Null),
                ("indexprs", Value::Null),
                ("indpred", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_views(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_views()
        .into_iter()
        .map(|name| {
            let (schema, view) = split_schema_name(&name);
            row([
                ("schemaname", str_value(schema)),
                ("viewname", str_value(view)),
                ("viewowner", str_value(current_user_name())),
                (
                    "definition",
                    str_value(
                        engine
                            .view(&name)
                            .map_or_else(String::new, |stmt| format!("{stmt:?}")),
                    ),
                ),
            ])
        })
        .collect()
}

fn build_pg_indexes(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_catalog_indexes()
        .into_iter()
        .map(|idx| {
            let columns = index_columns(&idx.columns_json);
            let (schema, table) = split_schema_name(&idx.table_name);
            let (_, index_name) = split_index_name(&idx.name, &schema);
            row([
                ("schemaname", str_value(schema)),
                ("tablename", str_value(table.clone())),
                ("indexname", str_value(index_name.clone())),
                ("tablespace", Value::Null),
                (
                    "indexdef",
                    str_value(indexdef(&index_name, &idx.index_type, &table, &columns)),
                ),
            ])
        })
        .collect()
}

fn build_pg_type() -> Vec<ResultRow> {
    let types = [
        (16_i64, "bool", 1_i64, "B"),
        (17, "bytea", -1, "U"),
        (20, "int8", 8, "N"),
        (21, "int2", 2, "N"),
        (23, "int4", 4, "N"),
        (25, "text", -1, "S"),
        (700, "float4", 4, "N"),
        (701, "float8", 8, "N"),
        (1043, "varchar", -1, "S"),
        (1082, "date", 4, "D"),
        (1083, "time", 8, "D"),
        (1114, "timestamp", 8, "D"),
        (1184, "timestamptz", 8, "D"),
        (1266, "timetz", 8, "D"),
        (114, "json", -1, "U"),
        (3802, "jsonb", -1, "U"),
        (1700, "numeric", -1, "N"),
        (2278, "void", 4, "P"),
        (380_000, "vector", -1, "U"),
    ];
    types
        .into_iter()
        .map(|(oid, name, len, category)| {
            row([
                ("oid", int_value(oid)),
                ("typname", str_value(name)),
                ("typnamespace", int_value(schema_oid("pg_catalog"))),
                ("typowner", int_value(current_user_oid())),
                ("typlen", int_value(len)),
                ("typbyval", bool_value(len > 0 && len <= 8)),
                ("typtype", str_value("b")),
                ("typcategory", str_value(category)),
                ("typispreferred", bool_value(false)),
                ("typisdefined", bool_value(true)),
                ("typdelim", str_value(",")),
                ("typrelid", int_value(0)),
                ("typsubscript", str_value("-")),
                ("typelem", int_value(0)),
                ("typarray", int_value(0)),
                ("typinput", int_value(0)),
                ("typoutput", int_value(0)),
                ("typreceive", int_value(0)),
                ("typsend", int_value(0)),
                ("typmodin", int_value(0)),
                ("typmodout", int_value(0)),
                ("typanalyze", int_value(0)),
                ("typalign", str_value("i")),
                ("typstorage", str_value("x")),
                ("typnotnull", bool_value(false)),
                ("typbasetype", int_value(0)),
                ("typtypmod", int_value(-1)),
                ("typndims", int_value(0)),
                ("typcollation", int_value(0)),
                ("typdefaultbin", Value::Null),
                ("typdefault", Value::Null),
                ("typacl", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_proc() -> Vec<ResultRow> {
    registered_names()
        .into_iter()
        .map(|name| {
            row([
                ("oid", int_value(stable_oid("proc", name))),
                ("proname", str_value(name)),
                ("pronamespace", int_value(schema_oid("pg_catalog"))),
                ("proowner", int_value(current_user_oid())),
                ("prolang", int_value(0)),
                ("procost", Value::Float(1.0)),
                ("prorows", Value::Float(0.0)),
                ("provariadic", int_value(0)),
                ("prosupport", str_value("-")),
                ("prokind", str_value("f")),
                ("prosecdef", bool_value(false)),
                ("proleakproof", bool_value(false)),
                ("proisstrict", bool_value(false)),
                ("proretset", bool_value(false)),
                ("provolatile", str_value("s")),
                ("proparallel", str_value("s")),
                ("pronargs", int_value(0)),
                ("pronargdefaults", int_value(0)),
                ("prorettype", int_value(25)),
                ("proargtypes", Value::List(Vec::new())),
                ("proallargtypes", Value::Null),
                ("proargmodes", Value::Null),
                ("proargnames", Value::Null),
                ("proargdefaults", Value::Null),
                ("protrftypes", Value::Null),
                ("prosrc", str_value(name)),
                ("probin", Value::Null),
                ("prosqlbody", Value::Null),
                ("proconfig", Value::Null),
                ("proacl", Value::Null),
            ])
        })
        .collect()
}

fn build_pg_database() -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(5)),
        ("datname", str_value("uqa")),
        ("datdba", int_value(current_user_oid())),
        ("encoding", int_value(6)),
        ("datlocprovider", str_value("c")),
        ("datistemplate", bool_value(false)),
        ("datallowconn", bool_value(true)),
        ("datconnlimit", int_value(-1)),
        ("datfrozenxid", int_value(0)),
        ("datminmxid", int_value(0)),
        ("dattablespace", int_value(0)),
        ("datcollate", str_value("C")),
        ("datctype", str_value("C")),
        ("daticulocale", Value::Null),
        ("datcollversion", Value::Null),
        ("datacl", Value::Null),
    ])]
}

fn build_pg_roles() -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(current_user_oid())),
        ("rolname", str_value(current_user_name())),
        ("rolsuper", bool_value(true)),
        ("rolinherit", bool_value(true)),
        ("rolcreaterole", bool_value(true)),
        ("rolcreatedb", bool_value(true)),
        ("rolcanlogin", bool_value(true)),
        ("rolreplication", bool_value(false)),
        ("rolconnlimit", int_value(-1)),
        ("rolpassword", str_value("********")),
        ("rolvaliduntil", Value::Null),
        ("rolbypassrls", bool_value(true)),
        ("rolconfig", Value::Null),
    ])]
}

fn build_pg_user() -> Vec<ResultRow> {
    vec![row([
        ("usename", str_value(current_user_name())),
        ("usesysid", int_value(current_user_oid())),
        ("usecreatedb", bool_value(true)),
        ("usesuper", bool_value(true)),
        ("userepl", bool_value(false)),
        ("usebypassrls", bool_value(true)),
        ("passwd", str_value("********")),
        ("valuntil", Value::Null),
        ("useconfig", Value::Null),
    ])]
}

fn build_pg_settings(engine: &Engine) -> Vec<ResultRow> {
    let settings = [
        ("server_version", "17.0-uqa", "Version and compatibility"),
        ("server_encoding", "UTF8", "Client connection defaults"),
        ("client_encoding", "UTF8", "Client connection defaults"),
        ("DateStyle", "ISO, MDY", "Locale and formatting"),
        ("TimeZone", "UTC", "Locale and formatting"),
        (
            "search_path",
            &engine.show_variable("search_path"),
            "Client connection defaults",
        ),
    ];
    settings
        .into_iter()
        .map(|(name, setting, category)| {
            row([
                ("name", str_value(name)),
                ("setting", str_value(setting)),
                ("unit", Value::Null),
                ("category", str_value(category)),
                ("short_desc", str_value(name)),
                ("extra_desc", Value::Null),
                ("context", str_value("user")),
                ("vartype", str_value("string")),
                ("source", str_value("default")),
                ("min_val", Value::Null),
                ("max_val", Value::Null),
                ("enumvals", Value::Null),
                ("boot_val", str_value(setting)),
                ("reset_val", str_value(setting)),
                ("sourcefile", Value::Null),
                ("sourceline", Value::Null),
                ("pending_restart", bool_value(false)),
            ])
        })
        .collect()
}

fn build_pg_sequences(engine: &Engine) -> Vec<ResultRow> {
    engine
        .list_sequences()
        .into_iter()
        .map(|name| {
            let (schema, sequence) = split_schema_name(&name);
            row([
                ("schemaname", str_value(schema)),
                ("sequencename", str_value(sequence)),
                ("sequenceowner", str_value(current_user_name())),
                ("data_type", str_value("bigint")),
                ("start_value", Value::Null),
                ("min_value", Value::Null),
                ("max_value", Value::Null),
                ("increment_by", Value::Null),
                ("cycle", bool_value(false)),
                ("cache_size", Value::Int(1)),
                ("last_value", Value::Null),
            ])
        })
        .collect()
}

fn prefix_row(qual: &str, doc: &Document) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in doc {
        out.insert(format!("{qual}.{k}"), v.clone());
    }
    out
}

/// Re-key a row that already has unprefixed column labels onto a new
/// qualifier. Used to plug CTE materializations into the JOIN executor
/// under whatever alias the outer query referenced them by.
fn reprefix_row(qual: &str, row: &ResultRow) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in row {
        // CTE rows are already keyed by their projection labels; lift
        // unqualified labels under the new qualifier so qualified refs
        // (`alias.col`) and unqualified suffix matches both resolve.
        let key = if k.contains('.') {
            // Strip an existing qualifier and re-prefix.
            let (_, col) = k.split_once('.').unwrap_or((qual, k.as_str()));
            format!("{qual}.{col}")
        } else {
            format!("{qual}.{k}")
        };
        out.insert(key, v.clone());
    }
    out
}

fn merge_rows(left: &ResultRow, right: &ResultRow) -> ResultRow {
    let mut out = left.clone();
    for (k, v) in right {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn null_row_for(table: &str, alias: Option<&str>, engine: &Engine) -> ResultRow {
    let qual = qualifier_for(table, alias);
    let mut out = ResultRow::new();
    if engine.table(table).is_none() {
        for column in engine.foreign_table_columns(table) {
            out.insert(format!("{qual}.{column}"), Value::Null);
        }
        return out;
    }
    // Emit NULLs for any column that ever appeared in the table; for an
    // empty table we still know the keys via document_count, but the
    // safe default is just an empty row - a missing key resolves to
    // NULL through Expr::Column / QualifiedColumn lookup anyway.
    let mut sample_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in engine.table_doc_ids(table) {
        if let Some(doc) = engine.get_document(table, id) {
            for k in doc.keys() {
                sample_keys.insert(k.clone());
            }
        }
        if sample_keys.len() > 16 {
            break;
        }
    }
    for k in sample_keys {
        out.insert(format!("{qual}.{k}"), Value::Null);
    }
    out
}

fn build_join_rows(
    engine: &Engine,
    from: &FromClause,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    build_join_rows_with_ctes(engine, from, params, &BTreeMap::new())
}

fn build_join_rows_with_ctes(
    engine: &Engine,
    from: &FromClause,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SQLError> {
    match from {
        FromClause::Table { name, alias } => {
            let qual = qualifier_for(name, alias.as_deref());
            // CTE reference takes precedence over a real table of the
            // same name (matches `PostgreSQL` semantics).
            if let Some(rows) = ctes.get(name) {
                return Ok(rows.iter().map(|row| reprefix_row(&qual, row)).collect());
            }
            if let Some(body) = engine.view(name) {
                let mut scoped_ctes = ctes.clone();
                let result = execute_select(engine, &body, params, &mut scoped_ctes)?;
                return Ok(result
                    .rows
                    .iter()
                    .map(|row| reprefix_row(&qual, row))
                    .collect());
            }
            // information_schema / pg_catalog virtual views.
            if let Some(rows) = build_info_schema_rows(engine, name) {
                return Ok(rows.iter().map(|r| reprefix_row(&qual, r)).collect());
            }
            if engine.foreign_table(name).is_some() {
                let rows = engine
                    .scan_foreign_table(name, None, &[], None)
                    .map_err(SQLError::Unsupported)?;
                return Ok(rows.iter().map(|r| reprefix_row(&qual, r)).collect());
            }
            if engine.table(name).is_none() {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )));
            }
            Ok(load_table_rows(engine, name)
                .iter()
                .map(|d| prefix_row(&qual, d))
                .collect())
        }
        FromClause::Join {
            left,
            right,
            kind,
            on,
            lateral,
        } => {
            let left_rows = build_join_rows_with_ctes(engine, left, params, ctes)?;
            // LATERAL: re-evaluate the right side once per left row,
            // so the right body can reference outer columns. The
            // engine substitutes the outer row into the EvalContext
            // through the row-level evaluator.
            if *lateral {
                return build_lateral_join_rows(
                    engine,
                    &left_rows,
                    right,
                    *kind,
                    on.as_ref(),
                    params,
                    ctes,
                );
            }
            let right_rows = build_join_rows_with_ctes(engine, right, params, ctes)?;
            let on_expr = on.as_ref();

            match kind {
                JoinKind::Inner | JoinKind::Cross => {
                    if matches!(kind, JoinKind::Inner) {
                        if let Some(rows) =
                            try_hash_inner_join(engine, &left_rows, &right_rows, on_expr, params)?
                        {
                            return Ok(rows);
                        }
                    }
                    Ok(cross_filter(
                        engine,
                        &left_rows,
                        &right_rows,
                        on_expr,
                        params,
                    )?)
                }
                JoinKind::Left => Ok(left_outer(
                    &left_rows,
                    &right_rows,
                    right,
                    on_expr,
                    params,
                    engine,
                )?),
                JoinKind::Right => Ok(left_outer(
                    &right_rows,
                    &left_rows,
                    left,
                    on_expr,
                    params,
                    engine,
                )?),
                JoinKind::Full => Ok(full_outer(
                    &left_rows,
                    &right_rows,
                    left,
                    right,
                    on_expr,
                    params,
                    engine,
                )?),
            }
        }
        FromClause::Values {
            rows,
            alias,
            column_aliases,
        } => Ok(build_values_rows(
            engine,
            rows,
            alias.as_deref(),
            column_aliases,
            params,
        )?),
        FromClause::Function {
            name,
            args,
            alias,
            column_aliases,
        } => Ok(build_table_function_rows(
            engine,
            name,
            args,
            alias.as_deref(),
            column_aliases,
            params,
        )?),
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let result = run_select(engine, (**body).clone(), params)?;
            Ok(materialize_subquery_rows(
                result,
                alias.as_deref(),
                column_aliases,
            ))
        }
    }
}

fn materialize_subquery_rows(
    result: SQLResult,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Vec<ResultRow> {
    let cols = column_aliases.to_vec();
    result
        .rows
        .into_iter()
        .map(|mut r| {
            if !cols.is_empty() {
                let pairs: Vec<(String, Value)> = result
                    .columns
                    .iter()
                    .zip(cols.iter())
                    .filter_map(|(orig, new)| r.remove(orig).map(|v| (new.clone(), v)))
                    .collect();
                let mut renamed = ResultRow::new();
                for (k, v) in pairs {
                    renamed.insert(k, v);
                }
                if let Some(q) = alias {
                    return prefix_row(q, &renamed);
                }
                renamed
            } else if let Some(q) = alias {
                prefix_row(q, &r)
            } else {
                r
            }
        })
        .collect()
}

fn build_values_rows(
    engine: &Engine,
    rows: &[Vec<Expr>],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let n_cols = rows[0].len();
    let columns: Vec<String> = (0..n_cols)
        .map(|i| {
            column_aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("column{}", i + 1))
        })
        .collect();
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let mut out: Vec<ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        let r = match alias {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    }
    Ok(out)
}

/// LATERAL join executor: re-evaluates the right side per left row
/// so the right body can reference outer columns. We splice the
/// outer row into a per-row CTE-style scope by registering it under
/// the `__lateral__` reserved name and inlining its keys into a
/// fresh CTE map; the right side then sees those columns as plain
/// row keys when its internal expressions evaluate. Mirrors
/// `PostgreSQL` LATERAL semantics.
#[allow(clippy::too_many_arguments)]
fn build_lateral_join_rows(
    engine: &Engine,
    left_rows: &[ResultRow],
    right: &FromClause,
    kind: JoinKind,
    on: Option<&Expr>,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, truthy, EvalContext};
    let mut out: Vec<ResultRow> = Vec::new();
    for left_row in left_rows {
        let right_rows = match right {
            FromClause::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let result = execute_lateral_subquery(engine, body, left_row, params, ctes)?;
                materialize_subquery_rows(result, alias.as_deref(), column_aliases)
            }
            FromClause::Function {
                name,
                args,
                alias,
                column_aliases,
            } => build_table_function_rows_with_row(
                engine,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                params,
                Some(left_row),
            )?,
            _ => build_join_rows_with_ctes(engine, right, params, ctes)?,
        };
        for r_row in &right_rows {
            let mut joined = ResultRow::new();
            for (k, v) in left_row {
                joined.insert(k.clone(), v.clone());
            }
            for (k, v) in r_row {
                joined.insert(k.clone(), v.clone());
            }
            let keep = match (on, kind) {
                (None, _) | (_, JoinKind::Cross) => true,
                (Some(filter), _) => {
                    let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
                    truthy(&eval(filter, &ctx)?)
                }
            };
            if keep {
                out.push(joined);
            }
        }
    }
    Ok(out)
}

fn execute_lateral_subquery(
    engine: &Engine,
    stmt: &SelectStmt,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    let mut scoped_ctes = ctes.clone();
    materialize_ctes(engine, &stmt.with, params, &mut scoped_ctes)?;

    let Some(from) = stmt.from.as_ref() else {
        let projected =
            project_join_row_with_engine(Some(engine), outer_row, &stmt.projections, params)?;
        return Ok(SQLResult::from_rows(
            projection_columns(&stmt.projections),
            vec![projected],
        ));
    };

    let inner_rows = build_join_rows_with_ctes(engine, from, params, &scoped_ctes)?;
    let mut filtered: Vec<ResultRow> = Vec::with_capacity(inner_rows.len());
    for inner in inner_rows {
        let merged = merge_lateral_scope_rows(outer_row, &inner);
        let keep = match stmt.r#where.as_ref() {
            None => true,
            Some(filter) => {
                let ctx = EvalContext::new(Some(&merged), params).with_engine(engine);
                uqa_sql::expr::truthy(&eval(filter, &ctx)?)
            }
        };
        if keep {
            filtered.push(merged);
        }
    }

    if has_aggregate(&stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        let columns = projection_columns(&stmt.projections);
        let rows = aggregate_join_rows(engine, stmt, &filtered, params)?;
        let rows = apply_row_order_limit(rows, stmt, engine, params)?;
        return Ok(SQLResult::from_rows(columns, rows));
    }

    let ordered = apply_row_order_limit(filtered, stmt, engine, params)?;
    let columns = projection_columns(&stmt.projections);
    let rows = ordered
        .iter()
        .map(|src| project_join_row_with_engine(Some(engine), src, &stmt.projections, params))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SQLResult::from_rows(columns, rows))
}

fn merge_lateral_scope_rows(outer_row: &ResultRow, inner_row: &ResultRow) -> ResultRow {
    let mut merged = outer_row.clone();
    for (key, value) in inner_row {
        merged.insert(key.clone(), value.clone());
        if let Some((_, column)) = key.rsplit_once('.') {
            merged.insert(column.to_string(), value.clone());
        }
    }
    merged
}

#[allow(clippy::similar_names)]
fn build_table_function_rows(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    build_table_function_rows_with_row(engine, name, args, alias, column_aliases, params, None)
}

#[allow(clippy::similar_names)]
fn build_table_function_rows_with_row(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
    row: Option<&ResultRow>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    let ctx = EvalContext::new(row, params).with_engine(engine);
    let lower = name.to_ascii_lowercase();
    let evaluated: Vec<Value> = args
        .iter()
        .map(|a| eval(a, &ctx))
        .collect::<Result<Vec<_>, SQLError>>()?;
    let default_col = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| name.to_string());
    let qual = alias;
    let mut out: Vec<ResultRow> = Vec::new();
    let push_scalar = |out: &mut Vec<ResultRow>, value: Value| {
        let mut r = ResultRow::new();
        r.insert(default_col.clone(), value);
        let r = match qual {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    };
    match lower.as_str() {
        "generate_series" => {
            if !(2..=3).contains(&evaluated.len()) {
                return Err(SQLError::TypeMismatch(
                    "generate_series requires 2-3 args".into(),
                ));
            }
            let start = match &evaluated[0] {
                Value::Int(i) => *i,
                Value::Float(f) => *f as i64,
                _ => return Err(SQLError::TypeMismatch("generate_series start".into())),
            };
            let stop = match &evaluated[1] {
                Value::Int(i) => *i,
                Value::Float(f) => *f as i64,
                _ => return Err(SQLError::TypeMismatch("generate_series stop".into())),
            };
            let step = if evaluated.len() == 3 {
                match &evaluated[2] {
                    Value::Int(i) => *i,
                    Value::Float(f) => *f as i64,
                    _ => return Err(SQLError::TypeMismatch("generate_series step".into())),
                }
            } else {
                1
            };
            if step == 0 {
                return Err(SQLError::TypeMismatch(
                    "generate_series step cannot be 0".into(),
                ));
            }
            let mut current = start;
            if step > 0 {
                while current <= stop {
                    push_scalar(&mut out, Value::Int(current));
                    current += step;
                }
            } else {
                while current >= stop {
                    push_scalar(&mut out, Value::Int(current));
                    current += step;
                }
            }
            Ok(out)
        }
        "unnest" => {
            for value in &evaluated {
                if let Value::List(items) = value {
                    for item in items {
                        push_scalar(&mut out, item.clone());
                    }
                } else {
                    push_scalar(&mut out, value.clone());
                }
            }
            Ok(out)
        }
        "regexp_split_to_table" => {
            if evaluated.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "regexp_split_to_table requires 2 args".into(),
                ));
            }
            let s = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 1".into())),
            };
            let pat = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 2".into())),
            };
            let re = regex::Regex::new(&pat)
                .map_err(|e| SQLError::TypeMismatch(format!("invalid regex: {e}")))?;
            for piece in re.split(&s) {
                push_scalar(&mut out, Value::Str(piece.to_string()));
            }
            Ok(out)
        }
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Object(obj) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an object"
                )));
            };
            let key_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "key".into());
            let val_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "value".into());
            for (k, v) in obj {
                let mut r = ResultRow::new();
                r.insert(key_col.clone(), Value::Str(k));
                r.insert(val_col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "json_array_elements"
        | "jsonb_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Array(arr) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an array"
                )));
            };
            let col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "value".into());
            for v in arr {
                let mut r = ResultRow::new();
                r.insert(col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        // -------------------------------------------------------------
        // Analyzer DDL exposed as table-functions. Mirror the canonical UQA implementation's
        // _build_create_analyzer / _build_drop_analyzer /
        // _build_list_analyzers / _build_set_table_analyzer.
        // -------------------------------------------------------------
        "create_analyzer" => {
            if evaluated.len() < 2 {
                return Err(SQLError::TypeMismatch(
                    "create_analyzer requires (name, config_json)".into(),
                ));
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 1".into())),
            };
            let config_json = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 2".into())),
            };
            engine
                .register_named_analyzer(&analyzer_name, &config_json)
                .map_err(SQLError::Unsupported)?;
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "create_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' created")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "drop_analyzer" => {
            if evaluated.is_empty() {
                return Err(SQLError::TypeMismatch(
                    "drop_analyzer requires a name argument".into(),
                ));
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("drop_analyzer arg 1".into())),
            };
            engine.drop_named_analyzer(&analyzer_name);
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "drop_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' dropped")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "list_analyzers" => {
            // Match UQA behavior for: include the four built-in analyzers
            // (`whitespace`, `standard`, `standard_cjk`, `keyword`) on
            // top of every user-registered named analyzer.
            let mut names: std::collections::BTreeSet<String> =
                engine.list_named_analyzers().into_iter().collect();
            for builtin in ["whitespace", "standard", "standard_cjk", "keyword"] {
                names.insert(builtin.to_string());
            }
            let key = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "analyzer_name".into());
            for n in names {
                let mut r = ResultRow::new();
                r.insert(key.clone(), Value::Str(n));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "fts_index_stats" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(
                    "fts_index_stats accepts optional table name".into(),
                ));
            }
            let table_filter = match evaluated.first() {
                Some(Value::Str(s)) => Some(s.as_str()),
                Some(_) => return Err(SQLError::TypeMismatch("fts_index_stats arg 1".into())),
                None => None,
            };
            for stat in engine.fts_index_stats(table_filter) {
                let mut r = ResultRow::new();
                r.insert("table_name".into(), Value::Str(stat.table_name));
                r.insert("field".into(), Value::Str(stat.field));
                r.insert("analyzer".into(), Value::Str(stat.analyzer));
                r.insert(
                    "posting_count".into(),
                    Value::Int(stat.posting_count as i64),
                );
                r.insert(
                    "doc_length_count".into(),
                    Value::Int(stat.doc_length_count as i64),
                );
                r.insert(
                    "indexed_doc_count".into(),
                    Value::Int(stat.indexed_doc_count as i64),
                );
                r.insert("term_count".into(), Value::Int(stat.term_count as i64));
                r.insert(
                    "total_field_length".into(),
                    Value::Int(stat.total_field_length as i64),
                );
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "set_table_analyzer" => {
            if evaluated.len() < 3 {
                return Err(SQLError::TypeMismatch(
                    "set_table_analyzer requires (table, field, analyzer_name[, phase])".into(),
                ));
            }
            let target_table = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 1".into())),
            };
            let field = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 2".into())),
            };
            let analyzer_name = match &evaluated[2] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 3".into())),
            };
            let phase = if evaluated.len() > 3 {
                match &evaluated[3] {
                    Value::Str(s) => s.clone(),
                    _ => "both".into(),
                }
            } else {
                "both".into()
            };
            engine
                .set_table_field_analyzer(&target_table, &field, &analyzer_name, &phase)
                .map_err(SQLError::Unsupported)?;
            let mut msg = format!("analyzer '{analyzer_name}' assigned to {target_table}.{field}");
            if phase != "both" {
                use std::fmt::Write as _;
                let _ = write!(msg, " (phase={phase})");
            }
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "set_table_analyzer".into()),
                Value::Str(msg),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "cypher" => age_cypher::build_rows(engine, args, &evaluated, qual, column_aliases),
        "rpq" => {
            if evaluated.len() != 3 {
                return Err(SQLError::TypeMismatch(
                    "rpq requires 3 args (expr, start, graph)".into(),
                ));
            }
            let expr_str = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("rpq.expr must be string".into())),
            };
            let start = match &evaluated[1] {
                Value::Int(n) => *n as u64,
                _ => return Err(SQLError::TypeMismatch("rpq.start must be integer".into())),
            };
            let graph = match &evaluated[2] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("rpq.graph must be string".into())),
            };
            let path = uqa_graph::parse_rpq(&expr_str)
                .map_err(|e| SQLError::Unsupported(format!("{e:?}")))?;
            let pl = engine
                .graph_with(&graph, |store| {
                    uqa_graph::RegularPathQuery::new(path, &graph)
                        .from_vertex(start)
                        .execute(store)
                })
                .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))?;
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "vertex_id".into());
            for entry in pl.inner().entries() {
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), Value::Int(entry.doc_id as i64));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "table function `{other}` in FROM"
        ))),
    }
}

/// Detect an equijoin shape `<col_a> = <col_b>` and run a hash join.
///
/// Returns `Some(rows)` when the predicate is a clean equality
/// between qualified columns from the two sides. Returns `None` for
/// every other shape; the caller then falls back to the nested-loop
/// cross filter.
fn try_hash_inner_join(
    engine: &Engine,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ResultRow>>, SQLError> {
    let Some(Expr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs,
        rhs,
    }) = on
    else {
        return Ok(None);
    };
    let Some((left_key, right_key)) =
        decide_join_sides(engine, left_rows, right_rows, lhs, rhs, params)
    else {
        return Ok(None);
    };
    // Use the shared hash-join algorithm from `uqa-joins`. The closures
    // evaluate the picked join keys against each row and lift the
    // result into a hashable `JoinKey`; null-valued keys are skipped
    // so they do not match anything.
    use uqa_joins::row_join::{hash_inner_join, JoinKey};
    let key_of = |row: &ResultRow, expr: &Expr| -> Option<JoinKey> {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        match uqa_sql::expr::eval(expr, &ctx) {
            Ok(uqa_core::Value::Null) | Err(_) => None,
            Ok(v) => Some(JoinKey::new(&v)),
        }
    };
    let out = hash_inner_join(
        left_rows,
        right_rows,
        |row| key_of(row, left_key),
        |row| key_of(row, right_key),
    );
    Ok(Some(out))
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
fn decide_join_sides<'a>(
    engine: &Engine,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a Expr,
    rhs: &'a Expr,
    params: &[SQLParam],
) -> Option<(&'a Expr, &'a Expr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(engine, l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(engine, r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(engine, l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(engine, r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

fn eval_yields_value(engine: &Engine, row: &ResultRow, expr: &Expr, params: &[SQLParam]) -> bool {
    let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
    matches!(uqa_sql::expr::eval(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

fn cross_filter(
    engine: &Engine,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    on: Option<&Expr>,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::with_capacity(left_rows.len() * right_rows.len());
    for l in left_rows {
        for r in right_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&merged), params).with_engine(engine);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
            }
        }
    }
    Ok(out)
}

fn left_outer(
    outer_rows: &[ResultRow],
    inner_rows: &[ResultRow],
    inner_from: &FromClause,
    on: Option<&Expr>,
    params: &[SQLParam],
    engine: &Engine,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    for l in outer_rows {
        let mut matched = false;
        for r in inner_rows {
            let merged = merge_rows(l, r);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&merged), params).with_engine(engine);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
                matched = true;
            }
        }
        if !matched {
            // Pad with NULLs for every column the inner side would
            // have contributed.
            let mut pad = l.clone();
            pad_nulls_for_from(&mut pad, inner_from, engine);
            out.push(pad);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn full_outer(
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    left_from: &FromClause,
    right_from: &FromClause,
    on: Option<&Expr>,
    params: &[SQLParam],
    engine: &Engine,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut out = Vec::new();
    let mut matched_right = vec![false; right_rows.len()];
    for left in left_rows {
        let mut matched_left = false;
        for (idx, right) in right_rows.iter().enumerate() {
            let merged = merge_rows(left, right);
            let keep = match on {
                None => true,
                Some(expr) => {
                    let ctx =
                        uqa_sql::expr::EvalContext::new(Some(&merged), params).with_engine(engine);
                    uqa_sql::expr::truthy(&uqa_sql::expr::eval(expr, &ctx)?)
                }
            };
            if keep {
                out.push(merged);
                matched_left = true;
                matched_right[idx] = true;
            }
        }
        if !matched_left {
            let mut padded = left.clone();
            pad_nulls_for_from(&mut padded, right_from, engine);
            out.push(padded);
        }
    }
    for (idx, right) in right_rows.iter().enumerate() {
        if matched_right[idx] {
            continue;
        }
        let mut padded = ResultRow::new();
        pad_nulls_for_from(&mut padded, left_from, engine);
        for (k, v) in right {
            padded.insert(k.clone(), v.clone());
        }
        out.push(padded);
    }
    Ok(out)
}

fn pad_nulls_for_from(row: &mut ResultRow, from: &FromClause, engine: &Engine) {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    for (name, alias) in &tables {
        let null_keys = null_row_for(name, alias.as_deref(), engine);
        for (k, v) in null_keys {
            row.entry(k).or_insert(v);
        }
    }
}

#[allow(dead_code)]
fn project_join_row(
    engine: &Engine,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    project_join_row_with_engine(Some(engine), src, projections, params)
}

fn project_join_row_with_engine(
    engine: Option<&Engine>,
    src: &ResultRow,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut ctx = uqa_sql::expr::EvalContext::new(Some(src), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let labels = projection_columns(projections);
    let mut out = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let Expr::Star = proj.expr {
            for (k, v) in src {
                out.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Window calls are pre-evaluated in `compute_window_columns`
        // and stored on the source row under the projection label;
        // read the cached value through.
        if matches!(proj.expr, Expr::WindowCall { .. }) {
            let value = src.get(&label).cloned().unwrap_or(Value::Null);
            out.insert(label, value);
            continue;
        }
        // `uqa_highlight()` evaluates against the analyzer for the
        // matched field, which the evaluator does not have access
        // to. Intercept the call here, resolve the string column +
        // query, and emit the wrapped text through
        // `uqa_analysis::highlight`.
        if let Expr::Func { name, args, .. } = &proj.expr {
            if let Some(value) = engine_func_intercept(engine, name, args, src, params)? {
                out.insert(label, value);
                continue;
            }
        }
        let value = uqa_sql::expr::eval(&proj.expr, &ctx)?;
        out.insert(label, value);
    }
    Ok(out)
}

/// Intercept registry functions that need engine-level access (the
/// scalar evaluator does not see the engine, just the row context).
/// Returns `Ok(Some(_))` when the function was handled, `Ok(None)`
/// to defer to the default scalar evaluator.
fn engine_func_intercept(
    engine: Option<&Engine>,
    name: &str,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(engine, row, args, params)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, row, params)?;
            Ok(Some(
                row.get(SCORE_COLUMN).cloned().unwrap_or(Value::Float(0.0)),
            ))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(engine, args, row, params)?)),
        "graph_create" | "create_graph" => {
            if let Some(eng) = engine {
                let _ = run_graph_create(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        "graph_drop" | "drop_graph" => {
            if let Some(eng) = engine {
                let _ = run_graph_drop(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        _ => Ok(None),
    }
}

fn run_deep_learn_projection(
    engine: Option<&Engine>,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let Some(engine) = engine else {
        return Err(SQLError::Unsupported(
            "deep_learn requires an engine-backed projection".into(),
        ));
    };
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "deep_learn".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(Some(row), params).with_engine(engine);
    let model_name = match eval(&args[0], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match eval(&args[1], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.training_set must be a table name or JSON string, got {other:?}"
            )));
        }
    };
    let trimmed = training_source.trim();
    let output = if trimmed.starts_with('{') {
        engine.deep_learn_json(&model_name, trimmed, &uqa_ml::LearnOptions::default())?
    } else {
        engine.deep_learn_table(
            &model_name,
            &training_source,
            &uqa_ml::LearnOptions::default(),
        )?
    };
    let mut report = BTreeMap::new();
    report.insert("model".into(), Value::Str(model_name));
    report.insert("examples".into(), Value::Int(output.report.examples as i64));
    report.insert(
        "feature_dimensions".into(),
        Value::Int(output.report.feature_dimensions as i64),
    );
    report.insert(
        "class_count".into(),
        Value::Int(output.report.class_count as i64),
    );
    Ok(Value::Map(report))
}

fn validate_score_projection_args(
    name: &str,
    args: &[Expr],
    row: &ResultRow,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let query_idx = args.len() - 1;
    if args.len() == 2 {
        let _ = expect_column_name(&args[0], &format!("{name}.field"))?;
    }
    let ctx = EvalContext::new(Some(row), params);
    match eval(&args[query_idx], &ctx)? {
        Value::Str(_) => Ok(()),
        other => Err(SQLError::TypeMismatch(format!(
            "{name}.query must be a string, got {other:?}"
        ))),
    }
}

/// Evaluate a `uqa_highlight(field, query[, start_tag, end_tag,
/// max_fragments, fragment_size])` projection. Matches UQA
/// reference's `_run_uqa_highlight` shape: field can be either a
/// bare column reference (looked up on the row) or a literal string;
/// the rest of the args are scalar literals after evaluation.
fn run_uqa_highlight(
    engine: Option<&Engine>,
    row: &ResultRow,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let mut ctx = uqa_sql::expr::EvalContext::new(Some(row), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let text = match &args[0] {
        Expr::Column(c) => match row.get(c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(Value::Null),
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        Expr::QualifiedColumn { qualifier, column } => {
            match row.get(&format!("{qualifier}.{column}")) {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(Value::Null),
                Some(other) => format!("{other:?}"),
                None => return Ok(Value::Null),
            }
        }
        other => match uqa_sql::expr::eval(other, &ctx)? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            v => format!("{v:?}"),
        },
    };
    let query_str = match uqa_sql::expr::eval(&args[1], &ctx)? {
        Value::Str(s) => s,
        Value::Null => return Ok(Value::Str(text)),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Str(s) => s,
            Value::Null => "<b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight start_tag must be string, got {other:?}"
                )));
            }
        },
        None => "<b>".into(),
    };
    let end_tag = match args.get(3) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Str(s) => s,
            Value::Null => "</b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight end_tag must be string, got {other:?}"
                )));
            }
        },
        None => "</b>".into(),
    };
    let max_fragments = match args.get(4) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Int(n) if n >= 0 => n as usize,
            Value::Null => 0,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments must be non-negative integer, got {other:?}"
                )));
            }
        },
        None => 0,
    };
    let fragment_size = match args.get(5) {
        Some(e) => match uqa_sql::expr::eval(e, &ctx)? {
            Value::Int(n) if n > 0 => n as usize,
            Value::Null => 150,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size must be positive integer, got {other:?}"
                )));
            }
        },
        None => 150,
    };
    let opts = uqa_analysis::HighlightOptions {
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    };
    // Pull every whitespace-separated token out of the query string
    // as a candidate match term. The canonical UQA behavior parses the FTS
    // query, but a simple split is what callers reach for in
    // practice and matches what the test fixture exercises.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight(&text, &terms, Some(&analyzer), &opts);
    Ok(Value::Str(out))
}

fn aggregate_join_rows(
    engine: &Engine,
    stmt: &SelectStmt,
    rows: &[ResultRow],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // GROUPING SETS / ROLLUP / CUBE: run the aggregator once per
    // grouping set, then concatenate the result rows. Columns that
    // aren't in the active grouping set come out as NULL.
    if !stmt.grouping_sets.is_empty() {
        let mut combined: Vec<ResultRow> = Vec::new();
        let sets = stmt.grouping_sets.clone();
        let labels = projection_columns(&stmt.projections);
        for set in sets {
            let mut sub = stmt.clone();
            sub.group_by.clone_from(&set);
            sub.grouping_sets = Vec::new();
            let part = aggregate_join_rows_relaxed(engine, &sub, rows, params)?;
            // Columns from the parent projection that aren't in the
            // active grouping set get filled with NULL on every row.
            for mut row in part {
                for (idx, proj) in stmt.projections.iter().enumerate() {
                    let label = labels[idx].clone();
                    if is_aggregate(&proj.expr) {
                        continue;
                    }
                    let in_set = set.iter().any(|g| exprs_match(&proj.expr, g));
                    if !in_set {
                        row.insert(label, Value::Null);
                    }
                }
                combined.push(row);
            }
        }
        return Ok(combined);
    }
    let agg_targets: Vec<&Projection> = stmt
        .projections
        .iter()
        .filter(|p| is_aggregate(&p.expr))
        .collect();

    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();

    for row in rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = groups.entry(group_values.clone()).or_insert_with(|| {
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                group_values,
            )
        });
        for (i, proj) in agg_targets.iter().enumerate() {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = &proj.expr
            else {
                continue;
            };
            if let Some(filter_expr) = filter.as_deref() {
                let keep =
                    uqa_sql::expr::eval(filter_expr, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
                if !keep {
                    continue;
                }
            }
            let value = aggregate_input_value(name, args, order_by, &ctx)?;
            if *distinct && !matches!(value, Value::Null) {
                let key = distinct_key(&value);
                if !bucket.0[i].distinct.insert(key) {
                    continue;
                }
            }
            let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
            for ob in order_by {
                let v = uqa_sql::expr::eval(&ob.expr, &ctx)?;
                sort_keys.push((v, ob.descending));
            }
            if order_by.is_empty() {
                bucket.0[i].observe(&value);
            } else {
                bucket.0[i].observe_with_sort_keys(&value, sort_keys);
            }
        }
    }

    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                Vec::new(),
            ),
        );
    }

    let mut out = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if is_aggregate(&proj.expr) {
                let Expr::Func { name, args, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value_with_args(name, acc, args));
            } else {
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    return Err(SQLError::Unsupported(format!(
                        "non-aggregated projection `{label}` must appear in GROUP BY"
                    )));
                }
            }
        }
        // HAVING filter: evaluated against a synthetic row that
        // contains the group-by column values plus every projection
        // alias. Aggregate references inside the HAVING expression
        // resolve through `eval_aggregate_in_having` which walks the
        // group rows to recompute the aggregate without re-projecting.
        if let Some(having_expr) = stmt.having.as_ref() {
            let resolved = resolve_having(having_expr, &row, stmt, &accs, &group_values, params)?;
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
            let kept =
                uqa_sql::expr::eval(&resolved, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
            if !kept {
                continue;
            }
        }
        out.push(row);
    }
    Ok(out)
}

/// Walk a HAVING expression and replace each aggregate-function
/// reference with its computed value from the group's accumulators.
/// Non-aggregate sub-expressions (column refs, comparisons, AND / OR)
/// pass through untouched so the caller can `eval` the result.
fn resolve_having(
    expr: &Expr,
    _projected_row: &ResultRow,
    stmt: &SelectStmt,
    accs: &[AggregateAccumulator],
    _group_values: &[Value],
    _params: &[SQLParam],
) -> Result<Expr, SQLError> {
    fn walk(e: &Expr, stmt: &SelectStmt, accs: &[AggregateAccumulator]) -> Result<Expr, SQLError> {
        if is_aggregate(e) {
            // Find the matching projection so we can pluck the
            // already-computed accumulator value. Falls back to
            // matching by aggregate-function shape (name + args).
            for (idx, proj) in stmt
                .projections
                .iter()
                .filter(|p| is_aggregate(&p.expr))
                .enumerate()
            {
                if exprs_match(&proj.expr, e) {
                    if let Expr::Func { name, args, .. } = &proj.expr {
                        let v = aggregate_value_with_args(name, &accs[idx], args);
                        return Ok(Expr::Literal(v));
                    }
                }
            }
            // Aggregate appears in HAVING but not in SELECT; reject.
            return Err(SQLError::Unsupported(
                "HAVING references an aggregate that is not in the SELECT list".into(),
            ));
        }
        match e {
            Expr::And(parts) => Ok(Expr::And(
                parts
                    .iter()
                    .map(|p| walk(p, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Expr::Or(parts) => Ok(Expr::Or(
                parts
                    .iter()
                    .map(|p| walk(p, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
            Expr::Not(inner) => Ok(Expr::Not(Box::new(walk(inner, stmt, accs)?))),
            Expr::Binary { op, lhs, rhs } => Ok(Expr::Binary {
                op: *op,
                lhs: Box::new(walk(lhs, stmt, accs)?),
                rhs: Box::new(walk(rhs, stmt, accs)?),
            }),
            Expr::IsNull { expr, negated } => Ok(Expr::IsNull {
                expr: Box::new(walk(expr, stmt, accs)?),
                negated: *negated,
            }),
            Expr::Between { expr, low, high } => Ok(Expr::Between {
                expr: Box::new(walk(expr, stmt, accs)?),
                low: Box::new(walk(low, stmt, accs)?),
                high: Box::new(walk(high, stmt, accs)?),
            }),
            Expr::InList {
                expr,
                list,
                negated,
            } => Ok(Expr::InList {
                expr: Box::new(walk(expr, stmt, accs)?),
                list: list
                    .iter()
                    .map(|x| walk(x, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
                negated: *negated,
            }),
            Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } => Ok(Expr::Func {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| walk(a, stmt, accs))
                    .collect::<Result<Vec<_>, _>>()?,
                distinct: *distinct,
                order_by: order_by.clone(),
                filter: filter.clone(),
            }),
            other => Ok(other.clone()),
        }
    }
    walk(expr, stmt, accs)
}

/// Variant of [`aggregate_join_rows`] used by the GROUPING SETS
/// dispatcher: projections that aren't in the active `group_by` are
/// emitted as NULL (matching `PostgreSQL`'s ROLLUP / CUBE semantics)
/// instead of raising an error.
fn aggregate_join_rows_relaxed(
    engine: &Engine,
    stmt: &SelectStmt,
    rows: &[ResultRow],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let agg_targets: Vec<&Projection> = stmt
        .projections
        .iter()
        .filter(|p| is_aggregate(&p.expr))
        .collect();
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    for row in rows {
        let ctx = uqa_sql::expr::EvalContext::new(Some(row), params).with_engine(engine);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = groups.entry(group_values.clone()).or_insert_with(|| {
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                group_values,
            )
        });
        for (i, proj) in agg_targets.iter().enumerate() {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = &proj.expr
            else {
                continue;
            };
            if let Some(filter_expr) = filter.as_deref() {
                let keep =
                    uqa_sql::expr::eval(filter_expr, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
                if !keep {
                    continue;
                }
            }
            let value = aggregate_input_value(name, args, order_by, &ctx)?;
            if *distinct && !matches!(value, Value::Null) {
                let key = distinct_key(&value);
                if !bucket.0[i].distinct.insert(key) {
                    continue;
                }
            }
            let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
            for ob in order_by {
                let v = uqa_sql::expr::eval(&ob.expr, &ctx)?;
                sort_keys.push((v, ob.descending));
            }
            if order_by.is_empty() {
                bucket.0[i].observe(&value);
            } else {
                bucket.0[i].observe_with_sort_keys(&value, sort_keys);
            }
        }
    }
    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                Vec::new(),
            ),
        );
    }
    let mut out = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if is_aggregate(&proj.expr) {
                let Expr::Func { name, args, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value_with_args(name, acc, args));
            } else {
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    row.insert(label, Value::Null);
                }
            }
        }
        out.push(row);
    }
    Ok(out)
}

fn exprs_match(lhs: &Expr, rhs: &Expr) -> bool {
    match (lhs, rhs) {
        (Expr::Star, Expr::Star) => true,
        (Expr::Column(a), Expr::Column(b)) => a == b,
        (
            Expr::QualifiedColumn {
                qualifier: aq,
                column: ac,
            },
            Expr::QualifiedColumn {
                qualifier: bq,
                column: bc,
            },
        ) => aq == bq && ac == bc,
        (Expr::Column(c), Expr::QualifiedColumn { column, .. })
        | (Expr::QualifiedColumn { column, .. }, Expr::Column(c)) => c == column,
        (Expr::Literal(a), Expr::Literal(b)) => literals_equal(a, b),
        (Expr::Param(a), Expr::Param(b)) => a == b,
        (
            Expr::Func {
                name: an,
                args: aa,
                distinct: ad,
                order_by: ao,
                filter: af,
            },
            Expr::Func {
                name: bn,
                args: ba,
                distinct: bd,
                order_by: bo,
                filter: bf,
            },
        ) => {
            an.eq_ignore_ascii_case(bn)
                && ad == bd
                && aa.len() == ba.len()
                && aa.iter().zip(ba.iter()).all(|(x, y)| exprs_match(x, y))
                && ao.len() == bo.len()
                && ao.iter().zip(bo.iter()).all(|(x, y)| {
                    x.descending == y.descending
                        && x.nulls == y.nulls
                        && exprs_match(&x.expr, &y.expr)
                })
                && match (af.as_deref(), bf.as_deref()) {
                    (None, None) => true,
                    (Some(x), Some(y)) => exprs_match(x, y),
                    _ => false,
                }
        }
        (
            Expr::Binary {
                op: ao,
                lhs: al,
                rhs: ar,
            },
            Expr::Binary {
                op: bo,
                lhs: bl,
                rhs: br,
            },
        ) => ao == bo && exprs_match(al, bl) && exprs_match(ar, br),
        (Expr::And(a), Expr::And(b)) | (Expr::Or(a), Expr::Or(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| exprs_match(x, y))
        }
        (Expr::Not(a), Expr::Not(b)) => exprs_match(a, b),
        _ => false,
    }
}

fn literals_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bytes(x), Value::Bytes(y)) => x == y,
        (Value::Temporal(x), Value::Temporal(y)) => x == y,
        _ => false,
    }
}

fn filter_table_rows(
    engine: &Engine,
    table: &str,
    filter: &Expr,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let mut out = Vec::new();
    for doc_id in engine.table_doc_ids(table) {
        let document = engine.get_document(table, doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
        let v = uqa_sql::expr::eval(filter, &ctx)?;
        if uqa_sql::expr::truthy(&v) {
            out.push(ScoredEntry { doc_id, score: 0.0 });
        }
    }
    Ok(out)
}

fn has_aggregate(projections: &[Projection]) -> bool {
    projections.iter().any(|p| is_aggregate(&p.expr))
}

fn is_aggregate(expr: &Expr) -> bool {
    matches!(expr, Expr::Func { name, .. } if matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "bool_and"
            | "bool_or"
            | "stddev"
            | "stddev_samp"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
            | "json_object_agg"
            | "jsonb_object_agg"
    ))
}

fn aggregate_input_value(
    name: &str,
    args: &[Expr],
    order_by: &[OrderBy],
    ctx: &EvalContext<'_>,
) -> Result<Value, SQLError> {
    match (name.to_ascii_lowercase().as_str(), args) {
        ("count", [Expr::Star]) | ("count", []) => Ok(Value::Int(1)),
        // Ordered-set aggregates: the percentile / mode fraction is a
        // direct positional argument; the value to fold comes from
        // `WITHIN GROUP (ORDER BY ...)` which the compiler parks in
        // `order_by[0]`.
        ("percentile_cont" | "percentile_disc" | "mode", _) => order_by
            .first()
            .map(|ob| uqa_sql::expr::eval(&ob.expr, ctx))
            .transpose()
            .map(|v| v.unwrap_or(Value::Null)),
        ("json_object_agg" | "jsonb_object_agg", [key_expr, value_expr]) => {
            let key = uqa_sql::expr::eval(key_expr, ctx)?;
            if matches!(key, Value::Null) {
                return Ok(Value::Null);
            }
            let value = uqa_sql::expr::eval(value_expr, ctx)?;
            Ok(Value::List(vec![key, value]))
        }
        ("json_object_agg" | "jsonb_object_agg", _) => Err(SQLError::TypeMismatch(format!(
            "{name} requires 2 arguments"
        ))),
        (_, args) => {
            let arg = args
                .first()
                .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
            uqa_sql::expr::eval(arg, ctx)
        }
    }
}

#[derive(Default)]
struct AggregateAccumulator {
    count: u64,
    sum: f64,
    min: Option<Value>,
    max: Option<Value>,
    /// Distinct-bookkeeping. Filled by the dispatcher when the
    /// aggregate was annotated with `DISTINCT`. Holds canonical-form
    /// keys so `Int(1)` and `Float(1.0)` collapse to the same bucket.
    distinct: std::collections::BTreeSet<String>,
    /// Every observed (non-null) value for collection-style
    /// aggregates (`STRING_AGG`, `ARRAY_AGG`, statistical aggregates,
    /// percentile / mode). Sort keys for ordered aggregates land in
    /// `sort_keys` parallel to this vector.
    values: Vec<Value>,
    /// Optional sort key per `values` entry, packed as a `Vec<(key,
    /// descending)>` so multi-key ORDER BY composes lexicographically.
    sort_keys: Vec<Vec<(Value, bool)>>,
    /// Boolean folds for `BOOL_AND` / `BOOL_OR`. Stay `None` until the
    /// first observation so an empty input set returns `NULL` (matches
    /// `PostgreSQL`).
    bool_and: Option<bool>,
    bool_or: Option<bool>,
}

impl AggregateAccumulator {
    fn observe(&mut self, value: &Value) {
        if matches!(value, Value::Null) {
            return;
        }
        self.count += 1;
        if let Ok(f) = value_as_f64(value) {
            self.sum += f;
        }
        match &self.min {
            Some(cur) if !value_lt(value, cur) => {}
            _ => self.min = Some(value.clone()),
        }
        match &self.max {
            Some(cur) if !value_gt(value, cur) => {}
            _ => self.max = Some(value.clone()),
        }
        self.values.push(value.clone());
        self.sort_keys.push(Vec::new());
        if let Value::Bool(b) = value {
            self.bool_and = Some(self.bool_and.unwrap_or(true) && *b);
            self.bool_or = Some(self.bool_or.unwrap_or(false) || *b);
        }
    }

    fn observe_with_sort_keys(&mut self, value: &Value, keys: Vec<(Value, bool)>) {
        if matches!(value, Value::Null) {
            return;
        }
        self.count += 1;
        if let Ok(f) = value_as_f64(value) {
            self.sum += f;
        }
        match &self.min {
            Some(cur) if !value_lt(value, cur) => {}
            _ => self.min = Some(value.clone()),
        }
        match &self.max {
            Some(cur) if !value_gt(value, cur) => {}
            _ => self.max = Some(value.clone()),
        }
        self.values.push(value.clone());
        self.sort_keys.push(keys);
        if let Value::Bool(b) = value {
            self.bool_and = Some(self.bool_and.unwrap_or(true) && *b);
            self.bool_or = Some(self.bool_or.unwrap_or(false) || *b);
        }
    }
}

/// Canonical-form key for `DISTINCT` deduplication. Mirrors the
/// approach in `uqa_execution::relational::distinct_key`.
fn distinct_key(v: &Value) -> String {
    match v {
        Value::Null => "\x00".into(),
        Value::Bool(b) => format!("b:{b}"),
        Value::Int(n) => format!("i:{n}"),
        Value::Float(f) => format!("f:{f}"),
        Value::Str(s) => format!("s:{s}"),
        Value::Bytes(b) => format!("y:{}", b.len()),
        Value::Temporal(t) => format!("t:{}", t.to_sql_string()),
        other => format!("o:{other:?}"),
    }
}

fn value_as_f64(v: &Value) -> Result<f64, SQLError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

fn value_lt(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x < y,
        (Value::Float(x), Value::Float(y)) => x < y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) < *y,
        (Value::Float(x), Value::Int(y)) => *x < (*y as f64),
        (Value::Str(x), Value::Str(y)) => x < y,
        (Value::Temporal(x), Value::Temporal(y)) => x < y,
        _ => false,
    }
}

fn value_gt(a: &Value, b: &Value) -> bool {
    value_lt(b, a)
}

fn build_aggregate_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // GROUPING SETS / ROLLUP / CUBE: run the aggregator per set, then
    // mask out columns not in the active set with NULL.
    if !stmt.grouping_sets.is_empty() {
        let mut combined: Vec<ResultRow> = Vec::new();
        let labels = projection_columns(&stmt.projections);
        for set in &stmt.grouping_sets {
            let mut sub = stmt.clone();
            sub.group_by.clone_from(set);
            sub.grouping_sets = Vec::new();
            let part = build_aggregate_rows_relaxed(engine, table, scored, &sub, params)?;
            for mut row in part {
                for (idx, proj) in stmt.projections.iter().enumerate() {
                    let label = labels[idx].clone();
                    if is_aggregate(&proj.expr) {
                        continue;
                    }
                    let in_set = set.iter().any(|g| exprs_match(&proj.expr, g));
                    if !in_set {
                        row.insert(label, Value::Null);
                    }
                }
                combined.push(row);
            }
        }
        return Ok(combined);
    }
    // group_key -> per-aggregate accumulator vector + the raw group key
    // values used to project the GROUP BY columns.
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    let agg_targets: Vec<&Projection> = stmt
        .projections
        .iter()
        .filter(|p| is_aggregate(&p.expr))
        .collect();

    for entry in scored {
        let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = groups.entry(group_values.clone()).or_insert_with(|| {
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                group_values,
            )
        });
        for (i, proj) in agg_targets.iter().enumerate() {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = &proj.expr
            else {
                continue;
            };
            if let Some(filter_expr) = filter.as_deref() {
                let keep =
                    uqa_sql::expr::eval(filter_expr, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
                if !keep {
                    continue;
                }
            }
            let value = aggregate_input_value(name, args, order_by, &ctx)?;
            if *distinct && !matches!(value, Value::Null) {
                let key = distinct_key(&value);
                if !bucket.0[i].distinct.insert(key) {
                    continue;
                }
            }
            let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
            for ob in order_by {
                let v = uqa_sql::expr::eval(&ob.expr, &ctx)?;
                sort_keys.push((v, ob.descending));
            }
            if order_by.is_empty() {
                bucket.0[i].observe(&value);
            } else {
                bucket.0[i].observe_with_sort_keys(&value, sort_keys);
            }
        }
    }

    if groups.is_empty() && stmt.group_by.is_empty() {
        // SELECT count(*) FROM t with no rows still produces a row of
        // zeros so downstream consumers see a stable shape.
        groups.insert(
            Vec::new(),
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                Vec::new(),
            ),
        );
    }

    let mut rows: Vec<ResultRow> = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if is_aggregate(&proj.expr) {
                let Expr::Func { name, args, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value_with_args(name, acc, args));
            } else {
                // Match a non-aggregate projection against the GROUP BY
                // key list using `exprs_match`, which understands both
                // bare column refs and complex expressions.
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if exprs_match(&proj.expr, g_expr) {
                        row.insert(label.clone(), g_value.clone());
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    return Err(SQLError::Unsupported(format!(
                        "non-aggregated projection `{label}` must appear in GROUP BY"
                    )));
                }
            }
        }
        if let Some(having_expr) = stmt.having.as_ref() {
            let resolved = resolve_having(having_expr, &row, stmt, &accs, &group_values, params)?;
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
            let kept =
                uqa_sql::expr::eval(&resolved, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
            if !kept {
                continue;
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Single-table aggregator variant used by the GROUPING SETS
/// dispatcher: projections that aren't in the active `group_by` come
/// out as NULL instead of erroring (`PostgreSQL` ROLLUP / CUBE
/// semantics).
fn build_aggregate_rows_relaxed(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    stmt: &SelectStmt,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut groups: BTreeMap<Vec<Value>, (Vec<AggregateAccumulator>, Vec<Value>)> = BTreeMap::new();
    let agg_targets: Vec<&Projection> = stmt
        .projections
        .iter()
        .filter(|p| is_aggregate(&p.expr))
        .collect();
    for entry in scored {
        let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        let ctx = uqa_sql::expr::EvalContext::new(Some(&document), params).with_engine(engine);
        let group_values: Vec<Value> = stmt
            .group_by
            .iter()
            .map(|g| uqa_sql::expr::eval(g, &ctx))
            .collect::<Result<Vec<_>, _>>()?;
        let bucket = groups.entry(group_values.clone()).or_insert_with(|| {
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                group_values,
            )
        });
        for (i, proj) in agg_targets.iter().enumerate() {
            let Expr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } = &proj.expr
            else {
                continue;
            };
            if let Some(filter_expr) = filter.as_deref() {
                let keep =
                    uqa_sql::expr::eval(filter_expr, &ctx).is_ok_and(|v| uqa_sql::expr::truthy(&v));
                if !keep {
                    continue;
                }
            }
            let value = aggregate_input_value(name, args, order_by, &ctx)?;
            if *distinct && !matches!(value, Value::Null) {
                let key = distinct_key(&value);
                if !bucket.0[i].distinct.insert(key) {
                    continue;
                }
            }
            let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
            for ob in order_by {
                let v = uqa_sql::expr::eval(&ob.expr, &ctx)?;
                sort_keys.push((v, ob.descending));
            }
            if order_by.is_empty() {
                bucket.0[i].observe(&value);
            } else {
                bucket.0[i].observe_with_sort_keys(&value, sort_keys);
            }
        }
    }
    if groups.is_empty() && stmt.group_by.is_empty() {
        groups.insert(
            Vec::new(),
            (
                (0..agg_targets.len())
                    .map(|_| AggregateAccumulator::default())
                    .collect(),
                Vec::new(),
            ),
        );
    }
    let mut rows: Vec<ResultRow> = Vec::with_capacity(groups.len());
    let labels = projection_columns(&stmt.projections);
    for (_, (accs, group_values)) in groups {
        let mut row = ResultRow::new();
        let mut agg_idx = 0;
        for (idx, proj) in stmt.projections.iter().enumerate() {
            let label = labels[idx].clone();
            if is_aggregate(&proj.expr) {
                let Expr::Func { name, args, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value_with_args(name, acc, args));
            } else if let Expr::Column(col) = &proj.expr {
                let mut placed = false;
                for (g_expr, g_value) in stmt.group_by.iter().zip(&group_values) {
                    if let Expr::Column(g_col) = g_expr {
                        if g_col == col {
                            row.insert(label.clone(), g_value.clone());
                            placed = true;
                            break;
                        }
                    }
                }
                if !placed {
                    row.insert(label, Value::Null);
                }
            } else {
                // Complex non-aggregate projections in ROLLUP / CUBE
                // also fall back to NULL.
                row.insert(label, Value::Null);
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn aggregate_value(name: &str, acc: &AggregateAccumulator) -> Value {
    aggregate_value_with_args(name, acc, &[])
}

fn aggregate_value_with_args(name: &str, acc: &AggregateAccumulator, args: &[Expr]) -> Value {
    let lname = name.to_ascii_lowercase();
    // Order the collected `values` by the captured sort keys when the
    // aggregate was annotated with ORDER BY (string_agg / array_agg /
    // percentile_*). This is a stable sort so equal keys preserve
    // insertion order, matching PostgreSQL.
    let ordered_values: Vec<Value> = if acc.sort_keys.iter().any(|k| !k.is_empty()) {
        let mut indexed: Vec<usize> = (0..acc.values.len()).collect();
        indexed.sort_by(|a, b| {
            let ak = &acc.sort_keys[*a];
            let bk = &acc.sort_keys[*b];
            for ((av, ad), (bv, _bd)) in ak.iter().zip(bk.iter()) {
                let cmp = av.cmp(bv);
                let cmp = if *ad { cmp.reverse() } else { cmp };
                if cmp != std::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            std::cmp::Ordering::Equal
        });
        indexed.into_iter().map(|i| acc.values[i].clone()).collect()
    } else {
        acc.values.clone()
    };

    match lname.as_str() {
        "count" => Value::Int(acc.count as i64),
        "sum" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.values.iter().all(|v| matches!(v, Value::Int(_))) {
                Value::Int(acc.sum as i64)
            } else {
                Value::Float(acc.sum)
            }
        }
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        "string_agg" => {
            if ordered_values.is_empty() {
                return Value::Null;
            }
            // Separator: literal second positional arg, or empty.
            let sep = match args.get(1) {
                Some(Expr::Literal(Value::Str(s))) => s.clone(),
                _ => String::new(),
            };
            let parts: Vec<String> = ordered_values
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    Value::Int(n) => Some(n.to_string()),
                    Value::Float(f) => Some(f.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Temporal(t) => Some(t.to_sql_string()),
                    _ => None,
                })
                .collect();
            Value::Str(parts.join(&sep))
        }
        "array_agg" => {
            if ordered_values.is_empty() {
                return Value::Null;
            }
            Value::List(ordered_values)
        }
        "json_object_agg" | "jsonb_object_agg" => {
            let mut map = BTreeMap::new();
            for value in ordered_values {
                let Value::List(pair) = value else {
                    continue;
                };
                if pair.len() != 2 || matches!(pair[0], Value::Null) {
                    continue;
                }
                map.insert(aggregate_json_key(&pair[0]), pair[1].clone());
            }
            if map.is_empty() {
                Value::Null
            } else {
                Value::Map(map)
            }
        }
        "bool_and" => match acc.bool_and {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "bool_or" => match acc.bool_or {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "stddev" | "stddev_samp" => {
            if acc.count < 2 {
                return Value::Null;
            }
            Value::Float(stddev_samp(&acc.values))
        }
        "stddev_pop" => {
            if acc.count == 0 {
                return Value::Null;
            }
            Value::Float(stddev_pop(&acc.values))
        }
        "variance" | "var_samp" => {
            if acc.count < 2 {
                return Value::Null;
            }
            Value::Float(variance_samp(&acc.values))
        }
        "var_pop" => {
            if acc.count == 0 {
                return Value::Null;
            }
            Value::Float(variance_pop(&acc.values))
        }
        "percentile_cont" => {
            if ordered_values.is_empty() {
                return Value::Null;
            }
            let frac = match args.first() {
                Some(Expr::Literal(Value::Float(f))) => *f,
                Some(Expr::Literal(Value::Int(n))) => *n as f64,
                _ => 0.5,
            };
            Value::Float(percentile_cont(&ordered_values, frac))
        }
        "percentile_disc" => {
            if ordered_values.is_empty() {
                return Value::Null;
            }
            let frac = match args.first() {
                Some(Expr::Literal(Value::Float(f))) => *f,
                Some(Expr::Literal(Value::Int(n))) => *n as f64,
                _ => 0.5,
            };
            percentile_disc(&ordered_values, frac)
        }
        "mode" => mode_value(&ordered_values),
        _ => Value::Null,
    }
}

fn aggregate_json_key(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

fn mean(values: &[Value]) -> f64 {
    let nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.is_empty() {
        0.0
    } else {
        nums.iter().sum::<f64>() / nums.len() as f64
    }
}

fn variance_samp(values: &[Value]) -> f64 {
    let nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.len() < 2 {
        return 0.0;
    }
    let m = mean(values);
    let total: f64 = nums.iter().map(|x| (x - m).powi(2)).sum();
    total / (nums.len() as f64 - 1.0)
}

fn variance_pop(values: &[Value]) -> f64 {
    let nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.is_empty() {
        return 0.0;
    }
    let m = mean(values);
    let total: f64 = nums.iter().map(|x| (x - m).powi(2)).sum();
    total / nums.len() as f64
}

fn stddev_samp(values: &[Value]) -> f64 {
    variance_samp(values).sqrt()
}

fn stddev_pop(values: &[Value]) -> f64 {
    variance_pop(values).sqrt()
}

fn percentile_cont(values: &[Value], frac: f64) -> f64 {
    let mut nums: Vec<f64> = values.iter().filter_map(|v| value_as_f64(v).ok()).collect();
    if nums.is_empty() {
        return 0.0;
    }
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let frac = frac.clamp(0.0, 1.0);
    let pos = frac * (nums.len() as f64 - 1.0);
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return nums[lo];
    }
    let weight = pos - lo as f64;
    nums[lo] * (1.0 - weight) + nums[hi] * weight
}

fn percentile_disc(values: &[Value], frac: f64) -> Value {
    let mut sorted: Vec<&Value> = values.iter().collect();
    sorted.sort();
    if sorted.is_empty() {
        return Value::Null;
    }
    let frac = frac.clamp(0.0, 1.0);
    // PostgreSQL: smallest rank where cumulative cum_dist >= frac.
    let n = sorted.len();
    let mut idx = (frac * n as f64).ceil() as usize;
    if idx == 0 {
        idx = 1;
    }
    if idx > n {
        idx = n;
    }
    sorted[idx - 1].clone()
}

fn mode_value(values: &[Value]) -> Value {
    use std::collections::BTreeMap;
    if values.is_empty() {
        return Value::Null;
    }
    let mut counts: BTreeMap<String, (Value, u64)> = BTreeMap::new();
    for v in values {
        let key = distinct_key(v);
        let entry = counts.entry(key).or_insert((v.clone(), 0));
        entry.1 += 1;
    }
    counts
        .into_values()
        .max_by_key(|(_, n)| *n)
        .map_or(Value::Null, |(v, _)| v)
}

/// Compute a projection's output column name. The position is folded
/// into the fallback so two unaliased expressions in the same SELECT
/// don't collide on a single key.
fn projection_label_at(proj: &Projection, position: usize) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        Expr::Column(c) => c.clone(),
        Expr::QualifiedColumn { column, .. } => column.clone(),
        Expr::Star => "*".into(),
        Expr::Func { name, .. } => name.clone(),
        _ => format!("expr_{position}"),
    }
}

fn apply_row_order_limit(
    rows: Vec<ResultRow>,
    stmt: &SelectStmt,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    // Build a Volcano sub-pipeline:  Sort? -> Limit?  on top of an
    // in-memory TableScan over the rows the caller already projected.
    use uqa_execution::physical::{run_to_rows, PhysicalOperator};
    use uqa_execution::relational::{Limit, Sort, SortKey};
    use uqa_execution::scan::TableScan;

    if rows.is_empty() {
        return Ok(rows);
    }
    let resolved_offset = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")?;
    let resolved_limit = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")?;
    if stmt.order_by.is_empty() && resolved_offset.is_none() && resolved_limit.is_none() {
        return Ok(rows);
    }

    let columns: Vec<String> = rows
        .first()
        .map(|r| r.keys().cloned().collect())
        .unwrap_or_default();

    let mut op: Box<dyn PhysicalOperator> = Box::new(TableScan::from_rows(columns, rows));

    if !stmt.order_by.is_empty() {
        let keys: Vec<SortKey> = stmt
            .order_by
            .iter()
            .map(|o| SortKey {
                expr: o.expr.clone(),
                descending: o.descending,
                nulls_first: o
                    .nulls
                    .map(|n| matches!(n, uqa_sql::ast::NullsOrder::First)),
            })
            .collect();
        op = Box::new(Sort::new(op, keys, params.to_vec()));
    }

    if resolved_offset.is_some() || resolved_limit.is_some() {
        op = Box::new(Limit::new(op, resolved_offset.unwrap_or(0), resolved_limit));
    }

    let (_cols, rows) = run_to_rows(op.as_mut()).map_err(|e| match e {
        uqa_execution::physical::ExecError::SQL(err) => err,
        uqa_execution::physical::ExecError::Other(msg) => SQLError::Internal(msg),
    })?;
    Ok(rows)
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Temporal(y)) => x.cmp(y),
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind(y)
            .map_or(Ordering::Equal, |parsed| x.cmp(&parsed)),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind(x)
            .map_or(Ordering::Equal, |parsed| parsed.cmp(y)),
        _ => Ordering::Equal,
    }
}

fn execute_function(
    engine: &Engine,
    table: &str,
    name: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.to_string()))?;
    match kind {
        FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
            run_text_match(engine, table, args, params)
        }
        FunctionKind::FTSMatch => run_fts_match(engine, table, args, params),
        FunctionKind::BayesianMatchWithPrior => {
            run_bayesian_match_with_prior(engine, table, args, params)
        }
        FunctionKind::SparseThreshold => run_sparse_threshold(engine, table, args, params),
        FunctionKind::KNNMatch => run_knn_match(engine, table, args, params),
        FunctionKind::FuseLogOdds => run_fuse_log_odds(engine, table, args, params),
        FunctionKind::GraphPagerank => run_graph_pagerank(engine, args, params),
        FunctionKind::GraphTraverse => run_graph_traverse(engine, args, params),
        FunctionKind::GraphNeighbors => run_graph_neighbors(engine, args, params),
        FunctionKind::MultiFieldMatch => run_multi_field_match(engine, table, args, params),
        FunctionKind::StagedRetrieval => run_staged_retrieval(engine, table, args, params),
        FunctionKind::DeepPredict => run_deep_predict(engine, args, params),
        FunctionKind::TraverseMatch => run_graph_traverse(engine, args, params),
        FunctionKind::TemporalTraverse => run_temporal_traverse(engine, args, params),
        FunctionKind::RPQ => run_rpq(engine, args, params),
        FunctionKind::GraphCreate => run_graph_create(engine, args, params),
        FunctionKind::GraphDrop => run_graph_drop(engine, args, params),
        FunctionKind::GraphEdges => run_graph_edges(engine, args, params),
        // The remaining UQA functions either return a non-posting
        // shape or are construction-time helpers; they reach the
        // projection-side handler instead of this row-emitting
        // dispatcher.
        FunctionKind::AttentionFusion | FunctionKind::LearnedFusion => {
            run_attention_fusion(engine, table, args, params)
        }
        FunctionKind::UQAHighlight
        | FunctionKind::UQAFacets
        | FunctionKind::CalibratedVectorMatch
        | FunctionKind::ScoreBM25
        | FunctionKind::ScoreBayesianBM25
        | FunctionKind::DeepLearn
        | FunctionKind::Convolve
        | FunctionKind::Pool
        | FunctionKind::Flatten
        | FunctionKind::Dense
        | FunctionKind::Softmax
        | FunctionKind::Layer
        | FunctionKind::Model => Err(SQLError::Unsupported(format!(
            "row-emitting dispatch for `{name}` is handled elsewhere"
        ))),
    }
}

fn run_deep_predict(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "deep_predict".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = match eval(&args[0], &ctx)? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_predict.model must be a string, got {other:?}"
            )));
        }
    };
    let scores = engine
        .deep_predict(&name)
        .ok_or_else(|| SQLError::Unsupported(format!("unknown model {name:?}")))?;
    Ok(scores
        .into_iter()
        .map(|(doc_id, score)| ScoredEntry { doc_id, score })
        .collect())
}

fn run_staged_retrieval(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if matches!(args.first(), Some(Expr::Func { .. })) && !is_named_arg_expr(&args[0]) {
        if args.is_empty() || args.len() % 2 != 0 {
            return Err(SQLError::BadArity {
                name: "staged_retrieval".into(),
                expected: "pairs of (signal, top_k)".into(),
                actual: args.len(),
            });
        }
        let ctx = EvalContext::new(None, params).with_engine(engine);
        let mut current: Option<Vec<ScoredEntry>> = None;
        for pair in args.chunks(2) {
            let rows = run_scored_signal(engine, table, &pair[0], params, "staged_retrieval")?;
            let top_k = expect_usize(&pair[1], "staged_retrieval.top_k", &ctx)?;
            let mut scored = rows;
            if let Some(prior) = &current {
                let prior_ids: std::collections::BTreeSet<u64> =
                    prior.iter().map(|e| e.doc_id).collect();
                scored.retain(|e| prior_ids.contains(&e.doc_id));
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(top_k);
            scored.sort_by_key(|e| e.doc_id);
            current = Some(scored);
        }
        return Ok(current.unwrap_or_default());
    }

    if args.is_empty() || args.len() % 3 != 0 {
        return Err(SQLError::BadArity {
            name: "staged_retrieval".into(),
            expected: ">= 3, multiple of 3 (field, query, top_k)".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
    let n_stages = args.len() / 3;
    let mut current: Option<Vec<ScoredEntry>> = None;
    for i in 0..n_stages {
        let field = expect_column_name(&args[3 * i], "staged_retrieval.field")?;
        let q = match eval(&args[3 * i + 1], &ctx)? {
            Value::Str(s) => s,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "staged_retrieval query must be string, got {other:?}"
                )));
            }
        };
        let top_k = expect_usize(&args[3 * i + 2], "staged_retrieval.top_k", &ctx)?;
        let mut scored = engine.search(table, &field, &q, &mode, usize::MAX);
        if let Some(prior) = &current {
            let prior_ids: std::collections::BTreeSet<u64> =
                prior.iter().map(|e| e.doc_id).collect();
            scored.retain(|e| prior_ids.contains(&e.doc_id));
        }
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored.sort_by_key(|e| e.doc_id);
        current = Some(scored);
    }
    Ok(current.unwrap_or_default())
}

fn run_multi_field_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 3 {
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: ">= 3 (fields..., query[, weights...])".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let (fields, queries, weights) = parse_multi_field_match_args(args, &ctx)?;
    let n_fields = fields.len();
    let mut per_doc: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for (i, (field, q)) in fields.iter().zip(queries.iter()).enumerate() {
        let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
        let scored = engine.search(table, field, q, &mode, usize::MAX);
        for entry in scored {
            let slot = per_doc
                .entry(entry.doc_id)
                .or_insert_with(|| vec![0.5; n_fields]);
            slot[i] = entry.score;
        }
    }
    // Pad missing slots so every doc has a full vector.
    for slot in per_doc.values_mut() {
        if slot.len() < n_fields {
            slot.resize(n_fields, 0.5);
        }
    }
    let mut out: Vec<ScoredEntry> = per_doc
        .into_iter()
        .map(|(doc_id, probs)| {
            let fused = if probs.len() == 1 {
                probs[0]
            } else {
                uqa_scoring::prob::log_odds_conjunction_weighted(&probs, &weights, 0.0)
                    .unwrap_or(0.5)
            };
            ScoredEntry {
                doc_id,
                score: fused,
            }
        })
        .collect();
    out.sort_by_key(|e| e.doc_id);
    Ok(out)
}

type MultiFieldMatchArgs = (Vec<String>, Vec<String>, Vec<f64>);

fn parse_multi_field_match_args(
    args: &[Expr],
    ctx: &EvalContext<'_>,
) -> Result<MultiFieldMatchArgs, SQLError> {
    let first_non_column = args.iter().position(|arg| !matches!(arg, Expr::Column(_)));
    if let Some(query_idx) = first_non_column {
        if query_idx >= 2 {
            let fields = args[..query_idx]
                .iter()
                .map(|arg| expect_column_name(arg, "multi_field_match.field"))
                .collect::<Result<Vec<_>, _>>()?;
            let query = expect_string_value(&args[query_idx], "multi_field_match.query", ctx)?;
            let weight_args = &args[query_idx + 1..];
            let weights = if weight_args.is_empty() {
                uniform_weights(fields.len())
            } else {
                if weight_args.len() != fields.len() {
                    return Err(SQLError::BadArity {
                        name: "multi_field_match".into(),
                        expected: "one weight per field".into(),
                        actual: weight_args.len(),
                    });
                }
                normalize_weights(
                    weight_args
                        .iter()
                        .map(|arg| expect_f64_value(arg, "multi_field_match.weight", ctx))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            };
            let queries = vec![query; fields.len()];
            return Ok((fields, queries, weights));
        }
    }

    if args.len() < 4 || args.len() % 2 != 0 {
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: ">= 3 (fields..., query[, weights...]) or even >= 4 (field, query pairs)"
                .into(),
            actual: args.len(),
        });
    }
    let n_fields = args.len() / 2;
    let mut fields = Vec::with_capacity(n_fields);
    let mut queries = Vec::with_capacity(n_fields);
    for i in 0..n_fields {
        fields.push(expect_column_name(&args[2 * i], "multi_field_match.field")?);
        queries.push(expect_string_value(
            &args[2 * i + 1],
            "multi_field_match.query",
            ctx,
        )?);
    }
    Ok((fields, queries, uniform_weights(n_fields)))
}

fn expect_string_value(
    expr: &Expr,
    label: &str,
    ctx: &EvalContext<'_>,
) -> Result<String, SQLError> {
    match eval(expr, ctx)? {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be string, got {other:?}"
        ))),
    }
}

fn expect_f64_value(expr: &Expr, label: &str, ctx: &EvalContext<'_>) -> Result<f64, SQLError> {
    match eval(expr, ctx)? {
        Value::Float(f) => Ok(f),
        Value::Int(i) => Ok(i as f64),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be numeric, got {other:?}"
        ))),
    }
}

fn uniform_weights(n: usize) -> Vec<f64> {
    vec![1.0 / n.max(1) as f64; n]
}

fn normalize_weights(weights: Vec<f64>) -> Vec<f64> {
    let total: f64 = weights.iter().sum();
    if total > 0.0 {
        weights.into_iter().map(|w| w / total).collect()
    } else {
        uniform_weights(weights.len())
    }
}

fn run_graph_pagerank(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_pagerank".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_pagerank.graph", &ctx)?;
    let entries = engine
        .graph_with(&name, |store| {
            uqa_graph::PageRank::new(&name)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

fn run_graph_traverse(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "graph_traverse".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_traverse.graph", &ctx)?;
    let start = expect_u64(&args[1], "graph_traverse.start", &ctx)?;
    let label = expect_optional_string(&args[2], "graph_traverse.label", &ctx)?;
    let max_hops = expect_u32(&args[3], "graph_traverse.max_hops", &ctx)?;
    let entries = engine
        .graph_with(&name, |store| {
            let mut traverse = uqa_graph::Traverse::new(start, &name).max_hops(max_hops);
            if let Some(l) = label.as_deref() {
                traverse = traverse.label(l);
            }
            traverse
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    Ok(entries)
}

fn run_graph_neighbors(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "graph_neighbors".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_neighbors.graph", &ctx)?;
    let vertex = expect_u64(&args[1], "graph_neighbors.vertex", &ctx)?;
    let label = expect_optional_string(&args[2], "graph_neighbors.label", &ctx)?;
    let direction_str = expect_string(&args[3], "graph_neighbors.direction", &ctx)?;
    let direction = match direction_str.to_ascii_lowercase().as_str() {
        "out" => uqa_graph::Direction::Out,
        "in" => uqa_graph::Direction::In,
        "both" => uqa_graph::Direction::Both,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "graph_neighbors.direction must be 'out'/'in'/'both', got {other:?}"
            )));
        }
    };
    let neighbors = engine
        .graph_with(&name, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::neighbors(
                store,
                vertex,
                label.as_deref(),
                direction,
                &name,
            )
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for nid in neighbors {
        if seen.insert(nid) {
            out.push(ScoredEntry {
                doc_id: nid,
                score: 1.0,
            });
        }
    }
    Ok(out)
}

fn run_graph_create(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_create".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_create.name", &ctx)?;
    engine.create_graph(name);
    Ok(Vec::new())
}

fn run_graph_drop(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_drop".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_drop.name", &ctx)?;
    engine.drop_graph(&name);
    Ok(Vec::new())
}

/// `graph_edges(graph [, label])` -- emit one entry per edge in the
/// named graph. The `doc_id` carries the edge id; the score is the
/// raw edge weight (`1.0` when no `weight` property is present).
fn run_graph_edges(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.is_empty() || args.len() > 2 {
        return Err(SQLError::BadArity {
            name: "graph_edges".into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "graph_edges.graph", &ctx)?;
    let label = if args.len() == 2 {
        expect_optional_string(&args[1], "graph_edges.label", &ctx)?
    } else {
        None
    };
    let edges = engine
        .graph_with(&name, |store| {
            <uqa_graph::MemoryGraphStore as uqa_graph::GraphStore>::edges_in_graph(store, &name)
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    let mut out = Vec::new();
    for edge in edges {
        if let Some(target_label) = label.as_deref() {
            if edge.label != target_label {
                continue;
            }
        }
        let weight = match edge.properties.get("weight") {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            _ => 1.0,
        };
        out.push(ScoredEntry {
            doc_id: edge.edge_id,
            score: weight,
        });
    }
    Ok(out)
}

/// `temporal_traverse(graph, start, label, max_hops, t_min, t_max)`
/// -- BFS traversal that respects edge `valid_from` / `valid_to`
/// properties. Emits `(vertex_id, score)` weighted by hop distance,
/// matching the canonical UQA behavior's shape.
fn run_temporal_traverse(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 6 {
        return Err(SQLError::BadArity {
            name: "temporal_traverse".into(),
            expected: "6".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let name = expect_string(&args[0], "temporal_traverse.graph", &ctx)?;
    let start = expect_u64(&args[1], "temporal_traverse.start", &ctx)?;
    let label = expect_optional_string(&args[2], "temporal_traverse.label", &ctx)?;
    let max_hops = expect_usize(&args[3], "temporal_traverse.max_hops", &ctx)?;
    let t_min = match eval(&args[4], &ctx)? {
        Value::Int(n) => n as f64,
        Value::Float(f) => f,
        Value::Null => f64::NEG_INFINITY,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "temporal_traverse.t_min must be numeric, got {other:?}"
            )));
        }
    };
    let t_max = match eval(&args[5], &ctx)? {
        Value::Int(n) => n as f64,
        Value::Float(f) => f,
        Value::Null => f64::INFINITY,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "temporal_traverse.t_max must be numeric, got {other:?}"
            )));
        }
    };
    let traversed = engine
        .graph_with(&name, |store| {
            use std::collections::VecDeque;
            use uqa_graph::GraphStore;
            let mut visited: std::collections::BTreeMap<u64, f64> =
                std::collections::BTreeMap::new();
            let mut queue: VecDeque<(u64, usize)> = VecDeque::new();
            queue.push_back((start, 0));
            visited.insert(start, 1.0);
            while let Some((v, depth)) = queue.pop_front() {
                if depth >= max_hops {
                    continue;
                }
                let edges = store.out_edge_ids(v, &name);
                for eid in edges {
                    let Some(edge) = store.get_edge(eid) else {
                        continue;
                    };
                    if let Some(target_label) = label.as_deref() {
                        if edge.label != target_label {
                            continue;
                        }
                    }
                    // Read the edge's temporal range; fall back to
                    // unbounded when the property is missing.
                    let edge_from = match edge.properties.get("valid_from") {
                        Some(Value::Int(n)) => *n as f64,
                        Some(Value::Float(f)) => *f,
                        _ => f64::NEG_INFINITY,
                    };
                    let edge_to = match edge.properties.get("valid_to") {
                        Some(Value::Int(n)) => *n as f64,
                        Some(Value::Float(f)) => *f,
                        _ => f64::INFINITY,
                    };
                    if edge_to < t_min || edge_from > t_max {
                        continue;
                    }
                    let nbr = edge.target_id;
                    let score = 1.0 / ((depth + 1) as f64 + 1.0);
                    if let std::collections::btree_map::Entry::Vacant(slot) = visited.entry(nbr) {
                        slot.insert(score);
                        queue.push_back((nbr, depth + 1));
                    }
                }
            }
            visited
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {name:?}")))?;
    let mut out: Vec<ScoredEntry> = traversed
        .into_iter()
        .map(|(v, score)| ScoredEntry { doc_id: v, score })
        .collect();
    out.sort_by_key(|e| e.doc_id);
    Ok(out)
}

/// `rpq(expr, start, graph)` - evaluate a Regular Path Query
/// (Definition 5.1.2). Mirrors the canonical UQA implementation's
/// `Engine.sql("SELECT * FROM rpq(expr, start, graph)")`.
fn run_rpq(
    engine: &Engine,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 3 {
        return Err(SQLError::BadArity {
            name: "rpq".into(),
            expected: "3".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let expr_str = expect_string(&args[0], "rpq.expr", &ctx)?;
    let start = expect_u64(&args[1], "rpq.start", &ctx)?;
    let graph = expect_string(&args[2], "rpq.graph", &ctx)?;
    let path =
        uqa_graph::parse_rpq(&expr_str).map_err(|e| SQLError::Unsupported(format!("{e:?}")))?;
    let entries = engine
        .graph_with(&graph, |store| {
            uqa_graph::RegularPathQuery::new(path, &graph)
                .from_vertex(start)
                .execute(store)
                .inner()
                .entries()
                .iter()
                .map(|e| ScoredEntry {
                    doc_id: e.doc_id,
                    score: e.payload.score,
                })
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| SQLError::Unsupported(format!("unknown graph {graph:?}")))?;
    Ok(entries)
}

fn expect_string(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<String, SQLError> {
    match eval(expr, ctx)? {
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string, got {other:?}"
        ))),
    }
}

fn expect_optional_string(
    expr: &Expr,
    name: &str,
    ctx: &EvalContext,
) -> Result<Option<String>, SQLError> {
    match eval(expr, ctx)? {
        Value::Null => Ok(None),
        Value::Str(s) if s.is_empty() => Ok(None),
        Value::Str(s) => Ok(Some(s)),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a string or NULL, got {other:?}"
        ))),
    }
}

fn expect_u64(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<u64, SQLError> {
    match eval(expr, ctx)? {
        Value::Int(n) if n >= 0 => Ok(n as u64),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must be a non-negative integer, got {other:?}"
        ))),
    }
}

pub(crate) fn run_text_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_text_match(engine, table, args, params)
}

pub(crate) fn run_knn_match_public(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    run_knn_match(engine, table, args, params)
}

fn expect_u32(expr: &Expr, name: &str, ctx: &EvalContext) -> Result<u32, SQLError> {
    let max_u32_as_i64: i64 = i64::from(u32::MAX);
    match eval(expr, ctx)? {
        Value::Int(n) if (0..=max_u32_as_i64).contains(&n) => Ok(n as u32),
        other => Err(SQLError::TypeMismatch(format!(
            "{name} must fit in u32, got {other:?}"
        ))),
    }
}

fn run_text_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "text_match".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let field = match &args[0] {
        Expr::Column(name) => name.clone(),
        Expr::QualifiedColumn { column, .. } => column.clone(),
        Expr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => "_all".to_string(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "text_match.field must be a column reference, got {other:?}"
            )));
        }
    };
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let query_value = eval(&args[1], &ctx)?;
    let query = match query_value {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "text_match query must be a string, got {other:?}"
            )));
        }
    };
    let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
    if field == "_all" || field.is_empty() {
        let mut by_doc: BTreeMap<DocId, f64> = BTreeMap::new();
        for field_name in engine.fts_fields_for_table(table) {
            for entry in engine.search(table, &field_name, &query, &mode, usize::MAX) {
                by_doc
                    .entry(entry.doc_id)
                    .and_modify(|score| *score = (*score).max(entry.score))
                    .or_insert(entry.score);
            }
        }
        return Ok(by_doc
            .into_iter()
            .map(|(doc_id, score)| ScoredEntry { doc_id, score })
            .collect());
    }
    Ok(engine.search(table, &field, &query, &mode, usize::MAX))
}

fn run_fts_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "fts_match".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let default_field = match &args[0] {
        Expr::Column(name) => Some(name.clone()),
        Expr::QualifiedColumn { column, .. } => Some(column.clone()),
        Expr::Literal(Value::Str(s)) if s.is_empty() || s == "_all" => None,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "fts_match.field must be a column reference, got {other:?}"
            )));
        }
    };
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let query = expect_string(&args[1], "fts_match.query", &ctx)?;
    let tokenizer = |_field: Option<&str>, phrase: &str| {
        phrase
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    uqa_sql::compile_fts_query_string(&query, default_field.as_deref(), &tokenizer)?;
    let expr = Expr::Func {
        name: "fts_match".into(),
        args: args.to_vec(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    Ok(
        crate::operator_tree_bridge::run_optimised(engine, table, Some(&expr), params)?
            .unwrap_or_default(),
    )
}

fn run_bayesian_match_with_prior(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 4 {
        return Err(SQLError::BadArity {
            name: "bayesian_match_with_prior".into(),
            expected: "4".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "bayesian_match_with_prior.field")?;
    let prior_field = expect_column_name(&args[2], "bayesian_match_with_prior.prior_field")?;
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let query = expect_string(&args[1], "bayesian_match_with_prior.query", &ctx)?;
    let mode = expect_string(&args[3], "bayesian_match_with_prior.mode", &ctx)?;

    let base = run_text_match(
        engine,
        table,
        &[Expr::Column(field), Expr::Literal(Value::Str(query))],
        params,
    )?;
    let prior_fn = prior_fn_for_mode(&mode, &prior_field)?;
    Ok(base
        .into_iter()
        .map(|entry| {
            let document = engine.get_document(table, entry.doc_id).unwrap_or_default();
            let prior = prior_fn(&document).clamp(1e-10, 1.0 - 1e-10);
            ScoredEntry {
                doc_id: entry.doc_id,
                score: combine_probability_with_prior(entry.score, prior),
            }
        })
        .collect())
}

fn prior_fn_for_mode(mode: &str, prior_field: &str) -> Result<uqa_scoring::PriorFn, SQLError> {
    match mode.to_ascii_lowercase().as_str() {
        "authority" => Ok(uqa_scoring::authority_prior(prior_field, None)),
        "recency" => Ok(uqa_scoring::recency_prior(prior_field, 30.0)),
        other => Err(SQLError::TypeMismatch(format!(
            "Unknown prior mode: {other}"
        ))),
    }
}

fn combine_probability_with_prior(probability: f64, prior: f64) -> f64 {
    let p = probability.clamp(1e-10, 1.0 - 1e-10);
    uqa_scoring::sigmoid(uqa_scoring::logit(p) + uqa_scoring::logit(prior))
}

fn named_arg_expr(expr: &Expr) -> Option<(&str, &Expr)> {
    let Expr::Func { name, args, .. } = expr else {
        return None;
    };
    if name != "__named_arg" || args.len() != 2 {
        return None;
    }
    let Expr::Literal(Value::Str(arg_name)) = &args[0] else {
        return None;
    };
    Some((arg_name.as_str(), &args[1]))
}

fn is_named_arg_expr(expr: &Expr) -> bool {
    named_arg_expr(expr).is_some()
}

fn run_scored_signal(
    engine: &Engine,
    table: &str,
    expr: &Expr,
    params: &[SQLParam],
    parent: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let Expr::Func {
        name, args: inner, ..
    } = expr
    else {
        return Err(SQLError::Unsupported(format!(
            "{parent} signal must be a function call"
        )));
    };
    match lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))? {
        FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
            run_text_match(engine, table, inner, params)
        }
        FunctionKind::FTSMatch => run_fts_match(engine, table, inner, params),
        FunctionKind::BayesianMatchWithPrior => {
            run_bayesian_match_with_prior(engine, table, inner, params)
        }
        FunctionKind::KNNMatch => run_knn_match(engine, table, inner, params),
        _ => Err(SQLError::Unsupported(format!(
            "function {name} cannot be nested under {parent}"
        ))),
    }
}

fn run_attention_fusion(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 2 {
        return Err(SQLError::BadArity {
            name: "fuse_attention".into(),
            expected: ">=2".into(),
            actual: args.len(),
        });
    }

    let mut score_maps: Vec<std::collections::BTreeMap<DocId, f64>> =
        Vec::with_capacity(args.len());
    let mut all_doc_ids = std::collections::BTreeSet::new();
    for arg in args {
        if is_named_arg_expr(arg) {
            continue;
        }
        let Expr::Func {
            name, args: inner, ..
        } = arg
        else {
            return Err(SQLError::Unsupported(
                "fuse_attention arguments must be function calls".into(),
            ));
        };
        let rows = match lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))? {
            FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
                run_text_match(engine, table, inner, params)?
            }
            FunctionKind::FTSMatch => run_fts_match(engine, table, inner, params)?,
            FunctionKind::BayesianMatchWithPrior => {
                run_bayesian_match_with_prior(engine, table, inner, params)?
            }
            FunctionKind::KNNMatch => run_knn_match(engine, table, inner, params)?,
            _ => {
                return Err(SQLError::Unsupported(format!(
                    "function {name} cannot be nested under fuse_attention"
                )));
            }
        };
        let mut map = std::collections::BTreeMap::new();
        for row in rows {
            all_doc_ids.insert(row.doc_id);
            map.insert(row.doc_id, row.score.clamp(1e-10, 1.0 - 1e-10));
        }
        score_maps.push(map);
    }

    if score_maps.is_empty() {
        return Err(SQLError::BadArity {
            name: "fuse_attention".into(),
            expected: ">=1 signal".into(),
            actual: 0,
        });
    }

    let n = score_maps.len() as f64;
    Ok(all_doc_ids
        .into_iter()
        .map(|doc_id| {
            let score = score_maps
                .iter()
                .map(|map| map.get(&doc_id).copied().unwrap_or(0.5))
                .sum::<f64>()
                / n;
            ScoredEntry { doc_id, score }
        })
        .collect())
}

fn run_sparse_threshold(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "sparse_threshold".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let Expr::Func {
        name, args: inner, ..
    } = &args[0]
    else {
        return Err(SQLError::Unsupported(
            "sparse_threshold source must be a function call".into(),
        ));
    };
    let rows = match lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))? {
        FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
            run_text_match(engine, table, inner, params)?
        }
        FunctionKind::BayesianMatchWithPrior => {
            run_bayesian_match_with_prior(engine, table, inner, params)?
        }
        FunctionKind::KNNMatch => run_knn_match(engine, table, inner, params)?,
        _ => {
            return Err(SQLError::Unsupported(format!(
                "function {name} cannot be nested under sparse_threshold"
            )));
        }
    };
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let threshold = expect_f64_value(&args[1], "sparse_threshold.threshold", &ctx)?;
    Ok(rows
        .into_iter()
        .filter_map(|entry| {
            let adjusted = entry.score - threshold;
            (adjusted > 0.0).then_some(ScoredEntry {
                doc_id: entry.doc_id,
                score: adjusted,
            })
        })
        .collect())
}

fn run_knn_match(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() != 3 {
        return Err(SQLError::BadArity {
            name: "knn_match".into(),
            expected: "3".into(),
            actual: args.len(),
        });
    }
    let field = expect_column_name(&args[0], "knn_match.field")?;
    let ctx = EvalContext::new(None, params).with_engine(engine);
    let vec_value = eval(&args[1], &ctx)?;
    let query_vector = value_to_vector(&vec_value)?;
    let k = expect_usize(&args[2], "knn_match.k", &ctx)?;
    Ok(engine.knn_search(table, &field, query_vector, k))
}

fn run_fuse_log_odds(
    engine: &Engine,
    table: &str,
    args: &[Expr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if args.len() < 2 {
        return Err(SQLError::BadArity {
            name: "fuse_log_odds".into(),
            expected: ">=2".into(),
            actual: args.len(),
        });
    }
    let mut alpha = 0.5;
    let mut score_maps: Vec<std::collections::BTreeMap<DocId, f64>> =
        Vec::with_capacity(args.len());
    let mut all_doc_ids = std::collections::BTreeSet::new();
    let ctx = EvalContext::new(None, params).with_engine(engine);
    for arg in args {
        if let Some((name, value_expr)) = named_arg_expr(arg) {
            if name.eq_ignore_ascii_case("alpha") {
                alpha = expect_f64_value(value_expr, "fuse_log_odds.alpha", &ctx)?;
            }
            continue;
        }
        match arg {
            Expr::Func {
                name, args: inner, ..
            } => {
                let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))?;
                let rows = match kind {
                    FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
                        if inner.len() != 2 {
                            return Err(SQLError::BadArity {
                                name: name.clone(),
                                expected: "2".into(),
                                actual: inner.len(),
                            });
                        }
                        run_text_match(engine, table, inner, params)?
                    }
                    FunctionKind::FTSMatch => run_fts_match(engine, table, inner, params)?,
                    FunctionKind::BayesianMatchWithPrior => {
                        run_bayesian_match_with_prior(engine, table, inner, params)?
                    }
                    FunctionKind::KNNMatch => {
                        if inner.len() != 3 {
                            return Err(SQLError::BadArity {
                                name: name.clone(),
                                expected: "3".into(),
                                actual: inner.len(),
                            });
                        }
                        run_knn_match(engine, table, inner, params)?
                    }
                    FunctionKind::FuseLogOdds => {
                        return Err(SQLError::Unsupported(
                            "nested fuse_log_odds is not supported".into(),
                        ));
                    }
                    FunctionKind::GraphPagerank
                    | FunctionKind::GraphTraverse
                    | FunctionKind::GraphNeighbors
                    | FunctionKind::MultiFieldMatch
                    | FunctionKind::StagedRetrieval
                    | FunctionKind::DeepPredict
                    | FunctionKind::UQAHighlight
                    | FunctionKind::UQAFacets
                    | FunctionKind::TraverseMatch
                    | FunctionKind::TemporalTraverse
                    | FunctionKind::RPQ
                    | FunctionKind::GraphCreate
                    | FunctionKind::GraphDrop
                    | FunctionKind::GraphEdges
                    | FunctionKind::AttentionFusion
                    | FunctionKind::LearnedFusion
                    | FunctionKind::CalibratedVectorMatch
                    | FunctionKind::ScoreBM25
                    | FunctionKind::ScoreBayesianBM25
                    | FunctionKind::SparseThreshold
                    | FunctionKind::DeepLearn
                    | FunctionKind::Convolve
                    | FunctionKind::Pool
                    | FunctionKind::Flatten
                    | FunctionKind::Dense
                    | FunctionKind::Softmax
                    | FunctionKind::Layer
                    | FunctionKind::Model => {
                        return Err(SQLError::Unsupported(format!(
                            "function {name} cannot be nested under fuse_log_odds"
                        )));
                    }
                };
                let mut map = std::collections::BTreeMap::new();
                for row in rows {
                    all_doc_ids.insert(row.doc_id);
                    map.insert(row.doc_id, row.score.clamp(1e-10, 1.0 - 1e-10));
                }
                score_maps.push(map);
            }
            Expr::Literal(Value::Float(v)) => {
                alpha = *v;
            }
            Expr::Literal(Value::Int(v)) => {
                alpha = *v as f64;
            }
            Expr::Literal(Value::Str(_)) => {
                // Compatibility with the canonical UQA implementation's optional gating string
                // argument. Gating is a fusion-layer concern; the SQL
                // engine keeps the same calibrated score semantics.
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "fuse_log_odds argument must be a function call, got {other:?}"
                )));
            }
        }
    }
    if score_maps.len() < 2 {
        return Err(SQLError::BadArity {
            name: "fuse_log_odds".into(),
            expected: ">=2 signal functions".into(),
            actual: score_maps.len(),
        });
    }
    let n = score_maps.len();
    Ok(all_doc_ids
        .into_iter()
        .map(|doc_id| {
            let probs: Vec<f64> = score_maps
                .iter()
                .map(|map| map.get(&doc_id).copied().unwrap_or(0.5))
                .collect();
            let score = if n == 1 {
                probs[0]
            } else {
                uqa_scoring::log_odds_conjunction(&probs, alpha)
            };
            ScoredEntry { doc_id, score }
        })
        .collect())
}

fn expect_column_name(expr: &Expr, label: &str) -> Result<String, SQLError> {
    match expr {
        Expr::Column(name) => Ok(name.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a column reference, got {other:?}"
        ))),
    }
}

fn expect_usize(expr: &Expr, label: &str, ctx: &EvalContext<'_>) -> Result<usize, SQLError> {
    let v = eval(expr, ctx)?;
    match v {
        Value::Int(n) if n >= 0 => Ok(n as usize),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!("{label} must be >= 0"))),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be an integer, got {other:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// Output assembly
// -------------------------------------------------------------------------

/// Render a `LIMIT` / `OFFSET` expression for the EXPLAIN output.
/// Integer literals show their value verbatim; anything else collapses
/// to `<expr>` because EXPLAIN runs without bound parameters.
fn explain_int_expr(expr: &Expr) -> String {
    match expr {
        Expr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}

/// Evaluate a `LIMIT` / `OFFSET` expression to a non-negative `u64`.
/// Mirrors the canonical UQA implementation's `_extract_int_value` - accepts integer constants,
/// `$N` parameter references, and any expression that the row-evaluator
/// can fold to an integer at execute time. Returns `None` when the
/// clause was absent.
fn resolve_limit_offset(
    expr: Option<&Expr>,
    engine: &Engine,
    params: &[SQLParam],
    label: &str,
) -> Result<Option<u64>, SQLError> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let ctx = uqa_sql::expr::EvalContext::new(None, params).with_engine(engine);
    let value = uqa_sql::expr::eval(expr, &ctx)?;
    match value {
        Value::Null => Ok(None),
        Value::Int(n) if n >= 0 => Ok(Some(n as u64)),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!(
            "{label} must be non-negative"
        ))),
        Value::Float(f) if f.is_finite() && f >= 0.0 && f.fract() == 0.0 => Ok(Some(f as u64)),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a non-negative integer, got {other:?}"
        ))),
    }
}

fn apply_order_limit(
    mut entries: Vec<ScoredEntry>,
    stmt: &SelectStmt,
    engine: &Engine,
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    if !stmt.order_by.is_empty() {
        // Phase 5 only honours `ORDER BY <score-alias|_score>`. Any
        // expression resolves to the entry's score in our limited
        // surface; richer ordering lands in Phase 6 with the proper
        // expression evaluator.
        let descending = stmt.order_by.iter().any(|o| o.descending);
        entries.sort_by(|a, b| {
            let cmp = a
                .score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal);
            if descending { cmp.reverse() } else { cmp }.then_with(|| a.doc_id.cmp(&b.doc_id))
        });
    }
    if let Some(offset) = resolve_limit_offset(stmt.offset.as_ref(), engine, params, "OFFSET")? {
        let off = offset as usize;
        if off >= entries.len() {
            entries.clear();
        } else {
            entries.drain(0..off);
        }
    }
    if let Some(limit) = resolve_limit_offset(stmt.limit.as_ref(), engine, params, "LIMIT")? {
        entries.truncate(limit as usize);
    }
    Ok(entries)
}

fn projection_columns(projections: &[Projection]) -> Vec<String> {
    let mut out = Vec::with_capacity(projections.len());
    for (idx, proj) in projections.iter().enumerate() {
        let base = projection_label_at(proj, idx);
        let mut label = base.clone();
        let mut suffix = 1usize;
        while out.iter().any(|existing: &String| existing == &label) {
            label = format!("{base}_{suffix}");
            suffix += 1;
        }
        out.push(label);
    }
    out
}

fn build_rows(
    engine: &Engine,
    table: &str,
    scored: &[ScoredEntry],
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::with_capacity(scored.len());
    for entry in scored {
        let mut document = engine.get_document(table, entry.doc_id).unwrap_or_default();
        document.insert(DOC_ID_COLUMN.into(), Value::Int(entry.doc_id as i64));
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        let row = build_projection_row(Some(engine), &document, projections, params)?;
        rows.push(row);
    }
    Ok(rows)
}

fn build_projection_row(
    engine: Option<&Engine>,
    document: &Document,
    projections: &[Projection],
    params: &[SQLParam],
) -> Result<ResultRow, SQLError> {
    let mut ctx = EvalContext::new(Some(document), params);
    if let Some(e) = engine {
        ctx = ctx.with_engine(e);
    }
    let labels = projection_columns(projections);
    let mut row = ResultRow::new();
    for (idx, proj) in projections.iter().enumerate() {
        let label = labels[idx].clone();
        if let Expr::Star = proj.expr {
            for (k, v) in document {
                if k.as_str() == SCORE_COLUMN || k.as_str() == DOC_ID_COLUMN {
                    continue;
                }
                row.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Engine-side registry hooks (uqa_highlight / graph_create /
        // graph_drop) need access to the engine; intercept them
        // before falling through to the scalar evaluator.
        if let Expr::Func { name, args, .. } = &proj.expr {
            if let Some(value) = engine_func_intercept(engine, name, args, document, params)? {
                row.insert(label, value);
                continue;
            }
        }
        let value = eval(&proj.expr, &ctx)?;
        row.insert(label, value);
    }
    Ok(row)
}

impl Engine {
    /// Run an arbitrary SQL statement against the engine. Phase 5
    /// supports the quickstart slice; statements outside the supported
    /// grammar return a structured `Unsupported` error.
    pub fn sql(&self, query: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        execute(self, query, params)
    }

    /// All doc ids on a table, used by the SELECT path when there is no
    /// WHERE clause.
    pub fn table_doc_ids(&self, table: &str) -> Vec<uqa_core::DocId> {
        let Some(t) = self.tables.read().get(table).cloned() else {
            return Vec::new();
        };
        let ids = t.document_store.read().doc_ids();
        ids
    }
}
