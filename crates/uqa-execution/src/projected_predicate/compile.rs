//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection-slot binding for supported scalar predicates.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::expr::cast_value_from_with_control;
use uqa_sql::{SQLError, SQLParam};

use super::ProjectedExpr;
use crate::{RowSchema, ScalarExpr};

mod construction;
mod session;
use construction::{boxed, inline, literal};

#[expect(
    clippy::too_many_lines,
    reason = "predicate compiler exhaustively accepts or rejects each IR shape"
)]
pub(super) fn compile(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ProjectedExpr>>, SQLError> {
    if session::required(expression, schema, params, control)? {
        return Ok(None);
    }
    // This evaluator has no value catalog. Preserve such operands for the canonical evaluator instead of losing record/enum identity during predicate pushdown.
    if matches!(
        expression,
        ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. } | ScalarExpr::Param(_)
    ) && uqa_sql::scalar_type_with_control(expression, schema, params, control)?
        .as_ref()
        .is_some_and(|ty| uqa_sql::type_resolution::catalog_value_type(ty))
    {
        return Ok(None);
    }
    let compiled = match expression {
        ScalarExpr::Collated { expr, .. } => return compile(expr, schema, params, control),
        ScalarExpr::Column(column) => {
            let Some(index) = schema.unqualified_position(column) else {
                return Ok(None);
            };
            inline(field(index, schema)?, control)?
        }
        ScalarExpr::Position(index) if *index < schema.len() => {
            inline(field(*index, schema)?, control)?
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            let Some(index) = schema.qualified_position(qualifier, column) else {
                return Ok(None);
            };
            inline(field(index, schema)?, control)?
        }
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => {
            literal(control.copy_value(value)?, control)?
        }
        ScalarExpr::Param(index) => literal(parameter(*index, params, control)?, control)?,
        ScalarExpr::Binary { op, lhs, rhs } => {
            let integer_width = match uqa_sql::scalar_integer_operation_width_with_control(
                lhs, rhs, schema, params, control,
            ) {
                Ok(width) => width,
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => None,
            };
            let real_arithmetic = matches!(
                uqa_sql::scalar_type_with_control(expression, schema, params, control)?.as_deref(),
                Some(uqa_sql::ast::ColumnType::Real)
            );
            construction::binary(
                *op,
                require(lhs, schema, params, control)?,
                require(rhs, schema, params, control)?,
                integer_width,
                real_arithmetic,
                control,
            )?
        }
        ScalarExpr::UnaryMinus(expression) => construction::unary(
            require(expression, schema, params, control)?,
            ProjectedExpr::UnaryMinus,
            control,
        )?,
        ScalarExpr::Not(expression) => construction::unary(
            require(expression, schema, params, control)?,
            ProjectedExpr::Not,
            control,
        )?,
        ScalarExpr::And(items) => {
            construction::boolean(require_all(items, schema, params, control)?, true, control)?
        }
        ScalarExpr::Or(items) => {
            construction::boolean(require_all(items, schema, params, control)?, false, control)?
        }
        ScalarExpr::IsNull { expr, negated } => {
            let (expression, memory) =
                boxed(require(expr, schema, params, control)?, control)?.into_parts();
            control.finish(
                ProjectedExpr::IsNull {
                    expression,
                    negated: *negated,
                },
                memory,
            )?
        }
        ScalarExpr::Between { expr, low, high } => construction::between(
            require(expr, schema, params, control)?,
            require(low, schema, params, control)?,
            require(high, schema, params, control)?,
            control,
        )?,
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => construction::membership(
            require(expr, schema, params, control)?,
            require_all(list, schema, params, control)?,
            *negated,
            control,
        )?,
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            distinct,
            order_by,
            filter,
            ..
        } if !distinct && order_by.is_empty() && filter.is_none() => {
            let operator = match binding.dispatch {
                Some(uqa_sql::ast::FunctionDispatch::Pattern { operator, .. }) => operator,
                Some(
                    dispatch @ (uqa_sql::ast::FunctionDispatch::AnyOperator
                    | uqa_sql::ast::FunctionDispatch::AllOperator),
                ) => return compiled_array_membership(dispatch, args, schema, params, control),
                _ => return Ok(None),
            };
            if !operator.is_like() {
                return Ok(None);
            }
            // An indeterminate input collation is reported for each row the canonical operator evaluates.
            let collation = uqa_sql::ast::FunctionBinding::selected_input_collation(Some(binding));
            if collation == 0 {
                return Ok(None);
            }
            let [expression, pattern] = args.as_slice() else {
                return Ok(None);
            };
            let Some(pattern) = pattern_literal(pattern, params, control)? else {
                return Ok(None);
            };
            if matches!(&*pattern, Value::Null) {
                return Ok(Some(inline(ProjectedExpr::Literal(Value::Null), control)?));
            }
            let expression = boxed(require(expression, schema, params, control)?, control)?;
            let pattern = uqa_sql::expr::CompiledLikePattern::from_value_with_escape_with_control(
                &pattern,
                operator.case_insensitive(),
                None,
                collation,
                control,
            )?;
            let (expression, memory) = expression.into_parts();
            let compiled = control.finish(
                ProjectedExpr::Like {
                    expression,
                    pattern,
                },
                memory,
            )?;
            if operator.negated() {
                construction::unary(compiled, ProjectedExpr::Not, control)?
            } else {
                compiled
            }
        }
        ScalarExpr::Cast { expr, ty, .. } => {
            // A cast to a type whose input function reads the catalog, such as `regclass`, resolves names through the engine, which the projected form has no access to.
            if catalog_input(ty, control)? {
                return Ok(None);
            }
            let expression = require(expr, schema, params, control)?;
            match &*expression {
                // Integer casts retain their original row-evaluation error timing.
                ProjectedExpr::Literal(value) if !is_integer_type(ty) => literal(
                    cast_value_from_with_control(value, ty, None, control)?,
                    control,
                )?,
                _ => construction::cast(expression, ty, control)?,
            }
        }

        ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Func { .. }
        | ScalarExpr::Array(_)
        | ScalarExpr::Row(_)
        | ScalarExpr::CompositeRow { .. }
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::Case { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return Ok(None),
    };
    Ok(Some(compiled))
}

