//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared field operators are selected before record NULL handling or array iteration.

pub(in crate::expr) use crate::type_resolution::ComparisonSupport;
use crate::{expr::SQLValueCatalog, SQLError};
use uqa_core::{memory::ProductionControl, ArrayValue};

struct ValueCompositeCatalog<'a>(Option<&'a dyn SQLValueCatalog>);

impl crate::expr::composites::CompositeTypeCatalog for ValueCompositeCatalog<'_> {
    fn composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<std::sync::Arc<crate::expr::composites::CompositeTypeDescriptor>>, SQLError>
    {
        self.0
            .ok_or_else(|| SQLError::Internal("composite operator lookup has no catalog".into()))?
            .value_composite_type(oid)
    }
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
        let composites = ValueCompositeCatalog(catalog);
        if let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type(oid) {
            return self.available_for_type(ty, Some(&composites), control);
        }
        if let Some(element) = crate::catalog::type_metadata::builtin_array_element(oid) {
            return self.available_for_type(element, Some(&composites), control);
        }
        let ty = catalog
            .map(|catalog| catalog.value_type_by_oid(oid))
            .transpose()?
            .flatten()
            .map(|ty| ty.retain_external_with_control(control))
            .transpose()?;
        if let Some(ty) = ty {
            return self.available_for_type(&ty, Some(&composites), control);
        }
        Err(SQLError::Internal(format!(
            "comparison type with OID {oid} has no catalog metadata"
        )))
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
