//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constants, columns, function calls, and window specification lowering.

use super::{
    compile_expr, compile_qualified_name, compile_sort_options, compile_window_frame, DecimalValue,
    Expr, NodeEnum, OrderBy, Result, SQLError, Value, WindowReference, WindowReferenceKind,
    WindowSpec,
};

pub(in crate::compiler) fn compile_const(c: &pg_query::protobuf::AConst) -> Result<Expr> {
    if c.isnull {
        if c.val.is_some() {
            return Err(SQLError::Internal(
                "NULL constant unexpectedly has a value payload".into(),
            ));
        }
        return Ok(Expr::Literal(Value::Null));
    }
    use pg_query::protobuf::a_const::Val;
    let Some(val) = c.val.as_ref() else {
        return Err(SQLError::Internal(
            "non-NULL constant has no value payload".into(),
        ));
    };
    let value = match val {
        Val::Ival(i) => Value::Int(i64::from(i.ival)),
        // Integer literals wider than int4 arrive as Fval strings (the
        // parser also folds a unary minus into the literal); ones that
        // fit i64 stay integers (PostgreSQL types them int8), the rest
        // become numeric.
        Val::Fval(f)
            if f.fval
                .strip_prefix('-')
                .unwrap_or(&f.fval)
                .bytes()
                .all(|b| b.is_ascii_digit()) =>
        {
            f.fval.parse::<i64>().map(Value::Int).or_else(|_| {
                DecimalValue::parse(&f.fval)
                    .map(Value::Decimal)
                    .ok_or_else(|| SQLError::Internal(format!("bad numeric literal {}", f.fval)))
            })?
        }
        Val::Fval(f) => DecimalValue::parse(&f.fval).map_or_else(
            || {
                f.fval
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|e| SQLError::Internal(e.to_string()))
            },
            |d| Ok(Value::Decimal(d)),
        )?,
        Val::Sval(s) => Value::Str(s.sval.clone()),
        Val::Boolval(b) => Value::Bool(b.boolval),
        other => {
            return Err(SQLError::Unsupported(format!("constant: {other:?}")));
        }
    };
    Ok(Expr::Literal(value))
}

pub(in crate::compiler) fn compile_column_ref(c: &pg_query::protobuf::ColumnRef) -> Result<Expr> {
    let mut parts: Vec<String> = Vec::new();
    for (index, f) in c.fields.iter().enumerate() {
        let inner = f
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("ColumnRef contains an empty field".into()))?;
        match inner {
            NodeEnum::String(s) if !s.sval.is_empty() => parts.push(s.sval.clone()),
            NodeEnum::String(_) => {
                return Err(SQLError::Internal(
                    "ColumnRef contains an empty name component".into(),
                ));
            }
            NodeEnum::AStar(_) if c.fields.len() == 1 && index == 0 => return Ok(Expr::Star),
            NodeEnum::AStar(_) => {
                let qualifier = parts.pop().ok_or_else(|| {
                    SQLError::Internal("qualified wildcard has no relation name".into())
                })?;
                return Ok(Expr::QualifiedStar(qualifier));
            }
            other => {
                return Err(SQLError::Internal(format!(
                    "ColumnRef contains unexpected field {other:?}"
                )));
            }
        }
    }
    match parts.len() {
        0 => Err(SQLError::Internal("empty ColumnRef".into())),
        1 => Ok(Expr::Column(parts.pop().ok_or_else(|| {
            SQLError::Internal("ColumnRef component disappeared during lowering".into())
        })?)),
        _ => {
            // `schema.table.col` collapses to `table.col`; `t.col`
            // round-trips as a qualified ref.
            let column = parts.pop().ok_or_else(|| {
                SQLError::Internal("ColumnRef column disappeared during lowering".into())
            })?;
            let qualifier = parts.pop().ok_or_else(|| {
                SQLError::Internal("ColumnRef qualifier disappeared during lowering".into())
            })?;
            Ok(Expr::qualified_column(qualifier, column))
        }
    }
}

