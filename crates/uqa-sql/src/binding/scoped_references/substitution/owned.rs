//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped substitutions select original syntax addresses before moving any replacement payload.

use crate::{
    plan::{MutableQueryPlan, RelationalPlan},
    schema::{SchemaMap, SchemaSet},
    ColumnIdentity, RowSchema, SQLError, ScalarExpr,
};
use std::ptr::NonNull;
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

/// Substitute only references to the selected enclosing row. Read-only lexical traversal runs to completion before edits; missing layouts and unsupported whole-row replacements return the unchanged owner and false.
pub fn substitute_outer_columns_owned<'i>(
    query: MutableQueryPlan,
    target: &RowSchema,
    inputs: impl IntoIterator<Item = (&'i ColumnIdentity, &'i ScalarExpr)>,
    control: &ProductionControl<'_>,
) -> Result<(MutableQueryPlan, bool), SQLError> {
    let Some(replacements) = OuterColumnSubstitutions::new(inputs, control)? else {
        return Ok((query, false));
    };
    replacements.substitute(query, target, control)
}

/// Borrowed replacements for one enclosing scope. Build the identity and qualifier indexes once and reuse them across child queries and local expressions.
pub struct OuterColumnSubstitutions<'i> {
    values: SchemaMap<(Option<&'i str>, &'i str), &'i ScalarExpr>,
    qualifiers: SchemaSet<&'i str>,
}

impl<'i> OuterColumnSubstitutions<'i> {
    /// An open replacement would capture a name in the child query. In that case the enclosing query must retain its original source.
    pub fn new(
        inputs: impl IntoIterator<Item = (&'i ColumnIdentity, &'i ScalarExpr)>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        let mut entries = ProductionVec::new(*control);
        for (identity, expression) in inputs {
            control.check()?;
            if !is_closed_expression(expression, control)? {
                return Ok(None);
            }
            entries.push_copy(((identity.qualifier(), identity.column()), expression))?;
        }
        if entries.is_empty() {
            return Ok(Some(Self {
                values: SchemaMap::default(),
                qualifiers: SchemaSet::default(),
            }));
        }
        let values = SchemaMap::from_produced(
            entries.iter().map(|&(key, expression)| {
                Ok((
                    control.finish(key, control.empty_reservation())?,
                    control.finish(expression, control.empty_reservation())?,
                ))
            }),
            control,
        )?;
        let qualifiers = SchemaSet::from_produced(
            values
                .keys()
                .filter_map(|(qualifier, _)| *qualifier)
                .map(|qualifier| Ok(control.finish(qualifier, control.empty_reservation())?)),
            control,
        )?;
        Ok(Some(Self { values, qualifiers }))
    }

    /// Find a replacement by the bound qualifier and column without allocating a key.
    pub fn get(&self, qualifier: Option<&str>, column: &str) -> Option<&'i ScalarExpr> {
        self.values.get(&(qualifier, column)).copied()
    }

    /// Rewrite references to the enclosing row after resolving local, LATERAL and whole-row references in the child.
    pub fn substitute(
        &self,
        query: MutableQueryPlan,
        target: &RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<(MutableQueryPlan, bool), SQLError> {
        if self.values.is_empty() {
            return Ok((query, true));
        }
        let mut replacements = ProductionMap::new(*control);
        let mut selected_fields = ProductionMap::new(*control);
        let mut valid = true;
        let mut visit = |node: &ScalarExpr| {
            control.check()?;
            if selected_fields.get(&NonNull::from(node)).is_some() {
                return Ok(());
            }
            if let Some(identity) = crate::type_resolution::relation_field_column_ref(node, target)
            {
                if let Some(replacement) = self.get(identity.qualifier(), identity.column()) {
                    let (base, _) = crate::type_resolution::field_selection::field_reference(node)
                        .expect("selected relation field");
                    selected_fields.insert_copy(NonNull::from(base), ())?;
                    replacements.insert_copy(NonNull::from(node), replacement)?;
                    return Ok(());
                }
            }
            let position = match node {
                ScalarExpr::Column(column) => {
                    if target.has_qualifier(column)
                        && !target.has_unqualified_column(column)
                        && self.qualifiers.contains(column.as_str())
                    {
                        valid = false;
                    }
                    target.unqualified_position(column)
                }
                ScalarExpr::QualifiedColumn { qualifier, column } => {
                    target.qualified_position(qualifier, column)
                }
                ScalarExpr::QualifiedStar(qualifier) => {
                    valid &= !self.qualifiers.contains(qualifier.as_str());
                    None
                }
                _ => None,
            };
            if let Some(replacement) = position
                .and_then(|position| target.identity(position))
                .and_then(|identity| self.get(identity.qualifier(), identity.column()))
            {
                replacements.insert_copy(NonNull::from(node), replacement)?;
            }
            Ok::<_, SQLError>(())
        };
        let mut references = super::super::ControlledScopedReferences::new(&mut visit, *control);
        references.query(&query)?;
        if !references.is_complete() || !valid {
            return Ok((query, false));
        }
        let query = query.map_queries_while(control, &mut |mut query| {
            query = retain_projection_labels(query, &replacements, control)?;
            let query = query.rewrite_root_scalar_nodes(control, &mut |node| {
                replacements
                    .remove(&NonNull::from(node))
                    .map(|replacement| replacement.copy_for_rewrite(control).map_err(Into::into))
                    .transpose()
            })?;
            Ok((query, true))
        })?;
        debug_assert!(
            replacements.is_empty(),
            "every selected outer reference is visited"
        );
        Ok((query, true))
    }
}

fn is_closed_expression(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut closed = true;
    expression.try_visit(&mut |node| {
        control.check()?;
        closed &= !matches!(
            node,
            ScalarExpr::Column(_)
                | ScalarExpr::QualifiedColumn { .. }
                | ScalarExpr::QualifiedStar(_)
                | ScalarExpr::Star
                | ScalarExpr::ScalarSubquery(_)
                | ScalarExpr::Exists { .. }
                | ScalarExpr::InSubquery { .. }
        );
        Ok::<_, SQLError>(true)
    })?;
    Ok(closed)
}

fn retain_projection_labels(
    mut query: MutableQueryPlan,
    replacements: &ProductionMap<'_, NonNull<ScalarExpr>, &ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    if let RelationalPlan::QueryBlock(block) = &query.root {
        for index in 0..block.projections.len() {
            let RelationalPlan::QueryBlock(block) = &query.root else {
                unreachable!()
            };
            let projection = &block.projections[index];
            if projection.alias.is_some() {
                continue;
            }
            let mut changed = false;
            projection.expr.try_visit(&mut |node| {
                control.check()?;
                changed |= replacements.get(&NonNull::from(node)).is_some();
                Ok::<_, SQLError>(true)
            })?;
            if changed {
                let alias = crate::semantics::with_projection_label(projection, control, |name| {
                    Ok(control.copy_text(name)?)
                })?;
                query = query.replace_projection_alias(index, Some(alias), control)?;
            }
        }
    }
    Ok(query)
}
