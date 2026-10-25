//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed `PostgreSQL` text output for result consumers.

use crate::ast::ColumnType;
use crate::expr::{
    format_regtype_value_with_catalog_and_control, EngineHook, EngineValueCatalog, SQLValueCatalog,
};
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString},
    Value,
};

mod records;

/// Format a result with its live session output style and declared type.
pub fn format_postgres_text(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    format_postgres_text_with_control(value, ty, engine, &ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .map_err(|_| SQLError::Internal("ordinary typed output owner".into()))
}

/// Format through the live session while charging output and temporary payloads to the caller.
pub fn format_postgres_text_with_control(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let _interval_style = if crate::expr::interval_style::output_can_contain_interval(value) {
        crate::expr::interval_style::live_scope(engine, control)?
    } else {
        None
    };
    let catalog = engine.map(EngineValueCatalog);
    format_postgres_text_with_catalog_and_control(
        value,
        ty,
        catalog
            .as_ref()
            .map(|catalog| catalog as &dyn SQLValueCatalog),
        control,
    )
}

/// Format an admitted value through borrowed type and output metadata, without requiring a stateful scalar execution hook.
pub fn format_postgres_text_with_catalog(
    value: &Value,
    ty: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<String, SQLError> {
    format_postgres_text_with_catalog_and_control(
        value,
        ty,
        catalog,
        &ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary typed output owner".into()))
}

/// Observe typed values into the caller's admitted output, including nested array and composite fields.
pub fn format_postgres_text_with_catalog_and_control(
    value: &Value,
    ty: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    if let Value::Datum(datum) = value {
        let mut base = ty;
        while let ColumnType::Domain { base: inner, .. } = base {
            base = inner;
        }
        let value = if matches!(
            base,
            ColumnType::Range(_)
                | ColumnType::Multirange(_)
                | ColumnType::DeclaredRange(_)
                | ColumnType::DeclaredMultirange(_)
        ) {
            crate::expr::datums::read_range_with_type(datum, base, catalog, control)?
        } else {
            crate::expr::datums::read_with_value_catalog_and_control(datum, catalog, control)?
        };
        return format_postgres_text_with_catalog_and_control(&value, ty, catalog, control);
    }
    if let ColumnType::Domain { base, .. } = ty {
        return format_postgres_text_with_catalog_and_control(value, base, catalog, control);
    }
    if let (ColumnType::Xml, Value::Str(text)) = (ty, value) {
        return crate::xml::output_with_control(text, control);
    }
    if matches!(ty, ColumnType::InternalChar) {
        let byte = crate::expr::internal_char::value_byte(value)
            .ok_or_else(|| SQLError::Internal("invalid internal char result carrier".into()))?;
        return Ok(crate::expr::internal_char::output_with_control(
            byte, control,
        )?);
    }
    if let Some(text) = format_regtype_value_with_catalog_and_control(value, ty, catalog, control)?
    {
        return Ok(text);
    }
    if matches!(ty, ColumnType::Int2Vector | ColumnType::OidVector) {
        return crate::expr::vector_value_to_string_with_control(value, control)?
            .ok_or_else(|| SQLError::Internal("invalid catalog vector result carrier".into()));
    }
    if let ColumnType::Array(element) = ty {
        return format_array(value, element, catalog, control);
    }
    if let Value::Row(row) = value {
        return records::format_row(row, catalog, control);
    }
    if let Value::Record(fields) = value {
        let oid = fields.type_oid().or(match ty {
            ColumnType::Composite(reference) => Some(reference.oid),
            _ => None,
        });
        if let Some(oid) = oid {
            return format_record(fields, oid, catalog, control);
        }
    }
    Ok(match value {
        Value::Bool(value) => control.copy_text(if *value { "t" } else { "f" })?,
        Value::FixedChar(value) => control.copy_text(value)?,
        Value::Float(value) if matches!(ty, ColumnType::Real) => {
            crate::expr::format_real_with_control(*value as f32, control)?
        }
        Value::Float(value) => uqa_core::format_float_pg_with_control(*value, control)?,
        Value::Enum(label) => {
            crate::expr::enums::enum_label_text_with_control(catalog, label, control)?
        }
        _ if crate::expr::enums::contains_enum_carrier(value) => {
            // Untyped legacy containers retain their established conversion semantics.
            let rendered =
                crate::expr::enums::render_enum_labels_with_control(catalog, value, control)?;
            crate::expr::value_to_string_with_control(&rendered, control)?
        }
        _ => crate::expr::value_to_string_with_control(value, control)?,
    })
}

fn format_array(
    value: &Value,
    element: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut element = element;
    while let ColumnType::Array(inner) = element {
        control.check()?;
        element = inner;
    }
    let catalog_element;
    if let Value::Array(array) = value {
        if let Some(oid) = array.element_type_oid() {
            if let Some(actual) = crate::catalog::type_metadata::builtin_scalar_type(oid) {
                element = actual;
            } else if crate::catalog::type_metadata::pg_type_oid(element) != i64::from(oid) {
                catalog_element = catalog
                    .map(|catalog| catalog.value_type_by_oid(oid))
                    .transpose()?
                    .flatten()
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "XX000".into(),
                        message: format!("cache lookup failed for type {oid}"),
                    })?
                    .retain_external_with_control(control)?;
                element = &catalog_element;
            }
        }
    }
    let mut text = ProductionString::new(*control);
    let values = match value {
        Value::Array(array) => array_parts(array, &mut text, control)?,
        Value::LegacyVector(vector) => array_parts(vector.as_array(), &mut text, control)?,
        Value::List(values) => values.as_slice(),
        _ => return Err(SQLError::Internal("invalid array result carrier".into())),
    };
    text.push('{')?;
    for (index, value) in values.iter().enumerate() {
        control.check()?;
        if index != 0 {
            text.push(',')?;
        }
        match value {
            Value::Null => text.push_str("NULL")?,
            Value::List(_) => {
                text.push_str(&format_array(value, element, catalog, control)?)?;
            }
            _ => {
                let rendered = format_postgres_text_with_catalog_and_control(
                    value, element, catalog, control,
                )?;
                let quoted = rendered.is_empty()
                    || rendered.eq_ignore_ascii_case("null")
                    || rendered.chars().any(|c| {
                        (c.is_ascii() && c.is_whitespace())
                            || matches!(c, ',' | '{' | '}' | '"' | '\\')
                    });
                append_quoted(&mut text, &rendered, quoted, false)?;
            }
        }
    }
    text.push('}')?;
    Ok(text.finish()?)
}

fn format_record(
    fields: &uqa_core::RecordValue,
    type_oid: u32,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let descriptor = catalog
        .ok_or_else(|| SQLError::Internal("composite type catalog is unavailable".into()))?
        .value_composite_type(type_oid)?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {type_oid} does not exist"),
        })?;
    let mut text = ProductionString::new(*control);
    text.push('(')?;
    for (index, (name, field)) in fields.iter().enumerate() {
        control.check()?;
        if index != 0 {
            text.push(',')?;
        }
        if matches!(field, Value::Null) {
            continue;
        }
        let rendered = match descriptor.attribute(name) {
            Some((_, attribute)) => format_postgres_text_with_catalog_and_control(
                field,
                &attribute.ty,
                catalog,
                control,
            )?,
            None => format_postgres_text_with_catalog_and_control(
                field,
                &crate::type_resolution::value_type(field).unwrap_or(ColumnType::Text),
                catalog,
                control,
            )?,
        };
        let quoted = rendered.is_empty()
            || rendered
                .chars()
                .any(|c| c.is_ascii_whitespace() || matches!(c, ',' | '(' | ')' | '"' | '\\'));
        append_quoted(&mut text, &rendered, quoted, true)?;
    }
    text.push(')')?;
    Ok(text.finish()?)
}

