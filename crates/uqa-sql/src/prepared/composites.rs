//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time composite descriptors for retained input constants and immutable executable caches.

use crate::{
    expr::composites::retained::{capture_descriptor, capture_type, project_value, Descriptors},
    plan::UnifiedPlan,
    type_resolution::FunctionTypeResolver,
    ColumnType, SQLError, ScalarExpr,
};
use std::sync::Arc;
use uqa_core::Value;

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
                        Some(ty) => capture_type(ty, types.composite_types(), &mut original),
                        None => types.resolve_type_name(ty).and_then(|ty| {
                            ty.map_or(Ok(()), |ty| {
                                capture_type(&ty, types.composite_types(), &mut original)
                            })
                        }),
                    },
                    ScalarExpr::Func {
                        binding: Some(binding),
                        ..
                    } => binding.composite_field.as_ref().map_or(Ok(()), |field| {
                        capture_descriptor(field.type_oid, types.composite_types(), &mut original)?;
                        capture_type(&field.result_type, types.composite_types(), &mut original)
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
        project_plan(plan, types, &self.original, true, None)
    }

    /// Update attribute names and field bindings for optimization while retaining the original scalar datums in the reusable executable. Interpreting a raw byte as boolean is not reversible, so an interpreted value cannot replace its source in a cache.
    pub fn project_for_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        project_plan(plan, types, &self.original, false, None)
    }

    /// Record the names and attribute numbers of the actual generic plan with the types of its retained scalar datums. Folded scalar results have no remaining composite dependency.
    pub fn with_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Self, SQLError> {
        let mut planned = Self::capture(plan, types)?.original.as_ref().clone();
        for (oid, descriptor) in &mut planned {
            let Some(original) = self.original.get(oid) else {
                continue;
            };
            for attribute in &mut Arc::make_mut(descriptor).attributes {
                if let Some(source) = original
                    .attributes
                    .iter()
                    .find(|source| source.number == attribute.number)
                {
                    attribute.ty.clone_from(&source.ty);
                }
            }
        }
        Ok(Self {
            original: self.original.clone(),
            planned: Some(Arc::new(planned)),
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
        self.planned.as_ref().map_or(Ok(None), |original| {
            project_plan(plan, types, original, true, self.planned_scope.as_deref())
        })
    }
}

fn project_plan(
    plan: &UnifiedPlan,
    types: &dyn FunctionTypeResolver,
    original: &Descriptors,
    interpret_datums: bool,
    binding_scope: Option<&Descriptors>,
) -> Result<Option<UnifiedPlan>, SQLError> {
    if original.is_empty() {
        return Ok(None);
    }
    let current = current_descriptors(original, types)?;
    let values_changed = *original != current;
    let bindings_changed = binding_scope.is_some_and(|scope| {
        current
            .iter()
            .any(|(oid, descriptor)| scope.get(oid) != Some(descriptor))
    });
    if !values_changed && !bindings_changed {
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
            composite_source,
        } = node
        {
            if !values_changed {
                return;
            }
            let result = bound_type
                .clone()
                .map_or_else(|| types.resolve_type_name(ty), |ty| Ok(Some(ty)))
                .and_then(|ty| {
                    ty.map_or_else(
                        || Ok(value.clone()),
                        |ty| {
                            project_literal(
                                value,
                                &ty,
                                composite_source,
                                original,
                                types,
                                interpret_datums,
                            )
                        },
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

fn project_literal(
    value: &Value,
    ty: &ColumnType,
    source: &mut Option<Box<crate::expr::composites::CompositeConstantSource>>,
    original: &Descriptors,
    types: &dyn FunctionTypeResolver,
    interpret_datums: bool,
) -> Result<Value, SQLError> {
    if !crate::expr::composites::literal::contains_records(value) {
        return Ok(value.clone());
    }
    let source = source.get_or_insert_with(|| {
        Box::new(crate::expr::composites::CompositeConstantSource {
            value: value.clone(),
            descriptors: original
                .values()
                .map(|value| value.as_ref().clone())
                .collect(),
        })
    });
    let source_descriptors = source
        .descriptors
        .iter()
        .map(|descriptor| (descriptor.type_oid, Arc::new(descriptor.clone())))
        .collect::<Descriptors>();
    let live = current_descriptors(&source_descriptors, types)?;
    project_value(
        &source.value,
        ty,
        &source_descriptors,
        &live,
        interpret_datums,
    )
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

#[cfg(test)]
fn project(
    value: &Value,
    ty: &ColumnType,
    original: &Descriptors,
    current: &Descriptors,
) -> Result<Value, SQLError> {
    project_value(value, ty, original, current, true)
}

#[cfg(test)]
mod tests;
