//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar SQL reconstruction with `PostgreSQL` operator precedence.

use std::fmt::Write as _;

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, ColumnType, Expr, FunctionBinding, FunctionDispatch};
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::QueryPlan;

use super::{quote_ident, render_column, Deparser, SQLError, Scope};

mod operators;
#[cfg(test)]
mod tests;

impl Deparser<'_> {
    pub fn expression(
        &self,
        expression: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let Some(clause) = uqa_sql::catalog::collations::expression::scalar_clause(expression)? {
            return self.collation(clause, scope, subqueries);
        }
        match expression {
            ScalarExpr::Column(name) => Ok(self.column_reference(None, name, scope)),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                Ok(self.column_reference(Some(qualifier), column, scope))
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
            ScalarExpr::TypedLiteral { value, ty, .. } => self.typed_literal(value, ty),
            ScalarExpr::Collated { expr, .. } => self.expression(expr, scope, subqueries),
            ScalarExpr::Param(index) => Ok(self.parameter(*index, scope)),
            ScalarExpr::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, scope, subqueries),
            ScalarExpr::And(items) => self.boolean_expression(items, true, scope, subqueries),
            ScalarExpr::Or(items) => self.boolean_expression(items, false, scope, subqueries),
            ScalarExpr::Not(inner) => self.negation(inner, scope, subqueries),
            ScalarExpr::UnaryMinus(inner) => self.unary_minus(inner, scope, subqueries),
            ScalarExpr::IsNull { expr, negated } => Ok(self.parenthesize(format!(
                "{} IS {}NULL",
                self.operand(expr, 80, false, scope, subqueries)?,
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
            ScalarExpr::CompositeRow { items, binding, .. } => {
                self.composite_row(items, binding, scope, subqueries, true)
            }
            ScalarExpr::Row(items) => Ok(format!(
                "ROW({})",
                self.expressions(items, scope, subqueries)?
            )),
            ScalarExpr::Cast { expr, ty, implicit } => {
                self.cast_with_origin(expr, ty, *implicit, scope, subqueries)
            }
            ScalarExpr::Func { .. } => self.aggregate(expression, scope, subqueries),
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                ..
            } => self.window_call(name, args, (filter.as_deref(), spec), scope, subqueries),
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

    fn collation(
        &self,
        clause: uqa_sql::catalog::collations::expression::Clause<'_, ScalarExpr>,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let name = match clause.oid {
            Some(oid) => crate::catalog::projection::regtypes::format_expression_collation(
                self.catalog,
                &self.dynamic,
                oid,
            )
            .ok_or_else(|| SQLError::Internal("unknown collation identity".into()))?,
            None => clause.name.to_string(),
        };
        Ok(self.parenthesize(format!(
            "{} COLLATE {name}",
            self.operand(clause.argument, 75, false, scope, subqueries)?
        )))
    }

    fn boolean_expression(
        &self,
        items: &[ScalarExpr],
        conjunction: bool,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let (operator, precedence) = if conjunction {
            (" AND ", 20)
        } else {
            (" OR ", 10)
        };
        let expressions = items
            .iter()
            .map(|item| self.operand(item, precedence, false, scope, subqueries))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.parenthesize(expressions.join(operator)))
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

    pub fn expressions(
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

    pub(super) fn cast(
        &self,
        expr: &ScalarExpr,
        ty: &str,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        self.cast_with_origin(expr, ty, false, scope, subqueries)
    }

    fn cast_with_origin(
        &self,
        expr: &ScalarExpr,
        ty: &str,
        implicit: bool,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let Some(rendered) = self.coercion(expr, ty, implicit, scope, subqueries)? {
            return Ok(rendered);
        }
        let display = self.type_display(ty)?;
        if let ScalarExpr::Literal(value) = expr {
            if matches!(value, Value::Str(_)) {
                if let Some(constant) = self.literal_cast_constant(value, ty)? {
                    return Ok(constant);
                }
            }
            let ty = type_name(ty);
            if literal_has_type(value, &ty) {
                return literal(value);
            }
            if matches!(value, Value::Str(_) | Value::Null) {
                let value = uqa_sql::render::expression_sql(&Expr::Literal(value.clone()))?;
                return Ok(format!("{value}::{display}"));
            }
        }
        let value = if let ScalarExpr::CompositeRow { items, binding, .. } = expr {
            if matches!(self.resolved_type(ty), Some(ColumnType::Domain { .. })) {
                self.composite_row(items, binding, scope, subqueries, false)?
            } else {
                self.expression(expr, scope, subqueries)?
            }
        } else {
            self.expression(expr, scope, subqueries)?
        };
        // A row constructor coerced to a named type is one `RowExpr`, which prints its type after the row.
        if matches!(expr, ScalarExpr::Row(_)) {
            return Ok(format!("{value}::{display}"));
        }
        if self.pretty {
            Ok(format!(
                "{}::{display}",
                self.operand(expr, 80, false, scope, subqueries)?
            ))
        } else {
            Ok(format!("({value})::{display}"))
        }
    }

    /// `get_coercion_expr`'s form of a cast of an `unknown` literal: `coerce_type` reads the literal with the type's input function, passing no type modifier except to `interval`, and the constant prints as `get_const_expr` prints it with the cast's type and modifier (`'11:00:00'::time(3) without time zone`, `'\x79'::bytea`, `1` for `'1'::int`). A type whose input consults the catalog, an enum type and a domain keep the written literal, which their own printing resolves; so does a literal the input function rejects, which the written text shows as it was accepted.
    fn literal_cast_constant(&self, value: &Value, ty: &str) -> Result<Option<String>, SQLError> {
        let Some(resolved) = self.resolved_type(ty) else {
            return Ok(None);
        };
        if matches!(resolved, ColumnType::Domain { .. })
            || uqa_sql::type_resolution::catalog_input_type(&resolved)
            || uqa_sql::expr::enums::is_enum_bearing(&resolved)
        {
            return Ok(None);
        }
        let input_type = if matches!(resolved, ColumnType::IntervalWithFields { .. }) {
            resolved.clone()
        } else {
            resolved.without_type_modifiers()
        };
        let Ok(constant) = uqa_sql::expr::cast_value(value, &input_type.catalog_name()) else {
            return Ok(None);
        };
        self.typed_literal(&constant, ty).map(Some)
    }

    fn value_null_test(
        &self,
        args: &[ScalarExpr],
        binding: Option<&FunctionBinding>,
        negated: bool,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let [argument] = args else {
            return Err(SQLError::Internal("invalid outer NULL test".into()));
        };
        let ty = binding
            .and_then(|binding| binding.argument_types.first())
            .and_then(|name| self.resolved_type(name));
        let mut ty = ty.as_ref();
        while let Some(ColumnType::Domain { base, .. }) = ty {
            ty = Some(base);
        }
        let operator = match (
            matches!(ty, Some(ColumnType::Record | ColumnType::Composite(_))),
            negated,
        ) {
            (true, true) => "IS DISTINCT FROM NULL",
            (true, false) => "IS NOT DISTINCT FROM NULL",
            (false, true) => "IS NOT NULL",
            (false, false) => "IS NULL",
        };
        Ok(self.parenthesize(format!(
            "{} {operator}",
            self.operand(argument, 80, false, scope, subqueries)?
        )))
    }

    fn boolean_test(
        &self,
        args: &[ScalarExpr],
        test: uqa_sql::ast::BooleanTest,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let [argument] = args else {
            return Err(SQLError::Internal("Boolean test takes one input".into()));
        };
        Ok(self.parenthesize(format!(
            "{} {}",
            self.operand(argument, 80, false, scope, subqueries)?,
            test.label(),
        )))
    }

    pub fn function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if let Some(
            dispatch @ (FunctionDispatch::ArraySubscripts
            | FunctionDispatch::ArraySlices
            | FunctionDispatch::ArraySlicesWithBounds(_)),
        ) = binding.and_then(|binding| binding.dispatch)
        {
            return self.array_subscripts(dispatch, args, scope, subqueries);
        }
        if binding.and_then(|binding| binding.dispatch) == Some(FunctionDispatch::FieldSelect) {
            return self.field_selection(args, scope, subqueries);
        }
        if let Some(FunctionDispatch::NumericOperator(operator)) =
            binding.and_then(|binding| binding.dispatch)
        {
            return self.numeric_operator(operator, args, scope, subqueries);
        }
        if let Some(symbol) = binding
            .and_then(|binding| binding.dispatch)
            .and_then(FunctionDispatch::json_extraction_symbol)
        {
            return self.json_operator(symbol, args, scope, subqueries);
        }
        match binding.and_then(|binding| binding.dispatch) {
            Some(FunctionDispatch::DistinctNot) => {
                let [argument] = args else {
                    return Err(SQLError::Internal("invalid distinct negation".into()));
                };
                return self.negation(argument, scope, subqueries);
            }
            Some(FunctionDispatch::RowDistinct) => {
                return self.boolean_expression(args, false, scope, subqueries);
            }
            Some(FunctionDispatch::BooleanTest(test)) => {
                return self.boolean_test(args, test, scope, subqueries);
            }
            Some(FunctionDispatch::ValueNullTest { negated }) => {
                return self.value_null_test(args, binding, negated, scope, subqueries);
            }
            // `get_rule_expr` prints a `DistinctExpr` as an operator.
            Some(FunctionDispatch::IsDistinct | FunctionDispatch::ValueIsDistinct) => {
                return self.distinct_operator(args, scope, subqueries);
            }
            // A `ScalarArrayOpExpr`: the operator, then `ANY` or `ALL` over the parenthesized array.
            Some(dispatch @ (FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator)) => {
                let [left, right, ScalarExpr::Literal(Value::Str(operator))] = args else {
                    return Err(SQLError::Internal(
                        "invalid quantified operator operands".into(),
                    ));
                };
                // The array stands inside the parentheses `ANY` and `ALL` write, so it needs none of its own.
                return Ok(self.parenthesize(format!(
                    "{} {operator} {} ({})",
                    self.operand(left, 80, false, scope, subqueries)?,
                    if dispatch == FunctionDispatch::AnyOperator {
                        "ANY"
                    } else {
                        "ALL"
                    },
                    self.expression(right, scope, subqueries)?
                )));
            }
            _ => {}
        }
        if let [left, right] = args {
            if let Some((operator, precedence)) = binary_function_operator(binding) {
                return Ok(self.parenthesize(format!(
                    "{} {operator} {}",
                    self.operand(left, precedence, false, scope, subqueries)?,
                    self.operand(right, precedence, true, scope, subqueries)?
                )));
            }
        }
        let name = self.function_name(name, binding)?;
        if matches!(
            binding.and_then(|binding| binding.dispatch),
            Some(FunctionDispatch::JsonPath { .. })
        ) {
            return self.json_path_call(&name, args, scope, subqueries);
        }
        let arguments = args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                let decoded = uqa_sql::scalar_call_argument(argument)?;
                if let Some(name) = decoded.name {
                    return Ok(format!(
                        "{} => {}",
                        quote_ident(name),
                        self.expression(decoded.value, scope, subqueries)?
                    ));
                }
                if matches!(argument, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
                    if let Some(ty) = binding.and_then(|binding| binding.argument_types.get(index))
                    {
                        return self.cast(argument, ty, scope, subqueries);
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
        // Stored calls retain object identity across routine and schema renames.
        let name = binding
            .filter(|binding| !binding.builtin)
            .and_then(|binding| binding.object_id)
            .and_then(|identity| {
                self.catalog
                    .snapshot()
                    .definitions
                    .sql_user_functions
                    .values()
                    .flatten()
                    .find(|function| function.def.object_id == Some(identity))
            })
            .map_or(name, |function| function.def.name.as_str());
        let (schema, local) =
            super::RelationIdentity::parse_reference(name).map_err(SQLError::Internal)?;
        if matches!(local.as_str(), "coalesce" | "nullif" | "greatest" | "least")
            && schema
                .as_deref()
                .is_none_or(|schema| schema == "pg_catalog")
        {
            return Ok(local.to_ascii_uppercase());
        }
        if let Some(binding) = binding {
            if let Some(schema) = schema
                .as_deref()
                .or_else(|| binding.builtin.then_some("pg_catalog"))
            {
                let argument_types = binding
                    .argument_types
                    .iter()
                    .map(|ty| super::super::regtypes::catalog_routine_type_oid(self.catalog, ty))
                    .collect::<Vec<_>>();
                if let Some(parts) =
                    self.alias_output()?
                        .routine_name_parts(schema, &local, &argument_types)
                {
                    return Ok(parts
                        .iter()
                        .map(|part| quote_ident(part))
                        .collect::<Vec<_>>()
                        .join("."));
                }
                if binding.builtin && schema == "pg_catalog" {
                    return Ok(quote_ident(&local));
                }
            }
        }
        let Some(schema) = schema else {
            return Ok(quote_ident(&local));
        };
        if schema == "pg_catalog" && binding.is_none() {
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
            order_syntax,
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
        if *order_syntax == uqa_sql::ast::FunctionCallSyntax::Extract {
            let [field, source] = args.as_slice() else {
                return Err(SQLError::Internal("EXTRACT requires two operands".into()));
            };
            let mut field = field;
            while let ScalarExpr::Cast { expr, .. } = field {
                field = expr;
            }
            let (ScalarExpr::Literal(Value::Str(field))
            | ScalarExpr::TypedLiteral {
                value: Value::Str(field),
                ..
            }) = field
            else {
                return Err(SQLError::Internal(
                    "EXTRACT field is not a text constant".into(),
                ));
            };
            return Ok(format!(
                "EXTRACT({field} FROM {})",
                self.expression(source, scope, subqueries)?
            ));
        }
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
            if *order_syntax == uqa_sql::ast::FunctionOrderSyntax::WithinGroup {
                write!(rendered, " WITHIN GROUP (ORDER BY {})", order.join(", "))
                    .expect("writing to a String cannot fail");
            } else {
                rendered.pop();
                write!(rendered, " ORDER BY {})", order.join(", "))
                    .expect("writing to a String cannot fail");
            }
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
        if !self.indent {
            return self.inline_case(base, when, otherwise, scope, subqueries);
        }
        // A standalone expression starts at indentation level zero; query clauses indent their expressions one level.
        let indent = " ".repeat(if self.standalone {
            scope.indent
        } else {
            scope.indent + 8
        });
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

    /// `CASE` as `get_rule_expr` prints it without `PRETTYFLAG_INDENT`, on one line.
    fn inline_case(
        &self,
        base: Option<&ScalarExpr>,
        when: &[(ScalarExpr, ScalarExpr)],
        otherwise: Option<&ScalarExpr>,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let mut rendered = String::from("CASE");
        if let Some(base) = base {
            rendered.push(' ');
            rendered.push_str(&self.expression(base, scope, subqueries)?);
        }
        for (condition, value) in when {
            write!(
                rendered,
                " WHEN {} THEN {}",
                self.expression(condition, scope, subqueries)?,
                self.expression(value, scope, subqueries)?
            )
            .expect("writing to a String cannot fail");
        }
        if let Some(otherwise) = otherwise {
            write!(
                rendered,
                " ELSE {}",
                self.expression(otherwise, scope, subqueries)?
            )
            .expect("writing to a String cannot fail");
        }
        rendered.push_str(" END");
        Ok(rendered)
    }
}

pub(super) fn literal(value: &Value) -> Result<String, SQLError> {
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

/// `format_type_with_typemod` of a stored built-in type name: the SQL spelling with its modifier (`integer`, `character varying(3)`, `time(3) without time zone`), or the name as written when it is not a built-in type.
pub(super) fn type_name(ty: &str) -> String {
    ColumnType::from_sql_name(ty).map_or_else(|_| ty.to_string(), |resolved| resolved.sql_name())
}

fn precedence(expression: &ScalarExpr) -> u8 {
    if let ScalarExpr::Collated { expr, .. } = expression {
        return precedence(expr);
    }
    // IS predicates bind below comparison operators and above NOT.
    match expression {
        // PostgreSQL's isSimpleNode keeps a CollateExpr parenthesized when used as an operand.
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if matches!(binding.dispatch, Some(FunctionDispatch::Collate { .. })) => 0,
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if matches!(
            binding.dispatch,
            Some(uqa_sql::ast::FunctionDispatch::NumericOperator(_))
        ) =>
        {
            let Some(uqa_sql::ast::FunctionDispatch::NumericOperator(operator)) = binding.dispatch
            else {
                unreachable!()
            };
            numeric_operator_precedence(operator)
        }
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if matches!(
            binding.dispatch,
            Some(
                uqa_sql::ast::FunctionDispatch::IsDistinct
                    | uqa_sql::ast::FunctionDispatch::ValueIsDistinct
                    | uqa_sql::ast::FunctionDispatch::ValueNullTest { .. }
                    | uqa_sql::ast::FunctionDispatch::BooleanTest(_)
            )
        ) =>
        {
            35
        }
        // `isSimpleNode` retains operand parentheses around quantified comparisons and non-arithmetic JSON operators.
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if matches!(
            binding.dispatch,
            Some(
                uqa_sql::ast::FunctionDispatch::AnyOperator
                    | uqa_sql::ast::FunctionDispatch::AllOperator
                    | uqa_sql::ast::FunctionDispatch::JsonExtract { .. }
            )
        ) =>
        {
            0
        }
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if binding.dispatch == Some(FunctionDispatch::RowDistinct) => 10,
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if binding.dispatch == Some(FunctionDispatch::DistinctNot) => 30,
        ScalarExpr::Func { binding, args, .. } if args.len() == 2 => {
            binary_function_operator(binding.as_ref()).map_or(100, |(_, precedence)| precedence)
        }
        ScalarExpr::InList { list, .. } if list.len() > 1 => 0,
        ScalarExpr::Or(_) => 10,
        ScalarExpr::And(_) | ScalarExpr::Between { .. } => 20,
        ScalarExpr::Not(_) => 30,
        ScalarExpr::Binary { op, .. } => operator_precedence(*op),
        ScalarExpr::IsNull { .. } => 35,
        ScalarExpr::InList { .. } => 40,
        ScalarExpr::UnaryMinus(_) => 70,
        ScalarExpr::Cast { .. } => 80,
        _ => 100,
    }
}

fn numeric_operator_precedence(operator: uqa_sql::ast::NumericOperator) -> u8 {
    use uqa_sql::ast::NumericOperator;
    match operator {
        NumericOperator::Modulo => 60,
        NumericOperator::Power => 65,
        NumericOperator::Plus => 70,
        NumericOperator::Absolute | NumericOperator::SquareRoot | NumericOperator::CubeRoot => 45,
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

/// The operator and its precedence that a function of two arguments prints as.
fn binary_function_operator(binding: Option<&FunctionBinding>) -> Option<(&'static str, u8)> {
    if let Some(FunctionDispatch::Pattern { operator, .. }) =
        binding.and_then(|binding| binding.dispatch)
    {
        return Some((operator.symbol(), 40));
    }
    if binding
        .and_then(|binding| binding.dispatch)
        .is_some_and(FunctionDispatch::is_concatenation)
    {
        return Some(("||", 45));
    }
    None
}

/// Whether a function node prints as a call, `name(arguments)`, rather than as subscripts or an operator.
pub(super) fn prints_as_call(binding: Option<&FunctionBinding>, argument_count: usize) -> bool {
    let printed_otherwise = matches!(
        binding.and_then(|binding| binding.dispatch),
        Some(
            FunctionDispatch::ArraySubscripts
                | FunctionDispatch::ArraySlices
                | FunctionDispatch::ArraySlicesWithBounds(_)
                | FunctionDispatch::FieldSelect
                | FunctionDispatch::NumericOperator(_)
                | FunctionDispatch::JsonExtract { .. }
                | FunctionDispatch::Collate { .. }
        )
    ) || (argument_count == 2
        && binary_function_operator(binding).is_some());
    !printed_otherwise
}
