//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-body AST lowering helpers for the SQL compiler.

use pg_query::protobuf::Node;
use pg_query::NodeEnum;
use uqa_core::{DecimalValue, Value};

use crate::ast::{
    BinaryOp, ColumnDef, CreateIndex, CreateTable, Expr, FromClause, InsertStmt, JoinKind, OrderBy,
    Projection, SelectStmt, SetOp, SetOpKind, TableKeyConstraint, TableKeyConstraintKind,
    WindowSpec, CTE,
};
use crate::error::{Result, SQLError};

use super::types::{
    compile_foreign_key_action, compile_foreign_key_match, compile_type_name, raw_type_name,
    validate_foreign_key_set_columns,
};
use super::{compile_qualified_name, range_var_name};

pub(super) fn extract_string(node: &Node) -> Result<String> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing string node".into()));
    };
    match inner {
        NodeEnum::String(s) => Ok(s.sval.clone()),
        _ => Err(SQLError::Internal(format!(
            "expected String node, got {inner:?}"
        ))),
    }
}

fn extract_strings(nodes: &[Node]) -> Result<Vec<String>> {
    nodes.iter().map(extract_string).collect()
}

/// Translate a `#>` / `#>>` operator into the argument list of
/// `json_extract_path`. The right-hand side is a Postgres text-array
/// literal like `'{a,b,c}'`; we split it into individual literal
/// segments so the scalar function can walk the path.
fn json_path_args(lhs: Expr, rhs: Expr) -> Vec<Expr> {
    let segments = match &rhs {
        Expr::Literal(uqa_core::Value::Str(s)) => s
            .trim_matches(|c: char| c == '{' || c == '}')
            .split(',')
            .map(|seg| Expr::Literal(uqa_core::Value::Str(seg.trim().to_string())))
            .collect::<Vec<_>>(),
        Expr::Literal(uqa_core::Value::List(items)) => items
            .iter()
            .map(|v| Expr::Literal(v.clone()))
            .collect::<Vec<_>>(),
        _ => vec![rhs],
    };
    let mut out = Vec::with_capacity(segments.len() + 1);
    out.push(lhs);
    out.extend(segments);
    out
}

// -------------------------------------------------------------------------
// CREATE TABLE
// -------------------------------------------------------------------------

mod ddl;
mod expression_atoms;
mod expression_core;
mod expression_operators;
mod from;
mod insert;
mod select;
mod window;

pub(in crate::compiler) use ddl::*;
pub(in crate::compiler) use expression_atoms::*;
pub(in crate::compiler) use expression_core::*;
pub(in crate::compiler) use expression_operators::*;
pub(in crate::compiler) use from::*;
pub(in crate::compiler) use insert::*;
pub(in crate::compiler) use select::*;
pub(in crate::compiler) use window::*;

#[cfg(test)]
mod malformed_tree_tests;