pub(in crate::compiler) fn compile_func_call(f: &pg_query::protobuf::FuncCall) -> Result<Expr> {
    let raw_name = compile_qualified_name(&f.funcname, "function call")?;
    if raw_name.is_empty() {
        return Err(SQLError::Internal("function call has an empty name".into()));
    }
    if f.func_variadic {
        return Err(SQLError::Unsupported(format!(
            "VARIADIC invocation of `{raw_name}` is not represented by Expr::Func"
        )));
    }
    let mut args = f
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    if f.agg_star {
        if !args.is_empty() {
            return Err(SQLError::Internal(format!(
                "function `{raw_name}` has both `*` and explicit arguments"
            )));
        }
        if f.agg_distinct || !f.agg_order.is_empty() || f.agg_within_group {
            return Err(SQLError::Internal(format!(
                "function `{raw_name}(*)` has incompatible aggregate modifiers"
            )));
        }
        args.push(Expr::Star);
    }
    if f.agg_within_group && f.agg_order.is_empty() {
        return Err(SQLError::Internal(format!(
            "ordered-set aggregate `{raw_name}` has no WITHIN GROUP ordering"
        )));
    }
    if let Some(over) = f.over.as_ref() {
        if f.agg_filter.is_some() || !f.agg_order.is_empty() || f.agg_distinct || f.agg_within_group
        {
            return Err(SQLError::Unsupported(format!(
                "window call `{raw_name}` uses aggregate modifiers not represented by WindowCall"
            )));
        }
        let spec = compile_window_spec(over)?;
        return Ok(Expr::WindowCall {
            name: raw_name,
            args,
            spec,
        });
    }
    // Translate the aggregate's ORDER BY clauses (e.g.
    // `string_agg(name, ',' ORDER BY name)`) into typed `OrderBy`
    // entries on `Expr::Func.order_by`.
    let mut agg_order: Vec<OrderBy> = Vec::new();
    for sort_node in &f.agg_order {
        let inner = sort_node.node.as_ref().ok_or_else(|| {
            SQLError::Internal("aggregate ORDER BY contains an empty item".into())
        })?;
        let NodeEnum::SortBy(sb) = inner else {
            return Err(SQLError::Internal(format!(
                "aggregate ORDER BY expected SortBy, got {inner:?}"
            )));
        };
        let expr_node = sb
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("agg_order SortBy without expr".into()))?;
        let key_expr = compile_expr(expr_node)?;
        let (descending, nulls) = compile_sort_options(sb, "aggregate ORDER BY")?;
        agg_order.push(OrderBy {
            expr: key_expr,
            descending,
            nulls,
        });
    }
    let agg_filter = match f.agg_filter.as_ref() {
        Some(inner) => Some(Box::new(compile_expr(inner)?)),
        None => None,
    };
    Ok(Expr::Func {
        binding: None,
        name: raw_name,
        args,
        distinct: f.agg_distinct,
        order_by: agg_order,
        filter: agg_filter,
    })
}

pub(in crate::compiler) fn compile_window_spec(
    w: &pg_query::protobuf::WindowDef,
) -> Result<WindowSpec> {
    let reference = match (w.name.is_empty(), w.refname.is_empty()) {
        (true, true) => None,
        (false, true) => Some(WindowReference {
            name: w.name.clone(),
            kind: WindowReferenceKind::Direct,
        }),
        (true, false) => Some(WindowReference {
            name: w.refname.clone(),
            kind: WindowReferenceKind::Copy,
        }),
        (false, false) => {
            return Err(SQLError::Internal(
                "window call carries both direct and copied references".into(),
            ));
        }
    };
    compile_window_spec_parts(w, reference)
}

pub(in crate::compiler) fn compile_named_window_spec(
    w: &pg_query::protobuf::WindowDef,
) -> Result<WindowSpec> {
    let reference = (!w.refname.is_empty()).then(|| WindowReference {
        name: w.refname.clone(),
        kind: WindowReferenceKind::Copy,
    });
    compile_window_spec_parts(w, reference)
}

fn compile_window_spec_parts(
    w: &pg_query::protobuf::WindowDef,
    reference: Option<WindowReference>,
) -> Result<WindowSpec> {
    let partition_by: Vec<Expr> = w
        .partition_clause
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    let mut order_by = Vec::new();
    for sort_node in &w.order_clause {
        let inner = sort_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("window ORDER BY contains an empty item".into()))?;
        let NodeEnum::SortBy(sb) = inner else {
            return Err(SQLError::Internal(format!(
                "window ORDER BY expected SortBy, got {inner:?}"
            )));
        };
        let expr_node = sb
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
        let expr = compile_expr(expr_node)?;
        let (descending, nulls) = compile_sort_options(sb, "window ORDER BY")?;
        order_by.push(OrderBy {
            expr,
            descending,
            nulls,
        });
    }
    let frame = compile_window_frame(w)?;
    Ok(WindowSpec {
        reference,
        partition_by,
        order_by,
        frame,
    })
}
