//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lift a `PostgreSQL` parse tree into the internal [`Statement`] AST.
//!
//! The facade exposes compilation while statement-family modules own
//! validation and lowering. Tree-shaped SELECT/DDL/expression lowering remains
//! in the private `tree` module, and `PostgreSQL` type interpretation remains
//! in the private `types` module.

use crate::ast::{
    AlterTableAction, AlterTableStmt, AlterViewKind, AlterViewOptionsAction, AlterViewOptionsStmt,
    ColumnDef, DeleteStmt, DropKind, DropStmt, Expr, Statement, TableKeyConstraint,
    TableKeyConstraintKind, TransactionStmt, UpdateStmt,
};
use crate::error::{Result, SQLError};
use pg_query::protobuf::{Node, RangeVar};
use pg_query::NodeEnum;
use types::compile_pg_type_name;

mod administrative;
mod dispatch;
mod dml;
mod drop_alter;
mod merge;
mod names;
mod relations;
mod returning;
mod routines;
mod sequences;
mod tree;
mod types;

pub use dispatch::{compile, plan_only_for_test};

pub(crate) fn compile_pg_expression(node: &Node) -> Result<Expr> {
    compile_expr(node)
}

pub(crate) fn compile_pg_projections(nodes: &[Node]) -> Result<Vec<crate::ast::Projection>> {
    compile_projections(nodes)
}

use names::render_relation_component;
pub(super) use names::{
    compile_on_commit, compile_qualified_name, range_var_name, relation_persistence,
    validate_create_table_envelope,
};
pub(in crate::compiler) use returning::compile_returning_clause;

use tree::{
    compile_column_def, compile_create_index, compile_create_table, compile_expr,
    compile_from_node, compile_insert, compile_projections, compile_select, compile_values_lists,
    compile_with_clause, extract_string,
};

#[cfg(test)]
mod tests;
