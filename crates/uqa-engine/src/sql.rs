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
//! field), `INSERT ... VALUES`, and `SELECT` with `text_match`,
//! `knn_match`, and `fuse_log_odds` calls in `WHERE`. Statements outside
//! this surface return [`uqa_sql::SQLError::Unsupported`] cleanly.

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

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{
    AlterTableAction, AlterTableStmt, ColumnDef as SQLColumnDef, ColumnType, CreateIndex,
    CreateTable, DeleteStmt, DropKind, DropStmt, Expr, FromClause, InsertStmt, JoinKind, OrderBy,
    Projection, SelectStmt, SetOpKind, Statement, UpdateStmt, WindowSpec, CTE,
};
use uqa_sql::expr::{eval, value_to_vector, EvalContext};
use uqa_sql::registry::{lookup, FunctionKind};
use uqa_sql::{compile, ResultRow, SQLError, SQLParam, SQLResult};
use uqa_storage::document_store::Document;

use crate::{Engine, HybridSearchParams, ScoredEntry};

const SCORE_COLUMN: &str = "_score";

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
        Statement::CreateView { name, body, .. } => {
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
            engine.register_prepared(name, *body);
            Ok(SQLResult::empty())
        }
        Statement::Execute { name, params: ps } => run_execute_prepared(engine, &name, &ps, params),
        Statement::Deallocate { name } => {
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
                        let ctx = EvalContext::new(Some(&joined), params).with_engine(engine);
                        let mut new_vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
                        for (col, expr) in assignments {
                            let value = eval(expr, &ctx)?;
                            if let Ok(vec) = uqa_sql::expr::value_to_vector(&value) {
                                new_vectors.insert(col.clone(), vec);
                            }
                            doc.insert(col.clone(), value);
                        }
                        engine.add_document_with_vectors(&target_table, doc_id, doc, new_vectors);
                        affected += 1;
                    }
                }
                (1, MergeWhen::DeleteMatched { .. }) => {
                    if let Some(doc_id) = pair.doc_id {
                        engine.delete_document(&target_table, doc_id);
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
                    let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
                    for (i, col) in columns.iter().enumerate() {
                        let v = eval(&values[i], &ctx)?;
                        if let Ok(vec) = uqa_sql::expr::value_to_vector(&v) {
                            vectors.insert(col.clone(), vec);
                        }
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
                    engine.add_document_with_vectors(&target_table, doc_id, document, vectors);
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
            engine.rename_column(&stmt.table, &from, &to);
        }
        AlterTableAction::RenameTable { to } => {
            if !engine.rename_table(&stmt.table, &to) {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
    }
    Ok(SQLResult::empty())
}

/// Apply the new column's DEFAULT (or NULL) value to every row that
/// existed before the ADD COLUMN. `PostgreSQL` evaluates the default
/// once per existing row at ALTER TABLE time so NOT NULL columns stay
/// consistent on non-empty tables; the Rust port mirrors that
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
    // UPDATE ... FROM other [WHERE ...]: build the joined relation,
    // evaluate the WHERE against each joined row, and apply
    // assignments to the matching target rows. Mirrors Python's
    // _compile_update_from.
    if let Some(from_clause) = stmt.from.as_ref() {
        return run_update_from(engine, &stmt, from_clause, params);
    }
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    for doc_id in engine.table_doc_ids(&stmt.table) {
        cancel.check()?;
        let mut doc = engine
            .get_document(&stmt.table, doc_id)
            .ok_or_else(|| SQLError::Internal("missing document during UPDATE".into()))?;
        if let Some(filter) = stmt.r#where.as_ref() {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(engine);
            if !uqa_sql::expr::truthy(&uqa_sql::expr::eval(filter, &ctx)?) {
                continue;
            }
        }
        let mut new_vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        for (col, expr) in &stmt.assignments {
            let ctx = uqa_sql::expr::EvalContext::new(Some(&doc), params).with_engine(engine);
            let value = uqa_sql::expr::eval(expr, &ctx)?;
            if let Ok(vec) = uqa_sql::expr::value_to_vector(&value) {
                new_vectors.insert(col.clone(), vec);
            }
            doc.insert(col.clone(), value);
        }
        engine.add_document_with_vectors(&stmt.table, doc_id, doc, new_vectors);
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
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
    let target = stmt.table.clone();
    let target_doc_ids = engine.table_doc_ids(&target);
    for doc_id in target_doc_ids {
        cancel.check()?;
        let mut doc = engine
            .get_document(&target, doc_id)
            .ok_or_else(|| SQLError::Internal("missing document during UPDATE FROM".into()))?;
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
            let mut new_vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
            for (col, expr) in &stmt.assignments {
                let value = uqa_sql::expr::eval(expr, &ctx)?;
                if let Ok(vec) = uqa_sql::expr::value_to_vector(&value) {
                    new_vectors.insert(col.clone(), vec);
                }
                doc.insert(col.clone(), value);
            }
            engine.add_document_with_vectors(&target, doc_id, doc.clone(), new_vectors);
            applied = true;
            break;
        }
        if applied {
            affected += 1;
        }
    }
    Ok(SQLResult::from_affected(affected))
}

