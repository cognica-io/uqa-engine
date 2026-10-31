//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Creation-time composite descriptors for retained input constants and immutable executable caches.

use crate::{
    expr::composites::retained::owners::DescriptorSet,
    plan::{MutableExecutablePlan, UnifiedPlan},
    type_resolution::FunctionTypeResolver,
    ColumnType, SQLError,
};
use uqa_core::memory::ProductionControl;

mod production;

/// Logical and optimized inputs share their own immutable descriptors across rollback. Each projection owns its replacement executable and preserves original scalar provenance.
#[derive(Clone, Debug, Default)]
pub struct CompositeInputs {
    original: DescriptorSet,
    planned: Option<DescriptorSet>,
    planned_scope: Option<DescriptorSet>,
}

impl CompositeInputs {
    pub(super) fn has_inputs(&self) -> bool {
        !self.original.is_empty()
    }
    pub(super) fn tracks_oid(&self, oid: u32) -> bool {
        self.original.contains_key(oid)
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
        Self::capture_with_control(plan, types, &ProductionControl::uncontrolled())
    }
    pub fn project_logical(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        ordinary(self.project_logical_owned(plan, types, &ProductionControl::uncontrolled())?)
    }
    pub fn project_for_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        ordinary(self.project_for_generic_owned(plan, types, &ProductionControl::uncontrolled())?)
    }
    pub fn project_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        ordinary(self.project_generic_owned(plan, types, &ProductionControl::uncontrolled())?)
    }
    pub fn with_generic(
        &self,
        plan: &UnifiedPlan,
        types: &dyn FunctionTypeResolver,
    ) -> Result<Self, SQLError> {
        self.with_generic_with_control(plan, types, &ProductionControl::uncontrolled())
    }
    pub fn generic_requires_rebuild(
        &self,
        types: &dyn FunctionTypeResolver,
    ) -> Result<bool, SQLError> {
        self.generic_requires_rebuild_with_control(types, &ProductionControl::uncontrolled())
    }
}

fn ordinary(plan: Option<MutableExecutablePlan>) -> Result<Option<UnifiedPlan>, SQLError> {
    plan.map(|plan| {
        let plan = plan.finish(&ProductionControl::uncontrolled())?;
        assert_eq!(plan.reserved_bytes(), 0, "ordinary composite projection");
        Ok(plan.into_parts().0)
    })
    .transpose()
}

#[cfg(test)]
fn project(
    value: &uqa_core::Value,
    ty: &ColumnType,
    original: &crate::expr::composites::retained::Descriptors,
    current: &crate::expr::composites::retained::Descriptors,
) -> Result<uqa_core::Value, SQLError> {
    crate::expr::composites::retained::project_value(value, ty, original, current, true)
}

#[cfg(test)]
mod tests;
