//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedure syntax observations own their decoded slots and argument names until selected invocation publication.

use super::{ColumnType, ExpressionPlan, RoutineOverloadContext, SQLError, ScalarCallArgument};
use crate::routines::resolution::OwnedStaticFunctionMatch;
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};

pub type OwnedArgumentTypeInference<'a> =
    dyn FnMut(&ExpressionPlan) -> Result<Option<Produced<ColumnType>>, SQLError> + 'a;

pub struct OwnedProcedureCallAnalysis<'a> {
    arguments: &'a [ExpressionPlan],
    decoded: Produced<Vec<ScalarCallArgument<'a>>>,
    names: Produced<Vec<Option<String>>>,
    explicit_variadic: bool,
}

impl<'a> OwnedProcedureCallAnalysis<'a> {
    pub fn new(
        arguments: &'a [ExpressionPlan],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        super::validate_call_arguments(arguments)?;
        let (decoded, explicit_variadic) =
            crate::ir::analyze_expression_call_arguments_with_control(arguments, control)?;
        let mut names = ProductionVec::new(*control);
        names.reserve(decoded.len())?;
        for argument in decoded.iter() {
            let (name, memory) = match argument.name {
                Some(name) => {
                    let (name, memory) = control.copy_text(name)?.into_parts();
                    (Some(name), memory)
                }
                None => (None, control.empty_reservation()),
            };
            names.push_produced(control.finish(name, memory)?)?;
        }
        Ok(Self {
            arguments,
            decoded,
            names: names.finish()?,
            explicit_variadic,
        })
    }

    pub fn decoded(&self) -> &[ScalarCallArgument<'a>] {
        &self.decoded
    }

    pub fn result_schema_with_control(
        &self,
        name: &str,
        overloads: &RoutineOverloadContext<'_>,
        types: &dyn crate::routines::declaration::RoutineTypeCatalog,
        infer: &mut OwnedArgumentTypeInference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<crate::RowSchema>, SQLError> {
        let mut argument_types = ProductionVec::new(*control);
        argument_types.reserve(self.decoded.len())?;
        for (argument, decoded) in self.arguments.iter().zip(self.decoded.iter()) {
            let ty = if matches!(
                decoded.value,
                crate::ScalarExpr::Literal(uqa_core::Value::Str(_) | uqa_core::Value::Null)
            ) {
                None
            } else {
                infer(argument)?
            };
            let (ty, memory) = ty.map_or((None, control.empty_reservation()), |ty| {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            });
            argument_types.push_produced(control.finish(ty, memory)?)?;
        }
        let argument_types = argument_types.finish()?;
        let resolved = self.resolve_types(name, overloads, &argument_types, control)?;
        crate::routines::invocation::call_output_schema_with_control(
            types,
            &resolved.function.def,
            &resolved.invocation.parameter_types,
            control,
        )
    }

    pub fn resolve_types(
        &self,
        name: &str,
        overloads: &RoutineOverloadContext<'_>,
        argument_types: &[Option<ColumnType>],
        control: &ProductionControl<'_>,
    ) -> Result<OwnedStaticFunctionMatch, SQLError> {
        if let Some(resolved) = overloads.resolve_static_sql_routine_match_with_control(
            name,
            None,
            &self.names,
            argument_types,
            self.explicit_variadic,
            super::RoutineCallKind::Procedure,
            control,
        )? {
            return Ok(resolved);
        }
        let mut signature = ProductionString::new(*control);
        for (index, ty) in argument_types.iter().enumerate() {
            if index != 0 {
                signature.push_str(", ")?;
            }
            signature.push_str(ty.as_ref().map_or("unknown", super::column_type_name))?;
        }
        let signature = signature.finish()?;
        let message = control.format(format_args!(
            "procedure {name}({}) does not exist",
            signature.as_str()
        ))?;
        let hint = control.copy_text("No procedure matches the given name and argument types. You might need to add explicit type casts.")?;
        Err(SQLError::Diagnostic {
            sqlstate: control.copy_text("42883")?.into_parts().0,
            message: message.into_parts().0,
            detail: None,
            hint: Some(hint.into_parts().0),
        })
    }
}