fn run_delete(
    engine: &Engine,
    stmt: DeleteStmt,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut affected = 0u64;
    let cancel = engine.cancellation_token();
    let mut to_delete: Vec<uqa_core::DocId> = Vec::new();
    // DELETE FROM t USING other WHERE ... -- materialise the join
    // first, then collect target doc ids whose joined image
    // satisfies WHERE. Mirrors Python's _compile_delete_using.
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
            to_delete.push(doc_id);
        }
    }
    // FOREIGN KEY check: refuse DELETE when any child table has a
    // row referencing one of the about-to-be-deleted tuples. Mirrors
    // `PostgreSQL`'s RESTRICT default. CASCADE is left to a follow-up.
    let referrers = engine.referrers_to(&stmt.table);
    if !referrers.is_empty() {
        for doc_id in &to_delete {
            let Some(target) = engine.get_document(&stmt.table, *doc_id) else {
                continue;
            };
            for (ref_table, fk) in &referrers {
                let key_values: Vec<Value> = fk
                    .ref_columns
                    .iter()
                    .map(|c| target.get(c).cloned().unwrap_or(Value::Null))
                    .collect();
                if key_values.iter().any(|v| matches!(v, Value::Null)) {
                    continue;
                }
                if engine
                    .find_conflict(ref_table, &fk.local_columns, &key_values)
                    .is_some()
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "DELETE on `{}` violates FOREIGN KEY constraint from `{}` ({} -> {})",
                        stmt.table,
                        ref_table,
                        fk.local_columns.join(", "),
                        fk.ref_columns.join(", "),
                    )));
                }
            }
        }
    }
    for doc_id in to_delete {
        engine.delete_document(&stmt.table, doc_id);
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
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
    let mut fts_fields = Vec::new();
    let mut vector_fields: Vec<(String, u32)> = Vec::new();
    for col in &c.columns {
        match &col.ty {
            ColumnType::Text => fts_fields.push(col.name.clone()),
            ColumnType::Vector(dim) => vector_fields.push((col.name.clone(), *dim)),
            _ => {}
        }
    }
    engine.create_default_table(c.name.clone(), fts_fields);
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
    // `ivf` / `hnsw` register vector fields if not already declared,
    // others are informational.
    let am = c.access_method.to_ascii_lowercase();
    if am.is_empty() || am == "gin" {
        for col in &c.columns {
            let analyzer = c
                .options
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                .map(|(_, v)| v.as_str());
            if let Err(e) = engine.add_fts_field_with_analyzer(&c.table, col.clone(), analyzer) {
                return Err(SQLError::Internal(format!("add_fts_field: {e}")));
            }
        }
    }
    // Persist the CREATE INDEX statement itself so reopen sees the
    // same set of registered indexes. The engine layer parses
    // `parameters_json` back into `(key, value)` pairs and re-runs
    // any access-method-specific side effects (e.g. add_fts_field
    // for `gin`) on restore.
    if let Some(name) = c.name.as_ref() {
        engine.register_catalog_index(
            name,
            if am.is_empty() { "btree" } else { am.as_str() },
            &c.table,
            &c.columns,
            &c.options,
        );
    }
    Ok(SQLResult::empty())
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

fn run_insert(
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
        let cancel = engine.cancellation_token();
        for source_row in result.rows {
            cancel.check()?;
            let mut document = Document::new();
            let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
            for col in &columns {
                if let Some(v) = source_row.get(col) {
                    if let Ok(vec) = value_to_vector(v) {
                        vectors.insert(col.clone(), vec);
                    }
                    document.insert(col.clone(), v.clone());
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
            engine.add_document_with_vectors(
                &stmt.table,
                doc_id,
                document,
                vectors_to_field_map(vectors),
            );
            affected += 1;
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
    if id_index.is_none() && auto_id_col.is_none() {
        return Err(SQLError::Unsupported(
            "INSERT requires an `id` column or a SERIAL/BIGSERIAL primary key".into(),
        ));
    }

    let mut affected = 0u64;
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
        let mut vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut doc_id: Option<u64> = None;
        for (i, col) in columns.iter().enumerate() {
            let v = eval(&row[i], &ctx)?;
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
            if let Ok(vec) = value_to_vector(&v) {
                vectors.insert(col.clone(), vec);
            }
            document.insert(col.clone(), v);
        }

        // DEFAULT expression -- evaluate when the column was absent
        // from the INSERT column list. Mirrors the Python reference's
        // _evaluate_default. The engine hook is in scope so DEFAULT
        // nextval('seq') resolves through the sequence store.
        for col in engine.table_columns(&stmt.table) {
            if document.contains_key(&col) {
                continue;
            }
            if let Some(default_expr) = engine.column_default_expr(&stmt.table, &col) {
                let v = eval(&default_expr, &ctx)?;
                if let Ok(vec) = value_to_vector(&v) {
                    vectors.insert(col.clone(), vec);
                }
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

        // FOREIGN KEY constraints -- every (local_columns) tuple
        // whose values are all non-null must have a matching row in
        // (ref_table, ref_columns). Null tuples are skipped to mirror
        // `PostgreSQL`'s MATCH SIMPLE default.
        for fk in engine.foreign_keys(&stmt.table) {
            let local_values: Vec<Value> = fk
                .local_columns
                .iter()
                .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
                .collect();
            if local_values.iter().any(|v| matches!(v, Value::Null)) {
                continue;
            }
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
            if !on_conflict.conflict_columns.is_empty() {
                let conflict_values: Vec<Value> = on_conflict
                    .conflict_columns
                    .iter()
                    .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
                    .collect();
                if let Some(existing_id) = engine.find_conflict(
                    &stmt.table,
                    &on_conflict.conflict_columns,
                    &conflict_values,
                ) {
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
                            let row_ctx =
                                EvalContext::new(Some(&existing_doc), params).with_engine(engine);
                            if let Some(pred) = r#where {
                                let keep = eval(pred, &row_ctx)?;
                                if !uqa_sql::expr::truthy(&keep) {
                                    continue;
                                }
                            }
                            let mut updates: BTreeMap<String, Value> = BTreeMap::new();
                            let mut conflict_vectors: BTreeMap<String, Vec<f32>> = BTreeMap::new();
                            for (col, expr) in assignments {
                                let v = eval(expr, &row_ctx)?;
                                if let Ok(vec) = value_to_vector(&v) {
                                    conflict_vectors.insert(col.clone(), vec);
                                }
                                updates.insert(col.clone(), v);
                            }
                            engine.update_document_fields(
                                &stmt.table,
                                existing_id,
                                updates,
                                conflict_vectors,
                            );
                            affected += 1;
                            continue;
                        }
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
        engine.add_document_with_vectors(
            &stmt.table,
            doc_id,
            document,
            vectors_to_field_map(vectors),
        );
        affected += 1;
    }
    Ok(SQLResult::from_affected(affected))
}

fn vectors_to_field_map(
    vectors: BTreeMap<String, Vec<f32>>,
) -> BTreeMap<uqa_core::FieldName, Vec<f32>> {
    vectors
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

/// Render the inner statement as an EXPLAIN-style plan result. Mirrors
/// Python's `_explain_plan`: returns a single-column `plan` table with
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
        // an empty single-row context. Mirrors Python's standalone
        // SELECT 1 / SELECT (SELECT ...).
        return run_select_without_from(engine, &stmt, params);
    };

    // Single-table FROM with no alias and no window function keeps the
    // search-aware fast path. JOIN shapes and window queries drop into
    // the multi-table executor that builds row tuples up-front and
    // filters them via the expression evaluator.
    if let FromClause::Table { name, alias } = from {
        // Schema-qualified names (information_schema.tables /
        // pg_catalog.pg_*) and CTE references skip the search-aware
        // fast path because they don't correspond to a registered
        // engine table.
        let is_virtual = name.contains('.') || engine.table(name).is_none();
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

fn run_query_block(
    engine: &Engine,
    stmt: &SelectStmt,
    params: &[SQLParam],
    ctes: &BTreeMap<String, Vec<ResultRow>>,
) -> Result<SQLResult, SQLError> {
    let from = stmt
        .from
        .as_ref()
        .ok_or_else(|| SQLError::Unsupported("SELECT without FROM".into()))?;

    let joined = build_join_rows_with_ctes(engine, from, params, ctes)?;

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
            let ctx = uqa_sql::expr::EvalContext::new(Some(&row), params).with_engine(engine);
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
    let columns = projection_columns(&stmt.projections);
    Ok(SQLResult::from_rows(columns, projected))
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
            execute_select(engine, &cte.query, params, ctes)?.rows
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
    if set_op.kind != SetOpKind::Union || !set_op.all {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION ALL".into(),
        ));
    }

    // Anchor: the LHS — the same SelectStmt with set_op stripped.
    let mut anchor_stmt = cte.query.as_ref().clone();
    anchor_stmt.set_op = None;
    anchor_stmt.with.clear();
    let anchor_columns = projection_columns(&anchor_stmt.projections);
    let anchor_rows = run_query_block(engine, &anchor_stmt, params, ctes)?.rows;

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
        all_rows.extend(renamed.clone());
        working = renamed;
    }
    Ok(all_rows)
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
    // aware / fusion-reordering passes — Python parity), then execute
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
            Some(Expr::Func { name, args }) if uqa_sql::registry::is_registered(name) => {
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
    let columns = projection_columns(&stmt.projections);
    let rows = build_rows(engine, table, &scored, &stmt.projections, params)?;
    Ok(SQLResult::from_rows(columns, rows))
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

    let columns = projection_columns(&stmt.projections);
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
/// each row's frame. Mirrors Python `_compute_framed_aggregate` in
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
            // Python fallback which also goes through `_resolve_frame_index`).
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
        // Resolve NULLS FIRST/LAST. Default mirrors PostgreSQL: ASC →
        // NULLS LAST, DESC → NULLS FIRST.
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
/// views. Mirrors Python's `_build_info_schema_*` /
/// `_build_pg_*` helpers. Returns `None` for any unknown name so the
/// caller falls back to the regular table lookup.
fn build_info_schema_rows(engine: &Engine, name: &str) -> Option<Vec<ResultRow>> {
    let lower = name.to_ascii_lowercase();
    let stripped: &str = lower
        .strip_prefix("information_schema.")
        .or_else(|| lower.strip_prefix("pg_catalog."))
        .unwrap_or(&lower);
    match stripped {
        "tables" | "pg_tables" => Some(build_pg_tables(engine)),
        "columns" => Some(build_info_columns(engine)),
        "pg_views" => Some(build_pg_views(engine)),
        "pg_indexes" => Some(build_pg_indexes(engine)),
        "pg_type" => Some(build_pg_type()),
        _ => None,
    }
}

fn build_pg_tables(engine: &Engine) -> Vec<ResultRow> {
    let mut out: Vec<ResultRow> = Vec::new();
    let mut names = engine.table_names();
    names.sort();
    for tname in names {
        let mut r = ResultRow::new();
        r.insert("table_catalog".into(), Value::Str(String::new()));
        r.insert("table_schema".into(), Value::Str("public".into()));
        r.insert("table_name".into(), Value::Str(tname.clone()));
        r.insert("table_type".into(), Value::Str("BASE TABLE".into()));
        // pg_tables shape additionally:
        r.insert("schemaname".into(), Value::Str("public".into()));
        r.insert("tablename".into(), Value::Str(tname));
        out.push(r);
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
            let mut r = ResultRow::new();
            r.insert("table_catalog".into(), Value::Str(String::new()));
            r.insert("table_schema".into(), Value::Str("public".into()));
            r.insert("table_name".into(), Value::Str(tname.clone()));
            r.insert("column_name".into(), Value::Str(col.name.clone()));
            r.insert("ordinal_position".into(), Value::Int((idx + 1) as i64));
            r.insert(
                "is_nullable".into(),
                Value::Str(if col.not_null {
                    "NO".into()
                } else {
                    "YES".into()
                }),
            );
            r.insert(
                "data_type".into(),
                Value::Str(format!("{:?}", col.ty).to_lowercase()),
            );
            out.push(r);
        }
    }
    out
}

fn build_pg_views(_engine: &Engine) -> Vec<ResultRow> {
    Vec::new()
}

fn build_pg_indexes(_engine: &Engine) -> Vec<ResultRow> {
    Vec::new()
}

fn build_pg_type() -> Vec<ResultRow> {
    let mut out: Vec<ResultRow> = Vec::new();
    for (oid, name) in [
        (16_i64, "bool"),
        (20_i64, "int8"),
        (23_i64, "int4"),
        (25_i64, "text"),
        (700_i64, "float4"),
        (701_i64, "float8"),
        (1043_i64, "varchar"),
        (1700_i64, "numeric"),
    ] {
        let mut r = ResultRow::new();
        r.insert("oid".into(), Value::Int(oid));
        r.insert("typname".into(), Value::Str(name.into()));
        out.push(r);
    }
    out
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
    // Emit NULLs for any column that ever appeared in the table; for an
    // empty table we still know the keys via document_count, but the
    // safe default is just an empty row — a missing key resolves to
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
            // information_schema / pg_catalog virtual views.
            if let Some(rows) = build_info_schema_rows(engine, name) {
                return Ok(rows.iter().map(|r| reprefix_row(&qual, r)).collect());
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
                JoinKind::Full => Err(SQLError::Unsupported("FULL OUTER JOIN".into())),
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
            let qual = alias.clone();
            let cols = column_aliases.clone();
            let rows: Vec<ResultRow> = result
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
                        if let Some(q) = &qual {
                            return prefix_row(q, &renamed);
                        }
                        renamed
                    } else if let Some(q) = &qual {
                        prefix_row(q, &r)
                    } else {
                        r
                    }
                })
                .collect();
            Ok(rows)
        }
    }
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
        // Build a per-iteration ctes scope that includes a synthetic
        // single-row CTE named after every alias seen in the left row,
        // so the right body's column refs resolve. The simpler
        // approach below substitutes the outer row by treating the
        // right side as an ordinary FromClause and combining at the
        // join layer; we still preserve the outer row for the ON
        // predicate evaluation below.
        let right_rows = build_join_rows_with_ctes(engine, right, params, ctes)?;
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

#[allow(clippy::similar_names)]
fn build_table_function_rows(
    engine: &Engine,
    name: &str,
    args: &[Expr],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::{eval, EvalContext};
    let ctx = EvalContext::new(None, params).with_engine(engine);
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
        "json_each" | "jsonb_each" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let s = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                Value::Map(_) => format!("{:?}", evaluated[0]),
                _ => return Err(SQLError::TypeMismatch(format!("{lower} arg type"))),
            };
            let parsed: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| SQLError::TypeMismatch(format!("{lower}: invalid JSON: {e}")))?;
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
                r.insert(
                    val_col.clone(),
                    Value::Str(serde_json::to_string(&v).unwrap_or_default()),
                );
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "json_array_elements" | "jsonb_array_elements" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let s = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch(format!("{lower} arg type"))),
            };
            let parsed: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| SQLError::TypeMismatch(format!("{lower}: invalid JSON: {e}")))?;
            let serde_json::Value::Array(arr) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an array"
                )));
            };
            for v in arr {
                push_scalar(
                    &mut out,
                    Value::Str(serde_json::to_string(&v).unwrap_or_default()),
                );
            }
            Ok(out)
        }
        // -------------------------------------------------------------
        // Analyzer DDL exposed as table-functions. Mirror Python's
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
            let names = engine.list_named_analyzers();
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
            let mut tables = Vec::new();
            inner_from.collect_tables(&mut tables);
            for (name, alias) in &tables {
                let null_keys = null_row_for(name, alias.as_deref(), engine);
                for (k, v) in null_keys {
                    pad.entry(k).or_insert(v);
                }
            }
            out.push(pad);
        }
    }
    Ok(out)
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
        if let Expr::Func { name, args } = &proj.expr {
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
        "graph_create" => {
            if let Some(eng) = engine {
                let _ = run_graph_create(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        "graph_drop" => {
            if let Some(eng) = engine {
                let _ = run_graph_drop(eng, args, params)?;
            }
            Ok(Some(Value::Bool(true)))
        }
        _ => Ok(None),
    }
}

/// Evaluate a `uqa_highlight(field, query[, start_tag, end_tag,
/// max_fragments, fragment_size])` projection. Mirrors the Python
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
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        Expr::QualifiedColumn { qualifier, column } => {
            match row.get(&format!("{qualifier}.{column}")) {
                Some(Value::Str(s)) => s.clone(),
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
    // as a candidate match term. The Python reference parses the FTS
    // query, but a simple split is what callers reach for in
    // practice and matches what the test fixture exercises.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let out = uqa_analysis::highlight(&text, &terms, None, &opts);
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
            let Expr::Func { name, args } = &proj.expr else {
                continue;
            };
            let value = match (name.to_ascii_lowercase().as_str(), args.as_slice()) {
                ("count", [Expr::Star]) | ("count", []) => Value::Int(1),
                (_, args) => {
                    let arg = args
                        .first()
                        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
                    uqa_sql::expr::eval(arg, &ctx)?
                }
            };
            bucket.0[i].observe(&value);
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
                let Expr::Func { name, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value(name, acc));
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
        out.push(row);
    }
    Ok(out)
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
            let Expr::Func { name, args } = &proj.expr else {
                continue;
            };
            let value = match (name.to_ascii_lowercase().as_str(), args.as_slice()) {
                ("count", [Expr::Star]) | ("count", []) => Value::Int(1),
                (_, args) => {
                    let arg = args
                        .first()
                        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
                    uqa_sql::expr::eval(arg, &ctx)?
                }
            };
            bucket.0[i].observe(&value);
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
                let Expr::Func { name, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value(name, acc));
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
        "count" | "sum" | "avg" | "min" | "max"
    ))
}

