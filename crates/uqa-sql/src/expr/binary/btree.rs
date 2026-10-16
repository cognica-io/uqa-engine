//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected B-tree inputs share admitted carriers without repeating input conversion.

use crate::{
    catalog::{index::operator_classes, type_metadata},
    expr::{composites, datums, SQLValueCatalog},
    ColumnType, SQLError,
};
use uqa_core::{memory::ProductionControl, ArrayValue, Value};

/// A borrowed input in the selected class's equality domain. Arrays retain their shape even when the original carrier is an atomic catalog vector. Image records keep a distinct observation contract.
pub enum BTreeInput<'a> {
    Value(&'a Value, &'a ColumnType),
    Array(&'a ArrayValue),
    RecordImage(&'a Value),
}

/// Observe one already admitted bound under its selected class. The callback cannot outlive physical decoding or a required carrier projection; unchanged values and vector arrays are borrowed. Collation and support invocation remain the consuming operation's responsibility.
pub fn with_btree_input<R>(
    binding: operator_classes::BoundBTreeOperatorClass<'_>,
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    observe: impl FnOnce(BTreeInput<'_>) -> Result<R, SQLError>,
) -> Result<R, SQLError> {
    control.check()?;
    let observe = |input: BTreeInput<'_>| {
        let result = observe(input)?;
        control.check()?;
        Ok(result)
    };
    let class = binding.class();
    if operator_classes::builtin_btree_operator_class(class.oid) != Some(class) {
        return Err(invalid());
    }
    let mut source = binding.declared_type();
    while let ColumnType::Domain { base, .. } = source {
        control.check()?;
        source = base;
    }
    if matches!(value, Value::Null) {
        return observe(BTreeInput::Value(value, source));
    }
    if class.comparison_oid == 3187 {
        return observe(BTreeInput::RecordImage(value));
    }
    if type_metadata::routine_pseudo_type_name(i64::from(class.input_type_oid)) == Some("anyarray")
    {
        return with_array(value, catalog, control, observe);
    }
    let Some(target) = type_metadata::builtin_scalar_type(class.input_type_oid) else {
        return observe(BTreeInput::Value(value, source));
    };
    if matches!(target, ColumnType::Record) {
        return observe(BTreeInput::Value(value, source));
    }
    if let Value::Datum(datum) = value {
        let selected = Value::Datum(datum.field(class.input_type_oid, datum.offset()));
        return observe(BTreeInput::Value(&selected, target));
    }
    if matches!(target, ColumnType::Bpchar) {
        if let Value::Str(text) = value {
            let (text, memory) = control.copy_text(text)?.into_parts();
            let selected = control.finish(Value::FixedChar(text), memory)?;
            return observe(BTreeInput::Value(&selected, target));
        }
    }
    if type_metadata::pg_type_by_value(target) {
        if let Some(selected) = composites::datum::reinterpret(value, source, target) {
            return observe(BTreeInput::Value(&selected, target));
        }
    }
    observe(BTreeInput::Value(value, target))
}

fn with_array<R>(
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    observe: impl FnOnce(BTreeInput<'_>) -> Result<R, SQLError>,
) -> Result<R, SQLError> {
    let decoded = if let Value::Datum(datum) = value {
        Some(datums::read_array_input_with_catalog_and_control(
            datum, catalog, control,
        )?)
    } else {
        None
    };
    let array = match decoded.as_deref().unwrap_or(value) {
        Value::Array(array) => array,
        Value::LegacyVector(vector) => vector.as_array(),
        _ => return Err(invalid()),
    };
    observe(BTreeInput::Array(array))
}

fn invalid() -> SQLError {
    SQLError::Internal("selected B-tree input lost its admitted class or carrier".into())
}

#[cfg(test)]
mod tests;
