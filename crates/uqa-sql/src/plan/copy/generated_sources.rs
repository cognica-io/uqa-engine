//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated source containers admit their constructor records before moving independent scalar and relational children.

use super::{Copying, Result};
use crate::{
    ast::{InternalRelationId, JoinKind},
    plan::{JoinExecutionStrategy, MutableSourcePlan, OwnedScalarExpr, SourcePlan},
};

impl Copying<'_> {
    pub(crate) fn captured_values_source(
        &mut self,
        values: impl ExactSizeIterator<Item = OwnedScalarExpr>,
        relation: InternalRelationId,
    ) -> Result<SourcePlan> {
        let width = values.len();
        Ok(SourcePlan::Values {
            lateral: false,
            rows: self.field("rows", |copying| {
                copying.map(std::iter::once(values), |copying, values| {
                    copying.map(values, Self::take_scalar)
                })
            })?,
            alias: None,
            column_aliases: Vec::new(),
            internal_relation: Some(relation),
            internal_column_types: self.map(0..width, |_, _| Ok(None))?,
            internal_column_collations: Vec::new(),
        })
    }

    pub(crate) fn cross_join_source(
        &mut self,
        left: MutableSourcePlan,
        right: MutableSourcePlan,
        lateral: bool,
    ) -> Result<SourcePlan> {
        Ok(SourcePlan::Join {
            comma: false,
            left: self.field("left", |copying| {
                copying.boxed(|copying| copying.take_source(left))
            })?,
            right: self.field("right", |copying| {
                copying.boxed(|copying| copying.take_source(right))
            })?,
            kind: JoinKind::Cross,
            on: self.field("on", |copying| copying.optional_scalar(None))?,
            using: None,
            natural: false,
            alias: None,
            column_aliases: Vec::new(),
            lateral,
            strategy: JoinExecutionStrategy::Auto,
        })
    }
}
