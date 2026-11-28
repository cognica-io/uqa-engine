//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared projection decisions for published caller syntax and private, independently owned query fields.

use super::{
    replacements::{self, Replacement},
    OutputPruningContext, QueryPlan, RelationalPlan, RowSchema, SQLError,
};
use crate::{scratch::Buffer, ComputePlan, QueryBlockPlan};
use std::ops::Deref;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    plan::{MutableQueryPlan, OwnedScalarExpr, QueryChildSlot, QueryScalarSlot},
    ScalarExpr,
};

pub(in crate::optimizer) trait ProjectionTarget:
    Deref<Target = QueryPlan> + Sized
{
    type Scalar;
    fn finish_scalar(
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<Self::Scalar, SQLError>;
    fn attach_output(
        self,
        schema: RowSchema,
        compute: impl FnOnce(&QueryBlockPlan) -> Result<ComputePlan, SQLError>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError>;
    fn expand(
        self,
        schema: Option<&RowSchema>,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, bool), SQLError>;
    fn publish(
        self,
        replacements: Buffer<Replacement<Self::Scalar>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError>;
    fn prune_union_child(
        self,
        slot: QueryChildSlot,
        required: &dyn Fn(usize) -> bool,
        context: OutputPruningContext<'_>,
    ) -> Result<Self, SQLError>;
}

pub(in crate::optimizer) fn prune_positions<Q: ProjectionTarget>(
    mut query: Q,
    required: &dyn Fn(usize) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
) -> Result<Q, SQLError> {
    let control = &context.production;
    control.check()?;
    if query.analyzed_output.is_none() {
        if let Some(schema) = schema {
            query = query.attach_output(
                schema.clone(),
                |block| block.compute_kind_with_control(context.aggregates, control),
                control,
            )?;
        }
    }
    match &query.root {
        RelationalPlan::SetOp { .. } => {
            return super::super::union_projection::prune(query, required, context)
        }
        RelationalPlan::QueryBlock(block)
            if !block.distinct && block.distinct_on.is_empty() && block.windows.is_empty() => {}
        _ => return Ok(query),
    }
    let schema = schema.or(query.analyzed_output.as_ref()).cloned();
    let (query, expanded) = query.expand(schema.as_ref(), control)?;
    if !expanded {
        return Ok(query);
    }
    let replacements =
        replacements::collect(&query, schema.as_ref(), required, context, |expression| {
            Q::finish_scalar(expression, control)
        })?;
    if replacements.is_empty() {
        return Ok(query);
    }
    query.publish(replacements, control)
}

impl ProjectionTarget for &mut QueryPlan {
    type Scalar = Produced<ScalarExpr>;

    fn finish_scalar(
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<Self::Scalar, SQLError> {
        Ok(expression.finish(control)?)
    }

    fn attach_output(
        self,
        schema: RowSchema,
        compute: impl FnOnce(&QueryBlockPlan) -> Result<ComputePlan, SQLError>,
        _: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.analyzed_output = Some(schema);
        if let RelationalPlan::QueryBlock(block) = &mut self.root {
            block.compute = compute(block)?;
        }
        Ok(self)
    }

    fn expand(
        self,
        schema: Option<&RowSchema>,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, bool), SQLError> {
        let RelationalPlan::QueryBlock(block) = &mut self.root else {
            unreachable!("query block projections")
        };
        let expanded =
            super::expand_local_stars(block, self.analyzed_source.as_deref(), schema, control)?;
        Ok((self, expanded))
    }

    fn publish(
        self,
        replacements: Buffer<Replacement<Self::Scalar>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let RelationalPlan::QueryBlock(block) = &mut self.root else {
            unreachable!("query block projections")
        };
        block.preserve_projection_privileges(
            replacements.iter().map(|entry| entry.index),
            control,
        )?;
        for replacement in replacements {
            let projection = &mut block.projections[replacement.index];
            if let Some(alias) = replacement.alias {
                projection.alias = Some(alias.into_parts().0);
            }
            projection.expr = replacement.expression.into_parts().0;
        }
        Ok(self)
    }

    fn prune_union_child(
        self,
        slot: QueryChildSlot,
        required: &dyn Fn(usize) -> bool,
        context: OutputPruningContext<'_>,
    ) -> Result<Self, SQLError> {
        let RelationalPlan::SetOp { left, right, .. } = &mut self.root else {
            unreachable!("UNION branch")
        };
        let child = match slot {
            QueryChildSlot::Left => left,
            QueryChildSlot::Right => right,
            _ => unreachable!("UNION member"),
        };
        super::super::union_projection::members(&mut **child, required, context)?;
        Ok(self)
    }
}

impl ProjectionTarget for MutableQueryPlan {
    type Scalar = OwnedScalarExpr;
    fn finish_scalar(
        expression: OwnedScalarExpr,
        _: &ProductionControl<'_>,
    ) -> Result<Self::Scalar, SQLError> {
        Ok(expression)
    }

    fn attach_output(
        self,
        schema: RowSchema,
        compute: impl FnOnce(&QueryBlockPlan) -> Result<ComputePlan, SQLError>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let query = self.set_analyzed_output(Some(schema), control)?;
        if let RelationalPlan::QueryBlock(block) = &query.root {
            let compute = compute(block)?;
            query.set_compute(compute, control)
        } else {
            Ok(query)
        }
    }

    fn expand(
        self,
        schema: Option<&RowSchema>,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, bool), SQLError> {
        super::owned::expand_stars(self, schema, control)
    }

    fn publish(
        mut self,
        replacements: Buffer<Replacement<Self::Scalar>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self = self.preserve_projection_privileges(
            replacements.iter().map(|entry| entry.index),
            control,
        )?;
        for replacement in replacements {
            if let Some(alias) = replacement.alias {
                self = self.replace_projection_alias(replacement.index, Some(alias), control)?;
            }
            self = self.map_scalar(
                QueryScalarSlot::Projection(replacement.index),
                control,
                |_| Ok(replacement.expression),
            )?;
        }
        Ok(self)
    }

    fn prune_union_child(
        self,
        slot: QueryChildSlot,
        required: &dyn Fn(usize) -> bool,
        context: OutputPruningContext<'_>,
    ) -> Result<Self, SQLError> {
        self.map_query_child(slot, &context.production, |child| {
            super::super::union_projection::members(child, required, context)
        })
    }
}