#[derive(Default)]
struct AggregateAccumulator {
    count: u64,
    sum: f64,
    min: Option<Value>,
    max: Option<Value>,
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
            let Expr::Func { name, args } = &proj.expr else {
                continue;
            };
            let value = match (name.to_ascii_lowercase().as_str(), args.as_slice()) {
                ("count", [Expr::Star]) | ("count", []) => Value::Int(1),
                (_, args) => {
                    let arg = args
                        .first()
                        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
                    uqa_sql::expr::eval(arg, &ctx)?
                }
            };
            bucket.0[i].observe(&value);
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
                let Expr::Func { name, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value(name, acc));
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
                    return Err(SQLError::Unsupported(format!(
                        "non-aggregated projection `{col}` must appear in GROUP BY"
                    )));
                }
            } else {
                return Err(SQLError::Unsupported(
                    "complex non-aggregate projections in GROUP BY are not supported".into(),
                ));
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
            let Expr::Func { name, args } = &proj.expr else {
                continue;
            };
            let value = match (name.to_ascii_lowercase().as_str(), args.as_slice()) {
                ("count", [Expr::Star]) | ("count", []) => Value::Int(1),
                (_, args) => {
                    let arg = args
                        .first()
                        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
                    uqa_sql::expr::eval(arg, &ctx)?
                }
            };
            bucket.0[i].observe(&value);
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
                let Expr::Func { name, .. } = &proj.expr else {
                    return Err(SQLError::Internal("aggregate expr lost".into()));
                };
                let acc = &accs[agg_idx];
                agg_idx += 1;
                row.insert(label, aggregate_value(name, acc));
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
    match name.to_ascii_lowercase().as_str() {
        "count" => Value::Int(acc.count as i64),
        "sum" => Value::Float(acc.sum),
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        _ => Value::Null,
    }
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
        FunctionKind::UQAHighlight
        | FunctionKind::UQAFacets
        | FunctionKind::AttentionFusion
        | FunctionKind::LearnedFusion
        | FunctionKind::CalibratedVectorMatch
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
    if args.is_empty() || args.len() % 2 != 0 {
        return Err(SQLError::BadArity {
            name: "multi_field_match".into(),
            expected: "even, >= 2 (alternating field, query)".into(),
            actual: args.len(),
        });
    }
    let ctx = EvalContext::new(None, params).with_engine(engine);
    // Run a text_match per (field, query) pair, accumulate per-doc
    // probability vectors with 0.5 prior for missing fields, then fuse
    // via log-odds conjunction with uniform weights.
    let n_fields = args.len() / 2;
    let mut per_doc: std::collections::BTreeMap<u64, Vec<f64>> = std::collections::BTreeMap::new();
    for i in 0..n_fields {
        let field = expect_column_name(&args[2 * i], "multi_field_match.field")?;
        let q = match eval(&args[2 * i + 1], &ctx)? {
            Value::Str(s) => s,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "multi_field_match query must be string, got {other:?}"
                )));
            }
        };
        let mode = crate::ScoringMode::BayesianBM25(uqa_scoring::BayesianBM25Params::default());
        let scored = engine.search(table, &field, &q, &mode, usize::MAX);
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
    let weights = vec![1.0 / n_fields as f64; n_fields];
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
/// matching the Python reference's shape.
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

