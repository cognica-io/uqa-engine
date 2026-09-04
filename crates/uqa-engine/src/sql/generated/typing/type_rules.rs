//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical_routine_type_name, generation_type_name, BinaryOp, GenerationType, RangeSubtype,
    SQLError, TypeClass, Value,
};

pub(super) fn infer_binary_type(
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

pub(super) fn temporal_plus_interval_type(
    temporal: &GenerationType,
) -> Result<GenerationType, SQLError> {
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

pub(super) fn value_generation_type(value: &Value) -> GenerationType {
    match value {
        Value::Null => GenerationType::Null,
        Value::Void => GenerationType::Void,
        Value::Bool(_) => GenerationType::Boolean,
        Value::Int(value) if i32::try_from(*value).is_ok() => GenerationType::Integer,
        Value::Int(_) => GenerationType::BigInteger,
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

pub(super) fn merge_array_generation_types(values: &[Value], element: &mut GenerationType) {
    for value in values {
        if let Value::List(nested) = value {
            merge_array_generation_types(nested, element);
        } else if let Ok(common) = common_type(element, &value_generation_type(value)) {
            *element = common;
        }
    }
}

pub(super) fn generation_type_from_name(name: &str) -> Option<GenerationType> {
    let canonical = canonical_routine_type_name(name);
    if let Some(element) = canonical.strip_suffix("[]") {
        return generation_type_from_name(element)
            .map(|element| GenerationType::Array(Box::new(element)));
    }
    Some(match canonical.as_str() {
        "bool" => GenerationType::Boolean,
        "int2" => GenerationType::SmallInteger,
        "int4" => GenerationType::Integer,
        "int8" => GenerationType::BigInteger,
        "oid" => GenerationType::Oid,
        "xid" => GenerationType::Xid,
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
        "int4range" => GenerationType::Range(RangeSubtype::Integer),
        "int8range" => GenerationType::Range(RangeSubtype::BigInteger),
        "numrange" => GenerationType::Range(RangeSubtype::Numeric),
        "daterange" => GenerationType::Range(RangeSubtype::Date),
        "tsrange" => GenerationType::Range(RangeSubtype::Timestamp),
        "tstzrange" => GenerationType::Range(RangeSubtype::TimestampTz),
        "int4multirange" => GenerationType::Multirange(RangeSubtype::Integer),
        "int8multirange" => GenerationType::Multirange(RangeSubtype::BigInteger),
        "nummultirange" => GenerationType::Multirange(RangeSubtype::Numeric),
        "datemultirange" => GenerationType::Multirange(RangeSubtype::Date),
        "tsmultirange" => GenerationType::Multirange(RangeSubtype::Timestamp),
        "tstzmultirange" => GenerationType::Multirange(RangeSubtype::TimestampTz),
        "vector" => GenerationType::Vector,
        "tensor" => GenerationType::Tensor,
        "record" => GenerationType::Record,
        _ => return None,
    })
}

pub(super) fn assignment_compatible(source: &GenerationType, target: &GenerationType) -> bool {
    use GenerationType as T;
    match (source, target) {
        (T::Null | T::UnknownLiteral(_), _) => true,
        (source, target) if source == target => true,
        (
            T::SmallInteger | T::Integer | T::BigInteger | T::Oid | T::Xid | T::Real | T::Numeric,
            T::SmallInteger | T::Integer | T::BigInteger | T::Oid | T::Xid | T::Real | T::Numeric,
        ) => true,
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

pub(super) fn common_types(types: &[GenerationType]) -> Result<GenerationType, SQLError> {
    types
        .iter()
        .try_fold(GenerationType::Null, |current, ty| {
            common_type(&current, ty)
        })
        .map(finalize_common_type)
}

pub(super) fn finalize_common_type(ty: GenerationType) -> GenerationType {
    match ty {
        GenerationType::Null | GenerationType::UnknownLiteral(_) => GenerationType::Text,
        other => other,
    }
}

pub(super) fn common_type(
    left: &GenerationType,
    right: &GenerationType,
) -> Result<GenerationType, SQLError> {
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

pub(super) fn common_numeric_type(types: &[GenerationType]) -> Result<GenerationType, SQLError> {
    require_class("numeric expression", types, TypeClass::Numeric)?;
    if types.iter().any(|ty| matches!(ty, GenerationType::Real)) {
        Ok(GenerationType::Real)
    } else if types.iter().any(|ty| matches!(ty, GenerationType::Numeric)) {
        Ok(GenerationType::Numeric)
    } else if types
        .iter()
        .any(|ty| matches!(ty, GenerationType::BigInteger))
    {
        Ok(GenerationType::BigInteger)
    } else if types.iter().any(|ty| {
        matches!(
            ty,
            GenerationType::Integer | GenerationType::Oid | GenerationType::Xid
        )
    }) {
        Ok(GenerationType::Integer)
    } else if types
        .iter()
        .any(|ty| matches!(ty, GenerationType::SmallInteger))
    {
        Ok(GenerationType::SmallInteger)
    } else {
        Ok(GenerationType::Real)
    }
}

pub(super) fn numeric_input_type(ty: &GenerationType) -> GenerationType {
    match ty {
        GenerationType::Real => GenerationType::Real,
        GenerationType::Numeric => GenerationType::Numeric,
        GenerationType::SmallInteger => GenerationType::SmallInteger,
        GenerationType::Integer => GenerationType::Integer,
        GenerationType::BigInteger => GenerationType::BigInteger,
        GenerationType::Oid | GenerationType::Xid => GenerationType::Integer,
        _ => GenerationType::Real,
    }
}

pub(super) fn concat_result_type(
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

pub(super) fn require_signature(
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

pub(super) fn require_arity(
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

pub(super) fn require_class(
    name: &str,
    args: &[GenerationType],
    expected: TypeClass,
) -> Result<(), SQLError> {
    for argument in args {
        require_one(name, argument, expected)?;
    }
    Ok(())
}

pub(super) fn require_one(
    name: &str,
    argument: &GenerationType,
    expected: TypeClass,
) -> Result<(), SQLError> {
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

pub(super) fn accepts_class(ty: &GenerationType, class: TypeClass) -> bool {
    if matches!(ty, GenerationType::Null | GenerationType::UnknownLiteral(_)) {
        return true;
    }
    match class {
        TypeClass::Boolean => matches!(ty, GenerationType::Boolean),
        TypeClass::Integer => matches!(
            ty,
            GenerationType::SmallInteger
                | GenerationType::Integer
                | GenerationType::BigInteger
                | GenerationType::Oid
                | GenerationType::Xid
        ),
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

pub(super) fn is_numeric(ty: &GenerationType) -> bool {
    matches!(
        ty,
        GenerationType::SmallInteger
            | GenerationType::Integer
            | GenerationType::BigInteger
            | GenerationType::Oid
            | GenerationType::Xid
            | GenerationType::Real
            | GenerationType::Numeric
    )
}

pub(super) fn is_temporal(ty: &GenerationType) -> bool {
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

pub(super) fn is_unknown(ty: &GenerationType) -> bool {
    matches!(ty, GenerationType::Null | GenerationType::UnknownLiteral(_))
}

pub(super) fn validate_unknown_literal_cast(
    actual: &GenerationType,
    declared_type: &str,
) -> Result<(), SQLError> {
    let GenerationType::UnknownLiteral(value) = actual else {
        return Ok(());
    };
    uqa_sql::expr::cast_value(&Value::Str(value.clone()), declared_type).map(|_| ())
}

pub(super) fn function_type_error(name: &str, actual: &GenerationType, expected: &str) -> SQLError {
    SQLError::TypeMismatch(format!(
        "function `{name}` argument has type {}, expected {expected}",
        generation_type_name(actual)
    ))
}

pub(super) fn non_immutable_function(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: format!(
            "generation expression function `{name}` is not immutable for these argument types"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::generated_call_arguments;
    use uqa_core::Value;
    use uqa_sql::ast::{Expr, FunctionBinding, FunctionDispatch};

    #[test]
    fn generated_call_arguments_decode_named_variadic_marker() {
        let arguments = vec![marker(
            FunctionDispatch::NamedArgument,
            vec![
                Expr::Literal(Value::Str("ITEMS".into())),
                marker(
                    FunctionDispatch::VariadicArgument,
                    vec![Expr::Literal(Value::Int(42))],
                ),
            ],
        )];

        let decoded = generated_call_arguments(&arguments).unwrap();
        assert_eq!(decoded[0].name.as_deref(), Some("ITEMS"));
        assert!(decoded[0].explicit_variadic);
        assert!(matches!(decoded[0].value, Expr::Literal(Value::Int(42))));
    }

    #[test]
    fn generated_call_arguments_reject_multiple_variadic_markers() {
        let arguments = vec![
            marker(
                FunctionDispatch::VariadicArgument,
                vec![Expr::Literal(Value::Int(1))],
            ),
            marker(
                FunctionDispatch::VariadicArgument,
                vec![Expr::Literal(Value::Int(2))],
            ),
        ];

        assert!(generated_call_arguments(&arguments)
            .unwrap_err()
            .to_string()
            .contains("more than one"));
    }

    fn marker(dispatch: FunctionDispatch, args: Vec<Expr>) -> Expr {
        let binding = FunctionBinding::dispatched(dispatch);
        Expr::Func {
            name: binding.name.clone(),
            binding: Some(binding),
            args,
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }
    }
}
