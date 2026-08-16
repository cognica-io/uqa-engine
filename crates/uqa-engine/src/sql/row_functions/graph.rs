//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph analytics, graph lifecycle, and AGE-compatible scalar helpers.

use super::{
    eval_scalar, expect_evaluated_string, Engine, OperatorTree, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, ScoredEntry, Value,
};

fn default_graph_name(engine: &Engine, function_name: &str) -> Result<String, SQLError> {
    let graphs = engine
        .list_graphs()
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?;
    match graphs.as_slice() {
        [name] => Ok(name.clone()),
        [] => Err(SQLError::Unsupported(format!(
            "{function_name} requires a graph argument because no graph is registered"
        ))),
        _ => Err(SQLError::Unsupported(format!(
            "{function_name} requires a graph argument because multiple graphs are registered: {}",
            graphs.join(", ")
        ))),
    }
}

pub(in crate::sql) fn expect_optional_graph_value(
    engine: &Engine,
    value: Option<&Value>,
    function_name: &str,
) -> Result<String, SQLError> {
    match value {
        Some(Value::Str(name)) => Ok(name.clone()),
        Some(other) => Err(SQLError::TypeMismatch(format!(
            "{function_name}.graph must be string, got {other:?}"
        ))),
        None => default_graph_name(engine, function_name),
    }
}

pub(in crate::sql) fn graph_pagerank_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        engine,
        &OperatorTree::PageRank {
            graph: name.to_string(),
        },
    )
}

pub(in crate::sql) fn graph_hits_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        engine,
        &OperatorTree::HITS {
            graph: name.to_string(),
        },
    )
}

pub(in crate::sql) fn graph_betweenness_entries(
    engine: &Engine,
    name: &str,
) -> Result<Vec<ScoredEntry>, SQLError> {
    execute_tree_entries(
        engine,
        &OperatorTree::BetweennessCentrality {
            graph: name.to_string(),
        },
    )
}

pub(in crate::sql) fn execute_tree_entries(
    engine: &Engine,
    tree: &OperatorTree,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let posting = crate::operator_tree_bridge::expect_posting_output(
        crate::operator_tree_bridge::execute_operator_tree_in_execution(engine, "", &[], tree)?,
        "SQL table function",
    )?;
    Ok(posting
        .entries()
        .iter()
        .map(|entry| ScoredEntry {
            doc_id: entry.doc_id,
            score: entry.payload.score,
        })
        .collect())
}

pub(in crate::sql) fn run_graph_create(
    engine: &Engine,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    run_graph_create_with_evaluator(engine, args, &mut |expr| eval_scalar(expr, &ctx))?;
    Ok(Vec::new())
}

pub(in crate::sql) fn run_graph_create_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "graph_create".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let name = expect_evaluated_string(evaluate(&args[0])?, "graph_create.name")?;
    engine
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))
}

pub(in crate::sql) fn run_graph_drop(
    engine: &Engine,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(engine);
    run_graph_drop_with_evaluator(engine, args, &mut |expr| eval_scalar(expr, &ctx))?;
    Ok(Vec::new())
}

pub(in crate::sql) fn run_graph_drop_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "graph_drop".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    let name = expect_evaluated_string(evaluate(&args[0])?, "graph_drop.name")?;
    let graph_exists = engine
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?;
    if let Some(cascade_expr) = args.get(1) {
        match evaluate(cascade_expr)? {
            Value::Bool(true) => {}
            Value::Bool(false) if graph_exists => {
                return Err(SQLError::Unsupported(format!(
                    "cannot drop graph {name:?} without cascade"
                )));
            }
            Value::Bool(false) => {}
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "graph_drop.cascade must be a boolean, got {other:?}"
                )));
            }
        }
    }
    engine
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))
}

// ---------------------------------------------------------------------
// Apache AGE graph and label management functions.
//
// Messages and SQLSTATEs follow AGE's `graph_commands.c` and
// `label_commands.c` so drivers and scripts written against AGE see the
// same errors.
// ---------------------------------------------------------------------

const AGE_INVALID_PARAMETER_VALUE: &str = "22023";
const AGE_UNDEFINED_SCHEMA: &str = "3F000";
const AGE_DUPLICATE_SCHEMA: &str = "42P06";
const AGE_UNDEFINED_TABLE: &str = "42P01";
const AGE_FEATURE_NOT_SUPPORTED: &str = "0A000";
const AGE_DEPENDENT_OBJECTS_STILL_EXIST: &str = "2BP01";

