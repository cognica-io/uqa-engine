//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured input rows and lexical joins keep independently movable source and scalar allocation owners.

use super::MutableSourcePlan;
use crate::{
    ast::{InternalRelationId, JoinKind},
    plan::{copy::Copying, JoinExecutionStrategy, OwnedScalarExpr, SourcePlan},
    SQLError,
};
use uqa_core::memory::ProductionControl;

impl MutableSourcePlan {
    /// Construct a relation reference from borrowed names without first materializing an ordinary source.
    pub fn table(
        name: &str,
        qualifier: &str,
        alias: Option<&str>,
        include_descendants: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if let Some(mut copying) = Copying::for_rewrite(*control) {
            let source = copying.table_source(name, qualifier, alias, include_descendants)?;
            return Ok(Self {
                source,
                memory: Some(copying.into_rewrite_memory()),
            });
        }
        Ok(Self {
            source: SourcePlan::Table {
                name: name.into(),
                executable_query: None,
                qualifier: qualifier.into(),
                alias: alias.map(str::to_owned),
                column_aliases: Vec::new(),
                bound_columns: None,
                include_descendants,
            },
            memory: None,
        })
    }

    pub fn captured_values(
        values: impl ExactSizeIterator<Item = OwnedScalarExpr>,
        relation: InternalRelationId,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if let Some(mut copying) = Copying::for_rewrite(*control) {
            let source = copying.captured_values_source(values, relation)?;
            return Ok(Self {
                source,
                memory: Some(copying.into_rewrite_memory()),
            });
        }
        let mut row = Vec::with_capacity(values.len());
        for value in values {
            row.push(
                value
                    .finish(control)?
                    .into_uncontrolled()
                    .expect("ordinary capture value"),
            );
        }
        Ok(Self {
            source: SourcePlan::Values {
                lateral: false,
                internal_column_types: vec![None; row.len()],
                internal_column_collations: Vec::new(),
                rows: vec![row],
                alias: None,
                column_aliases: Vec::new(),
                internal_relation: Some(relation),
            },
            memory: None,
        })
    }

    pub fn cross_join(
        left: Self,
        right: Self,
        lateral: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if let Some(mut copying) = Copying::for_rewrite(*control) {
            let source = copying.cross_join_source(left, right, lateral)?;
            return Ok(Self {
                source,
                memory: Some(copying.into_rewrite_memory()),
            });
        }
        let (left, left_owner) = left.into_rewrite_parts(control)?;
        let (right, right_owner) = right.into_rewrite_parts(control)?;
        assert!(
            left_owner.is_none() && right_owner.is_none(),
            "ordinary source operands"
        );
        Ok(Self {
            source: SourcePlan::Join {
                comma: false,
                left: Box::new(left),
                right: Box::new(right),
                kind: JoinKind::Cross,
                on: None,
                using: None,
                natural: false,
                alias: None,
                column_aliases: Vec::new(),
                lateral,
                strategy: JoinExecutionStrategy::Auto,
            },
            memory: None,
        })
    }
}
