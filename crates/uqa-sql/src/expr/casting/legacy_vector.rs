//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy `int2vector` and `oidvector` casts.

use uqa_core::Value;

use crate::error::{Result, SQLError};

use super::{cast_integer, cast_oid};

#[derive(Clone, Copy)]
enum ElementType {
    SmallInteger,
    Oid,
}

pub(super) fn cast_int2vector(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    cast(value, ElementType::SmallInteger, source_ty)
}

pub(super) fn cast_oidvector(value: &Value, source_ty: Option<&str>) -> Result<Value> {
    cast(value, ElementType::Oid, source_ty)
}

fn cast(value: &Value, target: ElementType, source_ty: Option<&str>) -> Result<Value> {
    let values = match value {
        Value::List(values) => values.clone(),
        Value::Array(array) if array.dimensions().len() <= 1 => array.elements().to_vec(),
        Value::Array(_) => {
            return Err(SQLError::TypeMismatch(format!(
                "array is not a valid {}",
                type_name(target)
            )))
        }
        Value::Str(text) | Value::FixedChar(text) => text
            .split_whitespace()
            .map(|element| Value::Str(element.to_string()))
            .collect(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to {}",
                type_name(target)
            )))
        }
    };
    let source_element = source_element_type(source_ty);
    values
        .iter()
        .map(|value| match target {
            ElementType::SmallInteger => cast_integer(value, "smallint"),
            ElementType::Oid => {
                let source = if matches!(value, Value::Str(_) | Value::FixedChar(_)) {
                    Some("unknown")
                } else {
                    source_element.or(Some("oid"))
                };
                cast_oid(value, source)
            }
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::List)
}

fn type_name(target: ElementType) -> &'static str {
    match target {
        ElementType::SmallInteger => "int2vector",
        ElementType::Oid => "oidvector",
    }
}

fn source_element_type(source_ty: Option<&str>) -> Option<&str> {
    let source = source_ty?.trim();
    if let Some(element) = source.strip_suffix("[]") {
        return Some(element.trim());
    }
    match source.strip_prefix("pg_catalog.").unwrap_or(source) {
        "int2vector" => Some("smallint"),
        "oidvector" => Some("oid"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::ArrayValue;

    #[test]
    fn casts_preserve_postgresql_element_width_rules() {
        assert_eq!(
            cast_oidvector(&Value::Str("1 2 4294967295".into()), None).unwrap(),
            Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(i64::from(u32::MAX)),
            ])
        );
        let negative = Value::Array(ArrayValue::try_new(vec![Value::Int(-1)]).unwrap());
        assert_eq!(
            cast_oidvector(&negative, Some("integer[]")).unwrap(),
            Value::List(vec![Value::Int(i64::from(u32::MAX))])
        );
        let error = cast_oidvector(&negative, Some("bigint[]")).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
    }
}
