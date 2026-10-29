//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function-source classification preserves borrowed arguments and admits only its selected columns.

use super::{FunctionBinding, FunctionVolatility, RoutineInliningContext, SQLError, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl};

impl RoutineInliningContext<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "source aliases, selected invocation and production control belong to the same existing expansion decision"
    )]
    pub fn scalar_from_columns_with_control(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        output_name: &str,
        alias: Option<&str>,
        columns: &[String],
        ordinality: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        let Some(function) =
            crate::routines::selected_bound_function_with_control(self.routines, binding, control)?
        else {
            return Ok(None);
        };
        if function.def.returns_set()
            || !closed_arguments_with_control(self, arguments, control)?
            || crate::semantics::user_function_composite_result_with_control(
                self.routines,
                &function,
                binding.invocation.as_deref(),
                control,
            )?
            .is_some()
        {
            return Ok(None);
        }
        let names = crate::semantics::user_table_function_columns_with_control(
            self.routines,
            &function,
            binding.invocation.as_deref(),
            output_name,
            alias,
            columns,
            ordinality,
            control,
        )?;
        Ok((names.len() == 1 + usize::from(ordinality)).then_some(names))
    }
}

fn closed_arguments_with_control(
    context: &RoutineInliningContext<'_>,
    arguments: &[ScalarExpr],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for argument in arguments {
        let mut closed = true;
        let mut failure = None;
        argument.visit(&mut |node| {
            if failure.is_some() {
                return;
            }
            if let Err(error) = control.check() {
                failure = Some(error);
                return;
            }
            closed &= !matches!(
                node,
                ScalarExpr::Column(_)
                    | ScalarExpr::QualifiedColumn { .. }
                    | ScalarExpr::InternalColumn(_)
                    | ScalarExpr::ScalarSubquery(_)
                    | ScalarExpr::Exists { .. }
                    | ScalarExpr::InSubquery { .. }
            );
        });
        if let Some(error) = failure {
            return Err(error.into());
        }
        if !closed
            || super::super::properties::volatility_with_control(
                context,
                argument,
                &crate::RowSchema::empty_with_control(control)?,
                control,
            )? == FunctionVolatility::Volatile
        {
            return Ok(false);
        }
    }
    control.check()?;
    Ok(true)
}
