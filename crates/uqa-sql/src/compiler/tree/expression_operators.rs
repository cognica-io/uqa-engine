//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL operator, boolean, and null-test lowering.

use super::expression_core::builtin_syntax_call;
use super::{
    compile_expr, extract_strings, json_path_args, BinaryOp, Expr, NodeEnum, Result, SQLError,
    Value,
};

fn compile_pattern_operands(
    rhs: &pg_query::protobuf::Node,
    wrapper_name: &str,
    operator_name: &str,
) -> Result<Vec<Expr>> {
    let Some(NodeEnum::FuncCall(function)) = rhs.node.as_ref() else {
        return Ok(vec![compile_expr(rhs)?]);
    };
    let function_name = extract_strings(&function.funcname)?
        .into_iter()
        .next_back()
        .ok_or_else(|| SQLError::Internal(format!("{operator_name} wrapper has no name")))?;
    if function_name != wrapper_name {
        return Ok(vec![compile_expr(rhs)?]);
    }
    if function.agg_distinct
        || function.agg_star
        || function.agg_within_group
        || function.func_variadic
        || !function.agg_order.is_empty()
        || function.agg_filter.is_some()
        || function.over.is_some()
    {
        return Err(SQLError::Internal(format!(
            "{operator_name} wrapper contains aggregate/function modifiers"
        )));
    }
    if !matches!(function.args.len(), 1 | 2) {
        return Err(SQLError::Internal(format!(
            "{wrapper_name} expects a pattern and optional escape"
        )));
    }
    function.args.iter().map(compile_expr).collect()
}

