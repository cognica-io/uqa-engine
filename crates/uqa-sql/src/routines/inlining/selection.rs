//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected routine preparation keeps arguments, specialization, and diagnostics independently owned until body substitution completes.

use super::{
    is_constant, is_null_constant, owned_arguments, owned_substitution, properties,
    RoutineInliningContext,
};
use crate::{
    ast::{CreateFunction, FunctionBinding, FunctionReturns, FunctionVolatility},
    plan::OwnedScalarExpr,
    routines::{
        result_check::declared_sql_function_result_type_with_control,
        routine_returns_anonymous_record_with_control,
        security::ensure_routine_execute_privilege_with_control, SQLUserFunction,
    },
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

/// A scalar replacement retains its routine recursion identity and the admitted diagnostic frame while the caller continues rewriting it.
pub struct InlineOwnedRoutineExpression {
    pub identity: [u8; 16],
    pub expression: OwnedScalarExpr,
    pub error_context: Option<Produced<String>>,
}

#[expect(
    clippy::large_enum_variant,
    reason = "one short-lived inline candidate stays on the stack; boxing it would add a separately admitted allocation before each body attempt"
)]
pub(super) enum InlineSelection {
    Declined,
    Expression(InlineOwnedRoutineExpression),
    Body(OwnedInlineBody),
}

pub(super) struct OwnedInlineBody {
    pub(super) function: Arc<SQLUserFunction>,
    specialized: Option<Produced<CreateFunction>>,
    arguments: OwnedScalarExpr,
    identity: [u8; 16],
    pub(super) error_context: Produced<String>,
}

impl OwnedInlineBody {
    pub(super) fn definition(&self) -> &CreateFunction {
        self.specialized.as_deref().unwrap_or(&self.function.def)
    }

    pub(super) fn substitute(
        self,
        expression: OwnedScalarExpr,
        parameters: &[SQLParam],
        control: &ProductionControl<'_>,
        context: &RoutineInliningContext<'_>,
    ) -> Result<Option<InlineOwnedRoutineExpression>, SQLError> {
        let properties = properties::inspect_with_control(
            context,
            &expression,
            parameters,
            &RowSchema::empty_with_control(control)?,
            control,
        )?;
        if !properties.permits(self.definition()) {
            return Ok(None);
        }
        let ScalarExpr::Or(arguments) = &*self.arguments else {
            unreachable!("routine argument container");
        };
        let uses =
            owned_substitution::use_counts_with_control(&expression, arguments.len(), control)?;
        for (argument, count) in arguments.iter().zip(uses.iter().copied()) {
            control.check()?;
            if count == 0 && self.definition().strict {
                return Ok(None);
            }
            if count > 1 && !properties::can_duplicate_with_control(context, argument, control)? {
                return Ok(None);
            }
        }
        let expression =
            owned_substitution::substitute_with(expression, |index| &arguments[index], control)?;
        Ok(Some(InlineOwnedRoutineExpression {
            identity: self.identity,
            expression,
            error_context: Some(self.error_context),
        }))
    }
}

impl RoutineInliningContext<'_> {
    pub(super) fn select_inline_with_control(
        &self,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        active: &[[u8; 16]],
        control: &ProductionControl<'_>,
    ) -> Result<InlineSelection, SQLError> {
        control.check()?;
        if binding.builtin || binding.resolution_error.is_some() {
            return Ok(InlineSelection::Declined);
        }
        let Some(identity) = binding.object_id else {
            return Ok(InlineSelection::Declined);
        };
        let Some(function) =
            crate::routines::selected_bound_function_with_control(self.routines, binding, control)?
        else {
            return Ok(InlineSelection::Declined);
        };
        let Some(invocation) = binding.invocation.as_deref() else {
            return Ok(InlineSelection::Declined);
        };
        let specialized = crate::routines::invocation::specialized_definition_with_control(
            &function.def,
            invocation,
            control,
        )?;
        let definition = specialized.as_deref().unwrap_or(&function.def);
        let Some(prepared) =
            owned_arguments::prepare(self, definition, invocation, arguments, control)?
        else {
            return Ok(InlineSelection::Declined);
        };
        let prepared = prepared.into_container(control)?;
        let ScalarExpr::Or(prepared_values) = &*prepared else {
            unreachable!("routine argument container");
        };
        if let Some(expression) = self.constant_call_with_control(
            definition,
            binding,
            prepared_values,
            arguments,
            control,
        )? {
            return Ok(InlineSelection::Expression(InlineOwnedRoutineExpression {
                identity,
                expression,
                error_context: None,
            }));
        }
        if !eligible_definition_with_control(definition, control)? || active.contains(&identity) {
            return Ok(InlineSelection::Declined);
        }
        match ensure_routine_execute_privilege_with_control(self.authority, definition, control) {
            Ok(()) => {}
            Err(error) if error.sqlstate() == Some("42501") => {
                return Ok(InlineSelection::Declined)
            }
            Err(error) => return Err(error),
        }
        let error_context = crate::routines::diagnostics::sql_context_with_control(
            definition,
            crate::routines::diagnostics::SQLRoutineStage::Inlining,
            control,
        )?;
        Ok(InlineSelection::Body(OwnedInlineBody {
            function,
            specialized,
            arguments: prepared,
            identity,
            error_context,
        }))
    }

    fn constant_call_with_control(
        &self,
        definition: &CreateFunction,
        binding: &FunctionBinding,
        arguments: &[ScalarExpr],
        arguments_for_evaluation: &[ScalarExpr],
        control: &ProductionControl<'_>,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        if definition.is_procedure
            || definition.returns_set()
            || definition.output_params().nth(1).is_some()
            || routine_returns_anonymous_record_with_control(definition, control)?
        {
            return Ok(None);
        }
        let declared_type =
            declared_sql_function_result_type_with_control(self.types, definition, control)?;
        let value = if definition.strict && arguments.iter().any(is_null_constant) {
            control.finish(Value::Null, control.empty_reservation())?
        } else if definition.volatility == FunctionVolatility::Immutable
            && arguments.iter().all(is_constant)
        {
            let arguments =
                owned_arguments::OwnedArguments::copy_from(arguments_for_evaluation, control)?;
            let call = OwnedScalarExpr::call(
                control.copy_text(&binding.name)?,
                Some(binding.clone_with_control(control)?),
                arguments.into_iter(),
                control,
            )?;
            self.expressions
                .evaluate_constant_routine_with_control(&call, control)?
        } else {
            return Ok(None);
        };
        Ok(Some(OwnedScalarExpr::typed_literal(
            value,
            declared_type.catalog_name_with_control(control)?,
            Some(declared_type),
            None,
            None,
            control,
        )?))
    }
}

pub(super) fn eligible_definition_with_control(
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if definition.language != "sql"
        || definition.is_procedure
        || definition.security.security_definer
        || definition.returns_set()
        || !definition.config.is_empty()
        || definition.output_params().nth(1).is_some()
    {
        return Ok(false);
    }
    if let FunctionReturns::Scalar { type_name } = &definition.returns {
        if crate::type_resolution::canonical_routine_type_name_with_control(type_name, control)?
            .as_str()
            == "record"
        {
            return Ok(false);
        }
    }
    Ok(true)
}
