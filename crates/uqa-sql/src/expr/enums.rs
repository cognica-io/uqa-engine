//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum label input and output, ordered label functions and I/O casts through the statement's catalog.
//!
//! Values carry an immutable label key; the catalog supplies the current label text, so
//! `ALTER TYPE ... RENAME VALUE` never rewrites stored values. Native comparisons use key order;
//! physical enum support functions observe raw OIDs and consult catalog order only when needed.

use std::sync::Arc;

use uqa_core::{ArrayValue, EnumLabelKey, EnumValue, Value};

use super::{Result, SQLError};
use crate::ast::ColumnType;

mod functions;
mod physical;
pub use functions::{enum_function_value, enum_function_value_with_state};
pub use physical::{comparison_identity, eval_comparison, EnumComparisonState};

/// One label of an enum type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumTypeLabel {
    pub oid: u32,
    pub key: EnumLabelKey,
    pub label: String,
}

/// The labels of one enum type in ascending key order, which is declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumTypeLabels {
    pub type_oid: u32,
    pub labels: Vec<EnumTypeLabel>,
}

impl EnumTypeLabels {
    pub fn by_key(&self, key: &EnumLabelKey) -> Option<&EnumTypeLabel> {
        self.labels
            .binary_search_by(|label| label.key.cmp(key))
            .ok()
            .map(|index| &self.labels[index])
    }

    pub fn by_text(&self, text: &str) -> Option<&EnumTypeLabel> {
        self.labels.iter().find(|label| label.label == text)
    }

    fn value(&self, label: &EnumTypeLabel) -> Value {
        Value::Enum(
            EnumValue::new(self.type_oid, label.key.clone()).with_label_oid(Some(label.oid)),
        )
    }
}

/// Catalog access for enum label input and output, supplied by the statement's execution context and by catalog-aware binding.
pub trait EnumLabelCatalog {
    /// The labels of one enum type in the statement's catalog generation, or `None` when the catalog has no such type.
    fn enum_type_labels(&self, type_oid: u32) -> Result<Option<Arc<EnumTypeLabels>>>;

    /// Resolve an already admitted physical label OID across enum types. Output uses the label's actual identity even if a retained tuple now declares another enum type; it does not repeat input safety checks.
    fn enum_value_by_oid(&self, _label_oid: u32) -> Result<Option<EnumValue>> {
        Ok(None)
    }

    /// The actual enum type and zero-based label position of an admitted physical OID in this generation. Positions have declaration/key order and avoid copying label keys for comparison.
    fn enum_label_position(&self, _label_oid: u32) -> Result<Option<(u32, usize)>> {
        Ok(None)
    }

    /// Whether the current transaction added this label to a type that it did not create. `PostgreSQL` rejects such a label until the transaction commits.
    fn enum_label_uncommitted(&self, label_oid: u32) -> bool;

    /// `format_type_be` of the type, which qualifies a type hidden by the search path.
    fn enum_type_name(&self, type_oid: u32) -> Result<Option<String>>;

    /// Whether the statement catalog defines any enum type; binding skips enum literal validation otherwise.
    fn has_enum_types(&self) -> bool;
}

pub(crate) fn enum_value_from_oid(
    catalog: Option<&dyn EnumLabelCatalog>,
    label_oid: u32,
) -> Result<Value> {
    catalog
        .map(|catalog| catalog.enum_value_by_oid(label_oid))
        .transpose()?
        .flatten()
        .map(Value::Enum)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "22P03".into(),
            message: format!("invalid internal value for enum: {label_oid}"),
        })
}

fn catalog_unavailable(type_oid: u32) -> SQLError {
    SQLError::Internal(format!(
        "enum type OID {type_oid} is not available in the statement catalog"
    ))
}

pub(crate) fn labels(
    catalog: Option<&dyn EnumLabelCatalog>,
    type_oid: u32,
) -> Result<Arc<EnumTypeLabels>> {
    catalog
        .ok_or_else(|| catalog_unavailable(type_oid))?
        .enum_type_labels(type_oid)?
        .ok_or_else(|| catalog_unavailable(type_oid))
}

pub(crate) fn type_name(catalog: Option<&dyn EnumLabelCatalog>, type_oid: u32) -> Result<String> {
    catalog
        .ok_or_else(|| catalog_unavailable(type_oid))?
        .enum_type_name(type_oid)?
        .ok_or_else(|| catalog_unavailable(type_oid))
}

