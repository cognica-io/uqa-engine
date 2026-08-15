//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM sources, joins, aliases, and table-function columns.

use super::{
    compile_expr, compile_select, extract_strings, range_var_name, render_relation_component, Expr,
    FromClause, JoinKind, Node, NodeEnum, Result, SQLError,
};

fn compile_relation_argument(node: &Node, function_name: &str) -> Result<String> {
    let Some(NodeEnum::ColumnRef(reference)) = node.node.as_ref() else {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}.relation must be a table identifier, not a scalar value"
        )));
    };
    let mut parts = Vec::with_capacity(reference.fields.len());
    for field in &reference.fields {
        let Some(NodeEnum::String(name)) = field.node.as_ref() else {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}.relation must be a table identifier"
            )));
        };
        if name.sval.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}.relation contains an empty identifier"
            )));
        }
        parts.push(render_relation_component(&name.sval));
    }
    match parts.len() {
        1 | 2 => Ok(parts.join(".")),
        _ => Err(SQLError::Unsupported(format!(
            "{function_name}.relation: cross-database relation names are not supported"
        ))),
    }
}

pub(in crate::compiler) fn compile_from_node(node: &Node) -> Result<FromClause> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty FROM node".into()));
    };
    match inner {
        NodeEnum::RangeVar(r) => {
            if !r.catalogname.is_empty() {
                return Err(SQLError::Unsupported(
                    "cross-database relation references are not supported".into(),
                ));
            }
            Ok(FromClause::Table {
                name: range_var_name(r),
                qualifier: r.relname.clone(),
                alias: r.alias.as_ref().and_then(|a| {
                    if a.aliasname.is_empty() {
                        None
                    } else {
                        Some(a.aliasname.clone())
                    }
                }),
            })
        }
        NodeEnum::JoinExpr(j) => {
            if j.alias.is_some() {
                return Err(SQLError::Unsupported(
                    "aliases on parenthesized JOIN expressions are not supported".into(),
                ));
            }
            let left = j
                .larg
                .as_ref()
                .ok_or_else(|| SQLError::Internal("JOIN missing left".into()))?;
            let right = j
                .rarg
                .as_ref()
                .ok_or_else(|| SQLError::Internal("JOIN missing right".into()))?;
            let kind = match j.jointype() {
                pg_query::protobuf::JoinType::JoinInner => JoinKind::Inner,
                pg_query::protobuf::JoinType::JoinLeft => JoinKind::Left,
                pg_query::protobuf::JoinType::JoinRight => JoinKind::Right,
                pg_query::protobuf::JoinType::JoinFull => JoinKind::Full,
                other => {
                    return Err(SQLError::Unsupported(format!("JOIN type {other:?}")));
                }
            };
            let on = j.quals.as_deref().map(compile_expr).transpose()?;
            let using = if j.using_clause.is_empty() {
                None
            } else {
                let columns = extract_strings(&j.using_clause)?;
                let alias = j.join_using_alias.as_ref().and_then(|alias| {
                    (!alias.aliasname.is_empty()).then(|| alias.aliasname.clone())
                });
                Some(crate::ast::JoinUsing { columns, alias })
            };
            if j.join_using_alias.is_some() && using.is_none() {
                return Err(SQLError::Internal(
                    "JOIN USING alias has no USING column list".into(),
                ));
            }
            if usize::from(on.is_some()) + usize::from(using.is_some()) + usize::from(j.is_natural)
                > 1
            {
                return Err(SQLError::Internal(
                    "JOIN contains more than one of ON, USING, or NATURAL".into(),
                ));
            }
            let lateral = right_is_lateral(right);
            Ok(FromClause::Join {
                left: Box::new(compile_from_node(left)?),
                right: Box::new(compile_from_node(right)?),
                kind,
                on,
                using,
                natural: j.is_natural,
                lateral,
            })
        }
        NodeEnum::RangeSubselect(rs) => {
            let body_node = rs
                .subquery
                .as_deref()
                .ok_or_else(|| SQLError::Internal("FROM (subquery) without body".into()))?;
            let inner = body_node
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("subquery body empty".into()))?;
            let select = match inner {
                NodeEnum::SelectStmt(s) => compile_select(s)?,
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "FROM (subquery) body must be SELECT, got {other:?}"
                    )));
                }
            };
            // Standalone VALUES land here as a SelectStmt with empty
            // target_list and a values_lists -- promote to
            // FromClause::Values for the engine fast path.
            let (alias, column_aliases) = compile_alias(rs.alias.as_ref())?;
            let body_inner = body_node
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("subquery body empty".into()))?;
            if let NodeEnum::SelectStmt(s) = body_inner {
                if !s.values_lists.is_empty() {
                    let rows = super::compile_values_lists(&s.values_lists)?;
                    return Ok(FromClause::Values {
                        rows,
                        alias,
                        column_aliases,
                    });
                }
            }
            Ok(FromClause::Subquery {
                body: Box::new(select),
                alias,
                column_aliases,
            })
        }
        NodeEnum::RangeFunction(rf) => {
            if rf.is_rowsfrom || rf.functions.len() > 1 {
                return Err(SQLError::Unsupported(
                    "ROWS FROM and multiple table functions in one FROM item are not supported"
                        .into(),
                ));
            }
            if rf.ordinality {
                return Err(SQLError::Unsupported(
                    "table functions WITH ORDINALITY are not supported".into(),
                ));
            }
            // A single function in `functions` carries the call. Re-use
            // compile_expr to lift it into an Expr::Func, then peel back
            // the name and arguments.
            let Some(first_node) = rf.functions.first() else {
                return Err(SQLError::Internal("RangeFunction without functions".into()));
            };
            // RangeFunction.functions is a list of `[FuncCall, alias_definition]`
            // pairs encoded as a List. Take the first element of the
            // first pair as the call.
            let call = match first_node.node.as_ref() {
                Some(NodeEnum::List(l)) => l
                    .items
                    .first()
                    .ok_or_else(|| SQLError::Internal("RangeFunction empty pair".into()))?,
                _ => first_node,
            };
            let Some(NodeEnum::FuncCall(raw_call)) = call.node.as_ref() else {
                return Err(SQLError::Internal(
                    "RangeFunction lost its function-call parse node".into(),
                ));
            };
            let output_name = extract_strings(&raw_call.funcname)?
                .into_iter()
                .last()
                .ok_or_else(|| SQLError::Internal("RangeFunction has an empty name".into()))?;
            let expr = compile_expr(call)?;
            let (name, mut args) = match expr {
                Expr::Func { name, args, .. } => (name, args),
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "RangeFunction body must be a function call, got {other:?}"
                    )));
                }
            };
            let relation = if crate::registry::is_operator_join_table_function(&name) {
                let relation_node = raw_call.args.first().ok_or_else(|| SQLError::BadArity {
                    name: name.clone(),
                    expected: "a relation identifier followed by operator operands".into(),
                    actual: 0,
                })?;
                let relation = compile_relation_argument(relation_node, &name)?;
                args.remove(0);
                Some(relation)
            } else {
                None
            };
            let (alias, column_aliases) = compile_alias(rf.alias.as_ref())?;
            let coldefs = compile_column_definitions(&rf.coldeflist)?;
            let column_types: Vec<String> = coldefs.iter().map(|(_, ty)| ty.clone()).collect();
            let coldef_aliases: Vec<String> = coldefs.into_iter().map(|(name, _)| name).collect();
            Ok(FromClause::Function {
                name,
                output_name,
                relation,
                args,
                alias,
                column_aliases: if coldef_aliases.is_empty() {
                    column_aliases
                } else {
                    coldef_aliases
                },
                column_types,
            })
        }
        other => Err(SQLError::Unsupported(format!("FROM form: {other:?}"))),
    }
}

