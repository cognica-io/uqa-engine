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

/// Apache AGE graph name validation: at least 3 characters and the
/// first character must be a letter or underscore.
fn age_graph_name_is_valid(name: &str) -> bool {
    name.len() >= 3
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn eval_age_graph_name_with(
    expr: &ScalarExpr,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<String, SQLError> {
    match evaluate(expr)? {
        Value::Null => Err(SQLError::Unsupported("graph name can not be NULL".into())),
        Value::Str(s) => Ok(s),
        other => Err(SQLError::TypeMismatch(format!(
            "graph name must be a string, got {other:?}"
        ))),
    }
}

/// `SELECT create_graph('name')` with AGE 1.6.0 semantics: validates
/// the name, rejects duplicates, and returns void (SQL NULL).
pub(in crate::sql) fn run_age_create_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() != 1 {
        return Err(SQLError::BadArity {
            name: "create_graph".into(),
            expected: "1".into(),
            actual: args.len(),
        });
    }
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !age_graph_name_is_valid(&name) {
        return Err(SQLError::Unsupported("graph name is invalid".into()));
    }
    if engine
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
    {
        return Err(SQLError::Unsupported(format!(
            "graph \"{name}\" already exists"
        )));
    }
    engine
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT drop_graph('name'[, cascade])` with AGE 1.6.0 semantics:
/// without `cascade => true` the drop always fails (the graph schema
/// always contains its label tables), and success returns void.
pub(in crate::sql) fn run_age_drop_graph_with_evaluator(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: "drop_graph".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !engine
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?
    {
        return Err(SQLError::Unsupported(format!(
            "graph \"{name}\" does not exist"
        )));
    }
    let cascade = match args.get(1) {
        Some(expr) => match evaluate(expr)? {
            Value::Bool(b) => b,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "drop_graph.cascade must be a boolean, got {other:?}"
                )));
            }
        },
        None => false,
    };
    if !cascade {
        // AGE maps this onto `DROP SCHEMA <name> RESTRICT`, which
        // always fails because the label tables live in the schema.
        return Err(SQLError::Unsupported(format!(
            "cannot drop schema {name} because other objects depend on it"
        )));
    }
    engine
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))?;
    Ok(Value::Null)
}
