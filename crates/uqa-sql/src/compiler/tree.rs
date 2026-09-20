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
    WindowReference, WindowReferenceKind, WindowSpec, CTE,
};
use crate::error::{Result, SQLError};

use super::types::{
    compile_foreign_key_action, compile_foreign_key_match, compile_type_name, raw_type_name,
    validate_foreign_key_set_columns,
};
use super::{
    compile_qualified_name, compile_returning_clause, range_var_name, render_relation_component,
};

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

pub(super) fn extract_strings(nodes: &[Node]) -> Result<Vec<String>> {
    nodes.iter().map(extract_string).collect()
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
mod locking;
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