fn age_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.to_string(),
        message: message.into(),
    }
}

fn age_graph_catalog_error(err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("read graph catalog: {err}"))
}

/// Evaluate a `name`/`cstring` argument of an AGE management function.
/// `null_message` is the AGE error for a SQL NULL argument.
fn eval_age_name_with(
    expr: &ScalarExpr,
    null_message: &str,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    match evaluate(expr)? {
        Value::Null => Err(age_error(AGE_INVALID_PARAMETER_VALUE, null_message)),
        Value::Str(s) | Value::FixedChar(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "graph name must be a string, got {other:?}"
        ))),
    }
}

fn eval_age_graph_name_with(
    expr: &ScalarExpr,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    eval_age_name_with(expr, "graph name can not be NULL", evaluate)
}

fn eval_age_bool_with(
    expr: &ScalarExpr,
    argument: &str,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    match evaluate(expr)? {
        Value::Bool(value) => Ok(value),
        other => Err(SQLError::TypeMismatch(format!(
            "{argument} must be a boolean, got {other:?}"
        ))),
    }
}

fn require_age_arity(
    name: &str,
    args: &[ScalarExpr],
    range: std::ops::RangeInclusive<usize>,
) -> Result<(), SQLError> {
    if range.contains(&args.len()) {
        return Ok(());
    }
    let expected = if range.start() == range.end() {
        range.start().to_string()
    } else {
        format!("{} or {}", range.start(), range.end())
    };
    Err(SQLError::BadArity {
        name: name.into(),
        expected,
        actual: args.len(),
    })
}

/// `SELECT create_graph('name')` with AGE semantics: validates the name,
/// rejects duplicate graphs and namespace collisions, and returns void
/// (SQL NULL). The graph namespace is reserved like AGE's `CREATE SCHEMA`.
pub(in crate::sql) fn run_age_create_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("create_graph", args, 1..=1)?;
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !uqa_graph::age_names::is_valid_graph_name(&name) {
        return Err(age_error(
            AGE_INVALID_PARAMETER_VALUE,
            "graph name is invalid",
        ));
    }
    if engine.has_graph(&name).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{name}\" already exists"),
        ));
    }
    if engine.has_schema(&name).map_err(age_graph_catalog_error)?
        || matches!(
            name.as_str(),
            "pg_catalog" | "information_schema" | "ag_catalog"
        )
    {
        return Err(age_error(
            AGE_DUPLICATE_SCHEMA,
            format!("schema \"{name}\" already exists"),
        ));
    }
    engine
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT drop_graph('name'[, cascade])` with AGE semantics: without
/// `cascade => true` the drop always fails because AGE issues
/// `DROP SCHEMA ... RESTRICT` on a namespace that still holds its label
/// tables; success returns void.
pub(in crate::sql) fn run_age_drop_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("drop_graph", args, 1..=2)?;
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !engine.has_graph(&name).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{name}\" does not exist"),
        ));
    }
    let cascade = match args.get(1) {
        Some(expr) => eval_age_bool_with(expr, "drop_graph.cascade", evaluate)?,
        None => false,
    };
    if !cascade {
        return Err(age_error(
            AGE_DEPENDENT_OBJECTS_STILL_EXIST,
            format!("cannot drop schema {name} because other objects depend on it"),
        ));
    }
    engine
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT graph_exists('name')`: AGE returns an agtype boolean, which
/// surfaces through SQL as the agtype text `true` / `false`.
pub(in crate::sql) fn run_age_graph_exists_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("graph_exists", args, 1..=1)?;
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    let exists = engine.has_graph(&name).map_err(age_graph_catalog_error)?;
    Ok(Value::Str(uqa_graph::agtype::render(&Value::Bool(exists))))
}

/// Shared body of `create_vlabel` / `create_elabel`.
fn run_age_create_label_with_evaluator(
    engine: &Engine,
    function_name: &str,
    kind: uqa_graph::LabelKind,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity(function_name, args, 2..=2)?;
    let graph = eval_age_name_with(&args[0], "graph name must not be NULL", evaluate)?;
    let label = eval_age_name_with(&args[1], "label name must not be NULL", evaluate)?;
    if !uqa_graph::age_names::is_valid_graph_name(&graph) {
        return Err(age_error(
            AGE_INVALID_PARAMETER_VALUE,
            "graph name is invalid",
        ));
    }
    if !uqa_graph::age_names::is_valid_label_name(&label) {
        return Err(age_error(
            AGE_INVALID_PARAMETER_VALUE,
            "label name is invalid",
        ));
    }
    if !engine.has_graph(&graph).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{graph}\" does not exist."),
        ));
    }
    let created = engine
        .create_graph_label(&graph, &label, kind)
        .map_err(|err| SQLError::Internal(format!("create label: {err}")))?;
    if !created {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("label \"{label}\" already exists"),
        ));
    }
    Ok(Value::Null)
}

