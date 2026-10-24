//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural outer-column identities survive local aliases with the same spelling.

use super::{
    ColumnIdentity, DeferredOuterRewrite, OuterReplacement, QueryBlockPlan, RowSchema, ScalarExpr,
};
use crate::ast::InternalRelationId;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Capture outer attributes before adding this query's local namespace. Previously captured identities keep their original row slot in descendant scopes.
pub fn bind_outer_rewrite_scope<'a>(
    block: &QueryBlockPlan,
    outer: &'a RowSchema,
) -> Cow<'a, RowSchema> {
    let aliases: Vec<_> = block
        .deferred_outer_rewrites
        .iter()
        .flat_map(|rewrite| rewrite.outer_columns.iter())
        .filter(|(_, column)| !outer.has_internal_column(**column))
        .map(|(identity, column)| (*column, identity.clone()))
        .collect();
    if aliases.is_empty() {
        Cow::Borrowed(outer)
    } else {
        Cow::Owned(RowSchema::with_named_internal_aliases(outer, &aliases))
    }
}

impl DeferredOuterRewrite {
    pub(super) fn capture_outer_columns(&mut self) {
        let relation = InternalRelationId::allocate();
        let mut columns = BTreeMap::new();
        for replacement in self.replacements.values() {
            let mut capture = |expression: &ScalarExpr| {
                expression.visit(&mut |node| {
                    if let ScalarExpr::QualifiedColumn { qualifier, column } = node {
                        if qualifier == &replacement.qualifier {
                            let attribute = columns.len();
                            columns
                                .entry(ColumnIdentity::qualified(qualifier, column))
                                .or_insert_with(|| relation.column(attribute));
                        }
                    }
                });
            };
            capture(&replacement.expression.scalar);
            for query in &replacement.expression.subqueries {
                query.visit_scalar_expressions(&mut capture);
            }
        }
        self.outer_columns = Arc::new(columns);
    }

    pub(super) fn bind_replacement_columns(&self, replacement: &mut OuterReplacement) {
        let expression = &mut replacement.expression;
        let mut bind = |node: &mut ScalarExpr| {
            if let ScalarExpr::QualifiedColumn { qualifier, column } = node {
                if qualifier != &replacement.qualifier {
                    return;
                }
                if let Some(reference) = self.outer_columns.get(&ColumnIdentity::qualified(
                    qualifier.as_str(),
                    column.as_str(),
                )) {
                    *node = ScalarExpr::InternalColumn(*reference);
                }
            }
        };
        crate::plan::rewrite_scalar_expression(&mut expression.scalar, &mut bind);
        for query in &mut expression.subqueries {
            query.rewrite_scalar_expressions(&mut bind);
        }
    }
}
