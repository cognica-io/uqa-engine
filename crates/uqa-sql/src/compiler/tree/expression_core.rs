//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core expression dispatch, indirection, sublinks, and CASE lowering.

use super::{
    compile_a_expr, compile_bool_expr, compile_column_ref, compile_const, compile_func_call,
    compile_null_test, compile_select, compile_type_cast, extract_strings, Expr, Node, NodeEnum,
    Result, SQLError, Value,
};

pub(in crate::compiler) fn compile_expr(node: &Node) -> Result<Expr> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing expr node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => compile_const(c),
        NodeEnum::ColumnRef(c) => compile_column_ref(c),
        NodeEnum::ParamRef(p) => {
            let index = usize::try_from(p.number).map_err(|_| {
                SQLError::Internal(format!(
                    "parameter index must be positive, got {}",
                    p.number
                ))
            })?;
            if index == 0 {
                return Err(SQLError::Internal(
                    "parameter index must be greater than zero".into(),
                ));
            }
            Ok(Expr::Param(index))
        }
        NodeEnum::FuncCall(f) => compile_func_call(f),
        NodeEnum::NamedArgExpr(arg) => {
            if arg.name.is_empty() {
                return Err(SQLError::Internal(
                    "NamedArgExpr without an argument name".into(),
                ));
            }
            let Some(value_node) = arg.arg.as_ref() else {
                return Err(SQLError::Internal("NamedArgExpr without value".into()));
            };
            Ok(Expr::Func {
                name: "__named_arg".into(),
                args: vec![
                    Expr::Literal(Value::Str(arg.name.clone())),
                    compile_expr(value_node)?,
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        NodeEnum::AArrayExpr(a) => {
            let elements: Vec<Expr> = a
                .elements
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Array(elements))
        }
        NodeEnum::TypeCast(tc) => compile_type_cast(tc),
        NodeEnum::AExpr(a) => compile_a_expr(a),
        NodeEnum::SqlvalueFunction(svf) => compile_sql_value_function(svf),
        NodeEnum::MergeSupportFunc(_) => Ok(Expr::Func {
            name: "merge_action".into(),
            args: Vec::new(),
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }),
        NodeEnum::BoolExpr(b) => compile_bool_expr(b),
        NodeEnum::NullTest(n) => compile_null_test(n),
        NodeEnum::CaseExpr(c) => compile_case_expr(c),
        NodeEnum::CoalesceExpr(ce) => {
            if ce.args.is_empty() {
                return Err(SQLError::Internal("COALESCE without arguments".into()));
            }
            let args: Vec<Expr> = ce
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Func {
                name: "coalesce".into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        NodeEnum::MinMaxExpr(me) => {
            use pg_query::protobuf::MinMaxOp;
            let name = match me.op() {
                MinMaxOp::IsGreatest => "greatest",
                MinMaxOp::IsLeast => "least",
                _ => {
                    return Err(SQLError::Unsupported(format!(
                        "MinMaxExpr op {:?}",
                        me.op()
                    )));
                }
            };
            let args: Vec<Expr> = me
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            if args.is_empty() {
                return Err(SQLError::Internal(format!(
                    "{} without arguments",
                    name.to_ascii_uppercase()
                )));
            }
            Ok(Expr::Func {
                name: name.into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        NodeEnum::SubLink(sl) => compile_sublink(sl),
        // ROW(a, b) constructors compare element-wise; the evaluator
        // reuses the list comparison rules for them.
        NodeEnum::RowExpr(row) => {
            let elements: Vec<Expr> = row
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Array(elements))
        }
        NodeEnum::AIndirection(ind) => compile_indirection(ind),
        other => Err(SQLError::Unsupported(format!("expression form: {other:?}"))),
    }
}

/// `expr[i]`, `expr[lo:hi]`, and chains thereof. Subscripts are
/// 1-based; slices clamp to the array, both per `PostgreSQL`.
pub(in crate::compiler) fn compile_indirection(
    ind: &pg_query::protobuf::AIndirection,
) -> Result<Expr> {
    let base = ind
        .arg
        .as_deref()
        .ok_or_else(|| SQLError::Internal("AIndirection without base".into()))?;
    let mut current = compile_expr(base)?;
    if ind.indirection.is_empty() {
        return Err(SQLError::Internal(
            "AIndirection without indirection steps".into(),
        ));
    }
    for step in &ind.indirection {
        let inner = step
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("indirection contains an empty step".into()))?;
        match inner {
            NodeEnum::AIndices(idx) => {
                if idx.is_slice {
                    let lower = idx
                        .lidx
                        .as_deref()
                        .map(compile_expr)
                        .transpose()?
                        .unwrap_or(Expr::Literal(Value::Null));
                    let upper = idx
                        .uidx
                        .as_deref()
                        .map(compile_expr)
                        .transpose()?
                        .unwrap_or(Expr::Literal(Value::Null));
                    current = Expr::Func {
                        name: "__slice".into(),
                        args: vec![current, lower, upper],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    };
                } else {
                    let index = idx
                        .uidx
                        .as_deref()
                        .map(compile_expr)
                        .transpose()?
                        .ok_or_else(|| SQLError::Internal("subscript without index".into()))?;
                    current = Expr::Func {
                        name: "__subscript".into(),
                        args: vec![current, index],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    };
                }
            }
            NodeEnum::String(field) => {
                if field.sval.is_empty() {
                    return Err(SQLError::Internal(
                        "indirection contains an empty field name".into(),
                    ));
                }
                // `(composite).field` access on map values.
                current = Expr::Func {
                    name: "__subscript".into(),
                    args: vec![current, Expr::Literal(Value::Str(field.sval.clone()))],
                    distinct: false,
                    order_by: Vec::new(),
                    filter: None,
                };
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "indirection step: {other:?}"
                )));
            }
        }
    }
    Ok(current)
}

pub(in crate::compiler) fn compile_sql_value_function(
    svf: &pg_query::protobuf::SqlValueFunction,
) -> Result<Expr> {
    use pg_query::protobuf::SqlValueFunctionOp;
    let name = match svf.op() {
        SqlValueFunctionOp::SvfopCurrentDate => "current_date",
        SqlValueFunctionOp::SvfopCurrentTimestamp
        | SqlValueFunctionOp::SvfopCurrentTimestampN
        | SqlValueFunctionOp::SvfopLocaltimestamp
        | SqlValueFunctionOp::SvfopLocaltimestampN
        | SqlValueFunctionOp::SvfopCurrentTime
        | SqlValueFunctionOp::SvfopCurrentTimeN
        | SqlValueFunctionOp::SvfopLocaltime
        | SqlValueFunctionOp::SvfopLocaltimeN => "current_timestamp",
        SqlValueFunctionOp::SvfopCurrentSchema => "current_schema",
        SqlValueFunctionOp::SvfopCurrentCatalog => "current_database",
        SqlValueFunctionOp::SvfopCurrentUser
        | SqlValueFunctionOp::SvfopCurrentRole
        | SqlValueFunctionOp::SvfopSessionUser
        | SqlValueFunctionOp::SvfopUser => "current_user",
        other => {
            return Err(SQLError::Unsupported(format!(
                "SQL value function {other:?}"
            )));
        }
    };
    Ok(Expr::Func {
        name: name.into(),
        args: Vec::new(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    })
}

pub(in crate::compiler) fn compile_sublink(sl: &pg_query::protobuf::SubLink) -> Result<Expr> {
    use pg_query::protobuf::SubLinkType;
    let body_node = sl
        .subselect
        .as_deref()
        .ok_or_else(|| SQLError::Internal("SubLink without subselect".into()))?;
    let inner_select = match body_node.node.as_ref() {
        Some(NodeEnum::SelectStmt(s)) => compile_select(s)?,
        _ => {
            return Err(SQLError::Unsupported("SubLink body must be SELECT".into()));
        }
    };
    let body = Box::new(inner_select);
    let operator = if sl.oper_name.is_empty() {
        None
    } else {
        Some(extract_strings(&sl.oper_name)?.join(""))
    };
    match sl.sub_link_type() {
        SubLinkType::ExprSublink => {
            if sl.testexpr.is_some() || operator.is_some() {
                return Err(SQLError::Internal(
                    "scalar SubLink unexpectedly has a test expression or operator".into(),
                ));
            }
            Ok(Expr::ScalarSubquery(body))
        }
        SubLinkType::ExistsSublink => {
            if sl.testexpr.is_some() || operator.is_some() {
                return Err(SQLError::Internal(
                    "EXISTS SubLink unexpectedly has a test expression or operator".into(),
                ));
            }
            Ok(Expr::Exists {
                body,
                negated: false,
            })
        }
        SubLinkType::AnySublink => {
            if !matches!(operator.as_deref(), None | Some("=")) {
                return Err(SQLError::Unsupported(format!(
                    "ANY subquery operator `{}` is not represented by InSubquery",
                    operator.as_deref().unwrap_or("")
                )));
            }
            let testexpr = sl
                .testexpr
                .as_deref()
                .ok_or_else(|| SQLError::Internal("ANY SubLink without testexpr".into()))?;
            Ok(Expr::InSubquery {
                expr: Box::new(compile_expr(testexpr)?),
                body,
                negated: false,
            })
        }
        SubLinkType::AllSublink => {
            if operator.as_deref() != Some("<>") {
                return Err(SQLError::Unsupported(format!(
                    "ALL subquery operator `{}` is not represented by InSubquery",
                    operator.as_deref().unwrap_or("")
                )));
            }
            // `lhs <> ALL (subquery)` is SQL's `lhs NOT IN (subquery)`.
            let testexpr = sl
                .testexpr
                .as_deref()
                .ok_or_else(|| SQLError::Internal("ALL SubLink without testexpr".into()))?;
            Ok(Expr::InSubquery {
                expr: Box::new(compile_expr(testexpr)?),
                body,
                negated: true,
            })
        }
        other => Err(SQLError::Unsupported(format!("SubLink type {other:?}"))),
    }
}

pub(in crate::compiler) fn compile_case_expr(c: &pg_query::protobuf::CaseExpr) -> Result<Expr> {
    let base = c
        .arg
        .as_ref()
        .map(|n| compile_expr(n))
        .transpose()?
        .map(Box::new);
    let mut when: Vec<(Expr, Expr)> = Vec::with_capacity(c.args.len());
    if c.args.is_empty() {
        return Err(SQLError::Internal(
            "CASE expression without WHEN arms".into(),
        ));
    }
    for arm in &c.args {
        let inner = arm
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE arm without body".into()))?;
        let NodeEnum::CaseWhen(cw) = inner else {
            return Err(SQLError::Internal(format!(
                "CASE arm expected CaseWhen, got {inner:?}"
            )));
        };
        let cond = cw
            .expr
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE WHEN without cond".into()))?;
        let result = cw
            .result
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE WHEN without THEN".into()))?;
        when.push((compile_expr(cond)?, compile_expr(result)?));
    }
    let else_branch = c
        .defresult
        .as_ref()
        .map(|n| compile_expr(n))
        .transpose()?
        .map(Box::new);
    Ok(Expr::Case {
        base,
        when,
        else_branch,
    })
}
