//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static type and routine binding for generated-column expressions.

use crate::engine_user_functions::{canonical_routine_type_name, routine_signature_types};
use crate::sql::{builtin_function_dispatch_name, ColumnType, Engine, SQLError, Value};
use uqa_sql::ast::{
    BinaryOp, ColumnDef, Expr, FunctionBinding, FunctionDispatch, FunctionReturns,
    GeneratedFunctionDependency, RangeFunctionOperation, RangeSubtype,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GenerationType {
    Null,
    UnknownLiteral(String),
    Boolean,
    Void,
    SmallInteger,
    Integer,
    BigInteger,
    Oid,
    Xid,
    Real,
    Numeric,
    Text,
    Uuid,
    Bytea,
    Json,
    JsonB,
    Array(Box<GenerationType>),
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
    Range(RangeSubtype),
    Multirange(RangeSubtype),
    Vector,
    Tensor,
    Record,
}

#[derive(Debug, Clone, Copy)]
enum TypeClass {
    Boolean,
    Integer,
    Numeric,
    Text,
    Bytea,
    Array,
    Json,
    JsonB,
    Temporal,
}

pub(super) fn infer_generation_expression(
    engine: &Engine,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(GenerationType, Vec<GeneratedFunctionDependency>), SQLError> {
    let mut dependencies = Vec::new();
    bind_function_calls(engine, columns, expression, &mut dependencies)?;
    let ty = infer_expression(engine, columns, expression, &mut dependencies)?;
    dependencies.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.argument_types.cmp(&right.argument_types))
    });
    dependencies.dedup();
    Ok((ty, dependencies))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves generated coercion diagnostics"
)]
fn bind_function_calls(
    engine: &Engine,
    columns: &[ColumnDef],
    expression: &mut Expr,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<(), SQLError> {
    match expression {
        Expr::Func {
            name,
            binding,
            args,
            filter,
            order_by,
            ..
        } => {
            for argument in args.iter_mut() {
                bind_function_calls(engine, columns, argument, dependencies)?;
            }
            for order in order_by {
                bind_function_calls(engine, columns, &mut order.expr, dependencies)?;
            }
            if let Some(filter) = filter {
                bind_function_calls(engine, columns, filter, dependencies)?;
            }
            if binding
                .as_ref()
                .and_then(|binding| binding.dispatch)
                .is_some()
            {
                return Ok(());
            }
            let call_arguments = generated_call_arguments(args)?;
            let explicit_variadic = call_arguments
                .iter()
                .any(|argument| argument.explicit_variadic);
            let mut argument_names = Vec::with_capacity(call_arguments.len());
            let mut argument_types = Vec::with_capacity(call_arguments.len());
            for argument in &call_arguments {
                argument_names.push(argument.name.clone());
                argument_types.push(infer_expression(
                    engine,
                    columns,
                    argument.value,
                    dependencies,
                )?);
            }
            if binding.is_none()
                && engine
                    .registered_runtime_function_volatility(name)
                    .is_some()
            {
                return Ok(());
            }
            if builtin::bind_fixed_builtin_call(
                builtin::FixedBuiltinCall {
                    engine,
                    columns,
                    name,
                    args,
                    argument_names: &argument_names,
                    argument_types: &argument_types,
                    explicit_variadic,
                },
                binding,
                dependencies,
            )? {
                return Ok(());
            }
            if binding
                .as_ref()
                .is_some_and(FunctionBinding::is_polymorphic_builtin_syntax)
            {
                return Ok(());
            }
            if binding.as_ref().is_some_and(|binding| !binding.builtin)
                || engine.lookup_visible_sql_functions(name)?.is_some()
            {
                let declared_argument_types = call_arguments
                    .iter()
                    .zip(&argument_types)
                    .map(|(argument, inferred)| {
                        Ok(generation_expression_column_type(
                            columns,
                            argument.value,
                            inferred,
                        ))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?;
                let selected =
                    <Engine as uqa_execution::FunctionTypeResolver>::resolve_function_overload(
                        engine,
                        name,
                        binding.as_ref(),
                        &argument_names,
                        &declared_argument_types,
                        explicit_variadic,
                    )?
                    .ok_or_else(|| {
                        uqa_execution::function_resolution_error(
                            "42883",
                            name,
                            &argument_names,
                            &declared_argument_types,
                            "does not exist",
                        )
                    })?;
                let selected = validate_bound_function(
                    engine,
                    &selected.binding,
                    &argument_names,
                    &argument_types,
                )?;
                dependencies.push(selected.clone());
                *binding = Some(selected);
            }
            Ok(())
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                bind_function_calls(engine, columns, item, dependencies)?;
            }
            Ok(())
        }
        Expr::Binary { lhs, rhs, .. } => {
            bind_function_calls(engine, columns, lhs, dependencies)?;
            bind_function_calls(engine, columns, rhs, dependencies)
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            bind_function_calls(engine, columns, inner, dependencies)
        }
        Expr::Between { expr, low, high } => {
            bind_function_calls(engine, columns, expr, dependencies)?;
            bind_function_calls(engine, columns, low, dependencies)?;
            bind_function_calls(engine, columns, high, dependencies)
        }
        Expr::InList { expr, list, .. } => {
            bind_function_calls(engine, columns, expr, dependencies)?;
            for item in list {
                bind_function_calls(engine, columns, item, dependencies)?;
            }
            Ok(())
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                bind_function_calls(engine, columns, base, dependencies)?;
            }
            for (condition, result) in when {
                bind_function_calls(engine, columns, condition, dependencies)?;
                bind_function_calls(engine, columns, result, dependencies)?;
            }
            if let Some(else_branch) = else_branch {
                bind_function_calls(engine, columns, else_branch, dependencies)?;
            }
            Ok(())
        }
        Expr::Default
        | Expr::Param(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => Ok(()),
    }
}

pub(super) fn column_generation_type(ty: &ColumnType) -> GenerationType {
    match ty {
        ColumnType::SmallInteger => GenerationType::SmallInteger,
        ColumnType::Integer => GenerationType::Integer,
        ColumnType::BigInteger => GenerationType::BigInteger,
        ColumnType::Oid => GenerationType::Oid,
        ColumnType::Xid => GenerationType::Xid,
        ColumnType::Boolean => GenerationType::Boolean,
        ColumnType::Void => GenerationType::Void,
        ColumnType::Text
        | ColumnType::RefCursor
        | ColumnType::Name
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::InternalChar
        | ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype
        | ColumnType::PgNodeTree
        | ColumnType::AclItem => GenerationType::Text,
        ColumnType::Uuid => GenerationType::Uuid,
        ColumnType::Real | ColumnType::DoublePrecision => GenerationType::Real,
        ColumnType::Numeric { .. } => GenerationType::Numeric,
        ColumnType::Json => GenerationType::Json,
        ColumnType::JsonB => GenerationType::JsonB,
        ColumnType::Bytea => GenerationType::Bytea,
        ColumnType::Int2Vector => GenerationType::Array(Box::new(GenerationType::Integer)),
        ColumnType::OidVector => GenerationType::Array(Box::new(GenerationType::Integer)),
        ColumnType::AnyArray => {
            GenerationType::Array(Box::new(GenerationType::UnknownLiteral("unknown".into())))
        }
        ColumnType::Record => GenerationType::Record,
        ColumnType::Array(element) => {
            GenerationType::Array(Box::new(column_generation_type(element)))
        }
        ColumnType::Date => GenerationType::Date,
        ColumnType::Time => GenerationType::Time,
        ColumnType::TimeTz => GenerationType::TimeTz,
        ColumnType::Timestamp => GenerationType::Timestamp,
        ColumnType::TimestampTz => GenerationType::TimestampTz,
        ColumnType::Interval => GenerationType::Interval,
        ColumnType::Range(subtype) => GenerationType::Range(*subtype),
        ColumnType::Multirange(subtype) => GenerationType::Multirange(*subtype),
        ColumnType::Vector(_) => GenerationType::Vector,
        ColumnType::Tensor(_) => GenerationType::Tensor,
        ColumnType::Domain { base, .. } => column_generation_type(base),
    }
}

pub(super) fn generation_type_assignable_to(source: &GenerationType, target: &ColumnType) -> bool {
    let target = column_generation_type(target);
    assignment_compatible(source, &target)
}

pub(super) fn generation_type_name(ty: &GenerationType) -> String {
    match ty {
        GenerationType::Null | GenerationType::UnknownLiteral(_) => "unknown".into(),
        GenerationType::Boolean => "boolean".into(),
        GenerationType::Void => "void".into(),
        GenerationType::SmallInteger => "smallint".into(),
        GenerationType::Integer => "integer".into(),
        GenerationType::BigInteger => "bigint".into(),
        GenerationType::Oid => "oid".into(),
        GenerationType::Xid => "xid".into(),
        GenerationType::Real => "double precision".into(),
        GenerationType::Numeric => "numeric".into(),
        GenerationType::Text => "text".into(),
        GenerationType::Uuid => "uuid".into(),
        GenerationType::Bytea => "bytea".into(),
        GenerationType::Json => "json".into(),
        GenerationType::JsonB => "jsonb".into(),
        GenerationType::Array(element) => format!("{}[]", generation_type_name(element)),
        GenerationType::Date => "date".into(),
        GenerationType::Time => "time without time zone".into(),
        GenerationType::TimeTz => "time with time zone".into(),
        GenerationType::Timestamp => "timestamp without time zone".into(),
        GenerationType::TimestampTz => "timestamp with time zone".into(),
        GenerationType::Interval => "interval".into(),
        GenerationType::Range(subtype) => subtype.range_name().into(),
        GenerationType::Multirange(subtype) => subtype.multirange_name().into(),
        GenerationType::Vector => "vector".into(),
        GenerationType::Tensor => "tensor".into(),
        GenerationType::Record => "record".into(),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves generated coercion diagnostics"
)]
fn infer_expression(
    engine: &Engine,
    columns: &[ColumnDef],
    expression: &Expr,
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<GenerationType, SQLError> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column_generation_type(&column.ty))
            .ok_or_else(|| SQLError::UnknownColumn(name.clone())),
        Expr::Literal(value) => Ok(value_generation_type(value)),
        Expr::Array(items) => {
            let mut element = GenerationType::Null;
            for item in items {
                let item = infer_expression(engine, columns, item, dependencies)?;
                element = common_type(&element, &item)?;
            }
            Ok(GenerationType::Array(Box::new(finalize_common_type(
                element,
            ))))
        }
        Expr::Row(items) => {
            for item in items {
                infer_expression(engine, columns, item, dependencies)?;
            }
            Ok(GenerationType::Record)
        }
        Expr::Binary { op, lhs, rhs } => {
            let lhs = infer_expression(engine, columns, lhs, dependencies)?;
            let rhs = infer_expression(engine, columns, rhs, dependencies)?;
            infer_binary_type(*op, &lhs, &rhs)
        }
        Expr::Not(inner) => {
            let ty = infer_expression(engine, columns, inner, dependencies)?;
            require_class("NOT", std::slice::from_ref(&ty), TypeClass::Boolean)?;
            Ok(GenerationType::Boolean)
        }
        Expr::UnaryMinus(inner) => {
            let ty = infer_expression(engine, columns, inner, dependencies)?;
            match ty {
                GenerationType::SmallInteger
                | GenerationType::Integer
                | GenerationType::BigInteger
                | GenerationType::Real
                | GenerationType::Numeric
                | GenerationType::Interval => Ok(ty),
                GenerationType::Oid | GenerationType::Xid => Ok(GenerationType::Integer),
                _ => Err(SQLError::TypeMismatch(format!(
                    "operator does not exist: - {}",
                    generation_type_name(&ty)
                ))),
            }
        }
        Expr::And(items) | Expr::Or(items) => {
            let types = items
                .iter()
                .map(|item| infer_expression(engine, columns, item, dependencies))
                .collect::<Result<Vec<_>, _>>()?;
            require_class("boolean expression", &types, TypeClass::Boolean)?;
            Ok(GenerationType::Boolean)
        }
        Expr::IsNull { expr, .. } => {
            infer_expression(engine, columns, expr, dependencies)?;
            Ok(GenerationType::Boolean)
        }
        Expr::Between { expr, low, high } => {
            let value = infer_expression(engine, columns, expr, dependencies)?;
            let low = infer_expression(engine, columns, low, dependencies)?;
            let high = infer_expression(engine, columns, high, dependencies)?;
            common_type(&value, &low)?;
            common_type(&value, &high)?;
            Ok(GenerationType::Boolean)
        }
        Expr::InList { expr, list, .. } => {
            let value = infer_expression(engine, columns, expr, dependencies)?;
            for item in list {
                let item = infer_expression(engine, columns, item, dependencies)?;
                common_type(&value, &item)?;
            }
            Ok(GenerationType::Boolean)
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base_type = base
                .as_deref()
                .map(|base| infer_expression(engine, columns, base, dependencies))
                .transpose()?;
            let mut result_type = GenerationType::Null;
            for (condition, result) in when {
                let condition = infer_expression(engine, columns, condition, dependencies)?;
                if let Some(base_type) = base_type.as_ref() {
                    common_type(base_type, &condition)?;
                } else {
                    require_class(
                        "CASE condition",
                        std::slice::from_ref(&condition),
                        TypeClass::Boolean,
                    )?;
                }
                let result = infer_expression(engine, columns, result, dependencies)?;
                result_type = common_type(&result_type, &result)?;
            }
            if let Some(else_branch) = else_branch {
                let else_type = infer_expression(engine, columns, else_branch, dependencies)?;
                result_type = common_type(&result_type, &else_type)?;
            }
            Ok(finalize_common_type(result_type))
        }
        Expr::Cast { expr, ty } => {
            infer_expression(engine, columns, expr, dependencies)?;
            generation_type_from_name(ty).ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "generation expression cast uses unsupported type `{ty}`"
                ))
            })
        }
        Expr::Func {
            name,
            binding,
            args,
            ..
        } => infer_function(engine, columns, name, binding.as_ref(), args, dependencies),
        Expr::Default
        | Expr::Param(_)
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::InternalColumn(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => Err(SQLError::TypeMismatch(
            "unsupported expression shape in generated-column type analysis".into(),
        )),
    }
}

