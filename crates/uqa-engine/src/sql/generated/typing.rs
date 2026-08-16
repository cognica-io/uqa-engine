//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static type and routine binding for generated-column expressions.

use crate::engine_user_functions::{canonical_routine_type_name, routine_signature_types};
use crate::sql::{builtin_function_dispatch_name, ColumnType, Engine, SQLError, Value};
use uqa_sql::ast::{
    BinaryOp, ColumnDef, Expr, FunctionBinding, FunctionReturns, GeneratedFunctionDependency,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GenerationType {
    Null,
    UnknownLiteral(String),
    Boolean,
    Integer,
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
            if name == uqa_sql::expr::NAMED_ARG_FUNCTION {
                return Ok(());
            }
            let mut argument_names = Vec::with_capacity(args.len());
            let mut argument_types = Vec::with_capacity(args.len());
            for argument in args {
                let (argument_name, value) = named_argument(argument)?;
                argument_names.push(argument_name);
                argument_types.push(infer_expression(engine, columns, value, dependencies)?);
            }
            if engine.lookup_sql_functions(name).is_some() {
                let selected =
                    resolve_user_function_binding(engine, name, &argument_names, &argument_types)?;
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
        | Expr::Literal(_)
        | Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => Ok(()),
    }
}

pub(super) fn column_generation_type(ty: &ColumnType) -> GenerationType {
    match ty {
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Oid
        | ColumnType::Xid => GenerationType::Integer,
        ColumnType::Boolean => GenerationType::Boolean,
        ColumnType::Text
        | ColumnType::Name
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::InternalChar
        | ColumnType::Regproc
        | ColumnType::Regclass
        | ColumnType::Regnamespace
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
        GenerationType::Integer => "integer".into(),
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
        GenerationType::Vector => "vector".into(),
        GenerationType::Tensor => "tensor".into(),
        GenerationType::Record => "record".into(),
    }
}

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
                GenerationType::Integer
                | GenerationType::Real
                | GenerationType::Numeric
                | GenerationType::Interval => Ok(ty),
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
    let mut argument_names = Vec::with_capacity(args.len());
    let mut argument_types = Vec::with_capacity(args.len());
    for argument in args {
        let (argument_name, value) = named_argument(argument)?;
        argument_names.push(argument_name);
        argument_types.push(infer_expression(engine, columns, value, dependencies)?);
    }

    if let Some(binding) = binding {
        let function = engine
            .lookup_sql_functions(&binding.name)
            .and_then(|overloads| {
                overloads.into_iter().find(|function| {
                    routine_signature_types(&function.def) == binding.argument_types
                })
            })
            .ok_or_else(|| SQLError::UnknownFunction(binding.name.clone()))?;
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
    infer_builtin_function(&dispatch_name, &argument_types)?
        .ok_or_else(|| SQLError::UnknownFunction(name.to_string()))
}

fn named_argument(expression: &Expr) -> Result<(Option<String>, &Expr), SQLError> {
    let Expr::Func { name, args, .. } = expression else {
        return Ok((None, expression));
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return Ok((None, expression));
    }
    let [Expr::Literal(Value::Str(name)), value] = args.as_slice() else {
        return Err(SQLError::TypeMismatch(
            "malformed named argument in generation expression".into(),
        ));
    };
    Ok((Some(name.to_ascii_lowercase()), value))
}

fn resolve_user_function_binding(
    engine: &Engine,
    name: &str,
    argument_names: &[Option<String>],
    argument_types: &[GenerationType],
) -> Result<FunctionBinding, SQLError> {
    let overloads = engine
        .lookup_sql_functions(name)
        .ok_or_else(|| SQLError::UnknownFunction(name.to_string()))?;
    let mut candidates = overloads
        .into_iter()
        .filter(|function| !function.def.is_procedure && !function.def.returns_set())
        .filter_map(|function| {
            user_function_match_cost(&function.def, argument_names, argument_types)
                .map(|matched| (function, matched))
        })
        .collect::<Vec<_>>();
    let Some(best_cost) = candidates.iter().map(|(_, matched)| matched.cost).min() else {
        return Err(SQLError::Routine {
            sqlstate: "42883".into(),
            message: format!(
                "function {name}({}) does not exist",
                argument_types
                    .iter()
                    .map(generation_type_name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    };
    candidates.retain(|(_, matched)| matched.cost == best_cost);
    for (argument_index, argument_type) in argument_types.iter().enumerate() {
        if !matches!(
            argument_type,
            GenerationType::Null | GenerationType::UnknownLiteral(_)
        ) {
            continue;
        }
        let mut categories = candidates
            .iter()
            .map(|(_, matched)| routine_type_category(&matched.argument_types[argument_index]))
            .collect::<Vec<_>>();
        categories.sort_unstable();
        categories.dedup();
        let selected_category = if categories.contains(&'S') {
            Some('S')
        } else if categories.len() == 1 {
            categories.first().copied()
        } else {
            None
        };
        if let Some(category) = selected_category {
            candidates.retain(|(_, matched)| {
                routine_type_category(&matched.argument_types[argument_index]) == category
            });
            if candidates.iter().any(|(_, matched)| {
                routine_type_is_preferred(&matched.argument_types[argument_index])
            }) {
                candidates.retain(|(_, matched)| {
                    routine_type_is_preferred(&matched.argument_types[argument_index])
                });
            }
        }
    }
    if candidates.len() != 1 {
        return Err(SQLError::Routine {
            sqlstate: "42725".into(),
            message: format!(
                "function {name}({}) is not unique",
                argument_types
                    .iter()
                    .map(generation_type_name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    let (function, matched) = candidates
        .pop()
        .ok_or_else(|| SQLError::Internal("bound function candidate disappeared".into()))?;
    for (actual, declared) in argument_types.iter().zip(&matched.argument_types) {
        validate_unknown_literal_cast(actual, declared)?;
    }
    if function.def.volatility != uqa_sql::ast::FunctionVolatility::Immutable {
        return Err(non_immutable_function(name));
    }
    Ok(FunctionBinding {
        name: function.def.name.clone(),
        argument_types: routine_signature_types(&function.def),
    })
}

struct UserFunctionMatch {
    cost: u32,
    argument_types: Vec<String>,
}

fn user_function_match_cost(
    function: &uqa_sql::ast::CreateFunction,
    argument_names: &[Option<String>],
    argument_types: &[GenerationType],
) -> Option<UserFunctionMatch> {
    let signature = function.signature_params();
    if argument_types.len() > signature.len() {
        return None;
    }
    let mut slots = vec![None; signature.len()];
    let mut matched_argument_types = Vec::with_capacity(argument_types.len());
    let mut positional = 0usize;
    let mut saw_named = false;
    for (argument_name, argument_type) in argument_names.iter().zip(argument_types) {
        let index = if let Some(argument_name) = argument_name {
            saw_named = true;
            signature
                .iter()
                .position(|parameter| parameter.name == *argument_name)?
        } else {
            if saw_named || positional >= signature.len() {
                return None;
            }
            let index = positional;
            positional += 1;
            index
        };
        if slots[index].replace(argument_type).is_some() {
            return None;
        }
        matched_argument_types.push(canonical_routine_type_name(&signature[index].type_name));
    }
    let mut cost = 0_u32;
    for (index, parameter) in signature.iter().enumerate() {
        let Some(argument_type) = slots[index] else {
            parameter.default.as_ref()?;
            continue;
        };
        cost = cost.checked_add(function_argument_cast_cost(
            argument_type,
            &parameter.type_name,
        )?)?;
    }
    Some(UserFunctionMatch {
        cost,
        argument_types: matched_argument_types,
    })
}

fn function_argument_cast_cost(actual: &GenerationType, declared: &str) -> Option<u32> {
    if matches!(
        actual,
        GenerationType::Null | GenerationType::UnknownLiteral(_)
    ) {
        return Some(1);
    }
    let declared = canonical_routine_type_name(declared);
    if generation_type_identity(actual).as_deref() == Some(declared.as_str()) {
        return Some(0);
    }
    if is_numeric(actual) && routine_type_category(&declared) == 'N' {
        return Some(1);
    }
    match actual {
        GenerationType::Text | GenerationType::Uuid
            if matches!(declared.as_str(), "text" | "varchar" | "bpchar") =>
        {
            Some(1)
        }
        GenerationType::Date if matches!(declared.as_str(), "timestamp" | "timestamptz") => Some(1),
        GenerationType::Time if declared == "timetz" => Some(1),
        GenerationType::Timestamp if declared == "timestamptz" => Some(1),
        GenerationType::Array(actual) => {
            let declared = declared.strip_suffix("[]")?;
            function_argument_cast_cost(actual, declared)
        }
        _ => None,
    }
}

fn generation_type_identity(ty: &GenerationType) -> Option<String> {
    Some(match ty {
        GenerationType::Null | GenerationType::UnknownLiteral(_) => return None,
        GenerationType::Boolean => "bool".into(),
        GenerationType::Integer => "int4".into(),
        GenerationType::Real => "float8".into(),
        GenerationType::Numeric => "numeric".into(),
        GenerationType::Text => "text".into(),
        GenerationType::Uuid => "uuid".into(),
        GenerationType::Bytea => "bytea".into(),
        GenerationType::Json => "json".into(),
        GenerationType::JsonB => "jsonb".into(),
        GenerationType::Array(element) => format!("{}[]", generation_type_identity(element)?),
        GenerationType::Date => "date".into(),
        GenerationType::Time => "time".into(),
        GenerationType::TimeTz => "timetz".into(),
        GenerationType::Timestamp => "timestamp".into(),
        GenerationType::TimestampTz => "timestamptz".into(),
        GenerationType::Interval => "interval".into(),
        GenerationType::Vector => "vector".into(),
        GenerationType::Tensor => "tensor".into(),
        GenerationType::Record => "record".into(),
    })
}

fn routine_type_category(type_name: &str) -> char {
    let canonical = canonical_routine_type_name(type_name);
    if canonical.ends_with("[]") {
        return 'A';
    }
    match canonical.as_str() {
        "bool" => 'B',
        "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" => 'N',
        "text" | "varchar" | "bpchar" | "name" => 'S',
        "date" | "time" | "timetz" | "timestamp" | "timestamptz" => 'D',
        "interval" => 'T',
        _ => 'U',
    }
}

fn routine_type_is_preferred(type_name: &str) -> bool {
    matches!(
        canonical_routine_type_name(type_name).as_str(),
        "bool" | "float8" | "text" | "timestamptz" | "interval"
    )
}

mod builtin;
use builtin::infer_builtin_function;
fn infer_binary_type(
    op: BinaryOp,
    lhs: &GenerationType,
    rhs: &GenerationType,
) -> Result<GenerationType, SQLError> {
    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        common_type(lhs, rhs)?;
        return Ok(GenerationType::Boolean);
    }
    let lhs_unknown = is_unknown(lhs);
    let rhs_unknown = is_unknown(rhs);
    if lhs_unknown && rhs_unknown {
        return Err(SQLError::TypeMismatch(format!(
            "operator {op:?} is not unique for two unknown operands"
        )));
    }
    if (is_numeric(lhs) && (is_numeric(rhs) || rhs_unknown)) || (is_numeric(rhs) && lhs_unknown) {
        if lhs_unknown {
            validate_unknown_literal_cast(lhs, &generation_type_name(rhs))?;
        }
        if rhs_unknown {
            validate_unknown_literal_cast(rhs, &generation_type_name(lhs))?;
        }
        return common_numeric_type(&[lhs.clone(), rhs.clone()]);
    }
    use GenerationType as T;
    match (lhs, rhs, op) {
        (T::JsonB, T::Text | T::Integer | T::Array(_), BinaryOp::Subtract) => Ok(T::JsonB),
        (T::Date, T::Integer, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Integer, T::Date, BinaryOp::Add) => Ok(T::Date),
        (T::Date, T::Date, BinaryOp::Subtract) => Ok(T::Integer),
        (T::Interval, T::Interval, BinaryOp::Add | BinaryOp::Subtract) => Ok(T::Interval),
        (T::Interval, other, BinaryOp::Add) | (other, T::Interval, BinaryOp::Add) => {
            temporal_plus_interval_type(other)
        }
        (other, T::Interval, BinaryOp::Subtract) => temporal_plus_interval_type(other),
        (T::Timestamp | T::TimestampTz | T::Time | T::TimeTz, other, BinaryOp::Subtract)
            if is_temporal(other) =>
        {
            Ok(T::Interval)
        }
        (T::Interval, numeric, BinaryOp::Multiply | BinaryOp::Divide) if is_numeric(numeric) => {
            Ok(T::Interval)
        }
        (numeric, T::Interval, BinaryOp::Multiply) if is_numeric(numeric) => Ok(T::Interval),
        _ => Err(SQLError::TypeMismatch(format!(
            "operator {op:?} does not exist for types {} and {}",
            generation_type_name(lhs),
            generation_type_name(rhs)
        ))),
    }
}

fn temporal_plus_interval_type(temporal: &GenerationType) -> Result<GenerationType, SQLError> {
    Ok(match temporal {
        GenerationType::Date => GenerationType::Timestamp,
        GenerationType::Time => GenerationType::Time,
        GenerationType::TimeTz => GenerationType::TimeTz,
        GenerationType::Timestamp => GenerationType::Timestamp,
        GenerationType::TimestampTz => GenerationType::TimestampTz,
        _ => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot apply interval arithmetic to {}",
                generation_type_name(temporal)
            )))
        }
    })
}

fn value_generation_type(value: &Value) -> GenerationType {
    match value {
        Value::Null => GenerationType::Null,
        Value::Bool(_) => GenerationType::Boolean,
        Value::Int(_) => GenerationType::Integer,
        Value::Float(_) => GenerationType::Real,
        Value::Decimal(_) => GenerationType::Numeric,
        Value::Str(value) => GenerationType::UnknownLiteral(value.clone()),
        Value::FixedChar(_) => GenerationType::Text,
        Value::Bytes(_) => GenerationType::Bytea,
        Value::Json(_) => GenerationType::Json,
        Value::JsonB(_) => GenerationType::JsonB,
        Value::Temporal(uqa_core::TemporalValue::Date { .. }) => GenerationType::Date,
        Value::Temporal(uqa_core::TemporalValue::Time { .. }) => GenerationType::Time,
        Value::Temporal(uqa_core::TemporalValue::TimeTz { .. }) => GenerationType::TimeTz,
        Value::Temporal(uqa_core::TemporalValue::Timestamp { .. }) => GenerationType::Timestamp,
        Value::Temporal(uqa_core::TemporalValue::TimestampTz { .. }) => GenerationType::TimestampTz,
        Value::Temporal(uqa_core::TemporalValue::Interval { .. }) => GenerationType::Interval,
        Value::Array(array) => {
            let mut element = GenerationType::Null;
            merge_array_generation_types(array.elements(), &mut element);
            GenerationType::Array(Box::new(element))
        }
        Value::List(values) => {
            let element = values.iter().fold(GenerationType::Null, |current, value| {
                common_type(&current, &value_generation_type(value)).unwrap_or(current)
            });
            GenerationType::Array(Box::new(element))
        }
        Value::Row(_) | Value::Record(_) => GenerationType::Record,
        Value::Map(_) => GenerationType::JsonB,
    }
}

fn merge_array_generation_types(values: &[Value], element: &mut GenerationType) {
    for value in values {
        if let Value::List(nested) = value {
            merge_array_generation_types(nested, element);
        } else if let Ok(common) = common_type(element, &value_generation_type(value)) {
            *element = common;
        }
    }
}

fn generation_type_from_name(name: &str) -> Option<GenerationType> {
    let canonical = canonical_routine_type_name(name);
    if let Some(element) = canonical.strip_suffix("[]") {
        return generation_type_from_name(element)
            .map(|element| GenerationType::Array(Box::new(element)));
    }
    Some(match canonical.as_str() {
        "bool" => GenerationType::Boolean,
        "int2" | "int4" | "int8" => GenerationType::Integer,
        "float4" | "float8" => GenerationType::Real,
        "numeric" => GenerationType::Numeric,
        "text" | "varchar" | "bpchar" => GenerationType::Text,
        "uuid" => GenerationType::Uuid,
        "bytea" => GenerationType::Bytea,
        "json" => GenerationType::Json,
        "jsonb" => GenerationType::JsonB,
        "date" => GenerationType::Date,
        "time" => GenerationType::Time,
        "timetz" => GenerationType::TimeTz,
        "timestamp" => GenerationType::Timestamp,
        "timestamptz" => GenerationType::TimestampTz,
        "interval" => GenerationType::Interval,
        "vector" => GenerationType::Vector,
        "tensor" => GenerationType::Tensor,
        "record" => GenerationType::Record,
        _ => return None,
    })
}

fn assignment_compatible(source: &GenerationType, target: &GenerationType) -> bool {
    use GenerationType as T;
    match (source, target) {
        (T::Null | T::UnknownLiteral(_), _) => true,
        (source, target) if source == target => true,
        (T::Integer | T::Real | T::Numeric, T::Integer | T::Real | T::Numeric) => true,
        (_, T::Text) => true,
        (T::Date, T::Timestamp | T::TimestampTz)
        | (T::Timestamp, T::Date | T::Time | T::TimestampTz)
        | (T::TimestampTz, T::Date | T::Time | T::TimeTz | T::Timestamp)
        | (T::Time, T::TimeTz | T::Interval)
        | (T::TimeTz | T::Interval, T::Time) => true,
        (T::Array(source), T::Array(target)) => assignment_compatible(source, target),
        (T::Array(element), T::Vector) => is_numeric(element),
        (T::Array(element), T::Tensor) => {
            matches!(element.as_ref(), T::Array(inner) if is_numeric(inner))
        }
        (T::Vector, T::Array(element)) => is_numeric(element),
        (T::Tensor, T::Array(element)) => {
            matches!(element.as_ref(), T::Array(inner) if is_numeric(inner))
        }
        _ => false,
    }
}

fn common_types(types: &[GenerationType]) -> Result<GenerationType, SQLError> {
    types
        .iter()
        .try_fold(GenerationType::Null, |current, ty| {
            common_type(&current, ty)
        })
        .map(finalize_common_type)
}

fn finalize_common_type(ty: GenerationType) -> GenerationType {
    match ty {
        GenerationType::Null | GenerationType::UnknownLiteral(_) => GenerationType::Text,
        other => other,
    }
}

fn common_type(left: &GenerationType, right: &GenerationType) -> Result<GenerationType, SQLError> {
    use GenerationType as T;
    match (left, right) {
        (T::Null | T::UnknownLiteral(_), other) | (other, T::Null | T::UnknownLiteral(_)) => {
            Ok(other.clone())
        }
        (left, right) if left == right => Ok(left.clone()),
        (left, right) if is_numeric(left) && is_numeric(right) => {
            common_numeric_type(&[left.clone(), right.clone()])
        }
        (T::Array(left), T::Array(right)) => Ok(T::Array(Box::new(common_type(left, right)?))),
        _ => Err(SQLError::TypeMismatch(format!(
            "generation expression cannot match types {} and {}",
            generation_type_name(left),
            generation_type_name(right)
        ))),
    }
}

fn common_numeric_type(types: &[GenerationType]) -> Result<GenerationType, SQLError> {
    require_class("numeric expression", types, TypeClass::Numeric)?;
    if types.iter().any(|ty| matches!(ty, GenerationType::Real)) {
        Ok(GenerationType::Real)
    } else if types.iter().any(|ty| matches!(ty, GenerationType::Numeric)) {
        Ok(GenerationType::Numeric)
    } else if types.iter().any(|ty| matches!(ty, GenerationType::Integer)) {
        Ok(GenerationType::Integer)
    } else {
        Ok(GenerationType::Real)
    }
}

fn numeric_input_type(ty: &GenerationType) -> GenerationType {
    match ty {
        GenerationType::Real => GenerationType::Real,
        GenerationType::Numeric => GenerationType::Numeric,
        GenerationType::Integer => GenerationType::Integer,
        _ => GenerationType::Real,
    }
}

fn concat_result_type(
    left: &GenerationType,
    right: &GenerationType,
) -> Result<GenerationType, SQLError> {
    use GenerationType as T;
    match (left, right) {
        (T::Array(_), T::Array(_)) => common_type(left, right),
        (T::Array(_), _) => Ok(left.clone()),
        (_, T::Array(_)) => Ok(right.clone()),
        (T::JsonB, T::JsonB) => Ok(T::JsonB),
        _ => Ok(T::Text),
    }
}

fn require_signature(
    name: &str,
    args: &[GenerationType],
    signature: &[TypeClass],
) -> Result<(), SQLError> {
    require_arity(name, args, signature.len(), signature.len())?;
    for (argument, expected) in args.iter().zip(signature) {
        require_one(name, argument, *expected)?;
    }
    Ok(())
}

fn require_arity(
    name: &str,
    args: &[GenerationType],
    minimum: usize,
    maximum: usize,
) -> Result<(), SQLError> {
    if (minimum..=maximum).contains(&args.len()) {
        return Ok(());
    }
    let expected = if minimum == maximum {
        minimum.to_string()
    } else if maximum == usize::MAX {
        format!("at least {minimum}")
    } else {
        format!("{minimum} to {maximum}")
    };
    Err(SQLError::TypeMismatch(format!(
        "function `{name}` expects {expected} arguments, got {}",
        args.len()
    )))
}

fn require_class(name: &str, args: &[GenerationType], expected: TypeClass) -> Result<(), SQLError> {
    for argument in args {
        require_one(name, argument, expected)?;
    }
    Ok(())
}

fn require_one(name: &str, argument: &GenerationType, expected: TypeClass) -> Result<(), SQLError> {
    if accepts_class(argument, expected) {
        Ok(())
    } else {
        Err(function_type_error(
            name,
            argument,
            match expected {
                TypeClass::Boolean => "boolean",
                TypeClass::Integer => "integer",
                TypeClass::Numeric => "numeric",
                TypeClass::Text => "text",
                TypeClass::Bytea => "bytea",
                TypeClass::Array => "array",
                TypeClass::Json => "json",
                TypeClass::JsonB => "jsonb",
                TypeClass::Temporal => "date/time/interval",
            },
        ))
    }
}

fn accepts_class(ty: &GenerationType, class: TypeClass) -> bool {
    if matches!(ty, GenerationType::Null | GenerationType::UnknownLiteral(_)) {
        return true;
    }
    match class {
        TypeClass::Boolean => matches!(ty, GenerationType::Boolean),
        TypeClass::Integer => matches!(ty, GenerationType::Integer),
        TypeClass::Numeric => is_numeric(ty),
        TypeClass::Text => matches!(ty, GenerationType::Text),
        TypeClass::Bytea => matches!(ty, GenerationType::Bytea),
        TypeClass::Array => matches!(
            ty,
            GenerationType::Array(_) | GenerationType::Vector | GenerationType::Tensor
        ),
        TypeClass::Json => matches!(ty, GenerationType::Json),
        TypeClass::JsonB => matches!(ty, GenerationType::JsonB),
        TypeClass::Temporal => is_temporal(ty),
    }
}

fn is_numeric(ty: &GenerationType) -> bool {
    matches!(
        ty,
        GenerationType::Integer | GenerationType::Real | GenerationType::Numeric
    )
}

fn is_temporal(ty: &GenerationType) -> bool {
    matches!(
        ty,
        GenerationType::Date
            | GenerationType::Time
            | GenerationType::TimeTz
            | GenerationType::Timestamp
            | GenerationType::TimestampTz
            | GenerationType::Interval
    )
}

fn is_unknown(ty: &GenerationType) -> bool {
    matches!(ty, GenerationType::Null | GenerationType::UnknownLiteral(_))
}

fn validate_unknown_literal_cast(
    actual: &GenerationType,
    declared_type: &str,
) -> Result<(), SQLError> {
    let GenerationType::UnknownLiteral(value) = actual else {
        return Ok(());
    };
    uqa_sql::expr::cast_value(&Value::Str(value.clone()), declared_type).map(|_| ())
}

fn function_type_error(name: &str, actual: &GenerationType, expected: &str) -> SQLError {
    SQLError::TypeMismatch(format!(
        "function `{name}` argument has type {}, expected {expected}",
        generation_type_name(actual)
    ))
}

fn non_immutable_function(name: &str) -> SQLError {
    SQLError::TypeMismatch(format!(
        "generation expression function `{name}` is not immutable for these argument types"
    ))
}
