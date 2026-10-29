//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A standalone scalar and its query namespace retain separate mutable constructor owners.

use super::{OwnedScalarExpr, ScalarQueryArena};
use crate::{plan::ExpressionPlan, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

pub struct MutableExpressionPlan {
    scalar: OwnedScalarExpr,
    arena: ScalarQueryArena,
}

impl super::MutableQueryPlan {
    pub(crate) fn replace_where_expression(
        self,
        expression: MutableExpressionPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (scalar, arena) = expression.into_parts(control)?;
        let mut query = self.replace_where(Some(scalar), control)?;
        query.restore_arena(arena);
        Ok(query)
    }
}

impl ExpressionPlan {
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableExpressionPlan, SQLError> {
        MutableExpressionPlan::copy_from(&self.scalar, &self.subqueries, control)
    }
}

impl MutableExpressionPlan {
    pub(crate) fn into_projection_query(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<super::MutableQueryPlan, SQLError> {
        let (expression, arena) = self.into_parts(control)?;
        let mut query = super::MutableQueryPlan::projected(
            [super::projections::OwnedProjection {
                expression,
                alias: None,
            }]
            .into_iter(),
            control,
        )?;
        query.restore_arena(arena);
        Ok(query)
    }

    pub(crate) fn from_projection_query(
        mut query: super::MutableQueryPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let arena = query.take_arena();
        let (_, mut projections) = query.take_projections(control)?;
        assert_eq!(projections.len(), 1, "standalone expression projection");
        Self::from_parts(
            projections.next().expect("expression").expression,
            arena,
            control,
        )
    }

    /// Compact scalar-query occurrences through the same namespace remapper used by immutable expressions, retiring removed child payloads before their leases.
    pub fn prune_subqueries(self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        struct Parts {
            expression: ExpressionPlan,
            scalar_memory: Option<crate::plan::scalar::ScalarMemory>,
            arena_memory: Option<super::memory::RewriteMemory>,
        }
        let (scalar, arena) = self.into_parts(control)?;
        let (scalar, scalar_memory) = scalar.into_rewrite_parts();
        let mut parts = Parts {
            expression: ExpressionPlan {
                scalar,
                subqueries: arena.queries,
            },
            scalar_memory,
            arena_memory: arena.memory,
        };
        let mut fields = parts.arena_memory.as_mut().map(|owner| match owner {
            super::memory::RewriteMemory::Fields { fields, .. } => fields,
            super::memory::RewriteMemory::Scalar(_) => unreachable!("query arena constructor"),
        });
        crate::plan::subqueries::prune_expression_with_retention(
            &mut parts.expression,
            *control,
            |arena, keep| {
                let mut retained = 0;
                for index in 0..arena.len() {
                    if keep(index) {
                        arena.swap(retained, index);
                        if let Some(fields) = &mut fields {
                            fields.swap(retained, index);
                        }
                        retained += 1;
                    }
                }
                arena.truncate(retained);
                if let Some(fields) = fields {
                    fields.truncate(retained);
                }
            },
        )?;
        Ok(Self {
            scalar: OwnedScalarExpr::from_rewrite_parts(
                parts.expression.scalar,
                parts.scalar_memory,
            ),
            arena: ScalarQueryArena {
                queries: parts.expression.subqueries,
                memory: parts.arena_memory,
            },
        })
    }

    pub fn copy_from(
        scalar: &crate::ScalarExpr,
        queries: &[crate::plan::QueryPlan],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            scalar: scalar.copy_for_rewrite(control)?,
            arena: ScalarQueryArena::copy_from(queries, control)?,
        })
    }

    pub fn from_parts(
        scalar: OwnedScalarExpr,
        arena: ScalarQueryArena,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let scalar = scalar.rewrite(control, Ok::<_, SQLError>)?;
        Ok(Self { scalar, arena })
    }

    pub fn into_parts(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(OwnedScalarExpr, ScalarQueryArena), SQLError> {
        let scalar = self.scalar.rewrite(control, Ok::<_, SQLError>)?;
        Ok((scalar, self.arena))
    }

    pub fn scalar(&self) -> &crate::ScalarExpr {
        &self.scalar
    }
    pub fn queries(&self) -> &[crate::plan::QueryPlan] {
        self.arena.queries()
    }

    pub fn map_scalar(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(OwnedScalarExpr, &mut ScalarQueryArena) -> Result<OwnedScalarExpr, SQLError>,
    ) -> Result<Self, SQLError> {
        self.scalar = self
            .scalar
            .rewrite(control, |scalar| map(scalar, &mut self.arena))?;
        Ok(self)
    }

    pub fn map_query(
        mut self,
        index: usize,
        control: &ProductionControl<'_>,
        map: impl FnOnce(super::MutableQueryPlan) -> Result<super::MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.arena.map_query(index, control, map)?;
        Ok(self)
    }

    pub fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ExpressionPlan>, SQLError> {
        let scalar = self.scalar.finish(control)?;
        let queries = self.arena.finish(control)?;
        let (scalar, scalar_memory) = scalar.into_parts();
        let (subqueries, queries_memory) = queries.into_parts();
        Ok(control.finish(
            ExpressionPlan { scalar, subqueries },
            control.combine(scalar_memory, queries_memory),
        )?)
    }
}
