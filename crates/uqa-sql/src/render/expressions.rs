//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar SQL reconstruction with query-local window identities.

use super::window::window_sql;
use super::{expr_list_in, ident, order_by_sql_in, select_sql, value_sql};
use crate::ast::{Expr, OrderBy, WindowDefinition};
use crate::SQLError;
use std::fmt::Write as _;

pub(super) fn render_expr(expression: &Expr) -> Result<String, SQLError> {
    render_expr_in(expression, &[])
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive scalar rendering keeps every durable AST variant explicit"
)]
pub(super) fn render_expr_in(
    expression: &Expr,
    windows: &[WindowDefinition],
) -> Result<String, SQLError> {
    let render_expr = |expression| render_expr_in(expression, windows);
    let expr_list = |expressions: &[Expr]| expr_list_in(expressions, windows);
    let order_by_sql = |orders: &[OrderBy]| order_by_sql_in(orders, windows);
    Ok(match expression {
        Expr::Star => "*".into(),
        Expr::QualifiedStar(qualifier) => format!("{}.*", ident(qualifier)),
        Expr::Default => "DEFAULT".into(),
        Expr::Column(name) => ident(name),
        Expr::QualifiedColumn { qualifier, column } => {
            format!("{}.{}", ident(qualifier), ident(column))
        }
        Expr::InternalColumn(column) => {
            return Err(SQLError::Internal(format!(
                "executor-only column {column:?} reached durable SQL rendering"
            )))
        }
        Expr::Literal(value) => value_sql(value)?,
        Expr::TypedLiteral { value, ty } => format!("({})::{ty}", value_sql(value)?),
        Expr::Param(index) => format!("${index}"),
        Expr::Func {
            binding:
                Some(crate::ast::FunctionBinding {
                    dispatch: Some(crate::ast::FunctionDispatch::JsonExtract { as_text, path }),
                    ..
                }),
            args,
            ..
        } => {
            let [lhs, rhs] = args.as_slice() else {
                return Err(SQLError::Internal(
                    "JSON extraction requires two operands".into(),
                ));
            };
            let operator = match (*path, *as_text) {
                (false, false) => "->",
                (false, true) => "->>",
                (true, false) => "#>",
                (true, true) => "#>>",
            };
            format!("({} {operator} {})", render_expr(lhs)?, render_expr(rhs)?)
        }
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            order_syntax,
            filter,
            ..
        } => {
            if *order_syntax == crate::ast::FunctionCallSyntax::Extract {
                let (field, source) = super::function_syntax::extract_fields(args)?;
                return Ok(format!(
                    "EXTRACT({} FROM {})",
                    super::string_literal(field),
                    render_expr(source)?
                ));
            }
            if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
                binding.as_ref().and_then(|binding| binding.dispatch)
            {
                return match args.as_slice() {
                    [argument] if operator.arity() == 1 => Ok(format!(
                        "({} {})",
                        operator.symbol(),
                        render_expr(argument)?
                    )),
                    [left, right] if operator.arity() == 2 => Ok(format!(
                        "({} {} {})",
                        render_expr(left)?,
                        operator.symbol(),
                        render_expr(right)?
                    )),
                    _ => Err(SQLError::Internal(
                        "invalid numeric operator operands".into(),
                    )),
                };
            }
            let mut arguments = expr_list(args)?;
            if *distinct {
                arguments = format!("DISTINCT {arguments}");
            }
            if !order_by.is_empty() && *order_syntax != crate::ast::FunctionOrderSyntax::WithinGroup
            {
                if !arguments.is_empty() {
                    arguments.push(' ');
                }
                arguments.push_str("ORDER BY ");
                arguments.push_str(&order_by_sql(order_by)?);
            }
            let name = super::function_syntax::ordinary_function_name(name);
            let mut rendered = format!("{name}({arguments})");
            if *order_syntax == crate::ast::FunctionOrderSyntax::WithinGroup {
                write!(
                    &mut rendered,
                    " WITHIN GROUP (ORDER BY {})",
                    order_by_sql(order_by)?
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(filter) = filter {
                write!(&mut rendered, " FILTER (WHERE {})", render_expr(filter)?)
                    .expect("writing to a String cannot fail");
            }
            rendered
        }
        Expr::Array(items) => format!("ARRAY[{}]", expr_list(items)?),
        Expr::Row(items) => format!("ROW({})", expr_list(items)?),
        Expr::Binary { op, lhs, rhs } => format!(
            "({} {} {})",
            render_expr(lhs)?,
            binary_operator_sql(*op),
            render_expr(rhs)?
        ),
        Expr::UnaryMinus(inner) => format!("(-{})", render_expr(inner)?),
        Expr::Not(inner) => format!("(NOT {})", render_expr(inner)?),
        Expr::And(items) => format!(
            "({})",
            items
                .iter()
                .map(render_expr)
                .collect::<Result<Vec<_>, _>>()?
                .join(" AND ")
        ),
        Expr::Or(items) => format!(
            "({})",
            items
                .iter()
                .map(render_expr)
                .collect::<Result<Vec<_>, _>>()?
                .join(" OR ")
        ),
        Expr::IsNull { expr, negated } => format!(
            "({} IS {}NULL)",
            render_expr(expr)?,
            if *negated { "NOT " } else { "" }
        ),
        Expr::Between { expr, low, high } => format!(
            "({} BETWEEN {} AND {})",
            render_expr(expr)?,
            render_expr(low)?,
            render_expr(high)?
        ),
        Expr::InList {
            expr,
            list,
            negated,
        } => format!(
            "({} {}IN ({}))",
            render_expr(expr)?,
            if *negated { "NOT " } else { "" },
            expr_list(list)?
        ),
        Expr::WindowCall {
            name,
            args,
            spec,
            filter,
            ..
        } => format!(
            "{name}({}){} OVER {}",
            expr_list(args)?,
            filter
                .as_deref()
                .map(render_expr)
                .transpose()?
                .map_or_else(String::new, |filter| format!(" FILTER (WHERE {filter})")),
            window_sql(spec, windows)?
        ),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let mut rendered = String::from("CASE");
            if let Some(base) = base {
                rendered.push(' ');
                rendered.push_str(&render_expr(base)?);
            }
            for (condition, result) in when {
                write!(
                    &mut rendered,
                    " WHEN {} THEN {}",
                    render_expr(condition)?,
                    render_expr(result)?
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(branch) = else_branch {
                rendered.push_str(" ELSE ");
                rendered.push_str(&render_expr(branch)?);
            }
            rendered.push_str(" END");
            rendered
        }
        Expr::Cast { expr, ty } => format!("CAST({} AS {ty})", render_expr(expr)?),
        Expr::ScalarSubquery(body) => format!("({})", select_sql(body)?),
        Expr::Exists { body, negated } => format!(
            "{}EXISTS ({})",
            if *negated { "NOT " } else { "" },
            select_sql(body)?
        ),
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => format!(
            "({} {}IN ({}))",
            render_expr(expr)?,
            if *negated { "NOT " } else { "" },
            select_sql(body)?
        ),
    })
}

const fn binary_operator_sql(operator: crate::ast::BinaryOp) -> &'static str {
    match operator {
        crate::ast::BinaryOp::Equal => "=",
        crate::ast::BinaryOp::NotEqual => "<>",
        crate::ast::BinaryOp::Less => "<",
        crate::ast::BinaryOp::LessEqual => "<=",
        crate::ast::BinaryOp::Greater => ">",
        crate::ast::BinaryOp::GreaterEqual => ">=",
        crate::ast::BinaryOp::Add => "+",
        crate::ast::BinaryOp::Subtract => "-",
        crate::ast::BinaryOp::Multiply => "*",
        crate::ast::BinaryOp::Divide => "/",
    }
}
