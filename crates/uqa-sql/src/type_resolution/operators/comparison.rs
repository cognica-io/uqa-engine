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
    Hashing,
}

/// Whether every declared component can be hashed using its SQL equality. Anonymous records retain comparison-based execution because their field operators are selected lazily.
pub fn type_supports_hashing(
    ty: &ColumnType,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    ComparisonSupport::Hashing.available_for_type(
        ty,
        resolver.composite_types(),
        &ProductionControl::uncontrolled(),
    )
}

/// Whether the declared type has an ordering operator, without raising an unavailable-operator diagnostic.
pub fn type_supports_ordering(
    ty: &ColumnType,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, SQLError> {
    ComparisonSupport::Ordering.available_for_type(
        ty,
        resolver.composite_types(),
        &ProductionControl::uncontrolled(),
    )
}

/// Shared requirements for one complete grouping or duplicate-elimination key.
pub struct ComparisonKeySupport {
    pub hashing: bool,
    pub ordering: bool,
}

/// Admitted scalar constants and constructors of them have one comparison class within the pinned statement. Projection still evaluates them; eliminating their key does not suppress input or constructor errors.
pub fn comparison_key_is_constant(expression: &crate::ScalarExpr) -> bool {
    use crate::ScalarExpr;
    match expression {
        ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral {
            parameter_index: None,
            ..
        } => true,
        ScalarExpr::Row(items)
        | ScalarExpr::Array(items)
        | ScalarExpr::CompositeRow { items, .. } => items.iter().all(comparison_key_is_constant),
        _ => false,
    }
}

impl Default for ComparisonKeySupport {
    fn default() -> Self {
        Self {
            hashing: true,
            ordering: true,
        }
    }
}

impl ComparisonKeySupport {
    pub fn include(
        &mut self,
        ty: &ColumnType,
        resolver: &dyn FunctionTypeResolver,
    ) -> Result<(), SQLError> {
        self.hashing &= type_supports_hashing(ty, resolver)?;
        self.ordering &= type_supports_ordering(ty, resolver)?;
        Ok(())
    }

    pub fn require_implementation(&self, operation: &str) -> Result<(), SQLError> {
        if self.hashing || self.ordering {
            return Ok(());
        }
        Err(SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: format!("could not implement {operation}"),
            detail: Some(
                "Some of the datatypes only support hashing, while others only support sorting."
                    .into(),
            ),
            hint: None,
        })
    }
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
            Self::Hashing => {
                !matches!(ty, ColumnType::Record | ColumnType::Named(_))
                    && super::equality_operator_available(ty)
            }
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
            Self::Hashing => "hashing",
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
