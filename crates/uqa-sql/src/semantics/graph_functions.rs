//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph-name selection and argument rules for SQL graph table functions.

use crate::SQLError;
use uqa_core::Value;

pub trait GraphNameCatalog {
    fn list_graphs(&self) -> Result<Vec<String>, SQLError>;
}

fn default_graph_name(
    catalog: &dyn GraphNameCatalog,
    function_name: &str,
) -> Result<String, SQLError> {
    let graphs = catalog.list_graphs()?;
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

pub fn expect_optional_graph_value(
    catalog: &dyn GraphNameCatalog,
    value: Option<&Value>,
    function_name: &str,
) -> Result<String, SQLError> {
    match value {
        Some(Value::Str(name)) => Ok(name.clone()),
        Some(other) => Err(SQLError::TypeMismatch(format!(
            "{function_name}.graph must be string, got {other:?}"
        ))),
        None => default_graph_name(catalog, function_name),
    }
}

pub fn centrality_graph(
    catalog: &dyn GraphNameCatalog,
    evaluated: &[Value],
    lower: &str,
) -> Result<String, SQLError> {
    if evaluated.len() > 1 {
        return Err(SQLError::TypeMismatch(format!(
            "{lower} accepts at most one graph argument"
        )));
    }
    let graph = expect_optional_graph_value(catalog, evaluated.first(), lower)?;
    Ok(graph)
}

pub fn regular_path_arguments(
    catalog: &dyn GraphNameCatalog,
    evaluated: &[Value],
) -> Result<(String, u64, String), SQLError> {
    if !(2..=3).contains(&evaluated.len()) {
        return Err(SQLError::TypeMismatch(
            "rpq requires 2 or 3 args (expr, start [, graph])".into(),
        ));
    }
    let expr_str = match &evaluated[0] {
        Value::Str(s) => s.clone(),
        _ => return Err(SQLError::TypeMismatch("rpq.expr must be string".into())),
    };
    let start = match &evaluated[1] {
        Value::Int(n) => u64::try_from(*n).map_err(|_| {
            SQLError::TypeMismatch("rpq.start must be a non-negative integer".into())
        })?,
        _ => return Err(SQLError::TypeMismatch("rpq.start must be integer".into())),
    };
    let graph = expect_optional_graph_value(catalog, evaluated.get(2), "rpq")?;
    Ok((expr_str, start, graph))
}
