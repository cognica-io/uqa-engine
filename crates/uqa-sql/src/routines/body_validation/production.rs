//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL body placeholders admit catalog-selected types before publishing their parameter vector.

use super::{sql_body_parameters, CreateFunction, RoutineTypeCatalog, SQLError, SQLParam};
use crate::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub fn reject_undefined_parameters_with_control(
    plan: crate::plan::PlanRef<'_>,
    declared: usize,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut result = Ok(());
    plan.visit_scalar_expressions(&mut |expression| {
        if result.is_err() {
            return;
        }
        result = expression.try_visit(&mut |node| {
            control.check()?;
            if let crate::ScalarExpr::Param(index) = node {
                if *index > declared {
                    return Err(SQLError::Routine {
                        sqlstate: control.copy_text("42P02")?.into_parts().0,
                        message: control
                            .format(format_args!("there is no parameter ${index}"))?
                            .into_parts()
                            .0,
                    });
                }
            }
            Ok(true)
        });
    });
    result
}

pub fn reject_output_argument_call_with_control(
    overloads: &crate::routines::resolution::RoutineOverloadContext<'_>,
    types: &dyn RoutineTypeCatalog,
    name: &str,
    arguments: &[crate::plan::ExpressionPlan],
    infer: &mut crate::routines::call::OwnedArgumentTypeInference<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if crate::routines::call::OwnedProcedureCallAnalysis::new(arguments, control)?
        .result_schema_with_control(name, overloads, types, infer, control)?
        .is_some()
    {
        return Err(super::output_argument_call_error());
    }
    Ok(())
}

pub fn routine_parameter_values_with_control(
    types: &dyn RoutineTypeCatalog,
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<SQLParam>>, SQLError> {
    control.check()?;
    let mut parameters = ProductionVec::new(*control);
    parameters.reserve(sql_body_parameters(definition).count())?;
    for parameter in sql_body_parameters(definition) {
        let resolved =
            types.resolve_catalog_column_type_with_control(&parameter.type_name, control)?;
        let resolved = match resolved {
            Some(ty) => Some(ty),
            None => match ColumnType::from_sql_name_with_control(&parameter.type_name, control) {
                Ok(ty) => Some(ty),
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => None,
            },
        };
        let (value, memory) = match resolved {
            Some(ty) => {
                let (ty, memory) = ty.into_parts();
                (SQLParam::typed_scalar(Value::Null, ty), memory)
            }
            None => (SQLParam::scalar(Value::Null), control.empty_reservation()),
        };
        parameters.push_produced(control.finish(value, memory)?)?;
    }
    Ok(parameters.finish()?)
}
