//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy `int2vector` and `oidvector` casts.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

use crate::error::{Result, SQLError};

use super::{cast_integer, cast_oid};

#[derive(Clone, Copy)]
enum ElementType {
    SmallInteger,
    Oid,
}

pub(super) fn cast_int2vector(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    cast(value, ElementType::SmallInteger, source_ty, control)
}

pub(super) fn cast_oidvector(
    value: &Value,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    cast(value, ElementType::Oid, source_ty, control)
}

fn cast(
    value: &Value,
    target: ElementType,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let mut output = ProductionVec::new(*control);
    match value {
        Value::List(values) => append(&mut output, values, target, source_ty, control)?,
        Value::Array(array) if array.dimensions().len() <= 1 => {
            append(&mut output, array.elements(), target, source_ty, control)?;
        }
        Value::Array(_) => {
            return Err(SQLError::TypeMismatch(format!(
                "array is not a valid {}",
                type_name(target)
            )))
        }
        Value::Str(text) | Value::FixedChar(text) => {
            for text in text.split_whitespace() {
                let (text, memory) = control.copy_text(text)?.into_parts();
                let text = control.finish(Value::Str(text), memory)?;
                append(
                    &mut output,
                    std::slice::from_ref(&*text),
                    target,
                    source_ty,
                    control,
                )?;
            }
        }
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "cannot cast {other:?} to {}",
                type_name(target)
            )))
        }
    }
    let (values, memory) = output.finish()?.into_parts();
    Ok(control.finish(Value::List(values), memory)?)
}

fn append(
    output: &mut ProductionVec<'_, Value>,
    values: &[Value],
    target: ElementType,
    source_ty: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    let source_element = source_element_type(source_ty);
    for value in values {
        let value = match target {
            ElementType::SmallInteger => cast_integer(value, "smallint", control)?,
            ElementType::Oid => {
                let source = if matches!(value, Value::Str(_) | Value::FixedChar(_)) {
                    Some("unknown")
                } else {
                    source_element.or(Some("oid"))
                };
                cast_oid(value, source, control)?
            }
        };
        output.push_produced(control.finish(value, control.empty_reservation())?)?;
    }
    Ok(())
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
            super::super::cast_value_from(&Value::Str("1 2 4294967295".into()), "oidvector", None)
                .unwrap(),
            Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(i64::from(u32::MAX)),
            ])
        );
        let negative = Value::Array(ArrayValue::try_new(vec![Value::Int(-1)]).unwrap());
        assert_eq!(
            super::super::cast_value_from(&negative, "oidvector", Some("integer[]")).unwrap(),
            Value::List(vec![Value::Int(i64::from(u32::MAX))])
        );
        let error =
            super::super::cast_value_from(&negative, "oidvector", Some("bigint[]")).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
    }
}