fn infer_function(
    engine: &Engine,
    columns: &[ColumnDef],
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[Expr],
    dependencies: &mut Vec<GeneratedFunctionDependency>,
) -> Result<GenerationType, SQLError> {
    let call_arguments = generated_call_arguments(args)?;
    let mut argument_names = Vec::with_capacity(call_arguments.len());
    let mut argument_types = Vec::with_capacity(call_arguments.len());
    for argument in call_arguments {
        argument_names.push(argument.name);
        argument_types.push(infer_expression(
            engine,
            columns,
            argument.value,
            dependencies,
        )?);
    }

    if let Some(binding) = binding {
        if binding.builtin {
            if let Some(dispatch) = binding.dispatch {
                if let Some(return_type) = infer_dispatched_function(dispatch, &argument_types)? {
                    return Ok(return_type);
                }
            }
            if let Some(return_type) = uqa_execution::fixed_builtin_return_type(binding) {
                return Ok(column_generation_type(&return_type));
            }
            let dispatch_name = builtin_function_dispatch_name(&binding.name);
            return infer_builtin_function(&dispatch_name, &argument_names, &argument_types)?
                .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()));
        }
        let function = engine
            .lookup_bound_sql_functions(&binding.name)
            .and_then(|overloads| {
                overloads.into_iter().find(|function| {
                    routine_signature_types(&function.def) == binding.argument_types
                })
            })
            .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()))?;
        if let Some(type_name) = binding
            .invocation
            .as_deref()
            .and_then(|invocation| invocation.return_type.as_deref())
        {
            let return_type = crate::sql::resolve_catalog_column_type(engine, type_name)
                .as_ref()
                .map(column_generation_type)
                .or_else(|| generation_type_from_name(type_name))
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "generated-column function `{name}` returns unsupported type `{type_name}`"
                    ))
                })?;
            return Ok(return_type);
        }
        let return_type = match &function.def.returns {
            FunctionReturns::Scalar { type_name } => generation_type_from_name(type_name)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "generated-column function `{name}` returns unsupported type `{type_name}`"
                    ))
                })?,
            FunctionReturns::None => {
                let outputs = function.def.output_params();
                if outputs.len() > 1 {
                    GenerationType::Record
                } else {
                    let output = outputs.first().ok_or_else(|| {
                        SQLError::TypeMismatch(format!(
                            "generated-column function `{name}` does not return a value"
                        ))
                    })?;
                    generation_type_from_name(&output.type_name).ok_or_else(|| {
                        SQLError::TypeMismatch(format!(
                            "generated-column function `{name}` returns unsupported type `{}`",
                            output.type_name
                        ))
                    })?
                }
            }
            FunctionReturns::SetOf { .. } | FunctionReturns::Table => unreachable!(),
        };
        return Ok(return_type);
    }

    if engine
        .registered_runtime_function_volatility(name)
        .is_some()
    {
        return Err(SQLError::TypeMismatch(format!(
            "registered function `{name}` has no declared SQL return type and cannot be used in a column generation expression"
        )));
    }

    let dispatch_name = builtin_function_dispatch_name(&name.to_ascii_lowercase());
    infer_builtin_function(&dispatch_name, &argument_names, &argument_types)?
        .ok_or_else(|| SQLError::UnknownFunction(name.to_string()))
}

