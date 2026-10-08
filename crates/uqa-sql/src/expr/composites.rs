//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite type values through the statement's catalog: attribute metadata, `record_in` input and the coercion of rows and records to a composite type.
//!
//! A composite value is a [`Value::Record`] whose fields carry the type's current attribute names in attribute order. Anonymous row constructors are [`Value::Row`] until a coercion gives them a composite type.

use std::sync::Arc;

use uqa_core::Value;

use super::{EngineHook, Result, SQLError};
use crate::ast::ColumnType;

mod changes;
pub mod constants;
pub mod constructor;
pub(crate) mod datum;
mod input;
pub mod literal;
pub use changes::{apply_attribute_change, type_contains_composite, AttributeChange};
pub use input::parse_record_fields;

/// One live attribute of a composite type in attribute-number order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeAttribute {
    pub name: String,
    pub ty: ColumnType,
    /// `pg_attribute.attnum`, which dropped attributes keep occupying.
    pub number: i16,
}

/// The live attributes of one composite type in the statement's catalog generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeTypeDescriptor {
    pub type_oid: u32,
    pub relation_oid: u32,
    pub attributes: Vec<CompositeAttribute>,
}

impl CompositeTypeDescriptor {
    pub fn attribute(&self, name: &str) -> Option<(usize, &CompositeAttribute)> {
        self.attributes
            .iter()
            .enumerate()
            .find(|(_, attribute)| attribute.name == name)
    }
}

/// Catalog access for composite values, supplied by the statement's execution context and by catalog-aware binding.
pub trait CompositeTypeCatalog {
    /// The attributes of one composite type, or `None` when the catalog has no such type.
    fn composite_type(&self, type_oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>>;
}

/// The attributes of a composite type the statement's catalog must contain.
pub fn descriptor(
    catalog: Option<&dyn CompositeTypeCatalog>,
    type_oid: u32,
) -> Result<Arc<CompositeTypeDescriptor>> {
    let catalog = catalog
        .ok_or_else(|| SQLError::Internal("composite type catalog is unavailable".into()))?;
    catalog
        .composite_type(type_oid)?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type with OID {type_oid} does not exist"),
        })
}

/// `ExecEvalFieldSelect` checks the current attribute OID against the prepared result type. Call only after evaluating a non-NULL record and checking the dropped marker.
pub fn validate_field_result(field: &crate::ast::CompositeFieldBinding) -> Result<()> {
    let Some(current) = &field.changed_type else {
        return Ok(());
    };
    Err(SQLError::Diagnostic {
        sqlstate: "42804".into(),
        message: format!("attribute {} has wrong type", field.number),
        detail: Some(format!(
            "Table has type {}, but query expects {}.",
            current.display_name(),
            field.result_type.display_name(),
        )),
        hint: None,
    })
}

fn cannot_cast(source: &str, target: &str, detail: Option<String>) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42846".into(),
        message: format!("cannot cast type {source} to {target}"),
        detail,
        hint: None,
    }
}

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

/// Convert one attribute's text with the attribute type's input function and type modifier, which rejects a value too long for its declared length instead of truncating it as an explicit cast does. A NULL field still reaches a domain's input function, which enforces the domain's NOT NULL constraint.
fn attribute_input(
    engine: &dyn EngineHook,
    text: Option<String>,
    ty: &ColumnType,
) -> Result<Value> {
    let value = text.map_or(Value::Null, Value::Str);
    if matches!(value, Value::Null) && !matches!(ty, ColumnType::Domain { .. }) {
        return Ok(Value::Null);
    }
    if crate::assignment::conversion::type_requires_catalog_resolution(ty) {
        return super::cast_value_with_type_resolution(
            &value,
            Some("unknown"),
            &ty.catalog_name(),
            Some(engine),
        );
    }
    crate::assignment::conversion::convert_value_to_column_type(value, ty)
}

/// `record_in` for a composite type: the fields are split and each is converted in order, so a field's input error precedes a later structural error.
pub fn composite_from_text(engine: &dyn EngineHook, text: &str, type_oid: u32) -> Result<Value> {
    let descriptor = descriptor(engine.composite_types(), type_oid)?;
    let mut fields = Vec::with_capacity(descriptor.attributes.len());
    parse_record_fields(text, descriptor.attributes.len(), |index, field| {
        let attribute = &descriptor.attributes[index];
        fields.push((
            attribute.name.clone(),
            attribute_input(engine, field, &attribute.ty)?,
        ));
        Ok(())
    })?;
    Ok(Value::Record(fields))
}

/// Coerce the fields of an anonymous row to a composite type position by position, as `coerce_record_to_complex` does. The field count must match the live attributes.
fn composite_from_fields(
    engine: &dyn EngineHook,
    values: &[Value],
    descriptor: &CompositeTypeDescriptor,
    target: &str,
) -> Result<Value> {
    if values.len() != descriptor.attributes.len() {
        let detail = if values.len() < descriptor.attributes.len() {
            "Input has too few columns."
        } else {
            "Input has too many columns."
        };
        return Err(cannot_cast("record", target, Some(detail.into())));
    }
    values
        .iter()
        .zip(&descriptor.attributes)
        .map(|(value, attribute)| {
            let converted = super::cast_value_with_type_resolution(
                value,
                None,
                &attribute.ty.catalog_name(),
                Some(engine),
            )?;
            Ok((attribute.name.clone(), converted))
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::Record)
}

/// Cast a value to a composite type: the identity for a value of the same type, `record_in` for unknown literals and string-category sources, and attribute-wise coercion for anonymous rows and records. Returns `None` when the target is not a composite type.
pub fn cast_to_composite(
    engine: &dyn EngineHook,
    value: &Value,
    source: Option<&ColumnType>,
    target: &ColumnType,
) -> Result<Option<Value>> {
    let ColumnType::Composite(reference) = target else {
        return Ok(None);
    };
    let target_name = || target.display_name();
    match (value, source) {
        (Value::Null, _) => Ok(Some(Value::Null)),
        (Value::Str(text) | Value::FixedChar(text), source)
            if source.is_none_or(string_category) =>
        {
            composite_from_text(engine, text, reference.oid).map(Some)
        }
        (Value::Record(_), Some(ColumnType::Composite(source))) if source.oid == reference.oid => {
            Ok(Some(value.clone()))
        }
        (Value::Record(_) | Value::Row(_), Some(source @ ColumnType::Composite(_))) => {
            Err(cannot_cast(&source.display_name(), &target_name(), None))
        }
        (Value::Row(values), _) => {
            let descriptor = descriptor(engine.composite_types(), reference.oid)?;
            composite_from_fields(engine, values, &descriptor, &target_name()).map(Some)
        }
        (Value::Record(fields), _) => {
            let descriptor = descriptor(engine.composite_types(), reference.oid)?;
            let values = fields
                .iter()
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>();
            composite_from_fields(engine, &values, &descriptor, &target_name()).map(Some)
        }
        (_, source) => Err(cannot_cast(
            &source.map_or_else(|| "unknown".into(), ColumnType::display_name),
            &target_name(),
            None,
        )),
    }
}
