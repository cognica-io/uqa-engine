//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite type values through the statement's catalog: attribute metadata, `record_in` input and the coercion of rows and records to a composite type.
//!
//! Logical composite fields use [`Value::Record`]; admitted input and retained tuples use [`Value::Datum`] so later catalog changes interpret their original physical layout. Anonymous row constructors are [`Value::Row`] until a coercion gives them a composite type.

use std::sync::Arc;

use uqa_core::Value;

use super::{EngineHook, Result, SQLError};
use crate::ast::ColumnType;

#[cfg(test)]
mod assignment_tests;
mod changes;
pub mod constants;
pub mod constructor;
pub(crate) mod datum;
mod descriptor;
pub(super) mod fields;
mod input;
pub mod literal;
mod ownership;
pub(crate) mod retained;
mod retention;
pub use changes::{apply_attribute_change, type_contains_composite, AttributeChange};
pub use input::parse_record_fields;
pub use ownership::{descriptor_with_control, OwnedCompositeTypeDescriptor};
pub use retained::CompositeConstantSource;
pub use retention::{flatten_value_with_control, retain_value_with_control};

/// Classify an admitted physical value without reading its payload or invoking input.
pub fn datum_is_composite(
    datum: &uqa_core::DatumValue,
    engine: &dyn super::EngineHook,
) -> Result<bool> {
    datum_is_composite_with_control(
        datum,
        engine,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn datum_is_composite_with_control(
    datum: &uqa_core::DatumValue,
    engine: &dyn super::EngineHook,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool> {
    control.check()?;
    if datum.record_fields().is_some() {
        return Ok(true);
    }
    let ty = engine.resolve_type_oid_with_control(datum.type_oid(), control)?;
    let mut ty = ty.as_deref();
    while let Some(ColumnType::Domain { base, .. }) = ty {
        ty = Some(base);
    }
    Ok(matches!(
        ty,
        Some(ColumnType::Composite(_) | ColumnType::Record)
    ))
}

/// One live attribute of a composite type in attribute-number order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompositeAttribute {
    pub name: String,
    pub ty: ColumnType,
    /// Selected declaration identity; absent metadata uses the attribute type's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation_oid: Option<u32>,
    /// `pg_attribute.attnum`, which dropped attributes keep occupying.
    pub number: i16,
}

impl CompositeAttribute {
    #[must_use]
    pub fn collation_oid(&self) -> u32 {
        self.collation_oid.unwrap_or_else(|| {
            u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                &self.ty,
            ))
            .expect("type collation identity")
        })
    }
}

/// The live attributes of one composite type in the statement's catalog generation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompositeTypeDescriptor {
    /// Physical slots left by removed attributes, without retaining a type dependency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<DroppedCompositeAttribute>,
    pub type_oid: u32,
    pub relation_oid: u32,
    pub attributes: Vec<CompositeAttribute>,
}

/// Width and alignment remain necessary when a retained tuple contains a removed field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DroppedCompositeAttribute {
    pub number: i16,
    pub length: i64,
    pub alignment: u8,
}

impl DroppedCompositeAttribute {
    pub fn from_type(number: i16, ty: &ColumnType) -> Self {
        Self {
            number,
            length: crate::catalog::type_metadata::pg_type_len(ty),
            alignment: crate::catalog::type_metadata::pg_type_align(ty).as_bytes()[0],
        }
    }
}

impl CompositeTypeDescriptor {
    /// Number of physical positions, including trailing dropped attributes.
    pub fn physical_width(&self) -> i16 {
        self.attributes
            .iter()
            .map(|attribute| attribute.number)
            .chain(self.dropped.iter().map(|attribute| attribute.number))
            .max()
            .unwrap_or(0)
    }

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