pub(in crate::compiler) fn compile_a_expr(a: &pg_query::protobuf::AExpr) -> Result<Expr> {
    use pg_query::protobuf::AExprKind;
    let kind = a.kind();
    match kind {
        AExprKind::AexprOp => {
            let op_name = extract_strings(&a.name)?.join("");
            if a.lexpr.is_none() {
                let rhs = a
                    .rexpr
                    .as_ref()
                    .ok_or_else(|| SQLError::Internal("AExpr missing rhs".into()))?;
                let rhs = compile_expr(rhs)?;
                let unary_func = |name: &str, arg: Expr| Expr::Func {
                    binding: None,
                    name: name.into(),
                    args: vec![arg],
                    distinct: false,
                    order_by: Vec::new(),
                    filter: None,
                };
                return match op_name.as_str() {
                    "+" => Ok(rhs),
                    "-" => Ok(Expr::UnaryMinus(Box::new(rhs))),
                    // |/ square root, ||/ cube root, @ absolute value.
                    "|/" => Ok(unary_func("sqrt", rhs)),
                    "||/" => Ok(unary_func("cbrt", rhs)),
                    "@" => Ok(unary_func("abs", rhs)),
                    other => Err(SQLError::Unsupported(format!("unary operator `{other}`"))),
                };
            }
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("AExpr missing lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("AExpr missing rhs".into()))?;
            let op = match op_name.as_str() {
                "=" => BinaryOp::Equal,
                "<>" | "!=" => BinaryOp::NotEqual,
                "<" => BinaryOp::Less,
                "<=" => BinaryOp::LessEqual,
                ">" => BinaryOp::Greater,
                ">=" => BinaryOp::GreaterEqual,
                "+" => BinaryOp::Add,
                "-" => BinaryOp::Subtract,
                "*" => BinaryOp::Multiply,
                "/" => BinaryOp::Divide,
                // String concatenation: rewrite `a || b` into a
                // concat_op() call. concat_op propagates NULL the way
                // the SQL `||` operator must (`'x' || NULL == NULL`),
                // which is distinct from PostgreSQL's `CONCAT()` that
                // skips NULL arguments.
                "||" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "concat_op".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "@@" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "fts_match".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "@?" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "jsonpath_exists".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "%" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "mod".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "^" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "power".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                // POSIX regex operators: `~` match, `~*` case-insensitive
                // match, `!~` / `!~*` their negations.
                "~" | "~*" | "!~" | "!~*" => {
                    let mut args = vec![compile_expr(lhs)?, compile_expr(rhs)?];
                    if op_name.ends_with('*') {
                        args.push(Expr::Literal(Value::Str("i".into())));
                    }
                    let call = Expr::Func {
                        binding: None,
                        name: "regexp_like".into(),
                        args,
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    };
                    return Ok(if op_name.starts_with('!') {
                        Expr::Not(Box::new(call))
                    } else {
                        call
                    });
                }
                // Array overlap.
                "&&" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "array_overlap".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "-|-" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "range_adjacent".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "~~" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "like".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "~~*" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "ilike".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "!~~" => {
                    return Ok(Expr::Not(Box::new(Expr::Func {
                        binding: None,
                        name: "like".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    })));
                }
                "!~~*" => {
                    return Ok(Expr::Not(Box::new(Expr::Func {
                        binding: None,
                        name: "ilike".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    })));
                }
                "->" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_extract_path".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "->>" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_extract_path_text".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "#>" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_extract_path".into(),
                        args: json_path_args(compile_expr(lhs)?, compile_expr(rhs)?),
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "#>>" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_extract_path_text".into(),
                        args: json_path_args(compile_expr(lhs)?, compile_expr(rhs)?),
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "#-" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_delete_path".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "@>" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "contains_op".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "<@" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "contained_by_op".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "?" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_has_key".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "?|" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_has_any_key".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "?&" => {
                    return Ok(Expr::Func {
                        binding: None,
                        name: "json_has_all_keys".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                other => return Err(SQLError::Unsupported(format!("operator `{other}`"))),
            };
            Ok(Expr::Binary {
                op,
                lhs: Box::new(compile_expr(lhs)?),
                rhs: Box::new(compile_expr(rhs)?),
            })
        }
        AExprKind::AexprBetween | AExprKind::AexprNotBetween => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without rhs".into()))?;
            let bounds = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) if l.items.len() == 2 => l.items.clone(),
                _ => return Err(SQLError::Internal("BETWEEN expects 2 bounds".into())),
            };
            let between = Expr::Between {
                expr: Box::new(compile_expr(expr)?),
                low: Box::new(compile_expr(&bounds[0])?),
                high: Box::new(compile_expr(&bounds[1])?),
            };
            Ok(if matches!(kind, AExprKind::AexprNotBetween) {
                Expr::Not(Box::new(between))
            } else {
                between
            })
        }
        AExprKind::AexprBetweenSym | AExprKind::AexprNotBetweenSym => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without rhs".into()))?;
            let bounds = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) if l.items.len() == 2 => l.items.clone(),
                _ => return Err(SQLError::Internal("BETWEEN expects 2 bounds".into())),
            };
            let call = Expr::Func {
                binding: None,
                name: "__between_symmetric".into(),
                args: vec![
                    compile_expr(expr)?,
                    compile_expr(&bounds[0])?,
                    compile_expr(&bounds[1])?,
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            Ok(if matches!(kind, AExprKind::AexprNotBetweenSym) {
                Expr::Not(Box::new(call))
            } else {
                call
            })
        }
        AExprKind::AexprDistinct | AExprKind::AexprNotDistinct => {
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IS DISTINCT FROM without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IS DISTINCT FROM without rhs".into()))?;
            let call = Expr::Func {
                binding: None,
                name: "__is_distinct".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            Ok(if matches!(kind, AExprKind::AexprNotDistinct) {
                Expr::Not(Box::new(call))
            } else {
                call
            })
        }
        AExprKind::AexprSimilar => {
            // `expr SIMILAR TO pattern` arrives with the pattern
            // wrapped in `similar_to_escape(pattern[, escape])`.
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SIMILAR TO without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SIMILAR TO without rhs".into()))?;
            let mut args = vec![compile_expr(lhs)?];
            args.extend(compile_pattern_operands(
                rhs,
                "similar_to_escape",
                "SIMILAR TO",
            )?);
            let call = Expr::Func {
                binding: None,
                name: "similar_to".into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            match op_name.as_str() {
                "~" => Ok(call),
                "!~" => Ok(Expr::Not(Box::new(call))),
                other => Err(SQLError::Internal(format!(
                    "SIMILAR TO has unexpected operator `{other}`"
                ))),
            }
        }
        AExprKind::AexprOpAny | AExprKind::AexprOpAll => {
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ANY/ALL without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ANY/ALL without rhs".into()))?;
            let name = if matches!(kind, AExprKind::AexprOpAny) {
                "__any_op"
            } else {
                "__all_op"
            };
            Ok(Expr::Func {
                binding: None,
                name: name.into(),
                args: vec![
                    compile_expr(lhs)?,
                    compile_expr(rhs)?,
                    Expr::Literal(Value::Str(op_name)),
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        AExprKind::AexprNullif => {
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("NULLIF without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("NULLIF without rhs".into()))?;
            return Ok(builtin_syntax_call(
                "nullif",
                vec![compile_expr(lhs)?, compile_expr(rhs)?],
            ));
        }
        AExprKind::AexprLike => {
            // libpg_query encodes LIKE as `~~` and NOT LIKE as `!~~` in
            // `a.name`. The keyword form lands here regardless of the
            // user's syntax (LIKE / NOT LIKE / ~~ / !~~), so we have to
            // peek at the name to recover the negation.
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without rhs".into()))?;
            let mut args = vec![compile_expr(lhs)?];
            args.extend(compile_pattern_operands(rhs, "like_escape", "LIKE")?);
            let func = Expr::Func {
                binding: None,
                name: "like".into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            return match op_name.as_str() {
                "~~" => Ok(func),
                "!~~" => Ok(Expr::Not(Box::new(func))),
                other => Err(SQLError::Internal(format!(
                    "LIKE has unexpected operator `{other}`"
                ))),
            };
        }
        AExprKind::AexprIlike => {
            // Same shape as AexprLike: ILIKE -> `~~*`, NOT ILIKE -> `!~~*`.
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without rhs".into()))?;
            let mut args = vec![compile_expr(lhs)?];
            args.extend(compile_pattern_operands(rhs, "like_escape", "ILIKE")?);
            let func = Expr::Func {
                binding: None,
                name: "ilike".into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            return match op_name.as_str() {
                "~~*" => Ok(func),
                "!~~*" => Ok(Expr::Not(Box::new(func))),
                other => Err(SQLError::Internal(format!(
                    "ILIKE has unexpected operator `{other}`"
                ))),
            };
        }
        AExprKind::AexprIn => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IN without rhs".into()))?;
            let items = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) => l.items.clone(),
                _ => return Err(SQLError::Internal("IN expects list".into())),
            };
            let list: Vec<Expr> = items.iter().map(compile_expr).collect::<Result<Vec<_>>>()?;
            let operator = extract_strings(&a.name)?.join("");
            let negated = match operator.as_str() {
                "=" => false,
                "<>" => true,
                other => {
                    return Err(SQLError::Internal(format!(
                        "IN has unexpected operator `{other}`"
                    )));
                }
            };
            Ok(Expr::InList {
                expr: Box::new(compile_expr(expr)?),
                list,
                negated,
            })
        }
        other => Err(SQLError::Unsupported(format!("AExpr kind: {other:?}"))),
    }
}

pub(in crate::compiler) fn compile_bool_expr(b: &pg_query::protobuf::BoolExpr) -> Result<Expr> {
    use pg_query::protobuf::BoolExprType;
    let kind = b.boolop();
    let args: Vec<Expr> = b
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    match kind {
        BoolExprType::AndExpr if args.len() >= 2 => Ok(Expr::And(args)),
        BoolExprType::OrExpr if args.len() >= 2 => Ok(Expr::Or(args)),
        BoolExprType::AndExpr | BoolExprType::OrExpr => Err(SQLError::Internal(format!(
            "{kind:?} requires at least two operands, got {}",
            args.len()
        ))),
        BoolExprType::NotExpr => {
            let [arg] = args.as_slice() else {
                return Err(SQLError::Internal(format!(
                    "NOT requires exactly one operand, got {}",
                    args.len()
                )));
            };
            Ok(Expr::Not(Box::new(arg.clone())))
        }
        _ => Err(SQLError::Unsupported(format!("BoolExpr {kind:?}"))),
    }
}

pub(in crate::compiler) fn compile_null_test(n: &pg_query::protobuf::NullTest) -> Result<Expr> {
    use pg_query::protobuf::NullTestType;
    let arg = n
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("NullTest without arg".into()))?;
    let negated = match n.nulltesttype() {
        NullTestType::IsNull => false,
        NullTestType::IsNotNull => true,
        other => {
            return Err(SQLError::Internal(format!(
                "NullTest has invalid kind {other:?}"
            )));
        }
    };
    Ok(Expr::IsNull {
        expr: Box::new(compile_expr(arg)?),
        negated,
    })
}
