//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared comparison capabilities shared by query analysis and value operators.

use crate::expr::composites::{descriptor_with_control, CompositeTypeCatalog};
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
        | ScalarExpr::RetainedLiteral(_)
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
    /// Include both selected equality operands. `PostgreSQL`'s date, timestamp and timestamptz cross-type operators are merge-joinable but do not share a hash family; individual type support cannot establish pairwise hash compatibility.
    pub fn include_equality_operands(
        &mut self,
        left: &ColumnType,
        right: &ColumnType,
        resolver: &dyn FunctionTypeResolver,
    ) -> Result<(), SQLError> {
        self.include_equality_operands_with_control(
            left,
            right,
            resolver,
            &ProductionControl::uncontrolled(),
        )
    }

    pub fn include_equality_operands_with_control(
        &mut self,
        left: &ColumnType,
        right: &ColumnType,
        resolver: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.include_with_control(left, resolver, control)?;
        self.include_with_control(right, resolver, control)?;
        self.hashing &= equality_types_share_hash_key(left, right);
        Ok(())
    }

    pub fn include(
        &mut self,
        ty: &ColumnType,
        resolver: &dyn FunctionTypeResolver,
    ) -> Result<(), SQLError> {
        self.include_with_control(ty, resolver, &ProductionControl::uncontrolled())
    }

    pub fn include_with_control(
        &mut self,
        ty: &ColumnType,
        resolver: &dyn FunctionTypeResolver,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.hashing &= ComparisonSupport::Hashing.available_for_type(
            ty,
            resolver.composite_types(),
            control,
        )?;
        self.ordering &= ComparisonSupport::Ordering.available_for_type(
            ty,
            resolver.composite_types(),
            control,
        )?;
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

/// Whether two selected operands can use the same native equality-key encoding. Each type must separately support hashing; cross-type datetime operators instead evaluate their SQL comparison, even though each temporal type has its own hash family.
pub fn equality_types_share_hash_key(left: &ColumnType, right: &ColumnType) -> bool {
    let left = super::base_type(left);
    let right = super::base_type(right);
    let datetime = |ty: &ColumnType| {
        matches!(
            ty,
            ColumnType::Date | ColumnType::Timestamp | ColumnType::TimestampTz
        )
    };
    !(datetime(left) && datetime(right) && left != right)
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
                let fields = descriptor_with_control(catalog, reference.oid, control)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_comparison_keys_keep_one_class_without_reading_the_value() {
        use crate::ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral};
        use crate::ScalarExpr;
        use std::sync::Arc;
        #[derive(Debug)]
        struct Unreadable;
        impl ConstantValueSource for Unreadable {
            fn read(&self, _: u64) -> Result<uqa_core::Value, SQLError> {
                panic!("comparison classification cannot read a constant")
            }
        }
        let literal = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
            RetainedConstantValue::new(Arc::new(Unreadable), 0),
            Some(ColumnType::Record),
            None,
            None,
        ));
        for expression in [
            literal.clone(),
            ScalarExpr::Row(vec![literal.clone()]),
            ScalarExpr::Array(vec![literal]),
        ] {
            assert!(comparison_key_is_constant(&expression));
        }
        assert!(!comparison_key_is_constant(&ScalarExpr::Param(1)));
    }

    struct Types;

    impl FunctionTypeResolver for Types {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(None)
        }
    }

    #[test]
    fn datetime_equality_pair_support_matches_postgresql_operator_families() {
        use ColumnType::{Date, Timestamp, TimestampTz};
        // Independently read from PostgreSQL 18.4 pg_operator; all nine operators have oprcanmerge=true.
        for (left, right, hashing) in [
            (Date, Date, true),
            (Date, Timestamp, false),
            (Date, TimestampTz, false),
            (Timestamp, Date, false),
            (Timestamp, Timestamp, true),
            (Timestamp, TimestampTz, false),
            (TimestampTz, Date, false),
            (TimestampTz, Timestamp, false),
            (TimestampTz, TimestampTz, true),
        ] {
            let domain = |base: ColumnType| ColumnType::Domain {
                schema: "public".into(),
                name: "datetime_value".into(),
                oid: 90_000,
                array_oid: None,
                collation_oid: None,
                base: Box::new(base),
            };
            let precision = |ty: &ColumnType| match ty {
                Timestamp => ColumnType::TimestampPrecision(6),
                TimestampTz => ColumnType::TimestampTzPrecision(3),
                other => other.clone(),
            };
            for (left, right) in [
                (left.clone(), right.clone()),
                (domain(precision(&left)), domain(precision(&right))),
            ] {
                let mut support = ComparisonKeySupport::default();
                support
                    .include_equality_operands(&left, &right, &Types)
                    .unwrap();
                assert_eq!(support.hashing, hashing, "{left:?} = {right:?}");
                assert!(support.ordering);
                // Each grouping key keeps its own declared comparison domain.
                assert!(type_supports_hashing(&left, &Types).unwrap());
                assert!(type_supports_hashing(&right, &Types).unwrap());
            }
        }
    }
}
