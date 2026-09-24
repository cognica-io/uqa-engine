//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy `int2vector` and `oidvector` casts.

use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    LegacyVectorKind, LegacyVectorValue, Value,
};

use crate::error::{Result, SQLError};

use super::{cast_integer, cast_oid};

use LegacyVectorKind as ElementType;

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
        Value::LegacyVector(vector) if vector.kind() == target => {
            return Ok(control.copy_value(value)?);
        }
        Value::LegacyVector(vector) => {
            append(&mut output, vector.elements(), target, source_ty, control)?;
        }
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
    let vector = LegacyVectorValue::try_new_with_control(target, output.finish()?, control)?
        .ok_or_else(|| SQLError::TypeMismatch(format!("invalid {} elements", type_name(target))))?;
    let (vector, memory) = vector.into_parts();
    Ok(control.finish(Value::LegacyVector(vector), memory)?)
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
    fn casts_and_assignment_match_postgresql_vector_comparisons() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../uqa-core/src/types/tests/pg18_legacy_vectors.json"
        )))
        .unwrap();
        for group in fixture["types"].as_array().unwrap() {
            let name = group["type"].as_str().unwrap();
            let ty = crate::ColumnType::from_sql_name(name).unwrap();
            let values: Vec<_> = group["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|text| {
                    let source = Value::Str(text.as_str().unwrap().into());
                    let cast = super::super::cast_value_from(&source, name, None).unwrap();
                    let assigned =
                        crate::assignment::conversion::convert_value_to_column_type(source, &ty)
                            .unwrap();
                    assert_eq!(cast, assigned);
                    assert_eq!(cast.array_view().unwrap().lower_bounds(), &[0]);
                    cast
                })
                .collect();
            for pair in group["comparisons"].as_array().unwrap() {
                let left = &values[pair[0].as_u64().unwrap() as usize];
                let right = &values[pair[1].as_u64().unwrap() as usize];
                let actual = crate::expr::compare_with_control(
                    left,
                    right,
                    &ProductionControl::uncontrolled(),
                )
                .unwrap();
                assert_eq!(actual.is_eq(), pair[2] == true);
                assert_eq!(actual.is_lt(), pair[3] == true);
                assert_eq!(actual.is_gt(), pair[4] == true);
            }
        }
    }

    #[test]
    fn vector_array_casts_preserve_binary_bounds_and_convert_oid_bits() {
        for (name, target, dimensions) in [
            ("int2vector", "smallint[]", vec![0]),
            ("int2vector", "integer[]", vec![]),
            ("oidvector", "oid[]", vec![0]),
            ("oidvector", "integer[]", vec![0]),
        ] {
            let empty =
                super::super::cast_value_from(&Value::Str(String::new()), name, None).unwrap();
            let cast = super::super::cast_value_from(&empty, target, Some(name)).unwrap();
            assert_eq!(cast.array_view().unwrap().dimensions(), dimensions);
            assert_eq!(crate::expr::value_to_string(&cast).unwrap(), "{}");
        }
        let oid = super::super::cast_value_from(
            &Value::Str("2147483648 4294967295".into()),
            "oidvector",
            None,
        )
        .unwrap();
        let converted =
            super::super::cast_value_from(&oid, "integer[]", Some("oidvector")).unwrap();
        assert_eq!(
            converted.array_view().unwrap().elements(),
            &[Value::Int(i64::from(i32::MIN)), Value::Int(-1)]
        );
        assert_eq!(converted.array_view().unwrap().lower_bounds(), &[0]);
        let array = super::super::cast_value_from(
            &Value::Str("{\"\",\"1 2\"}".into()),
            "int2vector[]",
            None,
        )
        .unwrap();
        assert_eq!(array.array_view().unwrap().dimensions(), &[2]);
        assert!(array
            .array_view()
            .unwrap()
            .elements()
            .iter()
            .all(|value| matches!(value, Value::LegacyVector(_))));
    }

    #[test]
    fn casts_preserve_postgresql_element_width_rules() {
        assert_eq!(
            super::super::cast_value_from(&Value::Str("1 2 4294967295".into()), "oidvector", None)
                .unwrap(),
            Value::LegacyVector(
                LegacyVectorValue::try_new(
                    ElementType::Oid,
                    vec![
                        Value::Int(1),
                        Value::Int(2),
                        Value::Int(i64::from(u32::MAX)),
                    ]
                )
                .unwrap()
            )
        );
        let negative = Value::Array(ArrayValue::try_new(vec![Value::Int(-1)]).unwrap());
        assert_eq!(
            super::super::cast_value_from(&negative, "oidvector", Some("integer[]")).unwrap(),
            Value::LegacyVector(
                LegacyVectorValue::try_new(ElementType::Oid, vec![Value::Int(i64::from(u32::MAX))])
                    .unwrap()
            )
        );
        let error =
            super::super::cast_value_from(&negative, "oidvector", Some("bigint[]")).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22003"));
    }
}
