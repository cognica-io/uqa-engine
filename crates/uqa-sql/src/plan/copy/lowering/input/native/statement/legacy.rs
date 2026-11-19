//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{construction, AsyncImageInput, ProductionControl, Result, RetainedExecutablePlan};
use crate::{
    ast::{FunctionBinding, FunctionDispatch},
    plan::{
        scalar::{ScalarEdit, ScalarEditIdentity},
        OwnedScalarExpr,
    },
    ScalarExpr,
};

struct LegacyCalls;

impl ScalarEditIdentity for LegacyCalls {
    type Identity = ();
    const BEFORE_CHILDREN: bool = false;
    fn identity(_: &ScalarExpr) {}
}

impl ScalarEdit<construction::Work<'_>> for LegacyCalls {
    async fn apply(
        &mut self,
        (): (),
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut construction::Work<'_>,
    ) -> Result<OwnedScalarExpr> {
        let ScalarExpr::Func { name, binding, .. } = &*expression else {
            return Ok(expression);
        };
        if let Some(dispatch) = FunctionBinding::legacy_serialized_dispatch_with_work(
            name,
            binding.as_ref(),
            control,
            work,
        )
        .await?
        {
            let selected = if let Some(binding) = binding {
                let (mut selected, lease) = binding
                    .clone_with_input_work(control, work)
                    .await?
                    .into_parts();
                selected.dispatch = Some(dispatch);
                control.finish(selected, lease)?
            } else {
                FunctionBinding::dispatched_with_control(dispatch, control)?
            };
            let name = crate::catalog::work::copy_text(work, &selected.name, control).await?;
            expression = expression
                .replace_call_metadata_from(name, Some(selected), control, work)
                .await?;
        }
        let ScalarExpr::Func { binding, args, .. } = &*expression else {
            unreachable!("migration preserves its function call")
        };
        if let Some(bounds) =
            FunctionBinding::legacy_array_slice_bounds(binding.as_ref(), args.len(), |i| {
                matches!(args[i], ScalarExpr::Literal(uqa_core::Value::Null))
            })
        {
            expression = expression.set_call_dispatch(
                Some(FunctionDispatch::ArraySlicesWithBounds(bounds)),
                control,
            )?;
        }
        Ok(expression)
    }
}

impl RetainedExecutablePlan {
    pub(crate) async fn upgrade_stored_dispatches(
        self,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self> {
        Ok(self
            .edit_scalars_from_input(LegacyCalls, control, input)
            .await?
            .0)
    }
}
