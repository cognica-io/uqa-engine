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
use crate::ast::{OperatorJoinRelations, TableFunction};

fn compile_relation_argument(node: &Node, function_name: &str, side: &str) -> Result<String> {
    let Some(NodeEnum::ColumnRef(reference)) = node.node.as_ref() else {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}.{side}_relation must be a table identifier, not a scalar value"
        )));
    };
    let mut parts = Vec::with_capacity(reference.fields.len());
    for field in &reference.fields {
        let Some(NodeEnum::String(name)) = field.node.as_ref() else {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}.{side}_relation must be a table identifier"
            )));
        };
        if name.sval.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "{function_name}.{side}_relation contains an empty identifier"
            )));
        }
        parts.push(render_relation_component(&name.sval));
    }
    match parts.len() {
        1 | 2 => Ok(parts.join(".")),
        _ => Err(SQLError::Unsupported(format!(
            "{function_name}.{side}_relation: cross-database relation names are not supported"
        ))),
    }
}

fn range_function_pair(node: &Node) -> Result<(&Node, &[Node])> {
    let Some(NodeEnum::List(pair)) = node.node.as_ref() else {
        return Ok((node, &[]));
    };
    let call = pair
        .items
        .first()
        .ok_or_else(|| SQLError::Internal("RangeFunction empty pair".into()))?;
    let column_definitions = match pair.items.get(1).and_then(|node| node.node.as_ref()) {
        Some(NodeEnum::List(definitions)) => definitions.items.as_slice(),
        None => &[],
        Some(other) => {
            return Err(SQLError::Internal(format!(
                "RangeFunction column-definition list has unexpected node {other:?}"
            )));
        }
    };
    Ok((call, column_definitions))
}

fn expands_multi_argument_unnest(
    call: &pg_query::protobuf::FuncCall,
    column_definitions: &[Node],
) -> Result<bool> {
    let names = extract_strings(&call.funcname)?;
    Ok(names.as_slice() == ["unnest"]
        && call.args.len() > 1
        && call.agg_order.is_empty()
        && call.agg_filter.is_none()
        && call.over.is_none()
        && !call.agg_star
        && !call.agg_distinct
        && !call.func_variadic
        && column_definitions.is_empty())
}

fn compile_table_function(call: &Node, column_definitions: &[Node]) -> Result<TableFunction> {
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
    let relations = if crate::registry::is_operator_join_table_function(&name) {
        let left_node = raw_call.args.first().ok_or_else(|| SQLError::BadArity {
            name: name.clone(),
            expected: "left relation and operand followed by right relation and operand".into(),
            actual: 0,
        })?;
        let right_node = raw_call.args.get(2).ok_or_else(|| SQLError::BadArity {
            name: name.clone(),
            expected: "left relation and operand followed by right relation and operand".into(),
            actual: raw_call.args.len(),
        })?;
        let relations = OperatorJoinRelations {
            left: compile_relation_argument(left_node, &name, "left")?,
            right: compile_relation_argument(right_node, &name, "right")?,
        };
        args.remove(2);
        args.remove(0);
        Some(relations)
    } else {
        None
    };
    let column_definitions = compile_column_definitions(column_definitions)?;
    Ok(TableFunction {
        name,
        binding: None,
        output_name,
        relations,
        args,
        column_aliases: column_definitions
            .iter()
            .map(|(name, _)| name.clone())
            .collect(),
        column_types: column_definitions
            .into_iter()
            .map(|(_, data_type)| data_type)
            .collect(),
    })
}

