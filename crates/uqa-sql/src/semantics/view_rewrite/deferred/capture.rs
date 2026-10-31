//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred view substitutions admit their selected expressions, identities and shared lookup storage before publication.

use super::{ColumnIdentity, DeferredOuterRewrite, OuterColumns, OuterReplacement, QueryBlockPlan};
use crate::{
    plan::{copy::SyntaxProduction, MutableExpressionPlan, OwnedScalarExpr, ScalarQueryArena},
    semantics::view_rewrite::{
        correlation::{owned, CorrelatedRewriteContext},
        layer_rewrite::embed_owned,
    },
    SQLError, ScalarExpr,
};
use std::sync::Arc;
use uqa_core::memory::{
    OwnedMap, OwnedSet, Produced, ProductionControl, ProductionMap, ProductionVec,
};

type Entries = OwnedMap<Produced<ColumnIdentity>, Produced<OuterReplacement>>;

impl DeferredOuterRewrite {
    pub(in crate::semantics::view_rewrite) fn capture_owned(
        context: &CorrelatedRewriteContext<'_>,
        block: &QueryBlockPlan,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let references = column_references(block, control)?;
        let mut entries = Entries::new();
        for column in &context.layer.columns {
            for qualifier in std::iter::once(None).chain(
                context
                    .target_qualifiers
                    .iter()
                    .map(|name| Some(name.as_str())),
            ) {
                control.check()?;
                if references.get(&(qualifier, column.name.as_str())).is_none() {
                    continue;
                }
                let identity =
                    ColumnIdentity::from_parts_with_control(qualifier, &column.name, control)?;
                let qualifier = qualifier.unwrap_or(context.default_target_qualifier);
                let expression = embed_owned(
                    context.services,
                    &column.expression,
                    context.layer,
                    qualifier,
                    control,
                )?;
                insert(
                    &mut entries,
                    identity,
                    replacement(expression, qualifier, control)?,
                    control,
                )?;
            }
        }
        if let Some(source) = context.source {
            let mut columns = ProductionMap::new(*control);
            for (position, name) in source.columns().iter().enumerate() {
                columns.insert_copy(source.public_name(position).unwrap_or(name), ())?;
            }
            for (&name, ()) in &columns {
                control.check()?;
                if references.get(&(None, name)).is_none() {
                    continue;
                }
                let Some(qualifier) = source
                    .unqualified_position(name)
                    .and_then(|position| source.identity(position))
                    .and_then(ColumnIdentity::qualifier)
                else {
                    continue;
                };
                let identity = ColumnIdentity::from_parts_with_control(None, name, control)?;
                if entries.contains_key(&identity) {
                    continue;
                }
                let scalar = OwnedScalarExpr::column(
                    control.copy_text(name)?,
                    Some(control.copy_text(qualifier)?),
                    control,
                )?;
                let expression = MutableExpressionPlan::from_parts(
                    scalar,
                    ScalarQueryArena::copy_from(&[], control)?,
                    control,
                )?;
                insert(
                    &mut entries,
                    identity,
                    replacement(expression, qualifier, control)?,
                    control,
                )?;
            }
        }
        let replacements = replacements(entries, control)?;
        let outer_columns = outer_columns(&replacements, control)?;
        Ok(control.finish(
            Self {
                outer_columns,
                replacements,
                shadowed_columns: OwnedSet::new(),
                shadowed_qualifiers: OwnedSet::new(),
            },
            control.empty_reservation(),
        )?)
    }

    pub(in crate::semantics::view_rewrite) fn rebase_owned(
        &self,
        context: &CorrelatedRewriteContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let mut entries = Entries::new();
        for (identity, value) in self.replacements.iter() {
            control.check()?;
            let expression = owned::expression(
                context,
                value.expression.copy_for_rewrite(control)?,
                control,
            )?;
            let identity = ColumnIdentity::from_parts_with_control(
                identity.qualifier(),
                identity.column(),
                control,
            )?;
            insert(
                &mut entries,
                identity,
                replacement(expression, &value.qualifier, control)?,
                control,
            )?;
        }
        let replacements = replacements(entries, control)?;
        let outer_columns = outer_columns(&replacements, control)?;
        let output = SyntaxProduction::new(*control);
        let shadowed_columns = copy_names(&self.shadowed_columns, &output, control)?;
        let shadowed_qualifiers = copy_names(&self.shadowed_qualifiers, &output, control)?;
        output.finish(Self {
            outer_columns,
            replacements,
            shadowed_qualifiers,
            shadowed_columns,
        })
    }
}