fn infer_dispatched_function(
    dispatch: FunctionDispatch,
    arguments: &[GenerationType],
) -> Result<Option<GenerationType>, SQLError> {
    let first = || {
        arguments.first().cloned().ok_or_else(|| {
            SQLError::TypeMismatch(format!("{} requires an argument", dispatch.label()))
        })
    };
    Ok(Some(match dispatch {
        FunctionDispatch::NamedArgument | FunctionDispatch::VariadicArgument => return Ok(None),
        FunctionDispatch::ArraySubscripts | FunctionDispatch::Subscript => match first()? {
            GenerationType::Array(element) => *element,
            GenerationType::Vector | GenerationType::Tensor => GenerationType::Real,
            GenerationType::Null | GenerationType::UnknownLiteral(_) => GenerationType::Null,
            other => {
                return Err(function_type_error(dispatch.label(), &other, "an array"));
            }
        },
        FunctionDispatch::ArraySlices
        | FunctionDispatch::Slice
        | FunctionDispatch::ArraySortJson => first()?,
        FunctionDispatch::AnyOperator
        | FunctionDispatch::AllOperator
        | FunctionDispatch::IsDistinct
        | FunctionDispatch::BetweenSymmetric => GenerationType::Boolean,
        FunctionDispatch::ToBinInt4
        | FunctionDispatch::ToBinInt8
        | FunctionDispatch::ToHexInt4
        | FunctionDispatch::ToHexInt8
        | FunctionDispatch::ToOctInt4
        | FunctionDispatch::ToOctInt8 => GenerationType::Text,
        FunctionDispatch::RandomInt4Range => GenerationType::Integer,
        FunctionDispatch::RandomInt8Range => GenerationType::BigInteger,
        FunctionDispatch::RandomNumericRange => GenerationType::Numeric,
        FunctionDispatch::Range {
            operation, subtype, ..
        } => match operation {
            RangeFunctionOperation::Lower | RangeFunctionOperation::Upper => {
                column_generation_type(&subtype.scalar_type())
            }
            RangeFunctionOperation::Merge => GenerationType::Range(subtype),
            RangeFunctionOperation::Multirange => GenerationType::Multirange(subtype),
            RangeFunctionOperation::IsEmpty
            | RangeFunctionOperation::LowerInclusive
            | RangeFunctionOperation::UpperInclusive
            | RangeFunctionOperation::LowerInfinite
            | RangeFunctionOperation::UpperInfinite
            | RangeFunctionOperation::Overlap
            | RangeFunctionOperation::Contains
            | RangeFunctionOperation::ContainedBy
            | RangeFunctionOperation::Adjacent => GenerationType::Boolean,
        },
    }))
}

