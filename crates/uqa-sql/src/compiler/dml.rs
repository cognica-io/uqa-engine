//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE and DELETE statement lowering.

use super::{
    compile_expr, compile_from_node, compile_returning_clause, compile_with_clause,
    range_var_alias, range_var_name, DeleteStmt, NodeEnum, Result, SQLError, UpdateStmt,
};

pub(super) fn compile_update(stmt: &pg_query::protobuf::UpdateStmt) -> Result<UpdateStmt> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("UPDATE without relation".into()))?;
    let table = range_var_name(relation);
    let target_qualifier = relation
        .alias
        .as_ref()
        .map(|alias| alias.aliasname.as_str())
        .filter(|alias| !alias.is_empty())
        .unwrap_or(&relation.relname)
        .to_string();
    let assignments = compile_set_clause(&stmt.target_list, "UPDATE")?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let from = match stmt.from_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let (returning, returning_aliases) = compile_returning_clause(stmt.returning_clause.as_ref())?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(UpdateStmt {
        table,
        target_relation_bound: false,
        target_qualifier,
        target_alias: range_var_alias(relation),
        include_descendants: relation.inh,
        assignments,
        r#where,
        with,
        from,
        returning,
        returning_aliases,
    })
}

/// The `SET` list of `UPDATE`, `INSERT ... ON CONFLICT DO UPDATE` and `MERGE ... UPDATE`, as `transformUpdateTargetList` reads it: an item `(a, b) = ROW(...)` assigns each column the row's element at its position.
pub(super) fn compile_set_clause(
    targets: &[pg_query::protobuf::Node],
    statement: &str,
) -> Result<Vec<(crate::ast::AssignmentTarget, crate::ast::Expr)>> {
    targets
        .iter()
        .map(|node| {
            let Some(NodeEnum::ResTarget(target)) = node.node.as_ref() else {
                return Err(SQLError::Internal(format!(
                    "{statement} contains a malformed assignment"
                )));
            };
            let value = target.val.as_ref().ok_or_else(|| {
                SQLError::Internal(format!("{statement} assignment without value"))
            })?;
            let value = match value.node.as_ref() {
                Some(NodeEnum::MultiAssignRef(reference)) => multiple_column_value(reference)?,
                _ => compile_expr(value)?,
            };
            Ok((compile_assignment_target(target)?, value))
        })
        .collect()
}

/// `transformMultiAssignRef`: the value one column of a multiple-column `SET` item takes.
fn multiple_column_value(
    reference: &pg_query::protobuf::MultiAssignRef,
) -> Result<crate::ast::Expr> {
    use pg_query::protobuf::SubLinkType;
    match reference
        .source
        .as_ref()
        .and_then(|source| source.node.as_ref())
    {
        Some(NodeEnum::RowExpr(row)) => {
            if usize::try_from(reference.ncolumns).ok() != Some(row.args.len()) {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "number of columns does not match number of values".into(),
                });
            }
            let element = usize::try_from(reference.colno)
                .ok()
                .and_then(|column| column.checked_sub(1))
                .and_then(|index| row.args.get(index))
                .ok_or_else(|| {
                    SQLError::Internal("multiple-column assignment outside its row".into())
                })?;
            compile_expr(element)
        }
        Some(NodeEnum::SubLink(link)) if link.sub_link_type() == SubLinkType::ExprSublink => Err(
            SQLError::Unsupported("multiple-column assignment from a sub-SELECT".into()),
        ),
        _ => Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message:
                "source for a multiple-column UPDATE item must be a sub-SELECT or ROW() expression"
                    .into(),
        }),
    }
}

pub(super) fn compile_assignment_target(
    target: &pg_query::protobuf::ResTarget,
) -> Result<crate::ast::AssignmentTarget> {
    use crate::ast::{AssignmentStep, AssignmentTarget};
    let mut indirection = Vec::with_capacity(target.indirection.len());
    for step in &target.indirection {
        indirection.push(match step.node.as_ref() {
            Some(NodeEnum::String(field)) => AssignmentStep::Field(field.sval.clone()),
            Some(NodeEnum::AIndices(index)) if index.is_slice => AssignmentStep::Slice {
                lower: index
                    .lidx
                    .as_deref()
                    .map(compile_expr)
                    .transpose()?
                    .map(Box::new),
                upper: index
                    .uidx
                    .as_deref()
                    .map(compile_expr)
                    .transpose()?
                    .map(Box::new),
            },
            Some(NodeEnum::AIndices(index)) => {
                AssignmentStep::Index(Box::new(compile_expr(index.uidx.as_deref().ok_or_else(
                    || SQLError::Internal("assignment subscript has no index".into()),
                )?)?))
            }
            other => {
                return Err(SQLError::Internal(format!(
                    "malformed assignment indirection: {other:?}"
                )))
            }
        });
    }
    Ok(AssignmentTarget {
        column: target.name.clone(),
        indirection,
    })
}

pub(super) fn compile_delete(stmt: &pg_query::protobuf::DeleteStmt) -> Result<DeleteStmt> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("DELETE without relation".into()))?;
    let table = range_var_name(relation);
    let target_qualifier = relation
        .alias
        .as_ref()
        .map(|alias| alias.aliasname.as_str())
        .filter(|alias| !alias.is_empty())
        .unwrap_or(&relation.relname)
        .to_string();
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let using = match stmt.using_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let (returning, returning_aliases) = compile_returning_clause(stmt.returning_clause.as_ref())?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(DeleteStmt {
        table,
        target_relation_bound: false,
        target_qualifier,
        target_alias: range_var_alias(relation),
        include_descendants: relation.inh,
        r#where,
        with,
        using,
        returning,
        returning_aliases,
    })
}
