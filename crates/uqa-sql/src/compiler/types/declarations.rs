//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain ALTER type declarations until the locked target admits their analysis.

use super::{compile_pg_type_name, extract_string, parse_regtype_name};
use crate::{ColumnType, SQLError};
use pg_query::{
    protobuf::{LimitOption, Node, ResTarget, SelectStmt, SetOperation, TypeName},
    NodeEnum,
};

/// A modifier is a parser expression, not necessarily an integer. Preserve its syntax without evaluating or validating it before ALTER checks its target and column.
pub(in crate::compiler) fn preserve_alter_type_declaration(
    ty: &TypeName,
    column: &str,
) -> Result<ColumnType, SQLError> {
    if ty.typmods.is_empty() {
        if let Ok(compiled) = compile_pg_type_name(ty, column) {
            return Ok(compiled);
        }
    }
    let mut declaration = ty
        .names
        .iter()
        .map(|node| extract_string(node).map(|name| format!("\"{}\"", name.replace('"', "\"\""))))
        .collect::<Result<Vec<_>, _>>()?
        .join(".");
    if !ty.typmods.is_empty() {
        declaration.push('(');
        declaration.push_str(
            &ty.typmods
                .iter()
                .map(modifier_sql)
                .collect::<Result<Vec<_>, _>>()?
                .join(", "),
        );
        declaration.push(')');
    }
    declaration.push_str(&"[]".repeat(ty.array_bounds.len()));
    Ok(ColumnType::Named(declaration))
}

fn modifier_sql(modifier: &Node) -> Result<String, SQLError> {
    let statement = NodeEnum::SelectStmt(Box::new(SelectStmt {
        target_list: vec![Node {
            node: Some(NodeEnum::ResTarget(Box::new(ResTarget {
                val: Some(Box::new(modifier.clone())),
                ..ResTarget::default()
            }))),
        }],
        op: SetOperation::SetopNone as i32,
        limit_option: LimitOption::Default as i32,
        ..SelectStmt::default()
    }));
    let sql = statement.deparse()?;
    sql.strip_prefix("SELECT ")
        .map(str::to_owned)
        .ok_or_else(|| {
            SQLError::Internal("type modifier did not deparse as a SELECT target".into())
        })
}

/// Run the ordinary declaration compiler at the admitted ALTER boundary. The type-name parser first excludes SQL outside the declaration, while the cast wrapper retains the original modifier expression nodes for the shared compiler.
pub(crate) fn compile_retained_type_declaration(name: &str) -> Result<ColumnType, SQLError> {
    if parse_regtype_name(name)?.is_none() {
        return Err(SQLError::TypeMismatch(format!(
            "invalid type declaration `{name}`"
        )));
    }
    let parsed = pg_query::parse(&format!("SELECT NULL::{name}"))?;
    let ty = parsed
        .protobuf
        .stmts
        .first()
        .and_then(|statement| statement.stmt.as_ref())
        .and_then(|node| match node.node.as_ref()? {
            NodeEnum::SelectStmt(select) => select.target_list.first(),
            _ => None,
        })
        .and_then(|node| match node.node.as_ref()? {
            NodeEnum::ResTarget(target) => target.val.as_ref(),
            _ => None,
        })
        .and_then(|node| match node.node.as_ref()? {
            NodeEnum::TypeCast(cast) => cast.type_name.as_ref(),
            _ => None,
        })
        .ok_or_else(|| SQLError::Internal("type declaration lost its parser type name".into()))?;
    compile_pg_type_name(ty, name)
}

#[cfg(test)]
mod tests;
