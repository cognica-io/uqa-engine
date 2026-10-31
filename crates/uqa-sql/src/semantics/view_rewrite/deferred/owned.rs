//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred namespace resolution preserves independently owned replacement entries and mutable scalar-query arenas.

use super::{DeferredOuterRewrite, RowSchema, SQLError};
use crate::{
    plan::{MutableQueryPlan, QueryBlockPlan, RelationalPlan},
    production::Buffer,
};
use std::borrow::Cow;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod expression;
mod namespaces;
mod sources;
pub use sources::resolve_outer_sources_owned;

/// Resolve a standalone expression in its actual local namespace while moving its scalar-query arena with every replacement.
pub fn resolve_outer_expression_rewrites_owned(
    mut expression: crate::plan::MutableExpressionPlan,
    rewrites: &[DeferredOuterRewrite],
    local: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<crate::plan::MutableExpressionPlan, SQLError> {
    control.check()?;
    for rewrite in rewrites {
        let rewrite =
            namespaces::in_namespace(rewrite, Some(local), namespaces::Selection::All, control)?;
        expression = rewrite.bind_expression_owned(expression, control)?;
    }
    Ok(expression)
}

pub fn bind_outer_rewrite_scope_with_control<'a>(
    block: &QueryBlockPlan,
    outer: &'a RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Cow<'a, RowSchema>, SQLError> {
    control.check()?;
    let mut aliases = ProductionVec::new(*control);
    for rewrite in &block.deferred_outer_rewrites {
        for (identity, column) in rewrite.outer_columns.iter() {
            control.check()?;
            if !outer.has_internal_column(*column) {
                aliases.push_copy((*column, identity))?;
            }
        }
    }
    if aliases.is_empty() {
        return Ok(Cow::Borrowed(outer));
    }
    Ok(Cow::Owned(
        RowSchema::with_named_internal_aliases_with_control(outer, &aliases, control)?,
    ))
}

pub fn resolve_outer_rewrites_owned(
    mut query: MutableQueryPlan,
    local: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Err(SQLError::Internal(
            "outer rewrites require a query block".into(),
        ));
    };
    let open = local.columns_are_open(None);
    if !super::has_resolvable_outer_rewrites(block, local) {
        return Ok(query);
    }
    let (retained, rewrites) = query.take_deferred_rewrites(control)?;
    query = retained;
    let mut pending = Buffer::new(control);
    for rewrite in rewrites {
        control.check()?;
        if open {
            let deferred = namespaces::in_namespace(
                &rewrite,
                Some(local),
                namespaces::Selection::Unqualified,
                control,
            )?;
            if !deferred.replacements.is_empty() {
                pending.push(deferred, control)?;
            }
        }
        let selected = namespaces::in_namespace(
            &rewrite,
            Some(local),
            if open {
                namespaces::Selection::Qualified
            } else {
                namespaces::Selection::All
            },
            control,
        )?;
        query = query.transform_root_scalar_arenas(control, &mut |_, expression, arena| {
            expression::node(&selected, expression, arena, control)
        })?;
    }
    query.replace_deferred_rewrites(pending.into_iter(), control)
}

impl DeferredOuterRewrite {
    pub(crate) fn from_inputs_owned(
        inputs: uqa_core::memory::OwnedMap<
            Produced<crate::ColumnIdentity>,
            Produced<crate::plan::ExpressionPlan>,
        >,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let outer_columns = control
            .finish(super::OuterColumns::new(), control.empty_reservation())?
            .into_shared(control)?;
        Ok(Self {
            outer_columns,
            replacements: super::replacements::Replacements::from_inputs_owned(inputs, control)?,
            shadowed_qualifiers: uqa_core::memory::OwnedSet::new(),
            shadowed_columns: uqa_core::memory::OwnedSet::new(),
        })
    }

    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        namespaces::in_namespace(self, None, namespaces::Selection::All, control)
    }
}