fn copy_names(
    names: &OwnedSet<String>,
    output: &SyntaxProduction<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedSet<String>, SQLError> {
    let mut values = OwnedSet::new();
    for name in names {
        control.check()?;
        let memory = control.reserve(OwnedSet::<String>::entry_bytes())?;
        let name = output.text(name)?;
        values.insert(name);
        output.take(control.finish((), memory)?);
    }
    Ok(values)
}

fn replacement(
    expression: MutableExpressionPlan,
    qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<OuterReplacement>, SQLError> {
    let expression = expression.finish(control)?;
    let qualifier = control.copy_text(qualifier)?;
    let (expression, expression_memory) = expression.into_parts();
    let (qualifier, qualifier_memory) = qualifier.into_parts();
    Ok(control.finish(
        OuterReplacement {
            expression,
            qualifier,
            memory: control.combine(expression_memory, qualifier_memory),
        },
        control.empty_reservation(),
    )?)
}

fn insert(
    entries: &mut Entries,
    identity: Produced<ColumnIdentity>,
    replacement: Produced<OuterReplacement>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(budget) = control.budget() {
        entries.insert_budgeted(identity, replacement, budget)?;
    } else {
        entries.insert(identity, replacement);
    }
    Ok(())
}

fn replacements(
    entries: Entries,
    control: &ProductionControl<'_>,
) -> Result<super::replacements::Replacements, SQLError> {
    let mut values = ProductionVec::new(*control);
    for (identity, value) in entries {
        control.check()?;
        let (identity, key_memory) = identity.into_parts();
        let (mut value, value_memory) = value.into_parts();
        value.memory = control.combine(
            value.memory.take(),
            control.combine(key_memory, value_memory),
        );
        let entry = control
            .finish((identity, value), control.empty_reservation())?
            .into_shared(control)?;
        values.push_produced(control.finish(entry, control.empty_reservation())?)?;
    }
    Ok(super::replacements::Replacements::Controlled(
        values.finish()?.into_shared(control)?,
    ))
}

fn outer_columns(
    replacements: &super::replacements::Replacements,
    control: &ProductionControl<'_>,
) -> Result<Arc<Produced<OuterColumns>>, SQLError> {
    let relation = crate::ast::InternalRelationId::allocate();
    let output = SyntaxProduction::new(*control);
    let mut columns = OuterColumns::new();
    for replacement in replacements.values() {
        let mut capture = |expression: &ScalarExpr| {
            expression.try_visit(&mut |node| {
                control.check()?;
                if let ScalarExpr::QualifiedColumn { qualifier, column } = node {
                    if qualifier == &replacement.qualifier {
                        let identity = ColumnIdentity::from_parts_with_control(
                            Some(qualifier),
                            column,
                            control,
                        )?;
                        if !columns.contains_key(&*identity) {
                            let attribute = relation.column(columns.len());
                            let identity = output.take(identity);
                            if let Some(budget) = control.budget() {
                                columns.insert_budgeted(identity, attribute, budget)?;
                            } else {
                                columns.insert(identity, attribute);
                            }
                        }
                    }
                }
                Ok::<_, SQLError>(true)
            })
        };
        capture(&replacement.expression.scalar)?;
        for query in &replacement.expression.subqueries {
            query.try_visit_scalar_expressions(&mut capture)?;
        }
    }
    Ok(output.finish(columns)?.into_shared(control)?)
}

fn column_references<'a, 'c>(
    block: &'a QueryBlockPlan,
    control: &ProductionControl<'c>,
) -> Result<ProductionMap<'c, (Option<&'a str>, &'a str), ()>, SQLError> {
    let mut references = ProductionMap::new(*control);
    let mut visit = |expression: &'a ScalarExpr| {
        expression.try_visit(&mut |node| {
            control.check()?;
            match node {
                ScalarExpr::Column(column) => {
                    references.insert_copy((None, column.as_str()), ())?;
                }
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    references.insert_copy((Some(qualifier.as_str()), column.as_str()), ())?;
                }
                _ => {}
            }
            Ok::<_, SQLError>(true)
        })
    };
    for expression in block
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(block.r#where.iter())
        .chain(&block.group_by)
        .chain(block.grouping_sets.iter().flatten())
        .chain(block.having.iter())
        .chain(block.order_by.iter().map(|order| &order.expr))
        .chain(block.limit.iter())
        .chain(block.offset.iter())
        .chain(&block.distinct_on)
        .chain(
            block
                .windows
                .iter()
                .flat_map(|window| window.spec.expressions()),
        )
    {
        visit(expression)?;
    }
    if let Some(source) = &block.from {
        source.try_visit_scalar_expressions(&mut visit)?;
    }
    for query in &block.subqueries {
        query.try_visit_scalar_expressions(&mut visit)?;
    }
    for rewrite in &block.deferred_outer_rewrites {
        rewrite.try_visit_scalar_expressions(&mut visit)?;
    }
    Ok(references)
}