/// `check_safe_enum_use`: a label added by the current transaction to a type it did not create is unusable until commit.
fn check_safe(
    catalog: Option<&dyn EnumLabelCatalog>,
    labels: &EnumTypeLabels,
    label: &EnumTypeLabel,
) -> Result<()> {
    if catalog.is_some_and(|catalog| catalog.enum_label_uncommitted(label.oid)) {
        return Err(SQLError::Diagnostic {
            sqlstate: "55P04".into(),
            message: format!(
                "unsafe use of new value \"{}\" of enum type {}",
                label.label,
                type_name(catalog, labels.type_oid)?
            ),
            detail: None,
            hint: Some("New enum values must be committed before they can be used.".into()),
        });
    }
    Ok(())
}

fn invalid_internal_value(value: &EnumValue) -> SQLError {
    SQLError::Internal(format!(
        "invalid internal value for enum type OID {}",
        value.type_oid()
    ))
}

/// The catalog label of a value, which carries the label OID and current text.
pub(crate) fn enum_label(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &EnumValue,
) -> Result<EnumTypeLabel> {
    labels(catalog, value.type_oid())?
        .by_key(value.key())
        .cloned()
        .ok_or_else(|| invalid_internal_value(value))
}

/// `enum_out`: the current label of a value.
pub fn enum_label_text(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &EnumValue,
) -> Result<String> {
    enum_label(catalog, value).map(|label| label.label)
}

/// Catalog OID of an enum value's label, the datum `PostgreSQL` stores for the value.
pub fn enum_label_oid(catalog: Option<&dyn EnumLabelCatalog>, value: &EnumValue) -> Result<u32> {
    enum_label(catalog, value).map(|label| label.oid)
}

/// `enum_in`: the label with exactly this text. Case and surrounding spaces are significant.
pub fn enum_value_from_text(
    catalog: Option<&dyn EnumLabelCatalog>,
    type_oid: u32,
    text: &str,
) -> Result<Value> {
    let labels = labels(catalog, type_oid)?;
    let Some(label) = labels.by_text(text) else {
        return Err(SQLError::Routine {
            sqlstate: "22P02".into(),
            message: format!(
                "invalid input value for enum {}: \"{text}\"",
                type_name(catalog, type_oid)?
            ),
        });
    };
    check_safe(catalog, &labels, label)?;
    Ok(labels.value(label))
}

/// `enum_first` and `enum_last` read only the argument's type; the returned label must be usable.
pub fn enum_endpoint(
    catalog: Option<&dyn EnumLabelCatalog>,
    type_oid: u32,
    last: bool,
) -> Result<Value> {
    let labels = labels(catalog, type_oid)?;
    let label = if last {
        labels.labels.last()
    } else {
        labels.labels.first()
    };
    let Some(label) = label else {
        return Err(SQLError::Routine {
            sqlstate: "55000".into(),
            message: format!("enum {} contains no values", type_name(catalog, type_oid)?),
        });
    };
    check_safe(catalog, &labels, label)?;
    Ok(labels.value(label))
}

/// `enum_range(lower, upper)`: labels from `lower` through `upper` inclusive, where a missing bound is open. A lower bound after the upper bound yields an empty array. Every returned label must be usable.
pub fn enum_range(
    catalog: Option<&dyn EnumLabelCatalog>,
    type_oid: u32,
    lower: Option<&EnumValue>,
    upper: Option<&EnumValue>,
) -> Result<Value> {
    let lower = lower
        .map(|value| enum_label_oid(catalog, value))
        .transpose()?;
    let upper = upper
        .map(|value| enum_label_oid(catalog, value))
        .transpose()?;
    range_by_oid(catalog, type_oid, lower, upper)
}

fn range_by_oid(
    catalog: Option<&dyn EnumLabelCatalog>,
    type_oid: u32,
    lower: Option<u32>,
    upper: Option<u32>,
) -> Result<Value> {
    let labels = labels(catalog, type_oid)?;
    let mut elements = Vec::new();
    let mut include = lower.is_none_or(|oid| oid == 0);
    for label in &labels.labels {
        if lower == Some(label.oid) {
            include = true;
        }
        if include {
            check_safe(catalog, &labels, label)?;
            elements.push(labels.value(label));
        }
        if upper == Some(label.oid) {
            break;
        }
    }
    ArrayValue::try_new(elements)
        .map(|array| array.with_element_type_oid(Some(type_oid)))
        .map(Value::Array)
        .ok_or_else(|| SQLError::Internal("enum range array has invalid dimensions".into()))
}

/// Replace every enum carrier inside a value by its label text, as the output functions of containers do.
pub fn render_enum_labels(catalog: Option<&dyn EnumLabelCatalog>, value: &Value) -> Result<Value> {
    map_enum_values(value, &mut |label| {
        enum_label_text(catalog, label).map(Value::Str)
    })
}