/// `x = ANY (array)` and `x <> ALL (array)` over a literal array are `IN` and `NOT IN` over its elements: the same equality and the same NULL result when no element decides. Only non-empty one-dimensional arrays of builtin, catalog-independent elements are compiled; an empty array keeps the canonical evaluator, whose ANY is false even for a NULL operand.
fn compiled_array_membership(
    dispatch: uqa_sql::ast::FunctionDispatch,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ProjectedExpr>>, SQLError> {
    let [expression, array, ScalarExpr::Literal(Value::Str(operator))] = args else {
        return Ok(None);
    };
    let (ScalarExpr::Literal(Value::Array(array))
    | ScalarExpr::TypedLiteral {
        value: Value::Array(array),
        ..
    }) = array
    else {
        return Ok(None);
    };
    let negated = match (dispatch, operator.as_str()) {
        (uqa_sql::ast::FunctionDispatch::AnyOperator, "=") => false,
        (uqa_sql::ast::FunctionDispatch::AllOperator, "<>" | "!=") => true,
        _ => return Ok(None),
    };
    let builtin_elements = array
        .element_type_oid()
        .is_some_and(|oid| oid < uqa_sql::catalog::oids::FIRST_NORMAL_OBJECT_ID);
    if array.elements().is_empty()
        || array.dimensions().len() != 1
        || !builtin_elements
        || array.elements().iter().any(|element| {
            matches!(element, Value::Enum(_))
                || uqa_sql::expr::composites::literal::contains_records(element)
                || uqa_sql::expr::datums::contains_datum(element)
        })
    {
        return Ok(None);
    }
    let expression = require(expression, schema, params, control)?;
    let mut list = ProductionVec::new(*control);
    for value in array.elements() {
        list.push_produced(literal(control.copy_value(value)?, control)?)?;
    }
    Ok(Some(construction::membership(
        expression,
        list.finish()?,
        negated,
        control,
    )?))
}

fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "smallint"
            | "int2"
            | "pg_catalog.int2"
            | "integer"
            | "int"
            | "int4"
            | "serial"
            | "serial4"
            | "pg_catalog.int4"
            | "bigint"
            | "int8"
            | "bigserial"
            | "serial8"
            | "pg_catalog.int8"
            | "oid"
            | "pg_catalog.oid"
            | "xid"
            | "pg_catalog.xid"
    )
}

