//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection-slot binding for supported scalar predicates.

use uqa_core::Value;
use uqa_sql::expr::cast_value;
use uqa_sql::{SQLError, SQLParam};

use super::{ProjectedExpr, ProjectedIntPredicate};
use crate::scalar::scalar_integer_binary_width;
use crate::{RowSchema, ScalarExpr};

mod session;

#[expect(
    clippy::too_many_lines,
    reason = "predicate compiler exhaustively accepts or rejects each IR shape"
)]
pub(super) fn compile(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ProjectedExpr>, SQLError> {
    if session::required(expression, schema, params)? {
        return Ok(None);
    }
    // This evaluator has no value catalog. Preserve such operands for the canonical evaluator instead of losing record/enum identity during predicate pushdown.
    if matches!(
        expression,
        ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. } | ScalarExpr::Param(_)
    ) && crate::scalar_type(expression, schema, params)?
        .as_ref()
        .is_some_and(uqa_sql::type_resolution::catalog_value_type)
    {
        return Ok(None);
    }
    let compiled = match expression {
        ScalarExpr::Collated { expr, .. } => return compile(expr, schema, params),
        ScalarExpr::Column(column) => {
            let Some(index) = schema.unqualified_position(column) else {
                return Ok(None);
            };
            field(index, schema)?
        }
        ScalarExpr::Position(index) if *index < schema.len() => field(*index, schema)?,
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            let Some(index) = schema.qualified_position(qualifier, column) else {
                return Ok(None);
            };
            field(index, schema)?
        }
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => {
            ProjectedExpr::Literal(value.clone())
        }
        ScalarExpr::Param(index) => ProjectedExpr::Literal(parameter(*index, params)?),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let integer_width = scalar_integer_binary_width(lhs, rhs, schema, params);
            let real_arithmetic = matches!(
                crate::scalar_type(expression, schema, params)?,
                Some(uqa_sql::ast::ColumnType::Real)
            );
            compiled_binary(
                *op,
                require(lhs, schema, params)?,
                require(rhs, schema, params)?,
                integer_width,
                real_arithmetic,
            )
        }
        ScalarExpr::UnaryMinus(expression) => {
            ProjectedExpr::UnaryMinus(Box::new(require(expression, schema, params)?))
        }
        ScalarExpr::Not(expression) => {
            ProjectedExpr::Not(Box::new(require(expression, schema, params)?))
        }
        ScalarExpr::And(items) => compiled_and(require_all(items, schema, params)?),
        ScalarExpr::Or(items) => ProjectedExpr::Or(require_all(items, schema, params)?),
        ScalarExpr::IsNull { expr, negated } => ProjectedExpr::IsNull {
            expression: Box::new(require(expr, schema, params)?),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => compiled_between(
            require(expr, schema, params)?,
            require(low, schema, params)?,
            require(high, schema, params)?,
        ),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ProjectedExpr::InList {
            expression: Box::new(require(expr, schema, params)?),
            list: require_all(list, schema, params)?,
            negated: *negated,
        },
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
                ) => return compiled_array_membership(dispatch, args, schema, params),
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
            let Some(pattern) = pattern_literal(pattern, params)? else {
                return Ok(None);
            };
            if matches!(&pattern, Value::Null) {
                return Ok(Some(ProjectedExpr::Literal(Value::Null)));
            }
            let compiled = ProjectedExpr::Like {
                expression: Box::new(require(expression, schema, params)?),
                pattern: uqa_sql::expr::CompiledLikePattern::from_value_with_escape_with_control(
                    &pattern,
                    operator.case_insensitive(),
                    None,
                    collation,
                    &uqa_core::memory::ProductionControl::uncontrolled(),
                )?,
            };
            if operator.negated() {
                ProjectedExpr::Not(Box::new(compiled))
            } else {
                compiled
            }
        }
        ScalarExpr::Cast { expr, ty, .. } => {
            // A cast to a type whose input function reads the catalog, such as `regclass`, resolves names through the engine, which the projected form has no access to.
            if uqa_sql::ast::ColumnType::from_sql_name(ty)
                .map_or(true, |ty| uqa_sql::type_resolution::catalog_input_type(&ty))
            {
                return Ok(None);
            }
            let expression = require(expr, schema, params)?;
            match expression {
                // Session-independent noninteger literal casts can be evaluated once; integer casts retain row-evaluation error timing.
                ProjectedExpr::Literal(value) if is_integer_type(ty) => ProjectedExpr::Cast {
                    expression: Box::new(ProjectedExpr::Literal(value)),
                    ty: ty.clone(),
                },
                ProjectedExpr::Literal(value) => ProjectedExpr::Literal(cast_value(&value, ty)?),
                expression => ProjectedExpr::Cast {
                    expression: Box::new(expression),
                    ty: ty.clone(),
                },
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
) -> Result<Option<ProjectedExpr>, SQLError> {
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
    Ok(Some(ProjectedExpr::InList {
        expression: Box::new(require(expression, schema, params)?),
        list: array
            .elements()
            .iter()
            .cloned()
            .map(ProjectedExpr::Literal)
            .collect(),
        negated,
    }))
}

fn compiled_and(items: Vec<ProjectedExpr>) -> ProjectedExpr {
    let all_integer_fields = items.iter().all(|item| {
        matches!(
            item,
            ProjectedExpr::IntFieldComparison { .. }
                | ProjectedExpr::IntFieldBetween { .. }
                | ProjectedExpr::IntFieldConjunction(_)
        )
    });
    if !all_integer_fields {
        return ProjectedExpr::And(items);
    }
    let mut predicates = Vec::with_capacity(items.len());
    for item in items {
        match item {
            ProjectedExpr::IntFieldComparison {
                field,
                op,
                literal,
                field_on_left,
            } => {
                push_integer_predicate(
                    &mut predicates,
                    ProjectedIntPredicate::Comparison {
                        field,
                        op,
                        literal,
                        field_on_left,
                    },
                );
            }
            ProjectedExpr::IntFieldBetween { field, low, high } => {
                push_integer_predicate(
                    &mut predicates,
                    ProjectedIntPredicate::Between { field, low, high },
                );
            }
            ProjectedExpr::IntFieldConjunction(items) => {
                for item in items {
                    push_integer_predicate(&mut predicates, item);
                }
            }
            _ => unreachable!("integer predicate conjunction was validated"),
        }
    }
    ProjectedExpr::IntFieldConjunction(predicates)
}

fn push_integer_predicate(
    predicates: &mut Vec<ProjectedIntPredicate>,
    next: ProjectedIntPredicate,
) {
    if let Some(range) = predicates
        .last()
        .and_then(|previous| adjacent_range(previous, &next))
    {
        *predicates
            .last_mut()
            .expect("matched the preceding comparison") = range;
    } else {
        predicates.push(next);
    }
}

/// Borrowing one field is stable within the input row. Coalesce only adjacent
/// inclusive lower/upper comparisons; no cast, effect or intervening predicate moves.
/// Commuted operands keep their original order in fallback error diagnostics.
fn adjacent_range(
    lower: &ProjectedIntPredicate,
    upper: &ProjectedIntPredicate,
) -> Option<ProjectedIntPredicate> {
    use uqa_sql::ast::BinaryOp::{GreaterEqual, LessEqual};
    let (
        ProjectedIntPredicate::Comparison {
            field: lower_field,
            op: GreaterEqual,
            literal: low,
            field_on_left: true,
        },
        ProjectedIntPredicate::Comparison {
            field: upper_field,
            op: LessEqual,
            literal: high,
            field_on_left: true,
        },
    ) = (lower, upper)
    else {
        return None;
    };
    (*lower_field == *upper_field).then_some(ProjectedIntPredicate::Between {
        field: *lower_field,
        low: *low,
        high: *high,
    })
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

fn compiled_binary(
    op: uqa_sql::ast::BinaryOp,
    lhs: ProjectedExpr,
    rhs: ProjectedExpr,
    integer_width: Option<uqa_sql::expr::IntegerWidth>,
    real_arithmetic: bool,
) -> ProjectedExpr {
    use uqa_sql::ast::BinaryOp;

    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        match (&lhs, &rhs) {
            (ProjectedExpr::Field(field), ProjectedExpr::Literal(Value::Int(literal))) => {
                return ProjectedExpr::IntFieldComparison {
                    field: *field,
                    op,
                    literal: *literal,
                    field_on_left: true,
                };
            }
            (ProjectedExpr::Literal(Value::Int(literal)), ProjectedExpr::Field(field)) => {
                return ProjectedExpr::IntFieldComparison {
                    field: *field,
                    op,
                    literal: *literal,
                    field_on_left: false,
                };
            }
            _ => {}
        }
    }
    ProjectedExpr::Binary {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
        integer_width,
        real_arithmetic,
    }
}

fn compiled_between(
    expression: ProjectedExpr,
    low: ProjectedExpr,
    high: ProjectedExpr,
) -> ProjectedExpr {
    if let (
        ProjectedExpr::Field(field),
        ProjectedExpr::Literal(Value::Int(low)),
        ProjectedExpr::Literal(Value::Int(high)),
    ) = (&expression, &low, &high)
    {
        return ProjectedExpr::IntFieldBetween {
            field: *field,
            low: *low,
            high: *high,
        };
    }
    ProjectedExpr::Between {
        expression: Box::new(expression),
        low: Box::new(low),
        high: Box::new(high),
    }
}

fn require(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ProjectedExpr, SQLError> {
    compile(expression, schema, params)?.ok_or_else(|| {
        SQLError::Unsupported("expression cannot use positional predicate evaluation".into())
    })
}

fn require_all(
    expressions: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<ProjectedExpr>, SQLError> {
    expressions
        .iter()
        .map(|expression| require(expression, schema, params))
        .collect()
}

/// Evaluate constant parser-owned escape helpers once while preparing the projection matcher.
fn pattern_literal(
    expression: &ScalarExpr,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => {
            Ok(Some(value.clone()))
        }
        ScalarExpr::Param(index) => parameter(*index, params).map(Some),
        ScalarExpr::Collated { expr, .. } => pattern_literal(expr, params),
        ScalarExpr::Cast { expr, ty, .. } => {
            if uqa_sql::ColumnType::from_sql_name(ty)
                .map_or(true, |ty| uqa_sql::type_resolution::catalog_input_type(&ty))
            {
                return Ok(None);
            }
            pattern_literal(expr, params)?
                .map(|value| cast_value(&value, ty))
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
            let mut values = Vec::with_capacity(args.len());
            for argument in args {
                let Some(value) = pattern_literal(argument, params)? else {
                    return Ok(None);
                };
                values.push((None, value));
            }
            uqa_sql::expr::eval_bound_builtin_function_call(
                binding,
                values,
                &uqa_sql::expr::EvalContext::new(None, params),
            )
            .map(Some)
        }
        _ => Ok(None),
    }
}

fn parameter(index: usize, params: &[SQLParam]) -> Result<Value, SQLError> {
    match index.checked_sub(1).and_then(|offset| params.get(offset)) {
        Some(SQLParam::Scalar(value) | SQLParam::TypedScalar { value, .. }) => Ok(value.clone()),
        Some(SQLParam::Vector(_) | SQLParam::Tensor(_) | SQLParam::DeferredError { .. }) => Err(
            SQLError::Unsupported("parameter requires canonical predicate evaluation".into()),
        ),
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