fn compile_range_function_members(node: &Node) -> Result<Vec<TableFunction>> {
    let (call, column_definitions) = range_function_pair(node)?;
    let Some(NodeEnum::FuncCall(raw_call)) = call.node.as_ref() else {
        return Err(SQLError::Internal(
            "RangeFunction lost its function-call parse node".into(),
        ));
    };
    if expands_multi_argument_unnest(raw_call, column_definitions)? {
        return raw_call
            .args
            .iter()
            .map(|argument| {
                Ok(TableFunction {
                    name: "pg_catalog.unnest".into(),
                    binding: None,
                    output_name: "unnest".into(),
                    relations: None,
                    args: vec![compile_expr(argument)?],
                    column_aliases: Vec::new(),
                    column_types: Vec::new(),
                })
            })
            .collect();
    }
    Ok(vec![compile_table_function(call, column_definitions)?])
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
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
            let (alias, column_aliases) = compile_alias(r.alias.as_ref())?;
            Ok(FromClause::Table {
                name: range_var_name(r),
                qualifier: r.relname.clone(),
                alias,
                column_aliases,
                include_descendants: r.inh,
            })
        }
        NodeEnum::JoinExpr(j) => {
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
            let (alias, column_aliases) = compile_alias(j.alias.as_ref())?;
            Ok(FromClause::Join {
                left: Box::new(compile_from_node(left)?),
                right: Box::new(compile_from_node(right)?),
                kind,
                on,
                using,
                natural: j.is_natural,
                alias,
                column_aliases,
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
                        internal_relation: None,
                        internal_column_types: Vec::new(),
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
            if rf.functions.is_empty() {
                return Err(SQLError::Internal("RangeFunction without functions".into()));
            }
            let functions = rf
                .functions
                .iter()
                .map(compile_range_function_members)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let (alias, column_aliases) = compile_alias(rf.alias.as_ref())?;
            if rf.is_rowsfrom || functions.len() > 1 {
                if !rf.coldeflist.is_empty() {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: "a column definition list cannot be applied to a function group"
                            .into(),
                    });
                }
                return Ok(FromClause::FunctionGroup {
                    functions,
                    alias,
                    column_aliases,
                    ordinality: rf.ordinality,
                });
            }
            let mut function = functions.into_iter().next().ok_or_else(|| {
                SQLError::Internal("RangeFunction produced no function members".into())
            })?;
            let coldefs = compile_column_definitions(&rf.coldeflist)?;
            let column_types: Vec<String> = coldefs.iter().map(|(_, ty)| ty.clone()).collect();
            let coldef_aliases: Vec<String> = coldefs.into_iter().map(|(name, _)| name).collect();
            Ok(FromClause::Function {
                name: function.name,
                binding: function.binding,
                output_name: function.output_name,
                relations: function.relations,
                args: function.args,
                alias,
                column_aliases: if coldef_aliases.is_empty() {
                    if function.column_aliases.is_empty() {
                        column_aliases
                    } else {
                        std::mem::take(&mut function.column_aliases)
                    }
                } else {
                    coldef_aliases
                },
                ordinality: rf.ordinality,
                column_types: if column_types.is_empty() {
                    function.column_types
                } else {
                    column_types
                },
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
                let type_name =
                    match super::super::types::compile_pg_type_name(type_node, &col.colname) {
                        Ok(column_type) => {
                            let mut type_name = extract_strings(&type_node.names)?
                                .last()
                                .ok_or_else(|| {
                                    SQLError::Internal(format!(
                                        "function column `{}` has an empty type name",
                                        col.colname
                                    ))
                                })?
                                .to_ascii_lowercase();
                            if !type_node.typmods.is_empty() {
                                let rendered = column_type.sql_name();
                                if let Some(modifier) = rendered.find('(') {
                                    let modifier_end = rendered[modifier..]
                                        .find(')')
                                        .map(|position| modifier + position + 1)
                                        .ok_or_else(|| {
                                        SQLError::Internal(format!(
                                            "function column `{}` has an unterminated type modifier",
                                            col.colname
                                        ))
                                    })?;
                                    type_name.push_str(&rendered[modifier..modifier_end]);
                                }
                            }
                            for _ in &type_node.array_bounds {
                                type_name.push_str("[]");
                            }
                            type_name
                        }
                        Err(SQLError::Unsupported(_)) => {
                            let names = extract_strings(&type_node.names)?;
                            if names.is_empty() {
                                return Err(SQLError::Internal(format!(
                                    "function column `{}` has an empty type name",
                                    col.colname
                                )));
                            }
                            let mut type_name = names.join(".").to_ascii_lowercase();
                            for _ in &type_node.array_bounds {
                                type_name.push_str("[]");
                            }
                            type_name
                        }
                        Err(error) => return Err(error),
                    };
                Ok((col.colname.clone(), type_name))
            }
            other => Err(SQLError::Unsupported(format!(
                "function column definition: {other:?}"
            ))),
        })
        .collect()
}