pub(in crate::compiler) fn right_is_lateral(node: &Node) -> bool {
    match node.node.as_ref() {
        Some(NodeEnum::RangeSubselect(rs)) => rs.lateral,
        Some(NodeEnum::RangeFunction(rf)) => rf.lateral,
        _ => false,
    }
}

pub(in crate::compiler) fn compile_alias(
    alias: Option<&pg_query::protobuf::Alias>,
) -> Result<(Option<String>, Vec<String>)> {
    let Some(a) = alias else {
        return Ok((None, Vec::new()));
    };
    let name = if a.aliasname.is_empty() {
        None
    } else {
        Some(a.aliasname.clone())
    };
    let cols = extract_strings(&a.colnames)?;
    Ok((name, cols))
}

/// Column definition list entries as `(name, lowercased type name)`.
/// The type name is the last component of the `TypeName` path
/// (`pg_catalog.int4` -> `int4`, `agtype` -> `agtype`); empty when the
/// definition omitted a type.
pub(in crate::compiler) fn compile_column_definitions(
    nodes: &[Node],
) -> Result<Vec<(String, String)>> {
    nodes
        .iter()
        .map(|node| match node.node.as_ref() {
            Some(NodeEnum::ColumnDef(col)) => {
                let type_node = col.type_name.as_ref().ok_or_else(|| {
                    SQLError::Internal(format!("function column `{}` has no type", col.colname))
                })?;
                let type_name = extract_strings(&type_node.names)?
                    .last()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "function column `{}` has an empty type name",
                            col.colname
                        ))
                    })?
                    .to_ascii_lowercase();
                Ok((col.colname.clone(), type_name))
            }
            other => Err(SQLError::Unsupported(format!(
                "function column definition: {other:?}"
            ))),
        })
        .collect()
}
