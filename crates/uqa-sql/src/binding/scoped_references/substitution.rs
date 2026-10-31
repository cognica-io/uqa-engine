//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replace only references to one analyzed enclosing row.

use std::{
    collections::{BTreeMap, BTreeSet},
    ptr::NonNull,
};

use crate::{plan::QueryPlan, ColumnIdentity, RowSchema, ScalarExpr};

mod owned;
pub use owned::substitute_outer_columns_owned;

/// Substitute enclosing scalar columns without capturing locally shadowed names. Replacements must already use opaque identities and contain no query-local subquery slots. Missing source layouts and whole-row substitutions leave the query unchanged and return false.
pub fn substitute_outer_columns(
    query: &mut QueryPlan,
    target: &RowSchema,
    inputs: &BTreeMap<ColumnIdentity, ScalarExpr>,
) -> bool {
    if inputs.is_empty() {
        return true;
    }
    let mut valid = inputs.values().all(|input| {
        let mut closed = true;
        input.visit(&mut |node| {
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
        });
        closed
    });
    if !valid {
        return false;
    }
    let mut replacements = BTreeMap::new();
    let mut selected_fields = BTreeSet::new();
    let mut visit = |node: &ScalarExpr| {
        if selected_fields.contains(&NonNull::from(node)) {
            return;
        }
        if let Some(identity) =
            crate::type_resolution::field_selection::relation_column(node, target)
        {
            if let Some(replacement) = inputs.get(&identity) {
                let (base, _) = crate::type_resolution::field_selection::field_reference(node)
                    .expect("selected relation field");
                selected_fields.insert(NonNull::from(base));
                replacements.insert(NonNull::from(node), replacement.clone());
                return;
            }
        }
        let position = match node {
            ScalarExpr::Column(column) => {
                if target.has_qualifier(column)
                    && !target.has_unqualified_column(column)
                    && inputs
                        .keys()
                        .any(|key| key.qualifier() == Some(column.as_str()))
                {
                    valid = false;
                }
                target.unqualified_position(column)
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                target.qualified_position(qualifier, column)
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                valid &= !inputs.keys().any(|key| key.qualifier() == Some(qualifier));
                None
            }
            _ => None,
        };
        if let Some(replacement) = position
            .and_then(|position| target.identity(position))
            .and_then(|identity| inputs.get(identity))
        {
            // Addresses identify existing syntax nodes only; none is dereferenced. No syntax moves between collection and replacement.
            replacements.insert(NonNull::from(node), replacement.clone());
        }
    };
    let mut references = super::ScopedReferences::new(&mut visit);
    references.query(query);
    if !references.is_complete() || !valid {
        return false;
    }
    query.visit_queries_mut(&mut |query| {
        if let crate::plan::RelationalPlan::QueryBlock(block) = &mut query.root {
            for projection in &mut block.projections {
                if projection.alias.is_some() {
                    continue;
                }
                let mut changed = false;
                projection.expr.visit(&mut |node| {
                    changed |= replacements.contains_key(&NonNull::from(node));
                });
                if changed {
                    projection.alias = Some(crate::semantics::projection_label_at(projection));
                }
            }
        }
        crate::plan::subqueries::visit_root(&mut query.root, &mut |node| {
            if let Some(replacement) = replacements.remove(&NonNull::from(&*node)) {
                *node = replacement;
            }
        });
    });
    debug_assert!(
        replacements.is_empty(),
        "every selected reference is visited"
    );
    true
}
