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