fn append_quoted(
    text: &mut ProductionString<'_>,
    rendered: &str,
    quoted: bool,
    doubled: bool,
) -> Result<(), SQLError> {
    if quoted {
        text.push('"')?;
    }
    for character in rendered.chars() {
        if quoted && matches!(character, '"' | '\\') {
            text.push(if doubled { character } else { '\\' })?;
        }
        text.push(character)?;
    }
    if quoted {
        text.push('"')?;
    }
    Ok(())
}

fn array_parts<'a>(
    array: &'a uqa_core::ArrayValue,
    text: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<&'a [Value], SQLError> {
    if !array.elements().is_empty() && array.lower_bounds().iter().any(|lower| *lower != 1) {
        for (lower, length) in array.lower_bounds().iter().zip(array.dimensions()) {
            let upper = i64::from(*lower) + *length as i64 - 1;
            text.push_str(&control.format(format_args!("[{lower}:{upper}]"))?)?;
        }
        text.push('=')?;
    }
    Ok(array.elements())
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
            collation_oid: None,
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

        fn temporal_interval_style(
            &self,
        ) -> Result<Option<uqa_core::TemporalIntervalStyle>, SQLError> {
            Ok(Some(uqa_core::TemporalIntervalStyle::Iso8601))
        }
    }

    #[test]
    fn controlled_session_output_keeps_live_style_and_releases_its_allowance() {
        use uqa_core::{memory::MemoryBudget, CancellationToken, TemporalValue};
        let catalog = Catalog {
            lookups: Cell::new(0),
        };
        let memory = MemoryBudget::new(4096);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&memory, &cancellation, &cancellation);
        let value = Value::Temporal(TemporalValue::Interval {
            months: 0,
            days: 1,
            micros: 0,
        });
        let text = format_postgres_text_with_control(
            &value,
            &ColumnType::Interval,
            Some(&catalog),
            &control,
        )
        .unwrap();
        assert_eq!(&**text, "P1D");
        assert!(memory.used() > 0);
        drop(text);
        assert_eq!(memory.used(), 0);
        cancellation.cancel();
        let error = format_postgres_text_with_control(
            &value,
            &ColumnType::Interval,
            Some(&catalog),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
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

    fn typed_row(fields: Vec<(Value, u32)>) -> Value {
        use uqa_core::{RecordFieldType, RowValue};
        let (values, types) = fields
            .into_iter()
            .map(|(value, oid)| {
                (
                    value,
                    RecordFieldType {
                        oid,
                        type_modifier: -1,
                        collation_oid: None,
                    },
                )
            })
            .unzip();
        Value::Row(RowValue::typed(values, types).unwrap())
    }

    #[test]
    fn anonymous_record_output_keeps_field_types_and_skips_null_type_lookup() {
        let catalog = Catalog {
            lookups: Cell::new(0),
        };
        let value = typed_row(vec![
            (Value::Float(f64::from(0.1_f32)), 700),
            (Value::Float(f64::from(0.1_f32)), 16_500),
            (Value::Null, 65_000),
        ]);
        // PostgreSQL 18.4: ROW(0.1::real, 0.1::domain_over_real, NULL::integer)::text.
        assert_eq!(
            format_postgres_text(&value, &ColumnType::Record, Some(&catalog)).unwrap(),
            "(0.1,0.1,)"
        );
        assert_eq!(catalog.lookups.get(), 1);
        let error = format_postgres_text(&value, &ColumnType::Record, None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(error.to_string(), "cache lookup failed for type 16500");

        // PostgreSQL also outputs a retained NULL field after its enum type is dropped.
        let null = typed_row(vec![(Value::Null, 65_000)]);
        assert_eq!(
            format_postgres_text(&null, &ColumnType::Record, None).unwrap(),
            "()"
        );
    }

    #[test]
    fn anonymous_record_output_bounds_temporary_memory_and_observes_both_cancellations() {
        use uqa_core::{memory::MemoryBudget, CancellationToken};
        let payload = "a".repeat(8192);
        let value = typed_row(vec![
            (Value::Float(f64::from(0.1_f32)), 700),
            (Value::Str(payload.clone()), 25),
        ]);
        for limit in [0, 64, 1024, 65_536] {
            let memory = MemoryBudget::new(limit);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let output =
                format_postgres_text_with_control(&value, &ColumnType::Record, None, &control);
            if limit == 65_536 {
                let text = output.unwrap();
                assert_eq!(&**text, format!("(0.1,{payload})"));
                assert!(memory.used() >= text.len());
            } else {
                assert_eq!(output.unwrap_err().sqlstate(), Some("53200"));
            }
            assert_eq!(memory.used(), 0);
            for token in [&original, &invoking] {
                token.cancel();
                let error =
                    format_postgres_text_with_control(&value, &ColumnType::Record, None, &control)
                        .unwrap_err();
                assert_eq!(error.sqlstate(), Some("57014"));
                token.reset();
                assert_eq!(memory.used(), 0);
            }
        }
    }
}