/// Retain physical identities without repeating enum input checks or changing admitted values. Legacy carriers resolve their missing OID once; existing identities remain usable after catalog changes.
pub(crate) fn retain_enum_oids(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &Value,
) -> Result<Value> {
    map_enum_values(value, &mut |label| {
        let oid = match label.label_oid() {
            Some(oid) => oid,
            None => enum_label_oid(catalog, label)?,
        };
        Ok(Value::Enum(label.clone().with_label_oid(Some(oid))))
    })
}

fn map_enum_values(
    value: &Value,
    convert: &mut dyn FnMut(&EnumValue) -> Result<Value>,
) -> Result<Value> {
    Ok(match value {
        Value::Enum(label) => convert(label)?,
        Value::Array(array) => Value::Array(map_array(array, |element| {
            map_enum_values(element, convert)
        })?),
        Value::List(values) => Value::List(
            values
                .iter()
                .map(|element| map_enum_values(element, convert))
                .collect::<Result<_>>()?,
        ),
        Value::Row(values) => Value::Row(
            values.clone().with_values(
                values
                    .iter()
                    .map(|element| map_enum_values(element, convert))
                    .collect::<Result<_>>()?,
            )?,
        ),
        Value::Record(fields) => Value::Record(
            fields
                .iter()
                .map(|(name, element)| Ok((name.clone(), map_enum_values(element, convert)?)))
                .collect::<Result<_>>()?,
        ),
        Value::Map(fields) => Value::Map(
            fields
                .iter()
                .map(|(name, element)| Ok((name.clone(), map_enum_values(element, convert)?)))
                .collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

/// Rebuild an array with the same bounds after converting every leaf element; nested dimensions are `List` elements.
fn map_array(
    array: &ArrayValue,
    mut convert: impl FnMut(&Value) -> Result<Value>,
) -> Result<ArrayValue> {
    fn leaves(
        values: &[Value],
        convert: &mut dyn FnMut(&Value) -> Result<Value>,
    ) -> Result<Vec<Value>> {
        values
            .iter()
            .map(|value| match value {
                Value::List(values) => leaves(values, convert).map(Value::List),
                value => convert(value),
            })
            .collect()
    }
    let elements = leaves(array.elements(), &mut convert)?;
    ArrayValue::with_lower_bounds(elements, array.lower_bounds().to_vec())
        .filter(|converted| converted.dimensions() == array.dimensions())
        .map(|converted| converted.with_element_type_oid(array.element_type_oid()))
        .ok_or_else(|| SQLError::Internal("enum array conversion changed array dimensions".into()))
}

/// Whether a value contains an enum carrier anywhere.
pub fn contains_enum_carrier(value: &Value) -> bool {
    enum_carrier_matches(value, &|_| true)
}

pub(crate) fn has_missing_enum_oid(value: &Value) -> bool {
    enum_carrier_matches(value, &|label| label.label_oid().is_none())
}

fn enum_carrier_matches(value: &Value, predicate: &dyn Fn(&EnumValue) -> bool) -> bool {
    match value {
        Value::Enum(label) => predicate(label),
        Value::Array(array) => array
            .elements()
            .iter()
            .any(|value| enum_carrier_matches(value, predicate)),
        Value::List(values) => values
            .iter()
            .any(|value| enum_carrier_matches(value, predicate)),
        Value::Row(values) => values
            .iter()
            .any(|value| enum_carrier_matches(value, predicate)),
        Value::Record(fields) => fields
            .iter()
            .any(|(_, value)| enum_carrier_matches(value, predicate)),
        Value::Map(fields) => fields
            .values()
            .any(|value| enum_carrier_matches(value, predicate)),
        _ => false,
    }
}

/// Whether a built-in call converts its arguments with their types' output functions, so enum arguments contribute their current labels. `||` concatenates text only when neither operand is an array; with an array operand it appends or prepends the enum value itself.
#[must_use]
pub fn call_applies_output_functions(name: &str, arguments: &[(Option<String>, Value)]) -> bool {
    match name {
        "concat_op" => !arguments
            .iter()
            .any(|(_, value)| matches!(value, Value::Array(_) | Value::List(_))),
        "concat" | "concat_ws" | "format" | "quote_literal" | "quote_nullable" | "to_json"
        | "to_jsonb" | "row_to_json" | "array_to_json" | "json_build_object"
        | "json_build_array" | "jsonb_build_object" | "jsonb_build_array" | "array_to_string" => {
            true
        }
        _ => false,
    }
}

/// Render the enum carriers of an output-function call's arguments as their labels.
pub fn render_call_arguments(
    catalog: Option<&dyn EnumLabelCatalog>,
    arguments: Vec<(Option<String>, Value)>,
) -> Result<Vec<(Option<String>, Value)>> {
    arguments
        .into_iter()
        .map(|(name, value)| {
            if contains_enum_carrier(&value) {
                Ok((name, render_enum_labels(catalog, &value)?))
            } else {
                Ok((name, value))
            }
        })
        .collect()
}

/// Arguments handed to host-registered functions: enum values become their labels, as procedural languages receive them through the type's output function.
pub fn render_host_arguments(
    catalog: Option<&dyn EnumLabelCatalog>,
    arguments: &[Value],
) -> Result<Vec<Value>> {
    arguments
        .iter()
        .map(|value| {
            if contains_enum_carrier(value) {
                render_enum_labels(catalog, value)
            } else {
                Ok(value.clone())
            }
        })
        .collect()
}

/// Whether a declared type is an enum or an array whose leaf is an enum.
#[must_use]
pub fn is_enum_bearing(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Enum(_) => true,
        ColumnType::Array(element) => is_enum_bearing(element),
        _ => false,
    }
}

/// Convert an `unknown` literal to an enum or enum-array type with the type's input function, as `PostgreSQL` does when it coerces an untyped constant during parse analysis. Returns `None` for any other target.
pub fn fold_unknown_literal(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &Value,
    target: &ColumnType,
) -> Result<Option<Value>> {
    match (target, value) {
        (ColumnType::Enum(_) | ColumnType::Array(_), Value::Null) if is_enum_bearing(target) => {
            Ok(Some(Value::Null))
        }
        (ColumnType::Enum(reference), Value::Str(text)) => {
            enum_value_from_text(catalog, reference.oid, text).map(Some)
        }
        (ColumnType::Array(element), Value::Str(text)) if is_enum_bearing(element) => {
            let mut leaf = element.as_ref();
            while let ColumnType::Array(inner) = leaf {
                leaf = inner;
            }
            let ColumnType::Enum(reference) = leaf else {
                return Ok(None);
            };
            let parsed = super::casting::parse_pg_array_literal(text)?;
            map_array(&parsed, |element| match element {
                Value::Null => Ok(Value::Null),
                Value::Str(text) => enum_value_from_text(catalog, reference.oid, text),
                other => Err(SQLError::Internal(format!(
                    "array literal parsing produced a non-text element {other:?}"
                ))),
            })
            .map(|array| {
                Some(Value::Array(
                    array.with_element_type_oid(Some(reference.oid)),
                ))
            })
        }
        _ => Ok(None),
    }
}

/// The string category accepted by automatic I/O conversion casts.
fn string_category(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Text
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Name => true,
        ColumnType::Domain { base, .. } => string_category(base),
        _ => false,
    }
}

fn cannot_cast(source: &str, target: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!("cannot cast type {source} to {target}"),
    }
}

/// Cast a value to an enum type: identity for the same type, `enum_in` for unknown literals and string-category sources, and 42846 otherwise. Returns `None` when the target is not an enum.
pub fn cast_to_enum(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &Value,
    source: Option<&ColumnType>,
    target: &ColumnType,
) -> Result<Option<Value>> {
    let ColumnType::Enum(reference) = target else {
        return Ok(None);
    };
    let source_name = |source: Option<&ColumnType>| -> Result<String> {
        Ok(match (value, source) {
            (Value::Enum(label), _) => type_name(catalog, label.type_oid())?,
            (_, Some(ColumnType::Enum(source))) => type_name(catalog, source.oid)?,
            (_, Some(source)) => source.sql_name(),
            (_, None) => "unknown".into(),
        })
    };
    match value {
        Value::Null => Ok(Some(Value::Null)),
        Value::Enum(label) if label.type_oid() == reference.oid => Ok(Some(value.clone())),
        Value::Str(text) | Value::FixedChar(text) if source.is_none_or(string_category) => {
            enum_value_from_text(catalog, reference.oid, text).map(Some)
        }
        _ => Err(cannot_cast(
            &source_name(source)?,
            &type_name(catalog, reference.oid)?,
        )),
    }
}

/// Cast an enum value to a string-category type through its label; any other target has no cast.
pub fn enum_output_for_cast(
    catalog: Option<&dyn EnumLabelCatalog>,
    label: &EnumValue,
    target: &ColumnType,
) -> Result<Value> {
    if string_category(target) {
        return enum_label_text(catalog, label).map(Value::Str);
    }
    Err(cannot_cast(
        &type_name(catalog, label.type_oid())?,
        &match target {
            ColumnType::Enum(reference) => type_name(catalog, reference.oid)?,
            other => other.sql_name(),
        },
    ))
}
