//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original composite inputs shared by prepared plans and durable expression constants.

use super::{datum, descriptor, CompositeTypeDescriptor};
use crate::{ColumnType, SQLError};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{ArrayValue, Value};

mod tuple;

pub(crate) type Descriptors = BTreeMap<u32, Arc<CompositeTypeDescriptor>>;

/// The original admitted datum and its field types, retained independently of subsequent descriptor interpretations.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CompositeConstantSource {
    pub value: Value,
    pub descriptors: Vec<CompositeTypeDescriptor>,
}

impl PartialEq for CompositeConstantSource {
    fn eq(&self, other: &Self) -> bool {
        self.descriptors == other.descriptors && self.value.has_same_representation(&other.value)
    }
}

impl CompositeConstantSource {
    pub(crate) fn capture(
        value: &Value,
        ty: &ColumnType,
        catalog: Option<&dyn super::CompositeTypeCatalog>,
    ) -> Result<Self, SQLError> {
        let mut descriptors = Descriptors::new();
        capture_type(ty, catalog, &mut descriptors)?;
        Ok(Self {
            value: value.clone(),
            descriptors: descriptors
                .into_values()
                .map(|value| value.as_ref().clone())
                .collect(),
        })
    }

    pub(crate) fn project_type_change(
        &self,
        ty: &ColumnType,
        target: u32,
        name: &str,
        to: &ColumnType,
        catalog: &dyn super::CompositeTypeCatalog,
    ) -> Result<Value, SQLError> {
        let original = self
            .descriptors
            .iter()
            .map(|descriptor| (descriptor.type_oid, Arc::new(descriptor.clone())))
            .collect::<Descriptors>();
        let mut current = Descriptors::new();
        for oid in original.keys() {
            if let Some(descriptor) = catalog.composite_type(*oid)? {
                current.insert(*oid, descriptor);
            }
        }
        if let Some(descriptor) = current.get_mut(&target) {
            if let Some(attribute) = Arc::make_mut(descriptor)
                .attributes
                .iter_mut()
                .find(|attribute| attribute.name == name)
            {
                attribute.ty = to.clone();
            }
        }
        project_value(&self.value, ty, &original, &current, true)
    }
}

pub(crate) fn capture_type(
    ty: &ColumnType,
    catalog: Option<&dyn super::CompositeTypeCatalog>,
    output: &mut Descriptors,
) -> Result<(), SQLError> {
    match ty {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
            capture_type(base, catalog, output)
        }
        ColumnType::Composite(reference) => capture_descriptor(reference.oid, catalog, output),
        _ => Ok(()),
    }
}

pub(crate) fn capture_descriptor(
    oid: u32,
    catalog: Option<&dyn super::CompositeTypeCatalog>,
    output: &mut Descriptors,
) -> Result<(), SQLError> {
    if output.contains_key(&oid) {
        return Ok(());
    }
    let descriptor = descriptor(catalog, oid)?;
    output.insert(oid, descriptor.clone());
    for attribute in &descriptor.attributes {
        capture_type(&attribute.ty, catalog, output)?;
    }
    Ok(())
}

pub(crate) fn project_value(
    value: &Value,
    ty: &ColumnType,
    original: &Descriptors,
    current: &Descriptors,
    interpret_datums: bool,
) -> Result<Value, SQLError> {
    match (ty, value) {
        (_, Value::Null) => Ok(Value::Null),
        (ColumnType::Domain { base, .. }, _) => {
            project_value(value, base, original, current, interpret_datums)
        }
        (ColumnType::Composite(reference), Value::Record(fields)) => {
            let (Some(before), Some(after)) =
                (original.get(&reference.oid), current.get(&reference.oid))
            else {
                return Ok(value.clone());
            };
            if interpret_datums {
                if let Some(projected) = tuple::project(fields, before, after) {
                    return Ok(projected);
                }
            }
            after
                .attributes
                .iter()
                .map(|attribute| {
                    let retained = before
                        .attributes
                        .iter()
                        .find(|old| old.number == attribute.number)
                        .and_then(|old| {
                            fields
                                .iter()
                                .find(|(name, _)| *name == old.name)
                                .map(|(_, value)| (old, value))
                        });
                    let value = retained.map_or(Ok(Value::Null), |(old, value)| {
                        let reinterpreted = interpret_datums
                            .then(|| datum::reinterpret(value, &old.ty, &attribute.ty))
                            .flatten();
                        project_value(
                            reinterpreted.as_ref().unwrap_or(value),
                            &attribute.ty,
                            original,
                            current,
                            interpret_datums,
                        )
                    })?;
                    Ok((attribute.name.clone(), value))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Record)
        }
        (ColumnType::Array(element), Value::Array(array)) => {
            let values = array
                .elements()
                .iter()
                .map(|value| {
                    project_array_element(value, element, original, current, interpret_datums)
                })
                .collect::<Result<Vec<_>, _>>()?;
            ArrayValue::with_lower_bounds(values, array.lower_bounds().to_vec())
                .map(|converted| converted.with_element_type_oid(array.element_type_oid()))
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::Internal("retained composite projection changed array shape".into())
                })
        }
        _ => Ok(value.clone()),
    }
}

fn project_array_element(
    value: &Value,
    element: &ColumnType,
    original: &Descriptors,
    current: &Descriptors,
    interpret_datums: bool,
) -> Result<Value, SQLError> {
    match value {
        Value::List(values) => values
            .iter()
            .map(|value| project_array_element(value, element, original, current, interpret_datums))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        value => project_value(value, element, original, current, interpret_datums),
    }
}
