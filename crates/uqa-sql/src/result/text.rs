//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed `PostgreSQL` text output for result consumers.

use std::fmt::Write;

use crate::ast::ColumnType;
use crate::expr::{format_regtype_value, value_to_string, vector_value_to_string, EngineHook};
use crate::SQLError;
use uqa_core::Value;

/// Format a non-NULL result value using its declared type and optional catalog resolver. NULL remains separate from text in the calling result protocol.
pub fn format_postgres_text(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    if let Value::Datum(datum) = value {
        return format_postgres_text(
            &crate::expr::datums::read_with_catalog(datum, engine)?,
            ty,
            engine,
        );
    }
    if let ColumnType::Domain { base, .. } = ty {
        return format_postgres_text(value, base, engine);
    }
    if let Some(text) = format_regtype_value(value, ty, engine)? {
        return Ok(text);
    }
    if matches!(ty, ColumnType::Int2Vector | ColumnType::OidVector) {
        return vector_value_to_string(value)?
            .ok_or_else(|| SQLError::Internal("invalid catalog vector result carrier".into()));
    }
    if let ColumnType::Array(element) = ty {
        return format_array(value, element, engine);
    }
    if let (ColumnType::Composite(reference), Value::Record(fields)) = (ty, value) {
        return format_record(fields, reference.oid, engine);
    }
    Ok(match value {
        Value::Bool(value) => if *value { "t" } else { "f" }.into(),
        Value::FixedChar(value) => value.clone(),
        Value::Float(value) if matches!(ty, ColumnType::Real) => {
            crate::expr::format_real(*value as f32)
        }
        Value::Float(value) => uqa_core::format_float_pg(*value),
        Value::Enum(label) => {
            crate::expr::enums::enum_label_text(engine.and_then(EngineHook::enum_labels), label)?
        }
        // Container output calls each enum field's output function, which reads the current label.
        _ if crate::expr::enums::contains_enum_carrier(value) => {
            value_to_string(&crate::expr::enums::render_enum_labels(
                engine.and_then(EngineHook::enum_labels),
                value,
            )?)?
        }
        _ => value_to_string(value)?,
    })
}

fn format_array(
    value: &Value,
    element: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    // SQL array type identity does not constrain value dimensions. Nested
    // declarations still name the scalar element formatter at every depth.
    let mut element = element;
    while let ColumnType::Array(inner) = element {
        element = inner;
    }
    let catalog_element;
    if let Value::Array(array) = value {
        if let Some(oid) = array.element_type_oid() {
            if let Some(actual) = crate::catalog::type_metadata::builtin_scalar_type(oid) {
                element = actual;
            } else if crate::catalog::type_metadata::pg_type_oid(element) != i64::from(oid) {
                catalog_element = engine
                    .map(|engine| engine.resolve_type_oid(oid))
                    .transpose()
                    .map_err(SQLError::Internal)?
                    .flatten()
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "XX000".into(),
                        message: format!("cache lookup failed for type {oid}"),
                    })?;
                element = &catalog_element;
            }
        }
    }
    let (values, prefix) = match value {
        Value::Array(array) => array_parts(array),
        Value::LegacyVector(vector) => array_parts(vector.as_array()),
        Value::List(values) => (values.as_slice(), String::new()),
        _ => return Err(SQLError::Internal("invalid array result carrier".into())),
    };
    let mut fields = Vec::with_capacity(values.len());
    for value in values {
        fields.push(match value {
            Value::Null => "NULL".into(),
            Value::List(_) | Value::Array(_) => format_array(value, element, engine)?,
            _ => {
                let text = format_postgres_text(value, element, engine)?;
                if text.is_empty()
                    || text.eq_ignore_ascii_case("null")
                    || text.chars().any(|c| {
                        c.is_ascii_whitespace() || matches!(c, ',' | '{' | '}' | '"' | '\\')
                    })
                {
                    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
                } else {
                    text
                }
            }
        });
    }
    Ok(format!("{prefix}{{{}}}", fields.join(",")))
}

/// `record_out`: each field through its attribute type's output function, an empty field for NULL, and double quotes around a field that is empty or holds a separator, parenthesis, quote, backslash or whitespace, doubling quotes and backslashes inside.
fn format_record(
    fields: &[(String, Value)],
    type_oid: u32,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    let descriptor = crate::expr::composites::descriptor(
        engine.and_then(EngineHook::composite_types),
        type_oid,
    )?;
    let mut text = String::from("(");
    for (index, (name, field)) in fields.iter().enumerate() {
        if index != 0 {
            text.push(',');
        }
        if matches!(field, Value::Null) {
            continue;
        }
        let rendered = match descriptor.attribute(name) {
            Some((_, attribute)) => format_postgres_text(field, &attribute.ty, engine)?,
            None => format_postgres_text(
                field,
                &crate::type_resolution::value_type(field).unwrap_or(ColumnType::Text),
                engine,
            )?,
        };
        let quoted = rendered.is_empty()
            || rendered.chars().any(|character| {
                character.is_ascii_whitespace() || matches!(character, ',' | '(' | ')' | '"' | '\\')
            });
        if quoted {
            text.push('"');
            for character in rendered.chars() {
                if matches!(character, '"' | '\\') {
                    text.push(character);
                }
                text.push(character);
            }
            text.push('"');
        } else {
            text.push_str(&rendered);
        }
    }
    text.push(')');
    Ok(text)
}

fn array_parts(array: &uqa_core::ArrayValue) -> (&[Value], String) {
    let mut prefix = String::new();
    if !array.elements().is_empty() && array.lower_bounds().iter().any(|lower| *lower != 1) {
        for (lower, length) in array.lower_bounds().iter().zip(array.dimensions()) {
            let upper = i64::from(*lower) + *length as i64 - 1;
            write!(prefix, "[{lower}:{upper}]").expect("writing to String cannot fail");
        }
        prefix.push('=');
    }
    (array.elements(), prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use uqa_core::ArrayValue;

    struct Catalog {
        lookups: Cell<usize>,
    }

    fn domain() -> ColumnType {
        ColumnType::Domain {
            schema: "public".into(),
            name: "real_value".into(),
            oid: 16_500,
            array_oid: Some(16_501),
            base: Box::new(ColumnType::Real),
        }
    }

    impl EngineHook for Catalog {
        fn nextval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }

        fn currval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }

        fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
            unreachable!()
        }

        fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
            self.lookups.set(self.lookups.get() + 1);
            Ok((oid == 16_500).then(domain))
        }
    }

    #[test]
    fn array_output_uses_original_catalog_element_and_reports_missing_identity() {
        let catalog = Catalog {
            lookups: Cell::new(0),
        };
        let value = Value::Array(
            ArrayValue::try_new(vec![Value::Float(f64::from(0.1_f32)), Value::Null])
                .unwrap()
                .with_element_type_oid(Some(16_500)),
        );
        let changed = ColumnType::Array(Box::new(ColumnType::Text));
        assert_eq!(
            format_postgres_text(&value, &changed, Some(&catalog)).unwrap(),
            "{0.1,NULL}"
        );
        assert_eq!(catalog.lookups.get(), 1);
        assert_eq!(
            format_postgres_text(
                &value,
                &ColumnType::Array(Box::new(domain())),
                Some(&catalog),
            )
            .unwrap(),
            "{0.1,NULL}"
        );
        assert_eq!(catalog.lookups.get(), 1);
        let error = format_postgres_text(&value, &changed, None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(error.to_string(), "cache lookup failed for type 16500");
    }
}