#[derive(Debug)]
pub(super) struct GeneratedCallArgument<'a> {
    pub(super) name: Option<String>,
    pub(super) value: &'a Expr,
    pub(super) explicit_variadic: bool,
}

pub(super) fn generated_call_arguments(
    arguments: &[Expr],
) -> Result<Vec<GeneratedCallArgument<'_>>, SQLError> {
    let decoded = arguments
        .iter()
        .map(generated_call_argument)
        .collect::<Result<Vec<_>, _>>()?;
    let variadic_positions = decoded
        .iter()
        .enumerate()
        .filter_map(|(position, argument)| argument.explicit_variadic.then_some(position))
        .collect::<Vec<_>>();
    if variadic_positions.len() > 1 {
        return Err(malformed_generated_argument(
            "call contains more than one explicit VARIADIC argument",
        ));
    }
    if variadic_positions
        .first()
        .is_some_and(|position| *position + 1 != arguments.len())
    {
        return Err(malformed_generated_argument(
            "explicit VARIADIC argument must be the final call argument",
        ));
    }
    Ok(decoded)
}

fn generated_call_argument(expression: &Expr) -> Result<GeneratedCallArgument<'_>, SQLError> {
    let Expr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok(GeneratedCallArgument {
            name: None,
            value: expression,
            explicit_variadic: false,
        });
    };
    if binding.as_ref().and_then(|binding| binding.dispatch)
        == Some(uqa_sql::ast::FunctionDispatch::NamedArgument)
    {
        validate_generated_marker(
            binding.as_ref(),
            uqa_sql::ast::FunctionDispatch::NamedArgument,
            *distinct,
            order_by,
            filter.as_deref(),
            name,
        )?;
        let [Expr::Literal(Value::Str(argument_name)), value] = args.as_slice() else {
            return Err(malformed_generated_argument(
                "named argument marker must contain a string name and one value",
            ));
        };
        let (value, explicit_variadic) = generated_variadic_argument(value)?;
        if !explicit_variadic
            && matches!(
                value,
                Expr::Func { binding, .. }
                    if binding.as_ref().and_then(|binding| binding.dispatch)
                        == Some(uqa_sql::ast::FunctionDispatch::NamedArgument)
            )
        {
            return Err(malformed_generated_argument(
                "call argument contains nested syntax markers",
            ));
        }
        return Ok(GeneratedCallArgument {
            name: Some(argument_name.clone()),
            value,
            explicit_variadic,
        });
    }
    let (value, explicit_variadic) = generated_variadic_argument(expression)?;
    Ok(GeneratedCallArgument {
        name: None,
        value,
        explicit_variadic,
    })
}

