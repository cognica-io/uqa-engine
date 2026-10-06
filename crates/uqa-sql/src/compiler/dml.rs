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

/// Preserve one query source for a multiple-column SET item; row constructors retain their independent element expressions.
pub(super) fn compile_set_clause(
    targets: &[pg_query::protobuf::Node],
    statement: &str,
) -> Result<Vec<(crate::ast::AssignmentTargets, crate::ast::Expr)>> {
    let mut assignments = Vec::new();
    let mut position = 0;
    while let Some(node) = targets.get(position) {
        let Some(NodeEnum::ResTarget(target)) = node.node.as_ref() else {
            return Err(SQLError::Internal(format!(
                "{statement} contains a malformed assignment"
            )));
        };
        let value = target
            .val
            .as_ref()
            .ok_or_else(|| SQLError::Internal(format!("{statement} assignment without value")))?;
        if let Some(NodeEnum::MultiAssignRef(reference)) = value.node.as_ref() {
            if reference.source.as_ref().is_some_and(|source| {
                matches!(
                    source.node.as_ref(), Some(NodeEnum::SubLink(link))
                        if link.sub_link_type() == pg_query::protobuf::SubLinkType::ExprSublink
                )
            }) {
                let width = usize::try_from(reference.ncolumns)
                    .ok()
                    .filter(|n| *n != 0)
                    .ok_or_else(|| SQLError::Internal("empty multiple-column assignment".into()))?;
                let mut group = Vec::with_capacity(width);
                for offset in 0..width {
                    let Some(NodeEnum::ResTarget(member)) = targets
                        .get(position + offset)
                        .and_then(|node| node.node.as_ref())
                    else {
                        return Err(SQLError::Internal(
                            "incomplete multiple-column assignment".into(),
                        ));
                    };
                    let Some(NodeEnum::MultiAssignRef(part)) =
                        member.val.as_ref().and_then(|node| node.node.as_ref())
                    else {
                        return Err(SQLError::Internal(
                            "invalid multiple-column assignment member".into(),
                        ));
                    };
                    if usize::try_from(part.colno).ok() != Some(offset + 1)
                        || part.ncolumns != reference.ncolumns
                    {
                        return Err(SQLError::Internal(
                            "unordered multiple-column assignment".into(),
                        ));
                    }
                    group.push(compile_assignment_target(member)?);
                }
                assignments.push((
                    crate::ast::AssignmentTargets::Multiple(group.into()),
                    compile_expr(reference.source.as_ref().expect("subquery source"))?,
                ));
                position += width;
                continue;
            }
        }
        let value = match value.node.as_ref() {
            Some(NodeEnum::MultiAssignRef(reference)) => multiple_column_value(reference)?,
            _ => compile_expr(value)?,
        };
        assignments.push((compile_assignment_target(target)?.into(), value));
        position += 1;
    }
    Ok(assignments)
}

/// `transformMultiAssignRef`: the value one column of a multiple-column `SET` item takes.
fn multiple_column_value(
    reference: &pg_query::protobuf::MultiAssignRef,
) -> Result<crate::ast::Expr> {
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
