//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar SQL reconstruction with `PostgreSQL` operator precedence.

use std::fmt::Write as _;

use uqa_core::Value;
use uqa_execution::{ScalarFrameBound, ScalarWindowSpec};
use uqa_planner::{QueryPlan, ScalarExpr};
use uqa_sql::ast::{BinaryOp, Expr, FrameMode, FunctionBinding};

use super::{quote_ident, render_column, Deparser, SQLError, Scope};

impl Deparser<'_> {
    pub(super) fn expression(
        &self,
        expression: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        match expression {
            ScalarExpr::Column(name) => Ok(scope.column(None, name)),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                Ok(scope.column(Some(qualifier), column))
            }
            ScalarExpr::Position(index) => scope
                .columns
                .get(*index)
                .map(|column| render_column(column, scope.qualify))
                .ok_or_else(|| {
                    SQLError::Internal("view column position outside source schema".into())
                }),
            ScalarExpr::Star => Ok("*".into()),
            ScalarExpr::QualifiedStar(qualifier) => Ok(format!("{}.*", quote_ident(qualifier))),
            ScalarExpr::Default => Ok("DEFAULT".into()),
            ScalarExpr::InternalColumn(_) => Err(SQLError::Internal(
                "executor-only column reached view SQL reconstruction".into(),
            )),
            ScalarExpr::Literal(value) => literal(value),
            ScalarExpr::Param(index) => Ok(format!("${index}")),
            ScalarExpr::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, scope, subqueries),
            ScalarExpr::And(items) | ScalarExpr::Or(items) => {
                let operator = if matches!(expression, ScalarExpr::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                let expressions = items
                    .iter()
                    .map(|item| {
                        self.operand(item, precedence(expression), false, scope, subqueries)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.parenthesize(expressions.join(operator)))
            }
            ScalarExpr::Not(inner) => self.negation(inner, scope, subqueries),
            ScalarExpr::UnaryMinus(inner) => self.unary_minus(inner, scope, subqueries),
            ScalarExpr::IsNull { expr, negated } => Ok(self.parenthesize(format!(
                "{} IS {}NULL",
                self.operand(expr, 40, false, scope, subqueries)?,
                if *negated { "NOT " } else { "" }
            ))),
            ScalarExpr::Between { expr, low, high } => {
                let lower = self.binary(BinaryOp::GreaterEqual, expr, low, scope, subqueries)?;
                let upper = self.binary(BinaryOp::LessEqual, expr, high, scope, subqueries)?;
                Ok(self.parenthesize(format!("{lower} AND {upper}")))
            }
            ScalarExpr::InList {
                expr,
                list,
                negated,
            } => self.in_list(expr, list, *negated, scope, subqueries),
            ScalarExpr::Array(items) => Ok(format!(
                "ARRAY[{}]",
                self.expressions(items, scope, subqueries)?
            )),
            ScalarExpr::Row(items) => Ok(format!(
                "ROW({})",
                self.expressions(items, scope, subqueries)?
            )),
            ScalarExpr::Cast { expr, ty } => self.cast(expr, ty, scope, subqueries),
            ScalarExpr::Func { .. } => self.aggregate(expression, scope, subqueries),
            ScalarExpr::WindowCall { name, args, spec } => Ok(format!(
                "{} OVER ({})",
                self.function(name, None, args, scope, subqueries)?,
                self.window(spec, scope, subqueries)?
            )),
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => self.case(
                base.as_deref(),
                when,
                else_branch.as_deref(),
                scope,
                subqueries,
            ),
            ScalarExpr::ScalarSubquery(id) => {
                Ok(format!("({})", self.subquery(*id, scope, subqueries)?))
            }
            ScalarExpr::Exists { subquery, negated } => Ok(format!(
                "({}EXISTS ({}))",
                if *negated { "NOT " } else { "" },
                self.subquery(*subquery, scope, subqueries)?
            )),
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            } => Ok(format!(
                "({} {}IN ({}))",
                self.expression(expr, scope, subqueries)?,
                if *negated { "NOT " } else { "" },
                self.subquery(*subquery, scope, subqueries)?
            )),
        }
    }

    fn unary_minus(
        &self,
        inner: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let ScalarExpr::Literal(Value::Int(value)) = inner {
            return Ok(format!("'-{value}'::integer"));
        }
        Ok(self.parenthesize(format!(
            "- {}",
            self.operand(inner, 70, false, scope, subqueries)?
        )))
    }

    pub(super) fn expressions(
        &self,
        expressions: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        expressions
            .iter()
            .map(|expression| self.expression(expression, scope, subqueries))
            .collect::<Result<Vec<_>, _>>()
            .map(|items| items.join(", "))
    }

    fn parenthesize(&self, rendered: String) -> String {
        if self.pretty {
            rendered
        } else {
            format!("({rendered})")
        }
    }

    fn operand(
        &self,
        expression: &ScalarExpr,
        parent: u8,
        right: bool,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let rendered = self.expression(expression, scope, subqueries)?;
        let precedence = precedence(expression);
        Ok(
            if self.pretty
                && (precedence < parent || (right && precedence == parent && parent >= 40))
            {
                format!("({rendered})")
            } else {
                rendered
            },
        )
    }

    fn binary(
        &self,
        op: BinaryOp,
        lhs: &ScalarExpr,
        rhs: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let precedence = operator_precedence(op);
        let left = self.operand(lhs, precedence, false, scope, subqueries)?;
        let right = self.operand(rhs, precedence, true, scope, subqueries)?;
        Ok(self.parenthesize(format!("{left} {} {right}", operator(op))))
    }

    fn in_list(
        &self,
        expr: &ScalarExpr,
        list: &[ScalarExpr],
        negated: bool,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let [item] = list {
            return self.binary(
                if negated {
                    BinaryOp::NotEqual
                } else {
                    BinaryOp::Equal
                },
                expr,
                item,
                scope,
                subqueries,
            );
        }
        let left = self.operand(expr, 40, false, scope, subqueries)?;
        Ok(self.parenthesize(format!(
            "{left} {} (ARRAY[{}])",
            if negated { "<> ALL" } else { "= ANY" },
            self.expressions(list, scope, subqueries)?
        )))
    }

    fn subquery(
        &self,
        index: usize,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let query = subqueries.get(index).ok_or_else(|| {
            SQLError::Internal("view subquery index outside query children".into())
        })?;
        self.query(query, &scope.child(), None)
    }

    fn cast(
        &self,
        expr: &ScalarExpr,
        ty: &str,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let ty = type_name(ty);
        if let ScalarExpr::Literal(value) = expr {
            if matches!(value, Value::Str(_))
                && matches!(
                    ty.as_str(),
                    "integer" | "bigint" | "smallint" | "numeric" | "boolean"
                )
            {
                let converted = uqa_sql::expr::cast_value(value, &ty)?;
                if literal_has_type(&converted, &ty) {
                    return literal(&converted);
                }
                return Ok(format!(
                    "{}::{ty}",
                    uqa_sql::render::expression_sql(&Expr::Literal(value.clone()))?
                ));
            }
            if literal_has_type(value, &ty) {
                return literal(value);
            }
            if matches!(value, Value::Str(_) | Value::Null) {
                let value = uqa_sql::render::expression_sql(&Expr::Literal(value.clone()))?;
                return Ok(format!("{value}::{ty}"));
            }
        }
        let value = self.expression(expr, scope, subqueries)?;
        if self.pretty {
            Ok(format!(
                "{}::{ty}",
                self.operand(expr, 80, false, scope, subqueries)?
            ))
        } else {
            Ok(format!("({value})::{ty}"))
        }
    }

    pub(super) fn function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let [left, right] = args {
            let operator = match name {
                "like" => Some(("~~", 40)),
                "ilike" => Some(("~~*", 40)),
                "concat_op" => Some(("||", 45)),
                _ => None,
            };
            if let Some((operator, precedence)) = operator {
                return Ok(self.parenthesize(format!(
                    "{} {operator} {}",
                    self.operand(left, precedence, false, scope, subqueries)?,
                    self.operand(right, precedence, true, scope, subqueries)?
                )));
            }
        }
        let name = self.function_name(name, binding)?;
        let arguments = args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                    if let Some(ty) = binding.and_then(|binding| binding.argument_types.get(index))
                    {
                        return self.cast(argument, &ty.to_string(), scope, subqueries);
                    }
                }
                self.expression(argument, scope, subqueries)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!("{name}({})", arguments.join(", ")))
    }

    fn function_name(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Result<String, SQLError> {
        let (schema, local) =
            super::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        if matches!(local.as_str(), "coalesce" | "nullif" | "greatest" | "least")
            && schema
                .as_deref()
                .is_none_or(|schema| schema == "pg_catalog")
        {
            return Ok(local.to_ascii_uppercase());
        }
        let Some(schema) = schema else {
            return Ok(quote_ident(&local));
        };
        if schema == "pg_catalog" && binding.is_none_or(|binding| binding.builtin) {
            return Ok(quote_ident(&local));
        }
        if let Some(binding) = binding {
            let visible = self
                .catalog
                .sql_functions(&self.dynamic, &quote_ident(&local))?
                .unwrap_or_default();
            if visible
                .iter()
                .any(|function| function.def.object_id == binding.object_id)
            {
                return Ok(quote_ident(&local));
            }
        }
        Ok(format!("{}.{}", quote_ident(&schema), quote_ident(&local)))
    }

    fn negation(
        &self,
        inner: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let ScalarExpr::Func { name, args, .. } = inner {
            if let [left, right] = args.as_slice() {
                if matches!(name.as_str(), "like" | "ilike") {
                    return Ok(self.parenthesize(format!(
                        "{} {} {}",
                        self.operand(left, 40, false, scope, subqueries)?,
                        if name == "like" { "!~~" } else { "!~~*" },
                        self.operand(right, 40, true, scope, subqueries)?
                    )));
                }
            }
        }
        Ok(self.parenthesize(format!(
            "NOT {}",
            self.operand(inner, 30, false, scope, subqueries)?
        )))
    }

    fn aggregate(
        &self,
        expression: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } = expression
        else {
            unreachable!()
        };
        let mut rendered = self.function(name, binding.as_ref(), args, scope, subqueries)?;
        if *distinct {
            let start = rendered
                .find('(')
                .expect("function has opening parenthesis")
                + 1;
            rendered.insert_str(start, "DISTINCT ");
        }
        if !order_by.is_empty() {
            let order = order_by
                .iter()
                .map(|order| {
                    self.order_expression(
                        &order.expr,
                        order.descending,
                        order.nulls,
                        scope,
                        subqueries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            rendered.pop();
            write!(rendered, " ORDER BY {})", order.join(", "))
                .expect("writing to a String cannot fail");
        }
        if let Some(filter) = filter {
            write!(
                rendered,
                " FILTER (WHERE {})",
                self.expression(filter, scope, subqueries)?
            )
            .expect("writing to a String cannot fail");
        }
        Ok(rendered)
    }

    fn case(
        &self,
        base: Option<&ScalarExpr>,
        when: &[(ScalarExpr, ScalarExpr)],
        otherwise: Option<&ScalarExpr>,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let indent = " ".repeat(scope.indent + 8);
        let mut rendered = format!("\n{indent}CASE");
        if let Some(base) = base {
            rendered.push(' ');
            rendered.push_str(&self.expression(base, scope, subqueries)?);
        }
        for (condition, value) in when {
            write!(
                rendered,
                "\n{indent}    WHEN {} THEN {}",
                self.expression(condition, scope, subqueries)?,
                self.expression(value, scope, subqueries)?
            )
            .expect("writing to a String cannot fail");
        }
        if let Some(otherwise) = otherwise {
            write!(
                rendered,
                "\n{indent}    ELSE {}",
                self.expression(otherwise, scope, subqueries)?
            )
            .expect("writing to a String cannot fail");
        }
        write!(rendered, "\n{indent}END").expect("writing to a String cannot fail");
        Ok(rendered)
    }

    fn window(
        &self,
        spec: &ScalarWindowSpec,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut parts = Vec::new();
        if !spec.partition_by.is_empty() {
            parts.push(format!(
                "PARTITION BY {}",
                self.expressions(&spec.partition_by, scope, subqueries)?
            ));
        }
        if !spec.order_by.is_empty() {
            let order = spec
                .order_by
                .iter()
                .map(|order| {
                    self.order_expression(
                        &order.expr,
                        order.descending,
                        order.nulls,
                        scope,
                        subqueries,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            parts.push(format!("ORDER BY {}", order.join(", ")));
        }
        if let Some(frame) = &spec.frame {
            let mode = match frame.mode {
                FrameMode::Rows => "ROWS",
                FrameMode::Range => "RANGE",
                FrameMode::Groups => "GROUPS",
            };
            parts.push(format!(
                "{mode} BETWEEN {} AND {}",
                self.frame_bound(&frame.start, scope, subqueries)?,
                self.frame_bound(&frame.end, scope, subqueries)?
            ));
        }
        Ok(parts.join(" "))
    }

    fn frame_bound(
        &self,
        bound: &ScalarFrameBound,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        Ok(match bound {
            ScalarFrameBound::UnboundedPreceding => "UNBOUNDED PRECEDING".into(),
            ScalarFrameBound::UnboundedFollowing => "UNBOUNDED FOLLOWING".into(),
            ScalarFrameBound::CurrentRow => "CURRENT ROW".into(),
            ScalarFrameBound::Preceding(value) => {
                format!("{} PRECEDING", self.expression(value, scope, subqueries)?)
            }
            ScalarFrameBound::Following(value) => {
                format!("{} FOLLOWING", self.expression(value, scope, subqueries)?)
            }
        })
    }
}

fn literal(value: &Value) -> Result<String, SQLError> {
    match value {
        Value::Str(value) => Ok(format!("'{}'::text", value.replace('\'', "''"))),
        Value::Int(value) if i32::try_from(*value).is_err() => Ok(format!("'{value}'::bigint")),
        Value::Int(value) if *value < 0 => Ok(format!("'{value}'::integer")),
        Value::Decimal(value) if !value.is_nan() && !value.is_infinite() => {
            let value = value.to_sql_string();
            if value.starts_with('-') || !value.contains('.') {
                Ok(format!("'{value}'::numeric"))
            } else {
                Ok(value)
            }
        }
        _ => uqa_sql::render::expression_sql(&Expr::Literal(value.clone())),
    }
}

fn literal_has_type(value: &Value, ty: &str) -> bool {
    match value {
        Value::Int(value) => {
            if i32::try_from(*value).is_ok() {
                ty == "integer"
            } else {
                ty == "bigint"
            }
        }
        Value::Decimal(_) => ty == "numeric",
        Value::Bool(_) => ty == "boolean",
        Value::Temporal(value) => match value {
            uqa_core::TemporalValue::Date { .. } => ty == "date",
            uqa_core::TemporalValue::Time { .. } => ty == "time" || ty == "time without time zone",
            uqa_core::TemporalValue::TimeTz { .. } => ty == "time with time zone",
            uqa_core::TemporalValue::Timestamp { .. } => {
                ty == "timestamp" || ty == "timestamp without time zone"
            }
            uqa_core::TemporalValue::TimestampTz { .. } => ty == "timestamp with time zone",
            uqa_core::TemporalValue::Interval { .. } => ty == "interval",
        },
        Value::Json(_) => ty == "json",
        Value::JsonB(_) => ty == "jsonb",
        _ => false,
    }
}

fn type_name(ty: &str) -> String {
    match ty.to_ascii_lowercase().as_str() {
        "int" | "int4" => "integer".into(),
        "int8" => "bigint".into(),
        "int2" => "smallint".into(),
        "float8" | "double" => "double precision".into(),
        "float4" => "real".into(),
        "bool" => "boolean".into(),
        "varchar" => "character varying".into(),
        _ => ty.to_string(),
    }
}

fn precedence(expression: &ScalarExpr) -> u8 {
    match expression {
        ScalarExpr::Or(_) => 10,
        ScalarExpr::And(_) | ScalarExpr::Between { .. } => 20,
        ScalarExpr::Not(_) => 30,
        ScalarExpr::Binary { op, .. } => operator_precedence(*op),
        ScalarExpr::IsNull { .. } | ScalarExpr::InList { .. } => 40,
        ScalarExpr::UnaryMinus(_) => 70,
        ScalarExpr::Cast { .. } => 80,
        _ => 100,
    }
}

fn operator_precedence(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Add | BinaryOp::Subtract => 50,
        BinaryOp::Multiply | BinaryOp::Divide => 60,
        _ => 40,
    }
}

fn operator(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Equal => "=",
        BinaryOp::NotEqual => "<>",
        BinaryOp::Less => "<",
        BinaryOp::LessEqual => "<=",
        BinaryOp::Greater => ">",
        BinaryOp::GreaterEqual => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Subtract => "-",
        BinaryOp::Multiply => "*",
        BinaryOp::Divide => "/",
    }
}
