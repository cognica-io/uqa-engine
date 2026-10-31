//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host parameter declarations bind against the current catalog independently of value observation.

use crate::{ColumnType, FunctionTypeResolver, RowSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

#[cfg(test)]
mod tests;

/// Bind every supplied host parameter, including unused inputs. Procedural variables have already been admitted and keep their separate, lazy expression semantics.
pub fn bind_parameter_types(
    parameters: &[SQLParam],
    types: &dyn FunctionTypeResolver,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let control = ProductionControl::uncontrolled();
    parameters
        .iter()
        .map(|parameter| {
            bind_parameter_type_with_control(parameter, types, &control).map(|ty| {
                ty.map(|ty| {
                    ty.into_uncontrolled()
                        .expect("ordinary parameter binding has no reservation")
                })
            })
        })
        .collect()
}

/// Refresh a prepared definition's declared parameter identities before argument conversion. A missing declaration is an error even for an unused parameter supplied as untyped NULL.
pub fn bind_declared_parameter_types(
    declarations: &[Option<ColumnType>],
    types: &dyn FunctionTypeResolver,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let control = ProductionControl::uncontrolled();
    declarations
        .iter()
        .map(|ty| {
            ty.as_ref()
                .map(|ty| {
                    declared_parameter_type(ty, types, &control).map(|ty| {
                        ty.into_uncontrolled()
                            .expect("ordinary declaration binding has no reservation")
                    })
                })
                .transpose()
        })
        .collect()
}

/// Resolve a retained declared identity without decoding its payload or rerunning domain input checks. Unknown strings and NULL keep their contextual type inference.
pub fn bind_parameter_type_with_control(
    parameter: &SQLParam,
    types: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    match parameter {
        SQLParam::Scalar(Value::Str(_) | Value::Null) => Ok(None),
        SQLParam::TypedScalar { ty, .. } => declared_parameter_type(ty, types, control).map(Some),
        _ => crate::type_resolution::scalar_type_with_control(
            &ScalarExpr::Param(1),
            &RowSchema::empty_with_control(control)?,
            std::slice::from_ref(parameter),
            control,
        ),
    }
}

fn declared_parameter_type(
    ty: &ColumnType,
    types: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    let mut leaf = ty;
    while let ColumnType::Array(element) = leaf {
        leaf = element;
    }
    if !matches!(
        leaf,
        ColumnType::Enum(_)
            | ColumnType::Composite(_)
            | ColumnType::Domain { .. }
            | ColumnType::DeclaredRange(_)
            | ColumnType::DeclaredMultirange(_)
    ) {
        return ty.clone_with_control(control).map_err(Into::into);
    }
    let name = ty.catalog_name_with_control(control)?;
    match types.resolve_type_name_with_control(&name, control) {
        Ok(Some(current)) => Ok(current),
        Ok(None) => Err(missing_parameter_type(ty)),
        Err(error) if error.sqlstate() == Some("42704") => Err(missing_parameter_type(ty)),
        Err(error) => Err(error),
    }
}

fn missing_parameter_type(ty: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!(
            "cache lookup failed for type {}",
            crate::catalog::type_metadata::pg_type_oid(ty),
        ),
    }
}