fn require(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<ProjectedExpr>, SQLError> {
    compile(expression, schema, params, control)?.ok_or_else(|| {
        SQLError::Unsupported("expression cannot use positional predicate evaluation".into())
    })
}

fn require_all(
    expressions: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ProjectedExpr>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(expressions.len())?;
    for expression in expressions {
        output.push_produced(require(expression, schema, params, control)?)?;
    }
    Ok(output.finish()?)
}

/// Evaluate constant parser-owned escape helpers once while preparing the projection matcher.
fn pattern_literal(
    expression: &ScalarExpr,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => {
            Ok(Some(control.copy_value(value)?))
        }
        ScalarExpr::Param(index) => parameter(*index, params, control).map(Some),
        ScalarExpr::Collated { expr, .. } => pattern_literal(expr, params, control),
        ScalarExpr::Cast { expr, ty, .. } => {
            if catalog_input(ty, control)? {
                return Ok(None);
            }
            pattern_literal(expr, params, control)?
                .map(|value| cast_value_from_with_control(&value, ty, None, control))
                .transpose()
        }
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            distinct: false,
            order_by,
            filter: None,
            ..
        } if order_by.is_empty()
            && matches!(
                binding.dispatch,
                Some(uqa_sql::ast::FunctionDispatch::PatternEscape { .. })
            ) =>
        {
            let mut values = ProductionVec::new(*control);
            values.reserve(args.len())?;
            for argument in args {
                let Some(value) = pattern_literal(argument, params, control)? else {
                    return Ok(None);
                };
                let (value, memory) = value.into_parts();
                values.push_produced(control.finish((None, value), memory)?)?;
            }
            uqa_sql::expr::eval_builtin_function_call_with_control(
                &binding.name,
                Some(binding),
                values.finish()?,
                control,
                &uqa_sql::expr::EvalContext::new(None, params),
            )
            .map(Some)
        }
        _ => Ok(None),
    }
}

fn parameter(
    index: usize,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    match index.checked_sub(1).and_then(|offset| params.get(offset)) {
        Some(SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. }) => {
            Ok(control.copy_value(value)?)
        }
        Some(
            SQLParam::Vector(_)
            | SQLParam::Tensor(_)
            | SQLParam::TypedTensor(_)
            | SQLParam::DeferredError { .. },
        ) => Err(SQLError::Unsupported(
            "parameter requires canonical predicate evaluation".into(),
        )),
        None => Err(SQLError::MissingParam(index)),
    }
}

fn field(index: usize, schema: &RowSchema) -> Result<ProjectedExpr, SQLError> {
    if schema
        .column_type(index)
        .is_some_and(uqa_sql::type_resolution::catalog_value_type)
    {
        return Err(SQLError::Unsupported(
            "field values require catalog predicate evaluation".into(),
        ));
    }
    Ok(ProjectedExpr::Field(index))
}

fn catalog_input(ty: &str, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
    match uqa_sql::ColumnType::from_sql_name_with_control(ty, control) {
        Ok(ty) => Ok(uqa_sql::type_resolution::catalog_input_type(&ty)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(true),
    }
}
