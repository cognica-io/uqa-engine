//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable SQL text rendering for cataloged expressions.

use std::fmt::Write as _;

use uqa_core::Value;
use uqa_sql::ast::Expr;

pub(super) fn default_expr_text(expr: Option<&Expr>) -> Value {
    expr.map_or(Value::Null, |expr| Value::Str(schema_expr_text(expr)))
}

pub(super) fn schema_expr_text(expr: &Expr) -> String {
    match expr {
        Expr::Star => "*".into(),
        Expr::Default => "DEFAULT".into(),
        Expr::Column(name) => name.clone(),
        Expr::QualifiedColumn {
            qualifier, column, ..
        } => format!("{qualifier}.{column}"),
        Expr::Literal(value) => schema_literal_text(value),
        Expr::Param(index) => format!("${index}"),
        Expr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
            ..
        } => {
            let mut rendered_args = args
                .iter()
                .map(schema_expr_text)
                .collect::<Vec<_>>()
                .join(", ");
            if *distinct {
                rendered_args = format!("DISTINCT {rendered_args}");
            }
            if !order_by.is_empty() {
                let order = order_by
                    .iter()
                    .map(|order| {
                        let direction = if order.descending { " DESC" } else { "" };
                        let nulls = match order.nulls {
                            Some(uqa_sql::ast::NullsOrder::First) => " NULLS FIRST",
                            Some(uqa_sql::ast::NullsOrder::Last) => " NULLS LAST",
                            None => "",
                        };
                        format!("{}{direction}{nulls}", schema_expr_text(&order.expr))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if !rendered_args.is_empty() {
                    rendered_args.push(' ');
                }
                rendered_args.push_str("ORDER BY ");
                rendered_args.push_str(&order);
            }
            let mut rendered = format!("{name}({rendered_args})");
            if let Some(filter) = filter {
                write!(
                    &mut rendered,
                    " FILTER (WHERE {})",
                    schema_expr_text(filter)
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
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Binary { op, lhs, rhs } => format!(
            "({} {} {})",
            schema_expr_text(lhs),
            match op {
                uqa_sql::ast::BinaryOp::Equal => "=",
                uqa_sql::ast::BinaryOp::NotEqual => "<>",
                uqa_sql::ast::BinaryOp::Less => "<",
                uqa_sql::ast::BinaryOp::LessEqual => "<=",
                uqa_sql::ast::BinaryOp::Greater => ">",
                uqa_sql::ast::BinaryOp::GreaterEqual => ">=",
                uqa_sql::ast::BinaryOp::Add => "+",
                uqa_sql::ast::BinaryOp::Subtract => "-",
                uqa_sql::ast::BinaryOp::Multiply => "*",
                uqa_sql::ast::BinaryOp::Divide => "/",
            },
            schema_expr_text(rhs)
        ),
        Expr::Not(inner) => format!("(NOT {})", schema_expr_text(inner)),
        Expr::UnaryMinus(inner) => format!("(-{})", schema_expr_text(inner)),
        Expr::And(items) => format!(
            "({})",
            items
                .iter()
                .map(schema_expr_text)
                .collect::<Vec<_>>()
                .join(" AND ")
        ),
        Expr::Or(items) => format!(
            "({})",
            items
                .iter()
                .map(schema_expr_text)
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
        Expr::IsNull { expr, negated } => format!(
            "({} IS {}NULL)",
            schema_expr_text(expr),
            if *negated { "NOT " } else { "" }
        ),
        Expr::Between { expr, low, high } => format!(
            "({} BETWEEN {} AND {})",
            schema_expr_text(expr),
            schema_expr_text(low),
            schema_expr_text(high)
        ),
        Expr::InList {
            expr,
            list,
            negated,
        } => format!(
            "({} {}IN ({}))",
            schema_expr_text(expr),
            if *negated { "NOT " } else { "" },
            list.iter()
                .map(schema_expr_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::WindowCall { name, args, .. } => format!(
            "{}({}) OVER (...)",
            name,
            args.iter()
                .map(schema_expr_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let mut rendered = "CASE".to_string();
            if let Some(base) = base {
                rendered.push(' ');
                rendered.push_str(&schema_expr_text(base));
            }
            for (condition, result) in when {
                write!(
                    &mut rendered,
                    " WHEN {} THEN {}",
                    schema_expr_text(condition),
                    schema_expr_text(result)
                )
                .expect("writing to a String cannot fail");
            }
            if let Some(else_branch) = else_branch {
                write!(&mut rendered, " ELSE {}", schema_expr_text(else_branch))
                    .expect("writing to a String cannot fail");
            }
            rendered.push_str(" END");
            rendered
        }
        Expr::Cast { expr, ty } => format!("({})::{ty}", schema_expr_text(expr)),
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
            schema_expr_text(expr),
            if *negated { "NOT " } else { "" }
        ),
    }
}

fn schema_literal_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
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
        Value::List(values) => format!(
            "ARRAY[{}]",
            values
                .iter()
                .map(schema_literal_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Map(value) => format!(
            "'{}'::jsonb",
            serde_json::to_string(value)
                .expect("serializing an in-memory Value map cannot fail")
                .replace('\'', "''")
        ),
    }
}
