//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared field operators are selected before record NULL handling or array iteration.

use crate::{ast::ColumnType, expr::SQLValueCatalog, SQLError};
use uqa_core::{memory::ProductionControl, ArrayValue};

#[derive(Clone, Copy)]
pub(in crate::expr) enum ComparisonSupport {
    Equality,
    Ordering,
}

impl ComparisonSupport {
    pub(super) fn error(self, sqlstate: &str, type_name: String) -> SQLError {
        let capability = match self {
            Self::Equality => "an equality operator",
            Self::Ordering => "a comparison function",
        };
        SQLError::Routine {
            sqlstate: sqlstate.into(),
            message: format!("could not identify {capability} for type {type_name}"),
        }
    }

    pub(super) fn available_for_oid(
        self,
        oid: u32,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        if matches!(oid, 705 | 2278) {
            return Ok(false);
        }
        if let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type(oid) {
            return self.available_for_type(ty, catalog, control);
        }
        if let Some(element) = crate::catalog::type_metadata::builtin_array_element(oid) {
            return self.available_for_type(element, catalog, control);
        }
        let ty = catalog
            .map(|catalog| catalog.value_type_by_oid(oid))
            .transpose()?
            .flatten()
            .map(|ty| ty.retain_external_with_control(control))
            .transpose()?;
        if let Some(ty) = ty {
            return self.available_for_type(&ty, catalog, control);
        }
        Err(SQLError::Internal(format!(
            "comparison type with OID {oid} has no catalog metadata"
        )))
    }

    fn available_for_type(
        self,
        ty: &ColumnType,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        match ty {
            ColumnType::Void | ColumnType::Json => Ok(false),
            ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
                self.available_for_type(base, catalog, control)
            }
            ColumnType::Composite(reference) => {
                let descriptor = catalog
                    .ok_or_else(|| {
                        SQLError::Internal("composite operator lookup has no catalog".into())
                    })?
                    .value_composite_type(reference.oid)?
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "42704".into(),
                        message: format!("type with OID {} does not exist", reference.oid),
                    })?;
                for attribute in &descriptor.attributes {
                    if !self.available_for_type(&attribute.ty, catalog, control)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // Anonymous record support is selected at each reached field, rather than from one declared descriptor.
            _ => Ok(match self {
                Self::Equality => crate::type_resolution::require_equality_operator(ty),
                Self::Ordering => crate::type_resolution::require_ordering_operator(ty),
            }
            .is_ok()),
        }
    }
}

pub(in crate::expr) fn require_array_element_support(
    array: &ArrayValue,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    support: ComparisonSupport,
    sqlstate: &str,
) -> Result<(), SQLError> {
    let Some(oid) = array.element_type_oid() else {
        return Ok(());
    };
    if support.available_for_oid(oid, catalog, control)? {
        return Ok(());
    }
    Err(support.error(sqlstate, comparison_type_name(oid, catalog)?))
}

pub(super) fn comparison_type_name(
    oid: u32,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<String, SQLError> {
    match oid {
        705 => return Ok("unknown".into()),
        2278 => return Ok("void".into()),
        _ => {}
    }
    let ty = catalog
        .map(|catalog| catalog.value_type_by_oid(oid))
        .transpose()?
        .flatten();
    let name = if let Some(ty) = ty
        .as_ref()
        .or_else(|| crate::catalog::type_metadata::builtin_scalar_type(oid))
    {
        ty.display_name()
    } else if let Some(element) = crate::catalog::type_metadata::builtin_array_element(oid) {
        format!("{}[]", element.display_name())
    } else {
        format!("type with OID {oid}")
    };
    Ok(name)
}
