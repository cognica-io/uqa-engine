//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable SQL text rendering for cataloged expressions.

use std::fmt::Write as _;

use crate::ast::Expr;
use crate::SQLError;
use uqa_core::Value;

pub fn default_expr_text(expr: Option<&Expr>) -> Result<Value, SQLError> {
    expr.map_or(Ok(Value::Null), |expr| {
        schema_expr_text(expr).map(Value::Str)
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub fn schema_expr_text(expr: &Expr) -> Result<String, SQLError> {
    Ok(match expr {
        Expr::Star => "*".into(),
        Expr::QualifiedStar(qualifier) => format!("{qualifier}.*"),
        Expr::Default => "DEFAULT".into(),
        Expr::Column(name) => name.clone(),
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => format!("{qualifier}.{column}"),
        Expr::InternalColumn(column) => {
            unreachable!("executor-only column {column:?} reached catalog SQL rendering")
        }
        Expr::Literal(value) => schema_literal_text(value)?,
        Expr::TypedLiteral { value, ty } => format!("({})::{ty}", schema_literal_text(value)?),
        Expr::Param(index) => format!("${index}"),
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
            if let Some(crate::ast::FunctionDispatch::NumericOperator(operator)) =
                binding.as_ref().and_then(|binding| binding.dispatch)
            {
                match args.as_slice() {
                    [argument] if operator.arity() == 1 => {
                        return Ok(format!(
                            "({} {})",
                            operator.symbol(),
                            schema_expr_text(argument)?
                        ))
                    }
                    [left, right] if operator.arity() == 2 => {
                        return Ok(format!(
                            "({} {} {})",
                            schema_expr_text(left)?,
                            operator.symbol(),
                            schema_expr_text(right)?
                        ))
                    }
                    _ => {}
                }
            }
            let mut rendered_args = args
                .iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ");
            if *distinct {
                rendered_args = format!("DISTINCT {rendered_args}");
            }
            let mut rendered = format!("{name}({rendered_args})");
            if !order_by.is_empty() {
                let order = order_by
                    .iter()
                    .map(|order| {
                        let direction = if order.descending { " DESC" } else { "" };
                        let nulls = match order.nulls {
                            Some(crate::ast::NullsOrder::First) => " NULLS FIRST",
                            Some(crate::ast::NullsOrder::Last) => " NULLS LAST",
                            None => "",
                        };
                        Ok(format!(
                            "{}{direction}{nulls}",
                            schema_expr_text(&order.expr)?
                        ))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?
                    .join(", ");
                if *order_syntax == crate::ast::FunctionOrderSyntax::WithinGroup {
                    write!(&mut rendered, " WITHIN GROUP (ORDER BY {order})")
                        .expect("writing to a String cannot fail");
                } else {
                    rendered.pop();
                    if !rendered_args.is_empty() {
                        rendered.push(' ');
                    }
                    write!(&mut rendered, "ORDER BY {order})")
                        .expect("writing to a String cannot fail");
                }
            }
            if let Some(filter) = filter {
                write!(
                    &mut rendered,
                    " FILTER (WHERE {})",
                    schema_expr_text(filter)?
                )
                .expect("writing to a String cannot fail");
            }
            rendered
        }
        Expr::Array(items) => format!(
            "ARRAY[{}]",
            items
                .iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Expr::Row(items) => format!(
            "ROW({})",
            items
                .iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Expr::Binary { op, lhs, rhs } => format!(
            "({} {} {})",
            schema_expr_text(lhs)?,
            match op {
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
            },
            schema_expr_text(rhs)?
        ),
        Expr::Not(inner) => format!("(NOT {})", schema_expr_text(inner)?),
        Expr::UnaryMinus(inner) => format!("(-{})", schema_expr_text(inner)?),
        Expr::And(items) => format!(
            "({})",
            items
                .iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(" AND ")
        ),
        Expr::Or(items) => format!(
            "({})",
            items
                .iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(" OR ")
        ),
        Expr::IsNull { expr, negated } => format!(
            "({} IS {}NULL)",
            schema_expr_text(expr)?,
            if *negated { "NOT " } else { "" }
        ),
        Expr::Between { expr, low, high } => format!(
            "({} BETWEEN {} AND {})",
            schema_expr_text(expr)?,
            schema_expr_text(low)?,
            schema_expr_text(high)?
        ),
        Expr::InList {
            expr,
            list,
            negated,
        } => format!(
            "({} {}IN ({}))",
            schema_expr_text(expr)?,
            if *negated { "NOT " } else { "" },
            list.iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Expr::WindowCall {
            name, args, filter, ..
        } => format!(
            "{}({}){} OVER (...)",
            name,
            args.iter()
                .map(schema_expr_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", "),
            filter
                .as_deref()
                .map(schema_expr_text)
                .transpose()?
                .map(|filter| format!(" FILTER (WHERE {filter})"))
                .unwrap_or_default()
        ),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let mut rendered = "CASE".to_string();
            if let Some(base) = base {
                rendered.push(' ');
                rendered.push_str(&schema_expr_text(base)?);
            }
            for (condition, result) in when {
                write!(
                    &mut rendered,
                    " WHEN {} THEN {}",
                    schema_expr_text(condition)?,
                    schema_expr_text(result)?
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(else_branch) = else_branch {
                write!(&mut rendered, " ELSE {}", schema_expr_text(else_branch)?)
                    .expect("writing to a String cannot fail");
            }
            rendered.push_str(" END");
            rendered
        }
        Expr::Cast { expr, ty } => format!("({})::{ty}", schema_expr_text(expr)?),
        Expr::ScalarSubquery(body) => format!("({body:?})"),
        Expr::Exists { body, negated } => {
            format!("{}EXISTS ({body:?})", if *negated { "NOT " } else { "" })
        }
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => format!(
            "({} {}IN ({body:?}))",
            schema_expr_text(expr)?,
            if *negated { "NOT " } else { "" }
        ),
    })
}

fn schema_literal_text(value: &Value) -> Result<String, SQLError> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Void => "''::void".into(),
        Value::Bool(value) => if *value { "true" } else { "false" }.into(),
        Value::Int(value) => value.to_string(),
        Value::Float(value) if value.is_finite() => value.to_string(),
        Value::Float(value) => format!("'{value}'::double precision"),
        Value::Str(value) | Value::FixedChar(value) => {
            format!("'{}'", value.replace('\'', "''"))
        }
        Value::Bytes(value) => {
            let mut hex = String::new();
            for byte in value {
                write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
            }
            format!("'\\x{hex}'::bytea")
        }
        Value::Temporal(value) => format!("'{value:?}'"),
        Value::Decimal(value) => format!("{value:?}"),
        Value::Json(value) => format!("'{}'::json", value.replace('\'', "''")),
        Value::JsonB(value) => format!("'{}'::jsonb", value.replace('\'', "''")),
        Value::Enum(value) => return Err(crate::expr::catalog_output_required(value)),
        Value::LegacyVector(vector) => crate::render::legacy_vector_expression(vector)
            .expect("stored SQL vector has SQL-produced bounds"),
        Value::Array(array)
            if array
                .lower_bounds()
                .iter()
                .any(|lower_bound| *lower_bound != 1) =>
        {
            format!(
                "'{}'",
                crate::expr::array_value_to_string(array)?.replace('\'', "''")
            )
        }
        Value::Array(array) => format!(
            "ARRAY[{}]",
            array
                .elements()
                .iter()
                .map(schema_literal_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Value::List(values) => format!(
            "ARRAY[{}]",
            values
                .iter()
                .map(schema_literal_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Value::Row(values) => format!(
            "ROW({})",
            values
                .iter()
                .map(schema_literal_text)
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Value::Record(fields) => format!(
            "ROW({})",
            fields
                .iter()
                .map(|(_, value)| schema_literal_text(value))
                .collect::<Result<Vec<_>, SQLError>>()?
                .join(", ")
        ),
        Value::Map(value) => format!(
            "'{}'::jsonb",
            serde_json::to_string(value)
                .expect("serializing an in-memory Value map cannot fail")
                .replace('\'', "''")
        ),
    })
}