/// `SELECT create_vlabel('graph', 'label')` with AGE semantics.
pub(in crate::sql) fn run_age_create_vlabel_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    run_age_create_label_with_evaluator(
        engine,
        "create_vlabel",
        uqa_graph::LabelKind::Vertex,
        args,
        evaluate,
    )
}

/// `SELECT create_elabel('graph', 'label')` with AGE semantics.
pub(in crate::sql) fn run_age_create_elabel_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    run_age_create_label_with_evaluator(
        engine,
        "create_elabel",
        uqa_graph::LabelKind::Edge,
        args,
        evaluate,
    )
}

/// `SELECT drop_label('graph', 'label'[, force])` with AGE semantics: the
/// label relation is dropped together with every entity that carries the
/// label, `force => true` is rejected exactly like AGE, and the default
/// labels stay because the graph depends on them.
pub(in crate::sql) fn run_age_drop_label_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("drop_label", args, 2..=3)?;
    let graph = eval_age_name_with(&args[0], "graph name must not be NULL", evaluate)?;
    let label = eval_age_name_with(&args[1], "label name must not be NULL", evaluate)?;
    let force = match args.get(2) {
        Some(expr) => eval_age_bool_with(expr, "drop_label.force", evaluate)?,
        None => false,
    };
    if !engine.has_graph(&graph).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{graph}\" does not exist"),
        ));
    }
    let labels = engine
        .list_graph_labels(&graph)
        .map_err(age_graph_catalog_error)?
        .unwrap_or_default();
    let Some(entry) = labels.iter().find(|entry| entry.name == label) else {
        return Err(age_error(
            AGE_UNDEFINED_TABLE,
            format!("label \"{label}\" does not exist"),
        ));
    };
    if force {
        return Err(age_error(
            AGE_FEATURE_NOT_SUPPORTED,
            "force option is not supported yet",
        ));
    }
    if entry.id == uqa_graph::VERTEX_DEFAULT_LABEL_ID
        || entry.id == uqa_graph::EDGE_DEFAULT_LABEL_ID
    {
        return Err(age_error(
            AGE_DEPENDENT_OBJECTS_STILL_EXIST,
            format!("cannot drop table {graph}.{label} because other objects depend on it"),
        ));
    }
    engine
        .drop_graph_label(&graph, &label)
        .map_err(|err| SQLError::Internal(format!("drop label: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT alter_graph('graph', 'RENAME', 'new_name')` with AGE
/// semantics; `RENAME` is the only operation AGE implements.
pub(in crate::sql) fn run_age_alter_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("alter_graph", args, 3..=3)?;
    let graph = eval_age_name_with(&args[0], "graph_name must not be NULL", evaluate)?;
    let operation = eval_age_name_with(&args[1], "operation must not be NULL", evaluate)?;
    let new_value = eval_age_name_with(&args[2], "new_value must not be NULL", evaluate)?;
    if !operation.eq_ignore_ascii_case("RENAME") {
        return Err(age_error(
            AGE_INVALID_PARAMETER_VALUE,
            format!("invalid operation \"{operation}\""),
        ));
    }
    if !uqa_graph::age_names::is_valid_graph_name(&new_value) {
        return Err(age_error(
            AGE_INVALID_PARAMETER_VALUE,
            "new graph name is invalid",
        ));
    }
    if !engine.has_graph(&graph).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{graph}\" does not exist"),
        ));
    }
    // `RenameSchema` rejects any taken name, including the graph's own
    // current name, so renaming a graph onto itself is a duplicate schema.
    if engine
        .has_graph(&new_value)
        .map_err(age_graph_catalog_error)?
        || engine
            .has_schema(&new_value)
            .map_err(age_graph_catalog_error)?
        || matches!(
            new_value.as_str(),
            "pg_catalog" | "information_schema" | "ag_catalog"
        )
    {
        return Err(age_error(
            AGE_DUPLICATE_SCHEMA,
            format!("schema \"{new_value}\" already exists"),
        ));
    }
    engine
        .rename_graph(&graph, &new_value)
        .map_err(|err| SQLError::Internal(format!("rename graph: {err}")))?;
    Ok(Value::Null)
}
