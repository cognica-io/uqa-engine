//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-body parameters own their copied values and declared types before nested planning starts.

use super::{CreateFunction, RoutineBodyDescriptions, SQLError, SQLParam, Value};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    assignment::routines::RoutineValueContext, routines::body_parameters::is_sql_body_parameter,
    ColumnType,
};

pub(super) fn body_parameters_with_control(
    expressions: &dyn RoutineValueContext,
    definition: &CreateFunction,
    bound: &[Value],
    descriptions: Option<RoutineBodyDescriptions<'_>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<SQLParam>>, SQLError> {
    produce(definition, bound, control, |index, name| {
        if let Some(saved) = descriptions {
            let environment =
                uqa_sql::routines::invocation::descriptions::RoutineConversionEnvironment {
                    control: *control,
                    ..saved.environment
                };
            let ty = saved
                .description
                .body_parameter_type(index, name, environment)?;
            return ty.clone_with_control(control).map_err(Into::into);
        }
        match ColumnType::from_sql_name_with_control(name, control) {
            Ok(ty) => Ok(ty),
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
            Err(_) => expressions
                .catalog_column_type_with_control(name, control)?
                .ok_or_else(|| SQLError::TypeMismatch(format!("unknown type `{name}`"))),
        }
    })
}

fn produce(
    definition: &CreateFunction,
    bound: &[Value],
    control: &ProductionControl<'_>,
    mut resolve: impl FnMut(usize, &str) -> Result<Produced<ColumnType>, SQLError>,
) -> Result<Produced<Vec<SQLParam>>, SQLError> {
    control.check()?;
    let call_count = definition.call_arity();
    if call_count != bound.len() {
        return Err(SQLError::Internal(format!(
            "routine `{}` received {} values for {} concrete call parameters",
            definition.name,
            bound.len(),
            call_count
        )));
    }
    let positions = definition
        .params
        .iter()
        .enumerate()
        .filter(|(_, parameter)| is_sql_body_parameter(parameter))
        .map(|(index, _)| index);
    let mut parameters = ProductionVec::new(*control);
    for ((value, parameter), index) in bound
        .iter()
        .zip(definition.call_params())
        .filter(|(_, parameter)| is_sql_body_parameter(parameter))
        .zip(positions)
    {
        // Preserve value-before-type production and release either partial owner on failure.
        let value = control.copy_value(value)?;
        let ty = resolve(index, &parameter.type_name)?;
        let (value, value_memory) = value.into_parts();
        let (ty, type_memory) = ty.into_parts();
        parameters.push_produced(control.finish(
            SQLParam::typed_scalar(value, ty),
            control.combine(value_memory, type_memory),
        )?)?;
    }
    parameters.finish().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    fn definition() -> CreateFunction {
        let uqa_sql::Statement::CreateFunction(definition) = uqa_sql::compile(
            "CREATE FUNCTION retained_parameters(first text, second text) RETURNS text LANGUAGE sql AS 'SELECT first'",
        ).unwrap().remove(0) else {
            panic!("function definition");
        };
        *definition
    }

    #[test]
    fn sql_body_parameter_copy_rejects_before_large_growth_and_releases_partial_input() {
        let definition = definition();
        let arguments = [Value::Str("first".into()), Value::Str("x".repeat(4 << 20))];
        let memory = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let sibling = memory.reserve(512).unwrap();
        let mut result = None;
        let allocations = allocation_counter::measure(|| {
            result = Some(produce(&definition, &arguments, &control, |_, _| {
                ColumnType::Text
                    .clone_with_control(&control)
                    .map_err(Into::into)
            }));
        });
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(allocations.bytes_total < 4096, "{allocations:?}");
        assert_eq!(memory.used(), sibling.bytes());
    }

    #[test]
    fn sql_body_parameter_owners_survive_until_nested_statement_finishes() {
        let definition = definition();
        let arguments = [
            Value::Str("first".repeat(100)),
            Value::Str("second".repeat(100)),
        ];
        let memory = MemoryBudget::new(16384);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let output = produce(&definition, &arguments, &control, |_, _| {
            ColumnType::Text
                .clone_with_control(&control)
                .map_err(Into::into)
        })
        .unwrap();
        assert_eq!(memory.used(), output.reserved_bytes());
        assert!(memory.used() >= 1100);
        for (parameter, expected) in output.iter().zip(&arguments) {
            assert_eq!(parameter.to_value().unwrap(), *expected);
            assert_eq!(parameter.declared_scalar_type(), Some(&ColumnType::Text));
        }
        drop(output);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                produce(&definition, &arguments, &control, |_, _| {
                    unreachable!("cancelled before type resolution")
                })
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
            token.reset();
            assert_eq!(memory.used(), 0);
        }
    }
}
