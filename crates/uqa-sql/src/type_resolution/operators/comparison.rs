//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared comparison capabilities shared by query analysis and value operators.

use crate::expr::composites::{descriptor, CompositeTypeCatalog};
use crate::{ColumnType, FunctionTypeResolver, SQLError};
use uqa_core::memory::ProductionControl;

#[derive(Clone, Copy)]
pub(crate) enum ComparisonSupport {
    Equality,
    Ordering,
}

impl ComparisonSupport {
    pub(crate) fn available_for_type(
        self,
        ty: &ColumnType,
        catalog: Option<&dyn CompositeTypeCatalog>,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        match ty {
            ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
                self.available_for_type(base, catalog, control)
            }
            ColumnType::Composite(reference) => {
                let fields = descriptor(catalog, reference.oid)?;
                for attribute in &fields.attributes {
                    if !self.available_for_type(&attribute.ty, catalog, control)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // Anonymous records select each field operator only when comparison reaches it.
            _ => Ok(self.available_without_catalog(ty)),
        }
    }

    fn available_without_catalog(self, ty: &ColumnType) -> bool {
        match self {
            Self::Equality => super::equality_operator_available(ty),
            Self::Ordering => super::ordering_operator_available(ty),
        }
    }

    pub(crate) fn require(
        self,
        ty: &ColumnType,
        resolver: Option<&dyn FunctionTypeResolver>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let available = match resolver {
            Some(resolver) => self.available_for_type(ty, resolver.composite_types(), control)?,
            None => self.available_without_catalog(ty),
        };
        if available {
            return Ok(());
        }
        let name = match resolver {
            Some(resolver) => resolver.format_type_name(ty)?,
            None => ty.sql_name(),
        };
        let capability = match self {
            Self::Equality => "equality",
            Self::Ordering => "ordering",
        };
        Err(SQLError::Diagnostic {
            sqlstate: "42883".into(),
            message: format!("could not identify an {capability} operator for type {name}"),
            detail: None,
            hint: matches!(self, Self::Ordering)
                .then(|| "Use an explicit ordering operator or modify the query.".into()),
        })
    }

    pub(crate) fn require_in_catalog(
        self,
        ty: &ColumnType,
        resolver: &dyn FunctionTypeResolver,
    ) -> Result<(), SQLError> {
        self.require(ty, Some(resolver), &ProductionControl::uncontrolled())
    }

    pub(crate) fn require_aggregate_ordering(
        ty: &ColumnType,
        resolver: Option<&dyn FunctionTypeResolver>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        Self::Ordering
            .require(ty, resolver, control)
            .map_err(|error| match error {
                SQLError::Diagnostic {
                    sqlstate, message, ..
                } if sqlstate == "42883" => SQLError::Diagnostic {
                    sqlstate,
                    message,
                    detail: Some(
                        "Aggregates with DISTINCT must be able to sort their inputs.".into(),
                    ),
                    hint: None,
                },
                error => error,
            })
    }
}