    /// External catalogs own their callback construction; the default copies exposed metadata under the consumer's control. Native catalogs select existing retained descriptors or admit each selected field before construction.
    fn composite_type_with_control(
        &self,
        type_oid: u32,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<OwnedCompositeTypeDescriptor>> {
        control.check()?;
        self.composite_type(type_oid)?
            .map(|descriptor| {
                OwnedCompositeTypeDescriptor::from_produced(
                    descriptor.clone_with_control(control)?,
                    control,
                )
            })
            .transpose()
    }
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
    validate_field_type(field, current)
}

pub(super) fn validate_field_type(
    field: &crate::ast::CompositeFieldBinding,
    current: &ColumnType,
) -> Result<()> {
    if crate::catalog::type_metadata::pg_type_oid(current)
        == crate::catalog::type_metadata::pg_type_oid(&field.result_type)
    {
        return Ok(());
    }
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
    let record = uqa_core::RecordValue::from_parts(fields, Some(type_oid));
    retention::retain_record(
        &record,
        &descriptor,
        engine.composite_types(),
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary composite input retention".into()))
}

/// Coerce the fields of an anonymous row to a composite type position by position, as `coerce_record_to_complex` does. The field count must match the live attributes.
fn composite_from_fields(
    engine: &dyn EngineHook,
    values: &[Value],
    field_types: Option<&[uqa_core::RecordFieldType]>,
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
    let fields = values
        .iter()
        .zip(&descriptor.attributes)
        .enumerate()
        .map(|(index, (value, attribute))| {
            let source = field_types
                .and_then(|types| types.get(index))
                .filter(|field| field.oid != 705)
                .map(|field| {
                    if let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type(field.oid)
                    {
                        return Ok(ty.clone());
                    }
                    if let Some(element) =
                        crate::catalog::type_metadata::builtin_array_element(field.oid)
                    {
                        return Ok(ColumnType::Array(Box::new(element.clone())));
                    }
                    engine
                        .resolve_type_oid(field.oid)
                        .map_err(SQLError::Internal)?
                        .ok_or_else(|| SQLError::Routine {
                            sqlstate: "42704".into(),
                            message: format!("type with OID {} does not exist", field.oid),
                        })
                })
                .transpose()?
                .map(|ty| ty.catalog_name());
            let converted = super::cast_value_with_type_resolution(
                value,
                source.as_deref(),
                &attribute.ty.catalog_name(),
                Some(engine),
            )?;
            Ok((attribute.name.clone(), converted))
        })
        .collect::<Result<Vec<_>>>()?;
    let record = uqa_core::RecordValue::from_parts(fields, Some(descriptor.type_oid));
    retention::retain_record(
        &record,
        descriptor,
        engine.composite_types(),
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| SQLError::Internal("ordinary record coercion retention".into()))
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
        (Value::Record(fields), Some(ColumnType::Composite(source)))
            if source.oid == reference.oid =>
        {
            Ok(Some(Value::Record(fields.clone().with_type_oid(Some(
                fields.type_oid().unwrap_or(source.oid),
            )))))
        }
        (Value::Datum(_), Some(ColumnType::Composite(source))) if source.oid == reference.oid => {
            Ok(Some(value.clone()))
        }
        (Value::Record(_) | Value::Row(_), Some(source @ ColumnType::Composite(_))) => {
            Err(cannot_cast(&source.display_name(), &target_name(), None))
        }
        (Value::Row(values), _) => {
            let descriptor = descriptor(engine.composite_types(), reference.oid)?;
            composite_from_fields(
                engine,
                values,
                values.field_types(),
                &descriptor,
                &target_name(),
            )
            .map(Some)
        }
        (Value::Record(fields), _) => {
            let descriptor = descriptor(engine.composite_types(), reference.oid)?;
            let values = fields
                .iter()
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>();
            composite_from_fields(engine, &values, None, &descriptor, &target_name()).map(Some)
        }
        (_, source) => Err(cannot_cast(
            &source.map_or_else(|| "unknown".into(), ColumnType::display_name),
            &target_name(),
            None,
        )),
    }
}
