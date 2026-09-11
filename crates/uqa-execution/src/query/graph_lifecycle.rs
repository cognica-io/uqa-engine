//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native and AGE graph command scheduling and result values.

use crate::{eval_scalar, ScalarEvalContext};
use uqa_core::{ScoredEntry, Value};
use uqa_graph::{GraphLabelInfo, LabelKind};
use uqa_sql::{
    expr::EngineHook,
    semantics::graph_commands::{
        age_error, eval_age_bool_with, eval_age_graph_name_with, eval_age_name_with,
        graph_create_name, graph_drop_name, require_age_arity, validate_graph_drop_cascade,
        AGE_DEPENDENT_OBJECTS_STILL_EXIST, AGE_DUPLICATE_SCHEMA, AGE_FEATURE_NOT_SUPPORTED,
        AGE_INVALID_PARAMETER_VALUE, AGE_UNDEFINED_SCHEMA, AGE_UNDEFINED_TABLE,
    },
    SQLError, SQLParam, ScalarExpr,
};
use uqa_storage::StorageBackendResult;

/// Graph catalog operations inside the public graph API's existing transaction boundary.
pub trait GraphLifecycle {
    fn has_graph(&self, name: &str) -> StorageBackendResult<bool>;
    fn has_namespace(&self, name: &str) -> StorageBackendResult<bool>;
    fn create_graph(&self, name: String) -> StorageBackendResult<bool>;
    fn drop_graph(&self, name: &str) -> StorageBackendResult<bool>;
    fn list_graph_labels(&self, graph: &str) -> StorageBackendResult<Option<Vec<GraphLabelInfo>>>;
    fn create_graph_label(
        &self,
        graph: &str,
        label: &str,
        kind: LabelKind,
    ) -> StorageBackendResult<bool>;
    fn drop_graph_label(&self, graph: &str, label: &str) -> StorageBackendResult<bool>;
    fn graph_label_relation_dependents(
        &self,
        graph: &str,
        label: &str,
    ) -> StorageBackendResult<Vec<String>>;
    fn rename_graph(&self, from: &str, to: &str) -> StorageBackendResult<bool>;
}

pub fn run_graph_create(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    params: &[SQLParam],
    hook: &dyn EngineHook,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(hook);
    run_graph_create_with_evaluator(runtime, args, &mut |expr| eval_scalar(expr, &ctx))?;
    Ok(Vec::new())
}

pub fn run_graph_create_with_evaluator(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    let name = graph_create_name(args, evaluate)?;
    runtime
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))
}

pub fn run_graph_drop(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    params: &[SQLParam],
    hook: &dyn EngineHook,
) -> Result<Vec<ScoredEntry>, SQLError> {
    let ctx = ScalarEvalContext::new(None, params).with_function_hook(hook);
    run_graph_drop_with_evaluator(runtime, args, &mut |expr| eval_scalar(expr, &ctx))?;
    Ok(Vec::new())
}

pub fn run_graph_drop_with_evaluator(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<bool, SQLError> {
    let name = graph_drop_name(args, evaluate)?;
    let graph_exists = runtime
        .has_graph(&name)
        .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))?;
    validate_graph_drop_cascade(&name, graph_exists, args.get(1), evaluate)?;
    runtime
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))
}

fn age_graph_catalog_error(err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("read graph catalog: {err}"))
}

