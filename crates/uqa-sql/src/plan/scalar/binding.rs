//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function bindings are copied only for borrowed column ASTs.

use super::{
    resources::{Lowering, Result},
    source::Source,
};
use crate::ast::{
    FunctionBinding, FunctionResolutionError, OperatorResolutionError, RoutineInvocationBinding,
};

mod input;

impl FunctionBinding {
    /// Copy a stored call identity through the native work channel. Names, argument vectors, invocation metadata and nested type payloads keep the selected allowance across every suspension.
    pub async fn clone_from_input(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> std::result::Result<uqa_core::memory::Produced<Self>, crate::SQLError> {
        control.check()?;
        let mut copying = Lowering {
            control: control
                .budget()
                .map(|_| super::resources::Control::from_production(*control)),
            owned_children: None,
        };
        let binding = copying.binding_from(self, None, work).await?;
        Ok(control.finish(binding, copying.control.map(|owner| owner.memory))?)
    }

    /// Copy the complete bound identity with the scalar lowerer's existing allocation admission.
    pub fn clone_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>> {
        control.check()?;
        if control.budget().is_none() {
            return Ok(control.finish(self.clone(), None)?);
        }
        let mut copying = Lowering {
            control: Some(super::resources::Control::from_production(*control)),
            owned_children: None,
        };
        let binding = copying.binding(Source::Borrowed(self))?;
        Ok(control.finish(binding, copying.control.map(|owner| owner.memory))?)
    }
    pub(crate) fn with_composite_projection_with_control(
        &self,
        dropped: bool,
        changed_type: Option<&crate::ColumnType>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>> {
        control.check()?;
        assert!(
            self.composite_field().is_some(),
            "composite projection requires selected field metadata"
        );
        let mut copying = Lowering {
            control: control
                .budget()
                .map(|_| super::resources::Control::from_production(*control)),
            owned_children: None,
        };
        let binding = copying.binding_fields(self, Some((dropped, changed_type)))?;
        Ok(control.finish(binding, copying.control.map(|owner| owner.memory))?)
    }
}

impl Lowering<'_> {
    pub(super) fn binding(
        &mut self,
        source: Source<'_, FunctionBinding>,
    ) -> Result<FunctionBinding> {
        self.check()?;
        let binding = match source {
            Source::Owned(binding) => return Ok(binding),
            Source::Borrowed(binding) => binding,
        };
        self.binding_fields(binding, None)
    }

    fn binding_fields(
        &mut self,
        binding: &FunctionBinding,
        field_projection: Option<(bool, Option<&crate::ColumnType>)>,
    ) -> Result<FunctionBinding> {
        crate::catalog::work::inline(self.binding_from(binding, field_projection, &mut ()))
    }
}
