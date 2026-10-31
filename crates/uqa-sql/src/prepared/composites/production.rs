//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared composite generations retain admitted descriptor maps and transform owned scalar nodes.

use super::CompositeInputs;
use crate::{
    expr::composites::{
        retained::owners::{self, DescriptorSet},
        CompositeTypeDescriptor, OwnedCompositeTypeDescriptor,
    },
    plan::{MutableExecutablePlan, UnifiedPlan},
    type_resolution::FunctionTypeResolver,
    SQLError, ScalarExpr,
};
use uqa_core::memory::{OwnedMap, ProductionControl};
mod projection;

impl CompositeInputs {
    pub fn capture_with_control(
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::capture_for_plan_with_control(plan.into(), types, control)
    }

    pub fn capture_for_plan_with_control(
        plan: crate::plan::PlanRef<'_>,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut entries = OwnedMap::new();
        let mut status: Result<(), SQLError> = Ok(());
        plan.visit_scalar_expressions(&mut |expression| {
            if status.is_err() {
                return;
            }
            status = expression.try_visit(&mut |node| {
                control.check()?;
                match node {
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
                    } => {
                        if let Some(ty) = bound_type {
                            owners::capture_type(
                                ty,
                                types.composite_types(),
                                &mut entries,
                                control,
                            )?;
                        } else if let Some(ty) =
                            types.resolve_type_name_with_control(ty, control)?
                        {
                            owners::capture_type(
                                &ty,
                                types.composite_types(),
                                &mut entries,
                                control,
                            )?;
                        }
                    }
                    ScalarExpr::Func {
                        binding: Some(binding),
                        ..
                    } => {
                        if let Some(field) = binding.composite_field() {
                            owners::capture_descriptor(
                                field.type_oid,
                                types.composite_types(),
                                &mut entries,
                                control,
                            )?;
                            owners::capture_type(
                                &field.result_type,
                                types.composite_types(),
                                &mut entries,
                                control,
                            )?;
                        }
                    }
                    _ => {}
                }
                Ok(true)
            });
        });
        status?;
        Ok(Self {
            original: DescriptorSet::from_entries(entries, control)?,
            planned: None,
            planned_scope: None,
        })
    }

    pub fn project_logical_owned(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExecutablePlan>, SQLError> {
        projection::project_plan(plan, types, &self.original, true, None, control)
    }
    pub fn project_for_generic_owned(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExecutablePlan>, SQLError> {
        projection::project_plan(plan, types, &self.original, false, None, control)
    }
    pub fn project_generic_owned(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Option<MutableExecutablePlan>, SQLError> {
        self.planned.as_ref().map_or(Ok(None), |original| {
            projection::project_plan(
                plan,
                types,
                original,
                true,
                self.planned_scope.as_ref(),
                control,
            )
        })
    }

    pub fn with_generic_with_control(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let observed = Self::capture_with_control(plan, types, control)?.original;
        let mut entries = OwnedMap::new();
        for (oid, descriptor) in observed.iter() {
            control.check()?;
            let descriptor = if let Some(original) = self.original.get(*oid) {
                let descriptor = CompositeTypeDescriptor::from_metadata_with_control(
                    descriptor.type_oid,
                    descriptor.relation_oid,
                    descriptor.attributes.iter().map(|attribute| {
                        let ty = original
                            .attributes
                            .iter()
                            .find(|source| source.number == attribute.number)
                            .map_or(&attribute.ty, |source| &source.ty);
                        Ok((
                            attribute.name.as_str(),
                            ty,
                            attribute.number,
                            attribute.collation_oid,
                        ))
                    }),
                    descriptor.dropped.iter().copied().map(Ok),
                    control,
                )?;
                OwnedCompositeTypeDescriptor::from_produced(descriptor, control)?
            } else {
                observed.owner(*oid).expect("observed descriptor owner")
            };
            owners::insert(&mut entries, descriptor, control)?;
        }
        Ok(Self {
            original: self.original.clone(),
            planned: Some(DescriptorSet::from_entries(entries, control)?),
            planned_scope: Some(current_descriptors(&self.original, types, control)?),
        })
    }

    pub fn generic_requires_rebuild_with_control(
        &self,
        types: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let Some(planned) = &self.planned_scope else {
            return Ok(false);
        };
        let current = current_descriptors(&self.original, types, control)?;
        for (oid, original) in self.original.iter() {
            control.check()?;
            let (Some(at_plan), Some(current)) = (planned.get(*oid), current.get(*oid)) else {
                continue;
            };
            for attribute in &current.attributes {
                control.check()?;
                if original
                    .attributes
                    .iter()
                    .any(|old| old.number == attribute.number)
                    && !at_plan
                        .attributes
                        .iter()
                        .any(|old| old.number == attribute.number)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn current_descriptors(
    original: &DescriptorSet,
    types: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<DescriptorSet, SQLError> {
    control.check()?;
    if original.is_empty() {
        return Ok(DescriptorSet::default());
    }
    let catalog = types
        .composite_types()
        .ok_or_else(|| SQLError::Internal("prepared composite catalog unavailable".into()))?;
    let mut entries = OwnedMap::new();
    for (oid, _) in original.iter() {
        if let Some(descriptor) = catalog.composite_type_with_control(*oid, control)? {
            owners::insert(&mut entries, descriptor, control)?;
        }
    }
    DescriptorSet::from_entries(entries, control)
}