fn generated_variadic_argument(expression: &Expr) -> Result<(&Expr, bool), SQLError> {
    let Expr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok((expression, false));
    };
    if binding.as_ref().and_then(|binding| binding.dispatch)
        != Some(uqa_sql::ast::FunctionDispatch::VariadicArgument)
    {
        return Ok((expression, false));
    }
    validate_generated_marker(
        binding.as_ref(),
        uqa_sql::ast::FunctionDispatch::VariadicArgument,
        *distinct,
        order_by,
        filter.as_deref(),
        name,
    )?;
    let [value] = args.as_slice() else {
        return Err(malformed_generated_argument(
            "VARIADIC argument marker must contain exactly one value",
        ));
    };
    if matches!(
        value,
        Expr::Func { binding, .. }
            if matches!(
                binding.as_ref().and_then(|binding| binding.dispatch),
                Some(
                    uqa_sql::ast::FunctionDispatch::VariadicArgument
                        | uqa_sql::ast::FunctionDispatch::NamedArgument
                )
            )
    ) {
        return Err(malformed_generated_argument(
            "call argument contains nested syntax markers",
        ));
    }
    Ok((value, true))
}

fn validate_generated_marker(
    binding: Option<&FunctionBinding>,
    expected_dispatch: uqa_sql::ast::FunctionDispatch,
    distinct: bool,
    order_by: &[uqa_sql::ast::OrderBy],
    filter: Option<&Expr>,
    name: &str,
) -> Result<(), SQLError> {
    if binding.is_none_or(|binding| {
        !binding.builtin
            || binding.dispatch != Some(expected_dispatch)
            || !binding.argument_types.is_empty()
            || binding.invocation.is_some()
            || binding.resolution_error.is_some()
    }) || distinct
        || !order_by.is_empty()
        || filter.is_some()
    {
        return Err(malformed_generated_argument(&format!(
            "{name} syntax marker contains function-call metadata"
        )));
    }
    Ok(())
}

