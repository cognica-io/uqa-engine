//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind one equality before selecting its physical hash and merge capabilities.

use super::{
    BinaryOp, FunctionTypeResolver, Produced, ProductionControl, RowSchema, SQLError, SQLParam,
    ScalarExpr,
};
use uqa_sql::{
    plan::OwnedScalarExpr,
    semantics::{join_predicates::bind_join_comparison_owned, volatility::VolatilityCatalog},
    type_resolution::ComparisonKeySupport,
};

pub(super) struct KeyBinding<'a> {
    pub left: &'a RowSchema,
    pub right: &'a RowSchema,
    pub schema: RowSchema,
    pub params: &'a [SQLParam],
    pub types: &'a dyn FunctionTypeResolver,
    pub volatility: &'a dyn VolatilityCatalog,
    pub control: ProductionControl<'a>,
}

pub(super) struct PairSelection {
    pub keys: Option<[Produced<ScalarExpr>; 2]>,
    pub merge_supported: bool,
}

impl KeyBinding<'_> {
    pub fn select(&self, lhs: &ScalarExpr, rhs: &ScalarExpr) -> Result<PairSelection, SQLError> {
        let control = &self.control;
        let bound = OwnedScalarExpr::binary(
            BinaryOp::Equal,
            lhs.copy_for_rewrite(control)?,
            rhs.copy_for_rewrite(control)?,
            control,
        )?;
        let bound = uqa_sql::bind_type_introspection_owned(
            bound,
            &self.schema,
            self.params,
            Some(self.types),
            control,
        )?;
        if uqa_sql::semantics::volatility::expr_contains_volatile_function_with_control(
            self.volatility,
            &bound,
            control,
        )? {
            return Ok(PairSelection {
                keys: None,
                merge_supported: false,
            });
        }
        let ScalarExpr::Binary { lhs, rhs, .. } = &*bound else {
            unreachable!("binding preserves a comparison operator")
        };
        let constant = uqa_sql::type_resolution::comparison_key_is_constant(lhs)
            && uqa_sql::type_resolution::comparison_key_is_constant(rhs);
        let Some((left_key, right_key)) =
            bind_join_comparison_owned(self.left, self.right, bound, control)?
        else {
            // Constants remain predicates; they cannot make a non-joinable record clause into a hash join.
            return Ok(PairSelection {
                keys: None,
                merge_supported: constant,
            });
        };
        let left_type = uqa_sql::scalar_type_with_resolver_and_control(
            &left_key,
            self.left,
            self.params,
            Some(self.types),
            control,
        )?;
        let right_type = uqa_sql::scalar_type_with_resolver_and_control(
            &right_key,
            self.right,
            self.params,
            Some(self.types),
            control,
        )?;
        let mut support = ComparisonKeySupport::default();
        if let (Some(left), Some(right)) = (&left_type, &right_type) {
            support.include_equality_operands_with_control(left, right, self.types, control)?;
        } else {
            // Schemaless inputs retain runtime comparison rather than claiming declared hash support.
            for ty in [&left_type, &right_type].into_iter().flatten() {
                support.include_with_control(ty, self.types, control)?;
            }
            support.hashing = false;
        }
        Ok(PairSelection {
            keys: support
                .hashing
                .then(|| Ok::<_, SQLError>([left_key.finish(control)?, right_key.finish(control)?]))
                .transpose()?,
            merge_supported: support.ordering,
        })
    }
}