/// `SELECT create_graph('name')` with AGE semantics: validates the name,
/// rejects duplicate graphs and namespace collisions, and returns void
/// (SQL NULL). The graph namespace is reserved like AGE's `CREATE SCHEMA`.
pub fn run_age_create_graph_with_evaluator(
    runtime: &dyn GraphLifecycle,
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
    if runtime.has_graph(&name).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{name}\" already exists"),
        ));
    }
    if runtime
        .has_namespace(&name)
        .map_err(age_graph_catalog_error)?
    {
        return Err(age_error(
            AGE_DUPLICATE_SCHEMA,
            format!("schema \"{name}\" already exists"),
        ));
    }
    runtime
        .create_graph(name)
        .map_err(|err| SQLError::Internal(format!("create graph: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT drop_graph('name'[, cascade])` with AGE semantics: false uses
/// `DROP SCHEMA ... RESTRICT` and succeeds only after every label relation is
/// gone, while true removes surviving labels; success returns void.
pub fn run_age_drop_graph_with_evaluator(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("drop_graph", args, 1..=2)?;
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    if !runtime.has_graph(&name).map_err(age_graph_catalog_error)? {
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
        let labels = runtime
            .list_graph_labels(&name)
            .map_err(age_graph_catalog_error)?
            .unwrap_or_default();
        if !labels.is_empty() {
            return Err(age_error(
                AGE_DEPENDENT_OBJECTS_STILL_EXIST,
                format!("cannot drop schema {name} because other objects depend on it"),
            ));
        }
    }
    runtime
        .drop_graph(&name)
        .map_err(|err| SQLError::Internal(format!("drop graph: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT graph_exists('name')`: AGE returns an agtype boolean, which
/// surfaces through SQL as the agtype text `true` / `false`.
pub fn run_age_graph_exists_with_evaluator(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    require_age_arity("graph_exists", args, 1..=1)?;
    let name = eval_age_graph_name_with(&args[0], evaluate)?;
    let exists = runtime.has_graph(&name).map_err(age_graph_catalog_error)?;
    Ok(Value::Str(uqa_core::agtype::render(&Value::Bool(exists))))
}

/// Shared body of `create_vlabel` / `create_elabel`.
fn run_age_create_label_with_evaluator(
    runtime: &dyn GraphLifecycle,
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
    if !runtime.has_graph(&graph).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{graph}\" does not exist."),
        ));
    }
    let labels = runtime
        .list_graph_labels(&graph)
        .map_err(age_graph_catalog_error)?
        .unwrap_or_default();
    if !labels
        .iter()
        .any(|entry| entry.name == kind.default_label_name())
    {
        return Err(age_error(
            AGE_UNDEFINED_TABLE,
            format!(
                "relation \"{graph}.{}\" does not exist",
                kind.default_label_name()
            ),
        ));
    }
    let created = runtime
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
pub fn run_age_create_vlabel_with_evaluator(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    run_age_create_label_with_evaluator(
        runtime,
        "create_vlabel",
        uqa_graph::LabelKind::Vertex,
        args,
        evaluate,
    )
}

/// `SELECT create_elabel('graph', 'label')` with AGE semantics.
pub fn run_age_create_elabel_with_evaluator(
    runtime: &dyn GraphLifecycle,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    run_age_create_label_with_evaluator(
        runtime,
        "create_elabel",
        uqa_graph::LabelKind::Edge,
        args,
        evaluate,
    )
}

/// `SELECT drop_label('graph', 'label'[, force])` with AGE semantics: the
/// label relation is dropped together with every entity that carries the
/// label, `force => true` is rejected exactly like AGE, and a default label
/// is restricted only while user labels of the same kind inherit from it.
pub fn run_age_drop_label_with_evaluator(
    runtime: &dyn GraphLifecycle,
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
    if !runtime.has_graph(&graph).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{graph}\" does not exist"),
        ));
    }
    let labels = runtime
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
    let dependent_views = runtime
        .graph_label_relation_dependents(&graph, &label)
        .map_err(|error| SQLError::Internal(format!("inspect label dependencies: {error}")))?;
    if !dependent_views.is_empty() {
        return Err(age_error(
            AGE_DEPENDENT_OBJECTS_STILL_EXIST,
            format!("cannot drop table {graph}.{label} because other objects depend on it"),
        ));
    }
    // AGE issues `DROP TABLE ... RESTRICT`: user label relations inherit from
    // the default relation of their kind, while the graph catalog itself does
    // not depend on either default relation.
    if (entry.id == uqa_graph::VERTEX_DEFAULT_LABEL_ID
        || entry.id == uqa_graph::EDGE_DEFAULT_LABEL_ID)
        && labels.iter().any(|candidate| {
            candidate.id >= uqa_graph::FIRST_USER_LABEL_ID && candidate.kind == entry.kind
        })
    {
        return Err(age_error(
            AGE_DEPENDENT_OBJECTS_STILL_EXIST,
            format!("cannot drop table {graph}.{label} because other objects depend on it"),
        ));
    }
    runtime
        .drop_graph_label(&graph, &label)
        .map_err(|err| SQLError::Internal(format!("drop label: {err}")))?;
    Ok(Value::Null)
}

/// `SELECT alter_graph('graph', 'RENAME', 'new_name')` with AGE
/// semantics; `RENAME` is the only operation AGE implements.
pub fn run_age_alter_graph_with_evaluator(
    runtime: &dyn GraphLifecycle,
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
    if !runtime.has_graph(&graph).map_err(age_graph_catalog_error)? {
        return Err(age_error(
            AGE_UNDEFINED_SCHEMA,
            format!("graph \"{graph}\" does not exist"),
        ));
    }
    // `RenameSchema` rejects any taken name, including the graph's own
    // current name, so renaming a graph onto itself is a duplicate schema.
    if runtime
        .has_namespace(&new_value)
        .map_err(age_graph_catalog_error)?
    {
        return Err(age_error(
            AGE_DUPLICATE_SCHEMA,
            format!("schema \"{new_value}\" already exists"),
        ));
    }
    runtime
        .rename_graph(&graph, &new_value)
        .map_err(|err| SQLError::Internal(format!("rename graph: {err}")))?;
    Ok(Value::Null)
}