fn malformed_generated_argument(message: &str) -> SQLError {
    SQLError::TypeMismatch(format!(
        "malformed generated-column call argument: {message}"
    ))
}

pub(super) fn generation_expression_column_type(
    columns: &[ColumnDef],
    expression: &Expr,
    inferred: &GenerationType,
) -> Option<ColumnType> {
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => columns
            .iter()
            .find(|column| column.name == *name)
            .map(|column| column.ty.clone()),
        Expr::Cast { ty, .. } => ColumnType::from_sql_name(ty).ok(),
        Expr::Literal(Value::Str(_) | Value::Null) => None,
        _ => ColumnType::from_sql_name(&generation_type_name(inferred)).ok(),
    }
}

pub(super) fn validate_bound_function(
    engine: &Engine,
    binding: &FunctionBinding,
    argument_names: &[Option<String>],
    argument_types: &[GenerationType],
) -> Result<FunctionBinding, SQLError> {
    let function = engine
        .lookup_bound_sql_functions(&binding.name)
        .and_then(|overloads| {
            overloads
                .into_iter()
                .find(|function| routine_signature_types(&function.def) == binding.argument_types)
        })
        .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()))?;
    if function.def.is_procedure || function.def.returns_set() {
        return Err(SQLError::TypeMismatch(format!(
            "generated-column function `{}` must return one scalar value",
            binding.name
        )));
    }
    if function.def.volatility != uqa_sql::ast::FunctionVolatility::Immutable {
        return Err(non_immutable_function(&binding.name));
    }
    let signature = function.def.signature_params();
    let mut positional = 0usize;
    for (argument_name, argument_type) in argument_names.iter().zip(argument_types) {
        let position = argument_name.as_ref().map_or_else(
            || {
                let position = positional;
                positional += 1;
                position
            },
            |argument_name| {
                signature
                    .iter()
                    .position(|parameter| parameter.name == *argument_name)
                    .unwrap_or(signature.len())
            },
        );
        let parameter = signature.get(position).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved generated-column function `{}` lost its argument mapping",
                binding.name
            ))
        })?;
        validate_unknown_literal_cast(argument_type, &parameter.type_name)?;
    }
    Ok(binding.clone())
}

mod builtin;
use builtin::infer_builtin_function;
mod type_rules;
use type_rules::{
    accepts_class, assignment_compatible, common_numeric_type, common_type, common_types,
    concat_result_type, finalize_common_type, function_type_error, generation_type_from_name,
    infer_binary_type, non_immutable_function, numeric_input_type, require_arity, require_class,
    require_one, require_signature, validate_unknown_literal_cast, value_generation_type,
};