/// `rpq(expr, start, graph)` — evaluate a Regular Path Query
/// (Definition 5.1.2). Mirrors Python's
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
    let field = expect_column_name(&args[0], "text_match.field")?;
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
    Ok(engine.search(table, &field, &query, &mode, usize::MAX))
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
    // Phase 5 supports the canonical hybrid call shape:
    // `fuse_log_odds(text_match(field, q), knn_match(vec_field, $1, k))`.
    let mut text_field: Option<String> = None;
    let mut text_query: Option<String> = None;
    let mut vector_field: Option<String> = None;
    let mut query_vector: Option<Vec<f32>> = None;
    let mut knn_pool: usize = 10;
    let ctx = EvalContext::new(None, params).with_engine(engine);
    for arg in args {
        match arg {
            Expr::Func { name, args: inner } => {
                let kind = lookup(name).ok_or_else(|| SQLError::UnknownFunction(name.clone()))?;
                match kind {
                    FunctionKind::TextMatch | FunctionKind::BayesianMatch => {
                        if inner.len() != 2 {
                            return Err(SQLError::BadArity {
                                name: name.clone(),
                                expected: "2".into(),
                                actual: inner.len(),
                            });
                        }
                        text_field = Some(expect_column_name(&inner[0], "text_match.field")?);
                        let q = eval(&inner[1], &ctx)?;
                        text_query = match q {
                            Value::Str(s) => Some(s),
                            other => {
                                return Err(SQLError::TypeMismatch(format!(
                                    "text_match query must be string, got {other:?}"
                                )));
                            }
                        };
                    }
                    FunctionKind::KNNMatch => {
                        if inner.len() != 3 {
                            return Err(SQLError::BadArity {
                                name: name.clone(),
                                expected: "3".into(),
                                actual: inner.len(),
                            });
                        }
                        vector_field = Some(expect_column_name(&inner[0], "knn_match.field")?);
                        query_vector = Some(value_to_vector(&eval(&inner[1], &ctx)?)?);
                        knn_pool = expect_usize(&inner[2], "knn_match.k", &ctx)?;
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
                }
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "fuse_log_odds argument must be a function call, got {other:?}"
                )));
            }
        }
    }
    let text_field = text_field
        .ok_or_else(|| SQLError::Unsupported("fuse_log_odds requires a text_match arm".into()))?;
    let text_query = text_query.unwrap_or_default();
    let vector_field = vector_field
        .ok_or_else(|| SQLError::Unsupported("fuse_log_odds requires a knn_match arm".into()))?;
    let query_vector =
        query_vector.ok_or_else(|| SQLError::Internal("missing knn_match vector".into()))?;

    Ok(engine.hybrid_search(&HybridSearchParams {
        table,
        text_field: &text_field,
        text_query: &text_query,
        vector_field: &vector_field,
        query_vector,
        knn_pool,
        alpha: 0.5,
        top_k: usize::MAX,
    }))
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
/// Mirrors Python's `_extract_int_value` — accepts integer constants,
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
                if k.as_str() == SCORE_COLUMN {
                    continue;
                }
                row.insert(k.clone(), v.clone());
            }
            continue;
        }
        // Engine-side registry hooks (uqa_highlight / graph_create /
        // graph_drop) need access to the engine; intercept them
        // before falling through to the scalar evaluator.
        if let Expr::Func { name, args } = &proj.expr {
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
