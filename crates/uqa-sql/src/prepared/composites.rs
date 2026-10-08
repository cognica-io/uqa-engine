//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time composite descriptors for retained input constants and immutable executable caches.

use crate::{
    expr::composites::{descriptor, CompositeTypeDescriptor},
    plan::UnifiedPlan,
    type_resolution::FunctionTypeResolver,
    ColumnType, SQLError, ScalarExpr,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{ArrayValue, Value};

mod datum;

type Descriptors = BTreeMap<u32, Arc<CompositeTypeDescriptor>>;

/// Logical and optimized inputs retain their own descriptors and values across rollback. Reading either plan projects a copy without rerunning already-folded expressions.
#[derive(Clone, Debug, Default)]
pub struct CompositeInputs {
    original: Arc<Descriptors>,
    planned: Option<Arc<Descriptors>>,
    planned_scope: Option<Arc<Descriptors>>,
}

impl CompositeInputs {
    pub(super) fn has_inputs(&self) -> bool {
        !self.original.is_empty()
    }

    pub(super) fn tracks_oid(&self, oid: u32) -> bool {
        self.original.contains_key(&oid)
    }

    pub(super) fn tracks_type(&self, ty: &ColumnType) -> bool {
        match ty {
            ColumnType::Array(element) => self.tracks_type(element),
            ColumnType::Composite(reference) => self.tracks_oid(reference.oid),
            _ => false,
        }
    }

    pub(super) fn tracks_name(&self, name: &str) -> bool {
        crate::ast::UserTypeIdentity::parse(name).is_some_and(|identity| {
            identity.kind == crate::ast::UserTypeKind::Composite && self.tracks_oid(identity.oid)
        })
    }

    pub fn capture(plan: &UnifiedPlan, types: &dyn FunctionTypeResolver) -> Result<Self, SQLError> {
        let mut original = Descriptors::new();
        let mut failure = None;
        plan.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |node| {
                if failure.is_some() {
                    return;
                }
                let result = match node {
                    ScalarExpr::TypedLiteral {
                        ty,
                        bound_type,
                        parameter_index: None,
                        ..
                    }
                    | ScalarExpr::CompositeRow {
                        binding: crate::ast::CompositeRowBinding { ty, .. },
                        bound_type,
                        ..
                    } => match bound_type {
                        Some(ty) => capture_type(ty, types, &mut original),
                        None => types.resolve_type_name(ty).and_then(|ty| {
                            ty.map_or(Ok(()), |ty| capture_type(&ty, types, &mut original))
                        }),
                    },
                    ScalarExpr::Func {
                        binding: Some(binding),
                        ..
                    } => binding.composite_field.as_ref().map_or(Ok(()), |field| {
                        capture_descriptor(field.type_oid, types, &mut original)?;
                        capture_type(&field.result_type, types, &mut original)
                    }),
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    failure = Some(error);
                }
            });
        });
        failure.map_or(
            Ok(Self {
                original: Arc::new(original),
                planned: None,
                planned_scope: None,
            }),
            Err,
        )
    }

    /// Project original inputs before result analysis and optimization without changing the retained source.
    pub fn project_logical(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        project_plan(plan, types, &self.original)
    }

    /// Record descriptors of the actual generic plan. Folded scalar results have no remaining composite dependency.
    pub fn with_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            original: self.original.clone(),
            planned: Some(Self::capture(plan, types)?.original),
            planned_scope: Some(Arc::new(current_descriptors(&self.original, types)?)),
        })
    }

    /// Attribute numbers cannot be reused. A number present at logical analysis, absent at optimization, and live again therefore comes from catalog undo; discard constants folded under the undone descriptor.
    pub fn generic_requires_rebuild(
        &self,
        types: &dyn FunctionTypeResolver,
    ) -> Result<bool, SQLError> {
        let Some(planned) = &self.planned_scope else {
            return Ok(false);
        };
        let current = current_descriptors(&self.original, types)?;
        for (oid, original) in self.original.iter() {
            let Some(at_plan) = planned.get(oid) else {
                continue;
            };
            let Some(current) = current.get(oid) else {
                continue;
            };
            if current.attributes.iter().any(|attribute| {
                original
                    .attributes
                    .iter()
                    .any(|old| old.number == attribute.number)
                    && !at_plan
                        .attributes
                        .iter()
                        .any(|old| old.number == attribute.number)
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Adapt a copy of the cached plan, preserving constants already folded by its original optimization and the source fields needed after rollback.
    pub fn project_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        self.planned
            .as_ref()
            .map_or(Ok(None), |original| project_plan(plan, types, original))
    }
}

fn project_plan(
    plan: &UnifiedPlan,
    types: &dyn FunctionTypeResolver,
    original: &Descriptors,
) -> Result<Option<UnifiedPlan>, SQLError> {
    if original.is_empty() {
        return Ok(None);
    }
    let current = current_descriptors(original, types)?;
    if *original == current {
        return Ok(None);
    }
    let mut projected = plan.clone();
    let mut failure = None;
    projected.rewrite_scalar_expressions(&mut |node| {
        if failure.is_some() {
            return;
        }
        if let ScalarExpr::TypedLiteral {
            value,
            ty,
            parameter_index: None,
            bound_type,
        } = node
        {
            let result = bound_type
                .clone()
                .map_or_else(|| types.resolve_type_name(ty), |ty| Ok(Some(ty)))
                .and_then(|ty| {
                    ty.map_or_else(
                        || Ok(value.clone()),
                        |ty| project(value, &ty, original, &current),
                    )
                });
            match result {
                Ok(projected) => *value = projected,
                Err(error) => failure = Some(error),
            }
        }
        if let ScalarExpr::Func {
            binding: Some(binding),
            args,
            ..
        } = node
        {
            if let Some(field) = &mut binding.composite_field {
                if let Some(descriptor) = current.get(&field.type_oid) {
                    let attribute = descriptor
                        .attributes
                        .iter()
                        .find(|attribute| attribute.number == field.number);
                    field.dropped = attribute.is_none();
                    field.changed_type = attribute
                        .filter(|attribute| {
                            crate::catalog::type_metadata::pg_type_oid(&attribute.ty)
                                != crate::catalog::type_metadata::pg_type_oid(&field.result_type)
                        })
                        .map(|attribute| attribute.ty.clone());
                    if let (Some(attribute), Some(ScalarExpr::Literal(Value::Str(name)))) =
                        (attribute, args.get_mut(1))
                    {
                        name.clone_from(&attribute.name);
                    }
                }
            }
        }
    });
    failure.map_or(Ok(Some(projected)), Err)
}

fn current_descriptors(
    original: &Descriptors,
    types: &dyn FunctionTypeResolver,
) -> Result<Descriptors, SQLError> {
    if original.is_empty() {
        return Ok(Descriptors::new());
    }
    let catalog = types
        .composite_types()
        .ok_or_else(|| SQLError::Internal("prepared composite catalog unavailable".into()))?;
    let mut current = Descriptors::new();
    for oid in original.keys() {
        // A removed nested type need not be read after its containing field is removed. Still-executable references report their normal missing-type error at evaluation.
        if let Some(descriptor) = catalog.composite_type(*oid)? {
            current.insert(*oid, descriptor);
        }
    }
    Ok(current)
}

fn capture_type(
    ty: &ColumnType,
    types: &dyn FunctionTypeResolver,
    output: &mut Descriptors,
) -> Result<(), SQLError> {
    match ty {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
            capture_type(base, types, output)
        }
        ColumnType::Composite(reference) => capture_descriptor(reference.oid, types, output),
        _ => Ok(()),
    }
}

fn capture_descriptor(
    oid: u32,
    types: &dyn FunctionTypeResolver,
    output: &mut Descriptors,
) -> Result<(), SQLError> {
    if output.contains_key(&oid) {
        return Ok(());
    }
    let descriptor = descriptor(types.composite_types(), oid)?;
    output.insert(oid, descriptor.clone());
    for attribute in &descriptor.attributes {
        capture_type(&attribute.ty, types, output)?;
    }
    Ok(())
}

fn project(
    value: &Value,
    ty: &ColumnType,
    original: &Descriptors,
    current: &Descriptors,
) -> Result<Value, SQLError> {
    match (ty, value) {
        (_, Value::Null) => Ok(Value::Null),
        (ColumnType::Domain { base, .. }, _) => project(value, base, original, current),
        (ColumnType::Composite(reference), Value::Record(fields)) => {
            let (Some(before), Some(after)) =
                (original.get(&reference.oid), current.get(&reference.oid))
            else {
                return Ok(value.clone());
            };
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
                        let reinterpreted = datum::reinterpret(value, &old.ty, &attribute.ty);
                        project(
                            reinterpreted.as_ref().unwrap_or(value),
                            &attribute.ty,
                            original,
                            current,
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
                .map(|value| project_array_element(value, element, original, current))
                .collect::<Result<Vec<_>, _>>()?;
            ArrayValue::with_lower_bounds(values, array.lower_bounds().to_vec())
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::Internal("prepared composite projection changed array shape".into())
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
) -> Result<Value, SQLError> {
    match value {
        Value::List(values) => values
            .iter()
            .map(|value| project_array_element(value, element, original, current))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        value => project(value, element, original, current),
    }
}

#[cfg(test)]
mod tests;
