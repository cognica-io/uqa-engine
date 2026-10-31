//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Signature declarations retain exactly their admitted names, defaults, and catalog type projections.

use super::super::{RoutineCallKind, RoutineTypeSnapshot};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionParamMode, UserTypeIdentity, UserTypeKind},
    type_resolution::{canonical_routine_type_name_with_control, RoutineParameterDescriptor},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn indices(
    definition: &CreateFunction,
    kind: RoutineCallKind,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let mut indices = ProductionVec::new(*control);
    for (index, parameter) in definition.params.iter().enumerate() {
        control.check()?;
        if participates(definition, parameter.mode, kind) {
            indices.push_copy(index)?;
        }
    }
    Ok(indices.finish()?)
}

pub(super) fn descriptors(
    catalog: &RoutineTypeSnapshot,
    definition: &CreateFunction,
    indices: &[usize],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<RoutineParameterDescriptor>>, SQLError> {
    let mut parameters = ProductionVec::new(*control);
    parameters.reserve(indices.len())?;
    for index in indices {
        control.check()?;
        let parameter = &definition.params[*index];
        let name = control.copy_text(&parameter.name)?;
        let type_name = canonical_routine_type_name_with_control(&parameter.type_name, control)?;
        let column_type = declared_type(catalog, &parameter.type_name, control)?;
        let default_type =
            crate::plan::copy::copy_syntax_with_control(&parameter.default_type, control)?;
        let (name, name_memory) = name.into_parts();
        let (type_name, type_memory) = type_name.into_parts();
        let (default_type, default_memory) = default_type.into_parts();
        let (column_type, column_memory) =
            column_type.map_or((None, control.empty_reservation()), |ty| {
                let (ty, memory) = ty.into_parts();
                (Some(ty), memory)
            });
        parameters.push_produced(control.finish(
            RoutineParameterDescriptor {
                name: Some(name),
                type_name,
                column_type,
                has_default: parameter.default.is_some(),
                default_type,
                variadic: parameter.mode == FunctionParamMode::Variadic,
            },
            control.combine(
                control.combine(name_memory, type_memory),
                control.combine(column_memory, default_memory),
            ),
        )?)?;
    }
    Ok(parameters.finish()?)
}

fn declared_type(
    catalog: &RoutineTypeSnapshot,
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if let Some(element) = type_name.strip_suffix("[]") {
        return declared_type(catalog, element, control)?
            .map(|element| ColumnType::array_with_control(element, control).map_err(Into::into))
            .transpose();
    }
    if let Some(identity) = UserTypeIdentity::parse(type_name) {
        if identity.kind != UserTypeKind::Domain {
            return Ok(None);
        }
        for domain in catalog.values() {
            control.check()?;
            if domain.oid == identity.oid {
                return domain
                    .column_type_with_control(control)
                    .map(Some)
                    .map_err(Into::into);
            }
        }
        return crate::catalog::system_catalog_domain_with_control(identity.oid, control)
            .map_err(Into::into);
    }
    match ColumnType::from_sql_name_with_control(type_name, control) {
        Ok(ty) => return Ok(Some(ty)),
        Err(error)
            if error.sqlstate() == Some("53200")
                || matches!(error.primary(), SQLError::Cancelled(_)) =>
        {
            return Err(error)
        }
        Err(_) => {}
    }
    let canonical = canonical_routine_type_name_with_control(type_name, control)?;
    for domain in catalog.values() {
        control.check()?;
        let ty = domain.column_type_with_control(control)?;
        let name = ty.sql_name_with_control(control)?;
        if *canonical_routine_type_name_with_control(&name, control)? == *canonical {
            return Ok(Some(ty));
        }
    }
    Ok(None)
}

fn participates(
    definition: &CreateFunction,
    mode: FunctionParamMode,
    kind: RoutineCallKind,
) -> bool {
    if kind == RoutineCallKind::Procedure && !definition.is_procedure {
        return true;
    }
    match mode {
        FunctionParamMode::In | FunctionParamMode::InOut | FunctionParamMode::Variadic => true,
        FunctionParamMode::Out => definition.is_procedure,
        FunctionParamMode::Table => false,
    }
}
